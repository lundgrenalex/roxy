//! HTTP client abstraction used by the proxy to forward JSON-RPC traffic.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, Response};
use futures_util::future::BoxFuture;
use http_body_util::BodyExt;
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// Trait representing the minimal interface the proxy needs to send HTTP requests.
pub trait HttpClient: Send + Sync {
    fn request(
        &self,
        req: Request<Body>,
    ) -> BoxFuture<'static, Result<Response<Body>, hyper_util::client::legacy::Error>>;
}

struct HyperHttpClient {
    inner: Client<HttpsConnector<HttpConnector>, Body>,
}

impl HyperHttpClient {
    fn new() -> Self {
        let mut connector = HttpsConnector::new();
        connector.https_only(false);

        let inner = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(256)
            .build(connector);

        Self { inner }
    }
}

impl HttpClient for HyperHttpClient {
    fn request(
        &self,
        req: Request<Body>,
    ) -> BoxFuture<'static, Result<Response<Body>, hyper_util::client::legacy::Error>> {
        let fut = self.inner.request(req);
        Box::pin(async move {
            let response = fut.await?;
            let (parts, incoming) = response.into_parts();
            let body = Body::from_stream(incoming.into_data_stream());
            Ok(Response::from_parts(parts, body))
        })
    }
}

/// Construct the default HTTP client used by the proxy.
pub fn build_http_client() -> Arc<dyn HttpClient> {
    Arc::new(HyperHttpClient::new())
}
