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

## P1 扩展

| 路径 | 作用 |
|---|---|
| `agents/security-reviewer.md` | 鉴权 / 密钥 / 限流 / RBAC / 租户隔离 / 计费归因 / 密钥泄露 子代理。改了 auth·billing·rate-limit 后、PR 前跑。 |
| `agents/rust-reviewer.md` | await 持锁 / unwrap panic / 状态码字符串匹配 / dead code / 错误传播 / 内存态并发 子代理。改了并发·状态·错误路径后跑。 |
| `../.mcp.json` | 团队共享 MCP：**context7**（Axum / reqwest / tracing 等版本敏感文档），**版本锁定** `@upstash/context7-mcp@3.2.2`（避免每次启动 `npx -y` 拉取未审核的最新版，降低供应链风险；升级时人工改版本号并审计）。GitHub 操作走已认证的 `gh` CLI（已在白名单）。 |
| `../Makefile` | 人/agent 统一入口：`make verify`（= 铁律）、`make smoke`、`make check/test/lint/audit/coverage/run`。`make verify` 委托给 `skills/verify/verify.sh`，单一真源。 |

子代理用法（主面板可并行调度）：
```bash
# 在 worktree 里
claude /agents      # 查看可用子代理
# 或主 agent 自动在 PR 前并行派出 security-reviewer + rust-reviewer
```
