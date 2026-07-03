# MVP 4 — 多 Provider 扩展 Implementation Plan (Roadmap Stub)

> **For agentic workers:** 本文件为 roadmap stub，待 MVP 0/1/2 完成后展开为 bite-sized TDD plan。

**Goal:** 通过 `OpenAICompatProvider` 通用适配器新增 7 个 OpenAI 兼容型 Provider + 策略路由

**Prerequisite:** MVP 0 + MVP 1 + MVP 2

---

## Roadmap Tasks (待展开)

### T1: OpenAICompatProvider 抽象 + FieldOverrides 结构
- 复用现有 `OpenAIProvider` 90% 请求路径
- 支持 `extra_headers`（并从 GP-4 修复迁移到它上面）
- FieldOverrides: `emit_reasoning_content`, `chat_template_kwargs`, `stream_field_renames`
- 支持非空 base_url、api_key 可选 (vLLM)

### T2: 首批 7 Provider 接入

| Provider | type alias | 独有 field_overrides |
|---------|------------|---------------------|
| DeepSeek Chat | `openai-compat` | emit_reasoning_content=true |
| DeepSeek Reasoner | `openai-compat` | 同上 + chat_template_kwargs |
| Kimi | `openai-compat` | (基本兼容) |
| 智谱 GLM | `openAI-compat` | (tool_choice 略异) |
| vLLM | `openai-compat` | api_key optional |
| SGLang | `openai-compat` | api_key optional |
| TGI | `openai-compat` | api_key optional |

`register_provider` 增加 `openai-compat` type 分支 → OpenAICompatProvider

### T3: Router 策略升级
- `RouteStrategy::Failover` (已可在 MVP 0 fallback chain 上升级)
- `RouteStrategy::TenantOverride` (tenant→model→provider 覆盖)
- `RouteStrategy::WeightedRoundRobin` (按权重灰度 A/B)
- 优先级：tenant_override → failover → weighted → static

### T4: 测试
集成测试验证 provider 路由选择逻辑；mock 下游验证 extra headers 与 field_overrides 正确透传

### T5: Commit

---

## 关键接口契约（提前锁定）

```rust
// crates/providers/src/openai_compat.rs
pub struct OpenAICompatProvider {
    name: String,
    api_key: Option<String>,
    base_url: String,
    models: Vec<String>,
    field_overrides: FieldOverrides,
    client: reqwest::Client,
}

pub struct FieldOverrides {
    pub emit_reasoning_content: bool,
    pub chat_template_kwargs: Option<serde_json::Value>,
    pub stream_field_renames: HashMap<String, String>,
}
```
