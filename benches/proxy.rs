use std::net::SocketAddr;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use axum::Router;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tower::{Service, ServiceExt};

use roxy::config::Config;
use roxy::http_client::build_http_client;
use roxy::metrics::Metrics;
use roxy::proxy::{build_router, AppState};

fn spawn_upstream(rt: &Runtime) -> (SocketAddr, JoinHandle<()>) {
    rt.block_on(async {
        let app = Router::new().route("/*path", any(|| async { StatusCode::OK }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("addr");

        let handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("upstream server error: {err}");
            }
        });

        (addr, handle)
    })
}

fn build_state(upstream_addr: SocketAddr) -> AppState {
    let yaml = format!(
        "upstreams:\n  bench:\n    timeout_ms: 5000\n    max_body_bytes: 65536\n    urls:\n      - \"http://{upstream_addr}\"\n"
    );

    let config = Config::from_yaml_str(&yaml).expect("config parses");
    let routes = config.upstreams;
    let client = build_http_client();
    let metrics = Metrics::new().expect("metrics constructed");
    AppState::new(routes, client, metrics)
}

fn bench_proxy(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime");
    let (upstream_addr, _handle) = spawn_upstream(&rt);
    let state = build_state(upstream_addr);
    let router: Router = build_router("/metrics", state);
    let mut group = c.benchmark_group("proxy_hot_path");
    group.throughput(Throughput::Elements(1));

    group.bench_function(BenchmarkId::from_parameter("dispatch"), |b| {
        b.to_async(&rt).iter(|| {
            let mut make_service = router
                .clone()
                .into_make_service_with_connect_info::<SocketAddr>();
            let request = Request::builder()
                .method("POST")
                .uri("/gate/bench")
                .body(Body::empty())
                .unwrap();

            async move {
                let conn_info = SocketAddr::from(([127, 0, 0, 1], 9999));
                let service = make_service.call(conn_info).await.expect("make service");
                let _ = service.oneshot(request).await.expect("proxy response");
            }
        });
    });

    group.finish();
}

criterion_group!(proxy_benches, bench_proxy);
criterion_main!(proxy_benches);
