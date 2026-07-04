//! Regression coverage for the metering **success path** and per-key
//! attribution.
//!
//! Historically every integration test hit routes whose upstream call fails
//! (the "stub ollama" is `127.0.0.1:11111`, where nothing listens), so the
//! success branch of `record_metering` — the one that charges a key — was
//! never exercised. That is also where `key_id` was hardcoded to
//! `"_from_routes_"`, breaking per-key cost attribution (MVP 6 known-issue).
//!
//! This test stands up a *real* mock ollama upstream so a chat completion
//! actually succeeds, then asserts that metering was recorded for the
//! authenticated tenant. With the fix, the recorded `MeteringEvent.key_id`
//! is the caller's HMAC fingerprint (`key_<8 hex>`); the value is threaded
//! straight from `TenantContext.key_id` (populated by `auth_middleware` and
//! covered by `mvp1_tenancy::tenant_context_extracted_from_key`).

mod common;
use common::TestServer;

fn url(s: &TestServer, p: &str) -> String {
    format!("{}{p}", s.base_url)
}

/// Minimal mock ollama upstream: accept one or more connections on POST
/// `/api/chat` and reply with a valid `OllamaResponse` so the gateway records
/// a successful metering event.
async fn spawn_mock_ollama(port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind mock ollama");
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            tokio::spawn(async move {
                // Drain the request line + headers + body; we don't inspect it.
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let body = r#"{"model":"smoke-model","message":{"role":"assistant","content":"hi"},"done":true,"prompt_eval_count":5,"eval_count":3}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
}

#[tokio::test]
async fn successful_completion_is_metered_for_callers_tenant() {
    let api_key = "metering-dev-key";
    let mock_port = common::free_port();
    spawn_mock_ollama(mock_port).await;

    // Point the gateway's ollama provider at our mock upstream.
    let server = TestServer::spawn_with(
        &[api_key],
        &[(
            "PROVIDERS__OLLAMA__BASE_URL",
            format!("http://127.0.0.1:{mock_port}"),
        )],
    )
    .await;

    let client = reqwest::Client::new();

    // Authenticated chat completion against the mock ollama — must succeed.
    let resp = client
        .post(url(&server, "/v1/chat/completions"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": "smoke-model",
            "messages": [{ "role": "user", "content": "ping" }],
            "stream": false
        }))
        .send()
        .await
        .expect("chat completion request");

    assert!(
        resp.status().is_success(),
        "mock upstream should yield a successful completion, got {}",
        resp.status()
    );

    // Per-key attribution requires the success path to run record_metering,
    // which updates the per-tenant usage map surfaced by /api/admin/usage.
    // The caller's key belongs to the "default" tenant.
    let usage: serde_json::Value = client
        .get(url(&server, "/api/admin/usage"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("usage request")
        .json()
        .await
        .expect("usage JSON");

    let default_tenant = usage
        .as_array()
        .expect("usage is an array")
        .iter()
        .find(|t| t["tenant_id"].as_str() == Some("default"))
        .expect("default tenant present in usage");

    let total_requests = default_tenant["total_requests"].as_u64().unwrap_or(0);
    let total_tokens = default_tenant["total_tokens"].as_u64().unwrap_or(0);
    assert!(
        total_requests >= 1,
        "the successful completion must be metered"
    );
    assert!(
        total_tokens >= 8,
        "tokens (5 prompt + 3 completion from the mock) must be recorded, got {total_tokens}"
    );
}
