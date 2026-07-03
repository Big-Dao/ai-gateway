# Enterprise AI Gateway — 设计文档 V2

> **状态**: Draft
> **日期**: 2026-07-02
> **范围**: 将 `/home/andy/ai-gateway/` 的 MVP Rust 网关升级为生产就绪的企业级 AI 服务网关

---

## 1. 概述与目标

### 1.1 背景

现有代码库是一个 3 144 行 Rust 的 AI 网关，提供 OpenAI 兼容的 `/v1/chat/completions` 接口，对接 OpenAI / Anthropic / Gemini / Ollama 四个 Provider。架构 Provider trait + circuit breaker + retry + Admin UI 设计正确，但存在**关键控制面未生效**（auth 中间件未挂载、限流仅有配置不执行、API Key 明文、默认安全配置声明与实际不符）。

### 1.2 升级目标

将网关升级为企业级多租户 AI 服务网关，覆盖四个核心能力域：

| 能力域 | 当前 | 目标 |
|--------|------|------|
| 安全合规 | auth 未挂载、Key 明文、无 RBAC、无审计 | 认证真正生效 + 三角色 RBAC + 多租户隔离 + 审计留痕 |
| 成本管控 | 无计量 | Token 计量 + 多维度配额引擎 + 成本字段预留 |
| 可靠性 | 可读 JSON 指标、无追踪、无健康分级 | Prometheus + OTel + 结构化错误 + K8s 健康分级 |
| 多模型 | 4 Provider（OpenAI/Anthropic/Gemini/Ollama） | + OpenAICompatProvider 适配器，首批新增 DeepSeek/Kimi/智谱/vLLM/SGLang/TGI |

### 1.3 非目标（明确不做的）

- Provider 按单价金额计费（预留字段，单独迭代）
- 原生多模态/方案 B 的完整 Provider 类型重构（等 Bedrock/Azure 真实需求触发）
- 多副本 + Redis 共享状态（企业 V2，当前单副本可运行）
- Admin UI 框架化重写（保持零依赖单二进制部署）

---

## 2. 范围与边界

### 2.1 在范围内

1. **安全闭坑**：auth 与 rate_limit 中间件真挂载、Key 加 HMAC 哈希、RBAC、租户隔离、审计日志
2. **可观测**：Prometheus 指标导出、OTel 链路追踪（feature gate）、结构化错误、分级健康检查
3. **计量配额**：Token 计量 + RPM/RPD/TPM/TPD 四层配额 + 成本预留字段
4. **多 Provider**：新增 `OpenAICompatProvider`、7 个新 Provider、策略路由（Failover/TenantOverride/AB/加权）
5. **部署就绪**：K8s 清单、SLO 告警规则、优雅关闭、PVC 持久化

### 2.2 不在范围内

- Provider 金额计费与对账
- 原生多模态 / Provider-native 类型系统
- 多副本状态共享 (Redis)
- 客户端 SDK
- Admin UI 大改（仅新增必要页面）

---

## 3. 架构设计

### 3.1 七层分层模型

从请求进入到响应写出按以下 7 层依次处理，每层单一职责、独立接口：

```
Transport → Auth → Tenant → Policy → Routing → Provider → Observability
 (HTTP)    (身份)  (多租户) (规则)  (模型→P) (LLM调用)  (计量/审计)
```

| 层 | 职责 | 接口/类型 |
|---|---|---|
| Transport | 路由/中间件挂载/SSE 帧/Body 校验 | Axum Router + middleware stack |
| Auth | 身份认证 + RBAC 鉴权 | `AuthProvider` trait + `AuthClaims` |
| Tenant | 多租户上下文注入 | `TenantContext` request extension |
| Policy | 校验+限流+配额+审计（可开关）| `PolicyEngine` 组合器 |
| Routing | 模型→Provider 映射 + 策略路由 | `Router` (failover/AB/加权) |
| Resilience | 重试+熔断+超时组合 | `RetryConfig` + `CircuitBreaker` |
| Provider | OpenAI-seam 泛化调用 | `LLMProvider` trait（稳定）|
| Observability | Prometheus+OTel+日志+审计 | `Metrics` + `AuditLog` + tracing |

### 3.2 Provider 架构（方案 A — OpenAI-seam）

维持现有 `LLMProvider` trait 签名不变，新增 `OpenAICompatProvider` 统一处理 OpenAI 兼容型 provider：

