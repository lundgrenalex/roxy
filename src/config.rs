//! Configuration parsing and upstream routing primitives backed by YAML files.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::Uri;
use serde::Deserialize;

use crate::errors::AppError;

/// Default socket address used when `bind_addr` is omitted.
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
/// Default HTTP endpoint that exposes Prometheus-compatible metrics.
pub const DEFAULT_METRICS_PATH: &str = "/metrics";
/// Default configuration file path if `CONFIG_PATH` is unset.
pub const DEFAULT_CONFIG_PATH: &str = "config.yaml";
/// Base path reserved for inbound JSON-RPC routes.
pub const DEFAULT_ROUTE_BASE_PATH: &str = "/gate";
/// Default per-upstream request timeout in milliseconds.
pub const DEFAULT_UPSTREAM_TIMEOUT_MS: u64 = 300_000;
/// Default maximum request body size in bytes.
pub const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Runtime configuration derived from a YAML file.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub metrics_path: String,
    pub upstreams: Vec<Route>,
}

impl Config {
    /// Load configuration using the `CONFIG_PATH` environment variable or the default path.
    pub fn from_env() -> Result<Self, AppError> {
        let path = env::var("CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
        Self::from_yaml_file(path)
    }

    /// Load configuration from an explicit YAML file path.
    pub fn from_yaml_file<P: AsRef<Path>>(path: P) -> Result<Self, AppError> {
        let contents = fs::read_to_string(&path)?;
        Self::from_yaml_str(&contents)
    }

    /// Load configuration directly from a YAML string (useful for tests).
    pub fn from_yaml_str(yaml: &str) -> Result<Self, AppError> {
        let raw: FileConfig = serde_yaml::from_str(yaml)
            .map_err(|err| AppError::Config(format!("failed to parse YAML: {err}")))?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: FileConfig) -> Result<Self, AppError> {
        let bind_addr = raw
            .bind_addr
            .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string())
            .parse()
            .map_err(|err| AppError::Config(format!("invalid bind_addr: {err}")))?;

        let mut metrics_path = raw
            .metrics_path
            .unwrap_or_else(|| DEFAULT_METRICS_PATH.to_string());
        if !metrics_path.starts_with('/') {
            metrics_path = format!("/{}", metrics_path);
        }

        let upstreams = parse_upstreams(raw.upstreams)?;

        Ok(Self {
            bind_addr,
            metrics_path,
            upstreams,
        })
    }

    /// Summarise configured upstreams for human-friendly logging.
    pub fn route_summary(&self) -> Vec<String> {
        self.upstreams.iter().map(|route| route.summary()).collect()
    }
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    bind_addr: Option<String>,
    #[serde(default)]
    metrics_path: Option<String>,
    upstreams: BTreeMap<String, UpstreamConfig>,
}

#[derive(Debug, Deserialize)]
struct UpstreamConfig {
    #[serde(default)]
    routing_type: Option<RoutingKind>,
    urls: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_body_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RoutingKind {
    RoundRobin,
}

impl Default for RoutingKind {
    fn default() -> Self {
        RoutingKind::RoundRobin
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RoutingStrategy {
    RoundRobin,
}

/// A path-prefix rule that rewrites inbound requests to a target upstream.
#[derive(Clone)]
pub struct Route(Arc<RouteInner>);

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Route")
            .field("name", &self.name())
            .field("prefix", &self.0.prefix)
            .field("strategy", &self.0.strategy)
            .field("targets", &self.0.targets)
            .finish()
    }
}

impl Route {
    fn new(
        name: Arc<str>,
        prefix: String,
        strategy: RoutingStrategy,
        targets: Vec<Target>,
        timeout: Duration,
        max_body_bytes: usize,
    ) -> Result<Self, AppError> {
        if targets.is_empty() {
            return Err(AppError::Config(format!(
                "upstream '{name}' must configure at least one url"
            )));
        }

        let prefix_match = prefix_matcher(&prefix);
        Ok(Self(Arc::new(RouteInner {
            name,
            prefix,
            prefix_match,
            strategy,
            targets,
            cursor: AtomicUsize::new(0),
            timeout,
            max_body_bytes,
        })))
    }

