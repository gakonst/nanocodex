use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hudsucker::hyper::{
    Method,
    header::{Entry, HeaderName, HeaderValue},
    http::uri::Authority,
};
use reqwest::Url;
use thiserror::Error;

use crate::{EgressEnvironment, EgressLayer, EgressLayerError, EgressRequest};

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_PLACEHOLDER_BYTES: usize = 4 * 1_024;
const MAX_PATH_PREFIX_BYTES: usize = 2_048;

/// Opaque reference understood only by a host-side [`SecretResolver`].
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SecretRef {
    provider: String,
    key: String,
}

impl SecretRef {
    /// Creates a provider-qualified reference without resolving its value.
    #[must_use]
    pub fn new(provider: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            key: key.into(),
        }
    }

    /// Returns the application-defined provider name.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the provider-owned opaque lookup key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Host-side secret resolution failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SecretResolverError {
    /// The referenced value does not exist or is not authorized.
    #[error("secret is unavailable")]
    Unavailable,
}

/// Resolves credential values on the host for already-authorized requests.
///
/// Implementations should avoid caching unless their backing provider defines
/// an explicit rotation contract. Resolved values are never placed in child
/// configuration or returned by this library.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// Resolves one opaque reference.
    async fn resolve(&self, reference: &SecretRef) -> Result<String, SecretResolverError>;
}

#[async_trait]
impl<R> SecretResolver for Arc<R>
where
    R: SecretResolver + ?Sized,
{
    async fn resolve(&self, reference: &SecretRef) -> Result<String, SecretResolverError> {
        self.as_ref().resolve(reference).await
    }
}

/// Host-memory resolver for small applications and deterministic tests.
///
/// This type deliberately omits `Debug` so formatting it cannot expose the
/// values it owns. Use [`SecretResolver`] directly for external or rotating
/// secret stores.
#[derive(Clone, Default)]
pub struct StaticSecretResolver {
    values: BTreeMap<SecretRef, String>,
}

impl StaticSecretResolver {
    /// Creates an empty resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces one host-only value.
    #[must_use]
    pub fn with_secret(mut self, reference: SecretRef, value: impl Into<String>) -> Self {
        self.values.insert(reference, value.into());
        self
    }
}

#[async_trait]
impl SecretResolver for StaticSecretResolver {
    async fn resolve(&self, reference: &SecretRef) -> Result<String, SecretResolverError> {
        self.values
            .get(reference)
            .cloned()
            .ok_or(SecretResolverError::Unavailable)
    }
}

/// Host-side formatting applied before a secret is injected.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SecretFormat {
    /// Injects the resolved value unchanged.
    #[default]
    Raw,
    /// Injects an RFC 6750-style `Bearer` credential.
    Bearer,
    /// Injects HTTP Basic credentials using a public username.
    Basic {
        /// Public username paired with the resolved secret as its password.
        username: String,
    },
    /// Wraps the resolved value in fixed public text.
    Affix {
        /// Text placed before the resolved value.
        prefix: String,
        /// Text placed after the resolved value.
        suffix: String,
    },
}

impl SecretFormat {
    fn apply(&self, value: &str) -> String {
        match self {
            Self::Raw => value.to_owned(),
            Self::Bearer => format!("Bearer {value}"),
            Self::Basic { username } => {
                format!("Basic {}", STANDARD.encode(format!("{username}:{value}")))
            }
            Self::Affix { prefix, suffix } => format!("{prefix}{value}{suffix}"),
        }
    }
}

#[derive(Clone, Debug)]
struct SecretReplacement {
    placeholder: String,
    headers: Vec<HeaderName>,
    query: bool,
    path: bool,
    body: bool,
    require: bool,
}

#[derive(Clone, Debug)]
enum SecretInjection {
    Header(HeaderName, SecretFormat),
    Query(String, SecretFormat),
}

#[derive(Clone, Debug)]
enum SecretAction {
    Replace(SecretReplacement),
    Inject(SecretInjection),
}

#[derive(Clone, Debug)]
struct ReplacementBuilder {
    placeholder: String,
    headers: Vec<String>,
    query: bool,
    path: bool,
    body: bool,
    require: bool,
    conflicting_placeholder: bool,
}

#[derive(Clone, Debug)]
enum InjectionBuilder {
    Header(String, SecretFormat),
    Query(String, SecretFormat),
}

/// One validated destination-bound secret injection or replacement rule.
#[derive(Clone, Debug)]
pub struct SecretRule {
    id: String,
    source: SecretRef,
    upstream: Url,
    methods: Vec<Method>,
    path_prefixes: Vec<String>,
    action: SecretAction,
    base_url_environment: String,
    placeholder_environment: Option<String>,
}