```
LLMProvider trait (不变)
  ├─ OpenAIProvider         → api.openai.com
  ├─ AnthropicProvider      → converter (保持)
  ├─ GeminiProvider         → converter (保持)
  ├─ OllamaProvider         → converter (保持)
  └─ OpenAICompatProvider (新) → DeepSeek / Kimi / 智谱 / vLLM / SGLang / TGI
        └─ field_overrides + extra 透传处理独有字段
      未来：BedrockProvider / AzureProvider (完整 converter)
```

**理由**：
- 你们首批新增 provider 均为 OpenAI 兼容 + 少量独有字段，override 模式零浪费
- Provider trait 公共签名稳定，不破坏现有行为
- `extra: HashMap<String, serde_json::Value>` 作为 escape hatch，真正高级能力（多模态原生、Bedrock）可在更了解需求后重构成 type 安全的形式

### 3.3 模块隔离保证

每一层都可**独立理解、独立测试、独立替换**：

- 改 Auth trait 实现（Key ↔ JWT）不影响 Routing
- 改熔断算法不影响 Provider 调用语意
- 加 Feature flag 不影响核心路径
- Provider 增减不破坏存量 client

---

## 4. 身份认证与多租户

### 4.1 当前问题（必须全部修复）

| # | 问题 | 严重度 | 修复动作 |
|---|------|--------|---------|
| GP-1 | `auth_middleware` 定义但未挂载，所有端点裸奔 | 🔴 P0 | 真正挂载中间件 |
| GP-2 | 限流完全未执行，只有配置 | 🔴 P0 | 实现 token-bucket 中间件 |
| GP-3 | API Key 明文在 RAM，默认 `test-key` | 🔴 P0 | HMAC 哈希存储，启动强制 secret |
| GP-4 | `extra_headers` 字段配置但未传给 Provider | 🟡 P1 | 透传到 reqwest HeaderMap |
| GP-5 | `resolve_provider()` dead code | 🟢 P2 | 清理或正式启用 |
| GP-6 | 环境变量前缀 `AI_GATERARY` 拼写错误 | 🟡 P1 | 修正为 `AI_GATEWAY` |

### 4.2 Auth 模型

```rust
// gateway-core/src/auth.rs (新增)
pub struct AuthClaims {
    pub key_id: String,
    pub tenant_id: String,
    pub role: Role,
    pub key_hmac: String,
}

pub enum Role {
    Admin,           // 全局管理
    TenantAdmin,     // 租户级管理
    Developer,       // 普通 API 调用者
}

pub struct ApiKeyEntry {
    pub key_id: String,
    pub hmac_hash: String,
    pub salt: String,
    pub tenant_id: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: KeyStatus,     // Active / Disabled / Expired
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str) -> Result<AuthClaims, GatewayError>;
}
```

**密钥启动注入**：
- 环境变量 `AI_GATEWAY_KEY_HMAC_SECRET` 必须注入，否则网关拒绝启动
- 可选 `AI_GATEWAY_BOOTSTRAP_KEYS` = JSON array（首次启动注入 keys，避免 config 变化）

### 4.3 API Key 存储

- `AuthConfig.api_keys: Vec<String>` → `Vec<ApiKeyEntry>`
- 哈希策略：`HMAC-SHA256(salt, key)`，每 key 独立 salt
- Admin list 端点安全：旧端点暴露 keys 明文 → 修复为仅显示 key_id + role + 过期时间，不暴露 hash

### 4.4 Auth + Tenant 中间件挂载

```rust
// routes.rs build_router 更新后
Router::new()
    .route("/v1/chat/completions", post(handler))
    .route("/v1/models", get(list_models))
    .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
    .layer(middleware::from_fn_with_state(state.clone(), tenant_middleware))
    .layer(middleware::from_fn_with_state(state.clone(), rate_limit_middleware))
    .layer(middleware::from_fn_with_state(state.clone(), audit_log_middleware))
```

### 4.5 权限矩阵

