use axum::{
    extract::{Request, State},
    http::{self, header},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use tracing::warn;

use crate::state::AppState;
use gateway_core::error::GatewayError;
use gateway_core::tenant::TenantContext;

fn error_response(e: GatewayError) -> Response {
    let status = http::StatusCode::from_u16(e.status_code())
        .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
    let retry_after = match &e {
        GatewayError::QuotaExceeded { .. } | GatewayError::RateLimited => Some("60".to_string()),
        _ => None,
    };
    let body = Json(e.to_error_response());
    match retry_after {
        Some(v) => (status, [(header::RETRY_AFTER, v)], body).into_response(),
        None => (status, body).into_response(),
    }
}

pub async fn quota_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    // Skip for unauthenticated paths
    let path = request.uri().path();
    if path.starts_with("/healthz") || path.starts_with("/readyz") || path == "/metrics" {
        return next.run(request).await;
    }

    // Extract tenant context from auth
    let (tenant_id, quotas) = {
        let config = state.config.read().await;
        let ctx = request.extensions().get::<TenantContext>().cloned();
        match ctx {
            Some(c) => {
                let quotas = config
                    .tenants
                    .get(&c.tenant_id)
                    .map(|t| t.quotas.clone())
                    .unwrap_or_default();
                (c.tenant_id, quotas)
            }
            None => {
                // No context: pass through
                return next.run(request).await;
            }
        }
    };

    // Check quota with a conservative token estimate
    let token_estimate = 100u64; // default conservative pre-flight estimate
    if let Err(violation) = state.quota.check(&tenant_id, &quotas, token_estimate).await {
        warn!(tenant = %tenant_id, limit_type = %violation.limit_type, "quota middleware: blocked");
        return error_response(violation.into());
    }

    // Record that a request happened (lightweight path — no MeteringEvent placeholder)
    state.metering.record_request(&tenant_id, 0, 0).await;

    next.run(request).await
}
