use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tracing::info;

use gateway_core::audit::{AuditAction, AuditActor, AuditEvent, AuditWriter, NoopAuditWriter};
use gateway_core::auth_key::ApiKeyStore;
use gateway_core::config::{AppConfig, ProviderConfig};
use gateway_core::error::GatewayError;
use gateway_core::provider::LLMProvider;
use gateway_core::tenant::Role;
use gateway_core::types::*;
use providers::*;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::metrics::{MeteringService, PrometheusExporter, QuotaEngine};

/// Usage metrics tracked per API key.
#[derive(Debug, Default)]
pub struct Metrics {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_errors: u64,
    /// model_name -> request count
    pub per_model: HashMap<String, u64>,
}

/// Shared application state.
pub struct AppState {
    /// Application config (RwLock allows runtime modification via admin API).
    pub config: RwLock<AppConfig>,
    /// HMAC-hashed API key store.
    pub auth_store: RwLock<ApiKeyStore>,
    /// Maps model name → provider instance (protected by RwLock for hot-reload).
    pub providers: RwLock<HashMap<String, Arc<dyn LLMProvider>>>,
    /// Maps provider name → provider instance (for fallback chain lookups).
    pub provider_instances: RwLock<HashMap<String, Arc<dyn LLMProvider>>>,
    /// Response cache.
    pub cache: moka::future::Cache<String, ChatCompletionResponse>,
    /// Whether auth is enabled. Atomic so the middleware's hot path can check
    /// it without acquiring the config RwLock (H3).
    pub auth_enabled: AtomicBool,
    /// Usage metrics.
    pub metrics: Mutex<Metrics>,
    /// Per-provider circuit breaker.
    pub circuit_breaker: Arc<CircuitBreaker>,
    /// Per-tenant rate limiter. Each tenant gets an independent token bucket.
    pub rate_limiter: Arc<crate::middleware::rate_limit::RateLimiter>,
    pub metering: MeteringService,
    pub quota: QuotaEngine,
    pub prometheus: PrometheusExporter,
    /// Append-only audit writer. Defaults to NoopAuditWriter when no audit
    /// path is configured; swap in FileAuditWriter via `new_with_audit`.
    pub audit_log: Arc<dyn AuditWriter>,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self, GatewayError> {
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        let mut provider_instances: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();

        for (name, provider_cfg) in &config.providers {
            let builtin = matches!(name.as_ref(), "openai" | "anthropic" | "gemini" | "ollama");
            // Route unknown provider names, or any provider with field_overrides,
            // through the OpenAI-compat adapter.
            let provider: Arc<dyn LLMProvider> =
                if provider_cfg.field_overrides.is_some() || !builtin {
                    Arc::new(OpenAICompatProvider::new(
                        name,
                        provider_cfg.api_key.clone(),
                        provider_cfg
                            .base_url
                            .clone()
                            .unwrap_or_else(|| "https://api.openai.com/v1".into()),
                        provider_cfg.models.clone(),
                        provider_cfg.extra_headers.clone(),
                        provider_cfg.field_overrides.clone().unwrap_or_default(),
                    ))
                } else {
                    match name.as_str() {
                        "openai" => Arc::new(OpenAIProvider::new(
                            provider_cfg.api_key.clone(),
                            provider_cfg.base_url.clone(),
                            provider_cfg.extra_headers.clone(),
                        )),
                        "anthropic" => Arc::new(AnthropicProvider::new(
                            provider_cfg.api_key.clone(),
                            provider_cfg.base_url.clone(),
                            provider_cfg.extra_headers.clone(),
                        )),
                        "gemini" => Arc::new(GeminiProvider::new(
                            provider_cfg.api_key.clone(),
                            provider_cfg.base_url.clone(),
                            provider_cfg.extra_headers.clone(),
                        )),
                        "ollama" => Arc::new(OllamaProvider::new(
                            provider_cfg.api_key.clone(),
                            provider_cfg.base_url.clone(),
                            provider_cfg.extra_headers.clone(),
                        )),
                        _ => {
                            tracing::warn!("Unknown provider '{}', skipping", name);
                            continue;
                        }
                    }
                };

            for model in &provider_cfg.models {
                info!("Registering model '{}' → provider '{}'", model, name);
                providers.insert(model.clone(), provider.clone());
            }
            provider_instances.insert(name.clone(), provider.clone());
        }

        if providers.is_empty() {
            return Err(GatewayError::ConfigError(
                "No providers/models configured".into(),
            ));
        }

        // Prefer structured_keys (MVP 1) over plaintext api_keys.
        let auth_store = if !config.auth.structured_keys.is_empty() {
            let tuples: Vec<(String, String, String)> = config
                .auth
                .structured_keys
                .iter()
                .map(|k| (k.0.clone(), k.1.clone(), k.2.clone()))
                .collect();
            ApiKeyStore::from_structured_keys(&tuples)
        } else if !config.auth.api_keys.is_empty() {
            ApiKeyStore::from_plaintext_keys(&config.auth.api_keys)
        } else {
            ApiKeyStore::new()
        };

        let cache = moka::future::Cache::builder()
            .max_capacity(config.cache.max_capacity)
            .time_to_live(std::time::Duration::from_secs(config.cache.ttl_seconds))
            .build();

        let circuit_breaker = CircuitBreaker::new(CircuitBreakerConfig::default());

        let rpm = config.rate_limit.requests_per_minute;
        let rate_limiter = Arc::new(crate::middleware::rate_limit::RateLimiter::new(rpm));
        let auth_enabled = AtomicBool::new(config.auth.enabled);
        let mut metering = MeteringService::new();

        // P0 stage 4: if a metering store is configured, replay persisted
        // events so usage/cost survive restart (replaces the old startup
        // reset that zeroed cost on every boot). Otherwise (no metering_path,
        // e.g. in tests) fall back to clearing the in-memory window.
        if let Some(path) = &config.metering_path {
            let store = Arc::new(crate::persistence::FileMeteringStore::new(
                std::path::PathBuf::from(path),
            ));
            match store.load().await {
                Ok(events) => {
                    let n = events.len();
                    metering.load_events(events).await;
                    tracing::info!(
                        target: "billing",
                        events_loaded = n,
                        "replayed persisted metering events"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        target: "billing",
                        error = %e,
                        "failed to load metering store; starting fresh"
                    );
                    metering.reset_billing_window().await;
                }
            }
            metering.set_store(store);
        } else {
            metering.reset_billing_window().await;
            tracing::info!(target: "billing", "billing window reset on startup (no metering_path)");
        }

        Ok(Self {
            config: RwLock::new(config),
            auth_store: RwLock::new(auth_store),
            providers: RwLock::new(providers),
            provider_instances: RwLock::new(provider_instances),
            cache,
            auth_enabled,
            metrics: Mutex::new(Metrics::default()),
            circuit_breaker,
            rate_limiter,
            metering,
            quota: QuotaEngine::new(),
            prometheus: PrometheusExporter::new(),
            audit_log: Arc::new(NoopAuditWriter),
        })
    }

