# MVP 6 — 成本计费 Implementation Plan (Detailed)

> **For agentic workers:** REQUIRED SUB-SKILL: Use the project's `/superpowers:executing-plans`
> skill to execute each step sequentially. After each step, run
> `cargo check && cargo test --workspace` — fix any breakage before moving on.
> This plan supersedes the roadmap stub `2026-07-02-mvp6-cost-billing.md`.

**Goal:** 从"统一费率的简单估价"升级到"按模型精确计费的成本闭环"——
包括 Per-Model 价目表、计量热路径的成本注入、Admin 成本看板、账单周期与超支告警。

**Architecture:** 价目表从 `AppConfig.rate_config` 扩展为 `PricingTable`（按 model 查找
input/output price per 1M tokens）。计量热路径 `record_metering()` 按 model 查找
对应价格注入 `MeteringEvent.estimated_cost_cents`。新增 `/api/admin/costs` 端点聚合
日/周/月 + Top 租户 + Model 维度的成本，Admin UI 新增 `/admin/cost` 页面。
账单周期以固定 window (UTC 自然日/周/月) 截断 `MeteringEvent.timestamp_ms`，
在 server 启动时做一次**内存重置**(清 `MeteringService.events` 与 usage cost)。

**Tech Stack:** 纯 std + 已有依赖（无需新增 crate）；
时间窗口用 `SystemTime → UNIX_EPOCH → Duration` 计算自然日偏移；
Admin UI 复用已有 `admin.css` 与 `admin.js` 的 `api()` helper。

**Key Insight:** 现有的 `RateCard.estimate_cost(usage)` 使用 ceil-division
（不足 1M 按 1M 计费）是 MVP 2 的简化模型。MVP 6 切换到 floor + 小数精度
`(tokens * price) / 1_000_000`，单位是 cents 的 `f64`——与现有
`MeteringEvent.estimated_cost_cents: f64` 类型完全兼容，不破坏下游。

---

## Step 1 — 扩展 `RateCard` 为 `PricingTable`

- [ ] 1.1 在 `crates/gateway-core/src/metering.rs` 定义新结构体:

  ```rust
  /// Per-model pricing entry (input = prompt, output = completion).
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  pub struct ModelRate {
      pub input_per_1m: f64,   // cost per 1M prompt tokens (cents)
      pub output_per_1m: f64,  // cost per 1M completion tokens (cents)
  }

  /// Platform-wide pricing table. Look up by model; fallback to `default`.
  #[derive(Debug, Clone, Serialize, Default)]
  pub struct PricingTable {
      pub default: Option<ModelRate>,
      pub models: HashMap<String, ModelRate>,
  }
  ```

- [ ] 1.2 为 `PricingTable` 实现 `estimate_cost(model: &str, usage: &Usage) -> f64`：
  用 `models.get(model).or(self.default.as_ref())` 查找，命中則
  `(prompt * input_per_1m + output * output_per_1m) / 1_000_000`；
  未命中回退到 `0.0`（免费），打 `tracing::debug`。

- [ ] 1.3 在 `impl Default for RateCard` 之上再保留 `RateCard` 结构体不删，
  但仅在 config 反序列化兼容路径使用；主路径切换到 `PricingTable`。
  在 `AppConfig` 中将 `rate_config: Option<RateCard>` 改为
  `pricing: PricingTable`（加上 `#[serde(default)]`，无配置时全免费）。

- [ ] 1.4 在 `config.example.toml` 新增示例段落：

  ```toml
  [pricing]
  default = { input_per_1m = 5.0, output_per_1m = 15.0 }

  [pricing.models.gpt-4o]
  input_per_1m = 2.5
  output_per_1m = 10.0

  [pricing.models.claude-sonnet-4-6]
  input_per_1m = 3.0
  output_per_1m = 15.0
  ```

- [ ] 1.5 写单元测试覆盖：(a) 命中学价；(b) fallback 到 default；(c) 无配置免费；
  (d) 小数精度（如 500_000 tokens × 2.5/1M = 1.25 cents）。

---

## Step 2 — 计量热路径按 model 注入 `estimated_cost_cents`

- [ ] 2.1 改 `routes.rs::record_metering`：
  把 `config.rate_config.as_ref().map(|card| card.estimate_cost(usage))` 替换为
  `config.pricing.estimate_cost(model, usage)`。
  `model` 参数已在函数签名中（`&str`）。

