# AI Gateway — 实施 Plans

> 由 spec `docs/superpowers/specs/2026-07-02-enterprise-ai-gateway-design.md` 展开

## 文件清单

| 文件 | MVP | 粒度 | 优先级 |
|------|-----|------|--------|
| `2026-07-02-mvp0-security-hardening.md` | 安全闭坑 | bite-sized TDD (993 行) | **P0** |
| `2026-07-02-mvp1-tenancy-rbac.md` | 多租户 + RBAC | task + 接口契约 (258 行) | **P0** |
| `2026-07-02-mvp2-metering-quota.md` | 计量 + 配额 | task + 接口契约 (93 行) | **P1** |
| `2026-07-02-mvp3-observability.md` | 可观测 | roadmap stub | P1 |
| `2026-07-02-mvp4-provider-expansion.md` | 多 Provider | roadmap stub | P1 |
| `2026-07-02-mvp5-otel-audit.md` | OTel+审计 | roadmap stub | P2 |
| `2026-07-02-mvp6-cost-billing.md` | 计费 | roadmap stub | P3 |

## 执行依赖关系

```
MVP 0 (安全闭坑) [P0]
  └─▶ MVP 1 (多租户) [P0]
         ├─▶ MVP 2 (计量配额) [P1]
         ├─▶ MVP 3 (可观测)   [P1] ← 并行
         └─▶ MVP 4 (多Provider) [P1]
                                  │
                                  ▼
                          MVP 5 (OTel+审计) [P2]
                                  │
                                  ▼
                          MVP 6 (计费) [P3]
```

## 执行模式

1. **Subagent-Driven (推荐)** — fresh subagent 按 task 执行；每个 task 完成两阶段 review
2. **Inline** — 在 session 内 batch execution with checkpoints

MVP 0 (bite-sized TDD) 两种模式均适用。MVP 6 roadmap stub 建议 subagent 独立派发。