impl SecretRule {
    /// Starts a rule builder for one credential-free HTTPS or loopback HTTP upstream.
    #[must_use]
    pub fn builder(
        id: impl Into<String>,
        source: SecretRef,
        upstream: impl Into<String>,
    ) -> SecretRuleBuilder {
        SecretRuleBuilder {
            id: id.into(),
            source,
            upstream: upstream.into(),
            methods: Vec::new(),
            path_prefixes: Vec::new(),
            replacement: None,
            replacement_required: true,
            injection: None,
            multiple_injections: false,
            base_url_environment: None,
            placeholder_environment: None,
        }
    }

    /// Returns the stable non-secret rule identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the opaque host resolver reference.
    #[must_use]
    pub const fn source(&self) -> &SecretRef {
        &self.source
    }

    /// Returns the authorized upstream base URL.
    #[must_use]
    pub fn upstream(&self) -> &str {
        self.upstream.as_str().trim_end_matches('/')
    }
}

/// Builder for one [`SecretRule`].
pub struct SecretRuleBuilder {
    id: String,
    source: SecretRef,
    upstream: String,
    methods: Vec<Method>,
    path_prefixes: Vec<String>,
    replacement: Option<ReplacementBuilder>,
    replacement_required: bool,
    injection: Option<InjectionBuilder>,
    multiple_injections: bool,
    base_url_environment: Option<String>,
    placeholder_environment: Option<String>,
}

impl SecretRuleBuilder {
    /// Adds one accepted HTTP method. No methods means all ordinary methods.
    #[must_use]
    pub fn method(mut self, method: Method) -> Self {
        if !self.methods.contains(&method) {
            self.methods.push(method);
        }
        self
    }

