//! Embedded HTTP egress proxy that handles MPP payment challenges.
//!
//! The proxy listens only on loopback. Callers pass [`MppEgress::environment`]
//! to untrusted child processes; the payment provider and its signing material
//! remain in the embedding process.

use std::{
    collections::HashSet,
    ffi::OsString,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Limited};
use hudsucker::{
    Body, HttpContext, HttpHandler, Proxy, RequestOrResponse,
    certificate_authority::CertificateAuthority,
    hyper::{
        Method, Request, Response, StatusCode,
        header::{
            CONNECTION, HOST, HeaderValue, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION,
            TRANSFER_ENCODING, UPGRADE,
        },
        http::uri::Authority,
    },
    rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
        IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType, string::Ia5String,
    },
    rustls::{
        ServerConfig,
        crypto::aws_lc_rs,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use mpp::client::{AcceptPaymentPolicy, ClientEvent, ClientEvents, Fetch, PaymentProvider};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{Semaphore, oneshot},
    task::JoinHandle,
};
use tracing::Instrument as _;

const DEFAULT_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_PAYMENT_RETRIES: usize = 4;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 32;
const CA_FILENAME: &str = "mpp-egress-ca.pem";
const MPP_REQUEST_ID: &str = "mpp-request-id";

/// Policy owned by one embedded proxy instance.
#[derive(Clone, Debug)]
pub struct EgressPolicy {
    /// Maximum replayable request-body size accepted from a child process.
    pub max_request_bytes: usize,
    /// Maximum number of distinct payment challenges accepted for one request.
    pub max_payment_retries: usize,
    /// Maximum number of requests concurrently forwarded to origin services.
    ///
    /// Additional child requests wait locally before receiving a payment
    /// challenge, so challenge lifetimes are not consumed behind payment-state
    /// serialization.
    pub max_concurrent_requests: usize,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_payment_retries: DEFAULT_MAX_PAYMENT_RETRIES,
            max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
        }
    }
}

/// A running loopback proxy and its ephemeral certificate authority.
pub struct MppEgress {
    proxy_url: String,
    proxy_password: String,
    proxy_authorization: String,
    temp_dir: TempDir,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), hudsucker::Error>>>,
}