    /// Name of the route for logging and metrics.
    pub fn name(&self) -> &str {
        self.0.name.as_ref()
    }

    /// Cloneable reference to the route name.
    pub fn name_arc(&self) -> Arc<str> {
        Arc::clone(&self.0.name)
    }

    /// Determine whether the request path is eligible for this route.
    pub fn matches(&self, path: &str) -> bool {
        if self.0.prefix == "/" {
            return true;
        }
        path == self.0.prefix || path.starts_with(&self.0.prefix_match)
    }

    /// Configured request timeout for this route.
    pub fn timeout(&self) -> Duration {
        self.0.timeout
    }

    /// Maximum allowed request body size for this route.
    pub fn max_body_bytes(&self) -> usize {
        self.0.max_body_bytes
    }

    /// Prepare the upstream URI and authority for a given inbound request URI.
    pub fn prepare_upstream(&self, original: &Uri) -> Result<PreparedUpstream, axum::http::Error> {
        let suffix = self.strip_prefix(original.path());
        let target = self.next_target();
        let merged_path = merge_paths(&target.base_path, suffix);
        let path_and_query = if let Some(query) = original.query() {
            format!("{}?{}", merged_path, query)
        } else {
            merged_path
        };

        let uri = Uri::builder()
            .scheme(target.scheme.as_str())
            .authority(target.authority.as_str())
            .path_and_query(path_and_query)
            .build()?;

        Ok(PreparedUpstream {
            uri,
            authority: target.authority,
        })
    }

    fn strip_prefix<'a>(&self, path: &'a str) -> &'a str {
        if self.0.prefix == "/" {
            return path.trim_start_matches('/');
        }

        match path.strip_prefix(&self.0.prefix) {
            Some(remaining) => remaining,
            None => path,
        }
    }

    fn next_target(&self) -> Target {
        let inner = &self.0;
        match inner.strategy {
            RoutingStrategy::RoundRobin => {
                let idx = inner.cursor.fetch_add(1, Ordering::Relaxed);
                let index = idx % inner.targets.len();
                inner.targets[index].clone()
            }
        }
    }

    /// Produce a short human-readable summary of the route and its primary target.
    pub fn summary(&self) -> String {
        let primary = self
            .0
            .targets
            .first()
            .map(|target| format!("{}{}", target.authority, target.base_path))
            .unwrap_or_else(|| "<missing>".to_string());
        format!("{} => {}", self.0.prefix, primary)
    }
}

struct RouteInner {
    name: Arc<str>,
    prefix: String,
    prefix_match: String,
    strategy: RoutingStrategy,
    targets: Vec<Target>,
    cursor: AtomicUsize,
    timeout: Duration,
    max_body_bytes: usize,
}

#[derive(Debug, Clone)]
struct Target {
    scheme: String,
    authority: String,
    base_path: String,
}

/// Result of preparing a request for an upstream target.
#[derive(Clone)]
pub struct PreparedUpstream {
    pub uri: Uri,
    pub authority: String,
}

