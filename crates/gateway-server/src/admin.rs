use axum::{
    extract::{Extension, Json, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Router,
};
use gateway_core::audit::AuditAction;
use gateway_core::metering::CostSummary;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::circuit_breaker::CircuitState;
use crate::metrics::metering::TenantUsage;
use crate::middleware::auth::AuthKey;
use crate::middleware::rbac::require_role;
use crate::routes::ApiError;
use crate::state::AppState;
use gateway_core::auth_key::ApiKeyEntry;
use gateway_core::tenant::Role;

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
    /// Per-billing-window cost threshold (cents). When a tenant's cumulative
    /// cost crosses this value, the metering service emits a one-shot warn
    /// event. Passing `Some(..)` arms the alert; `None` leaves it untouched.
    pub cost_alert_threshold_cents: Option<f64>,
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

#[derive(Debug, Deserialize)]
pub struct CostsParams {
    /// Time window: `24h` (default), `7d`, or `30d`.
    #[serde(default)]
    pub window: Option<String>,
    /// Tenant filter — only honoured for global admins.
    #[serde(default)]
    pub tenant: Option<String>,
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
            get(get_provider)
                .put(update_provider)
                .delete(delete_provider),
        )
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{key}", delete(delete_key))
        .route("/config/cache", put(update_cache_config))
        .route("/config/rate-limit", put(update_rate_limit))
        .route(
            "/config/rate-card",
            get(get_rate_card).put(update_rate_card),
        )
        .route("/config/quota/{tenant_id}", put(update_quota))
        .route("/usage/{tenant_id}", get(get_tenant_usage))
        .route("/usage", get(get_all_usage))
        .route("/tenants", get(list_tenants).post(create_tenant))
        .route("/tenants/{tenant_id}", delete(delete_tenant))
        .route("/logs", get(get_logs))
        .route("/circuit-breaker", get(get_circuit_breaker_status))
        .route("/costs", get(get_costs))
        .route("/billing/reset", post(post_billing_reset))
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<AdminMetricsResponse>, ApiError> {
    // RBAC: aggregate gateway metrics expose cross-tenant volume and config
    // shape — require at least tenant_admin (S2).
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

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

    Ok(Json(AdminMetricsResponse {
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
    }))
}

async fn list_providers(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<Vec<ProviderInfo>>, ApiError> {
    // RBAC: provider list exposes upstream base URLs and key presence —
    // tenant_admin minimum (S2).
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    let config = state.config.read().await;
    let providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(name, cfg)| ProviderInfo {
            name: name.clone(),
            api_key_set: cfg.api_key.as_ref().is_some_and(|k| !k.is_empty()),
            base_url: cfg.base_url.clone(),
            models: cfg.models.clone(),
        })
        .collect();

    Ok(Json(providers))
}

async fn get_provider(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(name): Path<String>,
) -> Result<Json<ProviderInfo>, ApiError> {
    // RBAC: provider detail exposes upstream base URL — tenant_admin (S2).
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    let config = state.config.read().await;
    let cfg = config.providers.get(&name).ok_or_else(|| {
        ApiError(gateway_core::error::GatewayError::ProviderNotFound(
            name.clone(),
        ))
    })?;

    Ok(Json(ProviderInfo {
        name,
        api_key_set: cfg.api_key.as_ref().is_some_and(|k| !k.is_empty()),
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

    info!(
        "Admin: creating provider '{}' with models {:?}",
        req.name, req.models
    );

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

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"status": "created"})),
    )
        .into_response())
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
            return Err(ApiError(
                gateway_core::error::GatewayError::ProviderNotFound(name),
            ));
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
    state
        .register_provider(&name, &cfg)
        .await
        .map_err(ApiError)?;

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
            return Err(ApiError(
                gateway_core::error::GatewayError::ProviderNotFound(name),
            ));
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
        store.verify_by_id(&auth_key.0).map(|e| Role::from_name(&e.role)),
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

    // Resolve the caller's full role (require_role above only checked the floor).
    let caller_role = {
        let store = state.auth_store.read().await;
        store
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_name(&e.role))
            .unwrap_or(Role::DEVELOPER)
    };

    // Target tenant: tenant_admins may only create keys in their own tenant;
    // only global admin may target a different tenant.
    let target_tenant = req.tenant_id.unwrap_or_else(|| tenant_id.clone());
    if target_tenant != tenant_id {
        require_role(&state, &auth_key, Role::ADMIN)
            .await
            .map_err(ApiError)?;
    }

    // Target role: must be a known role name (reject unknown strings instead
    // of silently downgrading to developer), and non-admin callers may NOT
    // escalate privileges by minting tenant_admin/admin keys.
    // (S2 fix: vertical privilege escalation via create_key.)
    let target_role = req.role.clone().unwrap_or_else(|| "developer".to_string());
    match target_role.as_str() {
        "developer" | "tenant_admin" | "admin" => {}
        _ => {
            return Err(ApiError(gateway_core::error::GatewayError::BadRequest(
                format!("unknown role '{target_role}'"),
            )))
        }
    }
    if caller_role < Role::ADMIN && Role::from_name(&target_role) >= Role::TENANT_ADMIN {
        return Err(ApiError(gateway_core::error::GatewayError::Forbidden(
            "insufficient privileges to create an elevated-role key".into(),
        )));
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
            target_tenant.clone(),
            target_role,
        ));

    // Also add to the in-memory store so it works immediately.
    let new_key_id = {
        let entry = ApiKeyEntry::new(&plaintext_key, &tenant_for_store, &role_for_store);
        let kid = entry.key_id.clone();
        state.auth_store.write().await.add(entry);
        kid
    };

    // Audit: KeyCreate. Logs only the derived key_id — never the plaintext.
    state
        .emit_audit(
            AuditAction::KeyCreate,
            gateway_core::audit::AuditActor {
                id: auth_key.0.clone(),
                role: caller_role,
                ip: None,
                context: None,
            },
            target_tenant.clone(),
            Some(new_key_id.as_str()),
            true,
            None,
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"status": "created", "key_id": new_key_id})),
    )
        .into_response())
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Path(plaintext_key): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: requires tenant_admin; scoped to own tenant.
    let caller_tenant = require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    // Resolve caller role for the audit record.
    let caller_role = {
        let store = state.auth_store.read().await;
        store
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_name(&e.role))
            .unwrap_or(Role::DEVELOPER)
    };

    // Locate the entry by verifying the plaintext key. We take the plaintext
    // (not the key_id fingerprint) so we can also remove the matching record
    // from the config store, which holds plaintext — the only stable bridge
    // between the in-memory HMAC store and the config source. (S2 fix: the
    // previous impl compared the fingerprint against plaintext and never
    // matched, so revocation was a silent no-op until process restart.)
    let entry = {
        let store = state.auth_store.read().await;
        store.verify(&plaintext_key).cloned().ok_or_else(|| {
            ApiError(gateway_core::error::GatewayError::BadRequest(
                "API key not found".into(),
            ))
        })?
    };

    // Tenant scope: deleting a key in another tenant requires global admin.
    if entry.tenant_id != caller_tenant {
        require_role(&state, &auth_key, Role::ADMIN)
            .await
            .map_err(ApiError)?;
    }

    // Remove from the in-memory store (immediate revocation) ...
    state.auth_store.write().await.remove_by_id(&entry.key_id);

    // ... and from the config source (prevents resurrection on restart).
    {
        let mut config = state.config.write().await;
        config.auth.structured_keys.retain(|k| k.0 != plaintext_key);
        config.auth.api_keys.retain(|k| k != &plaintext_key);
    }

    // Audit: KeyRevoke. Logs the key_id fingerprint only, never the plaintext.
    state
        .emit_audit(
            AuditAction::KeyRevoke,
            gateway_core::audit::AuditActor {
                id: auth_key.0.clone(),
                role: caller_role,
                ip: None,
                context: None,
            },
            caller_tenant,
            Some(entry.key_id.as_str()),
            true,
            None,
        )
        .await;

    info!(key_id = %entry.key_id, "Admin: removed API key");
    Ok(Json(serde_json::json!({
        "status": "deleted",
        "key_id": entry.key_id,
    })))
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
    if let Some(threshold) = req.cost_alert_threshold_cents {
        tenant.cost_alert_threshold_cents = Some(threshold);
    }

    info!(tenant = %tenant_id, "Admin: updated tenant quotas");

    let caller_role = {
        let store = state.auth_store.read().await;
        store
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_name(&e.role))
            .unwrap_or(Role::DEVELOPER)
    };
    state
        .emit_audit(
            AuditAction::QuotaUpdate,
            gateway_core::audit::AuditActor {
                id: auth_key.0,
                role: caller_role,
                ip: None,
                context: None,
            },
            caller_tenant,
            Some(tenant_id.as_str()),
            true,
            None,
        )
        .await;

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
        state.auth_store.read().await.verify_by_id(&auth_key.0).map(|e| Role::from_name(&e.role)),
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
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"status": "created"})),
    )
        .into_response())
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
    config.tenants.remove(&tenant_id).ok_or_else(|| {
        ApiError(gateway_core::error::GatewayError::BadRequest(format!(
            "Tenant '{}' not found",
            tenant_id
        )))
    })?;

    info!(tenant = %tenant_id, "Admin: deleted tenant");
    Ok(Json(serde_json::json!({"status": "deleted"})))
}

