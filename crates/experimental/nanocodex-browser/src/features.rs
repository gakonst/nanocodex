use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::BrowserSourceLocation;

/// Deterministic policy installed on every page before its first navigation.
///
/// This is harness policy rather than a model-callable action. Header and HTTP
/// credential values therefore never enter the browser tool schema or action
/// results. When browser tracing is enabled, the complete policy is recorded
/// as credential-bearing operational data.
#[derive(Clone, Default)]
pub struct BrowserContext {
    pub(crate) viewport: Option<BrowserViewport>,
    pub(crate) locale: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) user_agent: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) accept_language: Option<String>,
    pub(crate) color_scheme: Option<BrowserColorScheme>,
    pub(crate) reduced_motion: Option<BrowserReducedMotion>,
    pub(crate) geolocation: Option<BrowserGeolocation>,
    pub(crate) permissions: Vec<BrowserPermissionGrant>,
    pub(crate) extra_headers: Vec<(String, String)>,
    pub(crate) http_credentials: Option<(String, String)>,
    pub(crate) init_scripts: Vec<String>,
    pub(crate) cpu_throttle_rate: Option<f64>,
    pub(crate) network: Option<BrowserNetworkConditions>,
}

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserContext")
            .field("viewport", &self.viewport)
            .field("locale", &self.locale)
            .field("timezone", &self.timezone)
            .field("user_agent", &self.user_agent)
            .field("platform", &self.platform)
            .field("accept_language", &self.accept_language)
            .field("color_scheme", &self.color_scheme)
            .field("reduced_motion", &self.reduced_motion)
            .field("geolocation", &self.geolocation)
            .field("permissions", &self.permissions)
            .field(
                "extra_header_names",
                &self
                    .extra_headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "http_credentials",
                &self.http_credentials.as_ref().map(|_| "[configured]"),
            )
            .field("init_script_count", &self.init_scripts.len())
            .field("cpu_throttle_rate", &self.cpu_throttle_rate)
            .field("network", &self.network)
            .finish()
    }
}

impl BrowserContext {
    /// Sets the viewport and input-device emulation.
    #[must_use]
    pub const fn viewport(mut self, viewport: BrowserViewport) -> Self {
        self.viewport = Some(viewport);
        self
    }

    /// Overrides `navigator.language` and the browser locale.
    #[must_use]
    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = Some(locale.into());
        self
    }

    /// Overrides the page timezone with an IANA timezone identifier.
    #[must_use]
    pub fn timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }

    /// Overrides the HTTP and JavaScript user-agent string.
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Overrides `navigator.platform`.
    #[must_use]
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = Some(platform.into());
        self
    }

    /// Overrides the `Accept-Language` request header.
    #[must_use]
    pub fn accept_language(mut self, accept_language: impl Into<String>) -> Self {
        self.accept_language = Some(accept_language.into());
        self
    }

    /// Overrides the `prefers-color-scheme` media feature.
    #[must_use]
    pub const fn color_scheme(mut self, color_scheme: BrowserColorScheme) -> Self {
        self.color_scheme = Some(color_scheme);
        self
    }

    /// Overrides the `prefers-reduced-motion` media feature.
    #[must_use]
    pub const fn reduced_motion(mut self, reduced_motion: BrowserReducedMotion) -> Self {
        self.reduced_motion = Some(reduced_motion);
        self
    }

    /// Supplies a fixed geolocation to pages granted geolocation permission.
    #[must_use]
    pub const fn geolocation(mut self, geolocation: BrowserGeolocation) -> Self {
        self.geolocation = Some(geolocation);
        self
    }

    /// Grants one permission, optionally scoped to a single origin.
    #[must_use]
    pub fn grant_permission(mut self, permission: BrowserPermission, origin: Option<Url>) -> Self {
        self.permissions
            .push(BrowserPermissionGrant { permission, origin });
        self
    }

    /// Adds one request header to every page request.
    #[must_use]
    pub fn extra_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    /// Configures HTTP basic-authentication credentials.
    #[must_use]
    pub fn http_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.http_credentials = Some((username.into(), password.into()));
        self
    }

    /// Evaluates JavaScript before application scripts on every page.
    #[must_use]
    pub fn init_script(mut self, source: impl Into<String>) -> Self {
        self.init_scripts.push(source.into());
        self
    }

    /// Applies a deterministic CPU slowdown multiplier.
    #[must_use]
    pub const fn cpu_throttle_rate(mut self, rate: f64) -> Self {
        self.cpu_throttle_rate = Some(rate);
        self
    }

    /// Applies deterministic offline, latency, and bandwidth conditions.
    #[must_use]
    pub const fn network(mut self, conditions: BrowserNetworkConditions) -> Self {
        self.network = Some(conditions);
        self
    }
}