| 端点 | Admin | TenantAdmin | Developer |
|------|:-----:|:-----------:|:---------:|
| `/v1/chat/completions` | ✅ | ✅ | ✅ |
| `/v1/models` | ✅ | ✅ | ✅ |
| `/api/admin/providers` CRUD | ✅ | 本租户 | ❌ |
| `/api/admin/keys` CRUD | ✅ | 本租户 | ❌ |
| `/api/admin/tenants` CRUD | ✅ | ❌ | ❌ |
| `/api/admin/audit-logs` GET | ✅ | 本租户 | ❌ |
| `/api/admin/config/*` PUT | ✅ | ❌ | ❌ |
| `/metrics` GET | ✅ | ✅ | ✅ |
| `/healthz / /readyz / /deep-health` GET | 公开 | 公开 | 公开 |

### 4.6 多租户隔离

```rust
pub struct TenantContext {
    pub tenant_id: String,
    pub role: Role,
    pub key_id: String,
    pub quotas: TenantQuotas,
}

pub struct TenantQuotas {
    pub max_rpm: u32,
    pub max_rpd: u64,
    pub max_tpm: u64,
    pub max_tpd: u64,
}
```

**三层隔离**：
1. **数据**：metrics / audit / cache / log stream 全部带 `tenant_id` 标签
2. **配置**：每 tenant 独立配额、可用 Provider 列表、模型白名单（Admin UI 管理）
3. **审计**：admin 操作强制留痕 `{actor, target, action, resource, before/after, ts}`

**TenantConfig 配置精确字段（在 `AppConfig.tenants: HashMap<String, TenantConfig>` 中使用）**：
```rust
pub struct TenantConfig {
    pub tenant_id: String,
    pub name: String,                              // 显示名
    pub quotas: TenantQuotas,                      // 配额
    pub allowed_providers: Option<Vec<String>>,    // None = 全部
    pub allowed_models: Option<Vec<String>>,       // None = 全部
    pub created_at: DateTime<Utc>,
}
```

**租户路由覆盖**：
- 模型路由表支持 `tenant_override`：同模型可路由到不同 Provider（A 团队 `gpt-4o` 用 OpenAI，B 团队同名走 vLLM 本地部署）

---

## 5. 可观测性体系

### 5.1 Prometheus 指标导出

新增 `/metrics` 端点（与现有 JSON 版 `/api/admin/metrics` 共存，不破坏 Admin UI），使用 `prometheus` crate：

| 指标名 | 类型 | 标签 | 用途 |
|--------|------|------|------|
| `gateway_requests_total` | counter | model, provider, tenant, role, stream | 总请求量 |
| `gateway_request_duration_seconds` | histogram | model, provider, tenant | 延迟分布 (P50/90/95/99) |
| `gateway_tokens_total` | counter | model, provider, tenant, kind=prompt\|completion | Token 用量 |
| `gateway_request_errors_total` | counter | model, provider, error_type | 错误分布 |
| `gateway_cache_hits_total` / `gateway_cache_misses_total` | counter | — | 缓存命中率 |
| `gateway_active_requests` | gauge | tenant | 并发活跃请求 |
| `gateway_provider_circuit_breaker_state` | gauge | provider | 熔断器状态 (0=closed,1=open,2=half) |
| `gateway_rate_limit_remaining` | gauge | tenant, key_id | 剩余配额（每分钟） |

**直方图桶**：`[0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60]` (秒)

**ServiceMonitor / PodMonitor**：K8s 清单包含 `prometheus-servicemonitor.yaml`（Prometheus Operator）或 `additionalScrapeConfigs`。

### 5.2 OpenTelemetry 链路追踪

新增可选 feature = `otel`（默认 off，零配置零开销）：

**依赖**：`opentelemetry` + `opentelemetry-otlp` + `tracing-opentelemetry`

**特性开关**：默认 off，检测到 `OTEL_EXPORTER_OTLP_ENDPOINT` env 后激活

**Span 层级**（一次调用的追踪树）：
```
gateway.request  (根 span, 接入时自动生成 X-Request-Id)
  ├─ gateway.auth          (认证+鉴权)
  ├─ gateway.policy        (限流+配额校验)
  ├─ gateway.cache_lookup
  ├─ gateway.provider_call
  │    ├─ gateway.provider.http
  │    ├─ gateway.provider.retry
  │    └─ gateway.provider.fallback
  └─ gateway.cache_write
```

**Trace 传播**：
- 已接入客户端 → 从 `traceparent` / `tracestate` 头提取（W3C TraceContext）
- 未接入客户端 → 网关自动生成根 span context
- 向 Provider 传播 → 注入 `traceparent` 头（Provider 侧无需支持）
- 响应头 `X-Request-Id` 永久透传，便于客户端排障关联

