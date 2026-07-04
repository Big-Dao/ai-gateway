---
name: verify
description: Enforce the CLAUDE.md 铁律 before claiming any Rust change done — runs cargo check + cargo test + starts the server and curls /health and /v1/models end-to-end, then writes target/.verified (a 15-min marker the pre-commit hook requires for any commit touching .rs files). ALWAYS invoke before reporting a task as 完成/已修复, and before committing .rs changes.
---

# verify — 铁律落地

落实 CLAUDE.md「铁律」：声明任何 Rust 改动"完成 / 已修复"之前，**必须**先跑这个技能。
pre-commit 钩子 (`.claude/hooks/verify-gate.sh`) 会强制要求 `target/.verified` 新鲜，否则拦下任何含 `.rs` 的提交。

## 怎么跑

```bash
bash .claude/skills/verify/verify.sh
```

脚本依次执行（任一步失败即非 0 退出、**不写标记**）：

1. `cargo check --workspace` — 编译必须通过
2. `cargo test --workspace` — 全部测试必须 `ok`
3. 启动服务 → `curl /health` 必须 `200` → `curl /v1/models` 冒烟 → 关停
   - fresh worktree 没有 `config.toml` 时，脚本会自动从 `config.example.toml` 复制一份（gitignored，不会进 git）

三步全过 → 写入 `target/.verified`（15 分钟内有效）。

## 失败时怎么办

- **不要**对用户说"已完成 / 已修复"。
- 读脚本输出定位是哪一步失败，修复后重跑，直到三步全过。
- E2E 因端口占用失败：先 `pkill -9 -f gateway-server`，再重跑。
- 服务起不来：看 `${TMPDIR:-/tmp}/gateway-verify.log`。

## 边界

- 仅改文档 / 配置（无 `.rs`）时，pre-commit 钩子不会拦截；可酌情跳过 E2E，但仍建议至少跑 `cargo check`。
- 标记过期（>15 min）后再次提交 `.rs` 会被拦截 —— 重跑本技能即可刷新。
