# Build stage
FROM rust:1.85-slim AS builder

WORKDIR /app

# Copy manifests first so the dependency layer caches independently of source changes.
COPY Cargo.toml Cargo.lock ./
COPY crates/gateway-core/Cargo.toml ./crates/gateway-core/
COPY crates/gateway-server/Cargo.toml ./crates/gateway-server/
COPY crates/providers/Cargo.toml ./crates/providers/

# Build a dummy binary so deps compile once and are cached.
RUN mkdir -p crates/gateway-server/src && \
    echo 'fn main() {}' > crates/gateway-server/src/main.rs && \
    cargo build --release --bin gateway-server && \
    rm -rf target/release/gateway-server* target/release/deps/gateway_server*

# Copy real source and build the actual binary.
COPY . .
RUN cargo build --release --bin gateway-server

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/gateway-server /usr/local/bin/gateway-server

# Non-root user for security.
USER 1000:1000

EXPOSE 8080

ENTRYPOINT ["gateway-server"]
