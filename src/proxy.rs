//! HTTP routing logic that binds configuration, metrics, and the shared HTTP client together.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{self, HeaderValue, HOST};
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Error as AxumError;
use axum::Router;
use bytes::{Bytes, BytesMut};
use futures_util::stream::StreamExt;
use serde::Deserialize;
use tokio::time::timeout;
use tower::ServiceBuilder;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{warn, Level};

use crate::config::{PreparedUpstream, Route};
use crate::errors::ProxyError;
use crate::http_client::HttpClient;
use crate::metrics::Metrics;

/// Shared state injected into each request handler.
#[derive(Clone)]
pub struct AppState {
    routes: Arc<Vec<Route>>,
    client: Arc<dyn HttpClient>,
    metrics: Arc<Metrics>,
}

impl AppState {
    /// Construct state from owned configuration and dependencies.
    pub fn new(routes: Vec<Route>, client: Arc<dyn HttpClient>, metrics: Metrics) -> Self {
        Self {
            routes: Arc::new(routes),
            client,
            metrics: Arc::new(metrics),
        }
    }

    fn match_route(&self, path: &str) -> Option<Route> {
        self.routes
            .iter()
            .find(|route| route.matches(path))
            .cloned()
    }
}

/// Build the axum router with metrics and proxy handlers wired up.
pub fn build_router(metrics_path: &str, state: AppState) -> Router {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    Router::new()
        .route(metrics_path, get(metrics_handler))
        .fallback(proxy_handler)
        .with_state(state)
        .layer(ServiceBuilder::new().layer(trace_layer))
}

pub(crate) async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.metrics.encode() {
        Ok(buffer) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, prometheus::TEXT_FORMAT)
            .body(Body::from(buffer))
            .unwrap_or_else(|err| {
                warn!(?err, "failed to build metrics response");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }),
        Err(err) => {
            Metrics::handle_encode_error(err);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn proxy_handler(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Result<Response, ProxyError> {
    let method = req.method().clone();
    let original_uri = req.uri().clone();
    let path = original_uri.path().to_string();

    let route = match state.match_route(&path) {
        Some(route) => route,
        None => {
            state.metrics.record_unmatched(&method);
            return Err(ProxyError::NoMatchingUpstream { path });
        }
    };

    state.metrics.record_request(route.name(), &method);
    let request_start = Instant::now();

    let prepared = route.prepare_upstream(&original_uri).map_err(|err| {
        state
            .metrics
            .record_error(route.name(), &method, "rewrite_uri");
        ProxyError::InvalidUri {
            reason: err.to_string(),
        }
    })?;

    let original_host = req.headers().get(HOST).cloned();
    let inferred_proto = infer_request_proto(req.headers(), &original_uri);

    let (mut parts, body) = req.into_parts();
    let max_body_bytes = route.max_body_bytes();
    let body_bytes = match collect_body_with_limit(body, max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(BodyCollectError::Io(err)) => {
            state
                .metrics
                .record_error(route.name(), &method, "body_read");
            return Err(ProxyError::BodyRead {
                reason: err.to_string(),
            });
        }
        Err(BodyCollectError::TooLarge) => {
            state
                .metrics
                .record_error(route.name(), &method, "body_too_large");
            return Err(ProxyError::BodyTooLarge {
                limit: max_body_bytes,
            });
        }
    };

    if let Ok(methods) = extract_methods(body_bytes.as_ref()) {
        if !methods.is_empty() {
            state.metrics.record_method_calls(route.name(), &methods);
        }
    }

    parts.uri = prepared.uri.clone();
    let mut req = Request::from_parts(parts, Body::from(body_bytes));
    rewrite_request_headers(
        &mut req,
        &prepared,
        &peer_addr,
        original_host,
        &inferred_proto,
    );

    match timeout(route.timeout(), state.client.request(req)).await {
        Ok(Ok(response)) => {
            let elapsed = request_start.elapsed();
            state
                .metrics
                .record_response(route.name(), &method, response.status(), elapsed);

            let (mut parts, body) = response.into_parts();
            parts
                .headers
                .append(header::VIA, HeaderValue::from_static("1.1 roxy"));
            Ok(Response::from_parts(parts, body))
        }
        Ok(Err(err)) => {
            state
                .metrics
                .record_error(route.name(), &method, "upstream");
            Err(ProxyError::Upstream {
                upstream: route.name_arc(),
                source: err,
            })
        }
        Err(_) => {
            state
                .metrics
                .record_error(route.name(), &method, "upstream_timeout");
            Err(ProxyError::UpstreamTimeout {
                upstream: route.name_arc(),
            })
        }
    }
}

fn rewrite_request_headers(
    req: &mut Request<Body>,
    prepared: &PreparedUpstream,
    peer_addr: &SocketAddr,
    original_host: Option<HeaderValue>,
    inferred_proto: &str,
) {
    let peer_ip = peer_addr.ip().to_string();
    let forwarded_chain = if let Some(existing) = req.headers().get("x-forwarded-for") {
        match existing.to_str() {
            Ok(value) if !value.trim().is_empty() => format!("{value}, {peer_ip}"),
            _ => peer_ip.clone(),
        }
    } else {
        peer_ip.clone()
    };

    req.headers_mut().insert(
        header::HeaderName::from_static("x-forwarded-for"),
        HeaderValue::from_str(&forwarded_chain)
            .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    let proto_value = match req.headers().get("x-forwarded-proto") {
        Some(value) => value.to_str().unwrap_or(inferred_proto).to_owned(),
        None => {
            let header_value = HeaderValue::from_str(inferred_proto)
                .unwrap_or_else(|_| HeaderValue::from_static("http"));
            req.headers_mut().insert(
                header::HeaderName::from_static("x-forwarded-proto"),
                header_value.clone(),
            );
            header_value.to_str().unwrap_or("http").to_owned()
        }
    };

    if let Some(host) = original_host {
        req.headers_mut()
            .insert(header::HeaderName::from_static("x-forwarded-host"), host);
    }

    req.headers_mut().insert(
        HOST,
        HeaderValue::from_str(&prepared.authority)
            .expect("validated host header during configuration"),
    );

    let forwarded_value = format!("for=\"{}\";proto={}", peer_ip, proto_value);
    req.headers_mut().insert(
        header::HeaderName::from_static("forwarded"),
        HeaderValue::from_str(&forwarded_value)
            .unwrap_or_else(|_| HeaderValue::from_static("for=\"unknown\"")),
    );
}

fn infer_request_proto(headers: &header::HeaderMap, uri: &Uri) -> String {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned())
        .or_else(|| uri.scheme_str().map(|scheme| scheme.to_owned()))
        .unwrap_or_else(|| "http".to_string())
}

enum BodyCollectError {
    Io(AxumError),
    TooLarge,
}

async fn collect_body_with_limit(body: Body, limit: usize) -> Result<Bytes, BodyCollectError> {
    let mut stream = body.into_data_stream();
    let mut buffer = BytesMut::with_capacity(std::cmp::min(limit, 8 * 1024));

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(BodyCollectError::Io)?;
        if buffer.len() + chunk.len() > limit {
            return Err(BodyCollectError::TooLarge);
        }
        buffer.extend_from_slice(&chunk);
    }

    Ok(buffer.freeze())
}

#[derive(Deserialize)]
struct JsonRpcMethod<'a> {
    #[serde(borrow)]
    method: Cow<'a, str>,
}

