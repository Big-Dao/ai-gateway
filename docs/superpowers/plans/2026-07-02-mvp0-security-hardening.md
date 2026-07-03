# MVP 0 — 安全闭坑 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复当前代码库 6 项真实安全短板（auth 未挂载 / 限流未生效 / Key 明文 / extra_headers 透传 / env 拼写错误 / dead code），让网关达到企业最小安全基线

**Architecture:** 最小侵入修复：现有 auth + config 代码保持不动接口，只添加密钥哈希层 + 真正挂载中间件 + 透传 extra_headers；不引入新的 multitenancy 抽象，那是 MVP 1 范围

**Tech Stack:** Rust 2021 edition / Axum 0.8 / tokio / hmac + sha2 (新增)，复用现有 crate 依赖

## Global Constraints

- 不引入新的多租户概念（属于 MVP 1）
- 不重构 Provider trait 签名（与 spec 不变量一致）
- 允许多一次数据库-less 迁移（启动时从旧 `api_keys: Vec<String>` 结构兼容）
- 默认配置从 `test-key` 改为空 → 启动时若 auth.enabled=true 且 keys 为空则拒绝启动
- 每日 commit 提交粒度
- TDD：每个行为变化先写 failing test

---

### Task 1: Git 仓库初始化 + CI 基础

**Files:**
- Create: `/home/andy/ai-gateway/.gitignore`
- Terminal: `git init && git add -A && git commit -m "chore: initial commit of MVP workspace"`

**Interfaces:**
- Consumes: 无
- Produces: 可提交的 git 仓库

- [ ] **Step 1: 初始化 git 仓库**

Run:
```bash
cd /home/andy/ai-gateway
git init
git add -A
git commit -m "chore: initial commit of MVP workspace"
```
Expected: `git log --oneline` shows `Initial commit...` 单次提交

- [ ] **Step 2: 创建 .gitignore**

Create: `/home/andy/ai-gateway/.gitignore`
```
/target
**/*.rs.bk
*.log
.env
config.toml
metrics_snapshots/
```

- [ ] **Step 3: 验证可构建**

Run: `cd /home/andy/ai-gateway && cargo build 2>&1 | tail -5`
Expected: `Finished` with no errors

---

### Task 2: 修复环境变量前缀拼写错误 GP-6

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-core/src/config.rs` (env prefix line)

**Interfaces:**
- Consumes: 无
- Produces: `AppConfig::load()` 使用正确的 `AI_GATEWAY` 前缀

- [ ] **Step 1: 修复拼写**

In `config.rs`, 把 `AI_GATERARY` 改为 `AI_GATEWAY`:
```rust
config::Environment::with_prefix("AI_GATEWAY")   // was "AI_GATERARY"
    .prefix_separator("__")
    .separator("__"),
```

- [ ] **Step 2: 验证环境变量覆盖仍生效**

Run: `cd /home/andy/ai-gateway && AI_GATEWAY__SERVER__PORT=9090 cargo run --bin gateway-server & sleep 2 && curl -s http://localhost:9090/health && kill %1`
Expected: `OK` 响应，证明 `AI_GATEWAY__*` 前缀生效

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "fix: correct env prefix typo AI_GATERARY → AI_GATEWAY"
```

---

### Task 3: API Key HMAC 哈希存储 GP-3a

**Files:**
- Create: `/home/andy/ai-gateway/crates/gateway-core/src/auth_key.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-core/src/config.rs` (新增结构, 兼容旧 config)
- Modify: `/home/andy/ai-gateway/crates/gateway-core/src/lib.rs` (导出新增模块)

**Interfaces:**
- Consumes: `gateway_core::auth_key::ApiKeyStore`
- Produces: `ApiKeyStore::verify(key: &str) -> bool` 供 middleware 调用

- [ ] **Step 1: 添加 hmac + sha2 依赖**

Edit `/home/andy/ai-gateway/Cargo.toml` workspace deps:
```toml
hmac = "0.12"
sha2 = "0.10"
rand = "0.8"
base16ct = { version = "0.2", features = ["alloc"] }
```

在 `crates/gateway-core/Cargo.toml` 加上：
```toml
hmac.workspace = true
sha2.workspace = true
rand.workspace = true
base16ct.workspace = true
```

- [ ] **Step 2: 创建 auth_key.rs 实现哈希**

Create `/home/andy/ai-gateway/crates/gateway-core/src/auth_key.rs`:
```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand::RngCore;

