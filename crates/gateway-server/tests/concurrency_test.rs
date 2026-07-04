//! Concurrency integration tests for gateway-server.
//!
//! Verifies that shared-state primitives (metering, rate limiter, quota,
//! circuit breaker) behave correctly under concurrent load.

mod common;

use std::time::Duration;

use common::TestServer;

fn url(s: &TestServer, path: &str) -> String {
    format!("{}{path}", s.base_url)
}

// ─── Test 1: concurrent metering count ─────────────────────────────────

/// Fire 30 concurrent authenticated requests at a single tenant and verify
/// the metering layer records exactly 30 requests (no lost updates).
///
/// Uses a separate admin reader key in a different tenant so the final
/// usage-read request does not itself increment the metered tenant's count.
#[tokio::test]
async fn test_concurrent_metering_count() {
    let server = TestServer::spawn_with_keys(&[
        ("worker", "default", "developer"),
        ("reader", "reader-tenant", "admin"),
    ])
    .await;

    let client = reqwest::Client::new();
    let mut handles = Vec::with_capacity(30);

    for _ in 0..30 {
        let c = client.clone();
        let u = url(&server, "/v1/models");
        handles.push(tokio::spawn(async move {
            c.get(&u)
                .bearer_auth("worker")
                .send()
                .await
                .expect("request")
        }));
    }

    let mut ok = 0u64;
    for h in handles {
        let resp = h.await.expect("task panicked");
        let status = resp.status();
        assert!(
            status.is_success() || status.as_u16() == 429,
            "unexpected status {status}"
        );
        if status.is_success() {
            ok += 1;
        }
    }
    assert_eq!(ok, 30, "all 30 requests should succeed");

    // Give the async counters a moment to settle.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Read usage via the admin reader (different tenant — its GET does not
    // increment the metered tenant's counter).
    let usage: Vec<serde_json::Value> = client
        .get(url(&server, "/api/admin/usage"))
        .bearer_auth("reader")
        .send()
        .await
        .expect("usage request")
        .json()
        .await
        .expect("parse usage");

    let default_tenant = usage
        .iter()
        .find(|v| v["tenant_id"] == "default")
        .expect("default tenant visible to admin");
    assert_eq!(
        default_tenant["total_requests"].as_u64().unwrap(),
        30,
        "metering must record exactly 30 requests"
    );
}

// ─── Test 2: multi-tenant isolation ─────────────────────────────────────