    /// Build an `AppState` with a file-based audit writer at `path`.
    pub async fn new_with_config_and_audit(
        config: AppConfig,
        audit_path: std::path::PathBuf,
    ) -> Result<Self, GatewayError> {
        let base = Self::new(config).await?;
        let writer = gateway_core::audit::FileAuditWriter::new(audit_path)
            .map_err(|e| GatewayError::ConfigError(e.to_string()))?;
        Ok(Self {
            audit_log: Arc::new(writer),
            ..base
        })
    }

    /// Emit an audit event. Convenience wrapper that swallows write errors
    /// (audit must never break the request path).
    pub async fn emit_audit(
        &self,
        action: AuditAction,
        actor: AuditActor,
        tenant: impl Into<String>,
        resource: Option<&str>,
        success: bool,
        error: Option<&str>,
    ) {
        let mut event = AuditEvent::new(action, actor, tenant);
        if let Some(r) = resource {
            event = event.with_resource(r);
        }
        if let Some(e) = error {
            event = event.with_error(e);
        } else if !success {
            event = event.with_error("unknown");
        }
        if let Err(e) = self.audit_log.append(event).await {
            tracing::warn!(error = %e, "failed to append audit event");
        }
    }

    /// Look up a provider by model name.
    pub async fn get_provider(&self, model: &str) -> Option<Arc<dyn LLMProvider>> {
        let providers = self.providers.read().await;
        providers.get(model).cloned()
    }

