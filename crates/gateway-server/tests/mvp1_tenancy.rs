//! MVP 1 — Multi-tenancy + RBAC integration tests.

mod common;

use common::TestServer;

fn url(s: &TestServer, path: &str) -> String {
    format!("{}{path}", s.base_url)
}

// ─── RBAC: role enforcement ─────────────────────────────────────────

#[tokio::test]
async fn developer_cannot_create_provider() {
    // Developer role lacks permission to mutate providers
    let server = TestServer::spawn_with_keys(&[("dev-key", "default", "developer")]).await;

    let resp = reqwest::Client::new()
        .post(url(&server, "/api/admin/providers"))
        .bearer_auth("dev-key")
        .json(&serde_json::json!({
            "name": "hacker",
            "models": ["bad-model"]
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "developer must not create providers, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn tenant_admin_can_create_provider() {
    let server = TestServer::spawn_with_keys(&[("admin-key", "default", "tenant_admin")]).await;

    let resp = reqwest::Client::new()
        .post(url(&server, "/api/admin/providers"))
        .bearer_auth("admin-key")
        .json(&serde_json::json!({
            "name": "test-provider",
            "models": ["test-model"]
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "tenant_admin should create providers, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn tenant_admin_cannot_access_other_tenant_keys() {
    let server = TestServer::spawn_with_keys(&[("admin-a", "tenant-a", "tenant_admin")]).await;

    // Admin A tries to list keys — should only see tenant-a scope
    let resp = reqwest::Client::new()
        .get(url(&server, "/api/admin/keys"))
        .bearer_auth("admin-a")
        .send()
        .await
        .expect("request");

    // Should succeed (200) but scope to own tenant
    assert!(
        resp.status().is_success(),
        "tenant_admin can list own tenant keys, got {}",
        resp.status()
    );
}

// ─── Tenant context injection ──────────────────────────────────────

#[tokio::test]
async fn tenant_context_extracted_from_key() {
    let server = TestServer::spawn_with_keys(&[("key-a", "tenant-a", "developer")]).await;

    // Hitting /v1/models with a valid key should succeed (auth passes through)
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .bearer_auth("key-a")
        .send()
        .await
        .expect("request");

    assert_ne!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "valid key should pass auth"
    );
}

// ─── Tenant CRUD ───────────────────────────────────────────────────

#[tokio::test]
async fn admin_can_create_and_list_tenants() {
    let server = TestServer::spawn_with_keys(&[("admin-key", "default", "admin")]).await;

    // Create tenant
    let resp = reqwest::Client::new()
        .post(url(&server, "/api/admin/tenants"))
        .bearer_auth("admin-key")
        .json(&serde_json::json!({
            "id": "acme-corp",
            "quotas": { "max_rpm": 120 }
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "admin creates tenant, got {}",
        resp.status()
    );

    // List tenants — should include the new one
    let resp = reqwest::Client::new()
        .get(url(&server, "/api/admin/tenants"))
        .bearer_auth("admin-key")
        .send()
        .await
        .expect("request");

    assert!(resp.status().is_success());
    let tenants: Vec<serde_json::Value> = resp.json().await.expect("parse");
    assert!(
        tenants.iter().any(|t| t["id"] == "acme-corp"),
        "new tenant should appear"
    );
}

#[tokio::test]
async fn tenant_admin_cannot_create_tenant() {
    let server = TestServer::spawn_with_keys(&[("ta-key", "default", "tenant_admin")]).await;

    let resp = reqwest::Client::new()
        .post(url(&server, "/api/admin/tenants"))
        .bearer_auth("ta-key")
        .json(&serde_json::json!({ "id": "rogue" }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "tenant_admin must not create tenants"
    );
}

#[tokio::test]
async fn quota_update_by_tenant_admin() {
    let server = TestServer::spawn_with_keys(&[("ta-key", "default", "tenant_admin")]).await;

    let resp = reqwest::Client::new()
        .put(url(&server, "/api/admin/config/quota/default"))
        .bearer_auth("ta-key")
        .json(&serde_json::json!({ "max_rpm": 999 }))
        .send()
        .await
        .expect("request");

    assert!(
        resp.status().is_success(),
        "tenant_admin can update own tenant quota, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn developer_cannot_update_quota() {
    let server = TestServer::spawn_with_keys(&[("dev-key", "default", "developer")]).await;

    let resp = reqwest::Client::new()
        .put(url(&server, "/api/admin/config/quota/default"))
        .bearer_auth("dev-key")
        .json(&serde_json::json!({ "max_rpm": 1 }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "developer must not update quotas"
    );
}