/// Viewport and input-device emulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub mobile: bool,
    pub touch: bool,
    pub max_touch_points: u8,
}

impl BrowserViewport {
    /// Creates a desktop viewport with no touch emulation.
    #[must_use]
    pub const fn desktop(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            device_scale_factor: 1.0,
            mobile: false,
            touch: false,
            max_touch_points: 0,
        }
    }

    /// Creates a mobile viewport with touch emulation.
    #[must_use]
    pub const fn mobile(width: u32, height: u32, device_scale_factor: f64) -> Self {
        Self {
            width,
            height,
            device_scale_factor,
            mobile: true,
            touch: true,
            max_touch_points: 5,
        }
    }
}

/// A named, reproducible mobile device profile.
///
/// Profiles emulate the device's layout, pixel density, user agent, platform,
/// and touch capabilities in Chromium. They do not turn Chromium into Safari;
/// [`BrowserMobileState::engine`] always reports the actual browser engine.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDevicePreset {
    IphoneSe,
    Iphone15Pro,
    Pixel8,
    GalaxyS24,
    IpadMini,
}

impl BrowserDevicePreset {
    /// Returns the pinned descriptor behind this preset.
    #[must_use]
    pub fn descriptor(self, orientation: BrowserOrientation) -> BrowserDeviceDescriptor {
        let (name, width, height, device_scale_factor, user_agent, platform) = match self {
            Self::IphoneSe => (
                "iPhone SE",
                375,
                667,
                2.0,
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                "iPhone",
            ),
            Self::Iphone15Pro => (
                "iPhone 15 Pro",
                393,
                852,
                3.0,
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                "iPhone",
            ),
            Self::Pixel8 => (
                "Pixel 8",
                412,
                915,
                2.625,
                "Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Mobile Safari/537.36",
                "Linux armv8l",
            ),
            Self::GalaxyS24 => (
                "Galaxy S24",
                360,
                780,
                3.0,
                "Mozilla/5.0 (Linux; Android 14; SM-S921B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Mobile Safari/537.36",
                "Linux armv8l",
            ),
            Self::IpadMini => (
                "iPad mini",
                744,
                1133,
                2.0,
                "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                "iPad",
            ),
        };
        let (width, height) = match orientation {
            BrowserOrientation::Portrait => (width, height),
            BrowserOrientation::Landscape => (height, width),
        };
        BrowserDeviceDescriptor {
            preset: self,
            name: name.to_owned(),
            orientation,
            width,
            height,
            device_scale_factor,
            user_agent: user_agent.to_owned(),
            platform: platform.to_owned(),
            mobile: true,
            touch: true,
            max_touch_points: 5,
        }
    }
}

/// Orientation applied to a mobile device profile.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserOrientation {
    #[default]
    Portrait,
    Landscape,
}

/// Complete pinned settings for one emulated device.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDeviceDescriptor {
    pub preset: BrowserDevicePreset,
    pub name: String,
    pub orientation: BrowserOrientation,
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub user_agent: String,
    pub platform: String,
    pub mobile: bool,
    pub touch: bool,
    pub max_touch_points: u8,
}

impl BrowserDeviceDescriptor {
    pub(crate) const fn viewport(&self) -> BrowserViewport {
        BrowserViewport {
            width: self.width,
            height: self.height,
            device_scale_factor: self.device_scale_factor,
            mobile: self.mobile,
            touch: self.touch,
            max_touch_points: self.max_touch_points,
        }
    }
}

/// Browser-visible state used to prove that mobile emulation actually took effect.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserMobileState {
    pub provider: String,
    pub engine: String,
    pub url: String,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub visual_viewport_width: Option<f64>,
    pub visual_viewport_height: Option<f64>,
    pub visual_viewport_scale: Option<f64>,
    pub screen_width: f64,
    pub screen_height: f64,
    pub device_pixel_ratio: f64,
    pub user_agent: String,
    pub platform: String,
    pub max_touch_points: u64,
    pub coarse_pointer: bool,
    pub no_hover: bool,
    pub orientation: String,
    pub meta_viewport: Option<String>,
    pub verified: bool,
    pub mismatches: Vec<String>,
}

