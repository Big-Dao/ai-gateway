mod common;
use common::TestServer;

fn u(s: &TestServer, p: &str) -> String {
    format!("{}{p}", s.base_url)
}

/// Spawn a server with structured_keys (`api_keys` TOML field) and a high
/// rate-limit so rate-limit middleware never interferes with quota testing.
#[allow(dead_code)]
async fn spawn_quota_test_server() -> TestServer {
    TestServer::spawn_with(
        &["admin-key", "dev-key"],
        &[
            ("AUTH__ENABLED", "true".into()),
            ("RATE_LIMIT__REQUESTS_PER_MINUTE", "1000".into()),
        ],
    )
    .await
}

// ─── Admin quota API: read/update per-tenant quota ─────────────────────────

#[tokio::test]
async fn admin_can_update_quota() {
    let client = reqwest::Client::new();
    let server = TestServer::spawn_with_keys(&[("admin-key", "default", "admin")]).await;

    let resp = client
        .put(u(&server, "/api/admin/config/quota/default"))
        .bearer_auth("admin-key")
        .json(&serde_json::json!({ "max_rpm": 42, "max_rpd": 0, "max_tpm": 0, "max_tpd": 0 }))
        .send()
        .await
        .expect("quota PUT");
    assert!(
        resp.status().is_success(),
        "quota update returned {}",
        resp.status()
    );

    // Read via tenants endpoint
    let resp = client
        .get(u(&server, "/api/admin/tenants"))
        .bearer_auth("admin-key")
        .send()
        .await
        .expect("tenants GET");
    assert!(resp.status().is_success());
    let tenants: Vec<serde_json::Value> = resp.json().await.unwrap();
    let default = tenants.iter().find(|t| t["id"] == "default").unwrap();
    assert_eq!(default["quotas"]["max_rpm"], 42);
}

// ─── Quota enforcement via HTTP: verify 429 after RPM exceeded ───────────────
///
/// Uses /v1/models (a no-provider-touching endpoint) so the circuit breaker
/// stays closed and quota is the only gate that trips.

#[tokio::test]
async fn quota_rpm_exceeded_via_http() {
    let client = reqwest::Client::new();
    // Rate-limit set very high so its bucket never trips before quota.
    let server = TestServer::spawn_with_keys_and_rpm(
        &[
            ("admin-key", "default", "admin"),
            ("dev-key", "default", "developer"),
        ],
        10_000,
    )
    .await;

    // Set quota to RPM=10 for the default tenant via admin API
    let resp = client
        .put(u(&server, "/api/admin/config/quota/default"))
        .bearer_auth("admin-key")
        .json(&serde_json::json!({ "max_rpm": 10, "max_rpd": 0, "max_tpm": 0, "max_tpd": 0 }))
        .send()
        .await
        .expect("quota PUT");
    assert!(
        resp.status().is_success(),
        "quota update should succeed, got {}",
        resp.status()
    );

    // 9 more requests (admin PUT consumed 1quota slot) should succeed
    for i in 0..9 {
        let resp = client
            .get(u(&server, "/v1/models"))
            .bearer_auth("dev-key")
            .send()
            .await
            .expect("within-quota request");
        assert!(
            resp.status().is_success(),
            "request {} within quota should succeed, got {}",
            i + 1,
            resp.status()
        );
    }

    // 10th quota-consuming request (11th total incl. the admin PUT) must be 429
    let resp = client
        .get(u(&server, "/v1/models"))
        .bearer_auth("dev-key")
        .send()
        .await
        .expect("over-quota request");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "request exceeding quota should be blocked with 429, got {}",
        resp.status()
    );
}