    /// Adds one accepted path-segment prefix, such as `/v1/responses`.
    #[must_use]
    pub fn path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefixes.push(prefix.into());
        self
    }

    /// Selects the header and public placeholder replaced at egress.
    #[must_use]
    pub fn replace_header(
        mut self,
        header: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        self.replacement_mut(placeholder.into())
            .headers
            .push(header.into());
        self
    }

    /// Replaces the public placeholder in query parameter values.
    #[must_use]
    pub fn replace_query(mut self, placeholder: impl Into<String>) -> Self {
        self.replacement_mut(placeholder.into()).query = true;
        self
    }

    /// Replaces the public placeholder in the request path.
    #[must_use]
    pub fn replace_path(mut self, placeholder: impl Into<String>) -> Self {
        self.replacement_mut(placeholder.into()).path = true;
        self
    }

    /// Replaces the public placeholder in a buffered request body.
    #[must_use]
    pub fn replace_body(mut self, placeholder: impl Into<String>) -> Self {
        self.replacement_mut(placeholder.into()).body = true;
        self
    }

    /// Allows a matching request to proceed when no replacement location
    /// contains the public placeholder.
    ///
    /// Replacement is required by default. Optional replacement is useful for
    /// endpoints where authentication itself is optional; it must not be used
    /// to weaken a credential boundary.
    #[must_use]
    pub const fn optional_replacement(mut self) -> Self {
        self.replacement_required = false;
        if let Some(replacement) = &mut self.replacement {
            replacement.require = false;
        }
        self
    }

    /// Injects the resolved and formatted value into one request header,
    /// replacing any child-provided value.
    #[must_use]
    pub fn inject_header(mut self, header: impl Into<String>, format: SecretFormat) -> Self {
        self.multiple_injections |= self.injection.is_some();
        self.injection = Some(InjectionBuilder::Header(header.into(), format));
        self
    }

    /// Injects the resolved and formatted value into one query parameter,
    /// replacing every child-provided value for that parameter.
    #[must_use]
    pub fn inject_query(mut self, parameter: impl Into<String>, format: SecretFormat) -> Self {
        self.multiple_injections |= self.injection.is_some();
        self.injection = Some(InjectionBuilder::Query(parameter.into(), format));
        self
    }

    /// Names the child variables receiving the base URL and public placeholder.
    #[must_use]
    pub fn child_environment(
        mut self,
        base_url: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        self.base_url_environment = Some(base_url.into());
        self.placeholder_environment = Some(placeholder.into());
        self
    }

    /// Names the child variable receiving only the public upstream base URL.
    ///
    /// Injection rules do not expose a placeholder and should use this method.
    #[must_use]
    pub fn child_base_url_environment(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_environment = Some(base_url.into());
        self
    }

    fn replacement_mut(&mut self, placeholder: String) -> &mut ReplacementBuilder {
        let replacement = self.replacement.get_or_insert_with(|| ReplacementBuilder {
            placeholder: placeholder.clone(),
            headers: Vec::new(),
            query: false,
            path: false,
            body: false,
            require: self.replacement_required,
            conflicting_placeholder: false,
        });
        if replacement.placeholder != placeholder {
            replacement.conflicting_placeholder = true;
        }
        replacement.require = self.replacement_required;
        replacement
    }

    /// Validates and creates the destination-bound rule.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe origins, paths, headers, placeholders, or
    /// child environment names.
    pub fn build(self) -> Result<SecretRule, SecretConfigError> {
        validate_identifier(&self.id).ok_or(SecretConfigError::InvalidId)?;
        if self.source.provider.trim().is_empty()
            || self.source.key.trim().is_empty()
            || self.source.provider.len() > MAX_IDENTIFIER_BYTES
            || self.source.key.len() > 4 * 1_024
        {
            return Err(SecretConfigError::InvalidSource);
        }
        let upstream =
            Url::parse(&self.upstream).map_err(|_| SecretConfigError::InvalidUpstream)?;
        if !matches!(upstream.scheme(), "http" | "https")
            || upstream.host_str().is_none()
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.query().is_some()
            || upstream.fragment().is_some()
            || !valid_path_prefix(upstream.path())
        {
            return Err(SecretConfigError::InvalidUpstream);
        }
        if upstream.scheme() == "http" && !upstream.host_str().is_some_and(is_loopback_host) {
            return Err(SecretConfigError::InsecureUpstream);
        }
        if self.methods.iter().any(|method| {
            !matches!(
                *method,
                Method::GET
                    | Method::POST
                    | Method::PUT
                    | Method::PATCH
                    | Method::DELETE
                    | Method::HEAD
                    | Method::OPTIONS
            )
        }) {
            return Err(SecretConfigError::InvalidMethod);
        }
        if self
            .path_prefixes
            .iter()
            .any(|prefix| !valid_path_prefix(prefix))
        {
            return Err(SecretConfigError::InvalidPathPrefix);
        }
        if self.multiple_injections {
            return Err(SecretConfigError::InvalidAction);
        }
        let action = match (self.replacement, self.injection) {
            (Some(_), Some(_)) | (None, None) => return Err(SecretConfigError::InvalidAction),
            (Some(replacement), None) => {
                if replacement.conflicting_placeholder
                    || replacement.placeholder.is_empty()
                    || replacement.placeholder.len() > MAX_PLACEHOLDER_BYTES
                    || HeaderValue::from_str(&replacement.placeholder).is_err()
                    || (replacement.headers.is_empty()
                        && !replacement.query
                        && !replacement.path
                        && !replacement.body)
                {
                    return Err(SecretConfigError::InvalidPlaceholder);
                }
                let mut headers = Vec::new();
                for header in replacement.headers {
                    let header = HeaderName::from_bytes(header.as_bytes())
                        .map_err(|_| SecretConfigError::InvalidHeader)?;
                    if is_transport_owned_header(&header) {
                        return Err(SecretConfigError::InvalidHeader);
                    }
                    if !headers.contains(&header) {
                        headers.push(header);
                    }
                }
                SecretAction::Replace(SecretReplacement {
                    placeholder: replacement.placeholder,
                    headers,
                    query: replacement.query,
                    path: replacement.path,
                    body: replacement.body,
                    require: replacement.require,
                })
            }
            (None, Some(InjectionBuilder::Header(header, format))) => {
                let header = HeaderName::from_bytes(header.as_bytes())
                    .map_err(|_| SecretConfigError::InvalidHeader)?;
                if is_transport_owned_header(&header) {
                    return Err(SecretConfigError::InvalidHeader);
                }
                SecretAction::Inject(SecretInjection::Header(header, format))
            }
            (None, Some(InjectionBuilder::Query(parameter, format))) => {
                if parameter.is_empty()
                    || parameter.len() > MAX_IDENTIFIER_BYTES
                    || parameter.contains(['&', '=', '#', '\0'])
                {
                    return Err(SecretConfigError::InvalidQueryParameter);
                }
                SecretAction::Inject(SecretInjection::Query(parameter, format))
            }
        };
        let base_url_environment = self
            .base_url_environment
            .ok_or(SecretConfigError::MissingChildEnvironment)?;
        if !valid_environment_name(&base_url_environment) {
            return Err(SecretConfigError::InvalidEnvironment);
        }
        let placeholder_environment = match (&action, self.placeholder_environment) {
            (SecretAction::Replace(_), Some(environment)) => Some(environment),
            (SecretAction::Replace(_), None) => {
                return Err(SecretConfigError::MissingChildEnvironment);
            }
            (SecretAction::Inject(_), Some(_)) => {
                return Err(SecretConfigError::InvalidEnvironment);
            }
            (SecretAction::Inject(_), None) => None,
        };
        if placeholder_environment.as_ref().is_some_and(|environment| {
            !valid_environment_name(environment) || environment == &base_url_environment
        }) {
            return Err(SecretConfigError::InvalidEnvironment);
        }
        Ok(SecretRule {
            id: self.id,
            source: self.source,
            upstream,
            methods: self.methods,
            path_prefixes: self.path_prefixes,
            action,
            base_url_environment,
            placeholder_environment,
        })
    }
}

/// Policy for destinations not claimed by a secret rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnmatchedEgress {
    /// Reject unmatched destinations before contacting them.
    #[default]
    Deny,
    /// Forward unmatched destinations to later egress layers or the origin.
    Allow,
}

/// Host-side secret replacement as one independently composable egress layer.
#[derive(Clone)]
pub struct SecretEgress {
    resolver: Arc<dyn SecretResolver>,
    rules: Arc<[SecretRule]>,
    environment: EgressEnvironment,
    unmatched: UnmatchedEgress,
}

