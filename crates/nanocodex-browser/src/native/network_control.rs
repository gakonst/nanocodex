use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        fetch::{
            ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
            FulfillRequestParams, HeaderEntry,
        },
        network::ErrorReason,
    },
};
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use tracing::warn;
use url::{Host, Url};

use crate::{BrowserEgressPolicy, BrowserPageError, BrowserRouteResponse};

use super::{BrowserError, Diagnostics};

const MAX_NETWORK_ROUTES: usize = 128;
const MAX_ROUTE_ID_BYTES: usize = 256;
const MAX_ROUTE_PATTERN_BYTES: usize = 4 * 1024;
const MAX_ROUTE_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ROUTE_HEADERS: usize = 128;
const MAX_ROUTE_HEADER_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct NetworkControls {
    inner: Arc<StdMutex<NetworkControlState>>,
}

#[derive(Default)]
struct NetworkControlState {
    policy: Option<BrowserEgressPolicy>,
    routes: BTreeMap<String, NetworkRoute>,
}

#[derive(Clone)]
struct NetworkRoute {
    url_contains: String,
    response: BrowserRouteResponse,
}

pub(super) enum RequestDecision {
    Continue,
    Block,
    Fulfill(BrowserRouteResponse),
}

impl NetworkControls {
    pub(super) fn new(policy: Option<BrowserEgressPolicy>) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(NetworkControlState {
                policy,
                routes: BTreeMap::new(),
            })),
        }
    }

    pub(super) fn restricted(&self) -> Result<bool, BrowserError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .policy
            .is_some())
    }

    pub(super) fn route(
        &self,
        route_id: String,
        url_contains: String,
        response: BrowserRouteResponse,
    ) -> Result<(), BrowserError> {
        if route_id.trim().is_empty() {
            return Err(BrowserError::InvalidNetworkRoute {
                message: "route_id cannot be empty".to_owned(),
            });
        }
        if route_id.len() > MAX_ROUTE_ID_BYTES {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!(
                    "route_id is {} bytes, above the {MAX_ROUTE_ID_BYTES}-byte limit",
                    route_id.len()
                ),
            });
        }
        if url_contains.is_empty() {
            return Err(BrowserError::InvalidNetworkRoute {
                message: "url_contains cannot be empty".to_owned(),
            });
        }
        if url_contains.len() > MAX_ROUTE_PATTERN_BYTES {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!(
                    "url_contains is {} bytes, above the {MAX_ROUTE_PATTERN_BYTES}-byte limit",
                    url_contains.len()
                ),
            });
        }
        if !(100..=599).contains(&response.status) {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!("HTTP status {} is outside 100..=599", response.status),
            });
        }
        if response.body.len() > MAX_ROUTE_BODY_BYTES {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!(
                    "response body is {} bytes, above the {MAX_ROUTE_BODY_BYTES}-byte limit",
                    response.body.len()
                ),
            });
        }
        if response.headers.len() > MAX_ROUTE_HEADERS {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!(
                    "response has {} headers, above the {MAX_ROUTE_HEADERS}-header limit",
                    response.headers.len()
                ),
            });
        }
        let header_bytes = response.headers.iter().fold(0_usize, |total, header| {
            total
                .saturating_add(header.name.len())
                .saturating_add(header.value.len())
        });
        if header_bytes > MAX_ROUTE_HEADER_BYTES {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!(
                    "response headers are {header_bytes} bytes, above the \
                     {MAX_ROUTE_HEADER_BYTES}-byte limit"
                ),
            });
        }
        if response.headers.iter().any(|header| {
            header.name.trim().is_empty()
                || header.name.contains(['\r', '\n'])
                || header.value.contains(['\r', '\n'])
        }) {
            return Err(BrowserError::InvalidNetworkRoute {
                message: "response headers contain an empty name or newline".to_owned(),
            });
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
        if !state.routes.contains_key(&route_id) && state.routes.len() >= MAX_NETWORK_ROUTES {
            return Err(BrowserError::InvalidNetworkRoute {
                message: format!("browser retains at most {MAX_NETWORK_ROUTES} network routes"),
            });
        }
        state.routes.insert(
            route_id,
            NetworkRoute {
                url_contains,
                response,
            },
        );
        Ok(())
    }

    pub(super) fn remove_route(&self, route_id: &str) -> Result<(), BrowserError> {
        self.inner
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .routes
            .remove(route_id);
        Ok(())
    }

    pub(super) fn clear_routes(&self) -> Result<(), BrowserError> {
        self.inner
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .routes
            .clear();
        Ok(())
    }

    pub(super) fn decide(&self, url: &str) -> RequestDecision {
        let Ok(state) = self.inner.lock() else {
            return RequestDecision::Block;
        };
        if let Some(route) = state
            .routes
            .values()
            .find(|route| url.contains(&route.url_contains))
        {
            return RequestDecision::Fulfill(route.response.clone());
        }
        match &state.policy {
            None => RequestDecision::Continue,
            Some(policy) if url_allowed(url, policy) => RequestDecision::Continue,
            Some(_) => RequestDecision::Block,
        }
    }
}

