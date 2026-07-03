use axum::{
    routing::{get, post, put, delete},
    Router,
    extract::{State, Path, Json, Extension},
    response::{IntoResponse, Response},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::metrics::metering::TenantUsage;
use crate::middleware::auth::AuthKey;
use crate::middleware::rbac::{require_role, require_tenant};
use crate::state::AppState;
use crate::routes::ApiError;
use crate::circuit_breaker::CircuitState;
use gateway_core::auth_key::ApiKeyEntry;
use gateway_core::error::GatewayError;
use gateway_core::tenant::{Role, TenantContext};

// ─── Request/Response types ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub key: String,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuotaRequest {
    pub max_rpm: Option<u32>,
    pub max_rpd: Option<u64>,
    pub max_tpm: Option<u64>,
    pub max_tpd: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct TenantInfo {
    pub id: String,
    pub quotas: gateway_core::tenant::TenantQuotas,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_models: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTenantRequest {
    pub id: String,
    #[serde(default)]
    pub quotas: Option<gateway_core::tenant::TenantQuotas>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCacheConfigRequest {
    pub enabled: Option<bool>,
    pub max_capacity: Option<u64>,
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRateLimitRequest {
    pub requests_per_minute: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct AdminMetricsResponse {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_errors: u64,
    pub per_model: std::collections::HashMap<String, u64>,
    pub providers_count: usize,
    pub models_count: usize,
    pub cache_enabled: bool,
    pub cache_size: u64,
    pub rate_limit_rpm: u32,
    pub auth_enabled: bool,
    pub api_keys_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub api_key_set: bool,
    pub base_url: Option<String>,
    pub models: Vec<String>,
}

// ─── Router ──────────────────────────────────────────────────────────

pub fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/metrics", get(get_metrics))
        .route("/providers", get(list_providers).post(create_provider))
        .route(
            "/providers/{name}",
            get(get_provider).put(update_provider).delete(delete_provider),
        )
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{key}", delete(delete_key))
        .route("/config/cache", put(update_cache_config))
        .route("/config/rate-limit", put(update_rate_limit))
        .route("/config/rate-card", get(get_rate_card).put(update_rate_card))
        .route("/config/quota/{tenant_id}", put(update_quota))
        .route("/usage/{tenant_id}", get(get_tenant_usage))
        .route("/usage", get(get_all_usage))
        .route("/tenants", get(list_tenants).post(create_tenant))
        .route("/tenants/{tenant_id}", delete(delete_tenant))
        .route("/logs", get(get_logs))
        .route("/circuit-breaker", get(get_circuit_breaker_status))
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn get_metrics(State(state): State<Arc<AppState>>) -> Json<AdminMetricsResponse> {
    let m = state.metrics.lock().await;
    let config = state.config.read().await;
    let cache_size = if config.cache.enabled {
        state.cache.entry_count()
    } else {
        0
    };

    let providers_count = {
        let providers = state.providers.read().await;
        providers.len()
    };

    Json(AdminMetricsResponse {
        total_requests: m.total_requests,
        total_prompt_tokens: m.total_prompt_tokens,
        total_completion_tokens: m.total_completion_tokens,
        total_errors: m.total_errors,
        per_model: m.per_model.clone(),
        providers_count: config.providers.len(),
        models_count: providers_count,
        cache_enabled: config.cache.enabled,
        cache_size,
        rate_limit_rpm: config.rate_limit.requests_per_minute,
        auth_enabled: config.auth.enabled,
        api_keys_count: config.auth.api_keys.len(),
    })
}

async fn list_providers(State(state): State<Arc<AppState>>) -> Json<Vec<ProviderInfo>> {
    let config = state.config.read().await;
    let providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(name, cfg)| ProviderInfo {
            name: name.clone(),
            api_key_set: cfg.api_key.as_ref().map_or(false, |k| !k.is_empty()),
            base_url: cfg.base_url.clone(),
            models: cfg.models.clone(),
        })
        .collect();

    Json(providers)
}

async fn get_provider(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<ProviderInfo>, ApiError> {
    let config = state.config.read().await;
    let cfg = config
        .providers
        .get(&name)
        .ok_or_else(|| ApiError(gateway_core::error::GatewayError::ProviderNotFound(name.clone())))?;

    Ok(Json(ProviderInfo {
        name,
        api_key_set: cfg.api_key.as_ref().map_or(false, |k| !k.is_empty()),
        base_url: cfg.base_url.clone(),
        models: cfg.models.clone(),
    }))
}

async fn create_provider(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<CreateProviderRequest>,
) -> Result<Response, ApiError> {
    // RBAC: creating a providers requires at least tenant_admin
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    if req.name.is_empty() {
        return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
            "Provider name cannot be empty".into(),
        )));
    }

    {
        let config = state.config.read().await;
        if config.providers.contains_key(&req.name) {
            return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
                format!("Provider '{}' already exists", req.name),
            )));
        }
    }

    info!("Admin: creating provider '{}' with models {:?}", req.name, req.models);

    let provider_cfg = gateway_core::config::ProviderConfig {
        api_key: req.api_key,
        base_url: req.base_url,
        models: req.models.clone(),
        extra_headers: Default::default(),
        field_overrides: None,
    };

    state
        .register_provider(&req.name, &provider_cfg)
        .await
        .map_err(ApiError)?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({"status": "created"}))).into_response())
}

