# Changelog

## v0.1.5

- Added a minimal integration test harness (`tests/integration/basic.rs`) that spins a local upstream and validates end-to-end proxying.
- Logged upstream transport failures with structured WARN entries, improving troubleshooting of dropped connections.
- Bumped default version references and Helm chart tag to `v0.1.5`.
- Fixed JSON-RPC error metrics to handle gzip-compressed upstream responses, keeping `roxy_rpc_method_errors_total` accurate.
- Added unit coverage for ID-less, compressed error responses to guard the regression.

## v0.1.4

- Raised default per-upstream timeout to 5 minutes and max body size to 10 MiB across config, examples, and Helm values (Chainstack profile included).
- Added structured warnings when requests exceed body limits or when upstream calls time out.
- Published the JSON-RPC method error counter (`roxy_rpc_method_errors_total`) and updated documentation.

## v0.1.3

- Added per-method JSON-RPC error counter (`roxy_rpc_method_errors_total`) with labelled buckets (parse errors, invalid params, execution errors, etc.).
- Proxy now inspects upstream responses and records method-specific latency/errors without leaving the Hyper fast path.
- Raised default upstream request timeout to 5 minutes and request body cap to 10 MiB (configurable per route).

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