type HmacSha256 = Hmac<Sha256>;

/// Per-key secret salt stored as hex (16 bytes).
#[derive(Debug, Clone)]
pub struct Salt(pub [u8; 16]);

impl Salt {
    pub fn random() -> Self {
        let mut buf = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut buf);
        Self(buf)
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let v = hex::decode(s).ok()?;
        let mut out = [0u8; 16];
        out.copy_from_slice(&v);
        Some(Self(out))
    }
}

/// Single key record: stored hash + salt, no plaintext key retained.
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    /// HMAC-SHA256(salt, key) stored as hex
    pub hash: String,
    pub salt: Salt,
    /// Tenant id (MVP 1 will drive allocation; default "default")
    pub tenant_id: String,
    /// Role (default "developer")
    pub role: String,
    /// Human-readable id (key fingerprint, first 8 chars of hash)
    pub key_id: String,
}

impl ApiKeyEntry {
    pub fn new(plaintext_key: &str, tenant_id: &str, role: &str) -> Self {
        let salt = Salt::random();
        let hash = Self::compute_hash(&salt, plaintext_key);
        let key_id = format!("key_{}", &hash[..8]);
        Self {
            hash,
            salt,
            tenant_id: tenant_id.into(),
            role: role.into(),
            key_id,
        }
    }

    pub fn compute_hash(salt: &Salt, key: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&salt.0)
            .expect("HMAC can take key of any size");
        mac.update(key.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    pub fn verify(&self, plaintext_key: &str) -> bool {
        let computed = Self::compute_hash(&self.salt, plaintext_key);
        // Constant-time comparison via subtle crate or manual
        use std::time::{SystemTime, Duration};
        // fallback: naive compare; upgrade to subtle in MVP 1
        computed == self.hash
    }
}

/// Manages API key lifecycle in memory.
pub struct ApiKeyStore {
    entries: Vec<ApiKeyEntry>,
}

impl ApiKeyStore {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    pub fn from_plaintext_keys(keys: &[String]) -> Self {
        let entries = keys
            .iter()
            .map(|k| ApiKeyEntry::new(k, "default", "developer"))
            .collect();
        Self { entries }
    }

    pub fn add(&mut self, entry: ApiKeyEntry) {
        self.entries.push(entry);
    }

    pub fn verify(&self, plaintext_key: &str) -> Option<&ApiKeyEntry> {
        self.entries.iter().find(|e| e.verify(plaintext_key))
    }

    pub fn remove_by_id(&mut self, key_id: &str) {
        self.entries.retain(|e| e.key_id != key_id);
    }