async fn update_provider(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(name): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;
    {
        let config = state.config.read().await;
        if !config.providers.contains_key(&name) {
            return Err(ApiError(gateway_core::error::GatewayError::ProviderNotFound(name)));
        }
    }

    info!("Admin: updating provider '{}'", name);

    // Update config
    {
        let mut config = state.config.write().await;
        let cfg = config.providers.get_mut(&name).unwrap();
        if let Some(key) = req.api_key {
            cfg.api_key = Some(key);
        }
        if let Some(url) = req.base_url {
            cfg.base_url = Some(url);
        }
        if let Some(models) = req.models {
            cfg.models = models;
        }
    }

    // Re-register provider with new config
    let cfg = {
        let config = state.config.read().await;
        config.providers.get(&name).unwrap().clone()
    };
    state.register_provider(&name, &cfg).await.map_err(ApiError)?;

    Ok(Json(serde_json::json!({"status": "updated"})))
}

async fn delete_provider(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;
    {
        let config = state.config.read().await;
        if !config.providers.contains_key(&name) {
            return Err(ApiError(gateway_core::error::GatewayError::ProviderNotFound(name)));
        }
    }

    info!("Admin: deleting provider '{}'", name);

    // Collect models to unregister
    let models: Vec<String> = {
        let config = state.config.read().await;
        let cfg = config.providers.get(&name).unwrap();
        cfg.models.clone()
    };

    // Remove from config
    {
        let mut config = state.config.write().await;
        config.providers.remove(&name);
    }

    // Unregister models
    {
        let mut providers = state.providers.write().await;
        for model in &models {
            providers.remove(model);
        }
    }

    Ok(Json(serde_json::json!({"status": "deleted"})))
}

async fn list_keys(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: any authenticated user can list their tenant's keys
    let caller_tenant = require_role(&state, &auth_key, Role::DEVELOPER)
        .await
        .map_err(ApiError)?;

    let config = state.config.read().await;
    let store = state.auth_store.read().await;

    // Determine if caller is global admin
    let is_admin = matches!(
        store.verify_by_id(&auth_key.0).map(|e| Role::from_str(&e.role)),
        Some(role) if role >= Role::ADMIN
    );

    let entries: Vec<serde_json::Value> = store
        .list_entries()
        .iter()
        .filter(|e| is_admin || e.tenant_id == caller_tenant)
        .map(|e| {
            serde_json::json!({
                "key_id": e.key_id,
                "tenant_id": e.tenant_id,
                "role": e.role,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "enabled": config.auth.enabled,
        "keys": entries,
    })))
}