fn parse_upstreams(entries: BTreeMap<String, UpstreamConfig>) -> Result<Vec<Route>, AppError> {
    if entries.is_empty() {
        return Err(AppError::Config(
            "configuration must include at least one upstream".into(),
        ));
    }

    let mut seen_prefixes = HashSet::new();
    let mut routes = Vec::new();

    for (name, upstream) in entries {
        let normalized_prefix = derive_gate_prefix(&name)?;

        if !seen_prefixes.insert(normalized_prefix.clone()) {
            return Err(AppError::Config(format!(
                "duplicate upstream prefix '{normalized_prefix}'"
            )));
        }

        let timeout_ms = upstream.timeout_ms.unwrap_or(DEFAULT_UPSTREAM_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err(AppError::Config(format!(
                "upstream '{name}' timeout_ms must be greater than zero"
            )));
        }
        let timeout = Duration::from_millis(timeout_ms);

        let max_body_bytes = upstream.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES);
        if max_body_bytes == 0 {
            return Err(AppError::Config(format!(
                "upstream '{name}' max_body_bytes must be greater than zero"
            )));
        }

        let strategy = match upstream.routing_type.unwrap_or_default() {
            RoutingKind::RoundRobin => RoutingStrategy::RoundRobin,
        };

        if upstream.urls.is_empty() {
            return Err(AppError::Config(format!(
                "upstream '{name}' must provide at least one url"
            )));
        }

        let mut targets = Vec::with_capacity(upstream.urls.len());
        for url in upstream.urls {
            let uri: Uri = url.parse().map_err(|err| {
                AppError::Config(format!("invalid url '{url}' for '{name}': {err}"))
            })?;

            let scheme = uri
                .scheme_str()
                .ok_or_else(|| {
                    AppError::Config(format!("upstream '{name}' url is missing a scheme"))
                })?
                .to_string();
            let authority = uri
                .authority()
                .ok_or_else(|| {
                    AppError::Config(format!("upstream '{name}' url is missing a host"))
                })?
                .to_string();

            if uri.query().is_some() {
                return Err(AppError::Config(format!(
                    "upstream '{name}' url must not include a query string"
                )));
            }

            let base_path = normalize_base_path(uri.path());

            targets.push(Target {
                scheme,
                authority,
                base_path,
            });
        }

        routes.push(Route::new(
            Arc::from(name),
            normalized_prefix,
            strategy,
            targets,
            timeout,
            max_body_bytes,
        )?);
    }

    routes.sort_by_key(|route| Reverse(route.0.prefix.len()));

    Ok(routes)
}

fn normalize_prefix(prefix: &str) -> Result<String, AppError> {
    if prefix.is_empty() {
        return Err(AppError::Config("route prefix cannot be empty".into()));
    }

    let mut normalized = if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("/{prefix}")
    };

    if normalized.len() > 1 {
        normalized = normalized.trim_end_matches('/').to_string();
    }

    Ok(normalized)
}

fn sanitize_route_segment(name: &str) -> String {
    let mut sanitized = String::new();
    let mut last_dash = false;

    for ch in name.trim().chars() {
        let mapped = match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => Some(ch),
            _ => None,
        };

        if let Some(valid) = mapped {
            sanitized.push(valid);
            last_dash = false;
        } else if !last_dash {
            sanitized.push('-');
            last_dash = true;
        }
    }

    sanitized.trim_matches('-').to_string()
}

fn derive_gate_prefix(name: &str) -> Result<String, AppError> {
    let sanitized = sanitize_route_segment(name);
    let base = DEFAULT_ROUTE_BASE_PATH.trim_end_matches('/');

    let combined = if sanitized.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{}", sanitized)
    };

    normalize_prefix(&combined)
}

fn normalize_base_path(path: &str) -> String {
    if path.is_empty() || path == "/" {
        "/".to_string()
    } else {
        path.trim_end_matches('/').to_string()
    }
}

fn prefix_matcher(prefix: &str) -> String {
    if prefix == "/" {
        "/".to_string()
    } else {
        format!("{}/", prefix.trim_end_matches('/'))
    }
}