impl SecretEgress {
    /// Starts a fail-closed secret layer builder with one host-side resolver.
    #[must_use]
    pub fn builder<R>(resolver: R) -> SecretEgressBuilder
    where
        R: SecretResolver + 'static,
    {
        SecretEgressBuilder {
            resolver: Arc::new(resolver),
            rules: Vec::new(),
            unmatched: UnmatchedEgress::Deny,
        }
    }

    fn from_parts(
        resolver: Arc<dyn SecretResolver>,
        rules: Vec<SecretRule>,
        unmatched: UnmatchedEgress,
    ) -> Result<Self, SecretConfigError> {
        if rules.is_empty() {
            return Err(SecretConfigError::MissingRules);
        }
        let mut ids = BTreeMap::new();
        let mut environment = BTreeMap::new();
        for rule in &rules {
            if ids.insert(rule.id.clone(), ()).is_some() {
                return Err(SecretConfigError::DuplicateId(rule.id.clone()));
            }
            insert_environment(
                &mut environment,
                &rule.base_url_environment,
                rule.upstream().to_owned(),
            )?;
            if let (SecretAction::Replace(replacement), Some(placeholder_environment)) =
                (&rule.action, &rule.placeholder_environment)
            {
                insert_environment(
                    &mut environment,
                    placeholder_environment,
                    replacement.placeholder.clone(),
                )?;
            }
        }
        Ok(Self {
            resolver,
            rules: rules.into(),
            environment: EgressEnvironment::new(
                environment
                    .into_iter()
                    .map(|(name, value)| (name.into(), value.into())),
            ),
            unmatched,
        })
    }

    /// Returns child variables containing only public origins and placeholders.
    #[must_use]
    pub const fn environment(&self) -> &EgressEnvironment {
        &self.environment
    }
}

/// Builder for one host-owned secret replacement layer.
pub struct SecretEgressBuilder {
    resolver: Arc<dyn SecretResolver>,
    rules: Vec<SecretRule>,
    unmatched: UnmatchedEgress,
}

impl SecretEgressBuilder {
    /// Appends one validated destination-bound rule.
    #[must_use]
    pub fn rule(mut self, rule: SecretRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Appends validated destination-bound rules in declaration order.
    #[must_use]
    pub fn rules(mut self, rules: impl IntoIterator<Item = SecretRule>) -> Self {
        self.rules.extend(rules);
        self
    }

    /// Selects policy for destinations not claimed by a secret rule.
    #[must_use]
    pub const fn unmatched(mut self, unmatched: UnmatchedEgress) -> Self {
        self.unmatched = unmatched;
        self
    }

    /// Validates unique IDs and child variables and creates the layer.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no rules or when two rules claim the
    /// same ID or environment name.
    pub fn build(self) -> Result<SecretEgress, SecretConfigError> {
        SecretEgress::from_parts(self.resolver, self.rules, self.unmatched)
    }
}

impl SecretEgress {
    async fn authorize_http_request(
        &self,
        request: &mut reqwest::Request,
    ) -> Result<(), EgressLayerError> {
        let rules = match select_rules(&self.rules, request)? {
            RuleSelection::Apply(rules) => rules,
            RuleSelection::Unmatched if self.unmatched == UnmatchedEgress::Allow => {
                return Ok(());
            }
            RuleSelection::Unmatched => return Err(EgressLayerError::Denied),
        };
        for rule in rules {
            apply_rule(self.resolver.as_ref(), request, rule).await?;
            tracing::info!(
                target: "nanocodex_egress",
                secret_rule_id = %rule.id,
                http.request.method = %request.method(),
                "applied host-side secret policy"
            );
        }
        tracing::info!(
            target: "nanocodex_egress",
            content_kind = "egress.secret.request.headers",
            content = ?request.headers(),
            "trace content"
        );
        Ok(())
    }
}

#[async_trait]
impl EgressLayer for SecretEgress {
    async fn handle(
        &self,
        mut request: reqwest::Request,
        extensions: &mut ::http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        self.authorize_http_request(&mut request)
            .await
            .map_err(reqwest_middleware::Error::middleware)?;
        next.run(request, extensions).await
    }

    async fn authorize_connect(&self, request: &EgressRequest) -> Result<(), EgressLayerError> {
        authorize_connect_request(request, &self.rules, self.unmatched)
    }