async fn create_key(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Response, ApiError> {
    // RBAC: creating keys requires at least tenant_admin
    let tenant_id = require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    if req.key.is_empty() {
        return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
            "API key cannot be empty".into(),
        )));
    }

    // Determine target tenant + role from request (tenant_admin stays in own tenant)
    let target_tenant = req.tenant_id.unwrap_or(tenant_id);
    let target_role = req
        .role
        .clone()
        .unwrap_or_else(|| "developer".to_string());

    // Only global admin can create keys for other tenants
    if target_tenant != require_role(&state, &auth_key, Role::DEVELOPER).await? {
        require_role(&state, &auth_key, Role::ADMIN)
            .await
            .map_err(ApiError)?;
    }

    // Add as structured entry
    {
        let config = state.config.read().await;
        let already_exists = config.auth.structured_keys.iter().any(|k| k.0 == req.key);
        if already_exists {
            return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
                "API key already exists".into(),
            )));
        }
    }

    // Clone for the ApiKeyEntry creation below
    let plaintext_key = req.key.clone();
    let tenant_for_store = target_tenant.clone();
    let role_for_store = target_role.clone();

    info!(key_tenant = %target_tenant, key_role = %target_role, "Admin: adding new API key");
    state
        .config
        .write()
        .await
        .auth
        .structured_keys
        .push(gateway_core::config::StructuredKey(
            req.key,
            target_tenant,
            target_role,
        ));

    // Also add to the in-memory store so it works immediately
    {
        use gateway_core::auth_key::ApiKeyEntry;
use gateway_core::error::GatewayError;
        let entry = ApiKeyEntry::new(&plaintext_key, &tenant_for_store, &role_for_store);
        state.auth_store.write().await.add(entry);
    }

    Ok((StatusCode::CREATED, Json(serde_json::json!({"status": "created"}))).into_response())
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(key_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: requires tenant_admin; scoped to own tenant
    let caller_tenant = require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    // Check tenant scope on the entry being deleted
    {
        let config = state.config.read().await;
        if let Some(entry) = config.auth.structured_keys.iter().find(|k| k.0 == key_id) {
            if entry.1 != caller_tenant {
                require_role(&state, &auth_key, Role::ADMIN)
                    .await
                    .map_err(ApiError)?;
            }
        }
    }

    let mut config = state.config.write().await;
    let before = config.auth.structured_keys.len() + config.auth.api_keys.len();
    config.auth.structured_keys.retain(|k| k.0 != key_id);
    config.auth.api_keys.retain(|k| k != &key_id);
    let after = config.auth.structured_keys.len() + config.auth.api_keys.len();

    if after == before {
        return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
            "API key not found".into(),
        )));
    }

    // Also remove from in-memory store
    state.auth_store.write().await.remove_by_id(&key_id);

    info!(key_id = %key_id, "Admin: removed API key");
    Ok(Json(serde_json::json!({"status": "deleted"})))
}


async fn update_quota(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(tenant_id): Path<String>,
    Json(req): Json<UpdateQuotaRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: tenant_admin can update own tenant quotas; admin can update any
    let caller_tenant = require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;
    if caller_tenant != tenant_id {
        require_role(&state, &auth_key, Role::ADMIN)
            .await
            .map_err(ApiError)?;
    }

    let mut config = state.config.write().await;
    let tenant = config
        .tenants
        .entry(tenant_id.clone())
        .or_insert_with(gateway_core::tenant::TenantConfig::default);

    if let Some(rpm) = req.max_rpm {
        tenant.quotas.max_rpm = rpm;
    }
    if let Some(rpd) = req.max_rpd {
        tenant.quotas.max_rpd = rpd;
    }
    if let Some(tpm) = req.max_tpm {
        tenant.quotas.max_tpm = tpm;
    }
    if let Some(tpd) = req.max_tpd {
        tenant.quotas.max_tpd = tpd;
    }

    info!(tenant = %tenant_id, "Admin: updated tenant quotas");
    Ok(Json(serde_json::json!({
        "status": "updated",
        "tenant_id": tenant_id,
        "quotas": {
            "max_rpm": tenant.quotas.max_rpm,
            "max_rpd": tenant.quotas.max_rpd,
            "max_tpm": tenant.quotas.max_tpm,
            "max_tpd": tenant.quotas.max_tpd,
        }
    })))
}

