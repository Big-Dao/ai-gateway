# .claude/ — 项目级 Claude Code harness

本目录是让 Claude Code 在本项目里**长时间自主工作 + 交付达企业标准**的配置层。
所有内容随仓库提交，团队成员拉取后即生效（个人覆盖放 `.claude/settings.local.json`，已 gitignore）。

## 文件清单

| 路径 | 作用 |
|---|---|
| `settings.json` | 权限白名单（allow/deny）+ 两个 hook。**自主工作的地基**：白名单让 cargo/git/curl 不再逐条打断；deny 守住密钥与锁文件。 |
| `hooks/rustfmt-file.sh` | PostToolUse（Write\|Edit）：编辑 `.rs` 后自动 `rustfmt`，CI 的 `cargo fmt --check` 永远绿。 |
| `hooks/verify-gate.sh` | PreToolUse（`git commit`）：提交含 `.rs` 改动时，强制要求 `target/.verified` 新鲜（≤15min），否则拦下并把原因回灌给 Claude —— 把 CLAUDE.md「铁律」机器化。 |
| `skills/verify/SKILL.md` + `verify.sh` | `/verify` 技能：一键跑 cargo check + cargo test + 启服务 curl E2E，全过才写 `target/.verified`。 |

## 日常使用

```bash
# 改完 Rust，声明"完成"前 / 提交前：
bash .claude/skills/verify/verify.sh
# 三步全过 → 写入 target/.verified → 之后 git commit 含 .rs 才会被放行
```

- 权限提示仍频繁？检查 `/permissions`，或在 `.claude/settings.local.json` 补个人 allow。
- 铁律闸误拦（例如只改了文档却被拦）？只有暂存了 `.rs` 才会触发；纯文档提交不受影响。
- 权限规则语法以当前 Claude Code 版本为准（本文件用 `Bash(cmd:*)` 前缀写法）。
