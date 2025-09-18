//! Prometheus instrumentation primitives for the proxy.

use std::time::Duration;

use axum::http::{Method, StatusCode};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};
use tracing::error;

/// Aggregates all Prometheus metrics tracked by the proxy.
#[derive(Clone)]
pub struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    responses_total: IntCounterVec,
    errors_total: IntCounterVec,
    inflight_requests: IntGaugeVec,
    latency_seconds: HistogramVec,
    method_calls_total: IntCounterVec,
}

impl Metrics {
    /// Create a new metrics registry with counter and histogram families registered.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new_custom(Some("roxy".into()), None)?;

        let requests_total = IntCounterVec::new(
            Opts::new("requests_total", "Total requests accepted by the proxy").namespace("proxy"),
            &["upstream", "method"],
        )?;
        registry.register(Box::new(requests_total.clone()))?;

        let responses_total = IntCounterVec::new(
            Opts::new("responses_total", "Responses returned from upstreams").namespace("proxy"),
            &["upstream", "method", "status"],
        )?;
        registry.register(Box::new(responses_total.clone()))?;

        let errors_total = IntCounterVec::new(
            Opts::new("errors_total", "Proxy errors by upstream").namespace("proxy"),
            &["upstream", "method", "kind"],
        )?;
        registry.register(Box::new(errors_total.clone()))?;

        let inflight_requests = IntGaugeVec::new(
            Opts::new("inflight_requests", "In-flight requests per upstream").namespace("proxy"),
            &["upstream"],
        )?;
        registry.register(Box::new(inflight_requests.clone()))?;

        let method_calls_total = IntCounterVec::new(
            Opts::new("method_calls_total", "Total JSON-RPC method invocations").namespace("proxy"),
            &["upstream", "method"],
        )?;
        registry.register(Box::new(method_calls_total.clone()))?;

        let latency_seconds = HistogramVec::new(
            HistogramOpts::new("request_latency_seconds", "Proxy latency in seconds")
                .namespace("proxy")
                .buckets(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                ]),
            &["upstream", "method"],
        )?;
        registry.register(Box::new(latency_seconds.clone()))?;

        Ok(Self {
            registry,
            requests_total,
            responses_total,
            errors_total,
            inflight_requests,
            latency_seconds,
            method_calls_total,
        })
    }

    /// Record a received request before proxying to the upstream.
    pub fn record_request(&self, upstream: &str, method: &Method) {
        self.requests_total
            .with_label_values(&[upstream, method.as_str()])
            .inc();
        self.inflight_requests.with_label_values(&[upstream]).inc();
    }

    /// Record a successful upstream response along with latency.
    pub fn record_response(
        &self,
        upstream: &str,
        method: &Method,
        status: StatusCode,
        elapsed: Duration,
    ) {
        let status_label = status.as_u16().to_string();
        self.responses_total
            .with_label_values(&[upstream, method.as_str(), status_label.as_str()])
            .inc();
        self.latency_seconds
            .with_label_values(&[upstream, method.as_str()])
            .observe(elapsed.as_secs_f64());
        self.inflight_requests.with_label_values(&[upstream]).dec();
    }

    /// Record a proxy error to help track quality-of-service issues.
    pub fn record_error(&self, upstream: &str, method: &Method, kind: &str) {
        self.errors_total
            .with_label_values(&[upstream, method.as_str(), kind])
            .inc();
        self.inflight_requests.with_label_values(&[upstream]).dec();
    }

    /// Record a request that failed to match any upstream route.
    pub fn record_unmatched(&self, method: &Method) {
        self.errors_total
            .with_label_values(&["__unmatched__", method.as_str(), "no_route"])
            .inc();
    }

    /// Record one or more JSON-RPC methods extracted from the request payload.
    pub fn record_method_calls(&self, upstream: &str, methods: &[String]) {
        for method in methods {
            self.method_calls_total
                .with_label_values(&[upstream, method.as_str()])
                .inc();
        }
    }

    /// Snapshot all metric families for Prometheus exposition.
    pub fn gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }

    /// Encode metrics to a text body consumable by Prometheus scrapers.
    pub fn encode(&self) -> Result<Vec<u8>, prometheus::Error> {
        let encoder = TextEncoder::new();
        let metrics = self.gather();
        let mut buffer = Vec::with_capacity(1024);
        encoder.encode(&metrics, &mut buffer)?;
        Ok(buffer)
    }

    pub(crate) fn handle_encode_error(err: prometheus::Error) {
        error!(?err, "failed to encode prometheus metrics");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_method_calls_increments_counter() {
        let metrics = Metrics::new().expect("metrics constructed");
        metrics.record_method_calls(
            "gnosis",
            &["eth_call".to_string(), "eth_getLogs".to_string()],
        );

        let first = metrics
            .method_calls_total
            .with_label_values(&["gnosis", "eth_call"])
            .get();
        let second = metrics
            .method_calls_total
            .with_label_values(&["gnosis", "eth_getLogs"])
            .get();

        assert_eq!(first, 1);
        assert_eq!(second, 1);
    }
}
