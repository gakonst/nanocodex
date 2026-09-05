use std::{borrow::Cow, collections::HashSet, sync::Arc};

use async_trait::async_trait;
use hudsucker::hyper::{Method, header::HeaderName, http::uri::Authority};
use reqwest::Url;
use thiserror::Error;

use crate::{EgressLayer, EgressLayerError, EgressRequest};

const MAX_PATH_PREFIX_BYTES: usize = 2_048;

/// Enforcement behavior for a static egress policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PolicyMode {
    /// Reject requests that do not match an allow rule.
    #[default]
    Enforce,
    /// Trace requests that would be rejected while allowing them onward.
    Warn,
}

/// One validated exact-origin, method, and path allow rule.
#[derive(Clone, Debug)]
pub struct EgressPolicyRule {
    upstream: Url,
    methods: Vec<Method>,
    path_prefixes: Vec<String>,
}

impl EgressPolicyRule {
    /// Starts an allow rule for one credential-free HTTP(S) origin or base URL.
    #[must_use]
    pub fn builder(upstream: impl Into<String>) -> EgressPolicyRuleBuilder {
        EgressPolicyRuleBuilder {
            upstream: upstream.into(),
            methods: Vec::new(),
            path_prefixes: Vec::new(),
        }
    }

    fn matches_request(&self, request: &reqwest::Request) -> bool {
        matching_url(self, request.url())
            && allows_method_and_path(self, request.method(), request.url().path())
    }

    fn matches_connect(&self, authority: &Authority) -> bool {
        self.upstream.scheme() == "https"
            && self
                .upstream
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(authority.host()))
            && self.upstream.port_or_known_default() == authority.port_u16().or(Some(443))
    }
}

/// Builder for one [`EgressPolicyRule`].
pub struct EgressPolicyRuleBuilder {
    upstream: String,
    methods: Vec<Method>,
    path_prefixes: Vec<String>,
}

impl EgressPolicyRuleBuilder {
    /// Adds one accepted ordinary HTTP method. No methods means all.
    #[must_use]
    pub fn method(mut self, method: Method) -> Self {
        if !self.methods.contains(&method) {
            self.methods.push(method);
        }
        self
    }

    /// Adds one accepted path-segment prefix. No prefixes means the upstream
    /// base path and everything beneath it.
    #[must_use]
    pub fn path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefixes.push(prefix.into());
        self
    }

    /// Validates and creates the rule.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe origins, methods, or paths.
    pub fn build(self) -> Result<EgressPolicyRule, PolicyConfigError> {
        let upstream =
            Url::parse(&self.upstream).map_err(|_| PolicyConfigError::InvalidUpstream)?;
        if !matches!(upstream.scheme(), "http" | "https")
            || upstream.host_str().is_none()
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.query().is_some()
            || upstream.fragment().is_some()
            || !valid_path_prefix(upstream.path())
        {
            return Err(PolicyConfigError::InvalidUpstream);
        }
        if self.methods.iter().any(|method| !ordinary_method(method)) {
            return Err(PolicyConfigError::InvalidMethod);
        }
        if self
            .path_prefixes
            .iter()
            .any(|prefix| !valid_path_prefix(prefix))
        {
            return Err(PolicyConfigError::InvalidPathPrefix);
        }
        Ok(EgressPolicyRule {
            upstream,
            methods: self.methods,
            path_prefixes: self.path_prefixes,
        })
    }
}

/// Default-deny static request policy as one ordered egress layer.
#[derive(Clone)]
pub struct EgressPolicy {
    rules: Arc<[EgressPolicyRule]>,
    mode: PolicyMode,
}

impl EgressPolicy {
    /// Starts an enforcing policy builder.
    #[must_use]
    pub const fn builder() -> EgressPolicyBuilder {
        EgressPolicyBuilder {
            rules: Vec::new(),
            mode: PolicyMode::Enforce,
        }
    }

    fn authorize_request(&self, request: &reqwest::Request) -> Result<(), EgressLayerError> {
        if self.rules.iter().any(|rule| rule.matches_request(request)) {
            return Ok(());
        }
        match self.mode {
            PolicyMode::Enforce => Err(EgressLayerError::Denied),
            PolicyMode::Warn => {
                tracing::warn!(
                    target: "nanocodex_egress",
                    http_method = %request.method(),
                    url = %request.url(),
                    "static egress policy would deny request"
                );
                Ok(())
            }
        }
    }
}

/// Builder for [`EgressPolicy`].
pub struct EgressPolicyBuilder {
    rules: Vec<EgressPolicyRule>,
    mode: PolicyMode,
}

