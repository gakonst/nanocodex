//! Composable authenticated HTTP egress proxy with host-owned secrets.
//!
//! [`EgressProxy`] owns the loopback proxy, TLS interception, bounded request
//! forwarding, and lifecycle. Applications compose protocol behavior through
//! ordered [`EgressLayer`] implementations. Nanocodex uses that seam for MPP
//! payment and replay while keeping wallet material in the host process.
//! [`SecretEgress`] is an optional fail-closed layer that replaces public
//! placeholders only for an authorized destination, method, and path.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod policy;
mod secret;

pub use policy::{
    EgressPolicy, EgressPolicyBuilder, EgressPolicyRule, EgressPolicyRuleBuilder, HeaderAllowlist,
    HeaderAllowlistBuilder, PolicyConfigError, PolicyMode,
};
pub use secret::{
    SecretConfigError, SecretEgress, SecretEgressBuilder, SecretFormat, SecretRef, SecretResolver,
    SecretResolverError, SecretRule, SecretRuleBuilder, StaticSecretResolver, UnmatchedEgress,
};

/// HTTP types used to define egress rules without depending on the proxy backend.
pub mod http {
    pub use hudsucker::hyper::{HeaderMap, Method, Uri, header};
}

/// The intentional extension seam for an application-defined [`EgressLayer`].
///
/// Reexports keep middleware versioning owned by this crate while allowing an
/// application to adapt an existing middleware, as the Nanocodex binary does
/// for Tempo MPP.
pub mod middleware {
    pub use async_trait::async_trait;
    pub use http::Extensions;
    pub use reqwest::{Request, Response, StatusCode};
    pub use reqwest_middleware::{Error, Middleware, Next, Result};

    pub use super::ResponseBodyTooLarge;

    /// Requests bounded buffering of a selected origin response body.
    ///
    /// Layers that need to inspect response headers and then retry through
    /// [`Next`] can use this hook to drain the first response, preserving
    /// origin connection reuse. When several layers request the same status,
    /// the smallest limit wins.
    pub fn buffer_response_body(extensions: &mut Extensions, status: StatusCode, max_bytes: usize) {
        super::request_response_buffer(extensions, status, max_bytes);
    }
}

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hudsucker::{
    Body, HttpContext, HttpHandler, Proxy, RequestOrResponse,
    certificate_authority::CertificateAuthority,
    hyper::{
        Method, Request, Response, StatusCode,
        body::Body as HttpBody,
        header::{
            CONNECTION, CONTENT_LENGTH, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER,
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
        crypto::{CryptoProvider, ring},
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use reqwest::ResponseBuilderExt as _;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
    task::JoinHandle,
};
use tracing::Instrument as _;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

const DEFAULT_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 128;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 128;
const DEFAULT_MAX_IDLE_CONNECTIONS_PER_ORIGIN: usize = 32;
const DEFAULT_REQUEST_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const CA_FILENAME: &str = "egress-ca.pem";
const PROXY_ENVIRONMENT_NAMES: [&str; 13] = [
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
    "CURL_CA_BUNDLE",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "GIT_SSL_CAINFO",
];

/// Child-process configuration contributed by an egress layer.
///
/// This type deliberately omits `Debug` so child capabilities are not exposed
/// through incidental formatting. Values remain available through explicit
/// collection APIs.
#[derive(Clone, Default)]
pub struct EgressEnvironment {
    entries: Vec<(OsString, OsString)>,
}

impl EgressEnvironment {
    /// Creates an environment from owned names and values.
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Iterates over variable names and values.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&OsStr, &OsStr)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
    }

    /// Returns the value for one exact variable name.
    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        let name = name.as_ref();
        self.entries
            .iter()
            .find_map(|(candidate, value)| (candidate == name).then_some(value.as_os_str()))
    }

    /// Returns the number of variables.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the environment contains no variables.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl IntoIterator for EgressEnvironment {
    type Item = (OsString, OsString);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// Host-visible metadata passed through egress layers.
///
/// CONNECT authorization sees immutable destination, method, and header
/// metadata before the proxy opens an origin connection. This type deliberately
/// omits `Debug` because headers may contain child-supplied credentials.
#[derive(Clone)]
pub struct EgressRequest {
    method: Method,
    uri: hudsucker::hyper::Uri,
    headers: hudsucker::hyper::HeaderMap,
}

impl EgressRequest {
    /// Creates request metadata for a policy or deterministic test.
    #[must_use]
    pub const fn new(
        method: Method,
        uri: hudsucker::hyper::Uri,
        headers: hudsucker::hyper::HeaderMap,
    ) -> Self {
        Self {
            method,
            uri,
            headers,
        }
    }

    fn from_request(request: &Request<Body>) -> Self {
        Self::new(
            request.method().clone(),
            request.uri().clone(),
            request.headers().clone(),
        )
    }

    /// Returns the immutable HTTP method.
    #[must_use]
    pub const fn method(&self) -> &Method {
        &self.method
    }

    /// Returns the immutable absolute URI or CONNECT authority.
    #[must_use]
    pub const fn uri(&self) -> &hudsucker::hyper::Uri {
        &self.uri
    }

    /// Returns the child-supplied CONNECT headers.
    #[must_use]
    pub const fn headers(&self) -> &hudsucker::hyper::HeaderMap {
        &self.headers
    }
}

/// Failure returned while an egress layer authorizes a request.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum EgressLayerError {
    /// The authenticated child does not hold authority for this request.
    #[error("egress request denied by host policy")]
    Denied,
    /// The request cannot be matched safely against policy.
    #[error("egress request is invalid")]
    InvalidRequest,
    /// Policy or credential resolution is temporarily unavailable.
    #[error("egress layer is unavailable")]
    Unavailable,
}

/// One independently composable outbound HTTP behavior.
///
/// Layers receive ordinary forwarded requests in attachment order.
#[async_trait]
pub trait EgressLayer: Send + Sync + 'static {
    /// Handles one replayable outbound request and optionally invokes the rest
    /// of the stack with [`Next::run`].
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response>;

    /// Authorizes one CONNECT tunnel before any origin connection is opened.
    async fn authorize_connect(&self, _request: &EgressRequest) -> Result<(), EgressLayerError> {
        Ok(())
    }

    /// Returns public child configuration contributed by this layer.
    fn environment(&self) -> EgressEnvironment {
        EgressEnvironment::default()
    }

    /// Returns whether this layer may request bounded response buffering.
    ///
    /// Override this together with [`middleware::buffer_response_body`] so the
    /// proxy installs the buffering middleware only when the stack needs it.
    fn uses_response_buffering(&self) -> bool {
        false
    }
}

/// Private transport policy owned by one embedded proxy instance.
#[derive(Clone, Debug)]
struct ProxyPolicy {
    max_request_bytes: usize,
    max_buffered_request_bytes: usize,
    max_concurrent_requests: usize,
    max_concurrent_connections: usize,
    max_idle_connections_per_origin: usize,
    request_setup_timeout: Duration,
    graceful_shutdown_timeout: Duration,
    allow_loopback_upstreams: bool,
}

impl Default for ProxyPolicy {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_buffered_request_bytes: DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
            max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
            max_concurrent_connections: DEFAULT_MAX_CONCURRENT_CONNECTIONS,
            max_idle_connections_per_origin: DEFAULT_MAX_IDLE_CONNECTIONS_PER_ORIGIN,
            request_setup_timeout: DEFAULT_REQUEST_SETUP_TIMEOUT,
            graceful_shutdown_timeout: DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT,
            allow_loopback_upstreams: false,
        }
    }
}

#[derive(Clone, Default)]
struct BufferedResponseBodies(Vec<(reqwest::StatusCode, usize)>);

fn request_response_buffer(
    extensions: &mut ::http::Extensions,
    status: reqwest::StatusCode,
    max_bytes: usize,
) {
    let requested = extensions.get_or_insert_default::<BufferedResponseBodies>();
    if let Some((_, configured)) = requested
        .0
        .iter_mut()
        .find(|(candidate, _)| *candidate == status)
    {
        *configured = (*configured).min(max_bytes);
    } else {
        requested.0.push((status, max_bytes));
    }
}

struct BufferRequestedResponses {
    timeout: Duration,
}