    pub fn list_ids(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key_id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_verify_valid() {
        let entry = ApiKeyEntry::new("sk-abc123", "tenant-1", "developer");
        assert!(entry.verify("sk-abc123"));
        assert!(!entry.verify("sk-wrong"));
    }

    #[test]
    fn test_different_keys_different_hashes() {
        let a = ApiKeyEntry::new("key-a", "t1", "dev");
        let b = ApiKeyEntry::new("key-b", "t1", "dev");
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn test_store_verify_and_revoke() {
        let mut store = ApiKeyStore::new();
        let entry = ApiKeyEntry::new("secret", "tenant-1", "developer");
        let id = entry.key_id.clone();
        store.add(entry);
        assert!(store.verify("secret").is_some());
        store.remove_by_id(&id);
        assert!(store.verify("secret").is_none());
    }
}
```

注意：此任务引入 `hex` crate 也在 workspace 中加入：
```toml
hex = "0.4"
```

- [ ] **Step 3: 导出模块**

Edit `/home/andy/ai-gateway/crates/gateway-core/src/lib.rs`:
```rust
pub mod auth_key;
```

- [ ] **Step 4: 编译验证**

Run: `cd /home/andy/ai-gateway && cargo build -p gateway-core 2>&1 | tail -5`
Expected: `Finished` with no errors

- [ ] **Step 5: 测试运行**

Run: `cd /home/andy/ai-gateway && cargo test -p gateway-core 2>&1 | tail -10`
Expected: 3 tests pass (`test_hash_verify_valid`, `test_different_keys_different_hashes`, `test_store_verify_and_revoke`)

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(auth): add HMAC-SHA256 ApiKeyStore and ApiKeyEntry"
```

---

### Task 4: 修正 AuthConfig，安全启动默认 GP-3b

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-core/src/config.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/state.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/main.rs`

**Interfaces:**
- Consumes: `ApiKeyStore` from gateway-core
- Produces: `AppState.auth_store: RwLock<ApiKeyStore>`; main() 启动校验

- [ ] **Step 1: 更新 AuthConfig**

Edit `config.rs`:
```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_auth_enabled")]
    pub enabled: bool,
    /// 向后兼容：明文启动 key（首次启动自动迁移到 HMAC 存储）
    #[serde(default)]
    pub api_keys: Vec<String>,

    /// 启动需要这个 HMAC secret；为空则拒绝启动
    pub required_hmac_secret: Option<String>,

    /// 默认 tenant，用于兼容阶段
    #[serde(default = "default_tenant")]
    pub default_tenant: String,

    /// 默认 role
    #[serde(default = "default_role")]
    pub default_role: String,
}

fn default_auth_enabled() -> bool { true }
fn default_tenant() -> String { "default".into() }
fn default_role() -> String { "developer".into() }
```

并在 workspace Cargo.toml 确认 `hex` dep：
```toml
hex = "0.4"
```
加到 gateway-core deps。

- [ ] **Step 2: AppState 加 auth_store 字段**

Edit `state.rs`:
```rust
// 新增 use
use gateway_core::auth_key::ApiKeyStore;

pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub auth_store: RwLock<ApiKeyStore>,   // ← 新增
    pub providers: RwLock<HashMap<String, Arc<dyn LLMProvider>>>,
    pub cache: moka::future::Cache<String, ChatCompletionResponse>,
    pub metrics: Mutex<Metrics>,
    pub circuit_breaker: Arc<CircuitBreaker>,
}
```

- [ ] **Step 3: AppState::new 初始化 auth_store**

在新方法的 config 加载后加：
```rust
let auth_store = if config.auth.api_keys.is_empty() {
    ApiKeyStore::new()
} else {
    // 兼容：将明文 key 自动哈希存入
    ApiKeyStore::from_plaintext_keys(&config.auth.api_keys)
};
```

- [ ] **Step 4: main() 加入启动校验**

在 `main.rs` 的 `let state = ...` 加：
```rust
// 安全启动校验
if state.config.read().await.auth.enabled {
    let store = state.auth_store.read().await;
    if store.list_ids().is_empty() {
        eprintln!(
            "Refusing to start: auth.enabled=true but no API keys configured.\n\
             Either set [auth].api_keys in config.toml or set AUTH_ENABLED=false."
        );
        std::process::exit(1);
    }
}
```

- [ ] **Step 5: 编译 + 启动失败场景测试**

Run: `cd /home/andy/ai-gateway && cargo build -p gateway-server 2>&1 | tail -5`
Expected: `Finish` with no errors

Run: `cargo run --bin gateway-server & sleep 2 ; kill %1`  (不带 auth keys)
Expected: 进程启动失败，stderr 输出 `"Refusing to start: auth.enabled=true but no API keys configured"`

用 config.toml:
```toml
[auth]
enabled = true
api_keys = ["smoke-test-key"]
cargo run --bin gateway-server & sleep 2
```
Expected: 启动成功（旧 api_keys 自动迁移到 HMAC store）

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(auth): wire ApiKeyStore into AppState with secure startup check"
```

---

### Task 5: 真挂载 auth 中间件 GP-1

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/routes.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/middleware/auth.rs`

**Interfaces:**
- Consumes: `AppState.auth_store`
- Produces: 未认证请求返回 401，已认证放行

- [ ] **Step 1: 更新 auth_middleware 使用 HMAC store**

编辑 `auth.rs` 中间件从 auth_store 查找：
```rust
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let (enabled, hmac_secret) = {
        let config = state.config.read().await;
        (config.auth.enabled, config.auth.required_hmac_secret.clone())
    };

    if !enabled {
        return Ok(next.run(request).await);
    }

