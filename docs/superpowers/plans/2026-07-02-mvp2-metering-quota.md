# MVP 2 — 计量与配额 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 MVP 1 之后实现 token 计量引擎 + RPM/RPD/TPM/TPD 四层配额执行

**Architecture:** `MeteringService` (异步 mpsc channel 聚合 usage → 定期落 PVC 快照) + `QuotaEngine` (token-bucket + sliding window，按 tenant 强制执行)

**Prerequisite:** MVP 0 + MVP 1

---

## Global Constraints

- 限流 async channel，不阻塞请求路径（fallback drop-on-overflow）
- quota 快照落 PVC，可跨重启恢复
- 配额超限返回 429 + `error.code="quota_exceeded"` + `retry_after_seconds`
- TDD + frequent commit

---

### Task 1: 计量事件结构 + MeteringService

**Files:**
- Create: `crates/gateway-server/src/metrics/metering.rs`
- Modify: `state.rs` 持 `MeteringService`

**Interfaces:**
- Produces: `MeteringService::record(MeteringEvent)` / `MeteringService::aggregate_daily(tenant_id)`

```rust
pub struct MeteringEvent {
    pub request_id: String,
    pub tenant_id: String,
    pub key_id: String,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub status: RequestStatus,
    pub estimated_cost: Option<f64>,
}
```

MeteringService 启动 worker task：
- 批处理 recv N 条 → flush_to_pvc
- 提供 `aggregate(tenant, window)` 快照用于 admin 查询

---

### Task 2: QuotaEngine (RPM/RPD/TPM/TPD)

**Files:**
- Create: `crates/gateway-server/src/metrics/quota.rs`
- Modify: Policy Engine 层调 `QuotaEngine::check(tenant, tokens_used)`

**Interfaces:**
- Consumes: `TenantConfig.quotas`
- Produces: `QuotaEngine::check(...) -> Result<(), QuotaViolation>`

算法：
- RPM: token-bucket per tenant，复用 MVP 0 TokenBucket
- TPM: sliding window (环形 buffer bucket per minute)
- RPD/TPD: histogram 纬度 + 日切写 PVC

超限 → `GatewayError::QuotaExceeded { limit, current, resets_at }`

---

### Task 3: 计量配额中间件挂进 Router

**Files:** Modify `routes.rs` build_router

在 MVP 0 auth 中间件之后加：
```rust
.layer(from_fn_with_state(rate_limit_middleware))   // MVP 0 已有
.layer(from_fn_with_state(quota_middleware))        // NEW: 先扣 TPM，请求完成再补计
```

---

### Task 4: Admin 用量 API

**Files:** Modify `admin.rs`

- `GET /api/admin/metrics?v2=true` 返回 MV2 结构 `{tokens_by_model, tokens_by_tenant, per_key, windows}`
- `GET /api/admin/quotas` 返回 per-tenant 配额与当前用量

---

### Task 5: Dashboard 页面升级（Admin UI）

**Files:** 静态前端文件

模型/租户维度 token 用量图表；可切换 day/week/month

---

### Task 6: 集成测试

伪造固定 usage 跑 quota 校验；验证超限 429 + `error.code=quota_exceeded`

### Task 7: Commit 收尾

---

## Open questions (MVP 2 实施阶段确认)

- metrics 快照落 PVC 频率默认 30s
- quota 存储初步用 PVC 文件；未来切 Redis 多副本时再替换