### 5.3 结构化错误响应

统一改造 `ErrorResponse`，强制字段：

```json
{
  "error": {
    "message": "Rate limit exceeded",
    "type": "rate_limit_exceeded",
    "code": "rate_limit_exceeded",
    "param": null,
    "request_id": "req_01J...",
    "retry_after_seconds": 12,
    "details": {
      "tenant_id": "team-ml",
      "limit_tpm": 50000,
      "current_tpm": 50231
    }
  }
}
```

**错误类型→HTTP→code 映射**：

| GatewayError | HTTP | `error.code` | 备注 |
|---|---|---|---|
| AuthenticationFailed | 401 | `authentication_error` | 含 request_id |
| BadRequest | 400 | `invalid_request_error` | param 字段填充 |
| ProviderNotFound | 404 | `model_not_found` | 推荐可用模型列表 |
| RateLimited | 429 | `rate_limit_exceeded` | 网关自有限流 |
| QuotaExceeded (新增) | 429 | `quota_exceeded` | 租户配额命中 |
| UpstreamError(5xx) | 502 | `upstream_error` | 含 upstream_status |
| UpstreamError(503) | 503 | `server_overloaded` | 含 retry_after_seconds |
| Internal | 500 | `internal_error` | 内部不泄露敏感字段 |

**注意**：所有错误响应都带 `X-Request-Id` 响应头，方便关联排查。

### 5.4 分级健康检查

| 端点 | HTTP 方法 | 用途 | 检查内容 | 认证 |
|------|-----------|------|---------|------|
| `/healthz` | GET | K8s liveness | 进程存活 | 无 |
| `/readyz` | GET | K8s readiness | 配置加载 + cache 就绪 + 至少一个 provider circuit-closed | 无 |
| `/deep-health` | GET | 负载均衡/排障 | `/readyz` + 每个 provider 最近 5 次调用 snapshot | 无 |
| `/metrics` | GET | Prometheus 抓取 | 暴露全部 metrics | 无（内网） |

**返回码**：K8s 友好，200 = 可用，503 = 不可用，Content-Type `application/json`，例如：
```json
{
  "status": "ready",
  "checks": {
    "config_loaded": "ok",
    "cache_ready": "ok",
    "providers": {
      "openai": "closed",
      "anthropic": "closed",
      "ollama": "open"
    }
  }
}
```

### 5.5 审计日志（AuditLog）

独立审计 sink（stdout JSON 或 stderr 独立文件），异步写不阻塞请求路径。

**审计事件清单**：

| 事件 | 触发点 | 内容 |
|------|--------|------|
| `auth.login_success` | auth 中间件通过 | {key_id, tenant, role} |
| `auth.login_failure` | auth 失败 | {reason, key_fingerprint} |
| `key.create / revoke` | admin key 操作 | {actor, key_id, tenant} |
| `tenant.create / update / delete` | admin 租户操作 | {actor, tenant, changes} |
| `provider.create / update / delete` | admin 操作 | {actor, provider, changes} |
| `config.update` | admin 配置热更新 | {actor, section, before/after} |
| `quota.update` | admin 更新配额 | {actor, tenant, limits} |

**审计日志格式** (JSON)：
```json
{
  "timestamp": "2026-07-02T10:23:45.123Z",
  "type": "audit",
  "actor": {"key_id": "key_01H...", "tenant": "platform-team", "role": "admin"},
  "action": "provider.update",
  "resource": "providers.openai",
  "status": "success",
  "changes": {"models": {"before": ["gpt-4"], "after": ["gpt-4", "gpt-4o"]}},
  "request_id": "req_01J..."
}
```

**实现**：`audit::AuditWriter` (channel + 独立 task 批量落盘)

### 5.6 依赖变更

```toml
# workspace 新增
prometheus = "0.13"
opentelemetry = { version = "0.27", optional = true }
opentelemetry-otlp = { version = "0.27", optional = true }
tracing-opentelemetry = { version = "0.28", optional = true }
rustls = { version = "0.23", optional = true }
tokio-rustls = { version = "0.26", optional = true }

# features (gateway-server)
[features]
default = []
otel = ["dep:opentelemetry", "dep:opentelemetry-otlp", "dep:tracing-opentelemetry"]
tls = ["dep:rustls", "dep:tokio-rustls"]
```

