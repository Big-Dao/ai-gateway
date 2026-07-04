//! OpenAI API contract tests — verify the gateway's `/v1/chat/completions`
//! and `/v1/models` endpoints conform to the OpenAI response schema.
//!
//! Run with:
//!   cargo build --bin gateway-server --package gateway-server
//!   cargo test --package gateway-server --test contract_test

mod common;

use common::TestServer;

fn url(s: &TestServer, path: &str) -> String {
    format!("{}{path}", s.base_url)
}

// ─── 1. /v1/models response schema ───────────────────────────────────

#[tokio::test]
async fn test_list_models_response_schema() {
    let api_key = "contract-key-models";
    let server = TestServer::spawn(&[api_key]).await;

    let resp = reqwest::Client::new()
        .get(url(&server, "/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("request");

    assert!(
        resp.status().is_success(),
        "authenticated /v1/models should succeed, got {}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().await.expect("parse JSON body");

    // Top-level: { "object": "list", "data": [ ... ] }
    assert_eq!(
        body["object"].as_str(),
        Some("list"),
        "body.object must be 'list', got {}",
        body["object"]
    );

    let data = body["data"].as_array().expect("body.data must be an array");
    assert!(!data.is_empty(), "body.data must not be empty");

    for (i, item) in data.iter().enumerate() {
        assert!(item["id"].is_string(), "data[{}].id must be a string", i);
        assert_eq!(
            item["object"].as_str(),
            Some("model"),
            "data[{}].object must be 'model'",
            i
        );
    }
}

// ─── 2. /v1/chat/completions response schema ─────────────────────────

#[tokio::test]
async fn test_chat_completions_response_schema() {
    let api_key = "contract-key-chat";
    let server = TestServer::spawn(&[api_key]).await;

    // Non-streaming request against the stub ollama provider (port 11111 —
    // nothing is listening). The upstream call fails, so the gateway wraps
    // the error in OpenAI format. This still validates the error-contract.
    let payload = serde_json::json!({
        "model": "smoke-model",
        "messages": [ { "role": "user", "content": "ping" } ],
        "stream": false
    });

    let resp = reqwest::Client::new()
        .post(url(&server, "/v1/chat/completions"))
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("parse JSON body");

    if status.is_success() {
        // Success contract: { "id", "object": "chat.completion", "choices": [...], "usage": {...} }
        assert!(
            body["id"].is_string(),
            "success response must have string 'id'"
        );
        assert_eq!(
            body["object"].as_str(),
            Some("chat.completion"),
            "success response object must be 'chat.completion'"
        );

        let choices = body["choices"]
            .as_array()
            .expect("choices must be an array");
        assert!(!choices.is_empty(), "choices must not be empty");

        let msg = &choices[0]["message"];
        assert_eq!(
            msg["role"].as_str(),
            Some("assistant"),
            "message.role must be 'assistant'"
        );
        assert!(
            msg["content"].is_string(),
            "message.content must be a string"
        );
        assert!(
            choices[0]["finish_reason"].is_string(),
            "choices[0].finish_reason must be a string"
        );

        let usage = &body["usage"];
        assert!(
            usage["prompt_tokens"].is_number(),
            "usage.prompt_tokens must be numeric"
        );
        assert!(
            usage["completion_tokens"].is_number(),
            "usage.completion_tokens must be numeric"
        );
        assert!(
            usage["total_tokens"].is_number(),
            "usage.total_tokens must be numeric"
        );
    } else {
        // Error contract: { "error": { "message": "...", "type": "...", "code": "..." } }
        let err = &body["error"];
        assert!(!err.is_null(), "error response must have 'error' field");
        assert!(err["message"].is_string(), "error.message must be a string");
        assert!(err["type"].is_string(), "error.type must be a string");
        // `code` is optional in OpenAI spec but our gateway always emits it.
        assert!(
            err["code"].is_string() || err["code"].is_null(),
            "error.code must be string or null"
        );
    }
}

// ─── 3. Unauthorized error format ────────────────────────────────────

#[tokio::test]
async fn test_unauthorized_error_format() {
    let server = TestServer::spawn(&["contract-key-auth"]).await;

    let resp = reqwest::get(url(&server, "/v1/models"))
        .await
        .expect("request");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "missing Bearer token must yield 401"
    );

    let body: serde_json::Value = resp.json().await.expect("parse JSON body");

    let err = &body["error"];
    assert!(!err.is_null(), "401 body must have 'error' field");
    assert!(err["message"].is_string(), "error.message must be a string");
    assert_eq!(
        err["type"].as_str(),
        Some("authentication_error"),
        "error.type must be 'authentication_error'"
    );
    assert!(
        err["code"].is_string(),
        "error.code must be a string (authentication_error)"
    );
}

// ─── 4. Rate-limit error format ──────────────────────────────────────

#[tokio::test]
async fn test_rate_limit_error_format() {
    let api_key = "contract-key-ratelimit";
    // Very low RPM so a rapid burst trips the limiter.
    let server = TestServer::spawn_with_rpm(&[api_key], 2).await;

    let client = reqwest::Client::new();
    let mut saw_429 = false;

    // Fire 8 rapid requests — at least one must be 429.
    for _ in 0..8 {
        let resp = client
            .get(url(&server, "/v1/models"))
            .bearer_auth(api_key)
            .send()
            .await
            .expect("request");

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;

            // Must include Retry-After header (capture before body move).
            assert!(
                resp.headers().get("retry-after").is_some(),
                "429 response must include Retry-After header"
            );

            // Body must be OpenAI error format.
            let body: serde_json::Value = resp.json().await.expect("parse JSON body");
            let err = &body["error"];
            assert!(!err.is_null(), "429 body must have 'error' field");
            assert!(err["message"].is_string(), "error.message must be a string");
            assert!(
                err["type"].is_string(),
                "error.type must be a string (rate_limit_exceeded or similar)"
            );

            break;
        }
    }

    assert!(
        saw_429,
        "expected at least one 429 in a rapid burst at RPM=2"
    );
}

// ─── 5. x-request-id header present on every response ────────────────

#[tokio::test]
async fn test_request_id_header_present() {
    let api_key = "contract-key-reqid";
    let server = TestServer::spawn(&[api_key]).await;
    let client = reqwest::Client::new();

    // 200 case — healthz (no auth required).
    let resp = client
        .get(url(&server, "/healthz"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "200 response must carry x-request-id"
    );

    // 401 case — no Bearer token.
    let resp = client
        .get(url(&server, "/v1/models"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "401 response must carry x-request-id"
    );

    // 200 authenticated case.
    let resp = client
        .get(url(&server, "/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("request");
    assert!(resp.status().is_success());
    let rid = resp
        .headers()
        .get("x-request-id")
        .expect("authenticated 200 must carry x-request-id")
        .to_str()
        .expect("header is ASCII");
    assert_eq!(rid.len(), 36, "x-request-id should be a UUID v4");
}

// ─── 6. Tenant-scoped isolation ──────────────────────────────────────

#[tokio::test]
async fn test_tenant_scoped_isolation() {
    // Two tenants, each with a tenant_admin key.
    let server = TestServer::spawn_with_keys(&[
        ("key-tenant-a", "tenant-a", "tenant_admin"),
        ("key-tenant-b", "tenant-b", "tenant_admin"),
    ])
    .await;

    let client = reqwest::Client::new();

    // Tenant A admin queries /api/admin/usage — should only see tenant-a.
    let resp = client
        .get(url(&server, "/api/admin/usage"))
        .bearer_auth("key-tenant-a")
        .send()
        .await
        .expect("request");

    assert!(
        resp.status().is_success(),
        "tenant_admin should be able to read own usage, got {}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().await.expect("parse JSON body");
    let list = body.as_array().expect("usage response must be an array");

    // Every entry must belong to tenant-a.
    for (i, entry) in list.iter().enumerate() {
        assert_eq!(
            entry["tenant_id"].as_str(),
            Some("tenant-a"),
            "tenant-a admin must only see tenant-a data, entry {} has tenant_id {:?}",
            i,
            entry["tenant_id"]
        );
    }

    // Same check for tenant-b.
    let resp = client
        .get(url(&server, "/api/admin/usage"))
        .bearer_auth("key-tenant-b")
        .send()
        .await
        .expect("request");
    assert!(resp.status().is_success());

    let body: serde_json::Value = resp.json().await.expect("parse JSON body");
    let list = body.as_array().expect("usage response must be an array");
    for (i, entry) in list.iter().enumerate() {
        assert_eq!(
            entry["tenant_id"].as_str(),
            Some("tenant-b"),
            "tenant-b admin must only see tenant-b data, entry {} has tenant_id {:?}",
            i,
            entry["tenant_id"]
        );
    }
}