/// Severity of a deterministic mobile usability finding.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMobileFindingSeverity {
    Error,
    Warning,
}

/// One concrete mobile layout, input, or viewport problem.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserMobileFinding {
    pub rule: String,
    pub severity: BrowserMobileFindingSeverity,
    pub message: String,
    pub selector: Option<String>,
    pub measured_width: Option<f64>,
    pub measured_height: Option<f64>,
}

/// Results for one device/orientation in a mobile audit matrix.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserMobileAuditSample {
    /// Present for Chromium emulation and absent for real-device providers.
    pub device: Option<BrowserDeviceDescriptor>,
    pub device_name: String,
    pub state: BrowserMobileState,
    pub document_width: f64,
    pub horizontal_overflow: f64,
    pub interactive_elements: usize,
    pub findings: Vec<BrowserMobileFinding>,
}

/// A bounded, deterministic audit across mobile device profiles.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserMobileAudit {
    pub url: String,
    pub provider: String,
    pub samples: Vec<BrowserMobileAuditSample>,
    pub error_count: usize,
    pub warning_count: usize,
    pub passed: bool,
}

/// CSS `prefers-color-scheme` override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserColorScheme {
    Light,
    Dark,
}

/// CSS `prefers-reduced-motion` override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserReducedMotion {
    NoPreference,
    Reduce,
}

/// Fixed geolocation delivered to the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserGeolocation {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_meters: f64,
}

/// Permission that the harness can grant before navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPermission {
    Geolocation,
    ClipboardReadWrite,
    Notifications,
    Camera,
    Microphone,
    Midi,
    PointerLock,
    PaymentHandler,
}

#[derive(Clone, Debug)]
pub(crate) struct BrowserPermissionGrant {
    pub(crate) permission: BrowserPermission,
    pub(crate) origin: Option<Url>,
}

/// Fixed network conditions installed before navigation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserNetworkConditions {
    pub offline: bool,
    pub latency_ms: f64,
    pub download_bytes_per_second: f64,
    pub upload_bytes_per_second: f64,
}

impl BrowserNetworkConditions {
    /// Creates an online connection without latency or bandwidth limits.
    #[must_use]
    pub const fn unrestricted() -> Self {
        Self {
            offline: false,
            latency_ms: 0.0,
            download_bytes_per_second: -1.0,
            upload_bytes_per_second: -1.0,
        }
    }
}

/// Harness-owned client policy for Chrome UX Report field data.
///
/// The API key never enters the browser action schema, result stream, or
/// `Debug` output.
#[derive(Clone)]
pub struct BrowserCruxClient {
    pub(crate) api_key: String,
    pub(crate) endpoint: Option<Url>,
}

impl std::fmt::Debug for BrowserCruxClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserCruxClient")
            .field("api_key", &"[configured]")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl BrowserCruxClient {
    /// Configures a Chrome UX Report API key.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            endpoint: None,
        }
    }

    /// Overrides the official API endpoint.
    ///
    /// This is primarily useful for a controlled proxy or deterministic test
    /// server. The API key remains a query parameter at that boundary.
    #[must_use]
    pub fn endpoint(mut self, endpoint: Url) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
}

/// File-backed, replayable record of one ordered browser interaction session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSessionTrace {
    pub directory: PathBuf,
    pub events_path: PathBuf,
    pub network_path: PathBuf,
    pub diagnostics_path: PathBuf,
    pub action_count: usize,
    pub screenshot_count: usize,
    pub dom_snapshot_count: usize,
    pub duration_ms: u64,
    pub truncated: bool,
}

/// Zero-based source range for one CSS rule or declaration.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCssSourceRange {
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
}

/// One authored CSS declaration and its parser/cascade metadata.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans preserve independent authored CSS declaration flags from DevTools"
)]
pub struct BrowserCssProperty {
    pub name: String,
    pub value: String,
    pub important: bool,
    pub implicit: bool,
    pub parsed: bool,
    pub disabled: bool,
    pub range: Option<BrowserCssSourceRange>,
}