impl MppEgress {
    /// Starts an MPP-aware HTTP(S) proxy on an ephemeral loopback port.
    ///
    /// The provider is never shared with child processes. It should enforce
    /// currency-specific spend/deposit limits before signing a credential.
    ///
    /// # Errors
    ///
    /// Returns an error if the policy is invalid or the proxy listener, HTTP
    /// client, temporary CA, or background proxy task cannot be initialized.
    pub async fn start<P>(provider: P, policy: EgressPolicy) -> Result<Self, EgressError>
    where
        P: PaymentProvider + 'static,
    {
        if policy.max_request_bytes == 0 {
            return Err(EgressError::InvalidPolicy(
                "max_request_bytes must be greater than zero",
            ));
        }
        if policy.max_payment_retries == 0 {
            return Err(EgressError::InvalidPolicy(
                "max_payment_retries must be greater than zero",
            ));
        }
        if policy.max_concurrent_requests == 0 {
            return Err(EgressError::InvalidPolicy(
                "max_concurrent_requests must be greater than zero",
            ));
        }

        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .map_err(EgressError::Bind)?;
        let address = listener.local_addr().map_err(EgressError::LocalAddress)?;
        let (authority, certificate_pem) = ephemeral_authority()?;
        let temp_dir = tempfile::Builder::new()
            .prefix("nanocodex-mpp-egress-")
            .tempdir()
            .map_err(EgressError::TempDir)?;
        std::fs::write(temp_dir.path().join(CA_FILENAME), certificate_pem)
            .map_err(EgressError::WriteCertificate)?;

        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(EgressError::Client)?;
        let proxy_password = random_proxy_password();
        let proxy_authorization = format!(
            "Basic {}",
            STANDARD.encode(format!("nanocodex:{proxy_password}"))
        );
        let proxy_url = format!("http://nanocodex:{proxy_password}@{address}");
        let origin_permits = Arc::new(Semaphore::new(policy.max_concurrent_requests));
        let handler = PaymentHandler {
            provider,
            client,
            policy,
            origin_permits,
            proxy_authorization: proxy_authorization.clone(),
            authenticated_clients: Arc::new(Mutex::new(HashSet::new())),
            request_id_prefix: random_identifier(),
            request_ids: Arc::new(AtomicU64::new(1)),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(authority)
            .with_rustls_connector(aws_lc_rs::default_provider())
            .with_http_handler(handler)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .build()?;
        let task = tokio::spawn(proxy.start());

        Ok(Self {
            proxy_url,
            proxy_password,
            proxy_authorization,
            temp_dir,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        })
    }

    /// Returns the HTTP proxy URL listened on by this instance.
    #[must_use]
    pub fn proxy_url(&self) -> String {
        self.proxy_url.clone()
    }

    /// Returns environment overrides for curl and common HTTP runtimes.
    ///
    /// These values should be applied only to tool child processes, not to the
    /// embedding process, so model/control-plane traffic is not intercepted.
    #[must_use]
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        let proxy = OsString::from(self.proxy_url());
        let certificate = self.temp_dir.path().join(CA_FILENAME).into_os_string();
        [
            ("http_proxy", proxy.clone()),
            ("https_proxy", proxy.clone()),
            ("HTTP_PROXY", proxy.clone()),
            ("HTTPS_PROXY", proxy),
            ("no_proxy", OsString::new()),
            ("NO_PROXY", OsString::new()),
            ("CURL_CA_BUNDLE", certificate.clone()),
            ("SSL_CERT_FILE", certificate.clone()),
            ("REQUESTS_CA_BUNDLE", certificate.clone()),
            ("NODE_EXTRA_CA_CERTS", certificate),
            (
                "NANOCODEX_MPP_EGRESS_PASSWORD",
                OsString::from(&self.proxy_password),
            ),
            (
                "NANOCODEX_MPP_EGRESS_AUTHORIZATION",
                OsString::from(&self.proxy_authorization),
            ),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), value))
        .collect()
    }

    /// Stops accepting traffic and waits for active proxy connections to drain.
    ///
    /// # Errors
    ///
    /// Returns an error if the proxy task fails or cannot be joined.
    pub async fn shutdown(mut self) -> Result<(), EgressError> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.map_err(EgressError::Join)??;
        }
        Ok(())
    }

    /// Path to the public ephemeral CA certificate.
    #[must_use]
    pub fn certificate_path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join(CA_FILENAME)
    }
}

