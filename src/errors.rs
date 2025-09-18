//! Shared error types surfaced across modules.

use std::sync::Arc;

use thiserror::Error;

/// Application-level failures that should terminate the process.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Metrics(#[from] prometheus::Error),
}

/// Request-scoped errors returned to API clients.
#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("no upstream configured for path {path}")]
    NoMatchingUpstream { path: String },
    #[error("failed to build upstream uri: {reason}")]
    InvalidUri { reason: String },
    #[error("upstream '{upstream}' request failed: {source}")]
    Upstream {
        upstream: Arc<str>,
        #[source]
        source: hyper_util::client::legacy::Error,
    },
    #[error("failed to read request body: {reason}")]
    BodyRead { reason: String },
    #[error("upstream '{upstream}' timed out")]
    UpstreamTimeout { upstream: Arc<str> },
    #[error("request body too large (limit {limit} bytes)")]
    BodyTooLarge { limit: usize },
}