/// One matching authored CSS rule.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCssRule {
    pub selector: String,
    pub matching_selectors: Vec<String>,
    pub origin: String,
    pub style_sheet_id: Option<String>,
    pub source_url: Option<String>,
    pub range: Option<BrowserCssSourceRange>,
    pub properties: Vec<BrowserCssProperty>,
    /// Zero is the selected element's parent, one is its grandparent, and so on.
    pub inherited_from: Option<u32>,
    pub pseudo: Option<String>,
}

/// Complete authored style provenance for one element.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserMatchedStyles {
    pub inline_style: Vec<BrowserCssProperty>,
    pub attributes_style: Vec<BrowserCssProperty>,
    pub rules: Vec<BrowserCssRule>,
}

/// Native event listener attached to an element or its pierced subtree.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserEventListener {
    pub event_type: String,
    pub use_capture: bool,
    pub passive: bool,
    pub once: bool,
    pub script_id: String,
    /// One-based source line.
    pub line_number: u64,
    /// One-based source column.
    pub column_number: u64,
}

/// CSS pseudo-class that Chromium can force on an element.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPseudoClass {
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Hover,
    Target,
    Visited,
}

impl BrowserPseudoClass {
    pub(crate) const fn as_css(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Focus => "focus",
            Self::FocusVisible => "focus-visible",
            Self::FocusWithin => "focus-within",
            Self::Hover => "hover",
            Self::Target => "target",
            Self::Visited => "visited",
        }
    }
}

/// Pause policy installed in Chromium's JavaScript debugger.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPauseOnExceptions {
    None,
    Uncaught,
    All,
}

/// One JavaScript breakpoint installed by URL and source location.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserBreakpoint {
    pub breakpoint_id: String,
    pub url: String,
    /// One-based requested line.
    pub line_number: u64,
    /// One-based requested column.
    pub column_number: u64,
    pub resolved_locations: Vec<BrowserSourceLocation>,
}

/// One lexical scope visible from a paused JavaScript call frame.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDebuggerScope {
    pub kind: String,
    pub name: Option<String>,
    pub object_type: String,
    pub object_description: Option<String>,
}

/// One JavaScript frame retained from a debugger pause.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDebuggerFrame {
    pub function_name: String,
    pub location: BrowserSourceLocation,
    pub original: Option<BrowserSourceLocation>,
    pub scopes: Vec<BrowserDebuggerScope>,
}

/// Latest JavaScript pause observed by the active page.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDebuggerPause {
    pub sequence: u64,
    pub reason: String,
    pub hit_breakpoints: Vec<String>,
    pub frames: Vec<BrowserDebuggerFrame>,
}

/// One active service-worker registration.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserServiceWorker {
    pub scope: String,
    pub script_url: Option<String>,
    pub state: Option<String>,
}

/// One request retained by the Cache Storage API.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCacheEntry {
    pub method: String,
    pub url: String,
}

/// One named Cache Storage bucket and its request keys.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCache {
    pub name: String,
    pub entries: Vec<BrowserCacheEntry>,
}

/// One origin-local `IndexedDB` database.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserIndexedDbDatabase {
    pub name: String,
    pub version: u64,
}

/// Browser-managed worker and origin storage metadata.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserStorageReport {
    pub origin: String,
    pub service_workers: Vec<BrowserServiceWorker>,
    pub caches: Vec<BrowserCache>,
    pub indexed_db: Vec<BrowserIndexedDbDatabase>,
    pub local_storage_keys: Vec<String>,
    pub session_storage_keys: Vec<String>,
}

/// `SameSite` policy retained in a harness-owned browser state snapshot.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCookieSameSite {
    Strict,
    Lax,
    None,
}

/// One credential-bearing cookie in a harness-owned browser state snapshot.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires_epoch_seconds: Option<f64>,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<BrowserCookieSameSite>,
}

impl std::fmt::Debug for BrowserCookie {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserCookie")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("expires_epoch_seconds", &self.expires_epoch_seconds)
            .field("http_only", &self.http_only)
            .field("secure", &self.secure)
            .field("same_site", &self.same_site)
            .finish()
    }
}

