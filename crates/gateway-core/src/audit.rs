use serde::{Deserialize, Serialize};
use std::fmt;

use crate::tenant::Role;

/// The category of auditable action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    // Authentication
    AuthLoginSuccess,
    AuthLoginFailure,
    AuthLogout,

    // API Key management
    KeyCreate,
    KeyRevoke,

    // Tenant management
    TenantCreate,
    TenantRead,
    TenantUpdate,
    TenantDelete,

    // Provider management
    ProviderCreate,
    ProviderRead,
    ProviderUpdate,
    ProviderDelete,

    // Configuration
    ConfigUpdate,

    // Quota
    QuotaUpdate,
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AuditAction::AuthLoginSuccess => "auth.login_success",
            AuditAction::AuthLoginFailure => "auth.login_failure",
            AuditAction::AuthLogout => "auth.logout",
            AuditAction::KeyCreate => "key.create",
            AuditAction::KeyRevoke => "key.revoke",
            AuditAction::TenantCreate => "tenant.create",
            AuditAction::TenantRead => "tenant.read",
            AuditAction::TenantUpdate => "tenant.update",
            AuditAction::TenantDelete => "tenant.delete",
            AuditAction::ProviderCreate => "provider.create",
            AuditAction::ProviderRead => "provider.read",
            AuditAction::ProviderUpdate => "provider.update",
            AuditAction::ProviderDelete => "provider.delete",
            AuditAction::ConfigUpdate => "config.update",
            AuditAction::QuotaUpdate => "quota.update",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for AuditAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auth.login_success" => Ok(AuditAction::AuthLoginSuccess),
            "auth.login_failure" => Ok(AuditAction::AuthLoginFailure),
            "auth.logout" => Ok(AuditAction::AuthLogout),
            "key.create" => Ok(AuditAction::KeyCreate),
            "key.revoke" => Ok(AuditAction::KeyRevoke),
            "tenant.create" => Ok(AuditAction::TenantCreate),
            "tenant.read" => Ok(AuditAction::TenantRead),
            "tenant.update" => Ok(AuditAction::TenantUpdate),
            "tenant.delete" => Ok(AuditAction::TenantDelete),
            "provider.create" => Ok(AuditAction::ProviderCreate),
            "provider.read" => Ok(AuditAction::ProviderRead),
            "provider.update" => Ok(AuditAction::ProviderUpdate),
            "provider.delete" => Ok(AuditAction::ProviderDelete),
            "config.update" => Ok(AuditAction::ConfigUpdate),
            "quota.update" => Ok(AuditAction::QuotaUpdate),
            other => Err(format!("Unknown audit action: {}", other)),
        }
    }
}

/// Describes who performed the action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditActor {
    /// The actor's identity — could be an API key id, admin user, or "system".
    pub id: String,
    /// The actor's role at the time of the action.
    #[serde(default = "default_role")]
    pub role: Role,
    /// Optional: the source IP address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Optional: additional context (user-agent, session id, etc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

fn default_role() -> Role {
    Role::DEVELOPER
}

/// A single auditable event — the core data type.
///
/// This is the type produced by admin handler instrumentation and consumed
/// by the async `AuditWriter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event identifier.
    pub id: String,
    /// When the event occurred (RFC3339).
    pub timestamp: String,
    /// Who performed the action.
    pub actor: AuditActor,
    /// What action was performed.
    pub action: AuditAction,
    /// The tenant scope this action was taken within.
    pub tenant: String,
    /// The resource targeted by the action (e.g. tenant id, key value, provider name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Arbitrary structured metadata (old/new values, request payload snippet, etc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Whether the action succeeded.
    #[serde(default = "default_success")]
    pub success: bool,
    /// If action failed, an error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_success() -> bool {
    true
}

impl AuditEvent {
    /// Build a new audit event with auto-generated id and timestamp.
    pub fn new(action: AuditAction, actor: AuditActor, tenant: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            actor,
            action,
            tenant: tenant.into(),
            resource: None,
            details: None,
            success: true,
            error: None,
        }
    }

    /// Set the target resource.
    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    /// Attach structured details.
    pub fn with_details(mut self, details: impl Serialize) -> Result<Self, serde_json::Error> {
        self.details = Some(serde_json::to_value(details)?);
        Ok(self)
    }

    /// Mark the event as a failure.
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.success = false;
        self.error = Some(error.into());
        self
    }

    /// Mark the event as successful (default state, useful for builder clarity).
    pub fn with_success(mut self) -> Self {
        self.success = true;
        self.error = None;
        self
    }
}

// Re-export at crate::audit with idiomatic convenience constructor.
impl AuditActor {
    pub fn system() -> Self {
        Self {
            id: "system".to_string(),
            role: Role::ADMIN,
            ip: None,
            context: None,
        }
    }

