use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    middleware as axum_middleware,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Router,
};
use futures::StreamExt;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tracing::{info, instrument, warn};

use crate::metrics::metering::{MeteringEvent, RequestStatus};
use crate::middleware::auth::auth_middleware;
use crate::middleware::quota_middleware::quota_middleware;
use crate::middleware::rate_limit::rate_limit_middleware;
use crate::middleware::x_request_id::x_request_id_middleware;
use crate::retry::{chat_completion_stream_with_retry, chat_completion_with_retry, RetryConfig};
use crate::state::AppState;
use axum::Extension;
use gateway_core::error::GatewayError;
use gateway_core::tenant::TenantContext;
use gateway_core::types::*;

/// Wrapper that converts GatewayError into an OpenAI-format HTTP response.
pub struct ApiError(pub GatewayError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = http::StatusCode::from_u16(self.0.status_code())
            .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(self.0.to_error_response())).into_response()
    }
}

/// Convert GatewayError to our ApiError wrapper automatically.
impl From<GatewayError> for ApiError {
    fn from(e: GatewayError) -> Self {
        ApiError(e)
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Kubernetes-style health probes (must not require auth)
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/deep-health", get(deep_health))
        // API routes
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        // Admin API
        .nest("/api/admin", crate::admin::admin_router())
        // Admin UI (served at /admin)
        .route("/admin", get(crate::static_files::admin_page))
        .route(
            "/admin/static/admin.css",
            get(crate::static_files::admin_css),
        )
        .route("/admin/static/admin.js", get(crate::static_files::admin_js))
        .with_state(state.clone())
        // In axum the LAST `.layer()` call wraps all previous layers and
        // therefore runs FIRST on each request. We list middleware in the
        // reverse of the desired execution order:
        //
        //   Execution (outside → in): x_request_id → auth → rate_limit → quota → handler
        //   Registration order:       quota → rate_limit → auth → x_request_id (last)
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            quota_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.rate_limiter.clone(),
            rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(axum_middleware::from_fn(x_request_id_middleware))
}

/// Health check endpoint.
async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "healthy"})))
}

/// Liveness probe — always returns 200 "ok" to indicate the process is alive.
/// K8s uses this to decide whether to restart the container.
pub async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Readiness probe — returns 200 if the server is ready to serve traffic,
/// 503 otherwise. K8s uses this to decide whether to include the pod in the
/// service endpoints.
///
/// Ready = config readable + ≥1 provider circuit-closed/half-open (i.e. a
/// provider is currently eligible to receive requests).
pub async fn readiness(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Config must be readable (it isn't poisoned / uninitialized).
    // RwLock::read() would only fail if poisoned; here we treat lock acquisition
    // itself as the liveness signal. If we get the lock, config is "loaded".
    let config_ok = state.config.read().await.server.host.len() > 0;

    // Determine the best circuit state across all providers. If no providers
    // are tracked yet (fresh boot), we optimistically treat the cluster as
    // ready — the circuit breaker records Closed by default.
    let circuit_states = state.circuit_breaker.all_states().await;
    let any_accepting = if circuit_states.is_empty() {
        // Fresh startup: provider CBs haven't been registered by traffic yet,
        // but the breaker defaults unknown providers to Closed = accepting.
        true
    } else {
        circuit_states.values().any(|s| {
            matches!(s, crate::circuit_breaker::CircuitState::Closed)
                || matches!(s, crate::circuit_breaker::CircuitState::HalfOpen)
        })
    };

    if config_ok && any_accepting {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "providers": circuit_states.len(),
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "config_ok": config_ok,
                "providers": circuit_states.len(),
            })),
        )
    }
}

/// Deep health check — JSON payload with full status details for diagnostics.
/// Includes provider circuit states, cache config, total rejected counts.
pub async fn deep_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;
    let circuit_states = state.circuit_breaker.all_states().await;
    let total_rejected = state.circuit_breaker.total_rejected();

    let providers: Vec<serde_json::Value> = circuit_states
        .iter()
        .map(|(name, s)| {
            let state_str = match s {
                crate::circuit_breaker::CircuitState::Closed => "closed",
                crate::circuit_breaker::CircuitState::Open => "open",
                crate::circuit_breaker::CircuitState::HalfOpen => "half_open",
            };
            json!({ "name": name, "circuit_state": state_str })
        })
        .collect();

    let metrics = state.metrics.lock().await;

    Json(json!({
        "status": "ok",
        "server": {
            "host": config.server.host,
            "port": config.server.port,
        },
        "auth_enabled": config.auth.enabled,
        "cache": {
            "enabled": config.cache.enabled,
            "max_capacity": config.cache.max_capacity,
        },
        "rate_limit_rpm": config.rate_limit.requests_per_minute,
        "providers_tracked": circuit_states.len(),
        "providers": providers,
        "total_rejected": total_rejected,
        "metrics": {
            "total_requests": metrics.total_requests,
            "total_prompt_tokens": metrics.total_prompt_tokens,
            "total_completion_tokens": metrics.total_completion_tokens,
            "total_errors": metrics.total_errors,
        },
    }))
}

/// List available models.
async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Record a Prometheus sample so the request_total counter appears in /metrics
    state
        .prometheus
        .record_request("list_models", "internal", "default", "developer", false);

    let mut models = Vec::new();
    let providers = state.providers.read().await;
    for model_name in providers.keys() {
        models.push(ModelInfo {
            id: model_name.clone(),
            object: "model".into(),
            created: 0,
            owned_by: "ai-gateway".into(),
        });
    }

    Json(ModelList {
        object: "list".into(),
        data: models,
    })
}