fn merge_paths(base: &str, suffix: &str) -> String {
    let base = if base.is_empty() { "/" } else { base };

    if suffix.is_empty() {
        return base.to_string();
    }

    let trimmed_base = base.trim_end_matches('/');
    let trimmed_suffix = suffix.trim_start_matches('/');

    if trimmed_base.is_empty() {
        format!("/{}", trimmed_suffix)
    } else if trimmed_suffix.is_empty() {
        trimmed_base.to_string()
    } else {
        format!("{}/{}", trimmed_base, trimmed_suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Uri;

    const SAMPLE_YAML: &str = r#"
bind_addr: "127.0.0.1:9090"
metrics_path: "/custom-metrics"
upstreams:
  gnosis:
    routing_type: "round_robin"
    timeout_ms: 250
    max_body_bytes: 1024
    urls:
      - "https://alchemy.example"
      - "https://backup.alchemy.example/base"
  fallback:
    timeout_ms: 150
    max_body_bytes: 2048
    urls:
      - "https://fallback.example"
"#;

    #[test]
    fn parses_config_from_yaml() {
        let config = Config::from_yaml_str(SAMPLE_YAML).expect("config parses");
        assert_eq!(config.bind_addr, "127.0.0.1:9090".parse().unwrap());
        assert_eq!(config.metrics_path, "/custom-metrics");
        assert_eq!(config.upstreams.len(), 2);
        let gnosis = config
            .upstreams
            .iter()
            .find(|route| route.name() == "gnosis")
            .expect("gnosis route exists");
        assert!(gnosis.matches("/gate/gnosis/foo"));
        assert_eq!(gnosis.timeout(), Duration::from_millis(250));
        assert_eq!(gnosis.max_body_bytes(), 1024);
        let fallback = config
            .upstreams
            .iter()
            .find(|route| route.name() == "fallback")
            .expect("fallback route exists");
        assert!(fallback.matches("/gate/fallback/ping"));
        assert_eq!(fallback.timeout(), Duration::from_millis(150));
        assert_eq!(fallback.max_body_bytes(), 2048);
    }

    #[test]
    fn prepare_upstream_merges_paths_and_queries() {
        let config = Config::from_yaml_str(SAMPLE_YAML).unwrap();
        let route = config
            .upstreams
            .iter()
            .find(|route| route.name() == "gnosis")
            .cloned()
            .unwrap();
        let original = Uri::builder()
            .path_and_query("/gate/gnosis/rpc?foo=bar")
            .build()
            .unwrap();

        let prepared = route.prepare_upstream(&original).unwrap();
        assert_eq!(prepared.uri.scheme_str(), Some("https"));
        assert_eq!(
            prepared.uri.authority().map(|a| a.as_str()),
            Some("alchemy.example")
        );
        assert_eq!(
            prepared.uri.path_and_query().map(|pq| pq.as_str()),
            Some("/rpc?foo=bar")
        );
    }

    #[test]
    fn prepare_upstream_respects_base_path() {
        let config = Config::from_yaml_str(SAMPLE_YAML).unwrap();
        let route = config
            .upstreams
            .iter()
            .find(|route| route.name() == "gnosis")
            .cloned()
            .unwrap();
        let original = Uri::builder()
            .path_and_query("/gate/gnosis/debug/trace")
            .build()
            .unwrap();

        let prepared = route.prepare_upstream(&original).unwrap();
        assert_eq!(
            prepared.uri.path_and_query().map(|pq| pq.as_str()).unwrap(),
            "/debug/trace"
        );
    }

    #[test]
    fn sanitises_route_names() {
        let yaml = r#"
upstreams:
  "eth sepolia!":
    urls:
      - "https://sepolia.example"
"#;

        let config = Config::from_yaml_str(yaml).expect("config parses");
        let route = config
            .upstreams
            .iter()
            .find(|route| route.name() == "eth sepolia!")
            .expect("route exists");

        assert!(route.matches("/gate/eth-sepolia/rpc"));
        assert_eq!(
            route.timeout(),
            Duration::from_millis(DEFAULT_UPSTREAM_TIMEOUT_MS)
        );
        assert_eq!(route.max_body_bytes(), DEFAULT_MAX_BODY_BYTES);
    }
}
