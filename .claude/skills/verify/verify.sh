#!/usr/bin/env bash
# verify.sh — 落实 CLAUDE.md「铁律」的端到端验证。
#
# 三步：cargo check → cargo test → 启动服务 curl 冒烟。
# 全部通过才写入 target/.verified（15 分钟内有效）；pre-commit 钩子
# (verify-gate.sh) 据此放行任何含 .rs 的提交。任一步失败 → 非 0 退出，不写标记。
set -uo pipefail

REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO"

LOG="${TMPDIR:-/tmp}/gateway-verify.log"
MARKER="target/.verified"
PORT=8080
# Test key defaults to the example-config key (my-secret-key) so E2E works
# out of the box; override with GATEWAY_TEST_KEY=<real-key> for other setups.
TEST_API_KEY="${GATEWAY_TEST_KEY:-my-secret-key}"

ok()   { printf '  \xE2\x9C\x93 %s\n' "$1"; }
die()  { printf '  \xE2\x9C\x97 %s\n' "$1" >&2; cleanup; exit 1; }

cleanup() {
  pkill -9 -f gateway-server >/dev/null 2>&1 || true
}

echo "▶ 1/3  cargo check --workspace"
cargo check --workspace 2>&1 | tail -5
[ "${PIPESTATUS[0]}" -eq 0 ] || die "cargo check 失败 (exit ${PIPESTATUS[0]})"
ok "cargo check 通过"

echo "▶ 2/3  cargo test --workspace"
cargo test --workspace 2>&1 | tail -25
[ "${PIPESTATUS[0]}" -eq 0 ] || die "cargo test 失败 (exit ${PIPESTATUS[0]})"
ok "cargo test 全部通过"

echo "▶ 3/3  E2E 冒烟（启动服务 → curl /health → 关停）"
# config.toml 是 gitignored；fresh worktree 里可能缺失，从 example 复制一份（不会进 git）。
if [ ! -f config.toml ]; then
  cp config.example.toml config.toml 2>/dev/null && echo "  (从 config.example.toml 生成 config.toml)"
fi

cleanup
nohup cargo run --bin gateway-server > "$LOG" 2>&1 &
SRV=$!

code=000
for _ in $(seq 1 60); do
  code="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:${PORT}/health" 2>/dev/null || echo 000)"
  [ "$code" = "200" ] && break
  sleep 0.5
done
[ "$code" = "200" ] || die "服务未就绪 (/health=$code)；日志见 $LOG"
ok "/health → 200"

# 受保护端点冒烟（example config 的 auth.api_keys 含 my-secret-key；可用 GATEWAY_TEST_KEY 覆盖）
curl -s -o /dev/null \
  -w "  /v1/models (Bearer ${TEST_API_KEY}) → %{http_code}\n" \
  -H "Authorization: Bearer ${TEST_API_KEY}" "http://localhost:${PORT}/v1/models" || true

cleanup

mkdir -p target
date -u +'%Y-%m-%dT%H:%M:%SZ' > "$MARKER"
echo "✅ 铁律全部通过 — 已刷新 $MARKER（15 分钟内有效）"
