# AI Coding Agent Instructions

This is a Rust workspace for an OpenAI-compatible multi-provider AI gateway. Use this file as the fast path, and link out to the fuller project docs instead of copying them into chat.

## Start Here

- Read [BOARD.md](BOARD.md) before code changes. It is the project coordination channel and lists current ownership and known work areas.
- Follow [CONVENTIONS.md](CONVENTIONS.md) for multi-agent workflow and public-module coordination.
- Treat [CLAUDE.md](CLAUDE.md) as the authoritative development rulebook, especially the verification gate and known-issues checklist.
- Use [README.md](README.md) for user-facing API, deployment, and configuration behavior.

## Architecture Map

- [crates/gateway-core](crates/gateway-core) owns shared types, config, auth primitives, tenancy, metering primitives, provider traits, and `GatewayError`.
- [crates/providers](crates/providers) owns provider adapters. Prefer `OpenAICompatProvider` for OpenAI-compatible endpoints unless a provider needs custom request/response translation.
- [crates/gateway-server](crates/gateway-server) owns Axum routing, `AppState`, middleware, retry/fallback, circuit breakers, persistence, metrics, admin API, and embedded static admin UI.
- The request path is auth -> rate limit -> quota -> route -> retry/circuit breaker -> provider -> metering/cost accounting.

## Commands

- `make check` runs `cargo check --workspace`.
- `make test` runs `cargo test --workspace`.
- `make lint` runs clippy with warnings denied, matching CI.
- `make smoke` starts the server and curls `/health` plus `/v1/models`.
- `make verify` is mandatory before claiming any Rust change is done. It runs the project harness and refreshes `target/.verified`.

For targeted tests, use commands like `cargo test --package gateway-server --test mvp2_metering`.

## Verification Rule

Do not report Rust changes as complete until `make verify` passes. If a change touches auth, tenant isolation, metering, quota, cost attribution, or secret handling, also run the security review path described in [CLAUDE.md](CLAUDE.md). If it touches concurrency, shared state, retry, circuit breaker, or error paths, run the Rust review path described there.

## Conventions And Pitfalls

- Coordinate before changing public shared surfaces in `gateway-core` or route/middleware behavior in `gateway-server`.
- API keys must not be stored in plaintext; use the existing HMAC/salt approach in [crates/gateway-core/src/auth_key.rs](crates/gateway-core/src/auth_key.rs).
- Environment overrides use the `AI_GATEWAY__` prefix with double underscores for nesting.
- Persistence is partial: metering events and audit logs are JSONL-backed, while key store, quota state, and aggregate cost state are still in memory.
- `retry.rs` has known brittle string-based status handling; prefer structured status/error data when working nearby.
- The repo currently documents clippy debt in [CLAUDE.md](CLAUDE.md); avoid adding new warnings and fix local warnings in touched code.

## Useful Design Docs

- Enterprise design: [docs/superpowers/specs/2026-07-02-enterprise-ai-gateway-design.md](docs/superpowers/specs/2026-07-02-enterprise-ai-gateway-design.md)
- MVP plans index: [docs/superpowers/plans/README.md](docs/superpowers/plans/README.md)
- Claude harness details: [.claude/README.md](.claude/README.md)

## Improving These Instructions

Use `/chronicle improve` after real work sessions to surface recurring friction and turn it into concise updates here.