#[async_trait]
impl Middleware for BufferRequestedResponses {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let response = next.run(request, extensions).await?;
        let Some(limit) = extensions
            .get::<BufferedResponseBodies>()
            .and_then(|requested| {
                requested
                    .0
                    .iter()
                    .find(|(status, _)| *status == response.status())
                    .map(|(_, limit)| *limit)
            })
        else {
            return Ok(response);
        };
        let status = response.status();
        let response_url = response.url().clone();
        let response: ::http::Response<reqwest::Body> = response.into();
        let (parts, body) = response.into_parts();
        let body = tokio::time::timeout(self.timeout, Limited::new(body, limit).collect())
            .await
            .map_err(|_| {
                reqwest_middleware::Error::middleware(ResponseBodyReadTimeout {
                    status,
                    timeout: self.timeout,
                })
            })?
            .map_err(|error| {
                if error.downcast_ref::<LengthLimitError>().is_some() {
                    reqwest_middleware::Error::middleware(ResponseBodyTooLarge { status, limit })
                } else {
                    reqwest_middleware::Error::middleware(ResponseBodyReadError {
                        status,
                        source: error,
                    })
                }
            })?
            .to_bytes();
        record_body_content("egress.proxy.buffered.response.body", &body);
        let mut response = ::http::Response::builder()
            .status(parts.status)
            .version(parts.version)
            .url(response_url)
            .body(body)
            .map_err(reqwest_middleware::Error::middleware)?;
        *response.headers_mut() = parts.headers;
        response.extensions_mut().extend(parts.extensions);
        Ok(response.into())
    }
}

#[derive(Clone, Copy)]
struct GuardedResolver {
    allow_loopback: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let allow_loopback = self.allow_loopback;
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .collect::<Vec<_>>();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| denied_upstream_ip(address.ip(), allow_loopback))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "upstream address denied by egress network policy",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn denied_upstream_ip(address: IpAddr, allow_loopback: bool) -> bool {
    let address = match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
        address => address,
    };
    if !allow_loopback && address.is_loopback() {
        return true;
    }
    match address {
        IpAddr::V4(address) => address.octets() == [169, 254, 169, 254],
        IpAddr::V6(address) => address.segments() == [0xfd00, 0x00ec, 0, 0, 0, 0, 0, 0x0254],
    }
}

struct OriginResponseHeadersTimeout {
    timeout: Duration,
}

#[async_trait]
impl Middleware for OriginResponseHeadersTimeout {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        tokio::time::timeout(self.timeout, next.run(request, extensions))
            .await
            .map_err(|_| {
                reqwest_middleware::Error::middleware(OriginResponseHeadersTimedOut {
                    timeout: self.timeout,
                })
            })?
    }
}

#[derive(Debug, thiserror::Error)]
#[error("timed out after {timeout:?} while waiting for origin response headers")]
struct OriginResponseHeadersTimedOut {
    timeout: Duration,
}

/// A selected response exceeded the bounded buffering requested by a layer.
#[derive(Debug, thiserror::Error)]
#[error("{status} response body exceeds the configured {limit}-byte egress layer limit")]
pub struct ResponseBodyTooLarge {
    status: reqwest::StatusCode,
    limit: usize,
}

impl ResponseBodyTooLarge {
    /// Returns the selected response status.
    #[must_use]
    pub const fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    /// Returns the configured maximum body size in bytes.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Debug, thiserror::Error)]
#[error("failed to read the selected {status} response body")]
struct ResponseBodyReadError {
    status: reqwest::StatusCode,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

#[derive(Debug, thiserror::Error)]
#[error("timed out after {timeout:?} while reading the selected {status} response body")]
struct ResponseBodyReadTimeout {
    status: reqwest::StatusCode,
    timeout: Duration,
}

struct LayerMiddleware(Arc<dyn EgressLayer>);

#[async_trait]
impl Middleware for LayerMiddleware {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        self.0.handle(request, extensions, next).await
    }
}

/// A client-facing view of one running egress proxy.
///
/// The route contains a short-lived authenticated proxy URL and must not be
/// logged. It remains valid only while the [`EgressProxy`] that produced it is
/// alive. Callers can project the same route into a host child or an isolated
/// runtime by choosing where that runtime receives the public CA.
pub struct EgressRoute<'a> {
    proxy_url: &'a str,
    ca_certificate_pem: &'a [u8],
    layer_environment: &'a EgressEnvironment,
}

impl EgressRoute<'_> {
    /// Returns the authenticated HTTP proxy URL.
    #[must_use]
    pub const fn proxy_url(&self) -> &str {
        self.proxy_url
    }

    /// Returns the public ephemeral CA in PEM encoding.
    #[must_use]
    pub const fn ca_certificate_pem(&self) -> &[u8] {
        self.ca_certificate_pem
    }

    /// Returns proxy, CA, and layer variables for one child runtime.
    ///
    /// `certificate_path` is the path at which that runtime can read
    /// [`Self::ca_certificate_pem`]. It may be a host, guest, or container
    /// path. A caller using a separate network namespace must also make the
    /// loopback proxy reachable there; libkrun TSI guests share host socket
    /// reachability. The returned values include the short-lived proxy
    /// capability and therefore must not be logged.
    #[must_use]
    pub fn environment(&self, certificate_path: impl AsRef<OsStr>) -> EgressEnvironment {
        let proxy = OsString::from(self.proxy_url);
        let certificate = certificate_path.as_ref().to_owned();
        let mut environment = [
            ("http_proxy", proxy.clone()),
            ("https_proxy", proxy.clone()),
            ("all_proxy", proxy.clone()),
            ("HTTP_PROXY", proxy.clone()),
            ("HTTPS_PROXY", proxy.clone()),
            ("ALL_PROXY", proxy),
            ("no_proxy", OsString::new()),
            ("NO_PROXY", OsString::new()),
            ("CURL_CA_BUNDLE", certificate.clone()),
            ("SSL_CERT_FILE", certificate.clone()),
            ("REQUESTS_CA_BUNDLE", certificate.clone()),
            ("NODE_EXTRA_CA_CERTS", certificate.clone()),
            ("GIT_SSL_CAINFO", certificate),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), value))
        .collect::<Vec<_>>();
        environment.extend(self.layer_environment.clone());
        EgressEnvironment::new(environment)
    }
}

/// A running authenticated loopback proxy and its ephemeral certificate authority.
pub struct EgressProxy {
    proxy_url: String,
    ca_certificate_pem: Vec<u8>,
    ca_certificate_path: PathBuf,
    layer_environment: EgressEnvironment,
    _temp_dir: TempDir,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), hudsucker::Error>>>,
    #[cfg(test)]
    _test_permit: tokio::sync::OwnedSemaphorePermit,
}

impl EgressProxy {
    /// Starts a composable proxy builder with no outbound layers.
    #[must_use]
    pub fn builder() -> EgressProxyBuilder {
        EgressProxyBuilder::new()
    }

