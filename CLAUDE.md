# AI Gateway — 项目开发规范

> Rust 编写的多 LLM 提供商统一网关（OpenAI 兼容 API）。本文件是 Claude Code 的工作规范——
> **用户可见的 API/部署文档以 `README.md` 为准，本文件只讲"怎么改、怎么验、坑在哪"。**

---

## 铁律：没有测试验证，不准说"完成"

每次代码改动，**必须执行下方"质量门禁"全部步骤**，缺一不可。本仓库 CI 已强制相同标准——
本地不通过 = CI 必红。违反流程 = 没有完成，**不准**报告"已修复/已完成"。

### 质量门禁（与 `.github/workflows/ci.yml` 对齐，共 6 项）

```bash
# 0. 格式（CI: fmt job）
cargo fmt --all -- --check

# 1. 编译（快速反馈，必须 Finished、零 error）
cargo check --workspace 2>&1 | tail -5

# 2. Lint —— CI 以 -D warnings 视为 error，本地也必须零 warning（见下方"已知问题"当前未达标）
cargo clippy --workspace --all-targets -- --deny warnings 2>&1 | tail -15

# 3. 单测 + 集成测试（必须全部 ok，无 FAILED）
cargo test --workspace --all-targets 2>&1 | tail -25

# 4. 文档构建（CI: cargo doc）
cargo doc --workspace --no-deps

# 5. 安全审计（CI: security job；需先 cargo install cargo-audit --locked）
cargo audit
```

### 端到端验证（用户可见的改动必须做）

```bash
# 启动（二进制名 gateway-server，default-run 已配）
nohup cargo run --bin gateway-server > /tmp/gateway.log 2>&1 &
sleep 8

# config.example.toml 默认 auth.api_keys = ["my-secret-key", "another-key"]
# 这些明文 key 在启动时被 HMAC-SHA256 哈希入库；客户端调用时仍发送原始明文 key。
KEY=my-secret-key

curl -s -o /dev/null -w "health=%{http_code}\n" http://localhost:8080/health          # 期望 200
curl -s -o /dev/null -w "models=%{http_code}\n" -H "Authorization: Bearer $KEY" http://localhost:8080/v1/models
# 按改动列出每个受影响端点的期望状态码/响应体……

pkill -9 -f gateway-server
```

### 汇报前自检
- [ ] `cargo fmt --check` 通过
- [ ] `cargo check` 零 error
- [ ] `cargo clippy --deny warnings` 零 warning
- [ ] `cargo test --workspace --all-targets` 全通过
- [ ] 端点实际返回期望结果（附 curl 输出）
- [ ] 对照任务清单逐项打勾

---

## 铁律落地（harness，机器化）

> 本节由 `.claude/` harness 提供。铁律的三步已封装成一条命令，不再靠自觉。

**声明任何 Rust 改动"完成 / 已修复"之前，或提交含 `.rs` 的改动之前，必须先跑：**

```bash
make verify          # 等价：bash .claude/skills/verify/verify.sh
```

它依次执行 `cargo check` → `cargo test` → 启动服务 curl E2E，全过才写 `target/.verified`（15 分钟内有效）。
`.claude/hooks/verify-gate.sh` 会在 `git commit` 含 `.rs` 时强制要求该标记新鲜，否则拦下并把原因回灌。

### Definition of Done（自查，缺一不可）
- [ ] `make verify` 三步全过，`target/.verified` 已刷新
- [ ] 对照任务清单 / 已知问题清单逐项打勾
- [ ] 若改了鉴权 / 计费 / 限流 —— 跑过 `.claude/agents/security-reviewer.md` 子代理
- [ ] 若改了并发 / 状态 / 错误路径 —— 跑过 `.claude/agents/rust-reviewer.md` 子代理

违反以上 = 没有完成，不准报告"已修复 / 已完成"。

### 自主工作队列
未勾选的「已知问题」即自主 session 的待办：启动时读清单，挑下一项，`make verify` 通过后再交付。
（harness 细节见 `.claude/README.md`。）

---

## 项目结构

