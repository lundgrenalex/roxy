# syntax=docker/dockerfile:1.7

########## Build Stage ##########
FROM rust:bookworm AS builder

WORKDIR /app

# Pre-copy manifests to leverage Docker layer caching
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config.yaml ./config.yaml

# Build the binary in release mode
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    --mount=type=tmpfs,target=/tmp \
    cargo build --release \
    && cp /app/target/release/roxy /app/roxy

########## Runtime Stage ##########
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create an unprivileged user for the proxy
RUN useradd --system --home /app --shell /usr/sbin/nologin roxy

WORKDIR /app

COPY --from=builder /app/roxy /usr/local/bin/roxy
COPY config.yaml ./config.yaml

ENV CONFIG_PATH=/app/config.yaml
ENV RUST_LOG=info

USER roxy

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/roxy"]