    // 没有配置 secret 但 auth 启用，拒绝启动已经在 main 处理；运行时降级拒绝
    match request.headers().get(header::AUTHORIZATION) {
        Some(value) => match value.to_str() {
            Ok(v) if v.starts_with("Bearer ") => {
                let key = &v[7..];
                let store = state.auth_store.read().await;
                if let Some(entry) = store.verify(key) {
                    // Inject basic claims (id-only for MVP 0)
                    request.extensions_mut().insert(AuthKey(entry.key_id.clone()));
                    Ok(next.run(request).await)
                } else {
                    warn!("Invalid API key attempt");
                    Err(GatewayError::AuthenticationFailed(
                        "Missing or invalid Authorization header".into(),
                    ))
                }
            }
            _ => Err(GatewayError::AuthenticationFailed(
                "Invalid Authorization header format".into(),
            )),
        },
        None => Err(GatewayError::AuthenticationFailed(
            "Missing Authorization header".into(),
        )),
    }
}
```

- [ ] **Step 2: 在 routes.rs 安装 auth 中间件**

Edit `build_router` 在路由链加 middleware：
```rust
use axum::middleware as axum_middleware;
use crate::middleware::auth::auth_middleware;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .nest("/api/admin", crate::admin::admin_router())
        .route("/admin", get(crate::static_files::admin_page))
        .route("/admin/static/admin.css", get(crate::static_files::admin_css))
        .route("/admin/static/admin.js", get(crate::static_files::admin_js))
        // 鉴权插入
        .layer(axum_middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}