async fn list_tenants(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<Vec<TenantInfo>>, ApiError> {
    // RBAC: any authenticated developer can list tenants
    let caller_tenant = require_role(&state, &auth_key, Role::DEVELOPER)
        .await
        .map_err(ApiError)?;

    let config = state.config.read().await;
    let is_admin = matches!(
        state.auth_store.read().await.verify_by_id(&auth_key.0).map(|e| Role::from_str(&e.role)),
        Some(role) if role >= Role::ADMIN
    );

    let tenants: Vec<TenantInfo> = config
        .tenants
        .iter()
        .filter(|(id, _)| is_admin || **id == caller_tenant)
        .map(|(id, cfg)| TenantInfo {
            id: id.clone(),
            quotas: cfg.quotas.clone(),
            allowed_providers: cfg.allowed_providers.clone(),
            allowed_models: cfg.allowed_models.clone(),
        })
        .collect();

    Ok(Json(tenants))
}

async fn create_tenant(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<Response, ApiError> {
    // RBAC: only global admin can create tenants
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

    if req.id.is_empty() {
        return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
            "Tenant id cannot be empty".into(),
        )));
    }

    {
        let config = state.config.read().await;
        if config.tenants.contains_key(&req.id) {
            return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
                format!("Tenant '{}' already exists", req.id),
            )));
        }
    }

    let mut config = state.config.write().await;
    let tenant_cfg = gateway_core::tenant::TenantConfig {
        quotas: req.quotas.unwrap_or_default(),
        ..Default::default()
    };
    config.tenants.insert(req.id.clone(), tenant_cfg);

    info!(tenant = %req.id, "Admin: created tenant");
    Ok((StatusCode::CREATED, Json(serde_json::json!({"status": "created"}))).into_response())
}

async fn delete_tenant(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: only global admin can delete tenants
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

    // Don't allow deleting the default tenant
    if tenant_id == "default" {
        return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
            "Cannot delete default tenant".into(),
        )));
    }

    let mut config = state.config.write().await;
    config
        .tenants
        .remove(&tenant_id)
        .ok_or_else(|| ApiError(gateway_core::error::GatewayError::BadRequest(
            format!("Tenant '{}' not found", tenant_id),
        )))?;

    info!(tenant = %tenant_id, "Admin: deleted tenant");
    Ok(Json(serde_json::json!({"status": "deleted"})))
}

async fn update_cache_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateCacheConfigRequest>,
) -> Json<serde_json::Value> {
    let mut config = state.config.write().await;
    if let Some(enabled) = req.enabled {
        config.cache.enabled = enabled;
    }
    if let Some(cap) = req.max_capacity {
        config.cache.max_capacity = cap;
    }
    if let Some(ttl) = req.ttl_seconds {
        config.cache.ttl_seconds = ttl;
    }

    info!("Admin: updated cache config");
    Json(serde_json::json!({
        "status": "updated",
        "cache": {
            "enabled": config.cache.enabled,
            "max_capacity": config.cache.max_capacity,
            "ttl_seconds": config.cache.ttl_seconds,
        }
    }))
}

async fn update_rate_limit(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateRateLimitRequest>,
) -> Json<serde_json::Value> {
    let rpm = {
        let mut config = state.config.write().await;
        if let Some(rpm) = req.requests_per_minute {
            config.rate_limit.requests_per_minute = rpm;
        }
        config.rate_limit.requests_per_minute
    };
    // The bucket has captured the capacity at this point, so we need to tell it
    // explicitly — config updates do not flow through automatically, since
    // `TokenBucket::new` doesn't hold a reference to `config`.
    state.update_rate_limit_config(rpm);

    info!(rpm, "Admin: updated rate limit config");
    Json(serde_json::json!({
        "status": "updated",
        "requests_per_minute": rpm,
    }))
}

