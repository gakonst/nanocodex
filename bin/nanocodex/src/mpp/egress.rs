//! Tempo-owned MPP behavior composed into the generic egress proxy.

use std::sync::atomic::{AtomicU64, Ordering};

use mpp::client::{
    AcceptPaymentPolicy, ClientEvent, ClientEventSubscription, ClientEvents, PaymentMiddleware,
    PaymentProvider,
};
use nanocodex_egress::{
    EgressLayer,
    middleware::{
        Error, Extensions, Middleware, Next, Request, Response, Result as MiddlewareResult,
        StatusCode, async_trait, buffer_response_body,
    },
};

const DEFAULT_MAX_PAYMENT_REQUIRED_BYTES: usize = 1024 * 1024;
const MPP_REQUEST_ID: &str = "mpp-request-id";

/// MPP payment and replay as one Tempo-owned outbound egress layer.
pub(super) struct TempoEgress<P> {
    middleware: PaymentMiddleware<P>,
    _events: ClientEventSubscription,
    max_payment_required_bytes: usize,
    request_id_prefix: String,
    request_ids: AtomicU64,
}

impl<P> TempoEgress<P>
where
    P: PaymentProvider + 'static,
{
    /// Creates an MPP layer with the protocol library's retry default.
    pub(super) fn new(provider: P) -> Self {
        let events = ClientEvents::default();
        let subscription = events.on_any(|event| async move {
            record_payment_event(&event);
        });
        let middleware = PaymentMiddleware::new(provider)
            .with_accept_payment_policy(AcceptPaymentPolicy::Never)
            .with_events(events);
        Self {
            middleware,
            _events: subscription,
            max_payment_required_bytes: DEFAULT_MAX_PAYMENT_REQUIRED_BYTES,
            request_id_prefix: random_identifier(),
            request_ids: AtomicU64::new(1),
        }
    }

    #[cfg(test)]
    const fn max_payment_required_bytes(mut self, max_bytes: usize) -> Self {
        self.max_payment_required_bytes = max_bytes;
        self
    }
}