async fn update_cache_config(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<UpdateCacheConfigRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: mutating global cache config requires global admin (S2).
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

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
    Ok(Json(serde_json::json!({
        "status": "updated",
        "cache": {
            "enabled": config.cache.enabled,
            "max_capacity": config.cache.max_capacity,
            "ttl_seconds": config.cache.ttl_seconds,
        }
    })))
}

async fn update_rate_limit(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Json(req): Json<UpdateRateLimitRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: mutating the global rate-limit requires global admin (S2).
    // Without this any developer could push the token bucket to u32::MAX and
    // effectively disable rate limiting.
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

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
    Ok(Json(serde_json::json!({
        "status": "updated",
        "requests_per_minute": rpm,
    })))
}

async fn get_logs(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<Vec<crate::log_buffer::LogEntry>>, ApiError> {
    // RBAC: the log buffer contains cross-tenant request detail (model,
    // tenant, provider) — admin only (S2).
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;
    Ok(Json(crate::log_buffer::LOG_BUFFER.entries()))
}

async fn get_circuit_breaker_status(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: any authenticated caller may read breaker state (it carries no
    // tenant secrets), but authentication is now required (S2 — previously
    // this endpoint was reachable by an unauthenticated developer-class key
    // path, and the role gate was missing entirely).
    require_role(&state, &auth_key, Role::DEVELOPER)
        .await
        .map_err(ApiError)?;

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

    Ok(Json(serde_json::json!({
        "states": states_map,
        "total_rejected": total_rejected,
    })))
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
            .map(|e| Role::from_name(&e.role)),
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

/// POST /api/admin/billing/reset — clear all cost counters and detail
/// events. Bound to the current (daily) billing window: request and token
/// tallies are intentionally left untouched since they are cumulative, not
/// per-cycle metrics.
///
/// RBAC: only global admin can trigger a reset.
async fn post_billing_reset(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: only global admin may reset the billing window.
    require_role(&state, &auth_key, Role::ADMIN)
        .await
        .map_err(ApiError)?;

    let caller_role = {
        let store = state.auth_store.read().await;
        store
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_name(&e.role))
            .unwrap_or(Role::DEVELOPER)
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    state.metering.reset_billing_window().await;

    // Emit audit: BillingReset. The action is intentionally dedicated so the
    // audit stream can be filtered for billing lifecycle events.
    state
        .emit_audit(
            AuditAction::BillingReset,
            gateway_core::audit::AuditActor {
                id: auth_key.0.clone(),
                role: caller_role,
                ip: None,
                context: Some(serde_json::json!({ "reset_at_ms": now_ms })),
            },
            "default",
            Some("billing/reset"),
            true,
            None,
        )
        .await;

    info!(actor = %auth_key.0, "admin: billing window reset");

    Ok(Json(serde_json::json!({"status": "reset"})))
}

/// GET /api/admin/config/rate-card — view current pricing.
///
/// RBAC: any authenticated user.
/// GET /api/admin/costs?window=24h|7d|30d&tenant=<id>
///
/// RBAC: any authenticated user. Non-admin callers are implicitly scoped to
/// their own tenant — the `?tenant=` query param is only honoured for global
/// admins.
async fn get_costs(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
    Query(params): Query<CostsParams>,
) -> Result<Json<CostSummary>, ApiError> {
    // RBAC: any authenticated developer can view costs.
    let caller_tenant = require_role(&state, &auth_key, Role::DEVELOPER)
        .await
        .map_err(ApiError)?;

    let is_admin = matches!(
        state
            .auth_store
            .read()
            .await
            .verify_by_id(&auth_key.0)
            .map(|e| Role::from_name(&e.role)),
        Some(role) if role >= Role::ADMIN
    );

    // Non-admins must always be scoped to their own tenant; explicit
    // ?tenant= is an admin-only affordance.
    let tenant_filter = if is_admin {
        params.tenant.as_deref()
    } else {
        Some(caller_tenant.as_str())
    };

    // Parse window into (ms, human-readable label).
    let (window_ms, window_label): (u64, &str) = match params.window.as_deref() {
        Some("7d") => (7 * 24 * 60 * 60 * 1000, "7d"),
        Some("30d") => (30 * 24 * 60 * 60 * 1000, "30d"),
        _ => (24 * 60 * 60 * 1000, "24h"),
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut summary = state
        .metering
        .cost_summary(window_ms, now_ms, tenant_filter)
        .await;
    summary.window = window_label.to_string();

    Ok(Json(summary))
}

async fn get_rate_card(
    State(state): State<Arc<AppState>>,
    Extension(auth_key): Extension<AuthKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // RBAC: pricing is commercially sensitive — tenant_admin minimum (S2).
    require_role(&state, &auth_key, Role::TENANT_ADMIN)
        .await
        .map_err(ApiError)?;

    let config = state.config.read().await;
    match &config.rate_config {
        Some(card) => Ok(Json(serde_json::json!({
            "prompt_per_million": card.prompt_per_million,
            "completion_per_million": card.completion_per_million,
        }))),
        None => Ok(Json(serde_json::json!({
            "prompt_per_million": 0,
            "completion_per_million": 0,
            "note": "no rate card configured; all requests are free"
        }))),
    }
}