async fn get_logs(State(_state): State<Arc<AppState>>) -> Json<Vec<crate::log_buffer::LogEntry>> {
    Json(crate::log_buffer::LOG_BUFFER.entries())
}

async fn get_circuit_breaker_status(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let states = state.circuit_breaker.all_states().await;
    let total_rejected = state.circuit_breaker.total_rejected();

    let states_map: std::collections::HashMap<String, String> = states
        .into_iter()
        .map(|(k, v)| {
            let state_str = match v {
                CircuitState::Closed => "closed",
                CircuitState::Open => "open",
                CircuitState::HalfOpen => "half_open",
            };
            (k, state_str.to_string())
        })
        .collect();

    Json(serde_json::json!({
        "states": states_map,
        "total_rejected": total_rejected,
    }))
}

// ─── Metering & Usage handlers (MVP 2) ───────────────────────────────

/// GET /api/admin/usage/{tenant_id} — returns per-tenant aggregated usage.
///
/// RBAC: tenant_admin can read own tenant; global admin can read any.
async fn get_tenant_usage(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantUsage>, ApiError> {
    // RBAC: tenant_admin can read own tenant; admin can read any.
    let caller_tenant = require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;
    if caller_tenant != tenant_id {
        require_role(&state, &auth_key, Role::ADMIN)
            .await
            .map_err(ApiError)?;
    }

    let usage = state
        .metering
        .tenant_usage(&tenant_id)
        .await
        .unwrap_or_else(|| TenantUsage {
            tenant_id: tenant_id.clone(),
            ..Default::default()
        });
    Ok(Json(usage))
}

/// GET /api/admin/usage — returns usage for all visible tenants.
///
/// RBAC: tenant_admin sees only own tenant; global admin sees all.
async fn get_all_usage(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<Vec<TenantUsage>>, ApiError> {
    let caller_tenant = require_role(&state, &auth_key, Role::DEVELOPER)
        .await
        .map_err(ApiError)?;
    let is_admin = matches!(
        state
            .auth_store
            .read()
            .await
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_str(&e.role)),
        Some(role) if role >= Role::ADMIN
    );

    let all = state.metering.all_usage().await;
    let visible: Vec<TenantUsage> = if is_admin {
        all.into_values().collect()
    } else {
        all.into_values()
            .filter(|u| u.tenant_id == caller_tenant)
            .collect()
    };
    Ok(Json(visible))
}

/// PUT /api/admin/config/rate-card — update platform-wide pricing.
///
/// RBAC: only global admin can set pricing.
#[derive(Debug, Deserialize)]
pub struct UpdateRateCardRequest {
    pub prompt_per_million: Option<u64>,
    pub completion_per_million: Option<u64>,
}

async fn update_rate_card(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<UpdateRateCardRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

    let mut config = state.config.write().await;
    let card = config
        .rate_config
        .get_or_insert_with(gateway_core::metering::RateCard::default);
    if let Some(prompt) = req.prompt_per_million {
        card.prompt_per_million = prompt;
    }
    if let Some(completion) = req.completion_per_million {
        card.completion_per_million = completion;
    }
    let resp = serde_json::json!({
        "status": "updated",
        "rate_card": {
            "prompt_per_million": card.prompt_per_million,
            "completion_per_million": card.completion_per_million,
        }
    });
    info!(?card, "Admin: updated rate card config");
    Ok(Json(resp))
}

/// GET /api/admin/config/rate-card — view current pricing.
///
/// RBAC: any authenticated user.
async fn get_rate_card(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    match &config.rate_config {
        Some(card) => Json(serde_json::json!({
            "prompt_per_million": card.prompt_per_million,
            "completion_per_million": card.completion_per_million,
        })),
        None => Json(serde_json::json!({
            "prompt_per_million": 0,
            "completion_per_million": 0,
            "note": "no rate card configured; all requests are free"
        })),
    }
}
