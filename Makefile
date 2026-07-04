# AI Gateway — common entry points for humans and Claude Code.
# `make verify` satisfies the CLAUDE.md「铁律」(cargo check + test + live E2E)
# and writes target/.verified, which the pre-commit hook requires for .rs commits.

BIN  := gateway-server
PORT ?= 8080
# Test API key for the smoke target. Defaults to the example-config key so
# `make smoke` works out of the box; override with a real key via env/CLI
# (e.g. `GATEWAY_TEST_KEY=sk-... make smoke`) — never commit a real key.
GATEWAY_TEST_KEY ?= my-secret-key

.PHONY: help check test fmt lint audit run smoke verify coverage clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

check: ## cargo check --workspace
	cargo check --workspace

test: ## cargo test --workspace
	cargo test --workspace

fmt: ## cargo fmt --all
	cargo fmt --all

lint: ## cargo clippy, warnings as errors (matches CI)
	cargo clippy --workspace --all-targets -- -D warnings

audit: ## cargo audit (security advisories)
	cargo audit

run: ## Run the gateway server in the foreground
	cargo run --bin $(BIN)

smoke: ## Start server, curl /health + /v1/models, then stop
	@pkill -9 -f $(BIN) >/dev/null 2>&1 || true; \
	[ -f config.toml ] || cp config.example.toml config.toml; \
	nohup cargo run --bin $(BIN) > /tmp/gateway.log 2>&1 & echo $$! > /tmp/gateway.pid; \
	code=000; \
	for i in $$(seq 1 60); do \
	  code=$$(curl -s -o /dev/null -w '%{http_code}' http://localhost:$(PORT)/health 2>/dev/null || echo 000); \
	  [ "$$code" = "200" ] && break; sleep 0.5; \
	done; \
	echo "/health -> $$code"; \
	curl -s -o /dev/null -w "/v1/models -> %{http_code}\n" \
	  -H 'Authorization: Bearer $(GATEWAY_TEST_KEY)' http://localhost:$(PORT)/v1/models || true; \
	pkill -9 -f $(BIN) >/dev/null 2>&1 || true; \
	[ "$$code" = "200" ]

verify: ## CLAUDE.md 铁律 — check + test + E2E; writes target/.verified
	@bash .claude/skills/verify/verify.sh

coverage: ## cargo llvm-cov HTML report
	cargo llvm-cov --workspace --all-targets --html

clean: ## cargo clean
	cargo clean