    async fn start(
        policy: ProxyPolicy,
        layers: Vec<Arc<dyn EgressLayer>>,
    ) -> Result<Self, EgressError> {
        #[cfg(test)]
        let test_permit = test_proxy_permit().await;
        if policy.max_request_bytes == 0 {
            return Err(EgressError::ZeroMaxRequestBytes);
        }
        if policy.max_request_bytes > u32::MAX as usize {
            return Err(EgressError::MaxRequestBytesTooLarge {
                configured: policy.max_request_bytes,
                limit: u32::MAX as usize,
            });
        }
        if policy.max_buffered_request_bytes < policy.max_request_bytes {
            return Err(EgressError::BufferedRequestBudgetTooSmall {
                configured: policy.max_buffered_request_bytes,
                minimum: policy.max_request_bytes,
            });
        }
        if policy.max_buffered_request_bytes > Semaphore::MAX_PERMITS {
            return Err(EgressError::BufferedRequestBudgetTooLarge {
                configured: policy.max_buffered_request_bytes,
                limit: Semaphore::MAX_PERMITS,
            });
        }
        if policy.max_concurrent_requests == 0 {
            return Err(EgressError::ZeroMaxConcurrentRequests);
        }
        if policy.max_concurrent_requests > Semaphore::MAX_PERMITS {
            return Err(EgressError::MaxConcurrentRequestsTooLarge {
                configured: policy.max_concurrent_requests,
                limit: Semaphore::MAX_PERMITS,
            });
        }
        if policy.request_setup_timeout.is_zero() {
            return Err(EgressError::ZeroRequestSetupTimeout);
        }
        let max_concurrent_connections = NonZeroUsize::new(policy.max_concurrent_connections)
            .ok_or(EgressError::ZeroMaxConcurrentConnections)?;
        if max_concurrent_connections.get() > Semaphore::MAX_PERMITS {
            return Err(EgressError::MaxConcurrentConnectionsTooLarge {
                configured: max_concurrent_connections.get(),
                limit: Semaphore::MAX_PERMITS,
            });
        }

        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .map_err(EgressError::Bind)?;
        let address = listener.local_addr().map_err(EgressError::LocalAddress)?;
        let (authority, certificate_pem) = ephemeral_authority()?;
        let temp_dir = tempfile::Builder::new()
            .prefix("nanocodex-egress-")
            .tempdir()
            .map_err(EgressError::TempDir)?;
        let ca_certificate_path = temp_dir.path().join(CA_FILENAME);
        std::fs::write(&ca_certificate_path, &certificate_pem)
            .map_err(EgressError::WriteCertificate)?;

        install_default_rustls_crypto_provider();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(policy.request_setup_timeout)
            .dns_resolver(GuardedResolver {
                allow_loopback: policy.allow_loopback_upstreams,
            })
            .pool_max_idle_per_host(policy.max_idle_connections_per_origin)
            .build()
            .map_err(EgressError::Client)?;
        let mut client_builder = ClientBuilder::new(client);
        let layer_environment = collect_layer_environment(&layers)?;
        for layer in &layers {
            client_builder = client_builder.with(LayerMiddleware(Arc::clone(layer)));
        }
        if layers.iter().any(|layer| layer.uses_response_buffering()) {
            client_builder = client_builder.with(BufferRequestedResponses {
                timeout: policy.request_setup_timeout,
            });
        }
        // Keep this middleware innermost so the deadline covers each actual
        // origin exchange without cancelling outer payment or secret logic.
        client_builder = client_builder.with(OriginResponseHeadersTimeout {
            timeout: policy.request_setup_timeout,
        });
        let client = client_builder.build();
        let proxy_password = random_proxy_password();
        let proxy_authorization = format!(
            "Basic {}",
            STANDARD.encode(format!("nanocodex:{proxy_password}"))
        );
        let proxy_url = format!("http://nanocodex:{proxy_password}@{address}");
        let origin_permits = Arc::new(Semaphore::new(policy.max_concurrent_requests));
        let buffered_request_bytes = Arc::new(Semaphore::new(policy.max_buffered_request_bytes));
        let request_setup_timeout = policy.request_setup_timeout;
        let graceful_shutdown_timeout = policy.graceful_shutdown_timeout;
        let handler = ProxyHandler {
            client,
            policy,
            layers,
            origin_permits,
            buffered_request_bytes,
            authentication: ProxyAuthentication {
                authorization: proxy_authorization.into(),
                tunnel_authenticated: false,
            },
            request_ids: Arc::new(AtomicU64::new(1)),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(authority)
            .with_rustls_connector(ring::default_provider())
            .with_max_concurrent_connections(max_concurrent_connections)
            .with_unrecognized_connect_tunneling(false)
            .with_connect_setup_timeout(request_setup_timeout)
            .with_http_handler(handler)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .with_graceful_shutdown_timeout(graceful_shutdown_timeout)
            .build()?;
        let task = tokio::spawn(proxy.start());

        Ok(Self {
            proxy_url,
            ca_certificate_pem: certificate_pem.into_bytes(),
            ca_certificate_path,
            layer_environment: EgressEnvironment::new(layer_environment),
            _temp_dir: temp_dir,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
            #[cfg(test)]
            _test_permit: test_permit,
        })
    }

    /// Returns the authenticated HTTP proxy URL.
    ///
    /// The URL contains a short-lived bearer credential and must not be logged.
    #[must_use]
    pub fn proxy_url(&self) -> String {
        self.proxy_url.clone()
    }

    /// Borrows a route that can be projected into a host or isolated runtime.
    #[must_use]
    pub const fn route(&self) -> EgressRoute<'_> {
        EgressRoute {
            proxy_url: self.proxy_url.as_str(),
            ca_certificate_pem: self.ca_certificate_pem.as_slice(),
            layer_environment: &self.layer_environment,
        }
    }

    /// Returns environment overrides for curl and common HTTP runtimes.
    ///
    /// These values contain the proxy bearer capability. They should be applied
    /// only to tool child processes, not logged or installed in the embedding
    /// process, so model/control-plane traffic is not intercepted.
    #[must_use]
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        self.route()
            .environment(self.ca_certificate_path.as_os_str())
            .into_iter()
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
    pub fn ca_certificate_path(&self) -> &Path {
        &self.ca_certificate_path
    }
}

#[cfg(test)]
async fn test_proxy_permit() -> tokio::sync::OwnedSemaphorePermit {
    static PERMITS: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(PERMITS.get_or_init(|| Arc::new(Semaphore::new(4))))
        .acquire_owned()
        .await
        .unwrap_or_else(|error| unreachable!("test proxy semaphore closed: {error}"))
}

/// Builder for one bounded proxy and its ordered outbound layers.
pub struct EgressProxyBuilder {
    policy: ProxyPolicy,
    layers: Vec<Arc<dyn EgressLayer>>,
}

impl EgressProxyBuilder {
    /// Creates a builder with bounded defaults and direct HTTP forwarding.
    #[must_use]
    pub fn new() -> Self {
        Self {
            policy: ProxyPolicy::default(),
            layers: Vec::new(),
        }
    }

    /// Sets the maximum replayable request-body size accepted from a child.
    #[must_use]
    pub const fn max_request_bytes(mut self, max_bytes: usize) -> Self {
        self.policy.max_request_bytes = max_bytes;
        self
    }

    /// Sets the aggregate reservation budget for replayable request bodies.
    ///
    /// Known body lengths reserve their exact size; unknown lengths reserve
    /// the per-request maximum. This budget must fit one maximum-sized body.
    #[must_use]
    pub const fn max_buffered_request_bytes(mut self, max_bytes: usize) -> Self {
        self.policy.max_buffered_request_bytes = max_bytes;
        self
    }

    /// Sets the maximum number of requests concurrently forwarded to origins.
    ///
    /// Additional child requests wait locally before entering outbound layers.
    #[must_use]
    pub const fn max_concurrent_requests(mut self, max_requests: usize) -> Self {
        self.policy.max_concurrent_requests = max_requests;
        self
    }

    /// Sets the maximum number of accepted child proxy connections.
    ///
    /// Additional clients remain in the listener backlog before consuming a
    /// process file descriptor.
    #[must_use]
    pub const fn max_concurrent_connections(mut self, max_connections: usize) -> Self {
        self.policy.max_concurrent_connections = max_connections;
        self
    }

    /// Sets the maximum idle origin connections retained per origin.
    #[must_use]
    pub const fn max_idle_connections_per_origin(mut self, max_connections: usize) -> Self {
        self.policy.max_idle_connections_per_origin = max_connections;
        self
    }

    /// Sets the bounded deadline for request-body reads, CONNECT setup,
    /// origin connection establishment, and origin response headers.
    ///
    /// Successfully established streaming response bodies are not timed out.
    #[must_use]
    pub const fn request_setup_timeout(mut self, timeout: Duration) -> Self {
        self.policy.request_setup_timeout = timeout;
        self
    }

