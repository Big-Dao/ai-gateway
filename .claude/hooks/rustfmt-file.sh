#!/usr/bin/env bash
# PostToolUse hook (Write|Edit): auto-format the just-edited file if it is Rust.
# Keeps `cargo fmt --check` green in CI without a manual step.
# Reads the tool-event JSON on stdin (no jq needed; falls back to sed).
set -uo pipefail

input="$(cat)"
f=""
if command -v python3 >/dev/null 2>&1; then
  f="$(printf '%s' "$input" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' 2>/dev/null || true)"
else
  f="$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
fi

[ -n "$f" ] || exit 0
case "$f" in
  *.rs) rustfmt --edition 2021 "$f" >/dev/null 2>&1 || true ;;
esac
exit 0