---

## 6. 计量与配额

### 6.1 Metering（计量器）

Metering 在每次 Provider 返回 usage 后异步记录一条计量事件：

```rust
pub struct MeteringEvent {
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    pub tenant_id: String,
    pub key_id: String,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub status: RequestStatus,
    pub estimated_cost: Option<Decimal>,   // 当前留 None，计费期再统一填
}
```

**聚合维度**：`key / tenant / model / provider` 四维聚合
**时间粒度**：`minute / hour / day / month`（滑动窗口）

**持久化**：
- 主存：内存环形缓冲（容量可配置）
- 快照：定期 JSON 落盘 `config_dir/metrics_YYYY-MM-DD.json`
- 保留期 N 轮，滚动删除
- 未来可插 TimescaleDB / Cassandra（预留 `MeteringStore` 接口）

**API 输出**：
- `GET /api/admin/metrics?granularity=day&tenant=team-ml`
- 未来预留 `GET /api/admin/costs?...`（返回 501 Not Implemented 直到计费期）

### 6.2 QuotaEngine（配额引擎）```rust
pub struct QuotaLimits {
    pub max_rpm: u32,   // 每分钟请求数 (tenant 级)
    pub max_rpd: u64,   // 每日请求数
    pub max_tpm: u64,   // 每分钟 token
    pub max_tpd: u64,   // 每日 token
}```

**校验顺序** (快速短路，节省配额请求)：1. RPM — 本地 token-bucket（微秒级，按 tenant 维）2. TPM — 滑动窗口，环形计数
3. RPD / TPD — 日切，持久化计数器（可跨重启累计）
4. 任一命中 → 429 + retry_after_seconds + details**配额快照**（持久化）：- 每 N 秒快照到 `config_dir/quotas.json`
- 启动时恢复，保证日/月配额跨重启累计- 故障恢复窗口内可能少量超出（可接受，注释说明）

**超限响应**（QuotaExceeded → 429 + code `quota_exceeded`）：```json
{
  "error": {
    "type": "quota_exceeded",
    "code": "quota_exceeded",
    "message": "Daily token quota exceeded for tenant 'team-ml': 1,050,000 / 1,000,000",
    "details": {
      "tenant_id": "team-ml",
      "limit_tpd": 1000000,
      "current_tpd": 1050000,
      "resets_at": "2026-07-03T00:00:00Z"
    },
    "retry_after_seconds": 3600,
    "request_id": "req_01J..."
  }
}```

### 6.3 成本预留（Cost 预留，不实作）

- `MeteringEvent.estimated_cost: Option<Decimal>` 字段占位
- `PricingTable` 结构预留（config `pricing` section 在 7.4 描述）
- `GET /api/admin/costs` 接口占位返回 501 Not Implemented
- 计费期加载：按 `model → {input_per_million, output_per_million, currency}` 计算

---

## 7. 多 Provider 扩展

### 7.1 OpenAICompatProvider（通用兼容适配器）

```rust
// providers/src/openai_compat.rs (新增)
pub struct OpenAICompatProvider {
    name: String,
    api_key: Option<String>,
    base_url: String,
    models: Vec<String>,
    field_overrides: FieldOverrides,
    extra_headers: HashMap<String, String>,
    client: reqwest::Client,
}