impl Drop for MppEgress {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct PaymentHandler<P> {
    provider: P,
    client: reqwest::Client,
    policy: EgressPolicy,
    origin_permits: Arc<Semaphore>,
    proxy_authorization: String,
    authenticated_clients: Arc<Mutex<HashSet<SocketAddr>>>,
    request_id_prefix: String,
    request_ids: Arc<AtomicU64>,
}

impl<P> HttpHandler for PaymentHandler<P>
where
    P: PaymentProvider + 'static,
{
    async fn handle_request(
        &mut self,
        context: &HttpContext,
        mut request: Request<Body>,
    ) -> RequestOrResponse {
        let request_id = self.request_ids.fetch_add(1, Ordering::Relaxed);
        let logical_request_id = format!("{}-{request_id}", self.request_id_prefix);
        let span = tracing::info_span!(
            target: "mpp_egress",
            "mpp.egress.request",
            request.id = request_id,
            mpp.request.id = %logical_request_id,
            client.address = %context.client_addr,
            http.request.method = %request.method(),
            url.full = %request.uri(),
            request.upgrade = is_upgrade(&request),
        );
        async move {
            tracing::info!(
                target: "mpp_egress",
                content_kind = "mpp.egress.request.headers",
                content = ?request.headers(),
                "trace content"
            );
            if !self.authorize(context, &request) {
                tracing::warn!(
                    target: "mpp_egress",
                    stage = "mpp.egress.proxy_authentication.rejected",
                    http.response.status_code = StatusCode::PROXY_AUTHENTICATION_REQUIRED.as_u16(),
                    "MPP egress rejected an unauthenticated client"
                );
                return proxy_authentication_required().into();
            }
            tracing::info!(
                target: "mpp_egress",
                stage = "mpp.egress.proxy_authentication.accepted",
                "MPP egress authenticated its child client"
            );
            request.headers_mut().remove(PROXY_AUTHORIZATION);
            if request.method() == Method::CONNECT || is_upgrade(&request) {
                tracing::info!(
                    target: "mpp_egress",
                    stage = "mpp.egress.tunnel.forwarded",
                    "MPP egress forwarded a protocol tunnel without payment handling"
                );
                return request.into();
            }

            match self.forward(request, &logical_request_id).await {
                Ok(response) => response.into(),
                Err(ForwardError::RequestTooLarge) => {
                    tracing::warn!(
                        target: "mpp_egress",
                        stage = "mpp.egress.request.failed",
                        failure.kind = "request_too_large",
                        http.response.status_code = StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
                        "MPP egress rejected an unreplayable request body"
                    );
                    error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body exceeds the MPP egress replay limit",
                    )
                    .into()
                }
                Err(error) => {
                    tracing::warn!(
                        target: "mpp_egress",
                        stage = "mpp.egress.request.failed",
                        failure.kind = "payment_or_forwarding",
                        http.response.status_code = StatusCode::BAD_GATEWAY.as_u16(),
                        error = %error,
                        "MPP egress request failed"
                    );
                    error_response(StatusCode::BAD_GATEWAY, &error.to_string()).into()
                }
            }
        }
        .instrument(span)
        .await
    }
}

impl<P> PaymentHandler<P>
where
    P: PaymentProvider,
{
    fn authorize(&self, context: &HttpContext, request: &Request<Body>) -> bool {
        if self
            .authenticated_clients
            .lock()
            .is_ok_and(|clients| clients.contains(&context.client_addr))
        {
            return true;
        }
        let authorized = request
            .headers()
            .get(PROXY_AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == self.proxy_authorization);
        if authorized && let Ok(mut clients) = self.authenticated_clients.lock() {
            clients.insert(context.client_addr);
        }
        authorized
    }

    async fn forward(
        &self,
        request: Request<Body>,
        logical_request_id: &str,
    ) -> Result<Response<Body>, ForwardError> {
        let (mut parts, body) = request.into_parts();
        let body = Limited::new(body, self.policy.max_request_bytes)
            .collect()
            .await
            .map_err(|_| ForwardError::RequestTooLarge)?
            .to_bytes();
        record_body_content("mpp.egress.request.body", &body);
        remove_hop_by_hop_request_headers(&mut parts.headers);
        parts.headers.insert(
            MPP_REQUEST_ID,
            HeaderValue::from_str(logical_request_id).map_err(ForwardError::RequestId)?,
        );
        let queued = self.origin_permits.available_permits() == 0;
        if queued {
            tracing::info!(
                target: "mpp_egress",
                stage = "mpp.egress.origin.request.queued",
                origin.max_concurrent_requests = self.policy.max_concurrent_requests,
                "MPP egress queued the request before contacting its origin"
            );
        }
        let _origin_permit = self
            .origin_permits
            .acquire()
            .await
            .map_err(|_| ForwardError::Unavailable)?;
        tracing::info!(
            target: "mpp_egress",
            stage = "mpp.egress.origin.request.started",
            http.request.body.size = body.len(),
            payment.max_retries = self.policy.max_payment_retries,
            request.queued = queued,
            "MPP egress sent the original request"
        );

        let builder = self
            .client
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body);
        let events = payment_events();
        let _subscription = events.on_any(|event| async move {
            record_payment_event(&event);
        });
        let response = builder
            .send_with_payment_options_max_retries(
                &self.provider,
                &AcceptPaymentPolicy::Never,
                events,
                self.policy.max_payment_retries,
            )
            .await
            .map_err(ForwardError::Payment)?;

        let status = response.status();
        tracing::info!(
            target: "mpp_egress",
            stage = "mpp.egress.request.completed",
            http.response.status_code = status.as_u16(),
            "MPP egress completed the request"
        );
        Ok(convert_response(response, &tracing::Span::current()))
    }
}

