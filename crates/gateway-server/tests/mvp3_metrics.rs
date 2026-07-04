mod common;
use common::TestServer;

fn u(s: &TestServer, p: &str) -> String {
    format!("{}{p}", s.base_url)
}

#[tokio::test]
async fn mvp3_metrics_content_type_text_plain() {
    let s = TestServer::spawn(&["test-key"]).await;
    // /metrics now requires authentication (S2) — supply a valid key.
    let r = reqwest::Client::new()
        .get(u(&s, "/metrics"))
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let ct = r.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(
        ct.starts_with("text/plain"),
        "expected text/plain, got {ct}"
    );
}

#[tokio::test]
async fn mvp3_metrics_contains_requests_total() {
    let s = TestServer::spawn(&["test-key"]).await;
    // Make a real request so Prometheus vec metrics get initialized with labels
    let _ = reqwest::Client::new()
        .get(u(&s, "/v1/models"))
        .bearer_auth("test-key")
        .send()
        .await;

    // /metrics now requires authentication (S2).
    let r = reqwest::Client::new()
        .get(u(&s, "/metrics"))
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    let body: &str = &r.text().await.unwrap();

    // ALL 9 metrics declared in PrometheusExporter should render (even with zero values)
    // because they were registered with the registry and use IntCounter/IntGauge types.
    for metric in &[
        "gateway_requests_total",
        "gateway_tokens_total",
        "gateway_errors_total",
        "gateway_cache_hits_total",
        "gateway_cache_misses_total",
        "gateway_request_duration_seconds",
        "gateway_active_requests",
        "gateway_circuit_breaker_state",
        "gateway_rate_limit_remaining",
    ] {
        assert!(
            body.contains(metric),
            "missing metric: {metric},\nbody:\n{body}"
        );
    }

    // Verify the output format looks like Prometheus text exposition
    assert!(body.contains("# HELP"), "missing # HELP lines");
    assert!(body.contains("# TYPE"), "missing # TYPE lines");
}

#[tokio::test]
async fn mvp3_healthz_returns_200() {
    let s = TestServer::spawn(&["test-key"]).await;
    let r = reqwest::get(u(&s, "/healthz")).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap().trim(), "ok");
}

#[tokio::test]
async fn mvp3_readyz_returns_200() {
    let s = TestServer::spawn(&["test-key"]).await;
    let r = reqwest::get(u(&s, "/readyz")).await.unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["status"], "ready");
}

#[tokio::test]
async fn mvp3_deep_health_json() {
    let s = TestServer::spawn(&["test-key"]).await;
    let r = reqwest::get(u(&s, "/deep-health")).await.unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["status"], "ok");
    assert!(v.get("metrics").is_some());
    assert!(v["providers"].is_array());
}