/// Local and session storage captured for one open origin.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserOriginStorage {
    pub origin: String,
    pub local_storage: std::collections::BTreeMap<String, String>,
    pub session_storage: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for BrowserOriginStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserOriginStorage")
            .field("origin", &self.origin)
            .field(
                "local_storage_keys",
                &self.local_storage.keys().collect::<Vec<_>>(),
            )
            .field(
                "session_storage_keys",
                &self.session_storage.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Credential-bearing browser cookies and per-origin storage owned by the
/// embedding harness.
///
/// This type is never part of [`crate::BrowserAction`] or the browser tool
/// schema.
#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserStorageState {
    pub cookies: Vec<BrowserCookie>,
    pub origins: Vec<BrowserOriginStorage>,
}

impl std::fmt::Debug for BrowserStorageState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserStorageState")
            .field("cookie_count", &self.cookies.len())
            .field(
                "origins",
                &self
                    .origins
                    .iter()
                    .map(|origin| origin.origin.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Evidence attached to successful mutating browser actions.
///
/// Summary state is always captured for a real browser. `Snapshot` additionally
/// refreshes semantic element references after each action so an agent can
/// continue without a separate snapshot round trip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserAfterAction {
    /// Return page, network, console, error, dialog, and download deltas.
    #[default]
    Summary,
    /// Return summary state plus a compact interactive accessibility snapshot.
    Snapshot,
}

/// Network destinations a managed browser may contact.
///
/// An empty policy denies every network destination while retaining local
/// `about:`, `data:`, and `blob:` documents. Add exact origins or explicit
/// domain suffixes deliberately. This policy is enforced at the `DevTools`
/// request boundary for the page, frames, and workers.
#[derive(Clone, Debug, Default)]
pub struct BrowserEgressPolicy {
    pub(crate) allowed_origins: Vec<Url>,
    pub(crate) allowed_domain_suffixes: Vec<String>,
    pub(crate) allow_loopback: bool,
}

impl BrowserEgressPolicy {
    /// Starts a deny-by-default egress policy.
    #[must_use]
    pub const fn deny_by_default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_domain_suffixes: Vec::new(),
            allow_loopback: false,
        }
    }

    /// Allows one exact origin, including its scheme and effective port.
    #[must_use]
    pub fn allow_origin(mut self, origin: Url) -> Self {
        self.allowed_origins.push(origin);
        self
    }

    /// Allows a DNS name and all of its subdomains.
    ///
    /// The suffix must be a hostname rather than a URL. `example.com` matches
    /// both `example.com` and `api.example.com`, but not
    /// `notexample.com`.
    #[must_use]
    pub fn allow_domain(mut self, suffix: impl Into<String>) -> Self {
        self.allowed_domain_suffixes.push(suffix.into());
        self
    }

    /// Allows loopback hosts such as `127.0.0.1`, `::1`, and `localhost`.
    #[must_use]
    pub const fn allow_loopback(mut self, allow: bool) -> Self {
        self.allow_loopback = allow;
        self
    }
}

/// One frame that can be targeted explicitly by frame-aware actions.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserFrame {
    /// Chromium's session-stable frame identifier.
    pub frame_id: String,
    pub parent_frame_id: Option<String>,
    pub name: Option<String>,
    pub url: String,
    pub main: bool,
}

/// One open page in the owned browser process.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserTab {
    /// Chromium target identifier accepted by tab actions.
    pub tab_id: String,
    pub title: String,
    pub url: String,
    pub active: bool,
}

/// JavaScript dialog kind reported by Chromium.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
    Other,
}

/// Pending JavaScript dialog state.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDialog {
    pub kind: BrowserDialogKind,
    pub message: String,
    pub default_prompt: String,
    pub url: String,
}

/// A browser screenshot retained in the session-private artifact directory.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserImageArtifact {
    /// Opaque session identifier accepted by visual comparison actions.
    pub artifact_id: String,
    pub path: PathBuf,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    /// Present only at the Code Mode tool boundary.
    ///
    /// Direct Rust consumers keep file-backed artifacts and do not pay for a
    /// duplicate base64 value. Code Mode can pass this value directly to its
    /// `image(...)` output helper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_image: Option<BrowserModelImage>,
}

/// A model-visible image accepted by Code Mode's `image(...)` helper.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserModelImage {
    pub image_url: String,
}

