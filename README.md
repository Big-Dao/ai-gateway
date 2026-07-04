# AI Gateway (Rust)

一个用 Rust 编写的 AI 网关，为多个 LLM 提供商提供统一的 OpenAI 兼容 REST API 接口。

## 特性

- 🔀 **统一入口** — OpenAI 格式的 `/v1/chat/completions`，自动路由到对应提供商
- 🤖 **多提供商支持** — 内置 OpenAI / Anthropic Claude / Google Gemini / Ollama；任意 OpenAI 兼容端点（Mistral / Groq / Together / DeepSeek / vLLM …）经 `openai_compat` 适配器接入
- 🔑 **认证与多租户** — API Key 经 HMAC-SHA256 + 随机 salt 哈希存储；按租户隔离，RBAC 三级角色（`admin` / `tenant_admin` / `developer`）
- 🛡️ **限流与配额** — 全局令牌桶限流 + 每租户 RPM/RPD/TPM/TPD 配额，超限自动拒绝并返回 `Retry-After`
- ⚡ **弹性与降级** — 指数退避重试、按 Provider 隔离的熔断器、跨 Provider fallback 链
- 💾 **响应缓存** — 基于 moka 的高性能 TTL 缓存，减少重复请求成本
- 📊 **计量与成本** — 按租户/模型记录 token 用量；`PricingTable` 估算成本，`/costs` 汇总 + 阈值告警 + 账单周期重置
- 🌊 **流式响应** — 完整 SSE 流式支持
- 📈 **可观测性** — Prometheus 指标（`/metrics`）、结构化 JSON 日志、内存日志缓冲、`/deep-health` 上游健康探针、可选 JSONL 审计日志
- 🖥️ **Admin UI** — 内置 Web 管理面板，零依赖前端，支持运行时配置热更新

## 快速开始

### 1. 配置

```bash
cp config.example.toml config.toml
# 编辑 config.toml，填入你的 API Key
```

### 2. 构建 & 运行

```bash
cargo build --release
cargo run --bin gateway-server
```

或通过环境变量指定配置文件路径：

```bash
CONFIG_PATH=/path/to/config.toml cargo run --bin gateway-server
```

启动后访问:
- API: `http://localhost:8080`
- Admin UI: `http://localhost:8080/admin`

### 3. API 测试

```bash
# 非流式请求
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer my-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4o-mini",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'

# 流式请求
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer my-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-6",
    "messages": [{"role": "user", "content": "Hi"}],
    "stream": true
  }'

# 查看可用模型（/v1/* 需鉴权）
curl http://localhost:8080/v1/models -H "Authorization: Bearer my-secret-key"

# 健康检查（/health、/healthz、/readyz、/deep-health、/metrics 免鉴权）
curl http://localhost:8080/health
```

### 4. Admin UI

启动后浏览器打开 `http://localhost:8080/admin`，可管理:

| 页面 | 功能 |
|------|------|
| 📊 仪表盘 | 实时请求量、Token 用量、模型分布图表、系统状态 |
| 🤖 提供商 | 查看/添加/编辑/删除提供商（名称、API Key、Base URL、模型列表） |
| 🔑 API Keys | 管理客户端 API Key 的创建/删除 |
| ⚙️ 系统配置 | 调整缓存（启用/容量/TTL）和限流（RPM）参数 |
| 📋 实时日志 | 自动滚动的请求日志流，支持清空和自动刷新开关 |

### 5. Admin API

> 所有 `/api/admin/*` 端点均需 Bearer Token 鉴权。写操作（增删 provider/key、改配置、重置账单）要求 `admin` 角色；只读查询（metrics/usage/costs/logs）`developer` 角色亦可。下方 `<admin-key>` 代表具备 admin 权限的 key。

