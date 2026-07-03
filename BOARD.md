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
| MVP 0 | 基础 — CI、认证、限流、中间件 | 🟦 Dispatcher | ✅ done |
| MVP 1 | 多租户 + RBAC + Admin API | 🟦 Dispatcher | ✅ done |
| **MVP 2** | Metering — 用量计费计量 | 🟧 Meter | 🟢 开发中 |
| **MVP 3** | Observability — 可观测性 | 🟪 Tracer | 🟡 _(待 Tracer 确认)_ |
| **MVP 4** | Providers — 新提供商扩展 | 🟨 Bridge | 🟡 _(待 Bridge 确认)_ |
| **MVP 2** | Metering — 用量计费计量 | 🟩 **Sentinel** | ✅ 完成，⏳ 等合并 |
| **MVP 5** | OTel + 审计日志 | 🟩 **Sentinel** | 🟢 地基 done |
| **MVP 6** | 成本计费 | ⬜ _(空闲)_ | 📋 可认领 |

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

### MVP 2 — Metering (用量计费)

- [ ] 按租户记录 token 消耗量
- [ ] 按请求计费（ configurable 费率表 ）
- [ ] 用量查询 API (`/api/admin/usage/<tenant>`)
- [ ] 配额超限自动拒绝
- [ ] Admin UI: 用量仪表盘 + 费率配置页

### MVP 3 — Observability (可观测性)

- [ ] Prometheus 指标导出 (`/metrics`)
- [ ] 请求延迟分桶 histogram
- [ ] 上游提供商错误率统计
- [ ] 健康检查增强 (deep-health 上报各上游状态)
- [ ] 结构化 JSON 日志输出
- [ ] 可选: OpenTelemetry trace 导出

### MVP 4 — Providers (新提供商扩展)

- [ ] Mistral 适配器
- [ ] Groq / Together / Fireworks 等 OpenAI 兼容提供商（验证格式兼容）
- [ ] 提供商自动发现 / 模型列表动态获取
- [ ] 提供商优先级 / 权重路由
- [ ] 提供商级别熔断器配置
- [ ] Admin UI: 提供商发现 + 模型同步按钮

---

## ⚠️ 阻塞 / 求助

| 问题 | 求助者 | 谁能帮 | 状态 |
|------|-------|-------|------|
| — | — | — | — |

---

## 🔄 合并流程

1. 各自 worktree 开发完成 → push 到远程分支 `mvp2-metering` / `mvp3-observability` / `mvp4-providers`
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

*最后更新: 2026-07-03 by Agent-2 (longcat panel)*
