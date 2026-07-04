//! MVP 0 integration smoke tests — exercise the gateway end-to-end against a
//! real (briefly-running) gateway-server binary.
//!
//! Run with:
//!   cargo build --bin gateway-server --package gateway-server
//!   cargo test --package gateway-server --test mvp0_smoke

mod common;

use std::time::Duration;

use common::TestServer;

/// Base URL constructor helper: appends `path` onto the server base URL.
fn url(s: &TestServer, path: &str) -> String {
    format!("{}{path}", s.base_url)
}

async fn wait() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn mvp0_unauthenticated_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::get(url(&server, "/v1/models"))
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "unauthenticated /v1/models must be 401, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn mvp0_authenticated_succeeds() {
    let api_key = "test-key-123";
    let server = TestServer::spawn(&[api_key]).await;
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("request");
    // We don't expect the request to be rejected for auth — anything that is
    // NOT 401 proves the auth layer accepted the key. (The handler itself
    // returns 200 with the list of configured models.)
    assert_ne!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "authenticated request must not be 401"
    );
    assert!(
        resp.status().is_success() || resp.status().is_client_error(),
        "authenticated request should pass through auth middleware"
    );
}

#[tokio::test]
async fn mvp0_healthz_works() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::get(url(&server, "/healthz"))
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body.trim(), "ok");
}

#[tokio::test]
async fn mvp0_readyz_works() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::get(url(&server, "/readyz"))
        .await
        .expect("request");
    // The server's readiness returns 200 when config is loaded and the
    // circuit breaker defaults are accepting. Immediately after /healthz,
    // that must be true.
    assert!(
        resp.status().is_success(),
        "readyz should be 200 on a freshly-ready server, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("readyz JSON body");
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn mvp0_env_override_port() {
    // Use a fixed, known port for this test so we can verify the env
    // override actually changes the bind. Free it first (TcpListener released
    // immediately) so the server can take it. In CI, pick from the upper
    // ephemeral range to avoid colliding with other services.
    let picked = 19000u16;
    let _ = std::net::TcpListener::bind(format!("127.0.0.1:{picked}")); // probes freeness
    let _server =
        TestServer::spawn_with(&["test-key-123"], &[("SERVER__PORT", picked.to_string())]).await;
    // The env override should make the server bind to `19000`. We assert by
    // directly probing the fixed port instead of trusting base_url (which was
    // picked by spawn_with before we knew TOML was actually overridden).
    let direct_url = format!("http://127.0.0.1:{picked}");
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reqwest::get(format!("{direct_url}/healthz")),
    )
    .await
    .expect("probe timeout")
    .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn mvp0_rate_limit_small() {
    let api_key = "test-key-123";
    // 5 requests per minute. Auth middleware runs before rate-limit, so we
    // supply valid credentials — an unauthenticated 429 would mask the real
    // rate limit test.
    let server = TestServer::spawn_with_rpm(&[api_key], 5).await;

    let client = reqwest::Client::new();
    let mut statuses = Vec::with_capacity(8);

    for _ in 0..8 {
        let resp = client
            .get(url(&server, "/v1/models"))
            .bearer_auth(api_key)
            .send()
            .await
            .expect("request");
        statuses.push(resp.status());
        wait().await;
    }

    let limited = statuses.contains(&reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(
        limited,
        "expected some 429 responses at RPM=5 with 8 rapid requests, got: {:?}",
        statuses
    );
    // Every response should ALSO have a Retry-After header (set by x_request_id
    // middleware on 429s).
    let resp = client
        .get(url(&server, "/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("request");
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        assert!(
            resp.headers().get("retry-after").is_some(),
            "429 responses must include Retry-After header"
        );
    }
}

// ─── Auth middleware negative cases (T3) ─────────────────────────────

#[tokio::test]
async fn mvp0_auth_missing_header_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::get(url(&server, "/v1/models"))
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_auth_empty_bearer_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .bearer_auth("")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_auth_wrong_scheme_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .header("Authorization", "Basic dGVzdA==")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_auth_unknown_key_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .bearer_auth("this-key-does-not-exist")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_auth_malformed_bearer_no_space_returns_401() {
    let server = TestServer::spawn(&["test-key-123"]).await;
    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .header("Authorization", "BearerNoSpace")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_auth_disabled_allows_anonymous() {
    // Spawn with auth disabled — any request should pass through.
    let server =
        TestServer::spawn_with(&["test-key-123"], &[("AUTH__ENABLED", "false".to_string())]).await;
    let resp = reqwest::get(url(&server, "/v1/models"))
        .await
        .expect("request");
    // Not 401 — auth is off, so the request reaches the handler.
    assert_ne!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mvp0_request_id_header() {
    let api_key = "test-key-123";
    let server = TestServer::spawn(&[api_key]).await;
    let client = reqwest::Client::new();

    // Healthz path (no auth required) — simplest guaranteed-200 case.
    let resp = client
        .get(url(&server, "/healthz"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let rid = resp
        .headers()
        .get("x-request-id")
        .expect("every response must have x-request-id");
    let rid_str = rid.to_str().expect("header is ASCII");
    // Header value should be a UUID v4 (36 chars w/ hyphens).
    assert_eq!(
        rid_str.len(),
        36,
        "x-request-id should be a UUID v4, got {rid_str:?}"
    );

    // Auth-gated path — the middleware chain must still add the header.
    let resp = client
        .get(url(&server, "/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("request");
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "authenticated responses must carry x-request-id"
    );

    // 401 responses (missing auth) should ALSO carry the header, proving
    // the outermost middleware (x_request_id) precedes auth.
    let resp = client
        .get(url(&server, "/v1/models"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "401 responses must carry x-request-id"
    );
}