#[async_trait]
impl<P> EgressLayer for TempoEgress<P>
where
    P: PaymentProvider + 'static,
{
    async fn handle(
        &self,
        mut request: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<Response> {
        buffer_response_body(
            extensions,
            StatusCode::PAYMENT_REQUIRED,
            self.max_payment_required_bytes,
        );
        let request_id = self.request_ids.fetch_add(1, Ordering::Relaxed);
        let logical_request_id = format!("{}-{request_id}", self.request_id_prefix);
        tracing::Span::current().record("mpp.request.id", logical_request_id.as_str());
        let value = logical_request_id.parse().map_err(Error::middleware)?;
        request.headers_mut().insert(MPP_REQUEST_ID, value);
        self.middleware.handle(request, extensions, next).await
    }

    fn uses_response_buffering(&self) -> bool {
        true
    }
}

fn random_identifier() -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut identifier = String::with_capacity(64);
    for byte in rand::random::<[u8; 32]>() {
        identifier.push(char::from(HEX[usize::from(byte >> 4)]));
        identifier.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    identifier
}

fn record_payment_event(event: &ClientEvent) {
    match event {
        ClientEvent::ChallengeReceived(context) => {
            tracing::info!(
                target: "mpp_egress",
                stage = "mpp.egress.challenge.received",
                challenge.id = %context.challenge.id,
                challenge.realm = %context.challenge.realm,
                payment.method = %context.challenge.method,
                payment.intent = %context.challenge.intent,
                challenge.count = context.challenges.len(),
                "MPP egress selected a 402 payment challenge"
            );
            tracing::info!(
                target: "mpp_egress",
                content_kind = "mpp.egress.challenge",
                content = ?context,
                "trace content"
            );
        }
        ClientEvent::CredentialCreated(context) => {
            tracing::info!(
                target: "mpp_egress",
                stage = "mpp.egress.credential.created",
                challenge.id = %context.challenge.id,
                payment.method = %context.challenge.method,
                payment.intent = %context.challenge.intent,
                "MPP egress created a payment credential for replay"
            );
            tracing::info!(
                target: "mpp_egress",
                content_kind = "mpp.egress.credential",
                content = ?context.credential,
                "trace content"
            );
        }
        ClientEvent::PaymentResponse(context) => {
            tracing::info!(
                target: "mpp_egress",
                stage = "mpp.egress.payment.response",
                challenge.id = %context.challenge.id,
                payment.method = %context.challenge.method,
                payment.intent = %context.challenge.intent,
                http.response.status_code = context.status.as_u16(),
                "MPP egress received the paid replay response"
            );
            tracing::info!(
                target: "mpp_egress",
                content_kind = "mpp.egress.payment.response.credential",
                content = ?context.credential,
                "trace content"
            );
        }
        ClientEvent::PaymentFailed(context) => {
            let challenge_id = context
                .challenge
                .as_ref()
                .map_or("", |challenge| challenge.id.as_str());
            tracing::warn!(
                target: "mpp_egress",
                stage = "mpp.egress.payment.failed",
                challenge.id = challenge_id,
                error = %context.error,
                reason = ?context.reason,
                "MPP egress payment handling failed"
            );
            tracing::info!(
                target: "mpp_egress",
                content_kind = "mpp.egress.payment.failure",
                content = ?context,
                "trace content"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::Body,
        extract::Request,
        http::{StatusCode as AxumStatus, header::WWW_AUTHENTICATE},
        response::IntoResponse,
        routing::post,
    };
    use futures_util::{StreamExt, stream};
    use mpp::{
        Base64UrlJson, MppError, PaymentChallenge, PaymentCredential, PaymentPayload,
        client::DEFAULT_MAX_PAYMENT_RETRIES, format_www_authenticate,
    };
    use nanocodex_egress::{EgressProxy, EgressProxyBuilder};

    use super::*;

    #[derive(Clone, Default)]
    struct MockProvider {
        active: Arc<AtomicUsize>,
        commits: Arc<AtomicUsize>,
        delay_payments: bool,
        maximum: Arc<AtomicUsize>,
        payments: Arc<AtomicUsize>,
        rollbacks: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn with_payment_delay(mut self) -> Self {
            self.delay_payments = true;
            self
        }
    }

    impl PaymentProvider for MockProvider {
        fn supports(&self, method: &str, intent: &str) -> bool {
            method == "test" && intent == "charge"
        }

        async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.payments.fetch_add(1, Ordering::SeqCst);
            if self.delay_payments {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(PaymentCredential::new(
                challenge.to_echo(),
                PaymentPayload::hash("test-payment"),
            ))
        }

        async fn commit_payment(
            &self,
            _: &PaymentChallenge,
            _: &PaymentCredential,
        ) -> Result<(), MppError> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn rollback_payment(
            &self,
            _: &PaymentChallenge,
            _: &PaymentCredential,
        ) -> Result<(), MppError> {
            self.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    async fn spawn_origin(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    fn challenge_header(id: &str) -> String {
        let request = Base64UrlJson::from_value(&serde_json::json!({
            "amount": "1",
            "currency": "test"
        }))
        .unwrap();
        format_www_authenticate(&PaymentChallenge::new(
            id,
            "test.local",
            "test",
            "charge",
            request,
        ))
        .unwrap()
    }

    fn proxied_client(egress: &EgressProxy) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(egress.proxy_url()).unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    async fn mpp_proxy(
        provider: MockProvider,
        configure: impl FnOnce(EgressProxyBuilder) -> EgressProxyBuilder,
    ) -> EgressProxy {
        configure(EgressProxy::builder().allow_loopback_upstreams(true))
            .layer(TempoEgress::new(provider))
            .spawn()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pays_and_replays_the_exact_host_identified_request_once() {
        let challenge = challenge_header("challenge-1");
        let observations = Arc::new(Mutex::new(Vec::new()));
        let route_observations = Arc::clone(&observations);
        let origin = spawn_origin(Router::new().route(
            "/paid",
            post(move |request: Request<Body>| {
                let challenge = challenge.clone();
                let observations = Arc::clone(&route_observations);
                async move {
                    let request_id = request
                        .headers()
                        .get(MPP_REQUEST_ID)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned();
                    let paid = request.headers().contains_key("authorization");
                    let accepts_payment = request.headers().contains_key("accept-payment");
                    let body = axum::body::to_bytes(request.into_body(), 1024)
                        .await
                        .unwrap();
                    observations.lock().unwrap().push((
                        request_id,
                        body.to_vec(),
                        paid,
                        accepts_payment,
                    ));
                    if paid {
                        (AxumStatus::OK, "paid").into_response()
                    } else {
                        (
                            AxumStatus::PAYMENT_REQUIRED,
                            [(WWW_AUTHENTICATE, challenge)],
                            "payment required",
                        )
                            .into_response()
                    }
                }
            }),
        ))
        .await;
        let provider = MockProvider::default();
        let payments = Arc::clone(&provider.payments);
        let commits = Arc::clone(&provider.commits);
        let egress = mpp_proxy(provider, |builder| builder).await;

        let response = proxied_client(&egress)
            .post(format!("{origin}/paid"))
            .header(MPP_REQUEST_ID, "child-controlled")
            .body("same-body")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::OK);
        {
            let observations = observations.lock().unwrap();
            assert_eq!(observations.len(), 2);
            assert_eq!(observations[0].0, observations[1].0);
            assert_ne!(observations[0].0, "child-controlled");
            assert_eq!(observations[0].1, b"same-body");
            assert_eq!(observations[1].1, b"same-body");
            assert!(!observations[0].2);
            assert!(observations[1].2);
            assert!(observations.iter().all(|observation| !observation.3));
        }
        assert_eq!(payments.load(Ordering::SeqCst), 1);
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn preserves_the_existing_default_payment_retry_policy() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/rotating-challenge",
            post(move || {
                let calls = Arc::clone(&route_calls);
                async move {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let challenge = challenge_header(&format!("challenge-{call}"));
                    (
                        AxumStatus::PAYMENT_REQUIRED,
                        [(WWW_AUTHENTICATE, challenge)],
                        "payment required",
                    )
                }
            }),
        ))
        .await;
        let provider = MockProvider::default();
        let payments = Arc::clone(&provider.payments);
        let rollbacks = Arc::clone(&provider.rollbacks);
        let egress = mpp_proxy(provider, |builder| builder).await;

        let response = proxied_client(&egress)
            .post(format!("{origin}/rotating-challenge"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::PAYMENT_REQUIRED);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            DEFAULT_MAX_PAYMENT_RETRIES + 1
        );
        assert_eq!(payments.load(Ordering::SeqCst), DEFAULT_MAX_PAYMENT_RETRIES);
        assert_eq!(
            rollbacks.load(Ordering::SeqCst),
            DEFAULT_MAX_PAYMENT_RETRIES
        );
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn oversized_challenge_body_fails_before_payment() {
        let challenge = challenge_header("oversized-challenge");
        let origin = spawn_origin(Router::new().route(
            "/paid",
            post(move || {
                let challenge = challenge.clone();
                async move {
                    (
                        AxumStatus::PAYMENT_REQUIRED,
                        [(WWW_AUTHENTICATE, challenge)],
                        "larger than four bytes",
                    )
                }
            }),
        ))
        .await;
        let provider = MockProvider::default();
        let payments = Arc::clone(&provider.payments);
        let egress = EgressProxy::builder()
            .allow_loopback_upstreams(true)
            .layer(TempoEgress::new(provider).max_payment_required_bytes(4))
            .spawn()
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .post(format!("{origin}/paid"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::BAD_GATEWAY);
        assert_eq!(payments.load(Ordering::SeqCst), 0);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn promise_all_style_paid_requests_run_concurrently() {
        const REQUESTS: usize = 141;
        const CONCURRENCY: usize = 141;

        super::super::resource::ensure_mpp_file_descriptor_capacity().unwrap();
        let challenge = challenge_header("shared-challenge");
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/paid",
            post(move |request: Request| {
                let challenge = challenge.clone();
                let calls = Arc::clone(&route_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if request.headers().contains_key("authorization") {
                        (AxumStatus::OK, "paid").into_response()
                    } else {
                        (
                            AxumStatus::PAYMENT_REQUIRED,
                            [(WWW_AUTHENTICATE, challenge)],
                            "payment required",
                        )
                            .into_response()
                    }
                }
            }),
        ))
        .await;
        let provider = MockProvider::default().with_payment_delay();
        let payments = Arc::clone(&provider.payments);
        let commits = Arc::clone(&provider.commits);
        let maximum = Arc::clone(&provider.maximum);
        let egress = mpp_proxy(provider, |builder| {
            builder
                .max_concurrent_requests(CONCURRENCY)
                .max_concurrent_connections(CONCURRENCY * 2)
        })
        .await;
        let client = proxied_client(&egress);

        let statuses = stream::iter(0..REQUESTS)
            .map(|_| {
                let client = client.clone();
                let url = format!("{origin}/paid");
                async move { client.post(url).send().await.unwrap().status() }
            })
            .buffer_unordered(CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        assert!(statuses.iter().all(AxumStatus::is_success));
        assert_eq!(calls.load(Ordering::SeqCst), REQUESTS * 2);
        assert_eq!(payments.load(Ordering::SeqCst), REQUESTS);
        assert_eq!(commits.load(Ordering::SeqCst), REQUESTS);
        assert!(
            maximum.load(Ordering::SeqCst) > 1,
            "payment handling was unexpectedly serialized"
        );
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pays_ten_thousand_bounded_parallel_requests_exactly_once() {
        const REQUESTS: usize = 10_000;
        const CONCURRENCY: usize = 8;

        let challenge = challenge_header("shared-stress-challenge");
        let calls = Arc::new(AtomicUsize::new(0));
        let observations = Arc::new(Mutex::new(Vec::with_capacity(REQUESTS * 2)));
        let origin = spawn_origin(Router::new().route(
            "/paid",
            post({
                let calls = Arc::clone(&calls);
                let observations = Arc::clone(&observations);
                move |request: Request<Body>| {
                    let calls = Arc::clone(&calls);
                    let challenge = challenge.clone();
                    let observations = Arc::clone(&observations);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let request_id = request
                            .headers()
                            .get(MPP_REQUEST_ID)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned();
                        let paid = request.headers().contains_key("authorization");
                        let body = axum::body::to_bytes(request.into_body(), 1024)
                            .await
                            .unwrap();
                        observations
                            .lock()
                            .unwrap()
                            .push((request_id, body.to_vec(), paid));
                        if paid {
                            (AxumStatus::OK, "paid").into_response()
                        } else {
                            (
                                AxumStatus::PAYMENT_REQUIRED,
                                [(WWW_AUTHENTICATE, challenge)],
                                "payment required",
                            )
                                .into_response()
                        }
                    }
                }
            }),
        ))
        .await;
        let provider = MockProvider::default();
        let payments = Arc::clone(&provider.payments);
        let commits = Arc::clone(&provider.commits);
        let rollbacks = Arc::clone(&provider.rollbacks);
        let egress = mpp_proxy(provider, |builder| {
            builder.max_concurrent_requests(CONCURRENCY)
        })
        .await;
        let client = proxied_client(&egress);

        let statuses = stream::iter(0..REQUESTS)
            .map(|index| {
                let client = client.clone();
                let url = format!("{origin}/paid");
                async move {
                    client
                        .post(url)
                        .body(format!("body-{index}"))
                        .send()
                        .await
                        .unwrap()
                        .status()
                }
            })
            .buffer_unordered(CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        assert!(statuses.iter().all(AxumStatus::is_success));
        assert_eq!(calls.load(Ordering::SeqCst), REQUESTS * 2);
        assert_eq!(payments.load(Ordering::SeqCst), REQUESTS);
        assert_eq!(commits.load(Ordering::SeqCst), REQUESTS);
        assert_eq!(rollbacks.load(Ordering::SeqCst), 0);
        {
            let mut observations = observations.lock().unwrap().clone();
            observations.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(observations.len(), REQUESTS * 2);
            for pair in observations.as_chunks::<2>().0 {
                assert_eq!(pair[0].0, pair[1].0);
                assert_eq!(pair[0].1, pair[1].1);
                assert_ne!(pair[0].2, pair[1].2);
            }
        }
        egress.shutdown().await.unwrap();
    }
}