    pub fn user(id: impl Into<String>, role: Role) -> Self {
        Self {
            id: id.into(),
            role,
            ip: None,
            context: None,
        }
    }

    pub fn with_ip(mut self, ip: impl Into<String>) -> Self {
        self.ip = Some(ip.into());
        self
    }
}

/// Trait for async audit log writers.
///
/// Implementations write audit events to a backing store (in-memory buffer,
/// file, database, etc.) without blocking the request handler.
#[async_trait::async_trait]
pub trait AuditWriter: Send + Sync {
    /// Append an event to the audit log.
    async fn append(&self, event: AuditEvent) -> Result<(), AuditError>;
}

/// Errors that can occur when writing audit events.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("Audit writer channel closed")]
    ChannelClosed,

    #[error("Audit serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Audit IO error: {0}")]
    Io(String),

    #[error("Audit internal error: {0}")]
    Internal(String),
}

/// A no-op implementation for testing or when audit is disabled.
pub struct NoopAuditWriter;

#[async_trait::async_trait]
impl AuditWriter for NoopAuditWriter {
    async fn append(&self, _event: AuditEvent) -> Result<(), AuditError> {
        Ok(())
    }
}

/// Filter criteria for querying audit logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditFilter {
    /// Filter by actor id (substring match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Filter by specific action category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<AuditAction>,
    /// Filter by tenant id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Filter by success/failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    /// Inclusive start time (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Inclusive end time (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Pagination offset.
    #[serde(default)]
    pub offset: usize,
    /// Max number of results (default 100, max 1000).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for AuditFilter {
    fn default() -> Self {
        Self {
            actor_id: None,
            action: None,
            tenant: None,
            success: None,
            from: None,
            to: None,
            offset: 0,
            limit: default_limit(),
        }
    }
}

fn default_limit() -> usize {
    100
}

/// Paginated result of an audit log query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPage {
    /// Events in this page.
    pub events: Vec<AuditEvent>,
    /// Total number of events matching the filter.
    pub total: usize,
    /// Current offset.
    pub offset: usize,
    /// Whether more results exist.
    pub has_more: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_action_display() {
        assert_eq!(format!("{}", AuditAction::AuthLoginSuccess), "auth.login_success");
        assert_eq!(format!("{}", AuditAction::TenantCreate), "tenant.create");
        assert_eq!(format!("{}", AuditAction::ConfigUpdate), "config.update");
    }

    #[test]
    fn test_audit_action_roundtrip() {
        let actions = vec![
            AuditAction::AuthLoginSuccess,
            AuditAction::KeyRevoke,
            AuditAction::ProviderDelete,
            AuditAction::QuotaUpdate,
        ];
        for action in actions {
            let s = action.to_string();
            let parsed: AuditAction = s.parse().unwrap();
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn test_audit_event_builder() {
        let actor = AuditActor::user("key-123", Role::ADMIN);
        let event = AuditEvent::new(AuditAction::TenantCreate, actor, "acme-corp")
            .with_resource("acme-corp")
            .with_success();

        assert_eq!(event.action, AuditAction::TenantCreate);
        assert_eq!(event.tenant, "acme-corp");
        assert_eq!(event.resource.as_deref(), Some("acme-corp"));
        assert!(event.success);
        assert!(!event.id.is_empty());
    }

    #[test]
    fn test_audit_event_failure() {
        let actor = AuditActor::system();
        let event = AuditEvent::new(AuditAction::KeyRevoke, actor, "default")
            .with_error("Key not found");

        assert!(!event.success);
        assert_eq!(event.error.as_deref(), Some("Key not found"));
    }

    #[test]
    fn test_audit_actor_system() {
        let actor = AuditActor::system();
        assert_eq!(actor.id, "system");
        assert_eq!(actor.role, Role::ADMIN);
    }

    #[test]
    fn test_audit_event_serialization() {
        let actor = AuditActor::user("key-abc", Role::TENANT_ADMIN).with_ip("10.0.0.1");
        let event = AuditEvent::new(AuditAction::ProviderUpdate, actor, "tenant-x")
            .with_resource("openai");

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.action, AuditAction::ProviderUpdate);
        assert_eq!(deserialized.actor.id, "key-abc");
        assert_eq!(deserialized.actor.ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(deserialized.tenant, "tenant-x");
    }

    #[test]
    fn test_audit_action_deserialize() {
        let action: AuditAction = serde_json::from_str("\"key_create\"").unwrap();
        assert_eq!(action, AuditAction::KeyCreate);
        let action: AuditAction = serde_json::from_str("\"tenant_update\"").unwrap();
        assert_eq!(action, AuditAction::TenantUpdate);
    }

    #[test]
    fn test_audit_filter_default() {
        let filter = AuditFilter::default();
        assert_eq!(filter.limit, 100);
        assert_eq!(filter.offset, 0);
    }
}