pub struct FieldOverrides {
    pub emit_reasoning_content: bool,
    pub chat_template_kwargs: Option<serde_json::Value>,
    pub stream_field_renames: HashMap<String, String>,
}```

**关键行为**：
- 90% 请求 OpenAI 兼容透传
- Provider 调用前合并 `extra` + `field_overrides`
- 独有字段通过 `extra` 透传，不强校验（最大程度兼容新 provider）
- 流式响应时做字段重命名（例如 DeepSeek `reasoning_content` 保留为扩展 delta 字段）

**加载注册**：`register_provider` / admin create provider 时对 `'openai-compat'` provider type 走新适配器；`'deepseek' / 'vllm' / 'kimi' / 'sglang' / 'tgi' / 'zhipu'` 等别名映射到 `OpenAICompatProvider` 加预设 `field_overrides`。

### 7.2 首批新增 Provider 清单

| Provider | 类型 | Base URL 默认值 | 独有字段 |
|---------|------|----------------|---------|
| **DeepSeek Chat** | OpenAI 兼容 | `https://api.deepseek-chat.com/v1` | `reasoning_content`、`chat_template_kwargs`、`thinking_budget` |
| **DeepSeek Reasoner** | OpenAI 兼容 | 同上 | 同上 + `message.reasoning_content` |
| **Kimi (Moonshot)** | OpenAI 兼容 | `https://api.moonshot.cn/v1` | 基本兼容，模型 `moonshot-v1-*` |
| **智谱 GLM** | OpenAI 兼容 | `https://open.bigmodel.cn/api/paas/v4` | `tool_choice` 行为略异 |
| **vLLM** (自部署) | OpenAI 兼容 | 用户配置 | 完全兼容，模型用户定 |
| **SGLang** (自部署) | OpenAI 兼容 | 用户配置 | 完全兼容，模型用户定 |
| **TGI** (自部署) | OpenAI 兼容 | 用户配置 | 完全兼容，模型用户定 |

### 7.3 Router 策略升级

```rust
pub enum RouteStrategy {
    Static,                // 模型严格绑定 provider
    Failover,              // 主 provider circuit_open → 自动切换备
    TenantOverride,        // tenant 维度覆盖
    WeightedRoundRobin,    // 按权重灰度分流}

pub struct Router {
    base_map: HashMap<String, String>,    tenant_overrides: HashMap<String, HashMap<String, String>>,
    failover: HashMap<String, Vec<String>>,
    weights: HashMap<String, Vec<(String, f32)>>,
}
```

**路由决策优先级**：
1. **tenant_override** (最高优先级，满足业务强隔离需求)
2. **failover** (主 provider circuit_open/5xx → 自动切备)
3. **weighted/AB** (按权重灰度)
4. **static** (默认)
### 7.4 Pricing Config 预留

```toml
# 未来启用计费时加在 config.toml
[pricing]
enabled = false

[pricing.deepseek.deepseek-chat]
input_per_million = 0.07
output_per_million = 0.28
currency = "CNY"

[pricing.openai.gpt-4o]
input_per_million = 2.50
output_per_million = 10.00
currency = "USD"
```

当前 Spec 不实施，仅确保 `MeteringEvent.estimated_cost` 字段和 `/api/admin/costs` 接口形态就位。

---

## 8. 数据流与部署

### 8.1 请求生命周期（完整）

```
ENTRY: POST /v1/chat/completions
   │
   ▼
① Transport Layer
   ├─ 生成 X-Request-Id (UUIDv4)
   ├─ body size 检验 (默认 2MB)
   ├─ CORS 放行 (Admin UI)
   └─ OTel 根 span 接入
   │
   ▼
② Auth Provider
   ├─ 提取 Authorization: Bearer <key>
   ├─ HMAC 验证 ApiKeyEntry
   ├─ 校验 status/active/expires
   └─ 注入 AuthClaims 到 request extensions
   │
   ▼
③ Tenant Middleware
   ├─ 装载 TenantContext (quotas, 可用模型)
   └─ 默认 tenant 兜底
   │
   ▼
④ Policy Engine (顺序短路)
   ├─ 校验 model/参数合法性
   ├─ 限流 local token-bucket → tenant RPM
   ├─ 配额 TPM/TPD 滑动窗口
   └─ 命中 → 429+retry_after+details
   │
   ▼
⑤ Cache Lookup
   ├─ compute_key(model, messages, temperature, max_tokens, tenant_id)
   └─ HIT → 计量(缓存命中) → Response; MISS → 继续
   │
   ▼
⑥ Router Decision
   ├─ base_map → tenant_override → failover → weighted/static
   └─ 选定 provider
   │
   ▼
⑦ Resilience Stack (per provider)
   ├─ CircuitBreaker.allow_request?
   ├─ Retry { exp backoff 1s→16s, max_retries 2 }
   ├─ Cross-Provider fallback (failover chain)
   └─ 流式 → 跳过 intra-provider retry，直退备用
   │
   ▼
⑧ Provider Dispatch (LLMProvider trait)
   ├─ convert_request (如有 converter)
   ├─ HTTP 调用 (connection pool, 30s 超时)
   └─ convert_response → ChatCompletionResponse
   │
   ▼
⑨ Post Process
   ├─ Cache Write (TTL)
   ├─ Metering.record
   ├─ Metrics 更新
   └─ 审计留痕
   │
   ▼
EXIT: Response (X-Request-Id 透传)
```

