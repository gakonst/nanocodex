use std::{hint::black_box, net::Ipv4Addr, time::Duration};

use axum::{Router, extract::Request, routing::get};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use nanocodex_egress::{
    EgressLayer, EgressProxy,
    middleware::{Extensions, Next, Request as OutboundRequest, Response, async_trait},
};

struct MarkerLayer(&'static str);

#[async_trait]
impl EgressLayer for MarkerLayer {
    async fn handle(
        &self,
        mut request: OutboundRequest,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        request
            .headers_mut()
            .append("x-egress-benchmark", self.0.parse().unwrap());
        next.run(request, extensions).await
    }
}

fn benchmark_proxy_round_trip(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("benchmark runtime");
    let (origin, origin_task) = runtime.block_on(async {
        let app = Router::new().route(
            "/warm",
            get(|request: Request| async move {
                request
                    .headers()
                    .get_all("x-egress-benchmark")
                    .iter()
                    .count()
                    .to_string()
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("benchmark origin listener");
        let address = listener.local_addr().expect("benchmark origin address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("benchmark origin server");
        });
        (format!("http://{address}/warm"), task)
    });

    let mut group = criterion.benchmark_group("egress_proxy_round_trip");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(1));

    for (name, layers) in [("direct", 0_usize), ("two_layers", 2_usize)] {
        let proxy = runtime.block_on(async {
            let mut builder = EgressProxy::builder().allow_loopback_upstreams(true);
            if layers > 0 {
                builder = builder.layer(MarkerLayer("one"));
            }
            if layers > 1 {
                builder = builder.layer(MarkerLayer("two"));
            }
            builder.spawn().await.expect("benchmark proxy")
        });
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy.proxy_url()).expect("benchmark proxy URL"))
            .build()
            .expect("benchmark client");
        let expected = layers.to_string();
        group.bench_function(name, |bencher| {
            bencher.to_async(&runtime).iter(|| async {
                let response = client.get(&origin).send().await.expect("benchmark request");
                let body = response.text().await.expect("benchmark response body");
                assert_eq!(body, expected);
                black_box(body);
            });
        });
        runtime
            .block_on(proxy.shutdown())
            .expect("benchmark proxy shutdown");
    }
    group.finish();
    origin_task.abort();
}

criterion_group!(benches, benchmark_proxy_round_trip);
criterion_main!(benches);
