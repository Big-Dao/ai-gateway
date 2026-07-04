use tracing::warn;

use crate::middleware::auth::AuthKey;
use crate::state::AppState;
use gateway_core::error::GatewayError;
use gateway_core::tenant::Role;

/// Check that the authenticated key meets the minimum role requirement.
/// Returns `Ok(tenant_id)` on success, or a `GatewayError` (401/403) on failure.
pub async fn require_role(
    state: &AppState,
    auth_key: &AuthKey,
    min_role: Role,
) -> Result<String, GatewayError> {
    let store = state.auth_store.read().await;
    let entry = store
        .verify_by_id(&auth_key.0)
        .ok_or_else(|| GatewayError::AuthenticationFailed("Key not found".into()))?;

    let role = Role::from_str(&entry.role);
    if role < min_role {
        warn!(
            key_id = %auth_key.0,
            role = %role,
            required = %min_role,
            "RBAC: insufficient permissions"
        );
        return Err(GatewayError::Forbidden(format!(
            "Requires at least '{}' role, but key has '{}'",
            min_role, role
        )));
    }

    Ok(entry.tenant_id.clone())
}

/// Check that the authenticated key belongs to the given tenant (or is global admin).
#[allow(dead_code)] // reserved for tenant-scoped admin endpoints
pub async fn require_tenant(
    state: &AppState,
    auth_key: &AuthKey,
    tenant_id: &str,
) -> Result<(), GatewayError> {
    let store = state.auth_store.read().await;
    let entry = store
        .verify_by_id(&auth_key.0)
        .ok_or_else(|| GatewayError::AuthenticationFailed("Key not found".into()))?;

    let role = Role::from_str(&entry.role);
    if entry.tenant_id != tenant_id && role < Role::ADMIN {
        return Err(GatewayError::Forbidden(format!(
            "Key does not belong to tenant '{}'",
            tenant_id
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use gateway_core::auth_key::{ApiKeyEntry, ApiKeyStore};
    use gateway_core::tenant::Role;

    #[test]
    fn test_role_from_str_matches() {
        assert_eq!(Role::from_str("admin"), Role::ADMIN);
        assert_eq!(Role::from_str("tenant_admin"), Role::TENANT_ADMIN);
        assert_eq!(Role::from_str("developer"), Role::DEVELOPER);
    }

    #[test]
    fn test_apikey_entry_role_field() {
        let entry = ApiKeyEntry::new("test-key", "default", "tenant_admin");
        assert_eq!(entry.tenant_id, "default");
        assert_eq!(entry.role, "tenant_admin");
    }

    #[test]
    fn test_verify_by_id() {
        let mut store = ApiKeyStore::new();
        let entry = ApiKeyEntry::new("my-secret", "tenant-a", "developer");
        let id = entry.key_id.clone();
        store.add(entry);

        let found = store.verify_by_id(&id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().tenant_id, "tenant-a");

        assert!(store.verify_by_id("nonexistent").is_none());
    }
}