fn payment_events() -> ClientEvents {
    ClientEvents::default()
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

fn record_body_content(kind: &'static str, body: &[u8]) {
    if let Ok(content) = std::str::from_utf8(body) {
        tracing::info!(
            target: "mpp_egress",
            content_kind = kind,
            content,
            "trace content"
        );
    } else {
        tracing::info!(
            target: "mpp_egress",
            content_kind = kind,
            content = ?body,
            "trace content"
        );
    }
}

fn convert_response(response: reqwest::Response, span: &tracing::Span) -> Response<Body> {
    let status = response.status();
    let version = response.version();
    let mut headers = response.headers().clone();
    remove_hop_by_hop_response_headers(&mut headers);
    span.in_scope(|| {
        tracing::info!(
            target: "mpp_egress",
            content_kind = "mpp.egress.response.headers",
            content = ?headers,
            "trace content"
        );
    });
    let content_span = span.clone();
    let mut chunk_index = 0_u64;
    let body = Body::from_stream(
        response
            .bytes_stream()
            .map_ok(move |chunk| {
                content_span.in_scope(|| {
                    if let Ok(content) = std::str::from_utf8(&chunk) {
                        tracing::info!(
                            target: "mpp_egress",
                            content_kind = "mpp.egress.response.body",
                            response.chunk.index = chunk_index,
                            response.chunk.size = chunk.len(),
                            content,
                            "trace content"
                        );
                    } else {
                        tracing::info!(
                            target: "mpp_egress",
                            content_kind = "mpp.egress.response.body",
                            response.chunk.index = chunk_index,
                            response.chunk.size = chunk.len(),
                            content = ?chunk.as_ref(),
                            "trace content"
                        );
                    }
                });
                chunk_index = chunk_index.saturating_add(1);
                chunk
            })
            .map_err(|_| hudsucker::Error::Unknown),
    );
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = headers;
    response
}

fn is_upgrade(request: &Request<Body>) -> bool {
    request.headers().contains_key(UPGRADE)
        || request
            .headers()
            .get(CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn remove_hop_by_hop_request_headers(headers: &mut hudsucker::hyper::HeaderMap) {
    remove_connection_named_headers(headers);
    for name in [
        CONNECTION,
        HOST,
        PROXY_AUTHORIZATION,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
}

fn remove_hop_by_hop_response_headers(headers: &mut hudsucker::hyper::HeaderMap) {
    remove_connection_named_headers(headers);
    for name in [CONNECTION, TRANSFER_ENCODING, UPGRADE] {
        headers.remove(name);
    }
}

fn remove_connection_named_headers(headers: &mut hudsucker::hyper::HeaderMap) {
    let names = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| {
            name.trim()
                .parse::<hudsucker::hyper::header::HeaderName>()
                .ok()
        })
        .collect::<Vec<_>>();
    for name in names {
        headers.remove(name);
    }
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(message.to_owned()));
    *response.status_mut() = status;
    response
}

fn proxy_authentication_required() -> Response<Body> {
    let mut response = error_response(
        StatusCode::PROXY_AUTHENTICATION_REQUIRED,
        "proxy authentication required",
    );
    response.headers_mut().insert(
        PROXY_AUTHENTICATE,
        hudsucker::hyper::header::HeaderValue::from_static("Basic realm=\"nanocodex-mpp-egress\""),
    );
    response
}

fn random_proxy_password() -> String {
    random_identifier()
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

struct EphemeralAuthority {
    issuer: Issuer<'static, KeyPair>,
    private_key: PrivateKeyDer<'static>,
    cache: Mutex<std::collections::HashMap<Authority, Arc<ServerConfig>>>,
}

impl CertificateAuthority for EphemeralAuthority {
    async fn gen_server_config(&self, authority: &Authority) -> Arc<ServerConfig> {
        if let Ok(cache) = self.cache.lock()
            && let Some(config) = cache.get(authority)
        {
            return Arc::clone(config);
        }

        let mut params = CertificateParams::default();
        params.serial_number = Some(rand::random::<u64>().into());
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, authority.host());
        params.distinguished_name = distinguished_name;
        params
            .subject_alt_names
            .push(authority.host().parse::<IpAddr>().map_or_else(
                |_| {
                    SanType::DnsName(
                        Ia5String::try_from(authority.host())
                            .expect("HTTP authority host must be a valid DNS IA5 string"),
                    )
                },
                SanType::IpAddress,
            ));
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let certificate = params
            .signed_by(self.issuer.key(), &self.issuer)
            .expect("valid CA parameters must sign an ephemeral leaf certificate");
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate)],
                self.private_key.clone_key(),
            )
            .expect("generated leaf certificate and private key must match");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);

        if let Ok(mut cache) = self.cache.lock()
            && cache.len() < 1_024
        {
            cache.insert(authority.clone(), Arc::clone(&config));
        }
        config
    }
}

