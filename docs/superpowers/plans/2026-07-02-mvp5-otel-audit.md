# MVP 5 — OpenTelemetry 与审计日志 Implementation Plan (Roadmap Stub)

> **For agentic workers:** 本文件为 roadmap stub，待 MVP 0-4 完成后展开。

**Goal:** OTel 链路追踪 (feature gate) + 审计日志异步落盘 + 查询 API

**Prerequisite:** MVP 0-4

---

## Roadmap Tasks

### T1: OTel 链路追踪
- workspace feature gate `otel`
- deps: opentelemetry / opentelemetry-otlp / tracing-opentelemetry
- 默认 off，检测到 `OTEL_EXPORTER_OTLP_ENDPOINT` 后激活
- Span 树：gateway.request → auth → policy → cache → provider → cache_write
- W3C TraceContext `traceparent`/`tracestate` 传播

### T2: 审计日志异步落盘
- `AuditWriter` (mpsc + bg task)
- Admin handlers 全路径埋 audit event
- 事件: auth.login_success/failure, key.create/revoke, tenant CRUD, provider CRUD, config.update

### T3: GET /api/admin/audit-logs API
- 查询 / 过滤 (actor, action, tenant, time-range)
- 分页

### T4: 测试 / Commit
