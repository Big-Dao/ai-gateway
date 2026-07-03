# MVP 3 — 可观测性 Implementation Plan (Roadmap Stub)

> **For agentic workers:** 本文件为 roadmap stub，待 MVP 0/1/2 完成后展开为 bite-sized TDD plan。

**Goal:** Prometheus `/metrics` 端点 + 分级健康检查 + K8s 部署清单 + graceful shutdown

**Prerequisite:** MVP 0 + MVP 1 + MVP 2

---

## Roadmap Tasks (待展开)

### T1: Prometheus `/metrics` 端点
- 新增 `prometheus` crate 到 workspace
- 实现 `MetricsExporter` 注册 8 个关键指标（见 spec 5.1 表）
- 直方图桶 `[0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60]`
- 测试：GET /metrics 输出 Prometheus 格式

### T2: 分级健康检查
- `/healthz` (liveness) — 200 即活
- `/readyz` (readiness) — 配置加载 + cache 就绪 + ≥1 provider circuit-closed
- `/deep-health` — 含每个 provider 最近 5 次调用 snapshot
- 测试：各端点返回码与 JSON body

### T3: K8s 部署清单
- `deploy/` 目录：namespace / configmap / secret.template / deployment / service / ingress / servicemonitor / prometheus-rules
- HPA 基于 `gateway_active_requests`
- PVC 挂载 `metrics_snapshots/`

### T4: Graceful Shutdown
- `axum::serve.with_graceful_shutdown(ctrl_c)`
- drain in-flight 请求（最多 30s）

### T5: 集成测试
- 启动后 `/metrics` 输出包含 `gateway_requests_total`
- `/readyz` 在 provider 全 open 时返回 503

---

## 关键接口契约（提前锁定）

```rust
// crates/gateway-server/src/metrics/prometheus.rs
pub struct PrometheusExporter {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    tokens_total: IntCounterVec,
    errors_total: IntCounterVec,
    cache_hits: IntCounter,
    cache_misses: IntCounter,
    active_requests: IntGaugeVec,
    circuit_breaker_state: IntGaugeVec,
    rate_limit_remaining: IntGaugeVec,
}
impl PrometheusExporter {
    pub fn record_request(&self, model, provider, tenant, role, stream);
    pub fn record_tokens(&self, model, provider, tenant, kind, amount);
    pub fn record_error(&self, model, provider, error_type);
    pub fn render(&self) -> String;   // 输出 Prometheus text
}
```