fn ephemeral_authority() -> Result<(EphemeralAuthority, String), EgressError> {
    let key_pair = KeyPair::generate().map_err(EgressError::Certificate)?;
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "Nanocodex ephemeral MPP egress");
    params.distinguished_name = distinguished_name;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let certificate = params
        .self_signed(&key_pair)
        .map_err(EgressError::Certificate)?;
    let certificate_pem = certificate.pem();
    let private_key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
    let issuer =
        Issuer::from_ca_cert_pem(&certificate_pem, key_pair).map_err(EgressError::Certificate)?;
    Ok((
        EphemeralAuthority {
            issuer,
            private_key,
            cache: Mutex::new(std::collections::HashMap::new()),
        },
        certificate_pem,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("invalid MPP egress policy: {0}")]
    InvalidPolicy(&'static str),
    #[error("failed to bind the MPP egress listener")]
    Bind(#[source] std::io::Error),
    #[error("failed to read the MPP egress listener address")]
    LocalAddress(#[source] std::io::Error),
    #[error("failed to create the ephemeral MPP egress directory")]
    TempDir(#[source] std::io::Error),
    #[error("failed to write the ephemeral MPP egress CA certificate")]
    WriteCertificate(#[source] std::io::Error),
    #[error("failed to generate the ephemeral MPP egress CA")]
    Certificate(#[source] hudsucker::rcgen::Error),
    #[error("failed to build the MPP egress HTTP client")]
    Client(#[source] reqwest::Error),
    #[error("MPP egress proxy failed")]
    Proxy(#[from] hudsucker::Error),
    #[error("MPP egress proxy task failed")]
    Join(#[source] tokio::task::JoinError),
}

#[derive(Debug, thiserror::Error)]
enum ForwardError {
    #[error("request body is too large to replay")]
    RequestTooLarge,
    #[error("failed to encode the MPP request ID")]
    RequestId(#[source] hudsucker::hyper::header::InvalidHeaderValue),
    #[error("MPP egress stopped while the request was queued")]
    Unavailable,
    #[error("MPP payment request failed: {0}")]
    Payment(#[source] mpp::client::HttpError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        body::Body as AxumBody,
        extract::Request,
        http::{StatusCode as AxumStatus, header::WWW_AUTHENTICATE},
        response::IntoResponse,
        routing::{get, post},
    };
    use futures_util::future::join_all;
    use mpp::{
        Base64UrlJson, MppError, PaymentChallenge, PaymentCredential, PaymentPayload,
        format_www_authenticate,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

    struct LogWriter(Arc<StdMutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(Arc::clone(&self.0))
        }
    }

    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct MockProvider {
        payments: Arc<AtomicUsize>,
    }

    impl PaymentProvider for MockProvider {
        fn supports(&self, method: &str, intent: &str) -> bool {
            method == "test" && intent == "charge"
        }

        async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
            self.payments.fetch_add(1, Ordering::SeqCst);
            Ok(PaymentCredential::new(
                challenge.to_echo(),
                PaymentPayload::hash("test-payment"),
            ))
        }
    }

    async fn spawn_origin(app: Router) -> String {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    fn challenge_header() -> String {
        let request = Base64UrlJson::from_value(&serde_json::json!({
            "amount": "1",
            "currency": "test"
        }))
        .unwrap();
        format_www_authenticate(&PaymentChallenge::new(
            "challenge-1",
            "test.local",
            "test",
            "charge",
            request,
        ))
        .unwrap()
    }

    fn proxied_client(egress: &MppEgress) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(egress.proxy_url()).unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn rejects_clients_without_the_ephemeral_proxy_credential() {
        let egress = MppEgress::start(MockProvider::default(), EgressPolicy::default())
            .await
            .unwrap();
        let mut proxy: reqwest::Url = egress.proxy_url().parse().unwrap();
        proxy.set_username("").unwrap();
        proxy.set_password(None).unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy).unwrap())
            .build()
            .unwrap();

        let response = client.get("http://example.invalid/").send().await.unwrap();

        assert_eq!(response.status(), AxumStatus::PROXY_AUTHENTICATION_REQUIRED);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn passes_unpaid_http_responses_through() {
        let origin = spawn_origin(Router::new().route("/plain", get(|| async { "plain" }))).await;
        let egress = MppEgress::start(MockProvider::default(), EgressPolicy::default())
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .get(format!("{origin}/plain"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::OK);
        assert_eq!(response.text().await.unwrap(), "plain");
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn queues_excess_requests_before_contacting_the_origin() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(Semaphore::new(0));
        let app = Router::new().route(
            "/bounded",
            get({
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                let started = Arc::clone(&started);
                let gate = Arc::clone(&gate);
                move || {
                    let active = Arc::clone(&active);
                    let maximum = Arc::clone(&maximum);
                    let started = Arc::clone(&started);
                    let gate = Arc::clone(&gate);
                    async move {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(current, Ordering::SeqCst);
                        started.notify_one();
                        let permit = gate.acquire().await.unwrap();
                        permit.forget();
                        active.fetch_sub(1, Ordering::SeqCst);
                        "bounded"
                    }
                }
            }),
        );
        let origin = spawn_origin(app).await;
        let egress = MppEgress::start(
            MockProvider::default(),
            EgressPolicy {
                max_concurrent_requests: 3,
                ..EgressPolicy::default()
            },
        )
        .await
        .unwrap();
        let client = proxied_client(&egress);
        let requests = (0..12).map(|_| {
            let client = client.clone();
            let url = format!("{origin}/bounded");
            tokio::spawn(async move { client.get(url).send().await.unwrap().status() })
        });
        let requests = requests.collect::<Vec<_>>();

        while maximum.load(Ordering::SeqCst) < 3 {
            started.notified().await;
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 3);
        assert_eq!(active.load(Ordering::SeqCst), 3);

        gate.add_permits(12);
        let statuses = join_all(requests)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert!(statuses.iter().all(|status| *status == AxumStatus::OK));
        assert_eq!(maximum.load(Ordering::SeqCst), 3);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pays_and_replays_the_exact_request_body() {
        let calls = Arc::new(AtomicUsize::new(0));
        let request_ids = Arc::new(StdMutex::new(Vec::new()));
        let calls_for_route = Arc::clone(&calls);
        let request_ids_for_route = Arc::clone(&request_ids);
        let challenge = challenge_header();
        let app = Router::new().route(
            "/paid",
            post(move |request: Request<AxumBody>| {
                let calls = Arc::clone(&calls_for_route);
                let request_ids = Arc::clone(&request_ids_for_route);
                let challenge = challenge.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    request_ids.lock().unwrap().push(
                        request
                            .headers()
                            .get(MPP_REQUEST_ID)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    );
                    let paid = request.headers().contains_key("authorization");
                    let body = axum::body::to_bytes(request.into_body(), 1024)
                        .await
                        .unwrap();
                    if paid {
                        assert_eq!(body.as_ref(), b"same-body");
                        (AxumStatus::OK, "paid").into_response()
                    } else {
                        assert_eq!(body.as_ref(), b"same-body");
                        (
                            AxumStatus::PAYMENT_REQUIRED,
                            [(WWW_AUTHENTICATE, challenge)],
                            "payment required",
                        )
                            .into_response()
                    }
                }
            }),
        );
        let origin = spawn_origin(app).await;
        let provider = MockProvider::default();
        let payments = Arc::clone(&provider.payments);
        let egress = MppEgress::start(provider, EgressPolicy::default())
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .post(format!("{origin}/paid"))
            .body("same-body")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::OK);
        assert_eq!(response.text().await.unwrap(), "paid");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(payments.load(Ordering::SeqCst), 1);
        {
            let request_ids = request_ids.lock().unwrap();
            assert_eq!(request_ids.len(), 2);
            assert_eq!(request_ids[0], request_ids[1]);
        }
        egress.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_the_complete_payment_retry_sequence() {
        let logs = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(logs.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        let challenge = challenge_header();
        let app = Router::new().route(
            "/paid",
            post(move |request: Request<AxumBody>| {
                let challenge = challenge.clone();
                async move {
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
        );
        let origin = spawn_origin(app).await;
        let egress = MppEgress::start(MockProvider::default(), EgressPolicy::default())
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .post(format!("{origin}/paid"))
            .body("audit-body")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "paid");
        egress.shutdown().await.unwrap();

        let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        let stages = [
            "mpp.egress.origin.request.started",
            "mpp.egress.challenge.received",
            "mpp.egress.credential.created",
            "mpp.egress.payment.response",
            "mpp.egress.request.completed",
        ];
        let mut previous = 0;
        for stage in stages {
            let position = output[previous..].find(stage).unwrap() + previous;
            previous = position;
        }
        assert!(output.contains("mpp.egress.request.body"));
        assert!(output.contains("audit-body"));
        assert!(output.contains("mpp.egress.response.body"));
        assert!(output.contains("paid"));
    }

    #[tokio::test]
    async fn rejects_request_bodies_above_the_replay_limit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_route = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/upload",
            post(move || {
                let calls = Arc::clone(&calls_for_route);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "unexpected"
                }
            }),
        ))
        .await;
        let egress = MppEgress::start(
            MockProvider::default(),
            EgressPolicy {
                max_request_bytes: 4,
                ..EgressPolicy::default()
            },
        )
        .await
        .unwrap();

        let response = proxied_client(&egress)
            .post(format!("{origin}/upload"))
            .body("too-large")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::PAYLOAD_TOO_LARGE);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_environment_points_at_the_ephemeral_proxy_and_ca() {
        let egress = MppEgress::start(MockProvider::default(), EgressPolicy::default())
            .await
            .unwrap();
        let environment = egress.environment();
        let value = |name: &str| {
            environment
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
                .unwrap()
        };

        assert_eq!(value("https_proxy"), OsString::from(egress.proxy_url()));
        assert!(value("NO_PROXY").is_empty());
        assert_eq!(
            std::path::PathBuf::from(value("CURL_CA_BUNDLE")),
            egress.certificate_path()
        );
        assert!(egress.certificate_path().is_file());
        assert_eq!(
            value("NANOCODEX_MPP_EGRESS_PASSWORD"),
            OsString::from(&egress.proxy_password)
        );
        assert_eq!(
            value("NANOCODEX_MPP_EGRESS_AUTHORIZATION"),
            OsString::from(&egress.proxy_authorization)
        );
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "manual public-network HTTPS smoke"]
    async fn live_https_mitm_smoke() {
        let egress = MppEgress::start(MockProvider::default(), EgressPolicy::default())
            .await
            .unwrap();
        let environment = egress.environment();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("curl")
                .args(["--fail", "--silent", "--show-error", "https://example.com/"])
                .envs(environment)
                .output()
        })
        .await
        .unwrap()
        .unwrap();

        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Example Domain"));
        egress.shutdown().await.unwrap();
    }
}