### 8.2 K8s 部署清单

```
deploy/
  namespace.yaml
  configmap.yaml
  secret.template.yaml
  deployment.yaml
  service.yaml
  ingress.yaml
  servicemonitor.yaml
  prometheus-rules.yaml
```

### 8.3 SLO 目标

| 指标 | 目标 |
|------|------|
| 可用性 | 99.9%（月停机 < 43 分钟）|
| P99 网关额外延迟 | < 5ms |
| 全年错误率 | < 0.5%（不含客户端 4xx 与上游限速）|

### 8.4 不变量

1. Provider trait 签名稳定
2. OpenAI wire format 向后兼容
3. Admin UI 零依赖（单二进制部署）
4. 默认配置安全启动（无 secret 拒绝启动）
5. 计量与限流不阻塞请求路径（异步 channel + 批量）

---

## 9. API 变更清单

### 9.1 新增端点

| 端点 | 方法 | 用途 | 认证 |
|------|------|------|------|
| `/metrics` | GET | Prometheus 指标导出 | 无（内网） |
| `/healthz` | GET | K8s liveness | 无 |
| `/readyz` | GET | readiness 探测 | 无 |
| `/deep-health` | GET | 详细排障 snapshot | 无 |
| `/api/admin/tenants` | CRUD | 租户管理 | tenant-admin / admin |
| `/api/admin/quota/{tenant}` | GET / PUT | 租户配额 | tenant-admin / admin |
| `/api/admin/audit-logs` | GET | 审计日志查询 | admin |
| `/api/admin/costs` | GET | 成本预留（返回 501 直到计费期） | admin |

### 9.2 修改现有端点

| 端点 | 变化 |
|------|------|
| `/v1/chat/completions` | 请求需认证；响应加 `X-Request-Id`；错误结构化（含 `request_id`, `retry_after_seconds`, `details`） |
| `/v1/models` | 经 tenant 过滤可访问模型列表；响应加 `X-Request-Id` |
| `/api/admin/providers` | tenanted；create 支持 `type: openai-compat` + `field_overrides` + `extra_headers` |
| `/api/admin/keys` | 返回 key_id + role + tenant 替代明文 key；create key 必填 tenant/role |
| `/api/admin/config/*` | 继续支持；新增 `config/quota` PUT |
| `/api/admin/metrics` | 继续保留（Admin UI 兼容）；`/metrics` 为 Prometheus 版 |

### 9.3 兼容承诺

- 现有 `Authorization: Bearer <key>` 调用方**无需修改**
- 非管理员不可调 admin 端点（行为变化，符合安全升级方向）
- 默认配置移除 `test-key`，新部署必须配置（否则拒绝启动）
- Admin UI 新增若干管理页面（保持零依赖）

---

## 10. 技术约束与依赖

### 10.1 新增 workspace 依赖

```toml
prometheus = "0.13"
opentelemetry = { version = "0.27", optional = true }
opentelemetry-otlp = { version = "0.27", optional = true, features = ["grpc-tonic"] }
tracing-opentelemetry = { version = "0.28", optional = true }
rustls = { version = "0.23", optional = true }
tokio-rustls = { version = "0.26", optional = true }
```

### 10.2 Feature gate

```toml
# crates/gateway-server/Cargo.toml
[features]
default = []
otel = ["dep:opentelemetry", "dep:opentelemetry-otlp", "dep:tracing-opentelemetry"]
tls = ["dep:rustls", "dep:tokio-rustls"]
```

---

## 11. 风险与缓解

| 风险 | 可能性 | 影响 | 缓解 |
|------|--------|------|------|
| 计量/限流异步通道加剧内存 | 中 | 高 | 通道上限 + overflow 降级记录；capacity 可观测 |
| Key HMAC 与现有明文 key 迁移不兼容 | 中 | 中 | 双读共存一个版本后废除旧字段；启动时迁移 |
| OTel exporter 网关抖动 | 中 | 中 | OTel 异步 background；channel overflow 降级 drop；默认 off |
| vLLM/SGLang/TGI 自部署兼容性差异 | 高 | 低 | field override 模式不强校验；详尽 client 配置文档 |
| 多租户配额日切边界并发少量超出 | 高 | 低 | 文档声明接受；未来 Redis 原子增量解 |
| Provider converter 长期「抽象泄漏」 | 中 | 中 | Bedrock/Azure 出现真实需求时立即做方案 B 子重构 |
| moka DefaultHasher 跨重启缓存不一致 | 低 | 低 | 可接受；未来换确定性 hash 或 canonical JSON |

