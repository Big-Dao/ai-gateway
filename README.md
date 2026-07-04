# AI Gateway (Rust)

一个用 Rust 编写的 AI 网关，为多个 LLM 提供商提供统一的 OpenAI 兼容 REST API 接口。

## 特性

- 🔀 **统一入口** — OpenAI 格式的 `/v1/chat/completions`，自动路由到对应提供商
- 🤖 **多提供商支持** — OpenAI、Anthropic Claude、Google Gemini、Ollama（本地模型）
- 🔑 **API Key 认证** — Bearer Token 鉴权，支持运行时热更新
- 💾 **响应缓存** — 基于 moka 的高性能 TTL 缓存，减少重复请求成本
- 📊 **用量统计** — 请求数、Token 用量、错误计数、按模型分布
- 🌊 **流式响应** — 完整 SSE 流式支持
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

# 查看可用模型
curl http://localhost:8080/v1/models

# 健康检查
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

```bash
# 获取完整指标
curl http://localhost:8080/api/admin/metrics

# 列出提供商
curl http://localhost:8080/api/admin/providers

# 添加提供商
curl -X POST http://localhost:8080/api/admin/providers \
  -H "Content-Type: application/json" \
  -d '{"name":"mistral","api_key":"xxx","models":["mistral-7b"]}'

# 更新提供商
curl -X PUT http://localhost:8080/api/admin/providers/openai \
  -H "Content-Type: application/json" \
  -d '{"models":["gpt-4o","gpt-5"]}'

# 删除提供商
curl -X DELETE http://localhost:8080/api/admin/providers/mistral

# 添加 API Key
curl -X POST http://localhost:8080/api/admin/keys \
  -H "Content-Type: application/json" \
  -d '{"key":"new-client-key"}'

# 删除 API Key
curl -X DELETE http://localhost:8080/api/admin/keys/old-key

# 更新缓存配置
curl -X PUT http://localhost:8080/api/admin/config/cache \
  -H "Content-Type: application/json" \
  -d '{"enabled":true,"max_capacity":2000,"ttl_seconds":600}'

# 更新限流
curl -X PUT http://localhost:8080/api/admin/config/rate-limit \
  -H "Content-Type: application/json" \
  -d '{"requests_per_minute":120}'

# 获取日志
curl http://localhost:8080/api/admin/logs
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
    │   └── error.rs              # 统一错误
    ├── providers/                # 各提供商适配
    │   ├── openai.rs
    │   ├── anthropic.rs          # OpenAI ↔ Anthropic 格式转换
    │   ├── gemini.rs             # OpenAI ↔ Gemini 格式转换
    │   └── ollama.rs             # OpenAI ↔ Ollama 格式转换
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