    fn environment(&self) -> EgressEnvironment {
        self.environment.clone()
    }
}

fn authorize_connect_request(
    request: &EgressRequest,
    rules: &[SecretRule],
    unmatched: UnmatchedEgress,
) -> Result<(), EgressLayerError> {
    if unmatched == UnmatchedEgress::Allow {
        return Ok(());
    }
    let authority = request
        .uri()
        .authority()
        .cloned()
        .or_else(|| request.uri().to_string().parse().ok())
        .ok_or(EgressLayerError::InvalidRequest)?;
    rules
        .iter()
        .any(|rule| matching_upstream(rule, "https", &authority))
        .then_some(())
        .ok_or(EgressLayerError::Denied)
}

enum RuleSelection<'a> {
    Apply(Vec<&'a SecretRule>),
    Unmatched,
}

fn select_rules<'a>(
    rules: &'a [SecretRule],
    request: &reqwest::Request,
) -> Result<RuleSelection<'a>, EgressLayerError> {
    let path = safe_request_path(request.url().path()).ok_or(EgressLayerError::InvalidRequest)?;
    let mut origin_matched = false;
    let mut selected = Vec::new();
    for rule in rules {
        if !matching_upstream_url(rule, request.url()) {
            continue;
        }
        origin_matched = true;
        if !allows_request(rule, request.method(), &path) {
            continue;
        }
        selected.push(rule);
    }
    if !selected.is_empty() {
        Ok(RuleSelection::Apply(selected))
    } else if origin_matched {
        Err(EgressLayerError::Denied)
    } else {
        Ok(RuleSelection::Unmatched)
    }
}

fn matching_upstream(rule: &SecretRule, scheme: &str, authority: &Authority) -> bool {
    rule.upstream.scheme() == scheme
        && rule
            .upstream
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(authority.host()))
        && rule.upstream.port_or_known_default()
            == authority
                .port_u16()
                .or_else(|| (scheme == "https").then_some(443))
                .or_else(|| (scheme == "http").then_some(80))
}

fn matching_upstream_url(rule: &SecretRule, url: &Url) -> bool {
    rule.upstream.scheme() == url.scheme()
        && rule
            .upstream
            .host_str()
            .zip(url.host_str())
            .is_some_and(|(expected, actual)| expected.eq_ignore_ascii_case(actual))
        && rule.upstream.port_or_known_default() == url.port_or_known_default()
}

fn allows_request(rule: &SecretRule, method: &Method, path: &str) -> bool {
    let supported = matches!(
        *method,
        Method::GET
            | Method::POST
            | Method::PUT
            | Method::PATCH
            | Method::DELETE
            | Method::HEAD
            | Method::OPTIONS
    );
    let within_upstream = safe_request_path(rule.upstream.path())
        .is_some_and(|prefix| path_prefix_matches(&prefix, path));
    supported
        && within_upstream
        && (rule.methods.is_empty() || rule.methods.contains(method))
        && (rule.path_prefixes.is_empty()
            || rule
                .path_prefixes
                .iter()
                .any(|prefix| path_prefix_matches(prefix, path)))
}

async fn apply_rule(
    resolver: &dyn SecretResolver,
    request: &mut reqwest::Request,
    rule: &SecretRule,
) -> Result<(), EgressLayerError> {
    match &rule.action {
        SecretAction::Replace(replacement) => {
            let occurrences = replacement_occurrences(request, replacement)?;
            if occurrences == 0 {
                return if replacement.require {
                    Err(EgressLayerError::Denied)
                } else {
                    Ok(())
                };
            }
            let secret = resolve_secret(resolver, &rule.source).await?;
            apply_replacement(request, replacement, &secret)
        }
        SecretAction::Inject(injection) => {
            let secret = resolve_secret(resolver, &rule.source).await?;
            apply_injection(request, injection, &secret)
        }
    }
}

async fn resolve_secret(
    resolver: &dyn SecretResolver,
    source: &SecretRef,
) -> Result<String, EgressLayerError> {
    resolver
        .resolve(source)
        .await
        .map_err(|_| EgressLayerError::Unavailable)
}

fn replacement_occurrences(
    request: &reqwest::Request,
    replacement: &SecretReplacement,
) -> Result<usize, EgressLayerError> {
    let mut occurrences = 0;
    for header in &replacement.headers {
        for value in request.headers().get_all(header).iter() {
            let value = value.to_str().map_err(|_| EgressLayerError::Denied)?;
            let count = value.matches(&replacement.placeholder).count();
            // A child must not smuggle an alternate credential alongside the
            // public placeholder in a header claimed by this rule.
            if count == 0 {
                return Err(EgressLayerError::Denied);
            }
            occurrences += count;
        }
    }
    if replacement.query {
        occurrences += request
            .url()
            .query_pairs()
            .map(|(_, value)| value.matches(&replacement.placeholder).count())
            .sum::<usize>();
    }
    if replacement.path {
        occurrences += request
            .url()
            .path()
            .matches(&replacement.placeholder)
            .count();
    }
    if replacement.body
        && let Some(body) = request.body()
    {
        let body = body.as_bytes().ok_or(EgressLayerError::InvalidRequest)?;
        occurrences += count_bytes(body, replacement.placeholder.as_bytes());
    }
    Ok(occurrences)
}