pub(super) async fn start(
    page: &Page,
    controls: NetworkControls,
    diagnostics: Arc<StdMutex<Diagnostics>>,
) -> Result<JoinHandle<()>, BrowserError> {
    let mut requests = page.event_listener::<EventRequestPaused>().await?;
    page.execute(EnableParams::default()).await?;
    let page = page.clone();
    Ok(tokio::spawn(async move {
        while let Some(event) = requests.next().await {
            let decision = controls.decide(&event.request.url);
            let result = match decision {
                RequestDecision::Continue => page
                    .execute(ContinueRequestParams::new(event.request_id.clone()))
                    .await
                    .map(drop),
                RequestDecision::Block => {
                    record_blocked_request(&diagnostics, &event.request.url);
                    page.execute(FailRequestParams::new(
                        event.request_id.clone(),
                        ErrorReason::BlockedByClient,
                    ))
                    .await
                    .map(drop)
                }
                RequestDecision::Fulfill(response) => {
                    let params = fulfill_params(&event, response);
                    page.execute(params).await.map(drop)
                }
            };
            if let Err(error) = result {
                warn!(
                    target: "nanocodex_browser",
                    %error,
                    "failed to resolve an intercepted browser request"
                );
            }
        }
    }))
}

pub(super) fn fulfill_params(
    event: &EventRequestPaused,
    response: BrowserRouteResponse,
) -> FulfillRequestParams {
    let mut params =
        FulfillRequestParams::new(event.request_id.clone(), i64::from(response.status));
    params.response_headers = Some(
        response
            .headers
            .into_iter()
            .map(|header| HeaderEntry::new(header.name, header.value))
            .collect(),
    );
    params.body = Some(STANDARD.encode(response.body.as_bytes()).into());
    params
}

pub(super) fn record_blocked_request(diagnostics: &Arc<StdMutex<Diagnostics>>, url: &str) {
    if let Ok(mut diagnostics) = diagnostics.lock() {
        diagnostics.push_error(BrowserPageError {
            sequence: 0,
            text: format!("blocked network request outside the browser egress policy: {url}"),
            url: Some(url.to_owned()),
            line: None,
            column: None,
            stack: Vec::new(),
        });
    }
}

fn url_allowed(raw: &str, policy: &BrowserEgressPolicy) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if matches!(url.scheme(), "about" | "data" | "blob") {
        return true;
    }
    let Some(host) = url.host() else {
        return false;
    };
    if policy.allow_loopback && is_loopback(&host) {
        return true;
    }
    if policy
        .allowed_origins
        .iter()
        .any(|allowed| origins_equivalent(allowed, &url))
    {
        return true;
    }
    let host = host.to_string().to_ascii_lowercase();
    policy.allowed_domain_suffixes.iter().any(|suffix| {
        let suffix = suffix.trim().trim_start_matches('.').to_ascii_lowercase();
        !suffix.is_empty() && (host == suffix || host.ends_with(&format!(".{suffix}")))
    })
}

fn origins_equivalent(allowed: &Url, candidate: &Url) -> bool {
    let allowed_scheme = normalize_web_socket_scheme(allowed.scheme());
    let candidate_scheme = normalize_web_socket_scheme(candidate.scheme());
    allowed_scheme == candidate_scheme
        && allowed.host() == candidate.host()
        && allowed.port_or_known_default() == candidate.port_or_known_default()
}

fn normalize_web_socket_scheme(scheme: &str) -> &str {
    match scheme {
        "ws" => "http",
        "wss" => "https",
        scheme => scheme,
    }
}

const fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => IpAddr::V4(*address).is_loopback(),
        Host::Ipv6(address) => IpAddr::V6(*address).is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> BrowserRouteResponse {
        BrowserRouteResponse {
            status: 200,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    #[test]
    fn egress_matching_is_boundary_aware() {
        let policy = BrowserEgressPolicy::deny_by_default()
            .allow_domain("example.com")
            .allow_origin(Url::parse("https://exact.test:8443").unwrap());
        assert!(url_allowed("https://example.com/", &policy));
        assert!(url_allowed("wss://api.example.com/socket", &policy));
        assert!(!url_allowed("https://notexample.com/", &policy));
        assert!(url_allowed("https://exact.test:8443/", &policy));
        assert!(!url_allowed("https://exact.test/", &policy));
        assert!(!url_allowed("http://127.0.0.1/", &policy));
    }

    #[test]
    fn routes_bound_retained_state_and_response_size() {
        let controls = NetworkControls::new(None);
        for index in 0..MAX_NETWORK_ROUTES {
            controls
                .route(format!("route-{index}"), "/fixture".to_owned(), response())
                .unwrap();
        }
        controls
            .route("route-0".to_owned(), "/replacement".to_owned(), response())
            .unwrap();
        assert!(
            controls
                .route("overflow".to_owned(), "/fixture".to_owned(), response())
                .is_err()
        );

        let mut oversized = response();
        oversized.body = "x".repeat(MAX_ROUTE_BODY_BYTES + 1);
        assert!(
            NetworkControls::new(None)
                .route("oversized".to_owned(), "/fixture".to_owned(), oversized)
                .is_err()
        );
    }

    #[test]
    fn routes_reject_header_injection() {
        let mut injected = response();
        injected.headers.push(crate::BrowserRouteHeader {
            name: "x-safe".to_owned(),
            value: "value\r\nx-injected: yes".to_owned(),
        });
        assert!(
            NetworkControls::new(None)
                .route("headers".to_owned(), "/fixture".to_owned(), injected)
                .is_err()
        );

        let mut oversized = response();
        oversized.headers.push(crate::BrowserRouteHeader {
            name: "x-large".to_owned(),
            value: "x".repeat(MAX_ROUTE_HEADER_BYTES),
        });
        assert!(
            NetworkControls::new(None)
                .route("headers".to_owned(), "/fixture".to_owned(), oversized)
                .is_err()
        );
    }
}