```bash
# 获取完整指标
curl http://localhost:8080/api/admin/metrics -H "Authorization: Bearer <admin-key>"

# 列出提供商
curl http://localhost:8080/api/admin/providers -H "Authorization: Bearer <admin-key>"

# 添加提供商
curl -X POST http://localhost:8080/api/admin/providers \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"name":"mistral","api_key":"xxx","models":["mistral-7b"]}'

# 更新提供商
curl -X PUT http://localhost:8080/api/admin/providers/openai \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"models":["gpt-4o","gpt-5"]}'

# 删除提供商
curl -X DELETE http://localhost:8080/api/admin/providers/mistral \
  -H "Authorization: Bearer <admin-key>"

# 添加 API Key
curl -X POST http://localhost:8080/api/admin/keys \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"key":"new-client-key"}'

# 删除 API Key
curl -X DELETE http://localhost:8080/api/admin/keys/old-key \
  -H "Authorization: Bearer <admin-key>"

# 更新缓存配置
curl -X PUT http://localhost:8080/api/admin/config/cache \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"enabled":true,"max_capacity":2000,"ttl_seconds":600}'

# 更新限流
curl -X PUT http://localhost:8080/api/admin/config/rate-limit \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"requests_per_minute":120}'

# 费率卡（legacy 平台级，每 1M token 成本；推荐改用 [pricing] 配置）
curl http://localhost:8080/api/admin/config/rate-card -H "Authorization: Bearer <admin-key>"
curl -X PUT http://localhost:8080/api/admin/config/rate-card \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"prompt_per_million":100,"completion_per_million":300}'

# 租户管理
curl http://localhost:8080/api/admin/tenants -H "Authorization: Bearer <admin-key>"
curl -X POST http://localhost:8080/api/admin/tenants \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"id":"acme"}'
curl -X DELETE http://localhost:8080/api/admin/tenants/acme -H "Authorization: Bearer <admin-key>"

# 设置租户配额（RPM/RPD/TPM/TPD + 成本阈值告警，单位：分）
curl -X PUT http://localhost:8080/api/admin/config/quota/acme \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" \
  -d '{"max_rpm":120,"max_tpd":10000000,"cost_alert_threshold_cents":5000.0}'

# 用量查询（全量 或 单租户；developer 角色只能查本租户）
curl http://localhost:8080/api/admin/usage -H "Authorization: Bearer <admin-key>"
curl http://localhost:8080/api/admin/usage/acme -H "Authorization: Bearer <admin-key>"

# 各 Provider 熔断器状态
curl http://localhost:8080/api/admin/circuit-breaker -H "Authorization: Bearer <admin-key>"

# 成本汇总（MVP 6，window=24h|7d|30d，可选 ?tenant=<id>）
curl "http://localhost:8080/api/admin/costs?window=24h" -H "Authorization: Bearer <admin-key>"

# 重置账单周期（admin only）
curl -X POST http://localhost:8080/api/admin/billing/reset -H "Authorization: Bearer <admin-key>"

# 获取日志
curl http://localhost:8080/api/admin/logs -H "Authorization: Bearer <admin-key>"
```

## 项目结构

```
ai-gateway/
├── Cargo.toml                    # Workspace root
├── config.example.toml           # 示例配置
├── README.md
└── crates/
    ├── gateway-core/             # 核心类型、trait、配置、错误
    │   ├── types.rs              # OpenAI 兼容请求/响应类型
    │   ├── provider.rs           # LLMProvider trait
    │   ├── config.rs             # TOML 配置
    │   ├── tenant.rs             # 租户 / RBAC 角色 / TenantContext
    │   ├── auth_key.rs           # API Key HMAC-SHA256 + salt 存储
    │   ├── metering.rs           # PricingTable / RateCard 计费原语
    │   ├── audit.rs              # 审计日志 writer trait
    │   └── error.rs              # 统一错误
    ├── providers/                # 各提供商适配
    │   ├── openai.rs
    │   ├── anthropic.rs          # OpenAI ↔ Anthropic 格式转换
    │   ├── gemini.rs             # OpenAI ↔ Gemini 格式转换
    │   ├── ollama.rs             # OpenAI ↔ Ollama 格式转换
    │   └── openai_compat.rs      # 任意 OpenAI 兼容端点 + field_overrides
    └── gateway-server/           # HTTP 服务
        ├── main.rs               # 启动入口
        ├── routes.rs             # 主路由 + SSE
        ├── admin.rs              # Admin REST API (20+ 端点)
        ├── state.rs              # AppState (RwLock 热更新)
        ├── circuit_breaker.rs    # 熔断器
        ├── retry.rs              # 指数退避 + 跨 Provider 降级
        ├── json_logger.rs        # 结构化 JSON 日志
        ├── log_buffer.rs         # 内存日志环形缓冲
        ├── static_files.rs       # Admin UI 前端嵌入
        ├── metrics/              # 计量 / 配额 / Prometheus
        │   ├── metering.rs       # 按租户/模型 token 用量记录
        │   ├── quota.rs          # 每租户配额追踪
        │   └── prometheus.rs     # 指标导出
        └── middleware/            # Axum 中间件
            ├── auth.rs           # Bearer → HMAC 校验 → TenantContext
            ├── rate_limit.rs     # 每租户令牌桶限流
            ├── quota_middleware.rs  # 每租户 RPM/RPD/TPM/TPD 配额
            ├── rbac.rs           # 角色权限校验
            └── x_request_id.rs   # 请求 ID + Retry-After 传播
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `CONFIG_PATH` | 配置文件路径 | `config.toml` |
| `RUST_LOG` | 日志级别过滤 | `info` |

也可通过 `AI_GATEWAY__SECTION__KEY` 格式的环境变量覆盖任意配置项。

### 审计日志（可选）

```toml
# 在 config.toml 中启用 JSONL 审计日志
audit_path = "/var/log/ai-gateway/audit.jsonl"
```

管理员对配置/配额/密钥的每次写操作都会以 JSON 行格式追加到该文件。

## 架构

```
Client ──→ AI Gateway (Axum)
                │
                ├─ Auth Middleware
                ├─ Rate Limit Middleware
                ├─ Route → Model Router
                │
                ├─ OpenAI Provider ────→ api.openai.com
                ├─ Anthropic Provider ──→ api.anthropic.com
                ├─ Gemini Provider ────→ generativelanguage.googleapis.com
                └─ Ollama Provider ────→ localhost:11434
```

## License

MIT