fn apply_replacement(
    request: &mut reqwest::Request,
    replacement: &SecretReplacement,
    secret: &str,
) -> Result<(), EgressLayerError> {
    for header in &replacement.headers {
        let Entry::Occupied(mut entry) = request.headers_mut().entry(header.clone()) else {
            continue;
        };
        for value in entry.iter_mut() {
            let current = value.to_str().map_err(|_| EgressLayerError::Denied)?;
            *value = HeaderValue::from_str(&current.replace(&replacement.placeholder, secret))
                .map_err(|_| EgressLayerError::Unavailable)?;
        }
    }
    if replacement.query && request.url().query().is_some() {
        let pairs = request
            .url()
            .query_pairs()
            .map(|(key, value)| {
                (
                    key.into_owned(),
                    value.replace(&replacement.placeholder, secret),
                )
            })
            .collect::<Vec<_>>();
        request
            .url_mut()
            .query_pairs_mut()
            .clear()
            .extend_pairs(pairs);
    }
    if replacement.path {
        let path = request
            .url()
            .path()
            .replace(&replacement.placeholder, secret);
        request.url_mut().set_path(&path);
    }
    if replacement.body {
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or(EgressLayerError::InvalidRequest)?;
        let body = replace_bytes(body, replacement.placeholder.as_bytes(), secret.as_bytes());
        *request.body_mut() = Some(reqwest::Body::from(body));
    }
    Ok(())
}

fn apply_injection(
    request: &mut reqwest::Request,
    injection: &SecretInjection,
    secret: &str,
) -> Result<(), EgressLayerError> {
    match injection {
        SecretInjection::Header(header, format) => {
            let value = HeaderValue::from_str(&format.apply(secret))
                .map_err(|_| EgressLayerError::Unavailable)?;
            request.headers_mut().insert(header, value);
        }
        SecretInjection::Query(parameter, format) => {
            let value = format.apply(secret);
            let mut pairs = request
                .url()
                .query_pairs()
                .filter(|(key, _)| key != parameter)
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            pairs.push((parameter.clone(), value));
            request
                .url_mut()
                .query_pairs_mut()
                .clear()
                .extend_pairs(pairs);
        }
    }
    Ok(())
}

fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(haystack.len());
    let mut remaining = haystack;
    while let Some(index) = remaining
        .windows(needle.len())
        .position(|window| window == needle)
    {
        output.extend_from_slice(&remaining[..index]);
        output.extend_from_slice(replacement);
        remaining = &remaining[index + needle.len()..];
    }
    output.extend_from_slice(remaining);
    output
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

