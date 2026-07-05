# 🧑‍💻 多 Agent 协作看板

> 此文件是所有 Claude Code panel 的**唯一通信通道**。开工前、完工后都来更新。

---

## 📋 项目概览

| 项目 | 说明 |
|------|------|
| **项目名** | AI Gateway (Rust) |
| **描述** | 多 LLM 提供商统一 OpenAI 兼容 API 网关 |
| **技术栈** | Rust / Axum / Tokio |
| **主目录** | `/home/andy/test/ai-gateway` |
| **汇合分支** | `master` |

---

## 🧑‍💺 团队花名册

| 名字 | 角色 | 面板 | 状态 |
|------|------|------|------|
| 🟦 **Dispatcher** | 主控调度，MVP 0-1 done | ai-gateway 主 | 🟡 协调中 |
| 🟩 **Sentinel** 🐍 | 审计 + OTel；MVP 2 完成待合并 | longcat | ⏳ 等 Dispatcher 合并 |
| 🟧 **Meter** | 计量配额 | sub-MVP2 | 🟢 开发中 |
| 🟪 **Tracer** | 可观测性 | sub-MVP3 | 🟡 待确认状态 |
| 🟨 **Bridge** | 多 Provider | sub-MVP4 | 🟡 待确认状态 |
| ⬜ **Ledger** | 成本计费（空闲） | — | 📋 可认领 |

> 状态：🟢 开发中 / 🟡 空闲或等待 / 🔴 阻塞 / ✅ 完成

---

## 🏗️ MVP 路线与归属

| MVP | 描述 | 负责人 | Worktree 路径 | 状态 |
|-----|------|--------|--------------|------|
| MVP 0 | 基础 — CI、认证、限流、中间件 | 🟦 Dispatcher | — | ✅ done（安全清单 4/6 已修，见 CLAUDE.md） |
| MVP 1 | 多租户 + RBAC + Admin API | 🟦 Dispatcher | — | ✅ done |
| MVP 2 | Metering — 用量计费计量 | 🟧 Meter / 🟩 Sentinel | — | ✅ done（已合入 master） |
| MVP 3 | Observability — 可观测性 | 🟪 Tracer | — | ✅ done（Prometheus + JSON 日志 + deep-health + log buffer） |
| MVP 4 | Providers — 新提供商扩展 | 🟨 Bridge | — | 🟠 部分完成（OpenAICompat + 4 内置 done；优先级/权重/动态发现未做） |
| MVP 5 | OTel + 审计日志 | 🟩 Sentinel | — | 🟠 部分完成（审计 done；OTel trace 未实现） |
| MVP 6 | 成本计费 | ⬜ Ledger | — | ✅ done（PricingTable + costs API + billing reset + 阈值告警；`key_id` 接线遗漏见 CLAUDE.md） |

> 2026-07-05 重新核实：MVP 0–6 代码均已在 master（最新 commit `a54c0d5`）。CI lint 项（clippy / check / fmt）实测已全部通过——`CLAUDE.md` 旧清单标记的"阻断 CI"与 `is_retryable` 字符串匹配问题均已不在，文档已更正。遗留 TODO（计量事件未签名、Key Store/配额仍内存、OTel、MVP4 优先级路由）详见 `CLAUDE.md`「已知问题清单」。

---

## 🗂️ 文件归属

| 路径 | 负责人 | 说明 |
|------|--------|------|
| `crates/gateway-core/` | _(公共，改前协商)_ | 核心类型、trait、配置 |
| `crates/gateway-server/` | _(公共，改前协商)_ | HTTP 服务、路由、中间件 |
| `crates/providers/` | MVP 4 负责人 | 提供商适配器 |
| 各自 worktree 内新增文件 | 该 worktree 的 owner | 计量/监控相关新模块 |

---

## 📝 MVP 详细范围

> 2026-07-04 逐项核实代码后勾选。✅ 已实现 / ❌ 未实现。

### MVP 2 — Metering (用量计费) ✅

- [x] 按租户记录 token 消耗量 — `metrics/metering.rs`（`TenantUsage` / `ModelUsage`）
- [x] 按请求计费（configurable 费率表）— `gateway-core/src/metering.rs`（`PricingTable`）
- [x] 用量查询 API — `/api/admin/usage/{tenant_id}`、`/api/admin/usage`
- [x] 配额超限自动拒绝 — `middleware/quota_middleware.rs`
- [x] Admin UI: 用量仪表盘 — `static/index.html`

### MVP 3 — Observability (可观测性) ✅

- [x] Prometheus 指标导出 (`/metrics`) — `metrics/prometheus.rs`
- [x] 请求延迟分桶 histogram — `gateway_request_duration_seconds`（自定义 bucket）
- [x] 上游提供商错误率统计 — `record_request(provider, …, error)` 维度
- [x] 健康检查增强 — `/deep-health` 上报各 provider 熔断状态
- [x] 结构化 JSON 日志输出 — `json_logger.rs` + 内存环形缓冲 `log_buffer.rs`
- [ ] 可选: OpenTelemetry trace 导出 — 未实现（归属 MVP 5）

### MVP 4 — Providers (新提供商扩展) 🟠 部分完成

- [x] Mistral 适配器 — 经 `OpenAICompatProvider` 支持
- [x] Groq / Together / Fireworks 等 OpenAI 兼容提供商 — `OpenAICompatProvider` + `field_overrides`
- [ ] 提供商自动发现 / 模型列表动态获取 — 未实现
- [ ] 提供商优先级 / 权重路由 — 未实现（`build_fallback_chain` 仅"内置优先"排序）
- [ ] 提供商级别熔断器配置 — 熔断已按 provider 隔离，但参数为全局默认，不可按 provider 定制
- [ ] Admin UI: 提供商发现 + 模型同步按钮 — 未实现

---

## ⚠️ 阻塞 / 求助

| 问题 | 求助者 | 谁能帮 | 状态 |
|------|-------|-------|------|
| — | — | — | — |

---

## 🔄 合并流程

> MVP 0–6 的并行开发阶段已结束，全部合入 `master`（当前 HEAD `a54c0d5`）。
> 下方是当时的流程，也适用于未来任何 MVP：把 `mvp2-metering` 等换成新分支名即可。

1. 各自 worktree 开发完成 → push 到远程特性分支（如 `mvpN-<topic>`）
2. 在 BOARD.md 更新状态为"待合并"
3. 由主 panel (Agent-1) 发起 PR → code review → 合入 master
4. 合入后更新 BOARD.md 状态为"已合并"

---

## 📡 协作规则速览

1. **开工前读此文件**，确认没人占你要改的文件
2. **完工后更新此文件**，登记状态
3. **公共文件** (`gateway-core`, `gateway-server`) 改动前先在此声明意图
4. **遇到问题**写在「阻塞/求助」栏
5. **不要同时 push master** — worktree 分支各自 push，合并由一人协调

> 详细规则见 `CONVENTIONS.md`

---

*最后更新: 2026-07-05 — 文档刷新：MVP 状态对齐 master（HEAD `a54c0d5`），逐项核实代码；CI lint 项实测全过，遗留 TODO 见 `CLAUDE.md`「已知问题清单」。*