impl EgressPolicyBuilder {
    /// Appends one allow rule.
    #[must_use]
    pub fn rule(mut self, rule: EgressPolicyRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Selects enforce or observation-only warn behavior.
    #[must_use]
    pub const fn mode(mut self, mode: PolicyMode) -> Self {
        self.mode = mode;
        self
    }

    /// Creates the policy.
    ///
    /// # Errors
    ///
    /// Returns an error when no allow rule was configured.
    pub fn build(self) -> Result<EgressPolicy, PolicyConfigError> {
        if self.rules.is_empty() {
            return Err(PolicyConfigError::MissingRules);
        }
        Ok(EgressPolicy {
            rules: self.rules.into(),
            mode: self.mode,
        })
    }
}

#[async_trait]
impl EgressLayer for EgressPolicy {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        self.authorize_request(&request)
            .map_err(reqwest_middleware::Error::middleware)?;
        next.run(request, extensions).await
    }

    async fn authorize_connect(&self, request: &EgressRequest) -> Result<(), EgressLayerError> {
        let authority = request
            .uri()
            .authority()
            .cloned()
            .or_else(|| request.uri().to_string().parse().ok())
            .ok_or(EgressLayerError::InvalidRequest)?;
        if self
            .rules
            .iter()
            .any(|rule| rule.matches_connect(&authority))
        {
            return Ok(());
        }
        match self.mode {
            PolicyMode::Enforce => Err(EgressLayerError::Denied),
            PolicyMode::Warn => Ok(()),
        }
    }
}

/// Default-deny request-header filter with optional request scoping.
#[derive(Clone)]
pub struct HeaderAllowlist {
    headers: Arc<HashSet<HeaderName>>,
    prefixes: Arc<[String]>,
    rules: Arc<[EgressPolicyRule]>,
}

impl HeaderAllowlist {
    /// Starts a header allowlist builder.
    #[must_use]
    pub fn builder() -> HeaderAllowlistBuilder {
        HeaderAllowlistBuilder {
            headers: HashSet::new(),
            prefixes: Vec::new(),
            rules: Vec::new(),
            invalid_header: false,
        }
    }

    fn applies(&self, request: &reqwest::Request) -> bool {
        self.rules.is_empty() || self.rules.iter().any(|rule| rule.matches_request(request))
    }

    fn retain_headers(&self, request: &mut reqwest::Request) {
        if !self.applies(request) {
            return;
        }
        let stripped = request
            .headers()
            .keys()
            .filter(|name| {
                !self.headers.contains(*name)
                    && !self
                        .prefixes
                        .iter()
                        .any(|prefix| name.as_str().starts_with(prefix))
            })
            .cloned()
            .collect::<Vec<_>>();
        for name in &stripped {
            request.headers_mut().remove(name);
        }
        if !stripped.is_empty() {
            let stripped = stripped.iter().map(HeaderName::as_str).collect::<Vec<_>>();
            tracing::info!(
                target: "nanocodex_egress",
                stripped_headers = ?stripped,
                "request header allowlist removed child headers"
            );
        }
    }
}

/// Builder for [`HeaderAllowlist`].
pub struct HeaderAllowlistBuilder {
    headers: HashSet<HeaderName>,
    prefixes: Vec<String>,
    rules: Vec<EgressPolicyRule>,
    invalid_header: bool,
}

impl HeaderAllowlistBuilder {
    /// Allows one exact header name, matched case-insensitively.
    #[must_use]
    pub fn header(mut self, header: impl AsRef<str>) -> Self {
        match HeaderName::from_bytes(header.as_ref().as_bytes()) {
            Ok(header) => {
                self.headers.insert(header);
            }
            Err(_) => self.invalid_header = true,
        }
        self
    }

    /// Allows headers whose lowercase names begin with this ASCII prefix.
    #[must_use]
    pub fn header_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into().to_ascii_lowercase());
        self
    }

    /// Limits this filter to requests accepted by one scope rule. No rules
    /// applies the filter to every ordinary request.
    #[must_use]
    pub fn rule(mut self, rule: EgressPolicyRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Validates and creates the filter.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or invalid allowlist.
    pub fn build(self) -> Result<HeaderAllowlist, PolicyConfigError> {
        if self.invalid_header
            || self.prefixes.iter().any(|prefix| {
                prefix.is_empty()
                    || HeaderName::from_bytes(format!("{prefix}x").as_bytes()).is_err()
            })
        {
            return Err(PolicyConfigError::InvalidHeader);
        }
        if self.headers.is_empty() && self.prefixes.is_empty() {
            return Err(PolicyConfigError::MissingHeaders);
        }
        Ok(HeaderAllowlist {
            headers: Arc::new(self.headers),
            prefixes: self.prefixes.into(),
            rules: self.rules.into(),
        })
    }
}

#[async_trait]
impl EgressLayer for HeaderAllowlist {
    async fn handle(
        &self,
        mut request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        self.retain_headers(&mut request);
        next.run(request, extensions).await
    }
}