/// Pixel-level comparison of two browser image artifacts.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserVisualDiff {
    pub baseline_id: String,
    pub current_id: String,
    pub changed_pixel_ratio: f64,
    pub mean_channel_delta: f64,
    pub maximum_channel_delta: u8,
    pub dimensions_match: bool,
    pub diff_path: PathBuf,
    /// Present only at the Code Mode tool boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_image: Option<BrowserModelImage>,
}

/// Visual instability classified from consecutive rendered frames.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserVisualAnomalyKind {
    Flash,
    BlankFrame,
    LargeVisualChange,
}

/// One automatically detected visual instability.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserVisualAnomaly {
    pub kind: BrowserVisualAnomalyKind,
    pub frame_index: usize,
    pub elapsed_ms: u64,
    pub changed_pixel_ratio: f64,
    pub luminance_delta: f64,
    pub frame_path: PathBuf,
    /// Present only at the Code Mode tool boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_image: Option<BrowserModelImage>,
}

/// Summary of a bounded visual trace.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserVisualTrace {
    pub frame_count: usize,
    pub duration_ms: u64,
    pub dropped_frames: u64,
    pub maximum_changed_pixel_ratio: f64,
    pub cumulative_layout_shift: f64,
    pub anomalies: Vec<BrowserVisualAnomaly>,
}

/// Core Web Vitals and high-signal navigation performance state.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserWebVitals {
    pub url: String,
    pub first_contentful_paint_ms: Option<f64>,
    pub largest_contentful_paint_ms: Option<f64>,
    pub cumulative_layout_shift: f64,
    pub interaction_to_next_paint_ms: Option<f64>,
    pub time_to_first_byte_ms: Option<f64>,
    pub dom_content_loaded_ms: Option<f64>,
    pub load_ms: Option<f64>,
    pub long_task_count: usize,
    pub total_blocking_time_ms: f64,
    pub resource_count: usize,
    pub transferred_bytes: u64,
}

/// Typed summary derived from one Chromium performance trace.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserPerformanceTrace {
    pub path: PathBuf,
    pub event_count: usize,
    pub duration_ms: f64,
    pub scripting_ms: f64,
    pub rendering_ms: f64,
    pub painting_ms: f64,
    pub long_task_count: usize,
    pub longest_task_ms: f64,
    pub insights: Vec<BrowserPerformanceInsight>,
}

/// One actionable finding derived from a Chromium performance trace.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserPerformanceInsight {
    LongTask {
        start_ms: f64,
        duration_ms: f64,
        source: Option<BrowserPerformanceSource>,
    },
    ForcedReflow {
        start_ms: f64,
        duration_ms: f64,
        source: Option<BrowserPerformanceSource>,
    },
    SlowCssSelector {
        selector: String,
        duration_ms: f64,
        match_attempts: Option<u64>,
    },
    LayoutShift {
        start_ms: f64,
        score: f64,
        had_recent_input: bool,
    },
    LargestContentfulPaint {
        start_ms: f64,
        size: Option<f64>,
        element: Option<String>,
        url: Option<String>,
    },
    RenderBlockingResource {
        url: String,
        resource_type: String,
        duration_ms: Option<u64>,
        encoded_bytes: Option<u64>,
    },
    NetworkDependencyChain {
        urls: Vec<String>,
        duration_ms: u64,
    },
    DuplicateJavaScript {
        url: String,
        request_count: usize,
        transferred_bytes: u64,
    },
    CacheOpportunity {
        url: String,
        transferred_bytes: u64,
    },
    DomSize {
        nodes: usize,
        maximum_depth: usize,
        maximum_children: usize,
    },
    ThirdParty {
        origin: String,
        request_count: usize,
        transferred_bytes: u64,
    },
}

/// Source location retained by a trace-derived performance finding.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserPerformanceSource {
    /// Chromium script identifier used to resolve the corresponding source map.
    pub script_id: Option<String>,
    pub function_name: Option<String>,
    pub url: String,
    pub line_number: Option<u64>,
    pub column_number: Option<u64>,
    /// Original authored location when the generated script published a source map.
    pub original: Option<crate::BrowserSourceLocation>,
}

/// File-backed V8 CPU profile and its highest-sampled functions.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCpuProfile {
    pub path: PathBuf,
    pub duration_ms: f64,
    pub sample_count: usize,
    pub functions: Vec<BrowserCpuFunction>,
}

