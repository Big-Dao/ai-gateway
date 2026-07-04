# 多 Agent 协作约定

> 每个 panel 的 Claude Code 开始工作前都应阅读此文件。
> 面板间唯一的通信通道是 `BOARD.md` — 认真读、勤更新。

---

## 🏁 开工前必做

### 1. 读 BOARD.md
了解谁在做什么、文件归属、有无冲突。

### 2. 检查文件归属
确认你要改的代码**不在别人的 worktree 修改范围内**。

> MVP 0–6 的并行阶段已结束（全部合入 `master`，commit `b2e982a`），下列 worktree 已归档。
> 若开启新的并行 MVP，按同样约定各自建 `worktrees/mvpN-<topic>`。
> ~~~
> - MVP 2 (Metering) → `worktrees/mvp2-metering`
> - MVP 3 (Observability) → `worktrees/mvp3-observability`
> - MVP 4 (Providers) → `worktrees/mvp4-providers`
> ~~~

### 3. 在每个 worktree 内分别 git 操作
```bash
# 各自 worktree 里
git checkout mvpN-<topic>   # 例如 mvp2-metering（已归档）
# ... 开发 ...
git add -A && git commit -m "..."
```

---

## 📂 文件操作规则

### ✅ 可以做的
- 在自己的 worktree 里新增/修改文件
- 读其他 worktree 的代码（了解上下文）
- 更新 BOARD.md 状态
- 修改自己 MVP 范围内的模块

### ⚠️ 需要协商的
- 改动 `gateway-core` 的类型/接口 → 先在 BOARD.md 声明确认无冲突
- 改动 `gateway-server` 的路由/中间件 → 同上
- 引入新的依赖 → 先在 BOARD.md 声明

### ❌ 不要做的
- 直接 modify master 分支上的代码
- 同时改同一个文件（Git 合并会冲突）
- 重构/重命名别人写的代码
- 删除别人 worktree 的文件
- 在多 worktree 之间复制粘贴代码

---

## 🔄 完工流程

1. `cargo check --workspace` 编译通过
2. `cargo test --workspace` 全部通过
3. 端到端 curl 验证（参照 CLAUDE.md 的铁律）
4. 更新 BOARD.md：标记任务完成
5. push 到对应远程分支
6. 由 Agent-1 (主面板) 负责发起 PR 和合入

---

## 📢 如何在 BOARD.md 通信

用 Agent 标签开头留言：

```markdown
**[Agent-3]**: MVP 3 Prometheus endpoint 已就绪，等 Agent-2 的 metrics 统一接口合入后再合并
```

这样别人一眼知道是谁在说什么。

---

## 🚨 遇到冲突

1. 立刻在 BOARD.md「阻塞/求助」栏写明问题
2. 停止相关修改，避免冲突扩大
3. 等其他 agent 回复或线下协调

---

*最后更新: 2026-07-04 — 标注 MVP 0–6 并行阶段已合入 master（`b2e982a`），worktree 引用归档。*