fn extract_methods(bytes: &[u8]) -> Result<Vec<String>, serde_json::Error> {
    let first = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());

    match first {
        Some(b'{') => {
            let request: JsonRpcMethod<'_> = serde_json::from_slice(bytes)?;
            Ok(vec![request.method.into_owned()])
        }
        Some(b'[') => {
            let requests: Vec<JsonRpcMethod<'_>> = serde_json::from_slice(bytes)?;
            Ok(requests
                .into_iter()
                .map(|request| request.method.into_owned())
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        match self {
            ProxyError::NoMatchingUpstream { path } => (
                StatusCode::NOT_FOUND,
                format!("no upstream configured for path {path}"),
            )
                .into_response(),
            ProxyError::InvalidUri { reason } => (
                StatusCode::BAD_GATEWAY,
                format!("invalid upstream uri: {reason}"),
            )
                .into_response(),
            ProxyError::Upstream { upstream, source } => {
                warn!(%upstream, error = %source, "upstream request failed");
                (StatusCode::BAD_GATEWAY, "upstream request failed").into_response()
            }
            ProxyError::BodyRead { reason } => (
                StatusCode::BAD_REQUEST,
                format!("failed to read request body: {reason}"),
            )
                .into_response(),
            ProxyError::UpstreamTimeout { .. } => (
                StatusCode::GATEWAY_TIMEOUT,
                "upstream timed out".to_string(),
            )
                .into_response(),
            ProxyError::BodyTooLarge { limit } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body exceeds limit of {limit} bytes"),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, Uri};
    use futures_util::future::pending;
    use std::net::{IpAddr, Ipv4Addr};

    use crate::config::Config;
    use crate::http_client::build_http_client;
    use crate::metrics::Metrics;

    fn build_route() -> Route {
        let yaml = r#"
upstreams:
  gnosis:
    timeout_ms: 50
    max_body_bytes: 128
    urls:
      - "https://alchemy.example"
"#;

        Config::from_yaml_str(yaml)
            .unwrap()
            .upstreams
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn rewrite_request_headers_sets_forwarding_fields() {
        let route = build_route();
        let mut request = Request::builder()
            .method("POST")
            .uri("/gate/gnosis")
            .header(HOST, "proxy.local")
            .header("x-forwarded-for", "10.0.0.1")
            .body(Body::empty())
            .unwrap();

        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 10)), 50000);
        let original_host = request.headers().get(HOST).cloned();
        let original_uri = Uri::builder()
            .path_and_query("/gate/gnosis")
            .build()
            .unwrap();
        let prepared = route.prepare_upstream(&original_uri).unwrap();
        let proto = infer_request_proto(request.headers(), &original_uri);
        rewrite_request_headers(&mut request, &prepared, &peer, original_host, &proto);

        assert_eq!(
            request.headers().get(HOST).map(|h| h.to_str().unwrap()),
            Some("alchemy.example")
        );
        assert_eq!(
            request
                .headers()
                .get("x-forwarded-for")
                .map(|h| h.to_str().unwrap()),
            Some("10.0.0.1, 192.168.0.10")
        );
        assert_eq!(
            request
                .headers()
                .get("x-forwarded-proto")
                .map(|h| h.to_str().unwrap()),
            Some("http")
        );
        assert_eq!(
            request
                .headers()
                .get("forwarded")
                .map(|h| h.to_str().unwrap()),
            Some("for=\"192.168.0.10\";proto=http")
        );
        assert_eq!(
            request
                .headers()
                .get("x-forwarded-host")
                .map(|h| h.to_str().unwrap()),
            Some("proxy.local")
        );
    }

    #[test]
    fn rewrite_request_headers_preserves_forwarded_proto() {
        let route = build_route();
        let mut request = Request::builder()
            .method("POST")
            .uri("/gate/gnosis")
            .header(HOST, "proxy.local")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();

        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345);
        let original_host = request.headers().get(HOST).cloned();
        let original_uri = Uri::builder()
            .path_and_query("/gate/gnosis")
            .build()
            .unwrap();
        let prepared = route.prepare_upstream(&original_uri).unwrap();
        let proto = infer_request_proto(request.headers(), &original_uri);

        rewrite_request_headers(&mut request, &prepared, &peer, original_host, &proto);

        assert_eq!(
            request
                .headers()
                .get("x-forwarded-proto")
                .map(|h| h.to_str().unwrap()),
            Some("https"),
        );
        assert_eq!(
            request
                .headers()
                .get("forwarded")
                .map(|h| h.to_str().unwrap()),
            Some("for=\"10.0.0.1\";proto=https"),
        );
    }

    #[tokio::test]
    async fn oversized_body_returns_413() {
        let route = build_route();
        let max_body_bytes = route.max_body_bytes();
        let client = build_http_client();
        let metrics = Metrics::new().expect("metrics constructed");
        let state = AppState::new(vec![route], client, metrics);

        let oversized_payload = vec![b'{'; max_body_bytes + 1];
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000);
        let request = Request::builder()
            .uri("/gate/gnosis")
            .method("POST")
            .body(Body::from(oversized_payload))
            .unwrap();

        let result = proxy_handler(State(state), ConnectInfo(peer), request).await;

        match result {
            Err(ProxyError::BodyTooLarge { limit }) => {
                assert_eq!(limit, max_body_bytes);
            }
            other => panic!("expected body too large error, got {other:?}"),
        }
    }

    #[test]
    fn extract_methods_handles_single_and_batch() {
        let single = br#"{"jsonrpc":"2.0","method":"eth_call","params":[]}"#;
        let batch = br#"[{"jsonrpc":"2.0","method":"eth_call"},{"method":"eth_getLogs"}]"#;

        let single_methods = extract_methods(single).expect("single parse");
        let batch_methods = extract_methods(batch).expect("batch parse");

        assert_eq!(single_methods, vec!["eth_call".to_string()]);
        assert_eq!(
            batch_methods,
            vec!["eth_call".to_string(), "eth_getLogs".to_string()]
        );
    }

    #[test]
    fn extract_methods_trims_whitespace_and_handles_non_json() {
        let padded = b"  {\n  \"method\": \"eth_blockNumber\" }";
        let empty = b"";
        let text = b"not json";

        let padded_methods = extract_methods(padded).expect("padded parse");
        let empty_methods = extract_methods(empty).expect("empty parse");
        let text_methods = extract_methods(text).expect("text parse");

        assert_eq!(padded_methods, vec!["eth_blockNumber".to_string()]);
        assert!(empty_methods.is_empty());
        assert!(text_methods.is_empty());
    }

    struct PendingClient;

    impl HttpClient for PendingClient {
        fn request(
            &self,
            _req: Request<Body>,
        ) -> futures_util::future::BoxFuture<
            'static,
            Result<Response<Body>, hyper_util::client::legacy::Error>,
        > {
            Box::pin(async move {
                pending::<()>().await;
                unreachable!();
            })
        }
    }

    #[tokio::test]
    async fn upstream_timeout_returns_error() {
        let route = build_route();
        let client: Arc<dyn HttpClient> = Arc::new(PendingClient);
        let metrics = Metrics::new().expect("metrics constructed");
        let state = AppState::new(vec![route], client, metrics);

        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50000);
        let request = Request::builder()
            .method("POST")
            .uri("/gate/gnosis")
            .body(Body::from("{}"))
            .unwrap();

        let result = proxy_handler(State(state), ConnectInfo(peer), request).await;

        match result {
            Err(ProxyError::UpstreamTimeout { upstream }) => {
                assert_eq!(upstream.as_ref(), "gnosis");
            }
            other => panic!("expected upstream timeout, got {other:?}"),
        }
    }
}