fn validate_identifier(value: &str) -> Option<()> {
    (!value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then_some(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_uppercase())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn is_transport_owned_header(header: &HeaderName) -> bool {
    [
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "proxy-authorization",
    ]
    .iter()
    .any(|reserved| header.as_str().eq_ignore_ascii_case(reserved))
}

fn insert_environment(
    environment: &mut BTreeMap<String, String>,
    name: &str,
    value: String,
) -> Result<(), SecretConfigError> {
    if environment.get(name) == Some(&value) {
        return Ok(());
    }
    if environment.insert(name.to_owned(), value).is_some() {
        return Err(SecretConfigError::DuplicateEnvironment(name.to_owned()));
    }
    Ok(())
}

/// Invalid secret-rule configuration.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SecretConfigError {
    /// A secret layer was configured without any destination-bound rules.
    #[error("secret egress requires at least one rule")]
    MissingRules,
    /// A rule ID was empty, unbounded, or contained unsupported bytes.
    #[error("secret rule id must be a bounded ASCII identifier")]
    InvalidId,
    /// A resolver provider or key was empty or unbounded.
    #[error("secret source must contain a bounded provider and key")]
    InvalidSource,
    /// An upstream was not a credential-free absolute HTTP(S) base URL.
    #[error("secret upstream must be a credential-free absolute HTTP(S) base URL")]
    InvalidUpstream,
    /// A remote plaintext upstream could expose the resolved credential.
    #[error("secret upstream must use HTTPS unless it is loopback")]
    InsecureUpstream,
    /// A CONNECT or extension method was configured for replacement.
    #[error("secret rules support ordinary HTTP methods only")]
    InvalidMethod,
    /// A path prefix was relative or ambiguously encoded.
    #[error("secret path prefixes must be safe bounded absolute paths")]
    InvalidPathPrefix,
    /// No action or more than one action was configured.
    #[error("secret rule requires exactly one injection or replacement action")]
    InvalidAction,
    /// A header name was invalid or transport-owned.
    #[error("secret replacement header is invalid or transport-owned")]
    InvalidHeader,
    /// An injected query parameter name was empty, unbounded, or ambiguous.
    #[error("secret query parameter name is invalid")]
    InvalidQueryParameter,
    /// A placeholder was empty, unbounded, or not a valid header value.
    #[error("secret placeholder must be a non-empty bounded HTTP header value")]
    InvalidPlaceholder,
    /// Child base URL and placeholder variables were not configured.
    #[error("secret rule requires child base URL and placeholder variables")]
    MissingChildEnvironment,
    /// A child variable was invalid or both values used the same name.
    #[error("secret child variables must be distinct uppercase shell identifiers")]
    InvalidEnvironment,
    /// Two rules used the same stable ID.
    #[error("duplicate secret rule id `{0}`")]
    DuplicateId(String),
    /// Two rules claimed the same child variable.
    #[error("duplicate secret child environment `{0}`")]
    DuplicateEnvironment(String),
}

#[cfg(test)]
mod tests {
    use hudsucker::hyper::{HeaderMap, Uri};

    use super::*;

    #[test]
    fn secret_layer_rejects_an_empty_rule_set() {
        let result = SecretEgress::builder(StaticSecretResolver::new()).build();
        assert!(matches!(result, Err(SecretConfigError::MissingRules)));
    }

    #[test]
    fn replacement_rule_exports_only_public_child_values() {
        let rule = SecretRule::builder(
            "openai",
            SecretRef::new("environment", "OPENAI_API_KEY"),
            "https://api.openai.com",
        )
        .method(Method::POST)
        .path_prefix("/v1/responses")
        .replace_header("authorization", "nanocodex-secret-openai")
        .child_environment("OPENAI_BASE_URL", "OPENAI_API_KEY")
        .build()
        .unwrap();
        struct Unavailable;
        #[async_trait]
        impl SecretResolver for Unavailable {
            async fn resolve(&self, _reference: &SecretRef) -> Result<String, SecretResolverError> {
                Err(SecretResolverError::Unavailable)
            }
        }
        let rules = SecretEgress::builder(Unavailable)
            .rule(rule)
            .build()
            .unwrap();
        assert_eq!(
            rules.environment().get("OPENAI_BASE_URL"),
            Some(std::ffi::OsStr::new("https://api.openai.com"))
        );
        assert_eq!(
            rules.environment().get("OPENAI_API_KEY"),
            Some(std::ffi::OsStr::new("nanocodex-secret-openai"))
        );
    }

    #[test]
    fn rules_reject_ambiguous_paths_and_placeholders() {
        let build = |path: &str, placeholder: &str| {
            SecretRule::builder(
                "openai",
                SecretRef::new("environment", "OPENAI_API_KEY"),
                "https://api.openai.com",
            )
            .path_prefix(path)
            .replace_header("authorization", placeholder)
            .child_environment("OPENAI_BASE_URL", "OPENAI_API_KEY")
            .build()
        };
        assert_eq!(
            build("/v1/%2e%2e/admin", "placeholder").unwrap_err(),
            SecretConfigError::InvalidPathPrefix
        );
        assert_eq!(
            build("/v1/responses", "").unwrap_err(),
            SecretConfigError::InvalidPlaceholder
        );

        let insecure = SecretRule::builder(
            "openai",
            SecretRef::new("environment", "OPENAI_API_KEY"),
            "http://api.openai.com",
        )
        .replace_header("authorization", "placeholder")
        .child_environment("OPENAI_BASE_URL", "OPENAI_API_KEY")
        .build();
        assert_eq!(insecure.unwrap_err(), SecretConfigError::InsecureUpstream);

        let multiple_injections = SecretRule::builder(
            "invalid",
            SecretRef::new("memory", "secret"),
            "https://api.example.com",
        )
        .inject_header("authorization", SecretFormat::Bearer)
        .inject_query("key", SecretFormat::Raw)
        .child_base_url_environment("SERVICE_BASE_URL")
        .build();
        assert_eq!(
            multiple_injections.unwrap_err(),
            SecretConfigError::InvalidAction
        );
    }

    #[tokio::test]
    async fn duplicate_replacement_headers_fail_closed() {
        let reference = SecretRef::new("memory", "openai");
        let rule = SecretRule::builder("openai", reference.clone(), "https://api.openai.com")
            .method(Method::POST)
            .path_prefix("/v1/responses")
            .replace_header("authorization", "nanocodex-secret-openai")
            .child_environment("OPENAI_BASE_URL", "OPENAI_API_KEY")
            .build()
            .unwrap();
        let layer =
            SecretEgress::builder(StaticSecretResolver::new().with_secret(reference, "host-only"))
                .rule(rule)
                .build()
                .unwrap();
        let mut request = reqwest::Request::new(
            Method::POST,
            Url::parse("https://api.openai.com/v1/responses").unwrap(),
        );
        request.headers_mut().append(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer nanocodex-secret-openai"),
        );
        request.headers_mut().append(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer attacker-controlled"),
        );

        assert_eq!(
            layer.authorize_http_request(&mut request).await,
            Err(EgressLayerError::Denied)
        );
    }

    #[tokio::test]
    async fn replaces_every_configured_request_location() {
        let reference = SecretRef::new("memory", "service");
        let rule = SecretRule::builder("service", reference.clone(), "https://api.example.com/v1")
            .method(Method::POST)
            .replace_header("x-api-key", "public-token")
            .replace_query("public-token")
            .replace_path("public-token")
            .replace_body("public-token")
            .child_environment("SERVICE_BASE_URL", "SERVICE_TOKEN")
            .build()
            .unwrap();
        let layer =
            SecretEgress::builder(StaticSecretResolver::new().with_secret(reference, "host-only"))
                .rule(rule)
                .build()
                .unwrap();
        let mut request = reqwest::Request::new(
            Method::POST,
            Url::parse(
                "https://api.example.com/v1/public-token/action?key=public-token&keep=value",
            )
            .unwrap(),
        );
        request.headers_mut().insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_static("prefix-public-token-suffix"),
        );
        *request.body_mut() = Some(reqwest::Body::from("body=public-token"));

        layer.authorize_http_request(&mut request).await.unwrap();

        assert_eq!(request.url().path(), "/v1/host-only/action");
        assert_eq!(
            request
                .url()
                .query_pairs()
                .collect::<BTreeMap<_, _>>()
                .get("key")
                .map(Cow::as_ref),
            Some("host-only")
        );
        assert_eq!(request.headers()["x-api-key"], "prefix-host-only-suffix");
        assert_eq!(
            request.body().and_then(reqwest::Body::as_bytes),
            Some(b"body=host-only".as_slice())
        );
    }

    #[tokio::test]
    async fn applies_multiple_injections_for_one_request() {
        let bearer = SecretRef::new("memory", "bearer");
        let basic = SecretRef::new("memory", "basic");
        let query = SecretRef::new("memory", "query");
        let resolver = StaticSecretResolver::new()
            .with_secret(bearer.clone(), "bearer-secret")
            .with_secret(basic.clone(), "basic-secret")
            .with_secret(query.clone(), "query-secret");
        let rule = |id: &str, source, action: fn(SecretRuleBuilder) -> SecretRuleBuilder| {
            action(SecretRule::builder(
                id,
                source,
                "https://api.example.com/v1",
            ))
            .child_base_url_environment("SERVICE_BASE_URL")
            .build()
            .unwrap()
        };
        let layer = SecretEgress::builder(resolver)
            .rule(rule("bearer", bearer, |builder| {
                builder.inject_header("authorization", SecretFormat::Bearer)
            }))
            .rule(rule("basic", basic, |builder| {
                builder.inject_header(
                    "x-basic",
                    SecretFormat::Basic {
                        username: "agent".to_owned(),
                    },
                )
            }))
            .rule(rule("query", query, |builder| {
                builder.inject_query("api_key", SecretFormat::Raw)
            }))
            .build()
            .unwrap();
        let mut request = reqwest::Request::new(
            Method::GET,
            Url::parse("https://api.example.com/v1/data?api_key=child-value").unwrap(),
        );
        request.headers_mut().insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer child-value"),
        );

        layer.authorize_http_request(&mut request).await.unwrap();

        assert_eq!(request.headers()["authorization"], "Bearer bearer-secret");
        assert_eq!(
            request.headers()["x-basic"],
            "Basic YWdlbnQ6YmFzaWMtc2VjcmV0"
        );
        assert!(request.url().query().is_some_and(|query| {
            query.contains("api_key=query-secret") && !query.contains("child-value")
        }));
    }

    #[tokio::test]
    async fn optional_replacement_does_not_resolve_an_unused_secret() {
        struct Unavailable;
        #[async_trait]
        impl SecretResolver for Unavailable {
            async fn resolve(&self, _reference: &SecretRef) -> Result<String, SecretResolverError> {
                Err(SecretResolverError::Unavailable)
            }
        }
        let rule = SecretRule::builder(
            "optional",
            SecretRef::new("unavailable", "secret"),
            "https://api.example.com",
        )
        .optional_replacement()
        .replace_query("public-token")
        .child_environment("SERVICE_BASE_URL", "SERVICE_TOKEN")
        .build()
        .unwrap();
        let layer = SecretEgress::builder(Unavailable)
            .rule(rule)
            .build()
            .unwrap();
        let mut request = reqwest::Request::new(
            Method::GET,
            Url::parse("https://api.example.com/data?anonymous=true").unwrap(),
        );

        layer.authorize_http_request(&mut request).await.unwrap();
    }

    #[tokio::test]
    async fn connect_policy_only_opens_configured_tls_origins() {
        let rule = SecretRule::builder(
            "openai",
            SecretRef::new("environment", "OPENAI_API_KEY"),
            "https://api.openai.com",
        )
        .replace_header("authorization", "nanocodex-secret-openai")
        .child_environment("OPENAI_BASE_URL", "OPENAI_API_KEY")
        .build()
        .unwrap();
        let layer = SecretEgress::builder(StaticSecretResolver::new())
            .rule(rule)
            .build()
            .unwrap();
        let request = |authority: &'static str| {
            EgressRequest::new(
                Method::CONNECT,
                Uri::from_static(authority),
                HeaderMap::new(),
            )
        };

        assert!(
            layer
                .authorize_connect(&request("api.openai.com:443"))
                .await
                .is_ok()
        );
        assert_eq!(
            layer
                .authorize_connect(&request("attacker.invalid:443"))
                .await,
            Err(EgressLayerError::Denied)
        );
    }
}
