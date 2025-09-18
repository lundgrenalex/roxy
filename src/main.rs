use std::net::SocketAddr;
use std::process;

use axum::Router;
use tokio::net::TcpListener;
use tracing::{error, info};

use roxy::config::Config;
use roxy::errors::AppError;
use roxy::http_client::build_http_client;
use roxy::metrics::Metrics;
use roxy::proxy::{build_router, AppState};
use roxy::shutdown::shutdown_signal;

/// Entry point that configures tracing and starts the async runtime.
#[tokio::main]
async fn main() {
    init_tracing();

    if let Err(err) = run().await {
        error!(%err, "fatal error");
        process::exit(1);
    }
}

/// Boot the proxy, wiring configuration, HTTP server, and graceful shutdown together.
async fn run() -> Result<(), AppError> {
    let config = Config::from_env()?;
    info!(
        bind = %config.bind_addr,
        metrics = %config.metrics_path,
        routes = ?config.route_summary(),
        "starting roxy"
    );

    let client = build_http_client();
    let metrics = Metrics::new()?;

    let Config {
        bind_addr,
        metrics_path,
        upstreams,
    } = config;

    let state = AppState::new(upstreams, client, metrics);
    let app: Router = build_router(&metrics_path, state);

    let listener = TcpListener::bind(bind_addr).await?;
    info!(addr = %bind_addr, "listening for traffic");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

/// Initialize tracing subscribers using the `RUST_LOG` environment variable.
fn init_tracing() {
    let env_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_level(true)
        .compact();

    let _ = subscriber.try_init();
}