    /// Sets how long active connections may drain during proxy shutdown.
    #[must_use]
    pub const fn graceful_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.policy.graceful_shutdown_timeout = timeout;
        self
    }

    /// Allows explicit or DNS-resolved loopback origins.
    ///
    /// Loopback is denied by default to close credential-bearing SSRF and DNS
    /// rebinding paths. Enable this only for a deliberate local development or
    /// test origin. Cloud metadata addresses remain denied unconditionally.
    #[must_use]
    pub const fn allow_loopback_upstreams(mut self, allow: bool) -> Self {
        self.policy.allow_loopback_upstreams = allow;
        self
    }

    /// Appends one independently owned outbound behavior.
    ///
    /// Layers run in attachment order on the initial request. A layer controls
    /// whether and how the remaining stack runs through [`Next::run`].
    #[must_use]
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: EgressLayer,
    {
        self.layers.push(Arc::new(layer));
        self
    }

    /// Starts the proxy and its owned background task.
    ///
    /// # Errors
    ///
    /// Returns a typed initialization or proxy error.
    pub async fn spawn(self) -> Result<EgressProxy, EgressError> {
        EgressProxy::start(self.policy, self.layers).await
    }
}

impl Default for EgressProxyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        // Detach so the server can deliver shutdown to accepted connections;
        // its configured graceful timeout still bounds the remaining task.
        let _ = self.task.take();
    }
}

#[derive(Clone)]
struct ProxyHandler {
    client: ClientWithMiddleware,
    policy: ProxyPolicy,
    layers: Vec<Arc<dyn EgressLayer>>,
    origin_permits: Arc<Semaphore>,
    buffered_request_bytes: Arc<Semaphore>,
    authentication: ProxyAuthentication,
    request_ids: Arc<AtomicU64>,
}

#[derive(Clone)]
struct ProxyAuthentication {
    authorization: Arc<str>,
    // The proxy backend carries the mutated CONNECT handler into only that
    // intercepted stream; clones for unrelated client connections remain false.
    tunnel_authenticated: bool,
}

impl ProxyAuthentication {
    fn authorize(&mut self, request: &Request<Body>) -> bool {
        let has_authorization = request
            .headers()
            .get(PROXY_AUTHORIZATION)
            .is_some_and(|value| value.as_bytes() == self.authorization.as_bytes());
        if has_authorization {
            if request.method() == Method::CONNECT {
                self.tunnel_authenticated = true;
            }
            return true;
        }
        self.tunnel_authenticated
    }
}

impl HttpHandler for ProxyHandler {
    async fn handle_request(
        &mut self,
        context: &HttpContext,
        mut request: Request<Body>,
    ) -> RequestOrResponse {
        let request_id = self.request_ids.fetch_add(1, Ordering::Relaxed);
        let span = tracing::info_span!(
            target: "nanocodex_egress",
            "egress.proxy.request",
            request.id = request_id,
            mpp.request.id = tracing::field::Empty,
            client.address = %context.client_addr,
            http.request.method = %request.method(),
            url.full = %request.uri(),
            request.upgrade = is_upgrade(&request),
        );
        async move {
            tracing::info!(
                target: "nanocodex_egress",
                content_kind = "egress.proxy.request.headers",
                content = header_trace_content(request.headers()),
                "trace content"
            );
            if !self.authentication.authorize(&request) {
                tracing::warn!(
                    target: "nanocodex_egress",
                    stage = "egress.proxy.authentication.rejected",
                    http.response.status_code = StatusCode::PROXY_AUTHENTICATION_REQUIRED.as_u16(),
                    "egress proxy rejected an unauthenticated client"
                );
                return proxy_authentication_required().into();
            }
            tracing::info!(
                target: "nanocodex_egress",
                stage = "egress.proxy.authentication.accepted",
                "egress proxy authenticated its child client"
            );
            request.headers_mut().remove(PROXY_AUTHORIZATION);
            if is_upgrade(&request) {
                tracing::warn!(
                    target: "nanocodex_egress",
                    stage = "egress.proxy.protocol_upgrade.rejected",
                    http.response.status_code = StatusCode::BAD_REQUEST.as_u16(),
                    "egress proxy rejected an unsupported protocol upgrade"
                );
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "protocol upgrades are not supported by the egress proxy",
                )
                .into();
            }
            if request.method() == Method::CONNECT {
                let metadata = EgressRequest::from_request(&request);
                for layer in &self.layers {
                    if let Err(error) = layer.authorize_connect(&metadata).await {
                        return layer_error_response(error).into();
                    }
                }
                tracing::info!(
                    target: "nanocodex_egress",
                    stage = "egress.proxy.tunnel.forwarded",
                    "egress proxy forwarded a protocol tunnel"
                );
                return request.into();
            }

            match self.forward(request).await {
                Ok(response) => response.into(),
                Err(ForwardError::RequestTooLarge) => {
                    tracing::warn!(
                        target: "nanocodex_egress",
                        stage = "egress.proxy.request.failed",
                        failure.kind = "request_too_large",
                        http.response.status_code = StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
                        "egress proxy rejected an unreplayable request body"
                    );
                    error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body exceeds the egress proxy replay limit",
                    )
                    .into()
                }
                Err(ForwardError::RequestBody(_)) => {
                    tracing::warn!(
                        target: "nanocodex_egress",
                        stage = "egress.proxy.request.failed",
                        failure.kind = "request_body",
                        http.response.status_code = StatusCode::BAD_REQUEST.as_u16(),
                        "egress proxy could not read the child request body"
                    );
                    error_response(StatusCode::BAD_REQUEST, "failed to read request body").into()
                }
                Err(ForwardError::RequestBodyTimeout(timeout)) => {
                    tracing::warn!(
                        target: "nanocodex_egress",
                        stage = "egress.proxy.request.failed",
                        failure.kind = "request_body_timeout",
                        http.response.status_code = StatusCode::REQUEST_TIMEOUT.as_u16(),
                        ?timeout,
                        "egress proxy timed out while reading the child request body"
                    );
                    error_response(StatusCode::REQUEST_TIMEOUT, "request body read timed out")
                        .into()
                }
                Err(ForwardError::DeniedUpstream) => error_response(
                    StatusCode::FORBIDDEN,
                    "upstream address denied by egress network policy",
                )
                .into(),
                Err(error) => {
                    if let ForwardError::Layer(error) = &error
                        && let Some(error) = middleware_layer_error(error)
                    {
                        return layer_error_response(*error).into();
                    }
                    if let ForwardError::Layer(error) = &error
                        && middleware_origin_timeout(error).is_some()
                    {
                        return error_response(
                            StatusCode::GATEWAY_TIMEOUT,
                            "origin response headers timed out",
                        )
                        .into();
                    }
                    tracing::warn!(
                        target: "nanocodex_egress",
                        stage = "egress.proxy.request.failed",
                        failure.kind = "payment_or_forwarding",
                        http.response.status_code = StatusCode::BAD_GATEWAY.as_u16(),
                        error = %error,
                        "egress proxy request failed"
                    );
                    error_response(StatusCode::BAD_GATEWAY, &error.to_string()).into()
                }
            }
        }
        .instrument(span)
        .await
    }
}

impl ProxyHandler {
    async fn forward(&self, request: Request<Body>) -> Result<Response<Body>, ForwardError> {
        if request
            .uri()
            .host()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|address| {
                denied_upstream_ip(address, self.policy.allow_loopback_upstreams)
            })
        {
            return Err(ForwardError::DeniedUpstream);
        }
        let queued = self.origin_permits.available_permits() == 0;
        if queued {
            tracing::info!(
                target: "nanocodex_egress",
                stage = "egress.proxy.origin.request.queued",
                origin.max_concurrent_requests = self.policy.max_concurrent_requests,
                "egress proxy queued the request before buffering its body"
            );
        }
        let origin_permit = Arc::clone(&self.origin_permits)
            .acquire_owned()
            .await
            .map_err(|_| ForwardError::Unavailable)?;
        let (mut parts, body) = request.into_parts();
        let reservation = match body.size_hint().upper() {
            Some(upper) if upper > self.policy.max_request_bytes as u64 => {
                return Err(ForwardError::RequestTooLarge);
            }
            Some(upper) => usize::try_from(upper).map_err(|_| ForwardError::RequestTooLarge)?,
            None => self.policy.max_request_bytes,
        };
        let reservation = u32::try_from(reservation).map_err(|_| ForwardError::RequestTooLarge)?;
        let _buffered_body_permit = if reservation == 0 {
            None
        } else {
            Some(
                self.buffered_request_bytes
                    .acquire_many(reservation)
                    .await
                    .map_err(|_| ForwardError::Unavailable)?,
            )
        };
        let body = tokio::time::timeout(
            self.policy.request_setup_timeout,
            Limited::new(body, self.policy.max_request_bytes).collect(),
        )
        .await
        .map_err(|_| ForwardError::RequestBodyTimeout(self.policy.request_setup_timeout))?
        .map_err(|error| {
            if error.downcast_ref::<LengthLimitError>().is_some() {
                ForwardError::RequestTooLarge
            } else {
                ForwardError::RequestBody(error)
            }
        })?
        .to_bytes();
        record_body_content("egress.proxy.request.body", &body);
        remove_hop_by_hop_request_headers(&mut parts.headers);
        tracing::info!(
            target: "nanocodex_egress",
            stage = "egress.proxy.origin.request.started",
            http.request.body.size = body.len(),
            request.queued = queued,
            "egress proxy sent the original request"
        );