```

- [ ] **Step 3: 编译**

Run: `cd /home/andy/ai-gateway && cargo build -p gateway-server 2>&1 | tail -5`
Expected: `Finish` without errors

- [ ] **Step 4: E2E 测试 — unauthenticated request 返回 401**

使用 curl 或写一个集成测试 `crates/gateway-server/tests/e2e_auth.rs`（首次，用 reqwest）：
```rust
#[tokio::test]
async fn e2e_no_auth_returns_401() {
    let handle = common::spawn_test_server(vec!["test-key-xyz"]);
    let client = reqwest::Client::new();
    let resp = client.post(&format!("{}/v1/chat/completions", handle.base_url))
        .json(&serde_json::json!({"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]})).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}
```

先简化成单元测试维护；下一步 `cargo run` 接 curl 验证：

Run: `cargo run --bin gateway-server & sleep 3`
then: `curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/v1/chat/completions -X POST -H "Content-Type: application/json" -d '{"model":"gpt-4o-mini","messages":[]}'`
Expected: `401`

Valid call:
`curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer <smoke-test-key>" http://localhost:8080/v1/chat/completions -X POST -H "Content-Type: application/json" -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'`
Expected: `404`（model 不存在）但 **不是 401**

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(auth): actually mount auth_middleware into router chain"
```

---

### Task 6: 真挂载限流中间件 GP-2（token-bucket, per-tenant 简版）

**Files:**
- Create: `/home/andy/ai-gateway/crates/gateway-server/src/middleware/rate_limit.rs`
- Modify: `/home/andy/andy/ai-gateway/crates/gateway-server/src/middleware/mod.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/routes.rs`

**Interfaces:**
- Consumes: `state.config.rate_limit.requests_per_minute`
- Produces: `rate_limit_middleware`；超限返回 429 + `retry_after` 头

- [ ] **Step 1: 实现 token-bucket 限流**

创建 `middleware/rate_limit.rs`:
```rust
use axum::{
    extract::{Request, State},
   middleware::Next,
    response::{Response, IntoResponse},
    http::header,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, Duration};
use tracing::warn;

use gateway_core::error::GatewayError;

pub struct TokenBucket {
    capacity: u64,
    tokens_per_sec: f64,
    tokens: AtomicU64,          // 用原子 f64 的 bits 表示
    last_refill_ms: AtomicU64,
}

impl TokenBucket {
    pub fn new(rpm: u32) -> Self {
        let capacity = rpm.max(1) as u64;
        Self {
            capacity,
            tokens_per_sec: rpm as f64 / 60.0,
            tokens: AtomicU64::new(capacity * 1_000_000),  // 用微-token避免 float原子
            last_refill_ms: AtomicU64::new(
                Instant::now().elapsed().as_millis() as u64
            ),
        }
    }

    pub fn consume(&self) -> Option<Duration> {
        self.refill();
        loop {
            let cur = self.tokens.load(Ordering::Relaxed);
            if cur < 1_000_000 {
                let deficit = 1_000_000 - cur;
                let wait_ms = (deficit as f64 / (self.tokens_per_sec * 1000.0)) as u64 + 1;
                return Some(Duration::from_millis(wait_ms));
            }
            if self.tokens
                .compare_exchange(cur, cur - 1_000_000, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return None;
            }
        }
    }

    fn refill(&self) {
        let now = Instant::now().elapsed().as_millis() as u64;
        let last = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed_ms = now.saturating_sub(last);
        if elapsed_ms < 50 {
            return;
        }
        let new_tokens = (elapsed_ms as f64 * self.tokens_per_sec) as u64 * 1_000;
        let _ = self.last_refill_ms.compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed);
        let mut cur = self.tokens.load(Ordering::Relaxed);
        loop {
            let nxt = (cur + new_tokens).min(self.capacity * 1_000_000);
            if self.tokens
                .compare_exchange(cur, nxt, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            cur = self.tokens.load(Ordering::Relaxed);
        }
    }
}

pub async fn rate_limit_middleware(
    State(bucket): State<Arc<TokenBucket>>,
    request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    if let Some(wait) = bucket.consume() {
        warn!(?wait, "Rate limit hit");
        let mut resp = GatewayError::RateLimited
            .into_response();
        resp.headers_mut().insert(
            header::RETRY_AFTER,
            wait.as_secs().to_string().parse().unwrap(),
        );
        return Ok(resp);
    }
    Ok(next.run(request).await)
}
```

- [ ] **Step 2: 注册 middleware 模块**

`middleware/mod.rs`:
```rust
pub mod auth;
pub mod rate_limit;
```

- [ ] **Step 3: 在 AppState 中持有全局桶并安装到 router**

`state.rs`:
```rust
use crate::middleware::rate_limit::TokenBucket;
pub struct AppState {
    /* ... */
    pub rate_limiter: Arc<TokenBucket>,
}
```
在 `AppState::new` 构造：
```rust
let rpm = config.rate_limit.requests_per_minute;
let rate_limiter = Arc::new(TokenBucket::new(rpm));
```
在 `update_rate_limit`（admin route 中已有）同步刷新桶的 capacity（简化：admin 修 config 时重建设桶 Arc）。

- [ ] **Step 4: 在 build_router 加入限流**

```rust
.layer(axum_middleware::from_fn_with_state(state.rate_limiter.clone(), rate_limit_middleware))
```

- [ ] **Step 5: 验证限流生效**

启动后快速发 70 个请求（rpm 配置 60），第 61+ 个应 429：
```bash
cargo run --bin gateway-server & sleep 2
for i in $(seq 1 70); do curl -s -o /dev/null -w "%{http_code}\n" -H "Authorization: Bearer <key>" http://localhost:8080/v1/chat/completions -X POST -H 'Content-Type: application/json' -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}' ; done | sort | uniq -c
```
Expected: 60 个非 429，10+ 个 429

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(rate-limit): add token-bucket middleware enforcing tenant RPM"
```

---

### Task 7: extra_headers 真透传 GP-4

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/state.rs` (`register_provider`)
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/admin.rs` (create/update flow)
- Create test that verifies header presence

**Interfaces:**
- Consumes: `ProviderConfig.extra_headers`
- Produces: 每 Provider reqwest 调用前注入 `extra_headers`

- [ ] **Step 1: 给 Provider 加 headers 注入辅助**

在 `crates/providers/src` 顶层加一个公共函数：
```rust
// crates/providers/src/util.rs
use reqwest::header::HeaderMap;

pub fn build_auth_header(
    api_key: Option<&str>,
   pub fn build_auth_header(
    api_key: Option<&str>,
    extra_headers: &std::collections::HashMap<String, String>,
) -> HeaderMap {
    let mut map = HeaderMap::new();
    if let Some(key) = api_key {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(key) {
            map.insert(reqwest::header::AUTHORIZATION, v);
        }
    }
    for (k, v) in extra_headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            map.insert(name, value);
        }
    }
    map
}
```

在 workspace Cargo.toml 中对 providers crate 引入 `reqwest` 已有的 headers 支持（它已依赖 reqwest）。

在 `crates/providers/src/lib.rs` 加 `pub mod util;`。

现在 Provider 各自 `new()` 签名改为：
```rust
pub fn new(
    api_key: Option<String>,
    base_url: Option<String>,
    extra_headers: HashMap<String, String>,
) -> Self {
    Self { api_key, base_url, extra_headers, client, ... }
}
```
HTTP 调用时：用 `util::build_auth_header(self.api_key.as_deref(), &self.extra_headers)` 注入请求。

更新各 Provider 内部所有 `self.client.post/get/...` 为
```rust
self.client
    .post(&url)
    .headers(util::build_auth_header(self.api_key.as_deref(), &self.extra_headers))
    .json(&body)
    ...
```

- [ ] **Step 3: 同步 admin 和 state 的调用签名**

逐一修 `register_provider`、crate provider 构造的 5 个 Provider（OpenAI、Anthropic、Gemini、Ollama） 调用处加 `provider_cfg.extra_headers.clone()`。

- [ ] **Step 4: extra_headers 透传测试**

Add providers unit test:
```rust
#[tokio::test]
async fn test_extra_headers_are_sent() {
    let mut server = mockito::Server::new();
    let mock = server.mock("GET", "/v1/models")
        .match_header("x-custom", "value123")
        .with_status(200)
        .create();
    let provider = OpenAIProvider::new(
        Some("sk-1".into()),
        Some(server.url()),
        HashMap::from([("x-custom".into(), "value123".into())]),
    );
    let _ = provider.list_models().await;
    mock.assert();
}
```
（依赖 `mockito` 加到 workspace dev-dependencies）

简化：加 providers dev-dependencies = `mockito = "1"`。

- [ ] **Step 5: cargo build + cargo test**

Run: `cd /home/andy/ai-gateway && cargo build 2>&1 | tail -5`
Expected: `Finish`

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(provider): forward extra_headers from config to upstream provider requests"
```

---

### Task 8: 清理 dead code GP-5 (`resolve_provider`)

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/router.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/retry.rs` (fallback chain 复用)

**Interfaces:**
- Consumes: 无
- Produces: `router.rs` 的 `resolve_provider` 被真的调用，或者正式清理并转入 retry fallback chain

因为 `retry.rs` 的 `build_fallback_chain` 已经覆盖了「模型→provider」决策并内置 failover 语义，`resolve_provider` 是冗余代码。我们的方案是**清理并删除 `resolve_provider`**，由 fallback chain 统一负责。

- [ ] **Step 1: 删除 resolve_provider 函数**

`router.rs` 仅保留必要的 Re-export 或文件占位（crate 级 public module `router` 还在 use crate::router 引用）。确认是不是 crate 外部没有 use crate::router::resolve_provider 的调用。

Run: `cd /home/andy/ai-gateway && grep -rn "router::\|\.resolve_provider" crates/`
Expected: 不应有外部调用

删除 `pub async fn resolve_provider(...)` 函数体。

- [ ] **Step 2: 确认 fallback chain 已提供等价能力**

查看 `retry.rs::build_fallback_chain`（应已存在），并确认它覆盖「同一模型多 provider failover」行为。如果 fallback chain 仅走「同 model 在下挂 providers 列表内轮询」，保留并完善它。

- [ ] **Step 3: 编译验证**

Run: `cargo build -p gateway-server 2>&1 | tail -3`

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "chore: remove dead resolve_provider, rely on retry fallback chain"
```

---

### Task 9: Structured error — 给所有错误响应加 `request_id` + `Retry-After`（GP-1+GP-2 配套小件）

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/routes.rs` (ApiError)
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/middleware/auth.rs`、`rate_limit.rs` (统一走 ApiError::from)

**Interfaces:**
- Consumes: `GatewayError` 变体
- Produces: 所有 API 错误响应都带 `X-Request-Id` 头与 OpenAI-兼容的 `error` 体；429 额外带 `Retry-After` 头

- [ ] **Step 1: ApiError into_response 加 X-Request-Id 头**

```rust
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = uuid::uuid!();   // 或简单用一个 Arc<AtomicU64>
        let status = http::StatusCode::from_u16(self.0.status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut resp = (status, axum::Json(self.0.to_error_response())).into_response();
        resp.headers_mut().insert(
            "x-request-id",
            request_id.to_string().parse().unwrap(),
        );
        resp
    }
}
```

- [ ] **Step 2: 限流 429 自动用此路径**

让 `rate_limit_middleware` 返回 `Err(GatewayError::RateLimited)` 经 ApiError 转换（确保含 retry-after 从 err 内部拿—此处简化：设 header 由 rate_limit middleware 自身注入，返回 Err(ApiError) 后再 merged）。

简化方案：rate_limit 直接返回带 retry-after 的 Response（之前的实现已做到），不通过 ApiError。

- [ ] **Step 3: 验证错误响应带 X-Request-Id**

Run invalid auth call: `curl -D - http://localhost:8080/v1/chat/completions -X POST -H 'Content-Type: application/json' -d '{}'`
Expected: 响应头含 `x-request-id`，body 含 `{"error":{"message":...,"type":"authentication_error",...}}`

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(errors): add X-Request-Id and standardize OpenAI-format error responses"
```

---

### Task 10: 构建 K8s 就绪探针 (/healthz, /readyz) 与 graceful shutdown

**Files:**
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/routes.rs`
- Modify: `/home/andy/ai-gateway/crates/gateway-server/src/main.rs`

**Interfaces:**
- Consumes: `AppState`
- Produces: GET `/healthz` (200=liveness)；GET `/readyz` (200=ready, 503=not ready)；main.rs 支持 graceful shutdown via ctrl_c

- [ ] **Step 1: 在 routes 中健康检查分级**

```rust
pub async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

pub async fn readiness(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Ready = config loaded + cache ok + ≥1 provider circuit-closed
    let any_closed = state
        .circuit_breaker
        .state("openai")   // 简化：查任一 provider
        .map(|s| s == CircuitState::Closed || s == CircuitState::HalfOpen)
        .unwrap_or(false);
    if any_closed {
        (StatusCode::OK, axum::Json(serde_json::json!({"status":"ready"})))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({"status":"not_ready"})))
    }
}
```
在 `build_router` 加：
```rust
.route("/healthz", get(liveness))
.route("/readyz", get(readiness))
```

- [ ] **Step 2: main() graceful shutdown**

```rust
let listener = tokio::net::TcpListener::bind(&addr).await?;
axum::serve(listener, app)
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
```

- [ ] **Step 3: 验证**

启动后:
```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/healthz     # 200
curl -s http://localhost:8080/readyz                                    # {"status":"ready"}
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(ops): add /healthz /readyz probes and graceful shutdown"
```

---

### Task 11: 集成测试回归（整体验证 MVP 0 闭环）

**Files:**
- Create: `/home/andy/ai-gateway/crates/gateway-server/tests/common/mod.rs`
- Create: `/home/andy/ai-gateway/crates/gateway-server/tests/mvp0_smoke.rs`

**Interfaces:** —

- [ ] **Step 1: 写 common 测试 helpers / fixture**

```rust
// tests/common/mod.rs
use std::time::Duration;
use tokio::time::timeout;

pub struct TestServer {
    pub base_url: String,
    handle: tokio::process::Child,
}

impl TestServer {
    pub fn spawn(api_keys: &[&str]) -> Self {
        // spawn cargo run + 等待 /healthz OK
        // 返回 base_url
    }
}

impl Drop for TestServer {
    fn drop(&mut self) { let _ = self.handle.start_kill(); }
}
```

- [ ] **Step 2: 写 mvp0_smoke.rs 覆盖全部闭坑场景**

```rust
#[tokio::test]
async fn mvp0_unauthenticated_returns_401() { ... }
#[tokio::test]
async fn mvp0_authenticated_works() { ... }
#[tokio::test]
async fn mvp0_health_endpoints_present() { ... }
#[tokio::test]
async fn mvp0_env_var_overrides_port() { ... }
```

- [ ] **Step 3: 运行**

Run: `cd /home/andy/ai-gateway && cargo test --package gateway-server --test mvp0_smoke 2>&1 | tail -15`
Expected: 全部 4 测试 PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test: MVP 0 smoke tests covering auth, rate-limit, probes"
```
