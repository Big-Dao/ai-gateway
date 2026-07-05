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

/// Paths excluded from auth — K8s liveness/readiness probes only.
///
/// NOTE (S2): `/metrics` was previously on this list, which leaked
/// per-tenant usage volumes, token consumption and error rates to any
/// unauthenticated caller. It now requires a valid API key (any role).
/// Prometheus scrapers must supply a bearer token — see deploy/servicemonitor.yaml.
const UNAUTHENTICATED_PATHS: &[&str] = &["/healthz", "/readyz", "/deep-health", "/health"];

/// Whether the request path is exempt from the Bearer-token requirement.
///
/// Exact-match paths are listed above; we also unconditionally allow `/admin`
/// and `/admin/*` because the admin UI is static HTML/CSS/JS served to a plain
/// browser page-load (which has no Bearer header). Authorisation for the admin
/// *REST* API (`/api/admin/*`) is done separately — `admin.js` injects the key
/// from `localStorage` per-request, so those paths must NOT be matched here.
fn is_unauthenticated(path: &str) -> bool {
    UNAUTHENTICATED_PATHS.contains(&path) || path == "/admin" || path.starts_with("/admin/")
}

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
    if !enabled || is_unauthenticated(path) {
        return next.run(request).await;
    }

    match request.headers().get(header::AUTHORIZATION) {
        Some(value) => match value.to_str() {
            Ok(v) if v.starts_with("Bearer ") => {
                let key = &v[7..];
                // Verify the key and clone the matching entry, then DROP the
                // read lock before dispatching to the handler. Holding the
                // auth_store read lock across `next.run(...)` deadlocks any
                // handler that takes the auth_store write lock (create_key,
                // delete_key) — a latent bug surfaced by end-to-end testing.
                let entry = {
                    let store = state.auth_store.read().await;
                    store.verify(key).cloned()
                };
                if let Some(entry) = entry {
                    // Inject the key_id fingerprint for downstream handlers.
                    request
                        .extensions_mut()
                        .insert(AuthKey(entry.key_id.clone()));
                    // Inject tenant context (MVP 1).
                    request.extensions_mut().insert(TenantContext {
                        tenant_id: entry.tenant_id.clone(),
                        role: gateway_core::tenant::Role::from_name(&entry.role),
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
