# Roxy

Roxy is a Rust-built reverse proxy that keeps your EVM JSON-RPC requests flowing to the right upstreams without drama. Metrics are baked in because dashboards > guesswork.

> **Heads-up**: Roxy only speaks EVM JSON-RPC. Toss her non-EVM protocol chatter and she’ll just shrug.

Questions, beef, or memes? Catch me on X: [@fortvi](https://x.com/fortvi).

## What Roxy Does

- Routes based on `/gate/{upstream-name}` prefixes (HTTP JSON-RPC only right now).
- Pulls upstream config straight from environment variables.
- Keeps an async HTTPS client warm (HTTP/1.1 + HTTP/2) with pooling so latency stays chill.
- Exposes Prometheus metrics for request counts, latencies, errors, and in-flight calls.
- Understands gzip-encoded upstream responses when extracting JSON-RPC error telemetry, so your dashboards stay accurate.
- Shuts down gracefully so rolling updates don’t faceplant.
- Per-upstream request guards for max body size and upstream timeout.
- Hyper-fast hot path (≈45 µs per JSON-RPC dispatch on loopback with Hyper client).
- EVM-first feature set: JSON-RPC metrics, routing, and guardrails tuned for chain infra.
- All-in-one tooling: Make targets, Dockerfile, Helm chart, and per-upstream configuration ready to ship.

## Setup in 60 Seconds

Drop your config in `config.yaml` (or point `CONFIG_PATH` somewhere else). Every upstream rides under the `/gate/*` umbrella so you can keep the public surface tidy. Example config:

```yaml
bind_addr: "0.0.0.0:8080"        # optional, defaults shown
metrics_path: "/metrics"        # optional
upstreams:
  gnosis:                        # route key => /gate/gnosis/*
    routing_type: "round_robin"  # optional (currently only round_robin)
    timeout_ms: 300000           # optional per-upstream timeout (defaults to 5 minutes)
    max_body_bytes: 10485760     # optional per-upstream body cap (defaults to 10 MiB)
    urls:
      - "https://alchemyurltognosis"
      - "https://backup-gnosis"     # more URLs = round-robin love
  ethereum-mainnet:              # route key => /gate/ethereum-mainnet/*
    timeout_ms: 300000
    max_body_bytes: 10485760
    urls:
      - "https://private-eth-mainnet"
```

Give every upstream at least one full URL. Roxy auto-mounts each route at `/gate/{name}` after sanitising the key (spaces and punctuation collapse to `-`). Multiple URLs get served round-robin style, and the longest prefix still wins the match. Tune `timeout_ms` and `max_body_bytes` per upstream to override the defaults (5 minutes, 10 MiB).

Need more logs? Tweak `RUST_LOG`, e.g. `RUST_LOG=info,tower_http=warn`.

## Run It Local

```sh
make run
```

That blocks your shell; pass env vars inline or via your favourite `.env` sorcery. Prefer raw cargo? There’s a Makefile target for every mood:

- `make build` / `make debug`
- `make test`
- `make fmt`
- `make lint`

## Ship a Container

```sh
docker build -t roxy -f etc/docker/Dockerfile .
```

Then run it with your config mounted:

```sh
docker run --rm \
  -p 8080:8080 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -e CONFIG_PATH=/app/config.yaml \
  roxy
```

## Kubernetes Friends

Helm chart lives at `etc/helm/roxy`:

```sh
make helm-lint
make helm-package
```

Deploy when ready:

```sh
helm upgrade --install roxy etc/helm/roxy \
  --namespace rpc-proxy --create-namespace \
  --set image.repository="ghcr.io/your-org/roxy" \
  --set image.tag="v0.1.0"
```

## Metrics Buffet

Prometheus scrape lives at whatever `METRICS_PATH` says (defaults to `/metrics`). Expect the usual suspects:

- `roxy_proxy_requests_total{upstream,method}`
- `roxy_proxy_responses_total{upstream,method,status}`
- `roxy_proxy_errors_total{upstream,method,kind}` (`upstream="__unmatched__"` means no route matched)
- `roxy_proxy_method_calls_total{upstream,method}`
- `roxy_proxy_request_latency_seconds_bucket{upstream,method,...}`
- `roxy_proxy_inflight_requests{upstream}`
- `roxy_rpc_method_latency_seconds_bucket{upstream,method}`
- `roxy_rpc_method_errors_total{upstream,method,error}`

Hook those into Grafana, brag about low latencies, repeat.

## Benchmarks

```sh
cargo bench --bench proxy
```

Example run on a MacBook Pro (M2 Pro, 12-core) with loopback HTTP upstream:

```
proxy_hot_path/dispatch time:   [44.7 µs 45.9 µs 47.5 µs]
                        thrpt:  [21.0 Kelem/s 21.8 Kelem/s 22.4 Kelem/s]
```

Each iteration covers the full client → proxy → upstream round trip using the real Hyper client.

## Integration Tests

Minimal smoke tests live under `tests/integration`. Run them alongside unit tests with:

```sh
cargo test
```

The harness stands up a local upstream and ensures Roxy proxies JSON-RPC requests end to end.

## On the Horizon

- WebSocket JSON-RPC proxying so Roxy can hang out with streamers.

## Going Prod? Read Me

- Build with `cargo build --release` before shipping.
- Don’t pump high-cardinality labels into metrics (no request IDs in env vars, please).
- Tune `RUST_LOG` to match your vibe (`info,tower_http=warn` is a solid baseline).
- Run Roxy under something that can restart it (systemd, Kubernetes, Nomad, whatever keeps it honest).

## License

This project rolls under the [Crypto Rebel License (CRL) v0.7](LICENSE.md). Stay sovereign. Ship responsibly.
