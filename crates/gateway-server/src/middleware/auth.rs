use axum::{
    extract::{Request, State},
    http::{self, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tracing::warn;

use crate::state::AppState;
use gateway_core::error::GatewayError;
use gateway_core::tenant::TenantContext;

/// Extracted API key fingerprint (`key_<first 8 chars of hash>`) stored in request extensions.
#[derive(Clone, Debug)]
pub struct AuthKey(pub String);

/// Map a GatewayError into an HTTP Response (status + JSON body).
fn error_response(e: GatewayError) -> Response {
    let status = http::StatusCode::from_u16(e.status_code())
        .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(e.to_error_response())).into_response()
}

/// Paths excluded from auth — K8s load balancers, health monitors, and our
/// own smoke tests. The Admin UI (page + static assets) now REQUIRES a valid
/// Bearer token; see S2 security fix.
const UNAUTHENTICATED_PATHS: &[&str] =
    &["/healthz", "/readyz", "/deep-health", "/health", "/metrics"];

/// Authentication middleware — validates the Bearer token against the HMAC store.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    // Skip auth if disabled — atomic read, no RwLock contention on the hot path (H3).
    let enabled = state
        .auth_enabled
        .load(std::sync::atomic::Ordering::Relaxed);

    let path = request.uri().path();
    if !enabled || UNAUTHENTICATED_PATHS.contains(&path) {
        return next.run(request).await;
    }

    match request.headers().get(header::AUTHORIZATION) {
        Some(value) => match value.to_str() {
            Ok(v) if v.starts_with("Bearer ") => {
                let key = &v[7..];
                let store = state.auth_store.read().await;
                if let Some(entry) = store.verify(key) {
                    // Inject the key_id fingerprint for downstream handlers.
                    request
                        .extensions_mut()
                        .insert(AuthKey(entry.key_id.clone()));
                    // Inject tenant context (MVP 1).
                    request.extensions_mut().insert(TenantContext {
                        tenant_id: entry.tenant_id.clone(),
                        role: gateway_core::tenant::Role::from_str(&entry.role),
                        key_id: entry.key_id.clone(),
                    });
                    next.run(request).await
                } else {
                    warn!("Invalid API key attempt");
                    error_response(GatewayError::AuthenticationFailed(
                        "Missing or invalid Authorization header".into(),
                    ))
                }
            }
            _ => error_response(GatewayError::AuthenticationFailed(
                "Invalid Authorization header format".into(),
            )),
        },
        None => error_response(GatewayError::AuthenticationFailed(
            "Missing Authorization header".into(),
        )),
    }
}