/// One function aggregated from a V8 sampling CPU profile.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCpuFunction {
    pub function_name: String,
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
    pub samples: u64,
    pub estimated_self_time_ms: f64,
}

/// File-backed precise JavaScript coverage summary.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCoverage {
    pub path: PathBuf,
    pub scripts: Vec<BrowserScriptCoverage>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub unused_bytes: u64,
}

/// Executed and unexecuted byte ranges for one JavaScript resource.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserScriptCoverage {
    pub url: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub unused_bytes: u64,
    pub used_ratio: f64,
}

/// One constructor/class group from a V8 heap snapshot.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapClass {
    pub name: String,
    pub instance_count: usize,
    pub self_size: u64,
    /// Largest dominator-based retained size among instances of this class.
    pub maximum_retained_size: u64,
    /// V8 heap node identifier for the instance with the largest retained size.
    pub maximum_retained_node_id: u64,
}

/// File-backed V8 heap snapshot with a bounded retained-size summary.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapSnapshot {
    pub artifact_id: String,
    pub path: PathBuf,
    pub node_count: usize,
    pub total_self_size: u64,
    pub classes: Vec<BrowserHeapClass>,
}

/// Change in one heap class between two snapshots.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapClassDelta {
    pub name: String,
    pub instance_count_delta: i64,
    pub self_size_delta: i64,
    pub maximum_retained_size_delta: i64,
}

/// Bounded class-level comparison of two retained heap snapshots.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapComparison {
    pub before_id: String,
    pub after_id: String,
    pub node_count_delta: i64,
    pub self_size_delta: i64,
    pub growing_classes: Vec<BrowserHeapClassDelta>,
}

/// One node in a bounded reverse-reference graph from a heap object toward GC roots.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapRetainerNode {
    pub node_id: u64,
    pub name: String,
    pub class_name: String,
    pub node_type: String,
    pub self_size: u64,
    /// Reverse-reference distance from the queried target, which is zero.
    pub distance: u8,
    /// Node retained by this node. Absent only for the queried target.
    pub retains_node_id: Option<u64>,
    /// V8 edge kind connecting this node to `retains_node_id`.
    pub edge_type: Option<String>,
    /// Property, context, internal, or element name for the retaining edge.
    pub edge_name: Option<String>,
}

/// Bounded retaining graph explaining why one V8 heap object remains reachable.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapRetainers {
    pub artifact_id: String,
    pub target_node_id: u64,
    pub nodes: Vec<BrowserHeapRetainerNode>,
    /// Whether depth or node limits omitted additional retainers.
    pub truncated: bool,
}

/// One detailed V8 heap object sorted by dominator-based retained size.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapNode {
    pub node_id: u64,
    pub name: String,
    pub class_name: String,
    pub node_type: String,
    pub self_size: u64,
    pub retained_size: u64,
    pub detached: bool,
}

/// Duplicate string payload aggregated across V8 heap nodes.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapDuplicateString {
    pub value: String,
    pub instance_count: usize,
    pub self_size: u64,
}

/// Bounded detailed query over one retained V8 heap snapshot.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHeapInspection {
    pub artifact_id: String,
    pub matching_node_count: usize,
    pub nodes: Vec<BrowserHeapNode>,
    pub duplicate_strings: Vec<BrowserHeapDuplicateString>,
    pub truncated: bool,
}

/// Optional `WebM` screencast encoded by a caller-selected `ffmpeg` executable.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserVideoArtifact {
    pub path: PathBuf,
    /// Constant-rate frames written to the encoded video.
    pub frame_count: usize,
    /// Distinct frames received from Chromium before timeline backfill.
    pub captured_frame_count: usize,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub frames_per_second: u8,
}

/// File-backed PDF rendered by Chromium from the active page.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserPdfArtifact {
    pub path: PathBuf,
    pub bytes: u64,
    pub tagged: bool,
    pub document_outline: bool,
}

/// A deterministic response used by a browser network route.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserRouteResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<BrowserRouteHeader>,
    #[serde(default)]
    pub body: String,
}

/// One response header supplied by a browser route.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserRouteHeader {
    pub name: String,
    pub value: String,
}

/// A HAR artifact exported from retained browser network diagnostics.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserHarArtifact {
    pub path: PathBuf,
    pub entry_count: usize,
    pub body_count: usize,
}

