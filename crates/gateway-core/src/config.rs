use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default = "crate::tenant::default_tenants")]
    pub tenants: HashMap<String, crate::tenant::TenantConfig>,
    /// Platform-wide default rate card (cost per 1M tokens) for metering.
    #[serde(default)]
    pub rate_config: Option<crate::metering::RateCard>,
}

/// Per-provider behavioral tweaks for OpenAI-compatible providers.
///
/// Kept in `gateway-core` (not the `providers` crate) so the trait definition
/// and its config can live without a circular dependency. The provider crate
/// re-exports this as its own `FieldOverrides` to keep a single source of truth.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FieldOverrides {
    /// Emit `reasoning_content` in the assistant response (DeepSeek).
    #[serde(default)]
    pub emit_reasoning_content: bool,
    /// Extra chat template kwargs merged into each request (DeepSeek Reasoner).
    #[serde(default)]
    pub chat_template_kwargs: Option<serde_json::Value>,
    /// Stream field renames: maps OAI field → upstream field when differs.
    #[serde(default)]
    pub stream_field_renames: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".into()
}

fn default_port() -> u16 {
    8080
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    pub models: Vec<String>,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// OpenAI-compat behavioral overrides (DeepSeek, vLLM, etc).
    #[serde(default)]
    pub field_overrides: Option<FieldOverrides>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: default_rpm(),
        }
    }
}

fn default_rpm() -> u32 {
    60
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cache_capacity")]
    pub max_capacity: u64,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_capacity: default_cache_capacity(),
            ttl_seconds: default_ttl(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_cache_capacity() -> u64 {
    1000
}

fn default_ttl() -> u64 {
    300
}

/// A structured auth entry: (plaintext_key, tenant_id, role).
#[derive(Debug, Clone, Deserialize)]
pub struct StructuredKey(pub String, pub String, pub String);

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Backward-compat: plaintext startup keys (auto-hashed into HMAC store on boot).
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Structured key entries with tenant + role (MVP 1).
    #[serde(default)]
    pub structured_keys: Vec<StructuredKey>,
    /// HMAC secret for tenant-wide signing; MVP 4 does not enforce its presence.
    #[serde(default)]
    pub required_hmac_secret: Option<String>,
    /// Default tenant for MVP compat.
    #[serde(default = "default_tenant")]
    pub default_tenant: String,
    /// Default role for new keys.
    #[serde(default = "default_role")]
    pub default_role: String,
}

fn default_tenant() -> String {
    "default".into()
}

fn default_role() -> String {
    "developer".into()
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_keys: vec!["test-key".into()],
            structured_keys: vec![],
            required_hmac_secret: None,
            default_tenant: default_tenant(),
            default_role: default_role(),
        }
    }
}

impl AppConfig {
    /// Load configuration from a TOML file. Falls back to defaults if path is empty.
    pub fn load(path: Option<&str>) -> Result<Self, crate::error::GatewayError> {
        let mut builder = config::Config::builder();

        if let Some(p) = path {
            builder = builder.add_source(config::File::with_name(p).required(true));
        } else {
            builder = builder.add_source(config::File::with_name("config").required(false));
        }

        // Environment overrides: AI_GATEWAY__SECTION__KEY=value
        builder = builder.add_source(
            config::Environment::with_prefix("AI_GATEWAY")
                .prefix_separator("__")
                .separator("__"),
        );

        let settings = builder
            .build()
            .map_err(|e| crate::error::GatewayError::ConfigError(e.to_string()))?;

        settings
            .try_deserialize()
            .map_err(|e| crate::error::GatewayError::ConfigError(e.to_string()))
    }

    /// Build a model → provider name lookup table.
    pub fn model_routing(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (provider_name, cfg) in &self.providers {
            for model in &cfg.models {
                map.insert(model.clone(), provider_name.clone());
            }
        }
        map
    }
}