```
crates/
├── gateway-core/              # 核心类型、trait、配置、错误、鉴权原语
│   ├── types.rs               #   OpenAI 兼容请求/响应类型
│   ├── provider.rs            #   LLMProvider trait
│   ├── config.rs              #   TOML 配置 + AI_GATEWAY__ 环境变量覆盖
│   ├── auth_key.rs            #   API Key：HMAC-SHA256 + 随机 salt，不存明文
│   ├── tenant.rs              #   TenantContext（租户/角色/key 指纹）
│   ├── metering.rs            #   用量计量事件结构
│   ├── audit.rs               #   审计日志（管理员写操作 JSONL 追加）
│   └── error.rs               #   统一 GatewayError
├── providers/                 # 提供商适配器（实现 LLMProvider）
│   ├── openai.rs / openai_compat.rs   # 原生 + OpenAI 兼容（Mistral/Groq 等）
│   ├── anthropic.rs / gemini.rs / ollama.rs   # 含 OpenAI ↔ 各家格式转换
└── gateway-server/            # Axum HTTP 服务
    ├── main.rs                #   启动入口
    ├── routes.rs              #   /v1/* 主路由 + SSE 流式
    ├── admin.rs               #   /api/admin/* REST（20+ 端点，RBAC 守卫）
    ├── state.rs               #   AppState（RwLock 热更新）
    ├── persistence.rs         #   计量事件 JSONL 落盘 + 启动回放（#6）
    ├── retry.rs               #   指数退避 + 跨 Provider 降级链
    ├── circuit_breaker.rs     #   上游熔断器
    ├── metrics/               #   计量 / 配额 / Prometheus 导出
    ├── middleware/            #   auth / rate_limit / quota / rbac / x_request_id
    └── tests/                 #   集成测试（见下方"测试"）
```

**请求流：** Client → `auth` → `rate_limit` → `quota` → 路由 → `retry`/`circuit_breaker`
→ 选定 Provider（OpenAI/Anthropic/Gemini/Ollama）→ 上游 → 计量/成本入账（可落盘）→ 响应（可 SSE）。

## 配置与环境

| 变量 / 文件 | 说明 |
|---|---|
| `config.toml` | 运行时配置（gitignored）；模板见 `config.example.toml` |
| `CONFIG_PATH` | 指定配置文件路径，默认 `config.toml` |
| `RUST_LOG` | 日志级别过滤，默认 `info` |
| `AI_GATEWAY__SECTION__KEY` | 覆盖任意配置项，双下划线分层（如 `AI_GATEWAY__SERVER__PORT`）。**前缀曾误拼为 `AI_GATERARY`，已修正** |
| `audit_path` | 顶层配置项；启用 JSONL 审计日志（管理员每次写操作追加一行），用于合规留痕 |

- 默认端口 `8080`；Admin UI：`http://localhost:8080/admin`；指标：`/metrics`（免鉴权）。
- 健康检查 `/health` `/healthz` `/readyz` `/deep-health` `/metrics` 均免鉴权；其余 `/v1/*` 需 Bearer。

## 安全态势（已硬化项 / 企业基线）

- **API Key 存储**：HMAC-SHA256 + 随机 salt，内存中不留明文（`auth_key.rs`）。
- **鉴权**：Bearer → HMAC 常时比较（`subtle`）→ 注入 `TenantContext`（`middleware/auth.rs`）。
- **RBAC**：`require_role` 区分 `admin` / `developer`；写操作（增删 provider/key、改配置、重置账单）需 `admin`（`middleware/rbac.rs`、`admin.rs`）。
- **限流**：每租户令牌桶 + `Retry-After`（`middleware/rate_limit.rs`）。
- **配额**：每租户 RPM/RPD/TPM/TPD（`middleware/quota_middleware.rs`、`metrics/quota.rs`）。
- **租户隔离**：计量/配额/成本均按 `tenant_id` 分桶。
- **审计**：管理员写操作 JSONL 留痕（`audit.rs` + `audit_path`）。
- **持久化（部分）**：计量事件 JSONL 落盘 + 启动回放，重启不再丢账单数据（`persistence.rs`，#6）。**Key Store / 配额仍内存**；计量事件**未签名**（落盘文件可被篡改，后续随 DB 迁移一起补）。
- **容器**：多阶段构建，以非 root（`USER 1000:1000`）运行（`Dockerfile`）。
- **TLS**：上游用 `rustls-tls`（无 OpenSSL 依赖）。
- **韧性**：熔断器 + 指数退避 + 跨 Provider 降级。

## 部署与运维