/// Two tenants fire concurrent requests; each tenant's usage must reflect
/// only its own requests — no cross-tenant leakage.
///
/// Uses a separate admin reader in a third tenant so the final usage-read
/// does not perturb either metered tenant's counter.
#[tokio::test]
async fn test_multi_tenant_isolation() {
    let server = TestServer::spawn_with_keys(&[
        ("k1", "t1", "developer"),
        ("k2", "t2", "developer"),
        ("reader", "reader-tenant", "admin"),
    ])
    .await;

    let client = reqwest::Client::new();
    let mut handles = Vec::with_capacity(40);

    // Tenant t1 fires 20 requests, tenant t2 fires 20 requests.
    for _ in 0..20 {
        let c = client.clone();
        let u = url(&server, "/v1/models");
        handles.push(tokio::spawn(async move {
            c.get(&u)
                .bearer_auth("k1")
                .send()
                .await
                .expect("k1 request")
        }));
        let c = client.clone();
        let u = url(&server, "/v1/models");
        handles.push(tokio::spawn(async move {
            c.get(&u)
                .bearer_auth("k2")
                .send()
                .await
                .expect("k2 request")
        }));
    }

    for h in handles {
        let resp = h.await.expect("task panicked");
        assert!(
            resp.status().is_success(),
            "requests should succeed, got {}",
            resp.status()
        );
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Admin reader (third tenant) sees all tenants but its own request
    // only increments reader-tenant's counter.
    let usage: Vec<serde_json::Value> = client
        .get(url(&server, "/api/admin/usage"))
        .bearer_auth("reader")
        .send()
        .await
        .expect("usage request")
        .json()
        .await
        .expect("parse usage");

    let t1 = usage
        .iter()
        .find(|v| v["tenant_id"] == "t1")
        .expect("t1 visible to admin");
    let t2 = usage
        .iter()
        .find(|v| v["tenant_id"] == "t2")
        .expect("t2 visible to admin");

    assert_eq!(
        t1["total_requests"].as_u64().unwrap(),
        20,
        "t1 must see exactly its own 20 requests, not k2's"
    );
    assert_eq!(
        t2["total_requests"].as_u64().unwrap(),
        20,
        "t2 must see exactly its own 20 requests, not k1's"
    );
}

// ─── Test 3: rate limiter does not over-consume ─────────────────────────

/// With RPM=5, firing 20 concurrent requests must produce a mix of 200 and
/// 429 whose total is exactly 20 — no request silently disappears and no
/// more than 5 succeed (one bucket's worth of initial tokens).
#[tokio::test]
async fn test_rate_limiter_no_over_consume() {
    let api_key = "rate-limit-key";
    let server = TestServer::spawn_with_rpm(&[api_key], 5).await;

    let client = reqwest::Client::new();
    let mut handles = Vec::with_capacity(20);

    for _ in 0..20 {
        let c = client.clone();
        let u = url(&server, "/v1/models");
        let k = api_key.to_string();
        handles.push(tokio::spawn(async move {
            c.get(&u).bearer_auth(&k).send().await.expect("request")
        }));
    }

    let mut ok = 0u32;
    let mut limited = 0u32;
    for h in handles {
        let status = h.await.expect("task panicked").status();
        if status.is_success() {
            ok += 1;
        } else if status.as_u16() == 429 {
            limited += 1;
        } else {
            panic!("unexpected status {status}");
        }
    }

    assert_eq!(
        ok + limited,
        20,
        "every request must resolve to either 200 or 429"
    );
    println!("ok={ok} limited={limited}");
    assert!(ok <= 5, "at most RPM=5 requests may succeed, got {ok}");
    assert!(
        limited >= 15,
        "at least 15 requests must be rate-limited, got {limited}"
    );
}

// ─── Test 4: circuit breaker concurrent state ───────────────────────────

/// Drive the circuit breaker through concurrent failing `/v1/chat/completions`
/// calls against a stub provider that never responds.
///
/// Phase 1 fires enough concurrent failing calls to trip the breaker open
/// (default threshold = 5). Phase 2 fires more calls after the breaker is
/// open — these must be rejected at the `allow_request` gate, proving the
/// state machine transitions correctly under concurrency. The breaker must
/// never panic and must record `total_rejected > 0`.
#[tokio::test]
async fn test_circuit_breaker_concurrent_state() {
    let api_key = "cb-key";
    let server = TestServer::spawn(&[api_key]).await;
    let client = reqwest::Client::new();
    let cb_payload = serde_json::json!({
        "model": "smoke-model",
        "messages": [{"role": "user", "content": "hi"}]
    });

    // Phase 1: fire 8 concurrent failing calls to push past the failure
    // threshold (5) and trip the breaker open.
    let mut phase1 = Vec::with_capacity(8);
    for _ in 0..8 {
        let c = client.clone();
        let u = url(&server, "/v1/chat/completions");
        let k = api_key.to_string();
        let p = cb_payload.clone();
        phase1.push(tokio::spawn(async move {
            c.post(&u)
                .bearer_auth(&k)
                .json(&p)
                .header("content-type", "application/json")
                .send()
                .await
                .expect("request sent")
        }));
    }
    for h in phase1 {
        let _ = h.await.expect("phase 1 task panicked");
    }

    // Brief settle so the open-state write is visible.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Phase 2: fire more calls — the breaker is open so these are rejected.
    let mut phase2 = Vec::with_capacity(8);
    for _ in 0..8 {
        let c = client.clone();
        let u = url(&server, "/v1/chat/completions");
        let k = api_key.to_string();
        let p = cb_payload.clone();
        phase2.push(tokio::spawn(async move {
            c.post(&u)
                .bearer_auth(&k)
                .json(&p)
                .header("content-type", "application/json")
                .send()
                .await
                .expect("request sent")
        }));
    }
    for h in phase2 {
        let _ = h.await.expect("phase 2 task panicked");
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Poll the circuit-breaker status endpoint to verify final state.
    let status: serde_json::Value = client
        .get(url(&server, "/api/admin/circuit-breaker"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("cb status request")
        .json()
        .await
        .expect("parse cb status");

    let rejected = status["total_rejected"].as_u64().unwrap_or(0);
    let states = status["states"].as_object().expect("states map");
    let ollama_state = states
        .get("ollama")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    println!("total_rejected={rejected} ollama_state={ollama_state}");
    assert!(
        rejected > 0,
        "circuit breaker must have rejected some requests"
    );
    assert_eq!(
        ollama_state, "open",
        "after exceeding the failure threshold the breaker must be open"
    );
}