/// Severity of one browser accessibility finding.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAccessibilityImpact {
    Critical,
    Serious,
    Moderate,
    Minor,
}

/// One concrete accessibility finding and the element that caused it.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAccessibilityViolation {
    pub rule: String,
    pub impact: BrowserAccessibilityImpact,
    pub message: String,
    pub selector: String,
    pub frame_id: Option<String>,
    pub frame_url: Option<String>,
}

/// Results of the library's embedded accessibility audit.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAccessibilityAudit {
    pub url: String,
    pub checked_elements: usize,
    pub violations: Vec<BrowserAccessibilityViolation>,
}

/// One DOM node reported by axe-core.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAxeNode {
    pub selector: String,
    pub html: String,
    pub failure_summary: Option<String>,
    pub frame_id: Option<String>,
    pub frame_url: Option<String>,
}

/// One axe-core rule result and its bounded set of affected nodes.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAxeFinding {
    pub id: String,
    pub impact: Option<BrowserAccessibilityImpact>,
    pub description: String,
    pub help: String,
    pub help_url: String,
    pub tags: Vec<String>,
    pub node_count: usize,
    pub nodes: Vec<BrowserAxeNode>,
}

/// Full axe-core violation and incomplete-review report.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAxeAudit {
    pub url: String,
    pub engine_version: String,
    pub violations: Vec<BrowserAxeFinding>,
    pub incomplete: Vec<BrowserAxeFinding>,
    pub pass_count: usize,
    pub inapplicable_count: usize,
    pub truncated: bool,
}

/// Lighthouse category selected for an explicit audit.
#[derive(
    Clone, Copy, Debug, Deserialize, JsonSchema, Ord, PartialOrd, PartialEq, Eq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLighthouseCategory {
    Performance,
    Accessibility,
    BestPractices,
    Seo,
}

/// Device/scoring preset used by Lighthouse.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLighthouseFormFactor {
    Mobile,
    Desktop,
}

/// One Lighthouse category score, from zero through one when scored.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLighthouseCategoryScore {
    pub category: BrowserLighthouseCategory,
    pub score: Option<f64>,
}

/// One failing or informative Lighthouse audit retained in the typed summary.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLighthouseFinding {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub score: Option<f64>,
    pub score_display_mode: String,
    pub display_value: Option<String>,
    pub numeric_value: Option<f64>,
    pub numeric_unit: Option<String>,
}

/// Exact Lighthouse report plus a bounded typed summary.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLighthouseReport {
    pub path: PathBuf,
    pub lighthouse_version: String,
    pub requested_url: String,
    pub final_url: String,
    pub fetch_time: String,
    pub categories: Vec<BrowserLighthouseCategoryScore>,
    pub findings: Vec<BrowserLighthouseFinding>,
    pub omitted_finding_count: usize,
}

/// `CrUX` aggregation level for the active page.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCruxScope {
    #[default]
    Url,
    Origin,
}

/// Optional device filter for `CrUX` field data.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCruxFormFactor {
    Desktop,
    Phone,
    Tablet,
}

/// One bucket in a `CrUX` metric histogram.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCruxHistogramBin {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub density: f64,
}

/// One named fraction for a categorical `CrUX` metric.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCruxFraction {
    pub name: String,
    pub density: f64,
}

/// One typed metric in a `CrUX` field-data record.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCruxMetric {
    pub name: String,
    pub p75: Option<f64>,
    pub histogram: Vec<BrowserCruxHistogramBin>,
    pub fractions: Vec<BrowserCruxFraction>,
}

/// Date range covered by a `CrUX` record.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCruxCollectionPeriod {
    pub first_date: String,
    pub last_date: String,
}

/// Chrome UX Report field data for the active URL or origin.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCruxReport {
    pub requested_url: String,
    pub record_url: Option<String>,
    pub record_origin: Option<String>,
    pub form_factor: Option<BrowserCruxFormFactor>,
    pub collection_period: Option<BrowserCruxCollectionPeriod>,
    pub metrics: Vec<BrowserCruxMetric>,
    pub normalized_url: Option<String>,
}

/// One file download observed by the owned browser session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDownload {
    pub id: String,
    pub url: String,
    pub suggested_filename: String,
    pub path: Option<PathBuf>,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub completed: bool,
    pub failure: Option<String>,
}
