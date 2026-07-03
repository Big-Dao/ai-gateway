# MVP 1 — 多租户与 RBAC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 MVP 0 基础上引入「多租户 + 三角色 RBAC」模型，让网关支持多团队共用、数据隔离与权限分级

**Architecture:** 复用 ApiKeyEntry/ApiKeyStore；新增 `TenantContext` 与 `TenantConfig`，在 Policy Engine 层加入 RBAC 校验中间件；Admin API 对 tenanted 资源强制 tenant-scope 校验

**Tech Stack:** Same as MVP 0 (no new crate)

**Prerequisite:** MVP 0 全部完成

## Global Constraints

- 保持 Provider trait 签名稳定
- 单副本部署 (无 Redis) — quota 落 PVC 快照
- 默认 tenant `default` 兜底，避免现有调用方断掉
- 所有 admin 操作必须留审计痕
- TDD + 频繁 commit

---

### Task 1: TenantConfig / TenantContext 数据结构

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-core/src/config.rs`
- Create: `/home/andy/ai-gateway/crates/gateway-core/src/tenant.rs`

**Interfaces:**
- Produces: `tenant::TenantContext`, `tenant::TenantQuotas`, `config::TenantConfig`

- [ ] **Step 1: 在 config 中加 tenants 表与 TenantConfig**

```rust
// config.rs
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    /* ... existing ... */
    #[serde(default)]
    pub tenants: HashMap<String, TenantConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TenantConfig {
    pub name: String,
    #[serde(default = "default_tenant_quota")]
    pub quotas: TenantQuotas,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TenantQuotas {
    #[serde(default = "default_rpm")]
    pub max_rpm: u32,
    #[serde(default = "default_rpd")]
    pub max_rpd: u64,
    #[serde(default = "default_tpm")]
    pub max_tpm: u64,
    #[serde(default = "default_tpd")]
    pub max_tpd: u64,
}
fn default_rpm() -> u32 { 60 }
fn default_rpd() -> u64 { 10_000 }
fn default_tpm() -> u64 { 500_000 }
fn default_tpd() -> u64 { 5_000_000 }
fn default_tenant_quota() -> TenantQuotas { TenantQuotas {
    max_rpm: default_rpm(), max_rpd: default_rpd(),
    max_tpm: default_tpm(), max_tpd: default_tpd(),
}}
```

- [ ] **Step 2: tenant.rs 放 per-request 上下文**

```rust
// crates/gateway-core/src/tenant.rs
#[derive(Debug, Clone)]
pub struct TenantContext {
    pub tenant_id: String,
    pub role: Role,
    pub key_id: String,
    pub quotas: crate::config::TenantQuotas,
}
pub use crate::auth_key::Role;
```

- [ ] **Step 3: 导出**

`lib.rs`: `pub mod tenant;`

- [ ] **Step 4: cargo build**

- [ ] **Step 5: Commit**

---

### Task 2: RBAC 校验中间件

**Files:**
- Create: `/home/andy/ai-gateway/crates/gateway-server/src/middleware/rbac.rs`
- Modify: `middleware/mod.rs`

**Interfaces:**
- Consumes: `AuthKey` (request ext, MVP 0 注入)
- Produces: `rbac_middleware`，无权限 → 403

- [ ] **Step 1: rbac middleware**

```rust
// middleware/rbac.rs
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use crate::middleware::auth::AuthKey;
use gateway_core::auth_key::Role;
use gateway_core::error::GatewayError;
use crate::state::AppState;

pub async fn rbac_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
    required: Role,   // 简化：按需调用方指定
) -> Result<Response, GatewayError> {
    let key = request.extensions().get::<AuthKey>().cloned()
        .ok_or(GatewayError::AuthenticationFailed("Missing auth".into()))?;
    let store = state.auth_store.read().await;
    let entry = store.verify_by_id(&key.0).ok_or(...)?;
    if !(entry.role.0 >= required.0) {       // role 比较
        return Err(GatewayError::Forbidden(...));
    }
    Ok(next.run(request).await)
}
```

简化方案（推荐）：在 handler 函数内联做 RBAC 而非中间件（避免 middleware 泛型），各 handler 按权限调用：
```rust
require_role(&state, &key, Role::TenantAdmin).await?;
```

```rust
pub async fn require_role(state, key: &str, min: Role) -> Result<(), GatewayError> { ... }
```

加到 `middleware/rbac.rs` 中 — 简单且符合 Rust Axum 风格。

- [ ] **Step 2: 加 Forbidden variant to GatewayError + HTTP 403**

```rust
// error.rs
Forbidden(String),   // HTTP 403, error.code="insufficient_permissions"
```

- [ ] **Step 3: rbac 单元测试**

```rust
#[tokio::test]
async fn developer_cannot_create_provider() { ... }
#[tokio::test]
async fn tenant_admin_can_create_own_tenant_keys() { ... }
#[tokio::test]
async fn tenant_admin_cannot_access_other_tenant() { ... }
```

- [ ] **Step 4: Commit**

---

### Task 3: Admin API tenanted 化

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/admin.rs`

**Interfaces:**
- Consumes: `AuthKey`, `Role`
- Produces: tenant-scoped CRUD for providers/keys

- [ ] **Step 1: 所有 admin handlers 入口加 `require_role` + tenant 检查**

```rust
// before handler body：
let auth_key = request.extensions().get::<AuthKey>();
require_role(&state, auth_key, Role::TenantAdmin).await?;
// 然后 scope = 当前 key.tenant_id；操作只在其内生效
```

- [ ] **Step 2: Admin list 端点只返回当前 tenant 数据**

providers: `GET /api/admin/providers` 按当前 key 的 tenant 过滤

keys: 只返回相同 tenant 的 key 列表

- [ ] **Step 3: GET /api/admin/tenants 与 PUT/DELETE /api/admin/tenants/{id}**

Admin 角色可跨 tenant 操作；TenantAdmin 仅本 tenant。

---

### Task 4: 租户配额热更新 — PUT /api/admin/config/quota

**Files:**
- Modify: `admin.rs`
- 复用已有 `update_rate_limit` handler 模式

**Interfaces:** Same shape as existing config/cache update handler

- **实现**：
```rust
PUT /api/admin/config/quota/{tenant_id}   body: TenantQuotas
校验 Role ≥ tenant_admin 且属于该 tenant
写 config.tenants[id].quotas
```

---

### Task 5: Admin UI 新增租户管理页 / 密钥管理升级

**Files:**
- Modify: `crates/gateway-server/src/static/admin.js`
- Modify: `static/index.html`、`static/admin.css`

**Interfaces:**
- Consumes: 上述新 API
- Produces: Web UI 支持租户/密钥 CRUD

- ** Tenant list/create/update/delete 页
- ** Key role/tenant 下拉选择

---

### Task 6: 集成测试 — 多租户隔离

**Files:**
- Create: `crates/gateway-server/tests/mvp1_tenancy.rs`

**Interfaces:** —
- **测试点**：
  - 同一 token，不同 tenant 可见模型不同
  - TenantAdmin 不能 admin 越权
  - 默认 tenant 兜底仍可用

---

### Task 7: Commit + PR 收口

```bash
git add -A && git commit -m "feat(tenancy): MVP 1 complete — multitenancy + RBAC + tenanted admin API"
```