- [ ] 2.2 补 `key_id`：当前硬编码 `"_from_routes_"`，改为从 `TenantContext`
  (`Extension<TenantContext>`) 取真实 `key_id`——传参已有 `tenant_ctx`，
  直接在 `chat_completions` 签名里增加 `Extension(tenant_ctx)` 并下传。

- [ ] 2.3 在 `MeteringService::record()` 中校验 `estimated_cost_cents >= 0.0`，
  若为 NaN / Inf 则 clamp 到 `0.0` 并 `tracing::warn`——避免上游价格配置错误时
  污染聚合。

- [ ] 2.4 更新 `crates/gateway-server/src/metrics/metering.rs` 原有的
  `test_record_and_query_tenant_usage` 测试，assert `total_cost_cents`
  与新公式一致（手动计算期望值）。

---

## Step 3 — 成本聚合 API `/api/admin/costs`

- [ ] 3.1 在 `crates/gateway-core/src/metering.rs` 新增响应结构体：

  ```rust
  #[derive(Serialize)]
  pub struct CostBreakdown {
      pub tenant_id: String,
      pub total_cost_cents: f64,
      pub per_model: Vec<ModelCost>,
  }

  #[derive(Serialize)]
  pub struct ModelCost {
      pub model: String,
      pub cost_cents: f64,
      pub prompt_tokens: u64,
      pub completion_tokens: u64,
  }

  #[derive(Serialize)]
  pub struct CostSummary {
      pub window: String,          // "24h" | "7d" | "30d"
      pub total_cost_cents: f64,
      pub top_tenants: Vec<CostBreakdown>,
      pub per_model: Vec<ModelCost>,
      pub tenant_filter: Option<String>,
  }
  ```

- [ ] 3.2 在 `MeteringService` 新增方法
  `cost_summary(window_ms: u64, tenant_filter: Option<&str>) -> CostSummary`：
  遍历 `self.events` 中 `timestamp_ms > now - window_ms` 的事件，
  按 tenant/model 分别累加 cost。

- [ ] 3.3 在 `admin.rs::admin_router()` 注册路由：
  `GET /api/admin/costs` → `get_costs` handler；
  读取 query param `?window=24h|7d|30d`（默认 `24h`）与
  `?tenant=<id>` (admin only)。

- [ ] 3.4 实现 `get_costs` handler：RBAC 沿用已有
  `require_role` (developer 看自己的 tenant，admin 看全部)。
  返回 `Json<CostSummary>`。

- [ ] 3.5 写集成测试（在 `metering.rs` 测试模块）：
  喂入 window 内/外事件，断言 window 外的不计入。

---

## Step 4 — Admin UI 成本看板 (`/admin/cost`)

- [ ] 4.1 在 `crates/gateway-server/src/static/index.html` 中新增：
  - Sidebar `<li class="nav-item" data-page="cost">` 标签 "费用中心"
  - `<section class="page" id="page-cost">` 页面骨架:
    - 顶部 3 个 stat-card：总成本 / 本月预估 / 活跃模型数
    - 窗口切换按钮 (24h / 7d / 30d)，默认 24h
    - Top tenants 横向 bar chart (复用 `renderModelChart` 模式)
    - Model breakdown 表格 (model / cost / prompt_tokens / completion_tokens)

- [ ] 4.2 在 `admin.rs` `loadPageData` switch 中
  `case 'cost': loadCostDashboard(); break;`。

- [ ] 4.3 在 `admin.js` 实现 `loadCostDashboard()`：
  `const data = await api('/api/admin/costs?window=' + currentWindow);`
  渲染 stat-card + chart + table。窗口切换按钮注册 click listener
  重新 fetch。

- [ ] 4.4 在 `admin.css` 新增 `.cost-window-bar` / `.window-btn` 样式，
  保持与已有 `chart-bar` / `btn` 视觉一致。

---

## Step 5 — 账单周期重置

- [ ] 5.1 在 `MeteringService` 新增
  `async fn reset_billing_window(&self, window_start_ms: u64)`：
  清空 `events`，并把所有 `TenantUsage.total_cost_cents`、
  `ModelUsage.cost_cents` 归零（请求数、token 数**不归零**——它们是累计值）。

- [ ] 5.2 在 `static_files.rs` / `state.rs` 启动时调用
  `MeteringService.reset_billing_window(today_utc_start_ms())`：
  防止旧事件跨周期累积。
  `today_utc_start_ms()` 用 `SystemTime → Duration → 除以 86_400_000 再乘回` 计算。