/// Prometheus /metrics endpoint.
async fn get_metrics(State(state): State<Arc<AppState>>) -> axum::response::Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state.prometheus.render(),
    )
        .into_response()
}

/// Main chat completions endpoint with retry, circuit breaker, and fallback.
#[instrument(
    skip(state, payload),
    fields(model = %payload.model, stream = payload.stream)
)]
async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Extension(tenant_ctx): Extension<TenantContext>,
    _headers: HeaderMap,
    Json(payload): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let model = payload.model.clone();
    let stream = payload.stream;
    info!(%model, stream, tenant = %tenant_ctx.tenant_id, "Received chat completion request");

    let retry_config = RetryConfig::default();

    // Streaming path — with retry & fallback
    if stream {
        let result = chat_completion_stream_with_retry(
            &state,
            &state.circuit_breaker,
            &retry_config,
            payload,
        )
        .await;

        let (stream, _attempt_info) = match result {
            Ok(res) => res,
            Err(e) => {
                warn!(error = %e, "All providers failed for streaming request");
                return Err(ApiError(e));
            }
        };

        let sse = stream.map(|item| match item {
            Ok(chunk) => {
                let data = serde_json::to_string(&chunk).unwrap_or_default();
                Ok::<Event, std::convert::Infallible>(Event::default().data(data))
            }
            Err(e) => {
                let err_data = serde_json::to_string(&ErrorResponse {
                    error: ErrorDetail {
                        message: e.to_string(),
                        error_type: "upstream_error".into(),
                        param: None,
                        code: None,
                    },
                })
                .unwrap_or_default();
                Ok(Event::default().data(err_data))
            }
        });

        return Ok(Sse::new(sse).into_response());
    }

    // Non-streaming: check cache first
    let cache_enabled = state.config.read().await.cache.enabled;
    if cache_enabled {
        let cache_key = compute_cache_key(&payload);
        if let Some(cached) = state.cache.get(&cache_key).await {
            info!(%model, "Cache hit");
            return Ok(Json(cached).into_response());
        }

        // Call with retry + circuit breaker + fallback
        let (response, attempt_info) =
            chat_completion_with_retry(&state, &state.circuit_breaker, &retry_config, payload)
                .await
                .map_err(ApiError)?;

        let usage = response.usage.clone();

        // Store in cache
        state.cache.insert(cache_key, response.clone()).await;

        // Record metrics (global aggregate + per-tenant metering)
        state.record_usage(&response.model, &usage).await;
        record_metering(
            &state,
            &attempt_info.provider_name,
            &response.model,
            &usage,
            true,
            &tenant_ctx.tenant_id,
            &tenant_ctx.key_id,
        )
        .await;

        info!(
            provider = %attempt_info.provider_name,
            attempt = attempt_info.attempt_number,
            fallback = attempt_info.is_fallback,
            tokens = usage.total_tokens,
            "Request completed successfully"
        );

        Ok(Json(response).into_response())
    } else {
        let (response, attempt_info) =
            chat_completion_with_retry(&state, &state.circuit_breaker, &retry_config, payload)
                .await
                .map_err(ApiError)?;

        let usage = response.usage.clone();
        state.record_usage(&response.model, &usage).await;
        record_metering(
            &state,
            &attempt_info.provider_name,
            &response.model,
            &usage,
            true,
            &tenant_ctx.tenant_id,
            &tenant_ctx.key_id,
        )
        .await;

        info!(
            provider = %attempt_info.provider_name,
            attempt = attempt_info.attempt_number,
            fallback = attempt_info.is_fallback,
            tokens = usage.total_tokens,
            "Request completed successfully"
        );

        Ok(Json(response).into_response())
    }
}

/// Record a metering event for a completed LLM request.
///
/// Cost is looked up from `AppConfig.pricing` by `model`; unknown models or
/// an empty table fall back to 0.0 (free).
async fn record_metering(
    state: &AppState,
    provider_name: &str,
    model: &str,
    usage: &Usage,
    success: bool,
    tenant_id: &str,
    key_id: &str,
) {
    let (estimated_cost_cents, cost_alert_threshold_cents) = {
        let config = state.config.read().await;
        (
            config.pricing.estimate_cost(model, usage),
            config
                .tenants
                .get(tenant_id)
                .and_then(|t| t.cost_alert_threshold_cents),
        )
    };

    let event = MeteringEvent {
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        tenant_id: tenant_id.to_string(),
        key_id: key_id.to_string(),
        model: model.into(),
        provider: provider_name.into(),
        prompt_tokens: usage.prompt_tokens as u64,
        completion_tokens: usage.completion_tokens as u64,
        status: if success {
            RequestStatus::Success
        } else {
            RequestStatus::Error
        },
        estimated_cost_cents,
    };

    state
        .metering
        .record(event, cost_alert_threshold_cents)
        .await;
}

/// Compute a cache key from the request (model + messages).
fn compute_cache_key(req: &ChatCompletionRequest) -> String {
    let mut hasher = DefaultHasher::new();
    req.model.hash(&mut hasher);
    // Hash the serialized messages for a stable key
    let msg_json = serde_json::to_string(&req.messages).unwrap_or_default();
    msg_json.hash(&mut hasher);
    req.temperature.map(|t| t.to_bits()).hash(&mut hasher);
    req.max_tokens.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
