# MVP 6 — 成本计费 Implementation Plan (Roadmap Stub, P3)

> **For agentic workers:** 本文件为 roadmap stub，独立迭代。

**Goal:** Provider 价目表加载 + `MeteringEvent.estimated_cost` 计算 + 成本看板

**Prerequisite:** MVP 0-5

---

## Roadmap Tasks

### T1: PricingTable 加载
- config `[pricing.<provider>.<model>]` section
- input_per_million, output_per_million, currency

### T2: MeteringService 计算 estimated_cost
- 在事件落 PVC 时按价目表换算
- 多币种支持

### T3: Admin 成本 API + Dashboard
- `GET /api/admin/costs?...` 多维聚合
- Admin UI 成本看板

### T4: CSV / BI 导出