- [ ] 5.3 在 `admin.rs::admin_router()` 注册
  `POST /api/admin/billing/reset` + handler `post_billing_reset`，
  RBAC 限 admin 调用，触发 service-level reset 并写 audit 日志。

- [ ] 5.4 在 `usage` 页面的费率卡 block 下新增"账单周期"信息块：
  展示当前 window 起始时间 / 剩余天数 / 手动重置按钮（admin 可见）。

---

## Step 6 — 成本阈值告警（可选，可单独 defer）

- [ ] 6.1 在 `tenant.rs::TenantConfig` 新增字段
  `pub cost_alert_threshold_cents: Option<f64>`（serde default `None`）。

- [ ] 6.2 在 `MeteringService::record()` 写入后检查：
  若 `TenantUsage.total_cost_cents` 刚越过 `threshold`（上一次 < threshold），
  则 `tracing::warn!(tenant, cost, threshold, "cost alert triggered")`，
  并在 `TenantUsage` 上记录 `alert_triggered: bool` 防止重复。

- [ ] 6.3 在 `admin.rs::admin_router()` 注册
  `PUT /api/admin/config/quota/{tenant_id}` 已有——复用，新增可选字段
  `cost_alert_threshold_cents: Option<f64>`。

- [ ] 6.4 写单元测试：(a) 越过阈值时触发 warn；(b) 不再触发第二次。

---

## Files to create / modify

- `crates/gateway-core/src/metering.rs` — **modify**: 新增 `ModelRate` / `PricingTable` /
  `CostSummary` 等结构体 + 实现 `estimate_cost` / `cost_summary` / `reset_billing_window`。
- `crates/gateway-core/src/config.rs` — **modify**: `AppConfig.rate_config` 改为
  `pricing: PricingTable`（保留反序列化兼容）。
- `crates/gateway-core/src/tenant.rs` — **modify**: `TenantConfig` 新增
  `cost_alert_threshold_cents: Option<f64>`。
- `crates/gateway-server/src/routes.rs` — **modify**: `record_metering` 改用
  `pricing.estimate_cost(model, usage)`；补真实 `key_id`。
- `crates/gateway-server/src/admin.rs` — **modify**: 新增
  `get_costs` / `post_billing_reset` handler + 路由注册。
- `crates/gateway-server/src/state.rs` — **modify**: 启动时
  `reset_billing_window` 调用。
- `crates/gateway-server/src/static/index.html` — **modify**: 新增 sidebar navitem
  与 `page-cost` section。
- `crates/gateway-server/src/static/admin.js` — **modify**: 新增
  `loadCostDashboard()`。
- `crates/gateway-server/src/static/admin.css` — **modify**: 新增 `.cost-window-bar`
  样式。
- `config.example.toml` — **modify**: 新增 `[pricing]` section 示例。

## Verification

```bash
# 1. Build & test
cargo check --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20

# 2. End-to-end smoke
nohup cargo run --bin gateway-server > /tmp/gateway.log 2>&1 &
sleep 8

# Health
curl -s -o /dev/null -w "health=%{http_code}\n" http://localhost:8080/health

# Rate-card still readable
curl -s -w "\n" http://localhost:8080/api/admin/config/rate-card

# Cost summary empty seed (no traffic yet → total 0)
curl -s -w "\n" -H "Authorization: Bearer test-key" \
  http://localhost:8080/api/admin/costs?window=24h | head -c 400

# Send one real request → should incur cost if pricing configured
curl -s -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer test-key" \
  -H "Content-Type: application/json" \
  -d '{"model":"echo","messages":[{"role":"user","content":"hi"}],"max_tokens":10}'

# Costs now non-zero
curl -s -w "\n" -H "Authorization: Bearer test-key" \
  http://localhost:8080/api/admin/costs?window=24h

# Billing reset (admin only)
curl -s -w "\n" -X POST -H "Authorization: Bearer admin-key" \
  http://localhost:8080/api/admin/billing/reset

# Cleanup
pkill -9 -f gateway-server
```

## Non-goals (future MVPs)

- 多币种切换 (USD/CNY 汇率)
- 实时流式成本推送 (SSE)
- 发票/账单 PDF 导出
- Provider 侧实际花费 reconciliation（与 OpenAI Stripe 报表对账）
- 真正的 tiered / volume 阶梯定价（Step 1.1 结构已预留扩展位）