        let builder = self
            .client
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body);
        let response = builder.send().await.map_err(ForwardError::Layer)?;

        let status = response.status();
        tracing::info!(
            target: "nanocodex_egress",
            stage = "egress.proxy.request.completed",
            http.response.status_code = status.as_u16(),
            "egress proxy completed the request"
        );
        Ok(convert_response(
            response,
            &tracing::Span::current(),
            origin_permit,
        ))
    }
}

fn record_body_content(kind: &'static str, body: &[u8]) {
    if !tracing::enabled!(target: "nanocodex_egress", tracing::Level::INFO) {
        return;
    }
    if let Ok(content) = std::str::from_utf8(body) {
        tracing::info!(
            target: "nanocodex_egress",
            content_kind = kind,
            content,
            "trace content"
        );
    } else {
        tracing::info!(
            target: "nanocodex_egress",
            content_kind = kind,
            content = ?body,
            "trace content"
        );
    }
}

fn header_trace_content(headers: &hudsucker::hyper::HeaderMap) -> String {
    let entries = headers
        .iter()
        .map(|(name, value)| {
            let (encoding, value) = match std::str::from_utf8(value.as_bytes()) {
                Ok(value) => ("utf8", value.to_owned()),
                Err(_) => ("base64", STANDARD.encode(value.as_bytes())),
            };
            serde_json::json!({
                "name": name.as_str(),
                "encoding": encoding,
                "value": value,
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

fn convert_response(
    mut response: reqwest::Response,
    span: &tracing::Span,
    origin_permit: OwnedSemaphorePermit,
) -> Response<Body> {
    let status = response.status();
    let version = response.version();
    let mut headers = std::mem::take(response.headers_mut());
    remove_hop_by_hop_response_headers(&mut headers);
    let trace_content = tracing::enabled!(target: "nanocodex_egress", tracing::Level::INFO);
    if trace_content {
        span.in_scope(|| {
            tracing::info!(
                target: "nanocodex_egress",
                content_kind = "egress.proxy.response.headers",
                content = header_trace_content(&headers),
                "trace content"
            );
        });
    }
    let stream = response.bytes_stream();
    let body = if trace_content {
        let content_span = span.clone();
        let error_span = span.clone();
        let mut chunk_index = 0_u64;
        Body::from_stream(
            stream
                .map_ok(move |chunk| {
                    let _origin_permit = &origin_permit;
                    content_span.in_scope(|| {
                        if let Ok(content) = std::str::from_utf8(&chunk) {
                            tracing::info!(
                                target: "nanocodex_egress",
                                content_kind = "egress.proxy.response.body",
                                response.chunk.index = chunk_index,
                                response.chunk.size = chunk.len(),
                                content,
                                "trace content"
                            );
                        } else {
                            tracing::info!(
                                target: "nanocodex_egress",
                                content_kind = "egress.proxy.response.body",
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
                .map_err(move |error| response_stream_error(&error_span, error)),
        )
    } else {
        let error_span = span.clone();
        Body::from_stream(
            stream
                .map_ok(move |chunk| {
                    let _origin_permit = &origin_permit;
                    chunk
                })
                .map_err(move |error| response_stream_error(&error_span, error)),
        )
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = headers;
    response
}

fn response_stream_error(span: &tracing::Span, error: reqwest::Error) -> hudsucker::Error {
    span.in_scope(|| {
        tracing::warn!(
            target: "nanocodex_egress",
            stage = "egress.proxy.response.body.failed",
            error = ?error,
            "egress proxy failed while streaming the origin response body"
        );
    });
    hudsucker::Error::Unknown
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
        CONTENT_LENGTH,
        HOST,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

fn remove_hop_by_hop_response_headers(headers: &mut hudsucker::hyper::HeaderMap) {
    remove_connection_named_headers(headers);
    for name in [
        CONNECTION,
        PROXY_AUTHENTICATE,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
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

fn middleware_layer_error(error: &reqwest_middleware::Error) -> Option<&EgressLayerError> {
    match error {
        reqwest_middleware::Error::Middleware(error) => error.downcast_ref(),
        reqwest_middleware::Error::Reqwest(_) => None,
    }
}

fn middleware_origin_timeout(
    error: &reqwest_middleware::Error,
) -> Option<&OriginResponseHeadersTimedOut> {
    match error {
        reqwest_middleware::Error::Middleware(error) => error.downcast_ref(),
        reqwest_middleware::Error::Reqwest(_) => None,
    }
}

fn collect_layer_environment(
    layers: &[Arc<dyn EgressLayer>],
) -> Result<BTreeMap<OsString, OsString>, EgressError> {
    let mut environment = BTreeMap::<OsString, OsString>::new();
    for layer in layers {
        for (name, value) in layer.environment() {
            if !valid_environment_name(&name) {
                return Err(EgressError::InvalidEnvironmentName(name));
            }
            if value.as_encoded_bytes().contains(&0) {
                return Err(EgressError::InvalidEnvironmentValue(name));
            }
            if is_proxy_environment_name(&name) {
                return Err(EgressError::EnvironmentConflict(name));
            }
            if let Some((existing_name, existing_value)) = environment
                .iter()
                .find(|(candidate, _)| environment_names_equal(candidate, &name))
            {
                if existing_name != &name || existing_value != &value {
                    return Err(EgressError::EnvironmentConflict(name));
                }
                continue;
            }
            environment.insert(name, value);
        }
    }
    Ok(environment)
}

fn valid_environment_name(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    !bytes.is_empty() && !bytes.contains(&0) && !bytes.contains(&b'=')
}

fn is_proxy_environment_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        PROXY_ENVIRONMENT_NAMES
            .iter()
            .any(|owned| name.eq_ignore_ascii_case(owned))
    })
}

fn environment_names_equal(left: &OsStr, right: &OsStr) -> bool {
    match (left.to_str(), right.to_str()) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => left == right,
    }
}

fn layer_error_response(error: EgressLayerError) -> Response<Body> {
    let status = match error {
        EgressLayerError::Denied => StatusCode::FORBIDDEN,
        EgressLayerError::InvalidRequest => StatusCode::BAD_REQUEST,
        EgressLayerError::Unavailable => StatusCode::BAD_GATEWAY,
    };
    tracing::warn!(
        target: "nanocodex_egress",
        stage = "egress.proxy.layer.rejected",
        failure.kind = %error,
        http.response.status_code = status.as_u16(),
        "egress layer rejected the request"
    );
    error_response(status, &error.to_string())
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
        hudsucker::hyper::header::HeaderValue::from_static("Basic realm=\"nanocodex-egress\""),
    );
    response
}

fn random_proxy_password() -> String {
    random_identifier()
}

fn install_default_rustls_crypto_provider() {
    if CryptoProvider::get_default().is_none() {
        drop(ring::default_provider().install_default());
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

struct EphemeralAuthority {
    issuer: Issuer<'static, KeyPair>,
    private_key: PrivateKeyDer<'static>,
    provider: Arc<CryptoProvider>,
    fallback_server_config: Arc<ServerConfig>,
    cache: Mutex<std::collections::HashMap<Authority, Arc<ServerConfig>>>,
}

impl CertificateAuthority for EphemeralAuthority {
    async fn gen_server_config(&self, authority: &Authority) -> Arc<ServerConfig> {
        if let Ok(cache) = self.cache.lock()
            && let Some(config) = cache.get(authority)
        {
            return Arc::clone(config);
        }

        let config = match leaf_server_config(
            &self.issuer,
            &self.private_key,
            Arc::clone(&self.provider),
            authority.host(),
        ) {
            Ok(config) => Arc::new(config),
            Err(error) => {
                tracing::warn!(
                    target: "nanocodex_egress",
                    authority = %authority,
                    error = ?error,
                    "egress proxy could not issue a destination certificate"
                );
                Arc::clone(&self.fallback_server_config)
            }
        };

        if let Ok(mut cache) = self.cache.lock()
            && cache.len() < 1_024
        {
            cache.insert(authority.clone(), Arc::clone(&config));
        }
        config
    }
}

fn leaf_server_config(
    issuer: &Issuer<'static, KeyPair>,
    private_key: &PrivateKeyDer<'static>,
    provider: Arc<CryptoProvider>,
    host: &str,
) -> Result<ServerConfig, LeafCertificateError> {
    let mut params = CertificateParams::default();
    params.serial_number = Some(rand::random::<u64>().into());
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, host);
    params.distinguished_name = distinguished_name;
    let subject_name = match host.parse::<IpAddr>() {
        Ok(address) => SanType::IpAddress(address),
        Err(_) => SanType::DnsName(
            Ia5String::try_from(host).map_err(|_| LeafCertificateError::InvalidDnsName)?,
        ),
    };
    params.subject_alt_names.push(subject_name);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let certificate = params.signed_by(issuer.key(), issuer)?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate)],
            private_key.clone_key(),
        )?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

#[derive(Debug, thiserror::Error)]
enum LeafCertificateError {
    #[error("destination host is not a valid IA5 DNS name")]
    InvalidDnsName,
    #[error("failed to sign a destination certificate")]
    Certificate(#[from] hudsucker::rcgen::Error),
    #[error("failed to configure destination TLS")]
    Tls(#[from] hudsucker::rustls::Error),
}

fn ephemeral_authority() -> Result<(EphemeralAuthority, String), EgressError> {
    let key_pair = KeyPair::generate().map_err(EgressError::Certificate)?;
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "Nanocodex ephemeral egress");
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
    let provider = Arc::new(ring::default_provider());
    let fallback_server_config = leaf_server_config(
        &issuer,
        &private_key,
        Arc::clone(&provider),
        "invalid.invalid",
    )
    .map_err(|error| EgressError::LeafCertificate(Box::new(error)))?;
    Ok((
        EphemeralAuthority {
            issuer,
            private_key,
            provider,
            fallback_server_config: Arc::new(fallback_server_config),
            cache: Mutex::new(std::collections::HashMap::new()),
        },
        certificate_pem,
    ))
}

/// Failure to configure, start, run, or stop an egress proxy.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The replayable request-body limit was zero.
    #[error("egress max request bytes must be greater than zero")]
    ZeroMaxRequestBytes,
    /// The per-request body limit cannot be represented by the body budget.
    #[error("egress max request bytes {configured} exceeds the supported limit {limit}")]
    MaxRequestBytesTooLarge {
        /// Requested per-request limit.
        configured: usize,
        /// Maximum supported limit.
        limit: usize,
    },
    /// The aggregate body budget could not fit one maximum-sized request.
    #[error(
        "egress buffered request budget {configured} must be at least the per-request limit {minimum}"
    )]
    BufferedRequestBudgetTooSmall {
        /// Requested aggregate budget.
        configured: usize,
        /// Minimum valid aggregate budget.
        minimum: usize,
    },
    /// The aggregate body budget exceeded the runtime semaphore capacity.
    #[error("egress buffered request budget {configured} exceeds the supported limit {limit}")]
    BufferedRequestBudgetTooLarge {
        /// Requested aggregate budget.
        configured: usize,
        /// Maximum supported aggregate budget.
        limit: usize,
    },
    /// The forwarded-request concurrency limit was zero.
    #[error("egress max concurrent requests must be greater than zero")]
    ZeroMaxConcurrentRequests,
    /// The forwarded-request limit exceeded the runtime semaphore capacity.
    #[error("egress max concurrent requests {configured} exceeds the supported limit {limit}")]
    MaxConcurrentRequestsTooLarge {
        /// Requested forwarded-request limit.
        configured: usize,
        /// Maximum supported limit.
        limit: usize,
    },
    /// The accepted-connection concurrency limit was zero.
    #[error("egress max concurrent connections must be greater than zero")]
    ZeroMaxConcurrentConnections,
    /// The connection limit exceeded the runtime semaphore capacity.
    #[error("egress max concurrent connections {configured} exceeds the supported limit {limit}")]
    MaxConcurrentConnectionsTooLarge {
        /// Requested accepted-connection limit.
        configured: usize,
        /// Maximum supported limit.
        limit: usize,
    },
    /// The bounded setup timeout was zero.
    #[error("egress request setup timeout must be greater than zero")]
    ZeroRequestSetupTimeout,
    /// Two layers exported different values under the same child variable.
    #[error("egress layers conflict on child environment variable {0:?}")]
    EnvironmentConflict(OsString),
    /// A layer exported an invalid child environment name.
    #[error("egress layer exported an invalid child environment variable name {0:?}")]
    InvalidEnvironmentName(OsString),
    /// A layer exported a child environment value containing a NUL byte.
    #[error("egress layer exported a NUL-containing value for child environment variable {0:?}")]
    InvalidEnvironmentValue(OsString),
    /// The loopback listener could not be bound.
    #[error("failed to bind the egress proxy listener")]
    Bind(#[source] std::io::Error),
    /// The bound listener address could not be read.
    #[error("failed to read the egress proxy listener address")]
    LocalAddress(#[source] std::io::Error),
    /// The private ephemeral-CA directory could not be created.
    #[error("failed to create the ephemeral egress directory")]
    TempDir(#[source] std::io::Error),
    /// The public CA certificate could not be persisted for child runtimes.
    #[error("failed to write the ephemeral egress CA certificate")]
    WriteCertificate(#[source] std::io::Error),
    /// Ephemeral CA or leaf-certificate generation failed.
    #[error("failed to generate the ephemeral egress CA")]
    Certificate(#[source] hudsucker::rcgen::Error),
    /// The fallback destination certificate or TLS configuration failed.
    #[error("failed to configure ephemeral egress destination TLS")]
    LeafCertificate(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The origin-facing HTTP client could not be built.
    #[error("failed to build the egress HTTP client")]
    Client(#[source] reqwest::Error),
    /// The proxy rejected its configuration or failed while serving.
    #[error("egress proxy failed")]
    Proxy(#[from] hudsucker::Error),
    /// The background proxy task panicked or was cancelled unexpectedly.
    #[error("egress proxy task failed")]
    Join(#[source] tokio::task::JoinError),
}

#[derive(Debug, thiserror::Error)]
enum ForwardError {
    #[error("request body is too large to replay")]
    RequestTooLarge,
    #[error("failed to read the child request body")]
    RequestBody(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("timed out after {0:?} while reading the child request body")]
    RequestBodyTimeout(Duration),
    #[error("upstream address denied by egress network policy")]
    DeniedUpstream,
    #[error("egress proxy stopped while the request was queued")]
    Unavailable,
    #[error("egress layer or origin request failed: {0:#}")]
    Layer(#[source] reqwest_middleware::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use axum::{
        Router,
        body::{Body as AxumBody, Bytes as AxumBytes},
        extract::Request,
        http::StatusCode as AxumStatus,
        routing::{any, get, post},
    };
    use futures_util::future::join_all;

    #[test]
    fn proxy_authentication_only_retains_connect_tunnels() {
        let mut authentication = ProxyAuthentication {
            authorization: Arc::from("Basic test-credential"),
            tunnel_authenticated: false,
        };
        let authorized_request = |method| {
            Request::builder()
                .method(method)
                .header(PROXY_AUTHORIZATION, "Basic test-credential")
                .body(Body::empty())
                .unwrap()
        };
        let unauthenticated_request = || Request::new(Body::empty());

        assert!(authentication.authorize(&authorized_request(Method::GET)));
        assert!(!authentication.tunnel_authenticated);
        assert!(!authentication.authorize(&unauthenticated_request()));

        assert!(authentication.authorize(&authorized_request(Method::CONNECT)));
        assert!(authentication.tunnel_authenticated);
        assert!(authentication.clone().authorize(&unauthenticated_request()));

        let mut fresh_connection = ProxyAuthentication {
            authorization: Arc::clone(&authentication.authorization),
            tunnel_authenticated: false,
        };
        assert!(!fresh_connection.authorize(&unauthenticated_request()));
    }

    #[test]
    fn invalid_destination_names_fail_without_panicking() {
        let (authority, _) = ephemeral_authority().unwrap();
        let result = leaf_server_config(
            &authority.issuer,
            &authority.private_key,
            Arc::clone(&authority.provider),
            "invalid-\u{80}-name",
        );

        assert!(matches!(result, Err(LeafCertificateError::InvalidDnsName)));
    }

    #[test]
    fn layer_errors_preserve_the_middleware_error_chain() {
        #[derive(Debug, thiserror::Error)]
        #[error("required amount 104650 exceeds maximum 100000")]
        struct ChargeLimitExceeded;

        #[derive(Debug, thiserror::Error)]
        #[error("payment failed")]
        struct PaymentFailed(#[source] ChargeLimitExceeded);

        let error = ForwardError::Layer(reqwest_middleware::Error::middleware(PaymentFailed(
            ChargeLimitExceeded,
        )));

        assert_eq!(
            error.to_string(),
            "egress layer or origin request failed: payment failed: required amount 104650 exceeds maximum 100000"
        );
    }

    #[test]
    fn header_trace_content_is_lossless_json() {
        let mut headers = hudsucker::hyper::HeaderMap::new();
        headers.append("x-text", "first".parse().unwrap());
        headers.append("x-text", "second".parse().unwrap());
        headers.insert(
            "x-bytes",
            hudsucker::hyper::header::HeaderValue::from_bytes(&[0xff, 0x80]).unwrap(),
        );

        let content: serde_json::Value =
            serde_json::from_str(&header_trace_content(&headers)).unwrap();
        assert_eq!(
            content,
            serde_json::json!([
                {"name": "x-text", "encoding": "utf8", "value": "first"},
                {"name": "x-text", "encoding": "utf8", "value": "second"},
                {"name": "x-bytes", "encoding": "base64", "value": "/4A="},
            ])
        );
    }

    #[test]
    fn strips_fixed_and_connection_named_hop_headers() {
        let mut request = hudsucker::hyper::HeaderMap::new();
        request.insert(CONNECTION, "x-private-hop".parse().unwrap());
        for name in [
            "content-length",
            "host",
            "keep-alive",
            "proxy-authorization",
            "proxy-connection",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "x-private-hop",
        ] {
            request.insert(name, "value".parse().unwrap());
        }
        remove_hop_by_hop_request_headers(&mut request);
        assert!(request.is_empty());

        let mut response = hudsucker::hyper::HeaderMap::new();
        response.insert(CONNECTION, "x-private-hop".parse().unwrap());
        for name in [
            "keep-alive",
            "proxy-authenticate",
            "proxy-connection",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "x-private-hop",
        ] {
            response.insert(name, "value".parse().unwrap());
        }
        remove_hop_by_hop_response_headers(&mut response);
        assert!(response.is_empty());
    }

    struct HeaderLayer(&'static str);

    #[async_trait]
    impl EgressLayer for HeaderLayer {
        async fn handle(
            &self,
            mut request: reqwest::Request,
            extensions: &mut ::http::Extensions,
            next: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            let previous = request
                .headers()
                .get("x-egress-layers")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let value = if previous.is_empty() {
                self.0.to_owned()
            } else {
                format!("{previous},{}", self.0)
            };
            request
                .headers_mut()
                .insert("x-egress-layers", value.parse().unwrap());
            next.run(request, extensions).await
        }
    }

    struct ObserveBufferedUrlLayer(Arc<Mutex<Option<(reqwest::Url, reqwest::Url)>>>);

    #[async_trait]
    impl EgressLayer for ObserveBufferedUrlLayer {
        async fn handle(
            &self,
            request: reqwest::Request,
            extensions: &mut ::http::Extensions,
            next: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            let request_url = request.url().clone();
            request_response_buffer(extensions, reqwest::StatusCode::PAYMENT_REQUIRED, 1024);
            let response = next.run(request, extensions).await?;
            self.0
                .lock()
                .unwrap()
                .replace((request_url, response.url().clone()));
            Ok(response)
        }

        fn uses_response_buffering(&self) -> bool {
            true
        }
    }

    struct DenyLayer;

    #[async_trait]
    impl EgressLayer for DenyLayer {
        async fn handle(
            &self,
            _request: reqwest::Request,
            _extensions: &mut ::http::Extensions,
            _next: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            let mut response = ::http::Response::new(reqwest::Body::from("denied"));
            *response.status_mut() = reqwest::StatusCode::FORBIDDEN;
            Ok(response.into())
        }
    }

    struct EnvironmentLayer(&'static str, &'static str);

    #[async_trait]
    impl EgressLayer for EnvironmentLayer {
        async fn handle(
            &self,
            request: reqwest::Request,
            extensions: &mut ::http::Extensions,
            next: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            next.run(request, extensions).await
        }

        fn environment(&self) -> EgressEnvironment {
            EgressEnvironment::new([(self.0.into(), self.1.into())])
        }
    }

    struct FixedSecret(&'static str);

    #[async_trait]
    impl SecretResolver for FixedSecret {
        async fn resolve(&self, _reference: &SecretRef) -> Result<String, SecretResolverError> {
            Ok(self.0.to_owned())
        }
    }

    #[tokio::test]
    async fn builder_reports_each_zero_transport_limit_without_binding() {
        assert!(matches!(
            EgressProxy::builder().max_request_bytes(0).spawn().await,
            Err(EgressError::ZeroMaxRequestBytes)
        ));
        assert!(matches!(
            EgressProxy::builder()
                .max_concurrent_requests(0)
                .spawn()
                .await,
            Err(EgressError::ZeroMaxConcurrentRequests)
        ));
        assert!(matches!(
            EgressProxy::builder()
                .max_concurrent_connections(0)
                .spawn()
                .await,
            Err(EgressError::ZeroMaxConcurrentConnections)
        ));
        assert!(matches!(
            EgressProxy::builder()
                .max_request_bytes(8)
                .max_buffered_request_bytes(7)
                .spawn()
                .await,
            Err(EgressError::BufferedRequestBudgetTooSmall {
                configured: 7,
                minimum: 8
            })
        ));
        assert!(matches!(
            EgressProxy::builder()
                .request_setup_timeout(Duration::ZERO)
                .spawn()
                .await,
            Err(EgressError::ZeroRequestSetupTimeout)
        ));
    }

    #[tokio::test]
    async fn layers_cannot_override_transport_environment() {
        let result = EgressProxy::builder()
            .layer(EnvironmentLayer("Https_Proxy", "http://attacker.invalid"))
            .spawn()
            .await;

        assert!(matches!(
            result,
            Err(EgressError::EnvironmentConflict(name)) if name == "Https_Proxy"
        ));

        let result = EgressProxy::builder()
            .layer(EnvironmentLayer("INVALID=NAME", "value"))
            .spawn()
            .await;
        assert!(matches!(
            result,
            Err(EgressError::InvalidEnvironmentName(_))
        ));

        let result = EgressProxy::builder()
            .layer(EnvironmentLayer("SERVICE_VALUE", "value\0suffix"))
            .spawn()
            .await;
        assert!(matches!(
            result,
            Err(EgressError::InvalidEnvironmentValue(_))
        ));
    }

    async fn spawn_origin(app: Router) -> String {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    fn local_proxy_builder() -> EgressProxyBuilder {
        EgressProxy::builder().allow_loopback_upstreams(true)
    }

    fn proxied_client(egress: &EgressProxy) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(egress.proxy_url()).unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn rejects_clients_without_the_ephemeral_proxy_credential() {
        let egress = local_proxy_builder().spawn().await.unwrap();
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
    async fn denies_loopback_and_cloud_metadata_upstreams_by_default() {
        let origin = spawn_origin(Router::new().route("/", get(|| async { "unexpected" }))).await;
        let egress = EgressProxy::builder().spawn().await.unwrap();
        let client = proxied_client(&egress);

        let loopback = client.get(origin).send().await.unwrap();
        let metadata = client
            .get("http://169.254.169.254/latest/meta-data/")
            .send()
            .await
            .unwrap();

        assert_eq!(loopback.status(), AxumStatus::FORBIDDEN);
        assert_eq!(metadata.status(), AxumStatus::FORBIDDEN);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn passes_ordinary_http_responses_through() {
        let origin = spawn_origin(Router::new().route("/plain", get(|| async { "plain" }))).await;
        let egress = local_proxy_builder().spawn().await.unwrap();

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
    async fn buffered_responses_preserve_their_effective_url() {
        let origin = spawn_origin(Router::new().route(
            "/payment-required",
            get(|| async { (AxumStatus::PAYMENT_REQUIRED, "pay up") }),
        ))
        .await;
        let observed = Arc::new(Mutex::new(None));
        let egress = local_proxy_builder()
            .layer(ObserveBufferedUrlLayer(Arc::clone(&observed)))
            .spawn()
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .get(format!("{origin}/payment-required"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::PAYMENT_REQUIRED);
        let (request_url, response_url) = observed.lock().unwrap().take().unwrap();
        assert_eq!(response_url, request_url);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_protocol_upgrades_before_layers_or_origins() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/upgrade",
            get(move || {
                let calls = Arc::clone(&route_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "unexpected"
                }
            }),
        ))
        .await;
        let egress = local_proxy_builder().spawn().await.unwrap();

        let response = proxied_client(&egress)
            .get(format!("{origin}/upgrade"))
            .header(CONNECTION, "upgrade")
            .header(UPGRADE, "websocket")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::BAD_REQUEST);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn layers_run_in_attachment_order() {
        let origin = spawn_origin(Router::new().route(
            "/layers",
            get(|request: Request| async move {
                request
                    .headers()
                    .get("x-egress-layers")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            }),
        ))
        .await;
        let egress = local_proxy_builder()
            .layer(HeaderLayer("first"))
            .layer(HeaderLayer("second"))
            .spawn()
            .await
            .unwrap();

        let body = proxied_client(&egress)
            .get(format!("{origin}/layers"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(body, "first,second");
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_layer_can_short_circuit_before_the_origin() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/denied",
            get(move || {
                let route_calls = Arc::clone(&route_calls);
                async move {
                    route_calls.fetch_add(1, Ordering::SeqCst);
                    "unexpected"
                }
            }),
        ))
        .await;
        let egress = local_proxy_builder()
            .layer(DenyLayer)
            .spawn()
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .get(format!("{origin}/denied"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), AxumStatus::FORBIDDEN);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        let egress = local_proxy_builder()
            .max_concurrent_requests(3)
            .spawn()
            .await
            .unwrap();
        let client = proxied_client(&egress);
        let requests = (0..12)
            .map(|_| {
                let client = client.clone();
                let url = format!("{origin}/bounded");
                tokio::spawn(async move { client.get(url).send().await.unwrap().status() })
            })
            .collect::<Vec<_>>();

        while maximum.load(Ordering::SeqCst) < 3 {
            started.notified().await;
        }
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
    async fn request_permits_cover_streaming_response_bodies() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let origin = spawn_origin(Router::new().route(
            "/stream",
            get(move || {
                let route_calls = Arc::clone(&route_calls);
                async move {
                    route_calls.fetch_add(1, Ordering::SeqCst);
                    AxumBody::from_stream(futures_util::stream::pending::<
                        Result<AxumBytes, Infallible>,
                    >())
                }
            }),
        ))
        .await;
        let egress = local_proxy_builder()
            .max_concurrent_requests(1)
            .spawn()
            .await
            .unwrap();
        let client = proxied_client(&egress);

        let first = client.get(format!("{origin}/stream")).send().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second_client = client.clone();
        let second_origin = origin.clone();
        let second = tokio::spawn(async move {
            second_client
                .get(format!("{second_origin}/stream"))
                .send()
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!second.is_finished());

        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("dropping a streamed response must release its request permit")
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        drop(second);
        egress.shutdown().await.unwrap();
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
        let egress = local_proxy_builder()
            .max_request_bytes(4)
            .spawn()
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
    async fn bundled_secret_layer_replaces_only_at_the_authorized_origin() {
        let origin = spawn_origin(Router::new().route(
            "/allowed",
            get(|request: Request| async move {
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            }),
        ))
        .await;
        let placeholder = "nanocodex-secret-only-proof";
        let rule = SecretRule::builder("test", SecretRef::new("test", "token"), &origin)
            .method(Method::GET)
            .path_prefix("/allowed")
            .replace_header("authorization", placeholder)
            .child_environment("TEST_BASE_URL", "TEST_API_KEY")
            .build()
            .unwrap();
        let egress = local_proxy_builder()
            .layer(
                SecretEgress::builder(FixedSecret("host-only"))
                    .rule(rule)
                    .build()
                    .unwrap(),
            )
            .spawn()
            .await
            .unwrap();

        let response = proxied_client(&egress)
            .get(format!("{origin}/allowed"))
            .bearer_auth(placeholder)
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "Bearer host-only");
        let environment = egress.environment();
        assert!(environment.iter().all(|(_, value)| value != "host-only"));
        assert!(environment.iter().any(|(name, value)| {
            name == "TEST_API_KEY" && value == "nanocodex-secret-only-proof"
        }));
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn claimed_secret_origins_fail_closed_on_rule_mismatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = {
            let calls = Arc::clone(&calls);
            move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "unexpected"
                }
            }
        };
        let origin = spawn_origin(
            Router::new()
                .route("/allowed", any(handler.clone()))
                .route("/admin", any(handler)),
        )
        .await;
        let placeholder = "nanocodex-secret-fail-closed";
        let rule = SecretRule::builder("test", SecretRef::new("test", "token"), &origin)
            .method(Method::GET)
            .path_prefix("/allowed")
            .replace_header("authorization", placeholder)
            .child_environment("TEST_BASE_URL", "TEST_API_KEY")
            .build()
            .unwrap();
        let egress = local_proxy_builder()
            .layer(
                SecretEgress::builder(FixedSecret("host-only"))
                    .rule(rule)
                    .unmatched(UnmatchedEgress::Allow)
                    .build()
                    .unwrap(),
            )
            .spawn()
            .await
            .unwrap();
        let client = proxied_client(&egress);

        let responses = [
            client
                .post(format!("{origin}/allowed"))
                .bearer_auth(placeholder)
                .send()
                .await
                .unwrap(),
            client
                .get(format!("{origin}/admin"))
                .bearer_auth(placeholder)
                .send()
                .await
                .unwrap(),
            client
                .get(format!("{origin}/allowed"))
                .send()
                .await
                .unwrap(),
        ];

        assert!(
            responses
                .iter()
                .all(|response| response.status() == AxumStatus::FORBIDDEN)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_environment_contains_one_generic_proxy_capability() {
        let egress = EgressProxy::builder().spawn().await.unwrap();
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
            PathBuf::from(value("CURL_CA_BUNDLE")),
            egress.ca_certificate_path()
        );
        assert!(egress.ca_certificate_path().is_file());
        assert_eq!(value("ALL_PROXY"), OsString::from(egress.proxy_url()));
        assert_eq!(
            PathBuf::from(value("GIT_SSL_CAINFO")),
            egress.ca_certificate_path()
        );
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn route_projects_the_same_capability_into_an_isolated_runtime() {
        let egress = EgressProxy::builder().spawn().await.unwrap();
        let route = egress.route();
        let environment = route.environment("/run/nanocodex/egress-ca.pem");

        assert_eq!(
            environment.get("HTTPS_PROXY"),
            Some(OsStr::new(route.proxy_url()))
        );
        assert_eq!(
            environment.get("SSL_CERT_FILE"),
            Some(OsStr::new("/run/nanocodex/egress-ca.pem"))
        );
        assert!(
            route
                .ca_certificate_pem()
                .starts_with(b"-----BEGIN CERTIFICATE-----")
        );
        assert_eq!(route.ca_certificate_pem(), egress.ca_certificate_pem);
        egress.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "manual public-network HTTPS smoke"]
    async fn live_https_mitm_smoke() {
        let egress = EgressProxy::builder().spawn().await.unwrap();
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