/// Invalid static-policy or header-filter configuration.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PolicyConfigError {
    /// A static policy contained no allow rules.
    #[error("egress policy requires at least one allow rule")]
    MissingRules,
    /// An upstream was not a credential-free absolute HTTP(S) base URL.
    #[error("egress policy upstream must be a credential-free absolute HTTP(S) base URL")]
    InvalidUpstream,
    /// A CONNECT or extension method was configured.
    #[error("egress policy supports ordinary HTTP methods only")]
    InvalidMethod,
    /// A path prefix was relative or ambiguously encoded.
    #[error("egress policy path prefixes must be safe bounded absolute paths")]
    InvalidPathPrefix,
    /// A header name or prefix was invalid.
    #[error("header allowlist contains an invalid name or prefix")]
    InvalidHeader,
    /// A header allowlist did not contain any names or prefixes.
    #[error("header allowlist requires at least one name or prefix")]
    MissingHeaders,
}

fn matching_url(rule: &EgressPolicyRule, url: &Url) -> bool {
    rule.upstream.scheme() == url.scheme()
        && rule
            .upstream
            .host_str()
            .zip(url.host_str())
            .is_some_and(|(expected, actual)| expected.eq_ignore_ascii_case(actual))
        && rule.upstream.port_or_known_default() == url.port_or_known_default()
}

fn allows_method_and_path(rule: &EgressPolicyRule, method: &Method, path: &str) -> bool {
    let Some(path) = safe_request_path(path) else {
        return false;
    };
    let Some(upstream) = safe_request_path(rule.upstream.path()) else {
        return false;
    };
    ordinary_method(method)
        && path_prefix_matches(&upstream, &path)
        && (rule.methods.is_empty() || rule.methods.contains(method))
        && (rule.path_prefixes.is_empty()
            || rule
                .path_prefixes
                .iter()
                .any(|prefix| path_prefix_matches(prefix, &path)))
}

const fn ordinary_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::POST
            | Method::PUT
            | Method::PATCH
            | Method::DELETE
            | Method::HEAD
            | Method::OPTIONS
    )
}

fn path_prefix_matches(prefix: &str, path: &str) -> bool {
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| prefix.ends_with('/') || suffix.starts_with('/'))
}

fn valid_path_prefix(prefix: &str) -> bool {
    prefix.starts_with('/')
        && prefix.len() <= MAX_PATH_PREFIX_BYTES
        && safe_request_path(prefix).is_some()
}

fn safe_request_path(path: &str) -> Option<Cow<'_, str>> {
    if path.contains('\\')
        || path.split('/').any(|segment| segment == "..")
        || contains_ambiguous_path_escape(path)
    {
        return None;
    }
    if path.starts_with('/') && !path.starts_with("//") {
        Some(Cow::Borrowed(path))
    } else {
        Some(Cow::Owned(format!("/{}", path.trim_start_matches('/'))))
    }
}

fn contains_ambiguous_path_escape(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| decode_hex_byte(digits[0], digits[1]));
        let Some(encoded) = encoded else {
            return true;
        };
        if matches!(encoded, b'%' | b'.' | b'/' | b'\\' | b'?' | b'#') {
            return true;
        }
        index += 3;
    }
    false
}

fn decode_hex_byte(high: u8, low: u8) -> Option<u8> {
    Some(hex_value(high)? << 4 | hex_value(low)?)
}

const fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: Method, url: &str) -> reqwest::Request {
        reqwest::Request::new(method, Url::parse(url).unwrap())
    }

    #[test]
    fn static_policy_is_default_deny_and_path_segment_safe() {
        let rule = EgressPolicyRule::builder("https://api.example.com/v1")
            .method(Method::POST)
            .path_prefix("/v1/messages")
            .build()
            .unwrap();
        let policy = EgressPolicy::builder().rule(rule).build().unwrap();

        assert!(
            policy
                .authorize_request(&request(
                    Method::POST,
                    "https://api.example.com/v1/messages/1"
                ))
                .is_ok()
        );
        assert_eq!(
            policy.authorize_request(&request(
                Method::POST,
                "https://api.example.com/v1/messages-attacker"
            )),
            Err(EgressLayerError::Denied)
        );
        assert_eq!(
            policy.authorize_request(&request(
                Method::GET,
                "https://attacker.invalid/v1/messages"
            )),
            Err(EgressLayerError::Denied)
        );
    }

    #[test]
    fn header_allowlist_supports_exact_names_prefixes_and_scopes() {
        let scope = EgressPolicyRule::builder("https://api.example.com")
            .build()
            .unwrap();
        let filter = HeaderAllowlist::builder()
            .header("authorization")
            .header_prefix("x-trace-")
            .rule(scope)
            .build()
            .unwrap();
        let mut request = request(Method::GET, "https://api.example.com/data");
        for name in ["authorization", "x-trace-id", "cookie"] {
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                "value".parse().unwrap(),
            );
        }

        filter.retain_headers(&mut request);

        assert!(request.headers().contains_key("authorization"));
        assert!(request.headers().contains_key("x-trace-id"));
        assert!(!request.headers().contains_key("cookie"));
    }
}
