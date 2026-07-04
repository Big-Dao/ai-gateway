#!/usr/bin/env bash
# PreToolUse hook (matched to `git commit` via the hook `if` filter).
# Enforces the CLAUDE.md「铁律」: a commit that stages any .rs file is blocked
# unless target/.verified is fresh (<=15 min). That marker is written only by
# the `verify` skill (cargo check + cargo test + live curl E2E).
#
# Exit 0 = allow the commit; Exit 2 = block and feed the reason back to Claude.
set -uo pipefail

input="$(cat)"

# Defense-in-depth: confirm this really is a `git commit` even if the `if`
# filter is unavailable in some runtime. (jq is not assumed present.)
cmd=""
if command -v python3 >/dev/null 2>&1; then
  cmd="$(printf '%s' "$input" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("command",""))' 2>/dev/null || true)"
else
  cmd="$(printf '%s' "$input" | sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
fi
case "$cmd" in
  *"git commit"*) ;;
  *) exit 0 ;;
esac

# Doc-only / config-only commits (no staged .rs) are not gated.
git diff --cached --name-only 2>/dev/null | grep -q '\.rs$' || exit 0

# Fresh verify marker?
if test -f target/.verified && find target/.verified -mmin -15 2>/dev/null | grep -q .; then
  exit 0
fi

cat >&2 <<'EOF'
铁律未满足：本次提交包含 .rs 改动，但 target/.verified 缺失或已过期 (>15 min)。
请先运行 /verify（cargo check + cargo test + 启动服务 curl E2E），
成功后会刷新 target/.verified，随后重新提交即可。
EOF
exit 2