    /// Look up a provider instance by provider name (for fallback chain).
    pub async fn get_provider_by_name(&self, name: &str) -> Option<Arc<dyn LLMProvider>> {
        let instances = self.provider_instances.read().await;
        instances.get(name).cloned()
    }

    /// Register or update a provider at runtime with the given config.
    pub async fn register_provider(
        &self,
        name: &str,
        provider_cfg: &ProviderConfig,
    ) -> Result<(), GatewayError> {
        // If the provider has field_overrides, treat it as OpenAI-compatible.
        // Also fall back to compat mode for unknown provider names (not built-in).
        let builtin = matches!(name.as_ref(), "openai" | "anthropic" | "gemini" | "ollama");
        let provider: Arc<dyn LLMProvider> = if provider_cfg.field_overrides.is_some() || !builtin {
            Arc::new(OpenAICompatProvider::new(
                name,
                provider_cfg.api_key.clone(),
                provider_cfg
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into()),
                provider_cfg.models.clone(),
                provider_cfg.extra_headers.clone(),
                provider_cfg.field_overrides.clone().unwrap_or_default(),
            ))
        } else {
            match name {
                "openai" => Arc::new(OpenAIProvider::new(
                    provider_cfg.api_key.clone(),
                    provider_cfg.base_url.clone(),
                    provider_cfg.extra_headers.clone(),
                )),
                "anthropic" => Arc::new(AnthropicProvider::new(
                    provider_cfg.api_key.clone(),
                    provider_cfg.base_url.clone(),
                    provider_cfg.extra_headers.clone(),
                )),
                "gemini" => Arc::new(GeminiProvider::new(
                    provider_cfg.api_key.clone(),
                    provider_cfg.base_url.clone(),
                    provider_cfg.extra_headers.clone(),
                )),
                "ollama" => Arc::new(OllamaProvider::new(
                    provider_cfg.api_key.clone(),
                    provider_cfg.base_url.clone(),
                    provider_cfg.extra_headers.clone(),
                )),
                // Unreachable because !builtin caught above, but Rust needs it.
                _ => unreachable!(),
            }
        };

        // Store config
        self.config
            .write()
            .await
            .providers
            .insert(name.to_string(), provider_cfg.clone());

        // Register provider instance for fallback lookup
        self.provider_instances
            .write()
            .await
            .insert(name.to_string(), provider.clone());

        // Register models
        let mut providers = self.providers.write().await;
        // Remove old models that pointed to a previous instance of this provider
        let old_models: Vec<String> = self
            .config
            .read()
            .await
            .providers
            .get(name)
            .map(|old| old.models.clone())
            .unwrap_or_default();
        for old_model in &old_models {
            if !provider_cfg.models.contains(old_model) {
                providers.remove(old_model);
            }
        }
        for model in &provider_cfg.models {
            info!("Registering model '{}' → provider '{}'", model, name);
            providers.insert(model.clone(), provider.clone());
        }

        Ok(())
    }

    /// Record metrics after a request.
    pub async fn record_usage(&self, model: &str, usage: &Usage) {
        let mut m = self.metrics.lock().await;
        m.total_requests += 1;
        m.total_prompt_tokens += usage.prompt_tokens as u64;
        m.total_completion_tokens += usage.completion_tokens as u64;
        *m.per_model.entry(model.to_string()).or_insert(0) += 1;
    }

    /// Record an error.
    pub async fn record_error(&self) {
        let mut m = self.metrics.lock().await;
        m.total_errors += 1;
    }

    /// Update the default RPM used for subsequently-created tenant buckets.
    /// Existing buckets keep their current capacity — call this when you
    /// want NEW tenant buckets (created after this call) to use a new RPM.
    /// Admin `PUT /api/admin/config/rate-limit` calls this to enforce
    /// changes for future tenants immediately.
    pub fn update_rate_limit_config(&self, rpm: u32) {
        self.rate_limiter.set_default_rpm(rpm);
    }
}
