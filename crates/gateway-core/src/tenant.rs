use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Role hierarchy — higher value = more privilege.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Role(pub u8);

impl Role {
    pub const DEVELOPER: Role = Role(10);
    pub const TENANT_ADMIN: Role = Role(50);
    pub const ADMIN: Role = Role(100);

    /// Parse a role from its wire string form, falling back to [`Role::DEVELOPER`]
    /// for unknown values.
    ///
    /// Intentionally not `std::str::FromStr`: this mapping is infallible
    /// (unknown → developer), whereas `FromStr` requires a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "admin" => Role::ADMIN,
            "tenant_admin" => Role::TENANT_ADMIN,
            _ => Role::DEVELOPER,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self.0 {
            100 => "admin",
            50 => "tenant_admin",
            _ => "developer",
        }
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Role::from_str(&s))
    }
}

impl Serialize for Role {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Per-request tenant context extracted from the API key.
#[derive(Debug, Clone)]
pub struct TenantContext {
    pub tenant_id: String,
    pub role: Role,
    pub key_id: String,
}

/// Tenant-level quota configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TenantQuotas {
    #[serde(default = "default_rpm")]
    pub max_rpm: u32,
    #[serde(default = "default_rpd")]
    pub max_rpd: u64,
    #[serde(default = "default_tpm")]
    pub max_tpm: u64,
    #[serde(default = "default_tpd")]
    pub max_tpd: u64,
}

impl Default for TenantQuotas {
    fn default() -> Self {
        Self {
            max_rpm: default_rpm(),
            max_rpd: default_rpd(),
            max_tpm: default_tpm(),
            max_tpd: default_tpd(),
        }
    }
}

fn default_rpm() -> u32 {
    60
}
fn default_rpd() -> u64 {
    10_000
}
fn default_tpm() -> u64 {
    500_000
}
fn default_tpd() -> u64 {
    5_000_000
}

/// Per-tenant configuration entry.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TenantConfig {
    #[serde(default)]
    pub quotas: TenantQuotas,
    #[serde(default)]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// Cost threshold (in cents) that, when crossed within the current billing
    /// window, triggers a one-shot `tracing::warn!` alert. `None` disables the
    /// alert for this tenant (default).
    #[serde(default)]
    pub cost_alert_threshold_cents: Option<f64>,
}

/// Build a default-tenant config map for backward compat.
pub fn default_tenants() -> HashMap<String, TenantConfig> {
    let mut map = HashMap::new();
    map.insert("default".to_string(), TenantConfig::default());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_ordering() {
        assert!(Role::ADMIN > Role::TENANT_ADMIN);
        assert!(Role::TENANT_ADMIN > Role::DEVELOPER);
        assert!(Role::DEVELOPER >= Role::DEVELOPER);
    }

    #[test]
    fn test_role_from_str() {
        assert_eq!(Role::from_str("admin"), Role::ADMIN);
        assert_eq!(Role::from_str("tenant_admin"), Role::TENANT_ADMIN);
        assert_eq!(Role::from_str("developer"), Role::DEVELOPER);
        assert_eq!(Role::from_str("unknown"), Role::DEVELOPER); // fallback
    }

    #[test]
    fn test_role_display() {
        assert_eq!(format!("{}", Role::ADMIN), "admin");
        assert_eq!(format!("{}", Role::TENANT_ADMIN), "tenant_admin");
        assert_eq!(format!("{}", Role::DEVELOPER), "developer");
    }

    #[test]
    fn test_default_quotas() {
        let q = TenantQuotas::default();
        assert_eq!(q.max_rpm, 60);
        assert_eq!(q.max_rpd, 10_000);
    }

    #[test]
    fn test_role_deserialize() {
        let r: Role = serde_json::from_str("\"tenant_admin\"").unwrap();
        assert_eq!(r, Role::TENANT_ADMIN);
        let r: Role = serde_json::from_str("\"admin\"").unwrap();
        assert_eq!(r, Role::ADMIN);
    }

    #[test]
    fn test_default_tenants_has_default() {
        let tenants = default_tenants();
        assert!(tenants.contains_key("default"));
    }
}