```bash
# Docker（多阶段、非 root；产物 /usr/local/bin/gateway-server）
docker build -t ai-gateway .
docker run -p 8080:8080 -v "$PWD/config.toml:/app/config.toml:ro" ai-gateway

# Kubernetes —— 原始清单（deploy/ 含 namespace/deployment/service/hpa/configmap/secret/servicemonitor/prometheusrules/grafana-dashboard）
kubectl apply -f deploy/

# Kubernetes —— Helm（deploy/helm/）
helm install ai-gateway deploy/helm -f deploy/helm/values.yaml
```

CI（`.github/workflows/ci.yml`）5 个 job：`lint`（fmt+clippy+doc）→ `test`（ubuntu+macos）→ `security`（cargo-audit）→ `build`（release）→ `coverage`（llvm-cov，`continue-on-error`）。**PR 必须全绿方可合入。**

## 测试

集成测试位于 `crates/gateway-server/tests/`，共享夹具在 `common/mod.rs`：

| 文件 | 覆盖 |
|---|---|
| `mvp0_smoke.rs` | 健康检查 / 鉴权 / 路由冒烟 |
| `mvp1_tenancy.rs` | 多租户 + RBAC + Admin API |
| `mvp2_metering.rs` | 计量 / 配额 |
| `mvp3_metrics.rs` | Prometheus 指标 |
| `contract_test.rs` | OpenAI 兼容契约 |
| `concurrency_test.rs` | 并发 / 限流 |

跑单个文件：`cargo test --package gateway-server --test mvp2_metering`。

## 协作约定（多 Agent）

- **唯一通信通道是 `BOARD.md`**；开工前读、完工后更新。详细规则见 `CONVENTIONS.md`。
- 改动公共 crate（`gateway-core` / `gateway-server` 的类型/路由/中间件）或引入新依赖前，先在 `BOARD.md` 声明意图。
- 设计/规划文档在 `docs/superpowers/{specs,plans}/`，企业总体设计见 `specs/2026-07-02-enterprise-ai-gateway-design.md`。

---

## 已知问题清单（2026-07-05 核实，含 #6 合入后状态）

> 逐项对照代码核实。已完成项注明实现位置，未完成项注明现状。

### 当前阻断 CI（优先修，否则 PR 无法合入）

- [ ] **`cargo clippy --deny warnings` 失败** → `gateway-core` 报 6 个 error（如 `field_reassign_with_default`、`manual_div_ceil`、`new_without_default`）。CI 的 lint job 以 `-D warnings` 视为 error，**当前 master CI 为红**（#6 未修）。
- [ ] **`cargo check` 大量 warning** → 多为 unused import / never read 字段（流式中间结构体）。可用 `cargo fix` 收一部分。

### 安全/正确性

- [ ] `is_retryable` 用 `msg.contains("400")` 字符串匹配判断状态码（`retry.rs`）→ 应改为结构化状态码字段
- [ ] 计量事件落盘**未签名**（`persistence.rs`）：有文件写权限者可伪造/篡改账单记录 → 后续补逐事件 HMAC

### 企业级缺口（架构性）

- [ ] **持久化仍不全**：计量事件已落盘（#6），但 **Key Store / 配额 / 成本汇总仍内存**，重启需从 config 重建
- [ ] OTel 链路追踪未实现（spec 5.2）→ 仅有 Prometheus 指标 + JSON 日志 + 审计
- [ ] Provider 优先级 / 权重路由 / 动态模型发现未实现（MVP 4 范围）→ `build_fallback_chain` 仅"内置优先"排序

### 已完成（保留作上下文）

- [x] 计量事件 `key_id` 归因 → 已用 `TenantContext.key_id`，不再是 `"_from_routes_"` 占位（`routes.rs`，#6）
- [x] 计量事件持久化 → JSONL 落盘 + 启动回放（`persistence.rs`，#6；Key Store/配额仍待办）
- [x] API Key 明文存储 → HMAC-SHA256 + 随机 salt（`auth_key.rs`）
- [x] 限流中间件未生效 → 已接入路由，每租户令牌桶 + `Retry-After`（`rate_limit.rs`、`routes.rs`）
- [x] extra_headers 未透传 → 各 provider `build_headers()` 均已透传
- [x] 环境变量前缀 `AI_GATERARY` → 已修正为 `AI_GATEWAY`（`config.rs`）
- [x] Admin UI 认证 → Admin API 加 RBAC（`require_role` + `AuthKey` 扩展，`admin.rs`）