---

## 12. 交付里程碑与优先级

### MVP 0 — 安全闭坑（P0，立即）

修复 6 项当前代码安全问题（auth/rate_limit 挂载、HMAC、extra_headers、拼写错误、dead code 清理）

### MVP 1 — 多租户与 RBAC（P0）

`AuthProvider` trait + `Role` enum + 三角色权限矩阵 + `TenantContext` + Admin UI 租户管理页

### MVP 2 — 计量与配额（P1）

`MeteringService` 异步计量 + `QuotaEngine` 四层配额 + 超限结构化错误 + 升级 Dashboard

### MVP 3 — 可观测性（P1）

Prometheus `/metrics` + 分级健康检查 + 结构化错误 + K8s 清单与 ServiceMonitor + graceful shutdown

### MVP 4 — OTel 与审计（P2）

OpenTelemetry 链路追踪（feature gate）+ W3C TraceContext 传播 + AuditWriter 异步落盘 + 审计日志查询 API

### MVP 5 — 多 Provider 扩展（P1）

`OpenAICompatProvider` + 首批 7 Provider + Router 策略升级（Failover/TenantOverride/Weighted/AB）

### MVP 6 — 成本计费（P3，后续独立迭代）

`PricingTable` + `estimated_cost` 计算 + 成本看板 + 对账导出

### 依赖关系

```
MVP 0 (安全闭坑)
  └─▶ MVP 1 (多租户 + RBAC)
         ├─▶ MVP 2 (计量配额)
         ├─▶ MVP 3 (可观测)       ← 并行
         └─▶ MVP 5 (多 Provider)
                                  │
                                  ▼
                          MVP 4 (OTel + 审计)
                                  │
                                  ▼
                          MVP 6 (成本计费) [后续独立迭代]
```

---

## 附录 A：类型签名索引

| 签名文件 | 用途 |
|---------|------|
| `AuthClaims { key_id, tenant_id, role, key_hmac }` — crates/gateway-core/src/auth.rs | 认证声明 |
| `trait AuthProvider { authenticate(cred) }` — crates/gateway-core/src/auth.rs | 身份认证 |
| `TenantContext { tenant_id, role, key_id, quotas }` — gateway-server/.../tenant.rs | 租户上下文 |
| `QuotaLimits { max_rpm, max_rpd, max_tpm, max_tpd }` — gateway-core/src/config.rs | 配额配置 |
| `MeteringEvent { request_id, tokens, cost }` — gateway-server/.../metering.rs | 计量事件 |
| `RouteStrategy { Static, Failover, TenantOverride, WeightedRoundRobin }` — gateway-server/router.rs | 路由策略 |
| `OpenAICompatProvider { field_overrides }` — providers/.../openai_compat.rs | 兼容适配器 |
| `FieldOverrides { emit_reasoning_content, chat_template_kwargs, stream_field_renames }` — 同上 | 字段覆盖 |
| `enum Role { Admin, TenantAdmin, Developer }` — gateway-core/src/auth.rs | 角色 |

## 附录 B：Open Questions（实施阶段确认，不阻塞 spec 批准）

| # | 问题 | 建议默认 |
|---|------|---------|
| 1 | Key 更换迁移期长度（双读明文 hash 共存几版）| 1 个小版本后废除旧字段 |
| 2 | metrics snapshot 落盘频率 & 保留期 | 30s / 30 天 |
| 3 | vLLM/SGLang/TGI 部署文档归属 | 独立文档，不在本 spec 范围 |
| 4 | 审计日志保留期 | 默认 90 天 |
| 5 | Redis 多副本 case 在哪个 milestone 启动 | 企业 V2；V1 保持单副本本地状态 |
| 6 | Admin UI 国际化（中/英） | 先中文主导，后续抽取翻译文件 |

---

*本文档由 brainstorming 流程产出，待 spec self-review 后进入用户审读。*
