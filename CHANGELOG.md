# Changelog

## v0.1.2

- Added per-method latency histogram (`roxy_rpc_method_latency_seconds_bucket`) to accompany method call counts.
- Included JSON-RPC method timing emission in the proxy hot path while preserving Hyper client speed.
- Shipped a Criterion benchmark (`cargo bench --bench proxy`) that spins a local upstream for end-to-end measurements.
- Ensured Docker builds carry the benches directory and use locked cargo cache mounts for deterministic multi-platform builds.
- Updated Helm defaults to reference image tag `v0.1.2` (digest optional).

## v0.1.1

- Introduced per-upstream `timeout_ms` and `max_body_bytes` guardrails with defaults (10s / 2 MiB).
- Preserved EVM-first routing semantics while adding documentation, README tone refresh, and X contact link.
- Added a custom Crypto Rebel License and `/gate/*` routing notes.
