use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

mod artifacts;
mod audits;
mod credential_store;
mod devtools;
mod har;
mod interaction;
mod network_control;
mod network_observer;
mod performance_trace;
mod profiling;
mod session_trace;
mod source_maps;
mod video;
mod web_diagnostics;

pub(crate) use artifacts::MAX_IMAGE_ARTIFACT_BYTES;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Browser as Chromium, Page,
    browser::{BrowserConfig, BrowserConfigBuilder},
    cdp::{
        browser_protocol::{
            browser::{
                CancelDownloadParams, DownloadProgressState, EventDownloadProgress,
                EventDownloadWillBegin, PermissionDescriptor, PermissionSetting,
                SetDownloadBehaviorBehavior, SetDownloadBehaviorParams, SetPermissionParams,
            },
            dom::{GetBoxModelParams, SetFileInputFilesParams},
            dom_snapshot::{
                ArrayOfStrings, CaptureSnapshotParams, CaptureSnapshotReturns, DocumentSnapshot,
                LayoutTreeSnapshot, RareBooleanData, RareIntegerData, RareStringData, Rectangle,
                StringIndex,
            },
            emulation::{
                MediaFeature, SetCpuThrottlingRateParams, SetDeviceMetricsOverrideParams,
                SetEmulatedMediaParams, SetGeolocationOverrideParams, SetLocaleOverrideParams,
                SetTimezoneOverrideParams, SetTouchEmulationEnabledParams,
                SetUserAgentOverrideParams,
            },
            network::{
                Cookie, CookieParam, CookieSameSite, EmulateNetworkConditionsByRuleParams,
                EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
                EventResponseReceived, EventWebSocketClosed, EventWebSocketCreated,
                EventWebSocketFrameError, EventWebSocketFrameReceived, EventWebSocketFrameSent,
                EventWebSocketHandshakeResponseReceived, EventWebSocketWillSendHandshakeRequest,
                GetRequestPostDataParams, GetResponseBodyParams, Headers, Initiator,
                NetworkConditions, OverrideNetworkStateParams, ResourceTiming, Response,
                SetBypassServiceWorkerParams, SetExtraHttpHeadersParams, TimeSinceEpoch,
                WebSocketFrame,
            },
            page::{
                AddScriptToEvaluateOnNewDocumentParams, EventJavascriptDialogOpening,
                GetNavigationHistoryParams, HandleJavaScriptDialogParams,
                NavigateToHistoryEntryParams, Viewport,
            },
            target::{GetTargetsParams, SetAutoAttachParams, TargetId},
            web_authn::{
                AddCredentialParams, AddVirtualAuthenticatorParams, AuthenticatorId,
                AuthenticatorProtocol, AuthenticatorTransport, Credential, EnableParams,
                GetCredentialsParams, RemoveCredentialParams, VirtualAuthenticatorOptions,
            },
        },
        js_protocol::runtime::{
            EvaluateParams, EventConsoleApiCalled, EventExceptionThrown, ExecutionContextId,
            ReleaseObjectParams, RemoteObject, StackTrace,
        },
    },
    error::CdpError,
    layout::Point,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::{sync::Mutex, task::JoinHandle, time::timeout};
use tracing::{Instrument, info, info_span, warn};
use url::Url;

use crate::{
    BraveSession, BraveSessionError, BrowserAction, BrowserActionName, BrowserActionOutcome,
    BrowserActionResult, BrowserAfterAction, BrowserClickOptions, BrowserConsoleEntry,
    BrowserCookie, BrowserCookieSameSite, BrowserCruxClient, BrowserDialog, BrowserDialogKind,
    BrowserDocumentReadyState, BrowserDomDocument, BrowserDomLayout, BrowserDomNode,
    BrowserDomRect, BrowserDomSnapshot, BrowserDownload, BrowserEgressPolicy,
    BrowserElementContext, BrowserElementReference, BrowserFrame, BrowserGate, BrowserHttpHeader,
    BrowserImageArtifact, BrowserNetworkBodyKind, BrowserNetworkCallFrame, BrowserNetworkContext,
    BrowserNetworkInitiator, BrowserNetworkRequest, BrowserNetworkTiming, BrowserOriginStorage,
    BrowserPageError, BrowserPageState, BrowserPasskeyMode, BrowserPostActionSnapshot,
    BrowserReactEvent, BrowserReactStatus, BrowserStorageState, BrowserTab, BrowserTarget,
    BrowserTargetIndex, BrowserWebSocketDirection, BrowserWebSocketMessage, MAX_VIEWPORT_DIMENSION,
    ReactDiagnostics, VirtualAuthenticator, VirtualCredential,
    features::{BrowserColorScheme, BrowserContext, BrowserPermission, BrowserReducedMotion},
    session::cookie_applies_to,
    trace_serialized,
};
use credential_store::VirtualCredentialStore;

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(25);
const DEFAULT_NAVIGATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAIN_CONTEXT_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_EXPLICIT_WAIT: Duration = Duration::from_secs(30);
const MAX_SCRIPT_EVALUATION: Duration = Duration::from_secs(30);
const MAX_NETWORK_BODY_WAIT: Duration = Duration::from_secs(30);
const MAX_NETWORK_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_ACTION_INPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ACTION_VALUE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SNAPSHOT_REFS: usize = 20_000;
const MAX_DOM_SNAPSHOT_NODES: usize = 250_000;
const MAX_DOM_SNAPSHOT_STRING_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOM_COMPUTED_STYLES: usize = 128;
const MAX_CONSOLE_ENTRIES: usize = 1_000;
const MAX_PAGE_ERRORS: usize = 1_000;
const MAX_NETWORK_REQUESTS: usize = 4_000;
const MAX_WEB_SOCKET_MESSAGES: usize = 4_000;
const MAX_DIAGNOSTIC_TEXT_BYTES: usize = 16 * 1024;
const MAX_WEB_SOCKET_PAYLOAD_BYTES: usize = 32 * 1024;
const MAX_NETWORK_HEADERS: usize = 128;
const MAX_NETWORK_HEADER_BYTES: usize = 32 * 1024;
const MAX_NETWORK_STACK_FRAMES: usize = 32;
const MAX_NETWORK_FIELD_BYTES: usize = 4 * 1024;
const MAX_VISUAL_ARTIFACTS: usize = 128;
const MAX_HEAP_SNAPSHOTS: usize = 4;
const MAX_DOWNLOADS: usize = 128;
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UPLOAD_FILES: usize = 64;
const MAX_UPLOAD_FILE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_DIAGNOSTIC_RESULTS: usize = 200;
const MAX_DIAGNOSTIC_RESULTS: usize = 1_000;
const REACT_DIAGNOSTICS_PROTOCOL_VERSION: u32 = 1;
const REACT_DIAGNOSTICS_MAX_EVENTS: usize = 512;
const REACT_DIAGNOSTICS_BUNDLE: &str = include_str!("../assets/react_diagnostics.js");

pub(crate) struct NativeBrowser {
    runtime_dir: TempDir,
    executable: Option<PathBuf>,
    cdp_endpoint: Option<Url>,
    brave_session: Option<BraveSession>,
    launch_brave_executable: bool,
    virtual_authenticator: Option<VirtualAuthenticator>,
    react_diagnostics: Option<ReactDiagnostics>,
    egress_policy: Option<BrowserEgressPolicy>,
    file_root: Option<PathBuf>,
    context: BrowserContext,
    storage_state: Option<BrowserStorageState>,
    after_action: BrowserAfterAction,
    ffmpeg_executable: Option<PathBuf>,
    lighthouse_executable: Option<PathBuf>,
    crux_client: Option<BrowserCruxClient>,
    state: Mutex<BrowserState>,
}

#[derive(Default)]
struct BrowserState {
    next_sequence: u64,
    session: Option<Session>,
    closed: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the booleans are independent lifecycle flags for optional browser subsystems"
)]
struct Session {
    browser: Chromium,
    page: Page,
    handler: JoinHandle<()>,
    browser_tasks: Vec<JoinHandle<()>>,
    page_tasks: Vec<JoinHandle<()>>,
    network_observer: network_observer::NetworkObserver,
    diagnostics: Arc<StdMutex<Diagnostics>>,
    source_maps: source_maps::SourceMaps,
    devtools: devtools::DevtoolsDiagnostics,
    refs: HashMap<String, ElementTarget>,
    output_dir: PathBuf,
    virtual_authenticator: Option<VirtualAuthenticator>,
    virtual_credential_store: Option<VirtualCredentialStore>,
    passkey_mode: BrowserPasskeyMode,
    react_diagnostics_enabled: bool,
    authenticators: HashMap<String, InstalledAuthenticator>,
    closed_targets: HashSet<String>,
    allowed_origins: Vec<Url>,
    network_controls: network_control::NetworkControls,
    egress_targets: HashSet<String>,
    file_root: Option<PathBuf>,
    visual_artifacts: HashMap<String, BrowserImageArtifact>,
    visual_trace: Option<artifacts::VisualTraceState>,
    action_trace: Option<session_trace::SessionTraceState>,
    performance_trace: Option<performance_trace::PerformanceTraceState>,
    cpu_profile_active: bool,
    coverage_active: bool,
    heap_snapshots: HashMap<String, profiling::HeapAnalysis>,
    video: Option<video::VideoState>,
    react_diagnostics: Option<ReactDiagnostics>,
    axe_enabled: bool,
    context: BrowserContext,
    storage_state: Option<BrowserStorageState>,
    pointer_x: f64,
    pointer_y: f64,
    pointer_buttons: i64,
    ffmpeg_executable: Option<PathBuf>,
    lighthouse_executable: Option<PathBuf>,
    crux_client: Option<BrowserCruxClient>,
}

#[derive(Default)]
struct Diagnostics {
    console: VecDeque<BrowserConsoleEntry>,
    errors: VecDeque<BrowserPageError>,
    requests: VecDeque<NetworkEntry>,
    request_sequences: HashMap<String, u64>,
    pending_page_lifecycle: HashMap<String, PendingNetworkLifecycle>,
    next_request_sequence: u64,
    web_socket_messages: VecDeque<BrowserWebSocketMessage>,
    next_web_socket_message_sequence: u64,
    dropped_console: u64,
    dropped_errors: u64,
    dropped_requests: u64,
    dropped_web_socket_messages: u64,
    next_console_sequence: u64,
    next_error_sequence: u64,
    dialog: Option<BrowserDialog>,
    downloads: BTreeMap<String, BrowserDownload>,
}

#[derive(Clone)]
struct NetworkEntry {
    sequence: u64,
    request_id: String,
    source: NetworkSource,
    started_at_monotonic_seconds: f64,
    request: BrowserNetworkRequest,
}

#[derive(Default)]
struct PendingNetworkLifecycle {
    response: Option<PendingNetworkResponse>,
    completion: Option<PendingNetworkCompletion>,
}

struct PendingNetworkResponse {
    response: Response,
    resource_type: String,
}

enum PendingNetworkCompletion {
    Finished {
        timestamp_seconds: f64,
        encoded_data_length: f64,
    },
    Failed {
        timestamp_seconds: f64,
        error: String,
        resource_type: String,
    },
}

#[derive(Clone)]
enum NetworkSource {
    Page,
    ChildTarget {
        session_id: String,
        request_id: String,
    },
}

impl Diagnostics {
    fn reset_page(&mut self) {
        self.console.clear();
        self.errors.clear();
        self.requests.clear();
        self.request_sequences.clear();
        self.pending_page_lifecycle.clear();
        self.web_socket_messages.clear();
        self.dialog = None;
    }

    fn push_console(&mut self, mut entry: BrowserConsoleEntry) {
        truncate_utf8(&mut entry.level, MAX_NETWORK_FIELD_BYTES);
        self.next_console_sequence = self.next_console_sequence.saturating_add(1);
        entry.sequence = self.next_console_sequence;
        if entry.text.len() > MAX_DIAGNOSTIC_TEXT_BYTES {
            self.dropped_console = self.dropped_console.saturating_add(1);
            return;
        }
        if self.console.len() == MAX_CONSOLE_ENTRIES {
            self.console.pop_front();
            self.dropped_console = self.dropped_console.saturating_add(1);
        }
        self.console.push_back(entry);
    }

    fn push_error(&mut self, mut error: BrowserPageError) {
        if let Some(url) = &mut error.url {
            truncate_utf8(url, MAX_NETWORK_FIELD_BYTES);
        }
        self.next_error_sequence = self.next_error_sequence.saturating_add(1);
        error.sequence = self.next_error_sequence;
        if error.text.len() > MAX_DIAGNOSTIC_TEXT_BYTES {
            self.dropped_errors = self.dropped_errors.saturating_add(1);
            return;
        }
        if self.errors.len() == MAX_PAGE_ERRORS {
            self.errors.pop_front();
            self.dropped_errors = self.dropped_errors.saturating_add(1);
        }
        self.errors.push_back(error);
    }

    fn push_request(
        &mut self,
        request_id: &str,
        source: NetworkSource,
        started_at_monotonic_seconds: f64,
        mut request: BrowserNetworkRequest,
    ) {
        truncate_utf8(&mut request.url, MAX_DIAGNOSTIC_TEXT_BYTES);
        truncate_utf8(&mut request.method, MAX_NETWORK_FIELD_BYTES);
        truncate_utf8(&mut request.document_url, MAX_DIAGNOSTIC_TEXT_BYTES);
        truncate_utf8(&mut request.resource_type, MAX_NETWORK_FIELD_BYTES);
        self.next_request_sequence = self.next_request_sequence.saturating_add(1);
        let sequence = self.next_request_sequence;
        request.sequence = sequence;
        request_id.clone_into(&mut request.request_id);
        if self.requests.len() == MAX_NETWORK_REQUESTS
            && let Some(evicted) = self.requests.pop_front()
        {
            if self.request_sequences.get(&evicted.request_id) == Some(&evicted.sequence) {
                self.request_sequences.remove(&evicted.request_id);
            }
            self.dropped_requests = self.dropped_requests.saturating_add(1);
        }
        self.request_sequences
            .insert(request_id.to_owned(), sequence);
        self.requests.push_back(NetworkEntry {
            sequence,
            request_id: request_id.to_owned(),
            source,
            started_at_monotonic_seconds,
            request,
        });
        self.apply_pending_page_lifecycle(request_id);
    }

    fn request_entry_mut(&mut self, request_id: &str) -> Option<&mut NetworkEntry> {
        let sequence = self.request_sequences.get(request_id).copied()?;
        let first = self.requests.front()?.sequence;
        let offset = usize::try_from(sequence.checked_sub(first)?).ok()?;
        let entry = self.requests.get_mut(offset)?;
        (entry.sequence == sequence).then_some(entry)
    }

    fn record_page_response(
        &mut self,
        request_id: &str,
        response: &Response,
        resource_type: String,
    ) {
        if let Some(entry) = self.request_entry_mut(request_id) {
            apply_response(&mut entry.request, response);
            entry.request.resource_type = resource_type;
            return;
        }
        self.pending_page_lifecycle(request_id).response = Some(PendingNetworkResponse {
            response: response.clone(),
            resource_type,
        });
    }

    fn finish_page_request(
        &mut self,
        request_id: &str,
        timestamp_seconds: f64,
        encoded_data_length: f64,
    ) {
        if let Some(entry) = self.request_entry_mut(request_id) {
            finish_request(entry, timestamp_seconds, encoded_data_length);
            return;
        }
        self.pending_page_lifecycle(request_id).completion =
            Some(PendingNetworkCompletion::Finished {
                timestamp_seconds,
                encoded_data_length,
            });
    }

    fn fail_page_request(
        &mut self,
        request_id: &str,
        timestamp_seconds: f64,
        error: String,
        resource_type: String,
    ) {
        if let Some(entry) = self.request_entry_mut(request_id) {
            entry.request.failure = Some(bounded_string(&error, MAX_DIAGNOSTIC_TEXT_BYTES));
            entry.request.resource_type = bounded_string(&resource_type, MAX_NETWORK_FIELD_BYTES);
            finish_request(entry, timestamp_seconds, 0.0);
            return;
        }
        self.pending_page_lifecycle(request_id).completion =
            Some(PendingNetworkCompletion::Failed {
                timestamp_seconds,
                error,
                resource_type,
            });
    }

    fn pending_page_lifecycle(&mut self, request_id: &str) -> &mut PendingNetworkLifecycle {
        if !self.pending_page_lifecycle.contains_key(request_id)
            && self.pending_page_lifecycle.len() >= MAX_NETWORK_REQUESTS
            && let Some(evicted) = self.pending_page_lifecycle.keys().next().cloned()
        {
            self.pending_page_lifecycle.remove(&evicted);
        }
        self.pending_page_lifecycle
            .entry(request_id.to_owned())
            .or_default()
    }

    fn apply_pending_page_lifecycle(&mut self, request_id: &str) {
        let Some(pending) = self.pending_page_lifecycle.remove(request_id) else {
            return;
        };
        let Some(entry) = self.request_entry_mut(request_id) else {
            return;
        };
        if let Some(response) = pending.response {
            apply_response(&mut entry.request, &response.response);
            entry.request.resource_type = response.resource_type;
        }
        if let Some(completion) = pending.completion {
            match completion {
                PendingNetworkCompletion::Finished {
                    timestamp_seconds,
                    encoded_data_length,
                } => finish_request(entry, timestamp_seconds, encoded_data_length),
                PendingNetworkCompletion::Failed {
                    timestamp_seconds,
                    error,
                    resource_type,
                } => {
                    entry.request.failure = Some(bounded_string(&error, MAX_DIAGNOSTIC_TEXT_BYTES));
                    entry.request.resource_type =
                        bounded_string(&resource_type, MAX_NETWORK_FIELD_BYTES);
                    finish_request(entry, timestamp_seconds, 0.0);
                }
            }
        }
    }

    fn push_web_socket_message(
        &mut self,
        request_id: String,
        direction: BrowserWebSocketDirection,
        timestamp_seconds: f64,
        frame: &WebSocketFrame,
    ) {
        self.next_web_socket_message_sequence =
            self.next_web_socket_message_sequence.saturating_add(1);
        let sequence = self.next_web_socket_message_sequence;
        if frame.payload_data.len() > MAX_WEB_SOCKET_PAYLOAD_BYTES {
            self.dropped_web_socket_messages = self.dropped_web_socket_messages.saturating_add(1);
            return;
        }
        if self.web_socket_messages.len() == MAX_WEB_SOCKET_MESSAGES {
            self.web_socket_messages.pop_front();
            self.dropped_web_socket_messages = self.dropped_web_socket_messages.saturating_add(1);
        }
        let opcode = web_socket_opcode(frame.opcode);
        self.web_socket_messages.push_back(BrowserWebSocketMessage {
            sequence,
            request_id,
            direction,
            timestamp_ms: seconds_to_milliseconds(timestamp_seconds),
            opcode,
            payload: frame.payload_data.clone(),
            base64_encoded: opcode != 1,
        });
    }
}

fn truncate_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn bounded_string(value: &str, maximum: usize) -> String {
    let mut value = value.to_owned();
    truncate_utf8(&mut value, maximum);
    value
}

fn serialized_size(value: &impl Serialize) -> Result<u64, BrowserError> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

fn ensure_action_value_size(bytes: usize) -> Result<(), BrowserError> {
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    if bytes > MAX_ACTION_VALUE_BYTES {
        return Err(BrowserError::ActionValueTooLarge {
            bytes,
            maximum: MAX_ACTION_VALUE_BYTES,
        });
    }
    Ok(())
}

#[derive(Default)]
struct ByteCounter {
    bytes: u64,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn web_socket_opcode(opcode: f64) -> u8 {
    (0_u8..=15)
        .find(|candidate| opcode.total_cmp(&f64::from(*candidate)).is_eq())
        .unwrap_or(u8::MAX)
}

#[derive(Clone)]
pub(super) struct ElementTarget {
    pub(super) query: ElementQuery,
    pub(super) context_id: Option<ExecutionContextId>,
}

#[derive(Clone)]
pub(super) enum ElementQuery {
    SnapshotPath(Vec<String>),
    Locator(BrowserTarget),
}

struct InstalledAuthenticator {
    page: Page,
    id: AuthenticatorId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageStateWire {
    title: String,
    ready_state: String,
}

fn browser_context_trace_value(context: &BrowserContext) -> serde_json::Value {
    let viewport = context.viewport.map(|viewport| {
        serde_json::json!({
            "width": viewport.width,
            "height": viewport.height,
            "deviceScaleFactor": viewport.device_scale_factor,
            "mobile": viewport.mobile,
            "touch": viewport.touch,
            "maxTouchPoints": viewport.max_touch_points,
        })
    });
    let geolocation = context.geolocation.map(|geolocation| {
        serde_json::json!({
            "latitude": geolocation.latitude,
            "longitude": geolocation.longitude,
            "accuracyMeters": geolocation.accuracy_meters,
        })
    });
    let permissions = context
        .permissions
        .iter()
        .map(|grant| {
            serde_json::json!({
                "permission": format!("{:?}", grant.permission),
                "origin": grant.origin,
            })
        })
        .collect::<Vec<_>>();
    let extra_headers = context
        .extra_headers
        .iter()
        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
        .collect::<Vec<_>>();
    let http_credentials = context.http_credentials.as_ref().map(
        |(username, password)| serde_json::json!({ "username": username, "password": password }),
    );
    let network = context.network.map(|network| {
        serde_json::json!({
            "offline": network.offline,
            "latencyMs": network.latency_ms,
            "downloadBytesPerSecond": network.download_bytes_per_second,
            "uploadBytesPerSecond": network.upload_bytes_per_second,
        })
    });
    serde_json::json!({
        "viewport": viewport,
        "locale": context.locale,
        "timezone": context.timezone,
        "userAgent": context.user_agent,
        "platform": context.platform,
        "acceptLanguage": context.accept_language,
        "colorScheme": context.color_scheme.map(|value| format!("{value:?}")),
        "reducedMotion": context.reduced_motion.map(|value| format!("{value:?}")),
        "geolocation": geolocation,
        "permissions": permissions,
        "extraHeaders": extra_headers,
        "httpCredentials": http_credentials,
        "initScripts": context.init_scripts,
        "cpuThrottleRate": context.cpu_throttle_rate,
        "network": network,
    })
}

fn trace_browser_configuration(owner: &NativeBrowser) {
    let egress_policy = owner.egress_policy.as_ref().map(|policy| {
        serde_json::json!({
            "allowedOrigins": policy.allowed_origins,
            "allowedDomainSuffixes": policy.allowed_domain_suffixes,
            "allowLoopback": policy.allow_loopback,
        })
    });
    let crux_client = owner.crux_client.as_ref().map(|client| {
        serde_json::json!({
            "apiKey": client.api_key,
            "endpoint": client.endpoint,
        })
    });
    let value = serde_json::json!({
        "executable": owner.executable,
        "cdpEndpoint": owner.cdp_endpoint,
        "braveSession": owner.brave_session.as_ref().map(BraveSession::trace_value),
        "launchBraveExecutable": owner.launch_brave_executable,
        "virtualAuthenticator": owner.virtual_authenticator.is_some(),
        "reactDiagnostics": owner.react_diagnostics.map(|diagnostics| {
            serde_json::json!({
                "includeProfilingHooks": diagnostics.include_profiling_hooks(),
            })
        }),
        "egressPolicy": egress_policy,
        "fileRoot": owner.file_root,
        "context": browser_context_trace_value(&owner.context),
        "storageState": owner.storage_state,
        "afterAction": format!("{:?}", owner.after_action),
        "ffmpegExecutable": owner.ffmpeg_executable,
        "lighthouseExecutable": owner.lighthouse_executable,
        "cruxClient": crux_client,
    });
    trace_serialized("session.configuration", &value);
}

impl Session {
    #[allow(
        clippy::too_many_lines,
        reason = "launch ordering is security-sensitive: guards and observers precede any real navigation"
    )]
    async fn launch(owner: &NativeBrowser) -> Result<Self, BrowserError> {
        trace_browser_configuration(owner);
        let runtime_dir = owner.runtime_dir.path();
        let executable = owner.executable.as_deref();
        let cdp_endpoint = owner.cdp_endpoint.as_ref();
        let brave_session = owner.brave_session.as_ref();
        let launch_brave_executable = owner.launch_brave_executable;
        let virtual_authenticator = owner.virtual_authenticator.clone();
        let virtual_credential_store = virtual_authenticator
            .as_ref()
            .and_then(VirtualAuthenticator::credential_store_path)
            .map(|path| VirtualCredentialStore::load(path.to_path_buf()))
            .transpose()?;
        let react_diagnostics = owner.react_diagnostics;
        let egress_policy = owner.egress_policy.clone();
        let file_root = owner.file_root.clone();
        let profile = runtime_dir.join("profile");
        let output_dir = runtime_dir.join("screenshots");
        let download_dir = runtime_dir.join("downloads");
        tokio::fs::create_dir_all(&profile).await?;
        tokio::fs::create_dir_all(&output_dir).await?;
        tokio::fs::create_dir_all(&download_dir).await?;

        let opens_brave_profile = cdp_endpoint.is_none()
            && executable.is_none()
            && launch_brave_executable
            && brave_session.is_some();
        let imported_cookies = if !opens_brave_profile && let Some(brave_session) = brave_session {
            Some(export_profile_cookies(runtime_dir, brave_session).await?)
        } else {
            None
        };
        if let Some(cookies) = &imported_cookies {
            trace_serialized("session.imported_brave_cookies", cookies);
        }
        if opens_brave_profile && let Some(brave_session) = brave_session {
            brave_session.prepare(&profile).await?;
        }

        let (mut browser, mut handler) = if let Some(endpoint) = cdp_endpoint {
            Chromium::connect(endpoint.as_str()).await?
        } else {
            let mut config = BrowserConfig::builder()
                .user_data_dir(&profile)
                .window_size(1280, 720)
                .new_headless_mode();
            if opens_brave_profile {
                config = profile_launch_config(config).arg("restore-last-session");
            }
            if let Some(executable) = executable {
                config = config.chrome_executable(executable);
            } else if launch_brave_executable && let Some(brave_session) = brave_session {
                config = config.chrome_executable(brave_session.executable());
            }
            if egress_policy.is_some() {
                config = config
                    .arg("force-webrtc-ip-handling-policy=disable_non_proxied_udp")
                    .arg("webrtc-ip-handling-policy=disable_non_proxied_udp");
            }
            let config = build_config(config)?;
            Chromium::launch(config).await?
        };
        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(error) = event {
                    warn!(target: "nanocodex_browser", %error, "browser handler stopped");
                    break;
                }
            }
        });
        if let Some(cookies) = imported_cookies
            && let Err(error) = replace_browser_cookies(&browser, cookies).await
        {
            let _ = close_chromium(&mut browser, &handler).await;
            return Err(error);
        }
        if let Some(storage_state) = &owner.storage_state
            && let Err(error) =
                replace_browser_cookies(&browser, storage_cookie_params(storage_state)).await
        {
            let _ = close_chromium(&mut browser, &handler).await;
            return Err(error);
        }
        let page = match browser.new_page("about:blank").await {
            Ok(page) => page,
            Err(error) => {
                let _ = close_chromium(&mut browser, &handler).await;
                return Err(error.into());
            }
        };
        apply_browser_context(&browser, &page, &owner.context).await?;
        if let Some(storage_state) = &owner.storage_state {
            install_storage_state(&page, storage_state).await?;
        }
        if let Some(react_diagnostics) = react_diagnostics {
            install_react_diagnostics(&page, react_diagnostics).await?;
        }
        web_diagnostics::install(&page).await?;
        if egress_policy.is_some() {
            install_restricted_network_guards(&page).await?;
        }
        page.execute(SetAutoAttachParams::new(false, false)).await?;
        let diagnostics = Arc::new(StdMutex::new(Diagnostics::default()));
        let mut browser_tasks =
            start_download_diagnostics(&browser, &page, &download_dir, Arc::clone(&diagnostics))
                .await?;
        let (source_maps, source_maps_task) = source_maps::start(&page).await?;
        let mut page_tasks = vec![source_maps_task];
        let (devtools, devtools_tasks) = devtools::start(&page, source_maps.clone()).await?;
        page_tasks.extend(devtools_tasks);
        page_tasks.extend(start_diagnostics(&page, Arc::clone(&diagnostics)).await?);
        let network_controls = network_control::NetworkControls::new(egress_policy);
        browser_tasks.push(
            network_control::start(&page, network_controls.clone(), Arc::clone(&diagnostics))
                .await?,
        );
        let (network_observer, network_task) = network_observer::start(
            browser.websocket_address(),
            page.target_id().clone(),
            Arc::clone(&diagnostics),
        )
        .await?;
        browser_tasks.push(network_task);
        let egress_targets = HashSet::from([page.target_id().as_ref().to_owned()]);

        Ok(Self {
            browser,
            page,
            handler,
            browser_tasks,
            page_tasks,
            network_observer,
            diagnostics,
            source_maps,
            devtools,
            refs: HashMap::new(),
            output_dir,
            virtual_authenticator,
            virtual_credential_store,
            passkey_mode: BrowserPasskeyMode::Auto,
            react_diagnostics_enabled: react_diagnostics.is_some(),
            authenticators: HashMap::new(),
            closed_targets: HashSet::new(),
            allowed_origins: brave_session
                .map(|session| session.allowed_origins().to_vec())
                .unwrap_or_default(),
            network_controls,
            egress_targets,
            file_root,
            visual_artifacts: HashMap::new(),
            visual_trace: None,
            action_trace: None,
            performance_trace: None,
            cpu_profile_active: false,
            coverage_active: false,
            heap_snapshots: HashMap::new(),
            video: None,
            react_diagnostics,
            axe_enabled: false,
            context: owner.context.clone(),
            storage_state: owner.storage_state.clone(),
            pointer_x: 0.0,
            pointer_y: 0.0,
            pointer_buttons: 0,
            ffmpeg_executable: owner.ffmpeg_executable.clone(),
            lighthouse_executable: owner.lighthouse_executable.clone(),
            crux_client: owner.crux_client.clone(),
        })
    }

    async fn close(mut self) -> Result<(), BrowserError> {
        let credential_sync = self.synchronize_virtual_credentials().await;
        for task in &self.browser_tasks {
            task.abort();
        }
        for task in &self.page_tasks {
            task.abort();
        }
        let close = close_chromium(&mut self.browser, &self.handler).await;
        credential_sync?;
        close
    }

    async fn refresh_remote_cookies(
        &mut self,
        runtime_dir: &Path,
        brave_session: &BraveSession,
    ) -> Result<(), BrowserError> {
        let cookies = export_profile_cookies(runtime_dir, brave_session).await?;
        trace_serialized("session.refreshed_brave_cookies", &cookies);
        replace_browser_cookies(&self.browser, cookies).await?;
        Ok(())
    }

    async fn storage_state(&self) -> Result<BrowserStorageState, BrowserError> {
        let mut cookies = self
            .browser
            .get_cookies()
            .await?
            .into_iter()
            .map(browser_cookie)
            .collect::<Vec<_>>();
        cookies.sort_unstable_by(|left, right| {
            (&left.domain, &left.path, &left.name).cmp(&(&right.domain, &right.path, &right.name))
        });
        let mut origins = BTreeMap::new();
        for page in self.browser.pages().await? {
            let Some(page_url) = page.url().await? else {
                continue;
            };
            let Ok(page_url) = Url::parse(&page_url) else {
                continue;
            };
            if !matches!(page_url.scheme(), "http" | "https") {
                continue;
            }
            let state: Option<BrowserOriginStorage> =
                evaluate_typed(&page, CAPTURE_ORIGIN_STORAGE_SCRIPT).await?;
            if let Some(state) = state {
                origins.insert(state.origin.clone(), state);
            }
        }
        Ok(BrowserStorageState {
            cookies,
            origins: origins.into_values().collect(),
        })
    }

    async fn restore_storage_state(
        &mut self,
        state: BrowserStorageState,
    ) -> Result<(), BrowserError> {
        replace_browser_cookies(&self.browser, storage_cookie_params(&state)).await?;
        for page in self.browser.pages().await? {
            install_storage_state(&page, &state).await?;
        }
        self.storage_state = Some(state);
        Ok(())
    }

    async fn activate_page(&mut self, page: Page) -> Result<(), BrowserError> {
        for task in self.page_tasks.drain(..) {
            task.abort();
        }
        apply_browser_context(&self.browser, &page, &self.context).await?;
        if let Some(storage_state) = &self.storage_state {
            install_storage_state(&page, storage_state).await?;
        }
        if let Some(react_diagnostics) = self.react_diagnostics {
            install_react_diagnostics(&page, react_diagnostics).await?;
        }
        if self.axe_enabled {
            audits::install_axe(&page).await?;
        }
        web_diagnostics::install(&page).await?;
        if self.network_controls.restricted()? {
            install_restricted_network_guards(&page).await?;
        }
        page.execute(SetAutoAttachParams::new(false, false)).await?;
        let target_id = page.target_id().as_ref().to_owned();
        if !self.egress_targets.contains(&target_id) {
            let task = network_control::start(
                &page,
                self.network_controls.clone(),
                Arc::clone(&self.diagnostics),
            )
            .await?;
            self.egress_targets.insert(target_id.clone());
            self.browser_tasks.push(task);
        }
        self.network_observer.activate(target_id).await?;
        if let Ok(mut diagnostics) = self.diagnostics.lock() {
            diagnostics.reset_page();
        }
        let (source_maps, source_maps_task) = source_maps::start(&page).await?;
        let mut page_tasks = vec![source_maps_task];
        let (devtools, devtools_tasks) = devtools::start(&page, source_maps.clone()).await?;
        page_tasks.extend(devtools_tasks);
        page_tasks.extend(start_diagnostics(&page, Arc::clone(&self.diagnostics)).await?);
        self.page = page;
        self.page_tasks = page_tasks;
        self.source_maps = source_maps;
        self.devtools = devtools;
        self.refs.clear();
        Ok(())
    }

    const fn ensure_capture_inactive(&self) -> Result<(), BrowserError> {
        if self.visual_trace.is_some() {
            return Err(BrowserError::VisualTraceActive);
        }
        if self.performance_trace.is_some() {
            return Err(BrowserError::PerformanceTraceActive);
        }
        if self.cpu_profile_active {
            return Err(BrowserError::CpuProfileActive);
        }
        if self.coverage_active {
            return Err(BrowserError::CoverageActive);
        }
        if self.video.is_some() {
            return Err(BrowserError::VideoActive);
        }
        Ok(())
    }

    async fn tabs(&self) -> Result<Vec<BrowserTab>, BrowserError> {
        let active = self.page.target_id();
        let mut tabs = Vec::new();
        for page in self.browser.pages().await? {
            let tab_id = page.target_id().as_ref().to_owned();
            tabs.push(BrowserTab {
                active: page.target_id() == active,
                tab_id,
                title: page.get_title().await?.unwrap_or_default(),
                url: page.url().await?.unwrap_or_default(),
            });
        }
        tabs.sort_by(|left, right| {
            right
                .active
                .cmp(&left.active)
                .then_with(|| left.tab_id.cmp(&right.tab_id))
        });
        Ok(tabs)
    }

    async fn select_tab(&mut self, tab_id: &str) -> Result<(), BrowserError> {
        self.ensure_capture_inactive()?;
        if self.page.target_id().as_ref() == tab_id {
            return Ok(());
        }
        let page = self
            .browser
            .get_page(TargetId::new(tab_id.to_owned()))
            .await
            .map_err(|error| match error {
                CdpError::NotFound => BrowserError::UnknownTab {
                    tab_id: tab_id.to_owned(),
                },
                error => BrowserError::Cdp(error),
            })?;
        page.bring_to_front().await?;
        self.activate_page(page).await
    }

    async fn close_tab(&mut self, tab_id: &str) -> Result<(), BrowserError> {
        self.ensure_capture_inactive()?;
        let pages = self.browser.pages().await?;
        if pages.len() <= 1 {
            return Err(BrowserError::LastTab);
        }
        let page = pages
            .iter()
            .find(|page| page.target_id().as_ref() == tab_id)
            .cloned()
            .ok_or_else(|| BrowserError::UnknownTab {
                tab_id: tab_id.to_owned(),
            })?;
        let active = self.page.target_id().as_ref() == tab_id;
        let replacement = active.then(|| {
            pages
                .iter()
                .find(|candidate| candidate.target_id().as_ref() != tab_id)
                .cloned()
        });
        page.close().await?;
        self.authenticators.remove(tab_id);
        self.closed_targets.insert(tab_id.to_owned());
        if let Some(Some(replacement)) = replacement {
            replacement.bring_to_front().await?;
            self.activate_page(replacement).await?;
        }
        Ok(())
    }

    async fn network_body(
        &self,
        request_id: &str,
        kind: BrowserNetworkBodyKind,
    ) -> Result<(String, bool), BrowserError> {
        let source = {
            let diagnostics = self
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            diagnostics
                .requests
                .iter()
                .find(|entry| entry.request.request_id == request_id)
                .filter(|entry| entry.request.body_available)
                .map(|entry| entry.source.clone())
        }
        .ok_or_else(|| BrowserError::NetworkBodyUnavailable {
            request_id: request_id.to_owned(),
        })?;
        let body = tokio::time::timeout(MAX_NETWORK_BODY_WAIT, async {
            match source {
                NetworkSource::Page => match kind {
                    BrowserNetworkBodyKind::Request => {
                        let response = self
                            .page
                            .execute(GetRequestPostDataParams::new(request_id.to_owned()))
                            .await?;
                        Ok::<_, BrowserError>((response.post_data.clone(), false))
                    }
                    BrowserNetworkBodyKind::Response => {
                        let response = self
                            .page
                            .execute(GetResponseBodyParams::new(request_id.to_owned()))
                            .await?;
                        Ok((response.body.clone(), response.base64_encoded))
                    }
                },
                NetworkSource::ChildTarget {
                    session_id,
                    request_id,
                } => {
                    let body = self
                        .network_observer
                        .body(session_id, request_id, kind)
                        .await?;
                    Ok((body.body, body.base64_encoded))
                }
            }
        })
        .await
        .map_err(|_| BrowserError::NetworkBodyTimeout {
            request_id: request_id.to_owned(),
        })??;
        if body.0.len() > MAX_NETWORK_BODY_BYTES {
            return Err(BrowserError::NetworkBodyTooLarge {
                request_id: request_id.to_owned(),
                bytes: body.0.len(),
                maximum: MAX_NETWORK_BODY_BYTES,
            });
        }
        Ok(body)
    }

    async fn target(&self, target: &BrowserTarget) -> Result<ElementTarget, BrowserError> {
        if let crate::BrowserLocator::Ref { reference } = &target.locator {
            let reference = reference.strip_prefix('@').unwrap_or(reference);
            return self.refs.get(reference).cloned().ok_or_else(|| {
                BrowserError::UnknownReference {
                    reference: format!("@{reference}"),
                }
            });
        }

        let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
        loop {
            if let Some(target) = self.resolve_target_once(target).await? {
                return Ok(target);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(BrowserError::ActionabilityTimeout {
                    selector: target.display(),
                    reason: "target did not match an element in the current frame tree".to_owned(),
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn resolve_target_once(
        &self,
        target: &BrowserTarget,
    ) -> Result<Option<ElementTarget>, BrowserError> {
        let mut base = target.clone();
        let requested_index = base.index.take();
        let mut contexts = vec![None];
        contexts.extend(
            self.snapshot_child_frames()
                .await?
                .into_iter()
                .map(|frame| frame.context_id),
        );
        let query = ElementQuery::Locator(base.clone());
        let expression = count_script(&query)?;
        let mut matches = Vec::with_capacity(contexts.len());
        let mut total = 0_u64;
        for context_id in contexts {
            let count: u64 = evaluate_typed_in_context(&self.page, &expression, context_id).await?;
            if count > 0 {
                matches.push((context_id, count));
                total = total.saturating_add(count);
            }
        }
        if total == 0 {
            return Ok(None);
        }

        let global_index = match requested_index {
            None if total > 1 => {
                return Err(BrowserError::StrictSelectorViolation {
                    selector: target.display(),
                    count: usize::try_from(total).unwrap_or(usize::MAX),
                });
            }
            None | Some(BrowserTargetIndex::First) => 0,
            Some(BrowserTargetIndex::Last) => total.saturating_sub(1),
            Some(BrowserTargetIndex::Nth { index }) => {
                let index = u64::from(index);
                if index >= total {
                    return Ok(None);
                }
                index
            }
        };
        let mut preceding = 0_u64;
        for (context_id, count) in matches {
            if global_index < preceding.saturating_add(count) {
                let local_index = global_index.saturating_sub(preceding);
                base.index = (count > 1).then_some(BrowserTargetIndex::Nth {
                    index: u32::try_from(local_index).unwrap_or(u32::MAX),
                });
                return Ok(Some(ElementTarget {
                    query: ElementQuery::Locator(base),
                    context_id,
                }));
            }
            preceding = preceding.saturating_add(count);
        }
        Ok(None)
    }

    async fn target_for_wait(&self, target: &BrowserTarget) -> Result<ElementTarget, BrowserError> {
        if matches!(target.locator, crate::BrowserLocator::Ref { .. }) {
            return self.target(target).await;
        }
        Ok(self
            .resolve_target_once(target)
            .await?
            .unwrap_or_else(|| ElementTarget {
                query: ElementQuery::Locator(target.clone()),
                context_id: None,
            }))
    }

    async fn count_target(&self, target: &BrowserTarget) -> Result<u64, BrowserError> {
        if let crate::BrowserLocator::Ref { reference } = &target.locator {
            let reference = reference.strip_prefix('@').unwrap_or(reference);
            return Ok(u64::from(self.refs.contains_key(reference)));
        }
        let query = ElementQuery::Locator(target.clone());
        let expression = count_script(&query)?;
        let mut count: u64 = evaluate_typed(&self.page, &expression).await?;
        for frame in self.snapshot_child_frames().await? {
            count = count.saturating_add(
                evaluate_typed_in_context::<u64>(&self.page, &expression, frame.context_id).await?,
            );
        }
        Ok(count)
    }

    fn validate_navigation(&self, url: &Url) -> Result<(), BrowserError> {
        if self.allowed_origins.is_empty()
            || self
                .allowed_origins
                .iter()
                .any(|allowed| allowed.origin() == url.origin())
        {
            return Ok(());
        }
        Err(BrowserError::OriginNotAllowed { url: url.clone() })
    }

    async fn upload_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<String>, BrowserError> {
        if paths.len() > MAX_UPLOAD_FILES {
            return Err(BrowserError::UploadFileLimit {
                count: paths.len(),
                maximum: MAX_UPLOAD_FILES,
            });
        }
        let root = self
            .file_root
            .as_ref()
            .ok_or(BrowserError::FileRootNotConfigured)?;
        let mut resolved = Vec::with_capacity(paths.len());
        for path in paths {
            if path.is_absolute() {
                return Err(BrowserError::FileOutsideRoot { path });
            }
            let candidate = tokio::fs::canonicalize(root.join(&path)).await?;
            if !candidate.starts_with(root) {
                return Err(BrowserError::FileOutsideRoot { path: candidate });
            }
            let metadata = tokio::fs::metadata(&candidate).await?;
            if !metadata.is_file() {
                return Err(BrowserError::UploadFileInvalid { path: candidate });
            }
            if metadata.len() > MAX_UPLOAD_FILE_BYTES {
                return Err(BrowserError::UploadFileTooLarge {
                    path: candidate,
                    bytes: metadata.len(),
                    maximum: MAX_UPLOAD_FILE_BYTES,
                });
            }
            resolved.push(candidate.to_string_lossy().into_owned());
        }
        Ok(resolved)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "cross-frame snapshot assembly keeps reference identity and aggregate limits in one ordered pass"
    )]
    async fn snapshot(
        &mut self,
        interactive: bool,
        compact: bool,
        depth: Option<u32>,
        selector: Option<&str>,
        include_urls: bool,
    ) -> Result<SnapshotData, BrowserError> {
        let mut frames = vec![SnapshotFrame {
            context_id: None,
            frame_id: self
                .page
                .mainframe()
                .await?
                .map(|frame_id| frame_id.as_ref().to_owned()),
            url: None,
            name: None,
        }];
        if selector.is_none() {
            frames.extend(self.snapshot_child_frames().await?);
            frames[1..].sort_by(|left, right| {
                (&left.url, &left.name, left.context_id.map(|id| *id.inner())).cmp(&(
                    &right.url,
                    &right.name,
                    right.context_id.map(|id| *id.inner()),
                ))
            });
        }

        self.refs.clear();
        let mut refs = BTreeMap::new();
        let mut snapshot = String::new();
        let mut origin = String::new();
        for (index, frame) in frames.into_iter().enumerate() {
            let context_id = frame.context_id;
            let frame_url = frame.url;
            let frame_name = frame.name;
            let options = serde_json::to_string(&SnapshotOptions {
                interactive,
                compact,
                depth,
                selector,
                include_urls,
                reference_start: refs.len(),
            })?;
            let expression = format!("({SNAPSHOT_SCRIPT})({options})");
            let wire: SnapshotWire =
                match evaluate_typed_in_context(&self.page, expression, context_id).await {
                    Ok(wire) => wire,
                    Err(error) if index > 0 => {
                        warn!(
                            target: "nanocodex_browser",
                            %error,
                            frame_url,
                            "skipping a child frame that navigated during snapshot"
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            if let Err(error) = validate_snapshot_wire(snapshot.len(), refs.len(), &wire) {
                self.refs.clear();
                return Err(error);
            }
            if index == 0 {
                origin.clone_from(&wire.origin);
                snapshot.clone_from(&wire.snapshot);
            } else {
                let label = frame_name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("embedded frame");
                let url = frame_url.as_deref().unwrap_or(&wire.origin);
                snapshot.push_str("\n- iframe ");
                snapshot.push_str(&serde_json::to_string(label)?);
                snapshot.push_str(" [url=");
                snapshot.push_str(url);
                snapshot.push(']');
                for line in wire.snapshot.lines() {
                    snapshot.push_str("\n  ");
                    snapshot.push_str(line);
                }
            }
            for element in wire.refs {
                self.refs.insert(
                    element.reference.clone(),
                    ElementTarget {
                        query: ElementQuery::SnapshotPath(element.selector_path),
                        context_id,
                    },
                );
                refs.insert(
                    element.reference,
                    BrowserElementReference {
                        role: element.role,
                        name: element.name,
                        disabled: element.disabled,
                        frame_url: frame_url.clone(),
                        frame_id: frame.frame_id.clone(),
                    },
                );
            }
        }
        Ok(SnapshotData {
            origin,
            snapshot,
            refs,
        })
    }

    async fn snapshot_child_frames(&self) -> Result<Vec<SnapshotFrame>, BrowserError> {
        let main_frame = self.page.mainframe().await?;
        let mut frames = Vec::new();
        for frame_id in self.page.frames().await? {
            if main_frame.as_ref() == Some(&frame_id) {
                continue;
            }
            let context_id = match self.page.frame_execution_context(frame_id.clone()).await {
                Ok(Some(context_id)) => context_id,
                Ok(None) => continue,
                Err(error) => {
                    warn!(
                        target: "nanocodex_browser",
                        %error,
                        ?frame_id,
                        "skipping a child frame that navigated during context discovery"
                    );
                    continue;
                }
            };
            let url = match self.page.frame_url(frame_id.clone()).await {
                Ok(url) => url,
                Err(error) => {
                    warn!(
                        target: "nanocodex_browser",
                        %error,
                        ?frame_id,
                        "skipping a child frame that navigated during URL discovery"
                    );
                    continue;
                }
            };
            let name = match self.page.frame_name(frame_id.clone()).await {
                Ok(name) => name,
                Err(error) => {
                    warn!(
                        target: "nanocodex_browser",
                        %error,
                        ?frame_id,
                        "skipping a child frame that navigated during name discovery"
                    );
                    continue;
                }
            };
            frames.push(SnapshotFrame {
                context_id: Some(context_id),
                frame_id: Some(frame_id.as_ref().to_owned()),
                url,
                name,
            });
        }
        Ok(frames)
    }

    async fn frames(&self) -> Result<Vec<BrowserFrame>, BrowserError> {
        let main_frame = self.page.mainframe().await?;
        let mut frames = Vec::new();
        for frame_id in self.page.frames().await? {
            let parent = self.page.frame_parent(frame_id.clone()).await?;
            let name = self.page.frame_name(frame_id.clone()).await?;
            let url = self
                .page
                .frame_url(frame_id.clone())
                .await?
                .unwrap_or_default();
            frames.push(BrowserFrame {
                frame_id: frame_id.as_ref().to_owned(),
                parent_frame_id: parent.map(|parent| parent.as_ref().to_owned()),
                name,
                url,
                main: main_frame.as_ref() == Some(&frame_id),
            });
        }
        frames.sort_by(|left, right| {
            right
                .main
                .cmp(&left.main)
                .then_with(|| left.frame_id.cmp(&right.frame_id))
        });
        Ok(frames)
    }

    async fn frame_context(&self, frame_id: &str) -> Result<ExecutionContextId, BrowserError> {
        let frame_id = self
            .page
            .frames()
            .await?
            .into_iter()
            .find(|candidate| candidate.as_ref() == frame_id)
            .ok_or_else(|| BrowserError::UnknownFrame {
                frame_id: frame_id.to_owned(),
            })?;
        self.page
            .frame_execution_context(frame_id.clone())
            .await?
            .ok_or_else(|| BrowserError::UnknownFrame {
                frame_id: frame_id.as_ref().to_owned(),
            })
    }

    async fn sync_virtual_authenticators(&mut self) -> Result<(), BrowserError> {
        if self.virtual_authenticator.is_none() {
            return Ok(());
        }

        let targets = self.browser.execute(GetTargetsParams::default()).await?;
        let reported_targets = targets
            .target_infos
            .iter()
            .map(|target| target.target_id.as_ref())
            .collect::<HashSet<_>>();
        self.closed_targets
            .retain(|target_id| reported_targets.contains(target_id.as_str()));
        let mut pages = Vec::new();
        for target in &targets.target_infos {
            if !matches!(target.r#type.as_str(), "page" | "iframe") {
                continue;
            }
            if self.closed_targets.contains(target.target_id.as_ref()) {
                continue;
            }
            match self.browser.get_page(target.target_id.clone()).await {
                Ok(page) => pages.push(page),
                Err(CdpError::NotFound) => {}
                Err(error) => return Err(error.into()),
            }
        }

        let active_targets = pages
            .iter()
            .map(|page| page.target_id().as_ref().to_owned())
            .collect::<std::collections::HashSet<_>>();
        self.authenticators
            .retain(|target_id, _| active_targets.contains(target_id));

        for page in pages {
            let target_id = page.target_id().as_ref().to_owned();
            if self.authenticators.contains_key(&target_id) {
                continue;
            }
            let credentials = self
                .selected_virtual_credentials()
                .into_values()
                .collect::<Vec<_>>();
            let id = install_virtual_authenticator(&page, &credentials).await?;
            self.authenticators
                .insert(target_id, InstalledAuthenticator { page, id });
        }
        Ok(())
    }

    async fn synchronize_virtual_credentials(&mut self) -> Result<(), BrowserError> {
        if self.virtual_credential_store.is_none() {
            return Ok(());
        }
        if self.authenticators.is_empty() {
            return Ok(());
        }
        let presented = self
            .selected_virtual_credentials()
            .into_keys()
            .collect::<BTreeSet<_>>();
        let mut authenticators = self
            .authenticators
            .values()
            .map(|authenticator| (authenticator.page.clone(), authenticator.id.clone()))
            .collect::<Vec<_>>();
        authenticators.sort_unstable_by(|left, right| {
            left.0
                .target_id()
                .as_ref()
                .cmp(right.0.target_id().as_ref())
        });
        let mut snapshots = Vec::with_capacity(authenticators.len());
        for (page, id) in &authenticators {
            snapshots.push(
                page.execute(GetCredentialsParams::new(id.clone()))
                    .await?
                    .credentials
                    .clone(),
            );
        }
        self.virtual_credential_store
            .as_mut()
            .ok_or(BrowserError::VirtualCredentialStoreNotConfigured)?
            .reconcile(&snapshots, &presented)?;
        let expected = self.selected_virtual_credentials();
        synchronize_authenticator_snapshots(authenticators, snapshots, &expected).await
    }

    async fn apply_passkey_mode(&self) -> Result<(), BrowserError> {
        let mut authenticators = self
            .authenticators
            .values()
            .map(|authenticator| (authenticator.page.clone(), authenticator.id.clone()))
            .collect::<Vec<_>>();
        authenticators.sort_unstable_by(|left, right| {
            left.0
                .target_id()
                .as_ref()
                .cmp(right.0.target_id().as_ref())
        });
        let mut snapshots = Vec::with_capacity(authenticators.len());
        for (page, id) in &authenticators {
            snapshots.push(
                page.execute(GetCredentialsParams::new(id.clone()))
                    .await?
                    .credentials
                    .clone(),
            );
        }
        let expected = self.selected_virtual_credentials();
        synchronize_authenticator_snapshots(authenticators, snapshots, &expected).await
    }

    fn selected_virtual_credentials(&self) -> BTreeMap<(String, String), Credential> {
        let Some(store) = self.virtual_credential_store.as_ref() else {
            return BTreeMap::new();
        };
        let mut credentials = store.snapshot();
        match &self.passkey_mode {
            BrowserPasskeyMode::Auto => {}
            BrowserPasskeyMode::New => credentials.clear(),
            BrowserPasskeyMode::Use {
                credential_id,
                relying_party_id,
            } => credentials.retain(|(candidate_rp, candidate_id), _| {
                candidate_id == credential_id
                    && relying_party_id
                        .as_ref()
                        .is_none_or(|selected_rp| selected_rp == candidate_rp)
            }),
        }
        credentials
    }

    fn persisted_virtual_credentials(&self) -> Result<Vec<VirtualCredential>, BrowserError> {
        if self.virtual_authenticator.is_none() {
            return Err(BrowserError::VirtualAuthenticatorNotConfigured);
        }
        let store = self
            .virtual_credential_store
            .as_ref()
            .ok_or(BrowserError::VirtualCredentialStoreNotConfigured)?;
        Ok(store
            .credentials()
            .map(virtual_credential_metadata)
            .collect())
    }

    async fn virtual_credentials(&self) -> Result<Vec<VirtualCredential>, BrowserError> {
        if self.virtual_credential_store.is_some() {
            return self.persisted_virtual_credentials();
        }
        if self.virtual_authenticator.is_none() {
            return Err(BrowserError::VirtualAuthenticatorNotConfigured);
        }
        if self.authenticators.is_empty() {
            return Err(BrowserError::VirtualAuthenticatorNotReady);
        }
        let mut credentials = Vec::new();
        for authenticator in self.authenticators.values() {
            let response = authenticator
                .page
                .execute(GetCredentialsParams::new(authenticator.id.clone()))
                .await?;
            credentials.extend(response.credentials.iter().map(virtual_credential_metadata));
        }
        Ok(credentials)
    }

    fn resolve_passkey_mode(
        &self,
        credential_id: String,
        relying_party_id: Option<String>,
    ) -> Result<BrowserPasskeyMode, BrowserError> {
        let store = self
            .virtual_credential_store
            .as_ref()
            .ok_or(BrowserError::VirtualCredentialStoreNotConfigured)?;
        let mut matches = store.credentials().filter(|credential| {
            String::from(credential.credential_id.clone()) == credential_id
                && relying_party_id
                    .as_ref()
                    .is_none_or(|selected| credential.rp_id.as_ref() == Some(selected))
        });
        let selected = matches
            .next()
            .ok_or_else(|| BrowserError::UnknownVirtualCredential {
                credential_id: credential_id.clone(),
            })?;
        if matches.next().is_some() {
            return Err(BrowserError::AmbiguousVirtualCredential { credential_id });
        }
        Ok(BrowserPasskeyMode::Use {
            credential_id,
            relying_party_id: selected.rp_id.clone(),
        })
    }

    fn passkeys_result(
        &self,
        sequence: u64,
        action: BrowserActionName,
    ) -> Result<BrowserActionResult, BrowserError> {
        Ok(BrowserActionResult::Passkeys {
            sequence,
            executed: true,
            action,
            mode: self.passkey_mode.clone(),
            credentials: self.persisted_virtual_credentials()?,
        })
    }
}

fn virtual_credential_metadata(credential: &Credential) -> VirtualCredential {
    VirtualCredential {
        credential_id: String::from(credential.credential_id.clone()),
        relying_party_id: credential.rp_id.clone(),
        is_resident: credential.is_resident_credential,
        user_name: credential.user_name.clone(),
        user_display_name: credential.user_display_name.clone(),
        sign_count: credential.sign_count,
    }
}

async fn synchronize_authenticator_snapshots(
    authenticators: Vec<(Page, AuthenticatorId)>,
    snapshots: Vec<Vec<Credential>>,
    expected: &BTreeMap<(String, String), Credential>,
) -> Result<(), BrowserError> {
    for ((page, authenticator_id), snapshot) in authenticators.into_iter().zip(snapshots) {
        let current = snapshot
            .into_iter()
            .filter_map(|credential| {
                let relying_party = credential.rp_id.clone()?;
                let key = (
                    relying_party,
                    String::from(credential.credential_id.clone()),
                );
                Some((key, credential))
            })
            .collect::<BTreeMap<_, _>>();
        for (key, credential) in &current {
            if !expected.contains_key(key) {
                page.execute(RemoveCredentialParams::new(
                    authenticator_id.clone(),
                    credential.credential_id.clone(),
                ))
                .await?;
            }
        }
        for (key, credential) in expected {
            match current.get(key) {
                Some(current) if current == credential => {}
                Some(current) => {
                    page.execute(RemoveCredentialParams::new(
                        authenticator_id.clone(),
                        current.credential_id.clone(),
                    ))
                    .await?;
                    page.execute(AddCredentialParams::new(
                        authenticator_id.clone(),
                        credential.clone(),
                    ))
                    .await?;
                }
                None => {
                    page.execute(AddCredentialParams::new(
                        authenticator_id.clone(),
                        credential.clone(),
                    ))
                    .await?;
                }
            }
        }
    }
    Ok(())
}

fn validate_snapshot_wire(
    retained_bytes: usize,
    retained_refs: usize,
    wire: &SnapshotWire,
) -> Result<(), BrowserError> {
    let bytes = u64::try_from(retained_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(serialized_size(wire)?);
    let refs = retained_refs.saturating_add(wire.refs.len());
    if bytes > u64::try_from(MAX_SNAPSHOT_BYTES).unwrap_or(u64::MAX) || refs > MAX_SNAPSHOT_REFS {
        return Err(BrowserError::SnapshotTooLarge {
            bytes,
            refs,
            maximum_bytes: MAX_SNAPSHOT_BYTES,
            maximum_refs: MAX_SNAPSHOT_REFS,
        });
    }
    Ok(())
}

async fn export_profile_cookies(
    runtime_dir: &Path,
    brave_session: &BraveSession,
) -> Result<Vec<CookieParam>, BrowserError> {
    let broker_profile = tempfile::Builder::new()
        .prefix("brave-cookie-broker-")
        .tempdir_in(runtime_dir)?;
    brave_session.prepare(broker_profile.path()).await?;

    let config = profile_launch_config(
        BrowserConfig::builder()
            .user_data_dir(broker_profile.path())
            .new_headless_mode(),
    )
    .chrome_executable(brave_session.executable());
    let (mut browser, mut events) = Chromium::launch(build_config(config)?).await?;
    let handler = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let Err(error) = event {
                warn!(
                    target: "nanocodex_browser",
                    %error,
                    "Brave cookie broker handler stopped"
                );
                break;
            }
        }
    });

    let exported = browser.get_cookies().await.map_err(BrowserError::from);
    let shutdown = close_chromium(&mut browser, &handler).await;
    let cookies = exported?;
    shutdown?;

    let cookies = allowed_cookie_params(
        cookies,
        (!brave_session.copies_all_cookies()).then(|| brave_session.allowed_origins()),
    );
    info!(
        target: "nanocodex_browser",
        browser_cookie_count = cookies.len(),
        browser_cookie_origin_count = brave_session.allowed_origins().len(),
        browser_cookie_all_origins = brave_session.copies_all_cookies(),
        "prepared source-profile cookies for a dedicated browser"
    );
    Ok(cookies)
}

fn allowed_cookie_params(
    cookies: Vec<Cookie>,
    allowed_origins: Option<&[Url]>,
) -> Vec<CookieParam> {
    let allowed_hosts =
        allowed_origins.map(|origins| origins.iter().filter_map(Url::host_str).collect::<Vec<_>>());
    cookies
        .into_iter()
        .filter(|cookie| {
            allowed_hosts.as_ref().is_none_or(|hosts| {
                hosts
                    .iter()
                    .any(|allowed| cookie_applies_to(&cookie.domain, allowed))
            })
        })
        .filter_map(cookie_param)
        .collect()
}

fn cookie_param(cookie: Cookie) -> Option<CookieParam> {
    let Cookie {
        name,
        value,
        domain,
        path,
        expires,
        http_only,
        secure,
        session,
        same_site,
        priority,
        source_scheme,
        source_port,
        partition_key,
        partition_key_opaque,
        ..
    } = cookie;
    if partition_key_opaque == Some(true) {
        return None;
    }
    let expires =
        (!session && expires.is_finite() && expires >= 0.0).then(|| TimeSinceEpoch::new(expires));
    Some(CookieParam {
        name,
        value,
        url: None,
        domain: Some(domain),
        path: Some(path),
        secure: Some(secure),
        http_only: Some(http_only),
        same_site,
        expires,
        priority: Some(priority),
        same_party: None,
        source_scheme: Some(source_scheme),
        source_port: Some(source_port),
        partition_key,
    })
}

async fn replace_browser_cookies(
    browser: &Chromium,
    cookies: Vec<CookieParam>,
) -> Result<(), BrowserError> {
    let count = cookies.len();
    browser.clear_cookies().await?;
    if !cookies.is_empty() {
        browser.set_cookies(cookies).await?;
    }
    info!(
        target: "nanocodex_browser",
        browser_cookie_count = count,
        "synchronized cookies into the dedicated browser"
    );
    Ok(())
}

async fn close_chromium(
    browser: &mut Chromium,
    handler: &JoinHandle<()>,
) -> Result<(), BrowserError> {
    let close = browser.close().await.map(drop).map_err(BrowserError::from);
    let wait = browser.wait().await.map(drop).map_err(BrowserError::from);
    handler.abort();
    close?;
    wait
}

async fn install_virtual_authenticator(
    page: &Page,
    credentials: &[Credential],
) -> Result<AuthenticatorId, BrowserError> {
    page.execute(EnableParams::builder().enable_ui(false).build())
        .await?;
    let mut options = VirtualAuthenticatorOptions::new(
        AuthenticatorProtocol::Ctap2,
        AuthenticatorTransport::Internal,
    );
    options.has_resident_key = Some(true);
    options.has_user_verification = Some(true);
    options.automatic_presence_simulation = Some(true);
    options.is_user_verified = Some(true);
    let response = page
        .execute(AddVirtualAuthenticatorParams::new(options))
        .await?;
    for credential in credentials {
        page.execute(AddCredentialParams::new(
            response.authenticator_id.clone(),
            credential.clone(),
        ))
        .await?;
    }
    Ok(response.authenticator_id.clone())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReactDiagnosticsOptions {
    max_events: usize,
    include_profiling_hooks: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "context installation is kept in protocol order so every pre-navigation override is auditable"
)]
async fn apply_browser_context(
    browser: &Chromium,
    page: &Page,
    context: &BrowserContext,
) -> Result<(), BrowserError> {
    if let Some(viewport) = context.viewport {
        page.execute(SetDeviceMetricsOverrideParams::new(
            i64::from(viewport.width),
            i64::from(viewport.height),
            viewport.device_scale_factor,
            viewport.mobile,
        ))
        .await?;
        let mut touch = SetTouchEmulationEnabledParams::new(viewport.touch);
        if viewport.touch {
            touch.max_touch_points = Some(i64::from(viewport.max_touch_points));
        }
        page.execute(touch).await?;
    }

    if let Some(locale) = &context.locale {
        page.execute(SetLocaleOverrideParams::builder().locale(locale).build())
            .await?;
    }
    if let Some(timezone) = &context.timezone {
        page.execute(SetTimezoneOverrideParams::new(timezone))
            .await?;
    }
    if let Some(geolocation) = context.geolocation {
        page.execute(
            SetGeolocationOverrideParams::builder()
                .latitude(geolocation.latitude)
                .longitude(geolocation.longitude)
                .accuracy(geolocation.accuracy_meters)
                .build(),
        )
        .await?;
    }

    let accept_language = context.accept_language.as_ref().or(context.locale.as_ref());
    if context.user_agent.is_some() || accept_language.is_some() || context.platform.is_some() {
        let user_agent = match &context.user_agent {
            Some(user_agent) => user_agent.clone(),
            None => page
                .evaluate("navigator.userAgent")
                .await?
                .into_value::<String>()?,
        };
        let mut params = SetUserAgentOverrideParams::new(user_agent);
        params.accept_language = accept_language.cloned();
        params.platform.clone_from(&context.platform);
        page.execute(params).await?;
    }

    let mut media_features = Vec::with_capacity(2);
    if let Some(color_scheme) = context.color_scheme {
        media_features.push(MediaFeature::new(
            "prefers-color-scheme",
            match color_scheme {
                BrowserColorScheme::Light => "light",
                BrowserColorScheme::Dark => "dark",
            },
        ));
    }
    if let Some(reduced_motion) = context.reduced_motion {
        media_features.push(MediaFeature::new(
            "prefers-reduced-motion",
            match reduced_motion {
                BrowserReducedMotion::NoPreference => "no-preference",
                BrowserReducedMotion::Reduce => "reduce",
            },
        ));
    }
    if !media_features.is_empty() {
        page.execute(
            SetEmulatedMediaParams::builder()
                .features(media_features)
                .build(),
        )
        .await?;
    }

    if let Some(rate) = context.cpu_throttle_rate {
        page.execute(SetCpuThrottlingRateParams::new(rate)).await?;
    }
    if let Some(network) = context.network {
        page.execute(EmulateNetworkConditionsByRuleParams::new(
            network.offline,
            vec![NetworkConditions::new(
                "",
                network.latency_ms,
                network.download_bytes_per_second,
                network.upload_bytes_per_second,
            )],
        ))
        .await?;
        page.execute(OverrideNetworkStateParams::new(
            network.offline,
            network.latency_ms,
            network.download_bytes_per_second,
            network.upload_bytes_per_second,
        ))
        .await?;
    }

    let mut headers = serde_json::Map::new();
    for (name, value) in &context.extra_headers {
        headers.insert(name.clone(), serde_json::Value::String(value.clone()));
    }
    if let Some((username, password)) = &context.http_credentials {
        let credentials = STANDARD.encode(format!("{username}:{password}"));
        headers.insert(
            "authorization".to_owned(),
            serde_json::Value::String(format!("Basic {credentials}")),
        );
    }
    if !headers.is_empty() {
        page.execute(SetExtraHttpHeadersParams::new(Headers::new(
            serde_json::Value::Object(headers),
        )))
        .await?;
    }

    for source in &context.init_scripts {
        let mut params = AddScriptToEvaluateOnNewDocumentParams::new(source);
        params.run_immediately = Some(true);
        page.add_init_script(params).await?;
    }

    for grant in &context.permissions {
        for permission_name in permission_names(grant.permission) {
            let mut params = SetPermissionParams::new(
                PermissionDescriptor::new(*permission_name),
                PermissionSetting::Granted,
            );
            params.origin = grant
                .origin
                .as_ref()
                .map(|origin| origin.origin().ascii_serialization());
            browser.execute(params).await?;
        }
    }
    Ok(())
}

const fn permission_names(permission: BrowserPermission) -> &'static [&'static str] {
    match permission {
        BrowserPermission::Geolocation => &["geolocation"],
        BrowserPermission::ClipboardReadWrite => &["clipboard-read", "clipboard-write"],
        BrowserPermission::Notifications => &["notifications"],
        BrowserPermission::Camera => &["camera"],
        BrowserPermission::Microphone => &["microphone"],
        BrowserPermission::Midi => &["midi"],
        BrowserPermission::PointerLock => &["pointer-lock"],
        BrowserPermission::PaymentHandler => &["payment-handler"],
    }
}

async fn install_storage_state(
    page: &Page,
    state: &BrowserStorageState,
) -> Result<(), BrowserError> {
    let origins = serde_json::to_string(&state.origins)?;
    let source = format!(
        r"(() => {{
  const state = {origins}.find(candidate => candidate.origin === location.origin);
  if (!state) return;
  try {{
    localStorage.clear();
    for (const [key, value] of Object.entries(state.localStorage)) {{
      localStorage.setItem(key, value);
    }}
    sessionStorage.clear();
    for (const [key, value] of Object.entries(state.sessionStorage)) {{
      sessionStorage.setItem(key, value);
    }}
  }} catch {{
    // Opaque documents such as about:blank do not expose origin storage.
  }}
}})()"
    );
    let mut params = AddScriptToEvaluateOnNewDocumentParams::new(source);
    params.run_immediately = Some(true);
    page.add_init_script(params).await?;
    Ok(())
}

fn storage_cookie_params(state: &BrowserStorageState) -> Vec<CookieParam> {
    state
        .cookies
        .iter()
        .map(|cookie| {
            let mut param = CookieParam::new(&cookie.name, &cookie.value);
            param.domain = Some(cookie.domain.clone());
            param.path = Some(cookie.path.clone());
            param.secure = Some(cookie.secure);
            param.http_only = Some(cookie.http_only);
            param.same_site = cookie.same_site.map(|same_site| match same_site {
                BrowserCookieSameSite::Strict => CookieSameSite::Strict,
                BrowserCookieSameSite::Lax => CookieSameSite::Lax,
                BrowserCookieSameSite::None => CookieSameSite::None,
            });
            param.expires = cookie
                .expires_epoch_seconds
                .map(chromiumoxide::cdp::browser_protocol::network::TimeSinceEpoch::new);
            param
        })
        .collect()
}

fn browser_cookie(cookie: Cookie) -> BrowserCookie {
    BrowserCookie {
        name: cookie.name,
        value: cookie.value,
        domain: cookie.domain,
        path: cookie.path,
        expires_epoch_seconds: (!cookie.session && cookie.expires >= 0.0).then_some(cookie.expires),
        http_only: cookie.http_only,
        secure: cookie.secure,
        same_site: cookie.same_site.map(|same_site| match same_site {
            CookieSameSite::Strict => BrowserCookieSameSite::Strict,
            CookieSameSite::Lax => BrowserCookieSameSite::Lax,
            CookieSameSite::None => BrowserCookieSameSite::None,
        }),
    }
}

const CAPTURE_ORIGIN_STORAGE_SCRIPT: &str = r#"(() => {
  if (!/^https?:$/.test(location.protocol)) return null;
  const entries = storage => Object.fromEntries(
    Array.from({ length: storage.length }, (_, index) => storage.key(index))
      .filter(key => key != null)
      .sort()
      .map(key => [key, storage.getItem(key) ?? ""])
  );
  return {
    origin: location.origin,
    localStorage: entries(localStorage),
    sessionStorage: entries(sessionStorage)
  };
})()"#;

async fn install_react_diagnostics(
    page: &Page,
    diagnostics: ReactDiagnostics,
) -> Result<(), BrowserError> {
    let options = ReactDiagnosticsOptions {
        max_events: REACT_DIAGNOSTICS_MAX_EVENTS,
        include_profiling_hooks: diagnostics.include_profiling_hooks(),
    };
    let source = format!(
        "{REACT_DIAGNOSTICS_BUNDLE}\n;globalThis.__nanocodexInstallReactDiagnostics({});",
        serde_json::to_string(&options)?
    );
    let mut params = AddScriptToEvaluateOnNewDocumentParams::new(source);
    params.run_immediately = Some(true);
    page.add_init_script(params).await?;
    Ok(())
}

async fn install_restricted_network_guards(page: &Page) -> Result<(), BrowserError> {
    const SOURCE: &str = r#"(() => {
  const blocked = class {
    constructor() {
      throw new DOMException(
        "WebRTC is disabled by the browser egress policy",
        "SecurityError"
      );
    }
  };
  for (const name of ["RTCPeerConnection", "webkitRTCPeerConnection"]) {
    if (name in globalThis) {
      Object.defineProperty(globalThis, name, {
        value: blocked,
        configurable: false,
        writable: false
      });
    }
  }
})()"#;
    let mut params = AddScriptToEvaluateOnNewDocumentParams::new(SOURCE);
    params.run_immediately = Some(true);
    page.add_init_script(params).await?;
    page.execute(SetBypassServiceWorkerParams::new(true))
        .await?;
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        self.handler.abort();
        for task in &self.browser_tasks {
            task.abort();
        }
        for task in &self.page_tasks {
            task.abort();
        }
    }
}

impl Drop for NativeBrowser {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        let Some(session) = state.session.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = session.close().await {
                warn!(
                    target: "nanocodex_browser",
                    %error,
                    "failed to close browser while dropping final handle"
                );
            }
        });
    }
}

impl NativeBrowser {
    #[allow(
        clippy::too_many_arguments,
        reason = "private construction mirrors the deliberate public builder policy without another config layer"
    )]
    pub(crate) fn new(
        executable: Option<PathBuf>,
        cdp_endpoint: Option<Url>,
        brave_session: Option<BraveSession>,
        launch_brave_executable: bool,
        virtual_authenticator: Option<VirtualAuthenticator>,
        react_diagnostics: Option<ReactDiagnostics>,
        egress_policy: Option<BrowserEgressPolicy>,
        file_root: Option<PathBuf>,
        context: BrowserContext,
        storage_state: Option<BrowserStorageState>,
        after_action: BrowserAfterAction,
        ffmpeg_executable: Option<PathBuf>,
        lighthouse_executable: Option<PathBuf>,
        crux_client: Option<BrowserCruxClient>,
    ) -> Result<Arc<Self>, BrowserBuildError> {
        Ok(Arc::new(Self {
            runtime_dir: tempfile::Builder::new()
                .prefix("nanocodex-browser-")
                .tempdir()?,
            executable,
            cdp_endpoint,
            brave_session,
            launch_brave_executable,
            virtual_authenticator,
            react_diagnostics,
            egress_policy,
            file_root,
            context,
            storage_state,
            after_action,
            ffmpeg_executable,
            lighthouse_executable,
            crux_client,
            state: Mutex::new(BrowserState::default()),
        }))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the method keeps one browser action's bounded lifecycle and trace ordering linear"
    )]
    pub(crate) async fn execute(
        &self,
        action: BrowserAction,
    ) -> Result<BrowserActionResult, BrowserError> {
        let action_bytes = serialized_size(&action)?;
        if action_bytes > MAX_ACTION_INPUT_BYTES {
            return Err(BrowserError::ActionInputTooLarge {
                bytes: action_bytes,
                maximum: MAX_ACTION_INPUT_BYTES,
            });
        }
        let mut state = self.state.lock().await;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let action_name = action.name();
        let span = info_span!(
            target: "nanocodex_browser",
            "browser.action",
            browser.sequence = sequence,
            browser.action = ?action_name,
        );
        async {
            trace_serialized("action.input", &action);
            if state.closed {
                return Err(BrowserError::Closed);
            }
            self.ensure_session(&mut state).await?;
            let session = state
                .session
                .as_mut()
                .ok_or(BrowserError::SessionUnavailable)?;
            if !matches!(&action, BrowserAction::Open { .. }) {
                session.sync_virtual_authenticators().await?;
            }
            let observation = interaction::ActionObservation::capture(&session.diagnostics)?;
            let wait_for_completion = requires_action_completion(action_name);
            let trace_action = action.clone();
            let started = Instant::now();
            let execution = async {
                let mut result = execute_action(session, sequence, action).await?;
                if captures_action_outcome(action_name)
                    && let BrowserActionResult::Action { outcome, .. } = &mut result
                {
                    let network = if wait_for_completion {
                        interaction::wait_for_completion(
                            &session.page,
                            &session.diagnostics,
                            &observation,
                        )
                        .await?
                    } else {
                        interaction::empty_network()
                    };
                    *outcome = Some(Box::new(
                        capture_action_outcome(session, &observation, network, self.after_action)
                            .await?,
                    ));
                }
                Ok::<_, BrowserError>(result)
            }
            .await;
            let credential_sync = session.synchronize_virtual_credentials().await;
            let execution = match (execution, credential_sync) {
                (Ok(result), Ok(())) => Ok(result),
                (Ok(_), Err(error)) | (Err(error), _) => Err(error),
            };
            let duration = started.elapsed();
            let result = match execution {
                Ok(result) => {
                    if let Some(trace) = session.action_trace.as_mut() {
                        trace
                            .record_success(
                                &session.page,
                                sequence,
                                duration,
                                trace_action,
                                result.clone(),
                            )
                            .await?;
                    }
                    result
                }
                Err(error) => {
                    if let Some(trace) = session.action_trace.as_mut() {
                        trace
                            .record_failure(sequence, duration, trace_action, error.to_string())
                            .await?;
                    }
                    info!(
                        target: "nanocodex_browser",
                        sequence,
                        error = %error,
                        "browser action failed"
                    );
                    return Err(error);
                }
            };
            trace_serialized("action.result", &result);
            info!(
                target: "nanocodex_browser",
                sequence,
                action = ?action_name,
                "browser action completed"
            );
            Ok(result)
        }
        .instrument(span)
        .await
    }

    pub(crate) async fn start(&self) -> Result<(), BrowserError> {
        let span = info_span!(target: "nanocodex_browser", "browser.start");
        async {
            let mut state = self.state.lock().await;
            if state.closed {
                return Err(BrowserError::Closed);
            }
            self.ensure_session(&mut state).await?;
            info!(target: "nanocodex_browser", "browser session started");
            Ok(())
        }
        .instrument(span)
        .await
    }

    pub(crate) async fn storage_state(&self) -> Result<BrowserStorageState, BrowserError> {
        let span = info_span!(target: "nanocodex_browser", "browser.storage_state");
        async {
            let mut state = self.state.lock().await;
            if state.closed {
                return Err(BrowserError::Closed);
            }
            self.ensure_session(&mut state).await?;
            let storage_state = state
                .session
                .as_ref()
                .ok_or(BrowserError::SessionUnavailable)?
                .storage_state()
                .await?;
            trace_serialized("storage_state.result", &storage_state);
            Ok(storage_state)
        }
        .instrument(span)
        .await
    }

    pub(crate) async fn restore_storage_state(
        &self,
        storage_state: BrowserStorageState,
    ) -> Result<(), BrowserError> {
        let span = info_span!(target: "nanocodex_browser", "browser.restore_storage_state");
        async {
            trace_serialized("storage_state.input", &storage_state);
            let mut state = self.state.lock().await;
            if state.closed {
                return Err(BrowserError::Closed);
            }
            self.ensure_session(&mut state).await?;
            state
                .session
                .as_mut()
                .ok_or(BrowserError::SessionUnavailable)?
                .restore_storage_state(storage_state)
                .await
        }
        .instrument(span)
        .await
    }

    async fn ensure_session(&self, state: &mut BrowserState) -> Result<(), BrowserError> {
        if state.session.is_none() {
            state.session = Some(Session::launch(self).await?);
        }
        Ok(())
    }

    pub(crate) async fn close(&self) -> Result<(), BrowserError> {
        let session = {
            let mut state = self.state.lock().await;
            state.closed = true;
            state.session.take()
        };
        if let Some(session) = session {
            session.close().await?;
        }
        Ok(())
    }

    pub(crate) fn open_auth_handoff(&self, url: &Url) -> Result<(), BrowserError> {
        let session = self
            .brave_session
            .as_ref()
            .ok_or(BrowserError::BraveSessionNotConfigured)?;
        session.open_handoff(url)?;
        info!(
            target: "nanocodex_browser",
            url = url.as_str(),
            "opened authentication handoff in ordinary Brave"
        );
        Ok(())
    }

    pub(crate) fn validate_auth_handoff(&self, url: &Url) -> Result<(), BrowserError> {
        let session = self
            .brave_session
            .as_ref()
            .ok_or(BrowserError::BraveSessionNotConfigured)?;
        session.validate_handoff_url(url)?;
        Ok(())
    }

    pub(crate) async fn resume_auth_handoff(&self, url: Url) -> Result<(), BrowserError> {
        let span = info_span!(
            target: "nanocodex_browser",
            "browser.auth_handoff.resume"
        );
        async {
            let mut state = self.state.lock().await;
            if state.closed {
                return Err(BrowserError::Closed);
            }
            let brave_session = self
                .brave_session
                .as_ref()
                .ok_or(BrowserError::BraveSessionNotConfigured)?;
            brave_session.validate_handoff_url(&url)?;
            if self.cdp_endpoint.is_some() {
                if let Some(session) = state.session.as_mut() {
                    session
                        .refresh_remote_cookies(self.runtime_dir.path(), brave_session)
                        .await?;
                } else {
                    state.session = Some(Session::launch(self).await?);
                }
            } else {
                if let Some(session) = state.session.take() {
                    session.close().await?;
                }
                let profile = self.runtime_dir.path().join("profile");
                match tokio::fs::remove_dir_all(&profile).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                state.session = Some(Session::launch(self).await?);
            }
            let session = state
                .session
                .as_mut()
                .ok_or(BrowserError::SessionUnavailable)?;
            validate_url(url.as_str())?;
            session.validate_navigation(&url)?;
            navigate(&session.page, url.as_str()).await?;
            session.sync_virtual_authenticators().await?;
            session.refs.clear();
            info!(
                target: "nanocodex_browser",
                url = url.as_str(),
                "resumed private headless browser after authentication handoff"
            );
            Ok(())
        }
        .instrument(span)
        .await
    }

    pub(crate) async fn virtual_credentials(&self) -> Result<Vec<VirtualCredential>, BrowserError> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(BrowserError::Closed);
        }
        let session = state
            .session
            .as_mut()
            .ok_or(BrowserError::VirtualAuthenticatorNotReady)?;
        session.sync_virtual_authenticators().await?;
        session.synchronize_virtual_credentials().await?;
        session.virtual_credentials().await
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive match keeps the typed action implementation auditable"
)]
async fn execute_action(
    session: &mut Session,
    sequence: u64,
    action: BrowserAction,
) -> Result<BrowserActionResult, BrowserError> {
    match action {
        BrowserAction::Open { url } => {
            validate_url(&url)?;
            session.validate_navigation(&Url::parse(&url)?)?;
            navigate(&session.page, &url).await?;
            session.sync_virtual_authenticators().await?;
            session.refs.clear();
            Ok(action_result(sequence, BrowserActionName::Open))
        }
        BrowserAction::Reload => {
            reload(&session.page).await?;
            session.sync_virtual_authenticators().await?;
            session.refs.clear();
            Ok(action_result(sequence, BrowserActionName::Reload))
        }
        BrowserAction::DetectGate => {
            let signals: GateSignals =
                evaluate_typed(&session.page, format!("({DETECT_GATE_SCRIPT})()")).await?;
            Ok(BrowserActionResult::Gate {
                sequence,
                executed: true,
                gate: classify_gate(&signals),
            })
        }
        BrowserAction::Passkeys => {
            session.synchronize_virtual_credentials().await?;
            session.passkeys_result(sequence, BrowserActionName::Passkeys)
        }
        BrowserAction::PasskeyUse {
            credential_id,
            relying_party_id,
        } => {
            session.synchronize_virtual_credentials().await?;
            session.passkey_mode = session.resolve_passkey_mode(credential_id, relying_party_id)?;
            session.apply_passkey_mode().await?;
            session.passkeys_result(sequence, BrowserActionName::PasskeyUse)
        }
        BrowserAction::PasskeyNew => {
            session.persisted_virtual_credentials()?;
            session.synchronize_virtual_credentials().await?;
            session.passkey_mode = BrowserPasskeyMode::New;
            session.apply_passkey_mode().await?;
            session.passkeys_result(sequence, BrowserActionName::PasskeyNew)
        }
        BrowserAction::PasskeyAuto => {
            session.persisted_virtual_credentials()?;
            session.synchronize_virtual_credentials().await?;
            session.passkey_mode = BrowserPasskeyMode::Auto;
            session.apply_passkey_mode().await?;
            session.passkeys_result(sequence, BrowserActionName::PasskeyAuto)
        }
        BrowserAction::Snapshot {
            interactive,
            compact,
            depth,
            selector,
            include_urls,
        } => {
            let data = session
                .snapshot(
                    interactive,
                    compact,
                    depth,
                    selector.as_deref(),
                    include_urls,
                )
                .await?;
            Ok(BrowserActionResult::Snapshot {
                sequence,
                executed: true,
                origin: data.origin,
                snapshot: data.snapshot,
                refs: data.refs,
            })
        }
        BrowserAction::SnapshotFind { query, max_results } => {
            let data = session.snapshot(false, false, None, None, true).await?;
            let maximum = usize::from(max_results.unwrap_or(20).clamp(1, 100));
            let (matches, refs, truncated) =
                interaction::snapshot_matches(&data.snapshot, &data.refs, &query, maximum);
            Ok(BrowserActionResult::SnapshotFind {
                sequence,
                executed: true,
                origin: data.origin,
                query,
                matches,
                refs,
                truncated,
            })
        }
        BrowserAction::DomSnapshot {
            computed_styles,
            include_dom_rects,
            include_paint_order,
        } => {
            if computed_styles.len() > MAX_DOM_COMPUTED_STYLES {
                return Err(BrowserError::DomComputedStylesLimit {
                    count: computed_styles.len(),
                    maximum: MAX_DOM_COMPUTED_STYLES,
                });
            }
            let snapshot = capture_dom_snapshot(
                &session.page,
                computed_styles,
                include_dom_rects,
                include_paint_order,
            )
            .await?;
            Ok(BrowserActionResult::DomSnapshot {
                sequence,
                executed: true,
                snapshot: Box::new(snapshot),
            })
        }
        BrowserAction::Click { target, options } => {
            let target = session.target(&target).await?;
            let point = interaction::actionable_point(
                &session.page,
                &target,
                interaction::Actionability::Pointer,
            )
            .await?;
            interaction::dispatch_click(&session.page, point, options.unwrap_or_default()).await?;
            Ok(action_result(sequence, BrowserActionName::Click))
        }
        BrowserAction::Fill { target, text } => {
            let target = session.target(&target).await?;
            interaction::wait_until_actionable(
                &session.page,
                &target,
                interaction::Actionability::Editable,
            )
            .await?;
            evaluate_value_in_context(
                &session.page,
                element_script(
                    &target.query,
                    &format!(
                        r#"const value = {};
element.focus();
if (element.isContentEditable) {{
  element.textContent = value;
}} else {{
  const prototype = element instanceof HTMLTextAreaElement
    ? HTMLTextAreaElement.prototype
    : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
  if (setter) setter.call(element, value); else element.value = value;
}}
element.dispatchEvent(new InputEvent("input", {{ bubbles: true, inputType: "insertText", data: value }}));
element.dispatchEvent(new Event("change", {{ bubbles: true }}));
return true;"#,
                        serde_json::to_string(&text)?
                    ),
                )?,
                target.context_id,
                true,
            )
            .await?;
            Ok(action_result(sequence, BrowserActionName::Fill))
        }
        BrowserAction::Press {
            target,
            key,
            modifiers,
        } => {
            let target = session.target(&target).await?;
            interaction::wait_until_actionable(
                &session.page,
                &target,
                interaction::Actionability::Enabled,
            )
            .await?;
            evaluate_value_in_context(
                &session.page,
                element_script(&target.query, "element.focus(); return true;")?,
                target.context_id,
                true,
            )
            .await?;
            press_key(&session.page, &key, &modifiers).await?;
            Ok(action_result(sequence, BrowserActionName::Press))
        }
        BrowserAction::Hover { target } => {
            let target = session.target(&target).await?;
            let point = interaction::actionable_point(
                &session.page,
                &target,
                interaction::Actionability::Pointer,
            )
            .await?;
            session.page.move_mouse(point).await?;
            session.pointer_x = point.x;
            session.pointer_y = point.y;
            Ok(action_result(sequence, BrowserActionName::Hover))
        }
        BrowserAction::MouseMove { x, y, steps } => {
            let from = Point {
                x: session.pointer_x,
                y: session.pointer_y,
            };
            let to = Point {
                x: f64::from(x),
                y: f64::from(y),
            };
            interaction::dispatch_mouse_move(
                &session.page,
                from,
                to,
                steps.unwrap_or(1).clamp(1, 100),
                session.pointer_buttons,
            )
            .await?;
            session.pointer_x = to.x;
            session.pointer_y = to.y;
            Ok(action_result(sequence, BrowserActionName::MouseMove))
        }
        BrowserAction::MouseDown { button, modifiers } => {
            let button_mask = interaction::mouse_button_mask(button);
            let buttons = session.pointer_buttons | button_mask;
            interaction::dispatch_mouse_button(
                &session.page,
                Point {
                    x: session.pointer_x,
                    y: session.pointer_y,
                },
                button,
                &modifiers,
                true,
                buttons,
            )
            .await?;
            session.pointer_buttons = buttons;
            Ok(action_result(sequence, BrowserActionName::MouseDown))
        }
        BrowserAction::MouseUp { button, modifiers } => {
            let buttons = session.pointer_buttons & !interaction::mouse_button_mask(button);
            interaction::dispatch_mouse_button(
                &session.page,
                Point {
                    x: session.pointer_x,
                    y: session.pointer_y,
                },
                button,
                &modifiers,
                false,
                buttons,
            )
            .await?;
            session.pointer_buttons = buttons;
            Ok(action_result(sequence, BrowserActionName::MouseUp))
        }
        BrowserAction::MouseWheel { delta_x, delta_y } => {
            interaction::dispatch_mouse_wheel(
                &session.page,
                Point {
                    x: session.pointer_x,
                    y: session.pointer_y,
                },
                f64::from(delta_x),
                f64::from(delta_y),
                session.pointer_buttons,
            )
            .await?;
            Ok(action_result(sequence, BrowserActionName::MouseWheel))
        }
        BrowserAction::TouchTap { x, y } => {
            interaction::dispatch_touch_tap(
                &session.page,
                Point {
                    x: f64::from(x),
                    y: f64::from(y),
                },
            )
            .await?;
            Ok(action_result(sequence, BrowserActionName::TouchTap))
        }
        BrowserAction::TouchSwipe {
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
            steps,
        } => {
            interaction::dispatch_touch_swipe(
                &session.page,
                Point {
                    x: f64::from(from_x),
                    y: f64::from(from_y),
                },
                Point {
                    x: f64::from(to_x),
                    y: f64::from(to_y),
                },
                Duration::from_millis(duration_ms.unwrap_or(250).min(5_000)),
                steps.unwrap_or(10).clamp(1, 100),
            )
            .await?;
            Ok(action_result(sequence, BrowserActionName::TouchSwipe))
        }
        BrowserAction::KeyboardDown { key, modifiers } => {
            interaction::dispatch_keyboard_event(&session.page, &key, &modifiers, true).await?;
            Ok(action_result(sequence, BrowserActionName::KeyboardDown))
        }
        BrowserAction::KeyboardUp { key, modifiers } => {
            interaction::dispatch_keyboard_event(&session.page, &key, &modifiers, false).await?;
            Ok(action_result(sequence, BrowserActionName::KeyboardUp))
        }
        BrowserAction::InsertText { text } => {
            interaction::insert_text(&session.page, text).await?;
            Ok(action_result(sequence, BrowserActionName::InsertText))
        }
        BrowserAction::Scroll { target, x, y } => {
            if let Some(target) = target {
                let target = session.target(&target).await?;
                interaction::wait_until_actionable(
                    &session.page,
                    &target,
                    interaction::Actionability::Attached,
                )
                .await?;
                evaluate_value_in_context(
                    &session.page,
                    element_script(
                        &target.query,
                        &format!(
                            "element.scrollBy({{ left: {x}, top: {y}, behavior: \"instant\" }}); return true;"
                        ),
                    )?,
                    target.context_id,
                    true,
                )
                .await?;
            } else {
                evaluate_value(
                    &session.page,
                    format!("scrollBy({{ left: {x}, top: {y}, behavior: \"instant\" }}); true"),
                )
                .await?;
            }
            Ok(action_result(sequence, BrowserActionName::Scroll))
        }
        BrowserAction::SelectOption { target, values } => {
            let target = session.target(&target).await?;
            interaction::wait_until_actionable(
                &session.page,
                &target,
                interaction::Actionability::Enabled,
            )
            .await?;
            let values = serde_json::to_string(&values)?;
            evaluate_value_in_context(
                &session.page,
                element_script(
                    &target.query,
                    &format!(
                        r#"if (!(element instanceof HTMLSelectElement)) throw new Error("selector is not a select element");
const requested = new Set({values});
for (const option of element.options) option.selected = requested.has(option.value);
element.dispatchEvent(new Event("input", {{ bubbles: true }}));
element.dispatchEvent(new Event("change", {{ bubbles: true }}));
return Array.from(element.selectedOptions, option => option.value);"#
                    ),
                )?,
                target.context_id,
                true,
            )
            .await?;
            Ok(action_result(sequence, BrowserActionName::SelectOption))
        }
        BrowserAction::SetChecked { target, checked } => {
            let target = session.target(&target).await?;
            let current: bool = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    r#"if (!(element instanceof HTMLInputElement) || !["checkbox", "radio"].includes(element.type)) {
  throw new Error("selector is not a checkbox or radio input");
}
return element.checked;"#
                )?,
                target.context_id,
            )
            .await?;
            if current != checked {
                let point = interaction::actionable_point(
                    &session.page,
                    &target,
                    interaction::Actionability::Pointer,
                )
                .await?;
                interaction::dispatch_click(&session.page, point, BrowserClickOptions::default())
                    .await?;
            }
            Ok(action_result(sequence, BrowserActionName::SetChecked))
        }
        BrowserAction::Drag {
            source,
            destination,
        } => {
            let source = session.target(&source).await?;
            let target = session.target(&destination).await?;
            let target_point = interaction::actionable_point(
                &session.page,
                &target,
                interaction::Actionability::Pointer,
            )
            .await?;
            let source_point = interaction::actionable_point(
                &session.page,
                &source,
                interaction::Actionability::Pointer,
            )
            .await?;
            interaction::dispatch_drag(&session.page, source_point, target_point).await?;
            Ok(action_result(sequence, BrowserActionName::Drag))
        }
        BrowserAction::UploadFiles { target, paths } => {
            let target = session.target(&target).await?;
            interaction::wait_until_actionable(
                &session.page,
                &target,
                interaction::Actionability::Attached,
            )
            .await?;
            let paths = session.upload_paths(paths).await?;
            set_file_input_files(&session.page, &target, paths).await?;
            Ok(action_result(sequence, BrowserActionName::UploadFiles))
        }
        BrowserAction::SetViewport {
            width,
            height,
            device_scale_factor,
        } => {
            if width == 0
                || height == 0
                || width > MAX_VIEWPORT_DIMENSION
                || height > MAX_VIEWPORT_DIMENSION
            {
                return Err(BrowserError::InvalidViewport {
                    width,
                    height,
                    maximum: MAX_VIEWPORT_DIMENSION,
                });
            }
            let device_scale_factor = device_scale_factor.unwrap_or(1.0);
            if !device_scale_factor.is_finite() || device_scale_factor <= 0.0 {
                return Err(BrowserError::InvalidDeviceScaleFactor {
                    device_scale_factor,
                });
            }
            session
                .page
                .execute(SetDeviceMetricsOverrideParams::new(
                    i64::from(width),
                    i64::from(height),
                    device_scale_factor,
                    false,
                ))
                .await?;
            Ok(action_result(sequence, BrowserActionName::SetViewport))
        }
        BrowserAction::GoBack => {
            navigate_history(&session.page, -1).await?;
            session.refs.clear();
            Ok(action_result(sequence, BrowserActionName::GoBack))
        }
        BrowserAction::GoForward => {
            navigate_history(&session.page, 1).await?;
            session.refs.clear();
            Ok(action_result(sequence, BrowserActionName::GoForward))
        }
        BrowserAction::WaitForSelector { target, state } => {
            let target = session.target_for_wait(&target).await?;
            interaction::wait_for_selector(&session.page, &target, state.unwrap_or_default())
                .await?;
            Ok(action_result(sequence, BrowserActionName::WaitForSelector))
        }
        BrowserAction::WaitForText {
            text,
            target,
            hidden,
        } => {
            let target = match target.as_ref() {
                Some(target) => Some(session.target_for_wait(target).await?),
                None => None,
            };
            interaction::wait_for_text(&session.page, target.as_ref(), &text, hidden).await?;
            Ok(action_result(sequence, BrowserActionName::WaitForText))
        }
        BrowserAction::WaitForUrl { url_contains } => {
            interaction::wait_for_url(&session.page, &url_contains).await?;
            Ok(action_result(sequence, BrowserActionName::WaitForUrl))
        }
        BrowserAction::WaitForLoadState { state } => {
            interaction::wait_for_load_state(&session.page, state).await?;
            Ok(action_result(sequence, BrowserActionName::WaitForLoadState))
        }
        BrowserAction::WaitForFunction { expression } => {
            interaction::wait_for_function(&session.page, &expression).await?;
            Ok(action_result(sequence, BrowserActionName::WaitForFunction))
        }
        BrowserAction::WaitForTimeout { milliseconds } => {
            let duration = Duration::from_millis(milliseconds);
            if duration > MAX_EXPLICIT_WAIT {
                return Err(BrowserError::WaitTooLong {
                    milliseconds,
                    maximum: MAX_EXPLICIT_WAIT.as_millis(),
                });
            }
            tokio::time::sleep(duration).await;
            Ok(action_result(sequence, BrowserActionName::WaitForTimeout))
        }
        BrowserAction::Screenshot {
            full_page,
            annotate,
            target,
        } => {
            if session.visual_artifacts.len() >= MAX_VISUAL_ARTIFACTS {
                return Err(BrowserError::VisualArtifactLimit {
                    maximum: MAX_VISUAL_ARTIFACTS,
                });
            }
            if annotate {
                install_annotations(session).await?;
            }
            let clip = capture_clip(session, target.as_ref(), full_page).await;
            if clip.is_err() && annotate {
                let _ = evaluate_value(&session.page, REMOVE_ANNOTATIONS_SCRIPT).await;
            }
            let clip = clip?;
            let screenshot = artifacts::capture(
                &session.page,
                &session.output_dir,
                format!("screenshot-{sequence}"),
                full_page,
                clip,
            )
            .await;
            if annotate {
                let _ = evaluate_value(&session.page, REMOVE_ANNOTATIONS_SCRIPT).await;
            }
            let image = screenshot?;
            let path = image.path.clone();
            session
                .visual_artifacts
                .insert(image.artifact_id.clone(), image.clone());
            Ok(BrowserActionResult::Screenshot {
                sequence,
                executed: true,
                path,
                image: Some(image),
            })
        }
        BrowserAction::Pdf {
            landscape,
            print_background,
            prefer_css_page_size,
            tagged,
            document_outline,
        } => {
            let pdf = audits::pdf(
                &session.page,
                &session.output_dir,
                sequence,
                audits::PdfOptions {
                    landscape,
                    print_background,
                    prefer_css_page_size,
                    tagged,
                    document_outline,
                },
            )
            .await?;
            Ok(BrowserActionResult::Pdf {
                sequence,
                executed: true,
                pdf,
            })
        }
        BrowserAction::VisualBaseline { full_page, target } => {
            if session.visual_artifacts.len() >= MAX_VISUAL_ARTIFACTS {
                return Err(BrowserError::VisualArtifactLimit {
                    maximum: MAX_VISUAL_ARTIFACTS,
                });
            }
            let clip = capture_clip(session, target.as_ref(), full_page).await?;
            let image = artifacts::capture(
                &session.page,
                &session.output_dir,
                format!("visual-{sequence}"),
                full_page,
                clip,
            )
            .await?;
            session
                .visual_artifacts
                .insert(image.artifact_id.clone(), image.clone());
            Ok(BrowserActionResult::VisualBaseline {
                sequence,
                executed: true,
                image,
            })
        }
        BrowserAction::VisualDiff {
            baseline_id,
            threshold,
            full_page,
            target,
        } => {
            let baseline = session
                .visual_artifacts
                .get(&baseline_id)
                .cloned()
                .ok_or_else(|| BrowserError::VisualArtifactUnavailable {
                    artifact_id: baseline_id.clone(),
                })?;
            if session.visual_artifacts.len() >= MAX_VISUAL_ARTIFACTS {
                return Err(BrowserError::VisualArtifactLimit {
                    maximum: MAX_VISUAL_ARTIFACTS,
                });
            }
            let clip = capture_clip(session, target.as_ref(), full_page).await?;
            let current = artifacts::capture(
                &session.page,
                &session.output_dir,
                format!("visual-{sequence}"),
                full_page,
                clip,
            )
            .await?;
            let diff = artifacts::compare(&baseline, &current, &session.output_dir, threshold)?;
            session
                .visual_artifacts
                .insert(current.artifact_id.clone(), current);
            Ok(BrowserActionResult::VisualDiff {
                sequence,
                executed: true,
                diff,
            })
        }
        BrowserAction::VisualTraceStart {
            frames_per_second,
            max_frames,
        } => {
            if session.visual_trace.is_some() {
                return Err(BrowserError::VisualTraceActive);
            }
            let frames_per_second = frames_per_second.unwrap_or(10).clamp(1, 30);
            let max_frames = max_frames.unwrap_or(120).clamp(1, 600);
            session.visual_trace = Some(artifacts::start_trace(
                session.page.clone(),
                session.output_dir.clone(),
                sequence,
                frames_per_second,
                max_frames,
            ));
            Ok(action_result(sequence, BrowserActionName::VisualTraceStart))
        }
        BrowserAction::VisualTraceStop => {
            let trace = session
                .visual_trace
                .take()
                .ok_or(BrowserError::VisualTraceNotActive)?;
            let layout_shift = web_diagnostics::cumulative_layout_shift(&session.page).await?;
            let trace = artifacts::stop_trace(trace, layout_shift).await?;
            Ok(BrowserActionResult::VisualTrace {
                sequence,
                executed: true,
                trace,
            })
        }
        BrowserAction::SessionTraceStart {
            screenshots,
            dom_snapshots,
            max_actions,
        } => {
            if session.action_trace.is_some() {
                return Err(BrowserError::SessionTraceActive);
            }
            session.action_trace = Some(
                session_trace::SessionTraceState::start(
                    &session.output_dir,
                    sequence,
                    screenshots,
                    dom_snapshots,
                    usize::from(max_actions.unwrap_or(500).clamp(1, 2_000)),
                )
                .await?,
            );
            Ok(action_result(
                sequence,
                BrowserActionName::SessionTraceStart,
            ))
        }
        BrowserAction::SessionTraceStop => {
            let trace = session
                .action_trace
                .take()
                .ok_or(BrowserError::SessionTraceNotActive)?;
            let (requests, console, errors, dropped_console, dropped_errors, dropped_requests) = {
                let diagnostics = session
                    .diagnostics
                    .lock()
                    .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
                (
                    diagnostics
                        .requests
                        .iter()
                        .map(|entry| entry.request.clone())
                        .collect(),
                    diagnostics.console.iter().cloned().collect(),
                    diagnostics.errors.iter().cloned().collect(),
                    diagnostics.dropped_console,
                    diagnostics.dropped_errors,
                    diagnostics.dropped_requests,
                )
            };
            let trace = trace
                .stop(
                    requests,
                    console,
                    errors,
                    dropped_console,
                    dropped_errors,
                    dropped_requests,
                )
                .await?;
            Ok(BrowserActionResult::SessionTrace {
                sequence,
                executed: true,
                trace,
            })
        }
        BrowserAction::GetText { target } => {
            let target = session.target(&target).await?;
            let text: String = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    r#"return element.innerText ?? element.textContent ?? "";"#,
                )?,
                target.context_id,
            )
            .await?;
            ensure_action_value_size(text.len())?;
            Ok(BrowserActionResult::Text {
                sequence,
                executed: true,
                text,
            })
        }
        BrowserAction::GetHtml { target } => {
            let target = session.target(&target).await?;
            let html: String = evaluate_typed_in_context(
                &session.page,
                element_script(&target.query, "return element.innerHTML;")?,
                target.context_id,
            )
            .await?;
            ensure_action_value_size(html.len())?;
            Ok(BrowserActionResult::Html {
                sequence,
                executed: true,
                html,
            })
        }
        BrowserAction::GetValue { target } => {
            let target = session.target(&target).await?;
            let value: Option<String> = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    r#"return typeof element.value === "string" ? element.value : null;"#,
                )?,
                target.context_id,
            )
            .await?;
            if let Some(value) = &value {
                ensure_action_value_size(value.len())?;
            }
            Ok(BrowserActionResult::Value {
                sequence,
                executed: true,
                value,
            })
        }
        BrowserAction::GetAttribute { target, name } => {
            let target = session.target(&target).await?;
            let value: Option<String> = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    &format!(
                        "return element.getAttribute({});",
                        serde_json::to_string(&name)?
                    ),
                )?,
                target.context_id,
            )
            .await?;
            if let Some(value) = &value {
                ensure_action_value_size(value.len())?;
            }
            Ok(BrowserActionResult::Attribute {
                sequence,
                executed: true,
                value,
            })
        }
        BrowserAction::GetTitle => {
            let title: String = evaluate_typed(&session.page, "document.title").await?;
            ensure_action_value_size(title.len())?;
            Ok(BrowserActionResult::Title {
                sequence,
                executed: true,
                title,
            })
        }
        BrowserAction::GetUrl => {
            let url: String = evaluate_typed(&session.page, "location.href").await?;
            Ok(BrowserActionResult::Url {
                sequence,
                executed: true,
                url,
            })
        }
        BrowserAction::GetCount { target } => {
            let count = session.count_target(&target).await?;
            Ok(BrowserActionResult::Count {
                sequence,
                executed: true,
                count,
            })
        }
        BrowserAction::GetBox { target } => {
            let target = session.target(&target).await?;
            let bounds = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    r"const bounds = element.getBoundingClientRect();
return {
  x: bounds.x,
  y: bounds.y,
  width: bounds.width,
  height: bounds.height
};",
                )?,
                target.context_id,
            )
            .await?;
            Ok(BrowserActionResult::Box {
                sequence,
                executed: true,
                bounds: Some(bounds),
            })
        }
        BrowserAction::GetStyles { target } => {
            let target = session.target(&target).await?;
            let styles = evaluate_typed_in_context(
                &session.page,
                element_script(
                    &target.query,
                    r"const style = getComputedStyle(element);
return {
  display: style.display,
  position: style.position,
  color: style.color,
  backgroundColor: style.backgroundColor,
  fontFamily: style.fontFamily,
  fontSize: style.fontSize,
  fontWeight: style.fontWeight,
  visibility: style.visibility,
  opacity: style.opacity,
  zIndex: style.zIndex,
  overflow: style.overflow,
  overflowX: style.overflowX,
  overflowY: style.overflowY,
  boxSizing: style.boxSizing,
  width: style.width,
  height: style.height,
  margin: style.margin,
  padding: style.padding,
  border: style.border,
  transform: style.transform,
  transition: style.transition,
  animation: style.animation,
  pointerEvents: style.pointerEvents,
  flexDirection: style.flexDirection,
  alignItems: style.alignItems,
  justifyContent: style.justifyContent,
  gridTemplateColumns: style.gridTemplateColumns,
  gap: style.gap
};",
                )?,
                target.context_id,
            )
            .await?;
            Ok(BrowserActionResult::Styles {
                sequence,
                executed: true,
                styles: Box::new(styles),
            })
        }
        BrowserAction::MatchedStyles { target } => {
            let target = session.target(&target).await?;
            let styles =
                devtools::matched_styles(&session.page, &target, &session.devtools).await?;
            Ok(BrowserActionResult::MatchedStyles {
                sequence,
                executed: true,
                styles,
            })
        }
        BrowserAction::ForcePseudoState {
            target,
            pseudo_classes,
        } => {
            let target = session.target(&target).await?;
            devtools::force_pseudo_state(&session.page, &target, &session.devtools, pseudo_classes)
                .await?;
            Ok(action_result(sequence, BrowserActionName::ForcePseudoState))
        }
        BrowserAction::EventListeners {
            target,
            depth,
            pierce,
        } => {
            let target = session.target(&target).await?;
            let listeners = devtools::event_listeners(
                &session.page,
                &target,
                depth.unwrap_or(1).clamp(1, 32),
                pierce,
            )
            .await?;
            Ok(BrowserActionResult::EventListeners {
                sequence,
                executed: true,
                listeners,
            })
        }
        BrowserAction::DebuggerSetPauseOnExceptions { state } => {
            devtools::set_pause_on_exceptions(&session.page, state).await?;
            Ok(action_result(
                sequence,
                BrowserActionName::DebuggerSetPauseOnExceptions,
            ))
        }
        BrowserAction::DebuggerSetBreakpoint {
            url,
            line_number,
            column_number,
            condition,
        } => {
            let breakpoint = devtools::set_breakpoint(
                &session.page,
                &session.source_maps,
                url,
                line_number,
                column_number.unwrap_or(1),
                condition,
            )
            .await?;
            Ok(BrowserActionResult::Breakpoint {
                sequence,
                executed: true,
                breakpoint,
            })
        }
        BrowserAction::DebuggerRemoveBreakpoint { breakpoint_id } => {
            devtools::remove_breakpoint(&session.page, breakpoint_id).await?;
            Ok(action_result(
                sequence,
                BrowserActionName::DebuggerRemoveBreakpoint,
            ))
        }
        BrowserAction::DebuggerPaused => Ok(BrowserActionResult::DebuggerPause {
            sequence,
            executed: true,
            pause: session.devtools.latest_pause()?,
        }),
        BrowserAction::DebuggerResume => {
            devtools::resume(&session.page).await?;
            session.devtools.clear_pause()?;
            Ok(action_result(sequence, BrowserActionName::DebuggerResume))
        }
        BrowserAction::DebuggerStepOver => {
            devtools::step_over(&session.page).await?;
            session.devtools.clear_pause()?;
            Ok(action_result(sequence, BrowserActionName::DebuggerStepOver))
        }
        BrowserAction::DebuggerStepInto => {
            devtools::step_into(&session.page).await?;
            session.devtools.clear_pause()?;
            Ok(action_result(sequence, BrowserActionName::DebuggerStepInto))
        }
        BrowserAction::DebuggerStepOut => {
            devtools::step_out(&session.page).await?;
            session.devtools.clear_pause()?;
            Ok(action_result(sequence, BrowserActionName::DebuggerStepOut))
        }
        BrowserAction::StorageInspect => {
            let storage = devtools::storage_report(&session.page).await?;
            Ok(BrowserActionResult::Storage {
                sequence,
                executed: true,
                storage,
            })
        }
        BrowserAction::Console { limit } => {
            let diagnostics = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            let total = diagnostics.console.len();
            let mut entries = diagnostics
                .console
                .iter()
                .skip(total.saturating_sub(diagnostic_limit(limit)))
                .cloned()
                .collect::<Vec<_>>();
            let dropped = diagnostics.dropped_console;
            drop(diagnostics);
            session.source_maps.symbolicate_console(&mut entries)?;
            Ok(BrowserActionResult::Console {
                sequence,
                executed: true,
                entries,
                total,
                dropped,
            })
        }
        BrowserAction::Errors { limit } => {
            let diagnostics = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            let total = diagnostics.errors.len();
            let mut errors = diagnostics
                .errors
                .iter()
                .skip(total.saturating_sub(diagnostic_limit(limit)))
                .cloned()
                .collect::<Vec<_>>();
            let dropped = diagnostics.dropped_errors;
            drop(diagnostics);
            session.source_maps.symbolicate_errors(&mut errors)?;
            Ok(BrowserActionResult::Errors {
                sequence,
                executed: true,
                errors,
                total,
                dropped,
            })
        }
        BrowserAction::NetworkRequests {
            filter,
            after,
            limit,
        } => {
            let diagnostics = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            let matches = |entry: &&NetworkEntry| {
                filter
                    .as_ref()
                    .is_none_or(|filter| entry.request.url.contains(filter))
            };
            let total = diagnostics
                .requests
                .iter()
                .filter(|entry| matches(entry))
                .count();
            let limit = diagnostic_limit(limit);
            let matching = diagnostics
                .requests
                .iter()
                .filter(|entry| after.is_none_or(|after| entry.sequence > after))
                .filter(|entry| matches(entry))
                .collect::<Vec<_>>();
            let has_more = matching.len() > limit;
            let requests = matching
                .into_iter()
                .take(limit)
                .map(|entry| entry.request.clone())
                .collect::<Vec<_>>();
            let last_sequence = requests.last().map(|request| request.sequence);
            Ok(BrowserActionResult::NetworkRequests {
                sequence,
                executed: true,
                requests,
                total,
                dropped: diagnostics.dropped_requests,
                last_sequence,
                has_more,
            })
        }
        BrowserAction::NetworkBody { request_id, kind } => {
            let (body, base64_encoded) = session.network_body(&request_id, kind).await?;
            Ok(BrowserActionResult::NetworkBody {
                sequence,
                executed: true,
                request_id,
                kind,
                body,
                base64_encoded,
            })
        }
        BrowserAction::WebSocketMessages {
            request_id,
            after,
            limit,
        } => {
            let diagnostics = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            let matches = |message: &&BrowserWebSocketMessage| {
                request_id
                    .as_ref()
                    .is_none_or(|request_id| message.request_id == *request_id)
            };
            let total = diagnostics
                .web_socket_messages
                .iter()
                .filter(|message| matches(message))
                .count();
            let limit = diagnostic_limit(limit);
            let matching = diagnostics
                .web_socket_messages
                .iter()
                .filter(|message| after.is_none_or(|after| message.sequence > after))
                .filter(|message| matches(message))
                .collect::<Vec<_>>();
            let has_more = matching.len() > limit;
            let messages = matching
                .into_iter()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let last_sequence = messages.last().map(|message| message.sequence);
            Ok(BrowserActionResult::WebSocketMessages {
                sequence,
                executed: true,
                messages,
                total,
                dropped: diagnostics.dropped_web_socket_messages,
                last_sequence,
                has_more,
            })
        }
        BrowserAction::ReactEvents { after, limit } => {
            if !session.react_diagnostics_enabled {
                return Ok(BrowserActionResult::ReactEvents {
                    sequence,
                    executed: true,
                    status: BrowserReactStatus {
                        enabled: false,
                        active: false,
                        renderer_count: 0,
                        renderers: Vec::new(),
                        document_url: evaluate_typed(&session.page, "location.href").await?,
                        time_origin_ms: None,
                    },
                    events: Vec::new(),
                    total: 0,
                    dropped: 0,
                    last_sequence: None,
                    has_more: false,
                });
            }
            let request = ReactEventsRequest {
                after: after.unwrap_or_default(),
                limit: diagnostic_limit(limit),
            };
            let expression = format!(
                "globalThis.__nanocodexReactDiagnostics?.read({}) ?? null",
                serde_json::to_string(&request)?
            );
            let wire: Option<ReactEventsWire> = evaluate_typed(&session.page, expression).await?;
            let wire = wire.ok_or(BrowserError::ReactDiagnosticsUnavailable)?;
            if wire.protocol_version != REACT_DIAGNOSTICS_PROTOCOL_VERSION {
                return Err(BrowserError::ReactDiagnosticsProtocol {
                    expected: REACT_DIAGNOSTICS_PROTOCOL_VERSION,
                    actual: wire.protocol_version,
                });
            }
            Ok(BrowserActionResult::ReactEvents {
                sequence,
                executed: true,
                status: wire.status,
                events: wire.events,
                total: wire.total,
                dropped: wire.dropped,
                last_sequence: wire.last_sequence,
                has_more: wire.has_more,
            })
        }
        BrowserAction::ElementContext { target } => {
            if !session.react_diagnostics_enabled {
                return Err(BrowserError::ReactDiagnosticsUnavailable);
            }
            let target = session.target(&target).await?;
            let expression = element_script(
                &target.query,
                "return globalThis.__nanocodexReactDiagnostics?.elementContext(element) ?? null;",
            )?;
            let wire: Option<ElementContextWire> =
                evaluate_typed_in_context(&session.page, expression, target.context_id).await?;
            let wire = wire.ok_or(BrowserError::ReactDiagnosticsUnavailable)?;
            if wire.protocol_version != REACT_DIAGNOSTICS_PROTOCOL_VERSION {
                return Err(BrowserError::ReactDiagnosticsProtocol {
                    expected: REACT_DIAGNOSTICS_PROTOCOL_VERSION,
                    actual: wire.protocol_version,
                });
            }
            Ok(BrowserActionResult::ElementContext {
                sequence,
                executed: true,
                context: wire.context,
            })
        }
        BrowserAction::WebVitals => {
            let vitals = web_diagnostics::web_vitals(&session.page).await?;
            Ok(BrowserActionResult::WebVitals {
                sequence,
                executed: true,
                vitals,
            })
        }
        BrowserAction::PerformanceTraceStart => {
            if session.performance_trace.is_some() {
                return Err(BrowserError::PerformanceTraceActive);
            }
            let request_after = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .next_request_sequence;
            session.performance_trace =
                Some(performance_trace::start(&session.page, request_after).await?);
            Ok(action_result(
                sequence,
                BrowserActionName::PerformanceTraceStart,
            ))
        }
        BrowserAction::PerformanceTraceStop => {
            let trace = session
                .performance_trace
                .take()
                .ok_or(BrowserError::PerformanceTraceNotActive)?;
            let requests = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .requests
                .iter()
                .map(|entry| entry.request.clone())
                .collect::<Vec<_>>();
            let trace = performance_trace::stop(
                &session.page,
                &session.output_dir,
                sequence,
                trace,
                &requests,
                &session.source_maps,
            )
            .await?;
            Ok(BrowserActionResult::PerformanceTrace {
                sequence,
                executed: true,
                trace,
            })
        }
        BrowserAction::CpuProfileStart => {
            if session.cpu_profile_active {
                return Err(BrowserError::CpuProfileActive);
            }
            profiling::start_cpu(&session.page).await?;
            session.cpu_profile_active = true;
            Ok(action_result(sequence, BrowserActionName::CpuProfileStart))
        }
        BrowserAction::CpuProfileStop => {
            if !session.cpu_profile_active {
                return Err(BrowserError::CpuProfileNotActive);
            }
            let profile = profiling::stop_cpu(&session.page, &session.output_dir, sequence).await?;
            session.cpu_profile_active = false;
            Ok(BrowserActionResult::CpuProfile {
                sequence,
                executed: true,
                profile,
            })
        }
        BrowserAction::CoverageStart => {
            if session.coverage_active {
                return Err(BrowserError::CoverageActive);
            }
            profiling::start_coverage(&session.page).await?;
            session.coverage_active = true;
            Ok(action_result(sequence, BrowserActionName::CoverageStart))
        }
        BrowserAction::CoverageStop => {
            if !session.coverage_active {
                return Err(BrowserError::CoverageNotActive);
            }
            let coverage =
                profiling::stop_coverage(&session.page, &session.output_dir, sequence).await?;
            session.coverage_active = false;
            Ok(BrowserActionResult::Coverage {
                sequence,
                executed: true,
                coverage,
            })
        }
        BrowserAction::HeapSnapshot { collect_garbage } => {
            if session.heap_snapshots.len() >= MAX_HEAP_SNAPSHOTS {
                return Err(BrowserError::HeapSnapshotLimit {
                    maximum: MAX_HEAP_SNAPSHOTS,
                });
            }
            let (snapshot, analysis) = profiling::capture_heap(
                &session.page,
                &session.output_dir,
                sequence,
                collect_garbage,
            )
            .await?;
            session
                .heap_snapshots
                .insert(snapshot.artifact_id.clone(), analysis);
            Ok(BrowserActionResult::HeapSnapshot {
                sequence,
                executed: true,
                snapshot,
            })
        }
        BrowserAction::HeapCompare {
            before_id,
            after_id,
        } => {
            let before = session.heap_snapshots.get(&before_id).ok_or_else(|| {
                BrowserError::HeapSnapshotUnavailable {
                    artifact_id: before_id.clone(),
                }
            })?;
            let after = session.heap_snapshots.get(&after_id).ok_or_else(|| {
                BrowserError::HeapSnapshotUnavailable {
                    artifact_id: after_id.clone(),
                }
            })?;
            let comparison = profiling::compare_heaps(&before_id, before, &after_id, after);
            Ok(BrowserActionResult::HeapComparison {
                sequence,
                executed: true,
                comparison,
            })
        }
        BrowserAction::HeapRetainers {
            artifact_id,
            node_id,
            max_depth,
            max_nodes,
        } => {
            let analysis = session.heap_snapshots.get(&artifact_id).ok_or_else(|| {
                BrowserError::HeapSnapshotUnavailable {
                    artifact_id: artifact_id.clone(),
                }
            })?;
            let retainers = profiling::heap_retainers(
                artifact_id,
                analysis.path().to_path_buf(),
                node_id,
                max_depth.unwrap_or(5).clamp(1, 16),
                usize::from(max_nodes.unwrap_or(200).clamp(1, 1_000)),
            )
            .await?;
            Ok(BrowserActionResult::HeapRetainers {
                sequence,
                executed: true,
                retainers,
            })
        }
        BrowserAction::HeapInspect {
            artifact_id,
            class_name,
            minimum_retained_size,
            max_nodes,
            include_duplicate_strings,
        } => {
            let analysis = session.heap_snapshots.get(&artifact_id).ok_or_else(|| {
                BrowserError::HeapSnapshotUnavailable {
                    artifact_id: artifact_id.clone(),
                }
            })?;
            let inspection = profiling::inspect_heap(
                artifact_id,
                analysis.path().to_path_buf(),
                class_name,
                minimum_retained_size.unwrap_or_default(),
                usize::from(max_nodes.unwrap_or(100).clamp(1, 1_000)),
                include_duplicate_strings,
            )
            .await?;
            Ok(BrowserActionResult::HeapInspection {
                sequence,
                executed: true,
                inspection,
            })
        }
        BrowserAction::VideoStart {
            frames_per_second,
            quality,
        } => {
            if session.video.is_some() {
                return Err(BrowserError::VideoActive);
            }
            let frames_per_second = frames_per_second.unwrap_or(25).clamp(1, 30);
            let quality = quality.unwrap_or(80).clamp(1, 95);
            session.video = Some(
                video::start(
                    &session.page,
                    &session.output_dir,
                    sequence,
                    frames_per_second,
                    quality,
                    session.ffmpeg_executable.as_deref(),
                )
                .await?,
            );
            Ok(action_result(sequence, BrowserActionName::VideoStart))
        }
        BrowserAction::VideoStop => {
            let state = session.video.take().ok_or(BrowserError::VideoNotActive)?;
            let video = video::stop(&session.page, state).await?;
            Ok(BrowserActionResult::Video {
                sequence,
                executed: true,
                video,
            })
        }
        BrowserAction::ListFrames => Ok(BrowserActionResult::Frames {
            sequence,
            executed: true,
            frames: session.frames().await?,
        }),
        BrowserAction::EvaluateFrame {
            frame_id,
            expression,
        } => {
            let context_id = session.frame_context(&frame_id).await?;
            let value = timeout(
                MAX_SCRIPT_EVALUATION,
                evaluate_value_in_context(&session.page, expression, Some(context_id), false),
            )
            .await
            .map_err(|_| BrowserError::EvaluationTimeout {
                maximum: MAX_SCRIPT_EVALUATION,
            })??;
            Ok(BrowserActionResult::FrameEvaluation {
                sequence,
                executed: true,
                frame_id,
                value,
            })
        }
        BrowserAction::NewTab { url } => {
            session.ensure_capture_inactive()?;
            if let Some(raw_url) = &url {
                validate_url(raw_url)?;
                session.validate_navigation(&Url::parse(raw_url)?)?;
            }
            let page = session.browser.new_page("about:blank").await?;
            session.activate_page(page).await?;
            if let Some(url) = url {
                navigate(&session.page, &url).await?;
                session.sync_virtual_authenticators().await?;
            }
            Ok(action_result(sequence, BrowserActionName::NewTab))
        }
        BrowserAction::ListTabs => Ok(BrowserActionResult::Tabs {
            sequence,
            executed: true,
            tabs: session.tabs().await?,
        }),
        BrowserAction::SelectTab { tab_id } => {
            session.select_tab(&tab_id).await?;
            Ok(action_result(sequence, BrowserActionName::SelectTab))
        }
        BrowserAction::CloseTab { tab_id } => {
            session.close_tab(&tab_id).await?;
            Ok(action_result(sequence, BrowserActionName::CloseTab))
        }
        BrowserAction::Dialog => {
            let dialog = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .dialog
                .clone();
            Ok(BrowserActionResult::Dialog {
                sequence,
                executed: true,
                dialog,
            })
        }
        BrowserAction::HandleDialog {
            accept,
            prompt_text,
        } => {
            if session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .dialog
                .is_none()
            {
                return Err(BrowserError::DialogNotPending);
            }
            let mut params = HandleJavaScriptDialogParams::new(accept);
            params.prompt_text = prompt_text;
            session.page.execute(params).await?;
            session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .dialog = None;
            Ok(action_result(sequence, BrowserActionName::HandleDialog))
        }
        BrowserAction::NetworkRoute {
            route_id,
            url_contains,
            response,
        } => {
            session
                .network_controls
                .route(route_id, url_contains, response)?;
            Ok(action_result(sequence, BrowserActionName::NetworkRoute))
        }
        BrowserAction::RemoveNetworkRoute { route_id } => {
            session.network_controls.remove_route(&route_id)?;
            Ok(action_result(
                sequence,
                BrowserActionName::RemoveNetworkRoute,
            ))
        }
        BrowserAction::ClearNetworkRoutes => {
            session.network_controls.clear_routes()?;
            Ok(action_result(
                sequence,
                BrowserActionName::ClearNetworkRoutes,
            ))
        }
        BrowserAction::SetOffline { offline } => {
            session
                .page
                .execute(EmulateNetworkConditionsByRuleParams::new(
                    offline,
                    vec![NetworkConditions::new("", 0.0, -1.0, -1.0)],
                ))
                .await?;
            session
                .page
                .execute(OverrideNetworkStateParams::new(offline, 0.0, -1.0, -1.0))
                .await?;
            Ok(action_result(sequence, BrowserActionName::SetOffline))
        }
        BrowserAction::ExportHar { include_bodies } => {
            let requests = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .requests
                .iter()
                .map(|entry| entry.request.clone())
                .collect::<Vec<_>>();
            let mut exchanges = Vec::with_capacity(requests.len());
            for request in requests {
                let request_body = if include_bodies && request.has_post_data {
                    session
                        .network_body(&request.request_id, BrowserNetworkBodyKind::Request)
                        .await
                        .ok()
                        .map(|(body, base64_encoded)| har::HarBody {
                            body,
                            base64_encoded,
                        })
                } else {
                    None
                };
                let response_body = if include_bodies
                    && request.body_available
                    && request.completed
                    && request.status.is_some()
                {
                    session
                        .network_body(&request.request_id, BrowserNetworkBodyKind::Response)
                        .await
                        .ok()
                        .map(|(body, base64_encoded)| har::HarBody {
                            body,
                            base64_encoded,
                        })
                } else {
                    None
                };
                exchanges.push(har::HarExchange {
                    request,
                    request_body,
                    response_body,
                });
            }
            let har = har::write(&session.output_dir, sequence, exchanges).await?;
            Ok(BrowserActionResult::Har {
                sequence,
                executed: true,
                har,
            })
        }
        BrowserAction::AccessibilityAudit => {
            let mut audit =
                web_diagnostics::accessibility_audit(&session.page, None, None, None).await?;
            for frame in session.snapshot_child_frames().await? {
                let result = web_diagnostics::accessibility_audit(
                    &session.page,
                    frame.context_id,
                    frame.frame_id,
                    frame.url,
                )
                .await;
                match result {
                    Ok(child) => {
                        audit.checked_elements = audit
                            .checked_elements
                            .saturating_add(child.checked_elements);
                        audit.violations.extend(child.violations);
                    }
                    Err(error) => {
                        warn!(
                            target: "nanocodex_browser",
                            %error,
                            "skipping a child frame that navigated during accessibility audit"
                        );
                    }
                }
            }
            Ok(BrowserActionResult::Accessibility {
                sequence,
                executed: true,
                audit,
            })
        }
        BrowserAction::AxeAudit => {
            if !session.axe_enabled {
                audits::install_axe(&session.page).await?;
                session.axe_enabled = true;
            }
            let mut audit = audits::axe(&session.page, None, None, None).await?;
            for frame in session.snapshot_child_frames().await? {
                let result =
                    audits::axe(&session.page, frame.context_id, frame.frame_id, frame.url).await;
                match result {
                    Ok(child) => {
                        audit.pass_count = audit.pass_count.saturating_add(child.pass_count);
                        audit.inapplicable_count = audit
                            .inapplicable_count
                            .saturating_add(child.inapplicable_count);
                        audit.truncated |= child.truncated;
                        audit.violations.extend(child.violations);
                        audit.incomplete.extend(child.incomplete);
                    }
                    Err(error) => {
                        warn!(
                            target: "nanocodex_browser",
                            %error,
                            "skipping a child frame that navigated during axe audit"
                        );
                    }
                }
            }
            Ok(BrowserActionResult::Axe {
                sequence,
                executed: true,
                audit,
            })
        }
        BrowserAction::LighthouseAudit {
            categories,
            form_factor,
        } => {
            let executable = session
                .lighthouse_executable
                .as_deref()
                .ok_or(BrowserError::LighthouseNotConfigured)?;
            let url = session.page.url().await?.unwrap_or_default();
            let report = audits::lighthouse(
                executable,
                &url,
                session.browser.websocket_address(),
                &session.output_dir,
                sequence,
                categories,
                form_factor,
            )
            .await?;
            Ok(BrowserActionResult::Lighthouse {
                sequence,
                executed: true,
                report,
            })
        }
        BrowserAction::Crux { scope, form_factor } => {
            let client = session
                .crux_client
                .as_ref()
                .ok_or(BrowserError::CruxNotConfigured)?;
            let url = session.page.url().await?.unwrap_or_default();
            let report = audits::crux(client, &url, scope, form_factor).await?;
            Ok(BrowserActionResult::Crux {
                sequence,
                executed: true,
                report,
            })
        }
        BrowserAction::Downloads => {
            let downloads = session
                .diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?
                .downloads
                .values()
                .cloned()
                .collect();
            Ok(BrowserActionResult::Downloads {
                sequence,
                executed: true,
                downloads,
            })
        }
        BrowserAction::Evaluate { expression } => {
            let value = timeout(
                MAX_SCRIPT_EVALUATION,
                evaluate_value(&session.page, expression),
            )
            .await
            .map_err(|_| BrowserError::EvaluationTimeout {
                maximum: MAX_SCRIPT_EVALUATION,
            })??;
            let bytes = serialized_size(&value)?;
            if bytes > MAX_ACTION_VALUE_BYTES {
                return Err(BrowserError::ActionValueTooLarge {
                    bytes,
                    maximum: MAX_ACTION_VALUE_BYTES,
                });
            }
            Ok(BrowserActionResult::Evaluation {
                sequence,
                executed: true,
                value,
            })
        }
    }
}

async fn capture_dom_snapshot(
    page: &Page,
    computed_styles: Vec<String>,
    include_dom_rects: bool,
    include_paint_order: bool,
) -> Result<BrowserDomSnapshot, BrowserError> {
    // Chrome rejects an empty `computedStyles` array even though the protocol
    // describes it as valid. Request one throwaway property and omit it while
    // decoding so the public result still reflects the caller's exact list.
    let protocol_styles = if computed_styles.is_empty() {
        vec!["display".to_owned()]
    } else {
        computed_styles.clone()
    };
    let mut params = CaptureSnapshotParams::new(protocol_styles);
    params.include_dom_rects = Some(include_dom_rects);
    params.include_paint_order = Some(include_paint_order);
    let raw = page.execute(params).await?;
    let node_count = raw
        .documents
        .iter()
        .map(|document| document.nodes.node_type.as_deref().map_or(0, <[i64]>::len))
        .fold(0_usize, usize::saturating_add);
    let string_bytes = raw
        .strings
        .iter()
        .map(String::len)
        .fold(0_usize, usize::saturating_add);
    if node_count > MAX_DOM_SNAPSHOT_NODES || string_bytes > MAX_DOM_SNAPSHOT_STRING_BYTES {
        return Err(BrowserError::DomSnapshotTooLarge {
            nodes: node_count,
            string_bytes,
            maximum_nodes: MAX_DOM_SNAPSHOT_NODES,
            maximum_string_bytes: MAX_DOM_SNAPSHOT_STRING_BYTES,
        });
    }
    decode_dom_snapshot(&raw, &computed_styles)
}

fn decode_dom_snapshot(
    raw: &CaptureSnapshotReturns,
    computed_styles: &[String],
) -> Result<BrowserDomSnapshot, BrowserError> {
    let mut node_count = 0;
    let mut element_count = 0;
    let mut text_node_count = 0;
    let mut comment_count = 0;
    let mut attribute_count = 0;
    let mut shadow_tree_node_count = 0;
    let mut documents = Vec::with_capacity(raw.documents.len());

    for document in &raw.documents {
        let nodes = decode_dom_nodes(document, &raw.strings, computed_styles)?;
        node_count += nodes.len();
        element_count += nodes.iter().filter(|node| node.node_type == 1).count();
        text_node_count += nodes.iter().filter(|node| node.node_type == 3).count();
        comment_count += nodes.iter().filter(|node| node.node_type == 8).count();
        attribute_count += nodes
            .iter()
            .map(|node| node.attributes.len())
            .sum::<usize>();
        shadow_tree_node_count += nodes
            .iter()
            .filter(|node| node.shadow_root_type.is_some())
            .count();
        documents.push(BrowserDomDocument {
            document_url: snapshot_string(&raw.strings, document.document_url)?.to_owned(),
            title: snapshot_string(&raw.strings, document.title)?.to_owned(),
            base_url: snapshot_string(&raw.strings, document.base_url)?.to_owned(),
            content_language: snapshot_string(&raw.strings, document.content_language)?.to_owned(),
            encoding_name: snapshot_string(&raw.strings, document.encoding_name)?.to_owned(),
            public_id: snapshot_string(&raw.strings, document.public_id)?.to_owned(),
            system_id: snapshot_string(&raw.strings, document.system_id)?.to_owned(),
            frame_id: snapshot_string(&raw.strings, document.frame_id)?.to_owned(),
            scroll_offset_x: document.scroll_offset_x,
            scroll_offset_y: document.scroll_offset_y,
            content_width: document.content_width,
            content_height: document.content_height,
            nodes,
        });
    }

    Ok(BrowserDomSnapshot {
        documents,
        node_count,
        element_count,
        text_node_count,
        comment_count,
        attribute_count,
        shadow_tree_node_count,
    })
}

fn decode_dom_nodes(
    document: &DocumentSnapshot,
    strings: &[String],
    computed_styles: &[String],
) -> Result<Vec<BrowserDomNode>, BrowserError> {
    let nodes = &document.nodes;
    let node_types = nodes.node_type.as_deref().unwrap_or_default();
    let node_names = nodes.node_name.as_deref().unwrap_or_default();
    let node_values = nodes.node_value.as_deref().unwrap_or_default();
    let backend_node_ids = nodes.backend_node_id.as_deref().unwrap_or_default();
    let parents = nodes.parent_index.as_deref().unwrap_or_default();
    let attributes = nodes.attributes.as_deref().unwrap_or_default();
    let shadow_root_types = rare_strings(nodes.shadow_root_type.as_ref(), strings)?;
    let text_values = rare_strings(nodes.text_value.as_ref(), strings)?;
    let input_values = rare_strings(nodes.input_value.as_ref(), strings)?;
    let input_checked = nodes.input_checked.as_ref();
    let option_selected = nodes.option_selected.as_ref();
    let content_documents = rare_integers(nodes.content_document_index.as_ref())?;
    let pseudo_types = rare_strings(nodes.pseudo_type.as_ref(), strings)?;
    let pseudo_identifiers = rare_strings(nodes.pseudo_identifier.as_ref(), strings)?;
    let clickable = nodes.is_clickable.as_ref();
    let current_source_urls = rare_strings(nodes.current_source_url.as_ref(), strings)?;
    let origin_urls = rare_strings(nodes.origin_url.as_ref(), strings)?;
    let layouts = decode_layouts(&document.layout, strings, computed_styles)?;
    let mut decoded = Vec::with_capacity(node_types.len());

    for (index, node_type) in node_types.iter().copied().enumerate() {
        let parent_index = parents.get(index).copied().and_then(nonnegative_index);
        let backend_node_id = backend_node_ids
            .get(index)
            .map_or(0, |node_id| *node_id.inner());
        decoded.push(BrowserDomNode {
            index,
            parent_index,
            node_type,
            node_name: node_names
                .get(index)
                .map(|index| snapshot_string(strings, *index))
                .transpose()?
                .unwrap_or_default()
                .to_owned(),
            node_value: node_values
                .get(index)
                .map(|index| snapshot_string(strings, *index))
                .transpose()?
                .unwrap_or_default()
                .to_owned(),
            backend_node_id,
            attributes: attributes
                .get(index)
                .map(|attributes| decode_attributes(attributes, strings))
                .transpose()?
                .unwrap_or_default(),
            shadow_root_type: shadow_root_types.get(&index).cloned(),
            text_value: text_values.get(&index).cloned(),
            input_value: input_values.get(&index).cloned(),
            input_checked: rare_boolean(input_checked, index),
            option_selected: rare_boolean(option_selected, index),
            content_document_index: content_documents.get(&index).copied(),
            pseudo_type: pseudo_types.get(&index).cloned(),
            pseudo_identifier: pseudo_identifiers.get(&index).cloned(),
            is_clickable: rare_boolean(clickable, index).unwrap_or(false),
            current_source_url: current_source_urls.get(&index).cloned(),
            origin_url: origin_urls.get(&index).cloned(),
            layout: layouts.get(&index).cloned(),
        });
    }
    Ok(decoded)
}

fn decode_layouts(
    layout: &LayoutTreeSnapshot,
    strings: &[String],
    computed_styles: &[String],
) -> Result<HashMap<usize, BrowserDomLayout>, BrowserError> {
    let mut layouts = HashMap::with_capacity(layout.node_index.len());
    for (layout_index, node_index) in layout.node_index.iter().copied().enumerate() {
        let node_index = required_index(node_index, "layout node index")?;
        let bounds = layout.bounds.get(layout_index).ok_or_else(|| {
            invalid_dom_snapshot("layout bounds table is shorter than node table")
        })?;
        let styles = layout
            .styles
            .get(layout_index)
            .map(|styles| decode_styles(styles, strings, computed_styles))
            .transpose()?
            .unwrap_or_default();
        let text = layout
            .text
            .get(layout_index)
            .map(|index| snapshot_string(strings, *index))
            .transpose()?
            .unwrap_or_default()
            .to_owned();
        layouts.insert(
            node_index,
            BrowserDomLayout {
                bounds: decode_rectangle(bounds)?,
                text,
                styles,
                paint_order: layout
                    .paint_orders
                    .as_ref()
                    .and_then(|orders| orders.get(layout_index))
                    .copied(),
                offset_rect: optional_rectangle(layout.offset_rects.as_ref(), layout_index)?,
                scroll_rect: optional_rectangle(layout.scroll_rects.as_ref(), layout_index)?,
                client_rect: optional_rectangle(layout.client_rects.as_ref(), layout_index)?,
            },
        );
    }
    Ok(layouts)
}

fn decode_styles(
    styles: &ArrayOfStrings,
    strings: &[String],
    computed_styles: &[String],
) -> Result<BTreeMap<String, String>, BrowserError> {
    computed_styles
        .iter()
        .zip(styles.inner())
        .map(|(name, index)| Ok((name.clone(), snapshot_string(strings, *index)?.to_owned())))
        .collect()
}

fn decode_attributes(
    attributes: &ArrayOfStrings,
    strings: &[String],
) -> Result<BTreeMap<String, String>, BrowserError> {
    let attributes = attributes.inner();
    if !attributes.len().is_multiple_of(2) {
        return Err(invalid_dom_snapshot(
            "DOM attribute table contains an unpaired name",
        ));
    }
    attributes
        .chunks_exact(2)
        .map(|pair| {
            Ok((
                snapshot_string(strings, pair[0])?.to_owned(),
                snapshot_string(strings, pair[1])?.to_owned(),
            ))
        })
        .collect()
}

fn rare_strings(
    data: Option<&RareStringData>,
    strings: &[String],
) -> Result<HashMap<usize, String>, BrowserError> {
    let Some(data) = data else {
        return Ok(HashMap::new());
    };
    if data.index.len() != data.value.len() {
        return Err(invalid_dom_snapshot(
            "rare string indexes and values have different lengths",
        ));
    }
    data.index
        .iter()
        .copied()
        .zip(data.value.iter().copied())
        .map(|(node_index, value)| {
            Ok((
                required_index(node_index, "rare string node index")?,
                snapshot_string(strings, value)?.to_owned(),
            ))
        })
        .collect()
}

fn rare_integers(data: Option<&RareIntegerData>) -> Result<HashMap<usize, usize>, BrowserError> {
    let Some(data) = data else {
        return Ok(HashMap::new());
    };
    if data.index.len() != data.value.len() {
        return Err(invalid_dom_snapshot(
            "rare integer indexes and values have different lengths",
        ));
    }
    data.index
        .iter()
        .copied()
        .zip(data.value.iter().copied())
        .map(|(node_index, value)| {
            Ok((
                required_index(node_index, "rare integer node index")?,
                required_index(value, "rare integer value")?,
            ))
        })
        .collect()
}

fn rare_boolean(data: Option<&RareBooleanData>, node_index: usize) -> Option<bool> {
    data.map(|data| {
        i64::try_from(node_index).is_ok_and(|node_index| data.index.contains(&node_index))
    })
}

fn snapshot_string(strings: &[String], index: StringIndex) -> Result<&str, BrowserError> {
    let index = *index.inner();
    if index == -1 {
        return Ok("");
    }
    let index = required_index(index, "string table index")?;
    strings
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| invalid_dom_snapshot(format!("string table index {index} is out of bounds")))
}

fn optional_rectangle(
    rectangles: Option<&Vec<Rectangle>>,
    index: usize,
) -> Result<Option<BrowserDomRect>, BrowserError> {
    let Some(rectangle) = rectangles.and_then(|rectangles| rectangles.get(index)) else {
        return Ok(None);
    };
    if rectangle.inner().is_empty() {
        return Ok(None);
    }
    decode_rectangle(rectangle).map(Some)
}

fn decode_rectangle(rectangle: &Rectangle) -> Result<BrowserDomRect, BrowserError> {
    let values = rectangle.inner();
    if values.len() != 4 {
        return Err(invalid_dom_snapshot(format!(
            "DOM rectangle has {} values instead of four",
            values.len()
        )));
    }
    Ok(BrowserDomRect {
        x: values[0],
        y: values[1],
        width: values[2],
        height: values[3],
    })
}

fn nonnegative_index(value: i64) -> Option<usize> {
    usize::try_from(value).ok()
}

fn required_index(value: i64, field: &str) -> Result<usize, BrowserError> {
    usize::try_from(value)
        .map_err(|_| invalid_dom_snapshot(format!("{field} cannot be negative: {value}")))
}

fn invalid_dom_snapshot(message: impl Into<String>) -> BrowserError {
    BrowserError::InvalidDomSnapshot {
        message: message.into(),
    }
}

async fn navigate(page: &Page, url: &str) -> Result<(), BrowserError> {
    match timeout(DEFAULT_NAVIGATION_TIMEOUT, page.goto(url)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            if let Some(current_url) =
                wait_for_usable_document(page, url, DEFAULT_NAVIGATION_TIMEOUT).await
            {
                warn!(
                    target: "nanocodex_browser",
                    requested_url = url,
                    current_url,
                    %error,
                    "continuing after a navigation race replaced the original document"
                );
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(_) => {
            if let Some(current_url) =
                wait_for_usable_document(page, url, DEFAULT_NAVIGATION_TIMEOUT).await
            {
                warn!(
                    target: "nanocodex_browser",
                    requested_url = url,
                    current_url,
                    timeout_ms = DEFAULT_NAVIGATION_TIMEOUT.as_millis(),
                    "continuing after the main document became usable while subframes kept loading"
                );
                return Ok(());
            }
            Err(BrowserError::NavigationTimeout {
                url: url.to_owned(),
                milliseconds: DEFAULT_NAVIGATION_TIMEOUT.as_millis(),
            })
        }
    }
}

async fn reload(page: &Page) -> Result<(), BrowserError> {
    let url = page
        .url()
        .await?
        .unwrap_or_else(|| "about:blank".to_owned());
    match timeout(DEFAULT_NAVIGATION_TIMEOUT, page.reload()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            if wait_for_usable_document(page, &url, DEFAULT_NAVIGATION_TIMEOUT)
                .await
                .is_some()
            {
                warn!(
                    target: "nanocodex_browser",
                    current_url = url,
                    %error,
                    "continuing after a reload race left a usable document"
                );
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(_) => {
            if wait_for_usable_document(page, &url, DEFAULT_NAVIGATION_TIMEOUT)
                .await
                .is_some()
            {
                warn!(
                    target: "nanocodex_browser",
                    current_url = url,
                    timeout_ms = DEFAULT_NAVIGATION_TIMEOUT.as_millis(),
                    "continuing after reload produced a usable document while resources kept loading"
                );
                Ok(())
            } else {
                Err(BrowserError::NavigationTimeout {
                    url,
                    milliseconds: DEFAULT_NAVIGATION_TIMEOUT.as_millis(),
                })
            }
        }
    }
}

async fn navigate_history(page: &Page, delta: i64) -> Result<(), BrowserError> {
    let history = page.execute(GetNavigationHistoryParams::default()).await?;
    let target_index = history.current_index + delta;
    let target_index =
        usize::try_from(target_index).map_err(|_| BrowserError::HistoryEntryUnavailable)?;
    let entry = history
        .entries
        .get(target_index)
        .ok_or(BrowserError::HistoryEntryUnavailable)?;
    page.execute(NavigateToHistoryEntryParams::new(entry.id))
        .await?;
    Ok(())
}

async fn wait_for_usable_document(
    page: &Page,
    requested_url: &str,
    duration: Duration,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if let Ok(Some(current_url)) = page.url().await
            && usable_document_url(&current_url, requested_url)
            && evaluate_typed::<bool>(
                page,
                "document.readyState === 'interactive' || document.readyState === 'complete'",
            )
            .await
            .unwrap_or(false)
        {
            return Some(current_url);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn usable_document_url(current_url: &str, requested_url: &str) -> bool {
    if current_url.is_empty() || current_url.starts_with("chrome-error:") {
        return false;
    }
    if current_url == "about:blank" {
        return requested_url == "about:blank";
    }
    true
}

#[allow(
    clippy::too_many_lines,
    reason = "each typed DevTools event stream is registered and owned together"
)]
async fn start_diagnostics(
    page: &Page,
    diagnostics: Arc<StdMutex<Diagnostics>>,
) -> Result<Vec<JoinHandle<()>>, BrowserError> {
    let mut console_events = page.event_listener::<EventConsoleApiCalled>().await?;
    let console_diagnostics = Arc::clone(&diagnostics);
    let console = tokio::spawn(async move {
        while let Some(event) = console_events.next().await {
            trace_serialized("devtools.Runtime.consoleAPICalled", event.as_ref());
            let entry = BrowserConsoleEntry {
                sequence: 0,
                level: event.r#type.as_ref().to_owned(),
                text: event
                    .args
                    .iter()
                    .map(remote_object_text)
                    .collect::<Vec<_>>()
                    .join(" "),
                stack: source_maps::stack_frames(event.stack_trace.as_ref()),
            };
            let Ok(mut diagnostics) = console_diagnostics.lock() else {
                break;
            };
            diagnostics.push_console(entry);
        }
    });

    let mut error_events = page.event_listener::<EventExceptionThrown>().await?;
    let error_diagnostics = Arc::clone(&diagnostics);
    let errors = tokio::spawn(async move {
        while let Some(event) = error_events.next().await {
            trace_serialized("devtools.Runtime.exceptionThrown", event.as_ref());
            let details = &event.exception_details;
            let error = BrowserPageError {
                sequence: 0,
                text: details
                    .exception
                    .as_ref()
                    .and_then(|exception| exception.description.clone())
                    .unwrap_or_else(|| details.text.clone()),
                url: details.url.clone(),
                line: u64::try_from(details.line_number).ok(),
                column: u64::try_from(details.column_number).ok(),
                stack: source_maps::stack_frames(details.stack_trace.as_ref()),
            };
            let Ok(mut diagnostics) = error_diagnostics.lock() else {
                break;
            };
            diagnostics.push_error(error);
        }
    });

    let mut request_events = page.event_listener::<EventRequestWillBeSent>().await?;
    let request_diagnostics = Arc::clone(&diagnostics);
    let requests = tokio::spawn(async move {
        while let Some(event) = request_events.next().await {
            trace_serialized("devtools.Network.requestWillBeSent", event.as_ref());
            let id = event.request_id.as_ref().to_owned();
            let timestamp = *event.timestamp.inner();
            let Ok(mut diagnostics) = request_diagnostics.lock() else {
                break;
            };
            if let Some(redirect) = &event.redirect_response
                && let Some(entry) = diagnostics.request_entry_mut(&id)
            {
                apply_response(&mut entry.request, redirect);
                finish_request(entry, timestamp, redirect.encoded_data_length);
            }
            diagnostics.push_request(
                &id,
                NetworkSource::Page,
                timestamp,
                BrowserNetworkRequest {
                    sequence: 0,
                    request_id: String::new(),
                    context: BrowserNetworkContext::Page,
                    body_available: true,
                    url: event.request.url.clone(),
                    method: event.request.method.clone(),
                    document_url: event.document_url.clone(),
                    resource_type: event
                        .r#type
                        .as_ref()
                        .map_or_else(|| "Other".to_owned(), |kind| kind.as_ref().to_owned()),
                    started_at_epoch_ms: seconds_to_milliseconds(*event.wall_time.inner()),
                    duration_ms: None,
                    initiator: Some(network_initiator(&event.initiator)),
                    request_headers: network_headers(&event.request.headers),
                    has_post_data: event.request.has_post_data.unwrap_or(false),
                    status: None,
                    status_text: None,
                    response_headers: Vec::new(),
                    mime_type: None,
                    charset: None,
                    protocol: None,
                    remote_ip_address: None,
                    remote_port: None,
                    from_disk_cache: false,
                    from_service_worker: false,
                    encoded_data_length: None,
                    timing: None,
                    completed: false,
                    failure: None,
                },
            );
        }
    });

    let mut response_events = page.event_listener::<EventResponseReceived>().await?;
    let response_diagnostics = Arc::clone(&diagnostics);
    let responses = tokio::spawn(async move {
        while let Some(event) = response_events.next().await {
            trace_serialized("devtools.Network.responseReceived", event.as_ref());
            let id = event.request_id.as_ref();
            let Ok(mut diagnostics) = response_diagnostics.lock() else {
                break;
            };
            diagnostics.record_page_response(id, &event.response, event.r#type.as_ref().to_owned());
        }
    });

    let mut finished_events = page.event_listener::<EventLoadingFinished>().await?;
    let finished_diagnostics = Arc::clone(&diagnostics);
    let finished = tokio::spawn(async move {
        while let Some(event) = finished_events.next().await {
            trace_serialized("devtools.Network.loadingFinished", event.as_ref());
            let id = event.request_id.as_ref();
            let Ok(mut diagnostics) = finished_diagnostics.lock() else {
                break;
            };
            diagnostics.finish_page_request(
                id,
                *event.timestamp.inner(),
                event.encoded_data_length,
            );
        }
    });

    let mut failed_events = page.event_listener::<EventLoadingFailed>().await?;
    let failed_diagnostics = Arc::clone(&diagnostics);
    let failures = tokio::spawn(async move {
        while let Some(event) = failed_events.next().await {
            trace_serialized("devtools.Network.loadingFailed", event.as_ref());
            let id = event.request_id.as_ref();
            let Ok(mut diagnostics) = failed_diagnostics.lock() else {
                break;
            };
            diagnostics.fail_page_request(
                id,
                *event.timestamp.inner(),
                event.error_text.clone(),
                event.r#type.as_ref().to_owned(),
            );
        }
    });

    let mut web_socket_created_events = page.event_listener::<EventWebSocketCreated>().await?;
    let web_socket_created_diagnostics = Arc::clone(&diagnostics);
    let web_socket_created = tokio::spawn(async move {
        while let Some(event) = web_socket_created_events.next().await {
            trace_serialized("devtools.Network.webSocketCreated", event.as_ref());
            let id = event.request_id.as_ref().to_owned();
            let Ok(mut diagnostics) = web_socket_created_diagnostics.lock() else {
                break;
            };
            if diagnostics.request_entry_mut(&id).is_some() {
                continue;
            }
            diagnostics.push_request(
                &id,
                NetworkSource::Page,
                0.0,
                BrowserNetworkRequest {
                    sequence: 0,
                    request_id: String::new(),
                    context: BrowserNetworkContext::Page,
                    body_available: false,
                    url: event.url.clone(),
                    method: "GET".to_owned(),
                    document_url: String::new(),
                    resource_type: "WebSocket".to_owned(),
                    started_at_epoch_ms: 0,
                    duration_ms: None,
                    initiator: event.initiator.as_ref().map(network_initiator),
                    request_headers: Vec::new(),
                    has_post_data: false,
                    status: None,
                    status_text: None,
                    response_headers: Vec::new(),
                    mime_type: None,
                    charset: None,
                    protocol: Some("websocket".to_owned()),
                    remote_ip_address: None,
                    remote_port: None,
                    from_disk_cache: false,
                    from_service_worker: false,
                    encoded_data_length: None,
                    timing: None,
                    completed: false,
                    failure: None,
                },
            );
        }
    });

    let mut web_socket_request_events = page
        .event_listener::<EventWebSocketWillSendHandshakeRequest>()
        .await?;
    let web_socket_request_diagnostics = Arc::clone(&diagnostics);
    let web_socket_requests = tokio::spawn(async move {
        while let Some(event) = web_socket_request_events.next().await {
            trace_serialized(
                "devtools.Network.webSocketWillSendHandshakeRequest",
                event.as_ref(),
            );
            let id = event.request_id.as_ref().to_owned();
            let timestamp = *event.timestamp.inner();
            let Ok(mut diagnostics) = web_socket_request_diagnostics.lock() else {
                break;
            };
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                entry.started_at_monotonic_seconds = timestamp;
                entry.request.started_at_epoch_ms =
                    seconds_to_milliseconds(*event.wall_time.inner());
                entry.request.request_headers = network_headers(&event.request.headers);
            } else {
                diagnostics.push_request(
                    &id,
                    NetworkSource::Page,
                    timestamp,
                    BrowserNetworkRequest {
                        sequence: 0,
                        request_id: String::new(),
                        context: BrowserNetworkContext::Page,
                        body_available: false,
                        url: String::new(),
                        method: "GET".to_owned(),
                        document_url: String::new(),
                        resource_type: "WebSocket".to_owned(),
                        started_at_epoch_ms: seconds_to_milliseconds(*event.wall_time.inner()),
                        duration_ms: None,
                        initiator: None,
                        request_headers: network_headers(&event.request.headers),
                        has_post_data: false,
                        status: None,
                        status_text: None,
                        response_headers: Vec::new(),
                        mime_type: None,
                        charset: None,
                        protocol: Some("websocket".to_owned()),
                        remote_ip_address: None,
                        remote_port: None,
                        from_disk_cache: false,
                        from_service_worker: false,
                        encoded_data_length: None,
                        timing: None,
                        completed: false,
                        failure: None,
                    },
                );
            }
        }
    });

    let mut web_socket_response_events = page
        .event_listener::<EventWebSocketHandshakeResponseReceived>()
        .await?;
    let web_socket_response_diagnostics = Arc::clone(&diagnostics);
    let web_socket_responses = tokio::spawn(async move {
        while let Some(event) = web_socket_response_events.next().await {
            trace_serialized(
                "devtools.Network.webSocketHandshakeResponseReceived",
                event.as_ref(),
            );
            let id = event.request_id.as_ref();
            let Ok(mut diagnostics) = web_socket_response_diagnostics.lock() else {
                break;
            };
            if let Some(entry) = diagnostics.request_entry_mut(id) {
                entry.request.status = Some(event.response.status);
                entry.request.status_text = Some(event.response.status_text.clone());
                entry.request.response_headers = network_headers(&event.response.headers);
            }
        }
    });

    let mut web_socket_sent_events = page.event_listener::<EventWebSocketFrameSent>().await?;
    let web_socket_sent_diagnostics = Arc::clone(&diagnostics);
    let web_socket_sent = tokio::spawn(async move {
        while let Some(event) = web_socket_sent_events.next().await {
            trace_serialized("devtools.Network.webSocketFrameSent", event.as_ref());
            let Ok(mut diagnostics) = web_socket_sent_diagnostics.lock() else {
                break;
            };
            diagnostics.push_web_socket_message(
                event.request_id.as_ref().to_owned(),
                BrowserWebSocketDirection::Sent,
                *event.timestamp.inner(),
                &event.response,
            );
        }
    });

    let mut web_socket_received_events =
        page.event_listener::<EventWebSocketFrameReceived>().await?;
    let web_socket_received_diagnostics = Arc::clone(&diagnostics);
    let web_socket_received = tokio::spawn(async move {
        while let Some(event) = web_socket_received_events.next().await {
            trace_serialized("devtools.Network.webSocketFrameReceived", event.as_ref());
            let Ok(mut diagnostics) = web_socket_received_diagnostics.lock() else {
                break;
            };
            diagnostics.push_web_socket_message(
                event.request_id.as_ref().to_owned(),
                BrowserWebSocketDirection::Received,
                *event.timestamp.inner(),
                &event.response,
            );
        }
    });

    let mut web_socket_error_events = page.event_listener::<EventWebSocketFrameError>().await?;
    let web_socket_error_diagnostics = Arc::clone(&diagnostics);
    let web_socket_errors = tokio::spawn(async move {
        while let Some(event) = web_socket_error_events.next().await {
            trace_serialized("devtools.Network.webSocketFrameError", event.as_ref());
            let Ok(mut diagnostics) = web_socket_error_diagnostics.lock() else {
                break;
            };
            if let Some(entry) = diagnostics.request_entry_mut(event.request_id.as_ref()) {
                entry.request.failure = Some(event.error_message.clone());
            }
        }
    });

    let mut web_socket_closed_events = page.event_listener::<EventWebSocketClosed>().await?;
    let dialog_diagnostics = Arc::clone(&diagnostics);
    let web_socket_closed_diagnostics = diagnostics;
    let web_socket_closed = tokio::spawn(async move {
        while let Some(event) = web_socket_closed_events.next().await {
            trace_serialized("devtools.Network.webSocketClosed", event.as_ref());
            let Ok(mut diagnostics) = web_socket_closed_diagnostics.lock() else {
                break;
            };
            if let Some(entry) = diagnostics.request_entry_mut(event.request_id.as_ref()) {
                finish_request(entry, *event.timestamp.inner(), 0.0);
            }
        }
    });

    let mut dialog_events = page
        .event_listener::<EventJavascriptDialogOpening>()
        .await?;
    let dialogs = tokio::spawn(async move {
        while let Some(event) = dialog_events.next().await {
            trace_serialized("devtools.Page.javascriptDialogOpening", event.as_ref());
            let kind = match event.r#type.as_ref() {
                "alert" => BrowserDialogKind::Alert,
                "confirm" => BrowserDialogKind::Confirm,
                "prompt" => BrowserDialogKind::Prompt,
                "beforeunload" => BrowserDialogKind::BeforeUnload,
                _ => BrowserDialogKind::Other,
            };
            let Ok(mut diagnostics) = dialog_diagnostics.lock() else {
                break;
            };
            diagnostics.dialog = Some(BrowserDialog {
                kind,
                message: event.message.clone(),
                default_prompt: event.default_prompt.clone().unwrap_or_default(),
                url: event.url.clone(),
            });
        }
    });

    Ok(vec![
        console,
        errors,
        requests,
        responses,
        finished,
        failures,
        web_socket_created,
        web_socket_requests,
        web_socket_responses,
        web_socket_sent,
        web_socket_received,
        web_socket_errors,
        web_socket_closed,
        dialogs,
    ])
}

async fn start_download_diagnostics(
    browser: &Chromium,
    page: &Page,
    download_dir: &Path,
    diagnostics: Arc<StdMutex<Diagnostics>>,
) -> Result<Vec<JoinHandle<()>>, BrowserError> {
    browser
        .execute(
            SetDownloadBehaviorParams::builder()
                .behavior(SetDownloadBehaviorBehavior::AllowAndName)
                .download_path(download_dir.to_string_lossy())
                .events_enabled(true)
                .build()
                .map_err(|message| BrowserError::Configuration { message })?,
        )
        .await?;
    let mut started_events = browser.event_listener::<EventDownloadWillBegin>().await?;
    let started_diagnostics = Arc::clone(&diagnostics);
    let started_page = page.clone();
    let started = tokio::spawn(async move {
        while let Some(event) = started_events.next().await {
            trace_serialized("devtools.Browser.downloadWillBegin", event.as_ref());
            let cancel = {
                let Ok(mut diagnostics) = started_diagnostics.lock() else {
                    break;
                };
                record_download_start(&mut diagnostics, &event)
            };
            if cancel {
                let _ = started_page
                    .execute(CancelDownloadParams::new(event.guid.clone()))
                    .await;
            }
        }
    });

    let mut progress_events = browser.event_listener::<EventDownloadProgress>().await?;
    let progress_page = page.clone();
    let progress = tokio::spawn(async move {
        while let Some(event) = progress_events.next().await {
            trace_serialized("devtools.Browser.downloadProgress", event.as_ref());
            let cancel = {
                let Ok(mut diagnostics) = diagnostics.lock() else {
                    break;
                };
                record_download_progress(&mut diagnostics, &event)
            };
            if cancel {
                let _ = progress_page
                    .execute(CancelDownloadParams::new(event.guid.clone()))
                    .await;
            }
        }
    });
    Ok(vec![started, progress])
}

fn record_download_start(diagnostics: &mut Diagnostics, event: &EventDownloadWillBegin) -> bool {
    if !diagnostics.downloads.contains_key(&event.guid)
        && diagnostics.downloads.len() >= MAX_DOWNLOADS
    {
        return true;
    }
    diagnostics.downloads.insert(
        event.guid.clone(),
        BrowserDownload {
            id: event.guid.clone(),
            url: bounded_string(&event.url, MAX_DIAGNOSTIC_TEXT_BYTES),
            suggested_filename: bounded_string(&event.suggested_filename, MAX_NETWORK_FIELD_BYTES),
            path: None,
            received_bytes: 0,
            total_bytes: None,
            completed: false,
            failure: None,
        },
    );
    false
}

fn record_download_progress(diagnostics: &mut Diagnostics, event: &EventDownloadProgress) -> bool {
    if !diagnostics.downloads.contains_key(&event.guid)
        && diagnostics.downloads.len() >= MAX_DOWNLOADS
    {
        return true;
    }
    let download = diagnostics
        .downloads
        .entry(event.guid.clone())
        .or_insert_with(|| BrowserDownload {
            id: event.guid.clone(),
            url: String::new(),
            suggested_filename: String::new(),
            path: None,
            received_bytes: 0,
            total_bytes: None,
            completed: false,
            failure: None,
        });
    download.received_bytes = nonnegative_f64_to_u64(event.received_bytes);
    download.total_bytes =
        (event.total_bytes > 0.0).then(|| nonnegative_f64_to_u64(event.total_bytes));
    download.path = event.file_path.as_ref().map(PathBuf::from);
    let oversized = download.received_bytes > MAX_DOWNLOAD_BYTES
        || download
            .total_bytes
            .is_some_and(|bytes| bytes > MAX_DOWNLOAD_BYTES);
    match event.state {
        DownloadProgressState::InProgress => {}
        DownloadProgressState::Completed => download.completed = true,
        DownloadProgressState::Canceled => {
            download.failure = Some("download canceled".to_owned());
        }
    }
    if oversized {
        download.failure = Some(format!(
            "download exceeded the {MAX_DOWNLOAD_BYTES}-byte session limit"
        ));
    }
    oversized
}

fn apply_response(request: &mut BrowserNetworkRequest, response: &Response) {
    request.status = Some(response.status);
    request.status_text = Some(bounded_string(
        &response.status_text,
        MAX_NETWORK_FIELD_BYTES,
    ));
    request.response_headers = network_headers(&response.headers);
    request.mime_type = Some(bounded_string(&response.mime_type, MAX_NETWORK_FIELD_BYTES));
    request.charset = Some(bounded_string(&response.charset, MAX_NETWORK_FIELD_BYTES));
    request.protocol = response
        .protocol
        .as_deref()
        .map(|value| bounded_string(value, MAX_NETWORK_FIELD_BYTES));
    request.remote_ip_address = response
        .remote_ip_address
        .as_deref()
        .map(|value| bounded_string(value, MAX_NETWORK_FIELD_BYTES));
    request.remote_port = response.remote_port;
    request.from_disk_cache = response.from_disk_cache.unwrap_or(false);
    request.from_service_worker = response.from_service_worker.unwrap_or(false);
    request.encoded_data_length = Some(nonnegative_f64_to_u64(response.encoded_data_length));
    request.timing = response.timing.as_ref().map(network_timing);
}

fn finish_request(entry: &mut NetworkEntry, timestamp_seconds: f64, encoded_data_length: f64) {
    if entry.started_at_monotonic_seconds > 0.0
        && timestamp_seconds >= entry.started_at_monotonic_seconds
    {
        entry.request.duration_ms = Some(seconds_to_milliseconds(
            timestamp_seconds - entry.started_at_monotonic_seconds,
        ));
    }
    entry.request.encoded_data_length = Some(nonnegative_f64_to_u64(encoded_data_length));
    entry.request.completed = true;
}

fn network_headers(headers: &Headers) -> Vec<BrowserHttpHeader> {
    let Some(headers) = headers.inner().as_object() else {
        return Vec::new();
    };
    let mut retained_bytes = 0_usize;
    let mut headers = headers
        .iter()
        .take(MAX_NETWORK_HEADERS)
        .filter_map(|(name, value)| {
            let name = bounded_string(name, MAX_NETWORK_FIELD_BYTES);
            let sensitive = matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
            );
            let value = (!sensitive)
                .then(|| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
                .map(|mut value| {
                    truncate_utf8(&mut value, MAX_NETWORK_FIELD_BYTES);
                    value
                });
            let bytes = name.len() + value.as_ref().map_or(0, String::len);
            if retained_bytes.saturating_add(bytes) > MAX_NETWORK_HEADER_BYTES {
                return None;
            }
            retained_bytes = retained_bytes.saturating_add(bytes);
            Some(BrowserHttpHeader {
                name,
                value,
                sensitive,
            })
        })
        .collect::<Vec<_>>();
    headers.sort_unstable_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    headers
}

fn network_initiator(initiator: &Initiator) -> BrowserNetworkInitiator {
    let mut stack = Vec::new();
    if let Some(trace) = initiator.stack.as_ref() {
        collect_stack_frames(trace, &mut stack);
    }
    BrowserNetworkInitiator {
        kind: bounded_string(initiator.r#type.as_ref(), MAX_NETWORK_FIELD_BYTES),
        url: initiator
            .url
            .as_deref()
            .map(|url| bounded_string(url, MAX_NETWORK_FIELD_BYTES)),
        line_number: initiator.line_number,
        column_number: initiator.column_number,
        request_id: initiator
            .request_id
            .as_ref()
            .map(|request_id| request_id.as_ref().to_owned()),
        stack,
    }
}

fn collect_stack_frames(trace: &StackTrace, frames: &mut Vec<BrowserNetworkCallFrame>) {
    let mut trace = Some(trace);
    while let Some(current) = trace {
        let available = MAX_NETWORK_STACK_FRAMES.saturating_sub(frames.len());
        frames.extend(current.call_frames.iter().take(available).map(|frame| {
            BrowserNetworkCallFrame {
                function_name: bounded_string(&frame.function_name, MAX_NETWORK_FIELD_BYTES),
                url: bounded_string(&frame.url, MAX_NETWORK_FIELD_BYTES),
                line_number: frame.line_number,
                column_number: frame.column_number,
            }
        }));
        if frames.len() == MAX_NETWORK_STACK_FRAMES {
            break;
        }
        trace = current.parent.as_deref();
    }
}

const fn network_timing(timing: &ResourceTiming) -> BrowserNetworkTiming {
    BrowserNetworkTiming {
        request_time: timing.request_time,
        proxy_start: timing.proxy_start,
        proxy_end: timing.proxy_end,
        dns_start: timing.dns_start,
        dns_end: timing.dns_end,
        connect_start: timing.connect_start,
        connect_end: timing.connect_end,
        ssl_start: timing.ssl_start,
        ssl_end: timing.ssl_end,
        send_start: timing.send_start,
        send_end: timing.send_end,
        receive_headers_start: timing.receive_headers_start,
        receive_headers_end: timing.receive_headers_end,
    }
}

fn seconds_to_milliseconds(seconds: f64) -> u64 {
    Duration::try_from_secs_f64(seconds.max(0.0)).map_or(u64::MAX, |duration| {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "CDP byte counts are nonnegative integral f64 wire fields and Rust casts saturate overflow"
)]
const fn nonnegative_f64_to_u64(value: f64) -> u64 {
    value.max(0.0) as u64
}

fn remote_object_text(object: &RemoteObject) -> String {
    match &object.value {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => match serde_json::to_string(value) {
            Ok(value) => value,
            Err(_) => object.description.clone().unwrap_or_default(),
        },
        None => object.description.clone().unwrap_or_default(),
    }
}

async fn install_annotations(session: &Session) -> Result<(), BrowserError> {
    let refs = session
        .refs
        .iter()
        .filter(|(_, target)| target.context_id.is_none())
        .filter_map(|(reference, target)| match &target.query {
            ElementQuery::SnapshotPath(path) => Some((reference, path)),
            ElementQuery::Locator(_) => None,
        })
        .collect::<BTreeMap<_, _>>();
    let refs = serde_json::to_string(&refs)?;
    evaluate_value(
        &session.page,
        format!("({INSTALL_ANNOTATIONS_SCRIPT})({refs})"),
    )
    .await?;
    Ok(())
}

async fn evaluate_value(
    page: &Page,
    expression: impl Into<String>,
) -> Result<serde_json::Value, BrowserError> {
    evaluate_value_in_context(page, expression, None, false).await
}

async fn evaluate_value_in_context(
    page: &Page,
    expression: impl Into<String>,
    context_id: Option<ExecutionContextId>,
    user_gesture: bool,
) -> Result<serde_json::Value, BrowserError> {
    let expression = expression.into();
    let started_at = Instant::now();
    loop {
        let mut params = EvaluateParams::new(expression.clone());
        params.context_id = context_id;
        params.return_by_value = Some(true);
        params.await_promise = Some(true);
        params.user_gesture = Some(user_gesture);
        match page.evaluate_expression(params).await {
            Ok(value) => return Ok(value.into_value::<serde_json::Value>()?),
            Err(error)
                if context_id.is_none()
                    && started_at.elapsed() < MAIN_CONTEXT_RETRY_TIMEOUT
                    && is_transient_context_error(&error) =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_transient_context_error(error: &CdpError) -> bool {
    let CdpError::Chrome(error) = error else {
        return false;
    };
    error.code == -32_000
        && (error.message.contains("Cannot find context")
            || error.message.contains("execution context was destroyed")
            || error.message.contains("Execution context was destroyed")
            || error.message.contains("Inspected target navigated"))
}

async fn evaluate_typed<T>(page: &Page, expression: impl Into<String>) -> Result<T, BrowserError>
where
    T: serde::de::DeserializeOwned,
{
    evaluate_typed_in_context(page, expression, None).await
}

async fn evaluate_typed_in_context<T>(
    page: &Page,
    expression: impl Into<String>,
    context_id: Option<ExecutionContextId>,
) -> Result<T, BrowserError>
where
    T: serde::de::DeserializeOwned,
{
    Ok(serde_json::from_value(
        evaluate_value_in_context(page, expression, context_id, false).await?,
    )?)
}

fn element_script(query: &ElementQuery, body: &str) -> Result<String, serde_json::Error> {
    let query = query_resolver_prelude(query)?;
    Ok(format!(
        r#"(() => {{
{query}
const element = matches[0] ?? null;
if (!element) throw new Error("selector did not match an element");
{body}
}})()"#,
    ))
}

async fn capture_clip(
    session: &Session,
    target: Option<&BrowserTarget>,
    full_page: bool,
) -> Result<Option<Viewport>, BrowserError> {
    let Some(target) = target else {
        return Ok(None);
    };
    if full_page {
        return Err(BrowserError::TargetWithFullPage);
    }

    let target = session.target(target).await?;
    interaction::wait_until_actionable(
        &session.page,
        &target,
        interaction::Actionability::Attached,
    )
    .await?;
    evaluate_typed_in_context::<bool>(
        &session.page,
        element_script(
            &target.query,
            r#"element.scrollIntoView({ block: "center", inline: "center" });
return new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true))));"#,
        )?,
        target.context_id,
    )
    .await?;

    let mut params = EvaluateParams::new(element_script(&target.query, "return element;")?);
    params.context_id = target.context_id;
    params.return_by_value = Some(false);
    params.await_promise = Some(true);
    let result = session.page.evaluate_expression(params).await?;
    let object_id =
        result
            .object()
            .object_id
            .clone()
            .ok_or_else(|| BrowserError::Actionability {
                selector: target.query.display(),
                reason: "resolved element did not produce a remote object".to_owned(),
            })?;
    let model = session
        .page
        .execute(
            GetBoxModelParams::builder()
                .object_id(object_id.clone())
                .build(),
        )
        .await;
    let _ = session
        .page
        .execute(ReleaseObjectParams::new(object_id))
        .await;
    let border = model?.result.model.border;
    let coordinates = border.inner();
    if coordinates.len() != 8 || coordinates.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err(BrowserError::Actionability {
            selector: target.query.display(),
            reason: "element has an invalid rendered border box".to_owned(),
        });
    }
    let mut points = coordinates.chunks_exact(2);
    let first = points.next().ok_or_else(|| BrowserError::Actionability {
        selector: target.query.display(),
        reason: "element has no rendered border box".to_owned(),
    })?;
    let (mut left, mut right, mut top, mut bottom) = (first[0], first[0], first[1], first[1]);
    for point in points {
        left = left.min(point[0]);
        right = right.max(point[0]);
        top = top.min(point[1]);
        bottom = bottom.max(point[1]);
    }
    let width = right - left;
    let height = bottom - top;
    if width <= 0.0 || height <= 0.0 {
        return Err(BrowserError::Actionability {
            selector: target.query.display(),
            reason: "element has an empty rendered border box".to_owned(),
        });
    }
    let layout = session.page.layout_metrics().await?;
    Ok(Some(Viewport {
        x: left + layout.css_layout_viewport.page_x as f64,
        y: top + layout.css_layout_viewport.page_y as f64,
        width,
        height,
        scale: 1.0,
    }))
}

async fn set_file_input_files(
    page: &Page,
    target: &ElementTarget,
    files: Vec<String>,
) -> Result<(), BrowserError> {
    let mut params = EvaluateParams::new(element_script(&target.query, "return element;")?);
    params.context_id = target.context_id;
    params.return_by_value = Some(false);
    params.await_promise = Some(true);
    params.user_gesture = Some(true);
    let result = page.evaluate_expression(params).await?;
    let object_id =
        result
            .object()
            .object_id
            .clone()
            .ok_or_else(|| BrowserError::Configuration {
                message: "file input did not produce a remote object".to_owned(),
            })?;
    let mut params = SetFileInputFilesParams::new(files);
    params.object_id = Some(object_id);
    page.execute(params).await?;
    Ok(())
}

fn count_script(query: &ElementQuery) -> Result<String, serde_json::Error> {
    let query = query_resolver_prelude(query)?;
    Ok(format!("(() => {{\n{query}\nreturn matches.length;\n}})()"))
}

async fn press_key(
    page: &Page,
    key: &str,
    modifiers: &[crate::BrowserKeyModifier],
) -> Result<(), BrowserError> {
    interaction::dispatch_keyboard_event(page, key, modifiers, true).await?;
    interaction::dispatch_keyboard_event(page, key, modifiers, false).await
}

impl ElementQuery {
    fn javascript(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Query<'a> {
            SnapshotPath { path: &'a [String] },
            Locator { target: &'a BrowserTarget },
        }
        serde_json::to_string(&match self {
            Self::SnapshotPath(path) => Query::SnapshotPath { path },
            Self::Locator(target) => Query::Locator { target },
        })
    }

    fn display(&self) -> String {
        match self {
            Self::SnapshotPath(path) => path.join(" >>> "),
            Self::Locator(target) => target.display(),
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the generated resolver is one self-contained browser-side program shared by all locator actions"
)]
pub(super) fn query_resolver_prelude(query: &ElementQuery) -> Result<String, serde_json::Error> {
    let query = query.javascript()?;
    Ok(format!(
        r#"const query = {query};
const normalizeText = value => String(value ?? "").replace(/\s+/g, " ").trim();
const textMatches = (actual, expected, exact) => {{
  const left = normalizeText(actual);
  const right = normalizeText(expected);
  return exact ? left === right : left.toLocaleLowerCase().includes(right.toLocaleLowerCase());
}};
const allDeep = (root, results = []) => {{
  for (const element of root.querySelectorAll("*")) {{
    results.push(element);
    if (element.shadowRoot) allDeep(element.shadowRoot, results);
  }}
  return results;
}};
const resolvePath = path => {{
  let root = document;
  let element = null;
  for (let index = 0; index < path.length; index++) {{
    element = root.querySelector(path[index]);
    if (!element) return null;
    if (index + 1 < path.length) {{
      root = element.shadowRoot;
      if (!root) return null;
    }}
  }}
  return element;
}};
const implicitRole = element => {{
  const explicit = normalizeText(element.getAttribute?.("role")).split(" ")[0];
  if (explicit) return explicit;
  const tag = element.localName;
  if (tag === "a" && element.hasAttribute("href")) return "link";
  if (tag === "button") return "button";
  if (tag === "textarea") return "textbox";
  if (tag === "select") return element.multiple || element.size > 1 ? "listbox" : "combobox";
  if (tag === "option") return "option";
  if (tag === "img") return "img";
  if (/^h[1-6]$/.test(tag)) return "heading";
  if (tag === "ul" || tag === "ol") return "list";
  if (tag === "li") return "listitem";
  if (tag === "nav") return "navigation";
  if (tag === "main") return "main";
  if (tag === "article") return "article";
  if (tag === "table") return "table";
  if (tag === "tr") return "row";
  if (tag === "th") return "columnheader";
  if (tag === "td") return "cell";
  if (tag === "progress") return "progressbar";
  if (tag === "input") {{
    const type = (element.type || "text").toLowerCase();
    if (["button", "submit", "reset", "image"].includes(type)) return "button";
    if (type === "checkbox") return "checkbox";
    if (type === "radio") return "radio";
    if (type === "range") return "slider";
    if (type === "number") return "spinbutton";
    if (type === "search") return "searchbox";
    if (!["hidden", "file"].includes(type)) return "textbox";
  }}
  return "";
}};
const labelledText = element => {{
  const ids = normalizeText(element.getAttribute?.("aria-labelledby")).split(" ").filter(Boolean);
  const root = element.getRootNode?.() ?? document;
  return normalizeText(ids.map(id =>
    root.getElementById?.(id)?.textContent ?? document.getElementById(id)?.textContent ?? ""
  ).join(" "));
}};
const accessibleName = element => {{
  const aria = normalizeText(element.getAttribute?.("aria-label"));
  if (aria) return aria;
  const labelled = labelledText(element);
  if (labelled) return labelled;
  const labels = Array.from(element.labels ?? []).map(label => label.textContent).join(" ");
  if (normalizeText(labels)) return normalizeText(labels);
  if (element.localName === "img" || element.type === "image") {{
    const alt = normalizeText(element.getAttribute("alt"));
    if (alt) return alt;
  }}
  if (["button", "submit", "reset"].includes((element.type ?? "").toLowerCase())) {{
    const value = normalizeText(element.value);
    if (value) return value;
  }}
  const text = normalizeText(element.innerText ?? element.textContent);
  if (text) return text;
  return normalizeText(element.getAttribute?.("title"));
}};
const applyIndex = (matches, index) => {{
  if (!index) return matches;
  if (index.kind === "first") return matches.slice(0, 1);
  if (index.kind === "last") return matches.slice(-1);
  if (index.kind === "nth") return matches.slice(index.index, index.index + 1);
  return [];
}};
const resolveLocator = target => {{
  const candidates = allDeep(document);
  let matches = [];
  if (target.by === "css") {{
    matches = [];
    const visit = root => {{
      matches.push(...root.querySelectorAll(target.selector));
      for (const host of root.querySelectorAll("*")) {{
        if (host.shadowRoot) visit(host.shadowRoot);
      }}
    }};
    visit(document);
  }} else if (target.by === "role") {{
    matches = candidates.filter(element =>
      implicitRole(element) === target.role &&
      (target.name == null || textMatches(accessibleName(element), target.name, target.exact))
    );
  }} else if (target.by === "text") {{
    matches = candidates.filter(element =>
      textMatches(element.innerText ?? element.textContent, target.text, target.exact)
    );
    matches = matches.filter(element => !allDeep(element).some(child =>
      textMatches(child.innerText ?? child.textContent, target.text, target.exact)
    ));
  }} else if (target.by === "label") {{
    matches = candidates.filter(element =>
      Array.from(element.labels ?? []).some(label =>
        textMatches(label.innerText ?? label.textContent, target.label, target.exact)
      ) ||
      textMatches(labelledText(element), target.label, target.exact)
    );
  }} else if (target.by === "placeholder") {{
    matches = candidates.filter(element =>
      textMatches(element.getAttribute?.("placeholder"), target.placeholder, target.exact)
    );
  }} else if (target.by === "alt_text") {{
    matches = candidates.filter(element =>
      textMatches(element.getAttribute?.("alt"), target.text, target.exact)
    );
  }} else if (target.by === "title") {{
    matches = candidates.filter(element =>
      textMatches(element.getAttribute?.("title"), target.title, target.exact)
    );
  }} else if (target.by === "test_id") {{
    matches = candidates.filter(element => element.getAttribute?.("data-testid") === target.id);
  }}
  return applyIndex(matches, target.index);
}};
const resolveQueryAll = query => {{
  if (query.kind === "snapshot_path") {{
    const element = resolvePath(query.path);
    return element ? [element] : [];
  }}
  if (query.kind === "locator") return resolveLocator(query.target);
  return [];
}};
const matches = resolveQueryAll(query);"#
    ))
}

const fn action_result(sequence: u64, action: BrowserActionName) -> BrowserActionResult {
    BrowserActionResult::Action {
        sequence,
        action,
        executed: true,
        outcome: None,
    }
}

const fn requires_action_completion(action: BrowserActionName) -> bool {
    matches!(
        action,
        BrowserActionName::Open
            | BrowserActionName::Reload
            | BrowserActionName::Click
            | BrowserActionName::Fill
            | BrowserActionName::Press
            | BrowserActionName::Hover
            | BrowserActionName::MouseMove
            | BrowserActionName::MouseDown
            | BrowserActionName::MouseUp
            | BrowserActionName::MouseWheel
            | BrowserActionName::TouchTap
            | BrowserActionName::TouchSwipe
            | BrowserActionName::KeyboardDown
            | BrowserActionName::KeyboardUp
            | BrowserActionName::InsertText
            | BrowserActionName::Scroll
            | BrowserActionName::SelectOption
            | BrowserActionName::SetChecked
            | BrowserActionName::Drag
            | BrowserActionName::UploadFiles
            | BrowserActionName::SetViewport
            | BrowserActionName::GoBack
            | BrowserActionName::GoForward
            | BrowserActionName::HandleDialog
    )
}

const fn captures_action_outcome(action: BrowserActionName) -> bool {
    requires_action_completion(action)
        || matches!(
            action,
            BrowserActionName::ForcePseudoState
                | BrowserActionName::NewTab
                | BrowserActionName::SelectTab
                | BrowserActionName::CloseTab
        )
}

async fn capture_action_outcome(
    session: &mut Session,
    observation: &interaction::ActionObservation,
    network: crate::BrowserActionNetwork,
    after_action: BrowserAfterAction,
) -> Result<BrowserActionOutcome, BrowserError> {
    let url = session.page.url().await?.unwrap_or_default();
    let dialog = session
        .diagnostics
        .lock()
        .map_err(|_| BrowserError::DiagnosticsUnavailable)?
        .dialog
        .clone();
    let (title, ready_state) = if dialog.is_some() {
        (None, BrowserDocumentReadyState::Unavailable)
    } else {
        let state = evaluate_typed::<PageStateWire>(
            &session.page,
            "({ title: document.title, readyState: document.readyState })",
        )
        .await?;
        (
            Some(state.title),
            match state.ready_state.as_str() {
                "loading" => BrowserDocumentReadyState::Loading,
                "interactive" => BrowserDocumentReadyState::Interactive,
                "complete" => BrowserDocumentReadyState::Complete,
                _ => BrowserDocumentReadyState::Unavailable,
            },
        )
    };
    let (mut console, mut errors, downloads) = {
        let diagnostics = session
            .diagnostics
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
        (
            diagnostics
                .console
                .iter()
                .filter(|entry| entry.sequence > observation.console_after())
                .cloned()
                .collect::<Vec<_>>(),
            diagnostics
                .errors
                .iter()
                .filter(|error| error.sequence > observation.errors_after())
                .cloned()
                .collect::<Vec<_>>(),
            observation.new_downloads(&diagnostics),
        )
    };
    session.source_maps.symbolicate_console(&mut console)?;
    session.source_maps.symbolicate_errors(&mut errors)?;
    let snapshot = if after_action == BrowserAfterAction::Snapshot && dialog.is_none() {
        Some(
            match session.snapshot(true, true, Some(20), None, true).await {
                Ok(snapshot) => BrowserPostActionSnapshot::Captured {
                    origin: snapshot.origin,
                    snapshot: snapshot.snapshot,
                    refs: snapshot.refs,
                },
                Err(error) => BrowserPostActionSnapshot::Unavailable {
                    error: error.to_string(),
                },
            },
        )
    } else {
        None
    };
    Ok(BrowserActionOutcome {
        page: BrowserPageState {
            url,
            title,
            ready_state,
        },
        network,
        console,
        errors,
        dialog,
        downloads,
        snapshot,
    })
}

fn diagnostic_limit(limit: Option<u16>) -> usize {
    limit
        .map_or(DEFAULT_DIAGNOSTIC_RESULTS, usize::from)
        .min(MAX_DIAGNOSTIC_RESULTS)
}

fn validate_url(value: &str) -> Result<(), BrowserError> {
    let url = Url::parse(value)?;
    match url.scheme() {
        "http" | "https" | "data" | "about" => Ok(()),
        scheme => Err(BrowserError::UnsupportedUrlScheme {
            scheme: scheme.to_owned(),
        }),
    }
}

fn build_config(builder: BrowserConfigBuilder) -> Result<BrowserConfig, BrowserError> {
    builder
        .build()
        .map_err(|message| BrowserError::Configuration { message })
}

fn profile_launch_config(builder: BrowserConfigBuilder) -> BrowserConfigBuilder {
    // Chromiumoxide's Puppeteer-derived defaults select `password-store=basic`
    // and `use-mock-keychain`. Those are appropriate for an empty automation
    // profile but make real Brave cookie ciphertext undecryptable. Keep the
    // other launch invariants while deliberately using the OS keychain.
    builder
        .disable_default_args()
        .arg("disable-background-networking")
        .arg(("enable-features", "NetworkService,NetworkServiceInProcess"))
        .arg("disable-background-timer-throttling")
        .arg("disable-backgrounding-occluded-windows")
        .arg("disable-breakpad")
        .arg("disable-client-side-phishing-detection")
        .arg("disable-component-extensions-with-background-pages")
        .arg("disable-default-apps")
        .arg("disable-dev-shm-usage")
        .arg(("disable-features", "TranslateUI"))
        .arg("disable-hang-monitor")
        .arg("disable-ipc-flooding-protection")
        .arg("disable-popup-blocking")
        .arg("disable-prompt-on-repost")
        .arg("disable-renderer-backgrounding")
        .arg("disable-sync")
        .arg(("force-color-profile", "srgb"))
        .arg("metrics-recording-only")
        .arg("no-first-run")
        .arg("enable-automation")
        .arg(("enable-blink-features", "IdleDetection"))
        .arg(("lang", "en_US"))
}

struct SnapshotData {
    origin: String,
    snapshot: String,
    refs: BTreeMap<String, BrowserElementReference>,
}

struct SnapshotFrame {
    context_id: Option<ExecutionContextId>,
    frame_id: Option<String>,
    url: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GateSignals {
    title: String,
    body_text: String,
    visible_captcha: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReactEventsRequest {
    after: u64,
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReactEventsWire {
    protocol_version: u32,
    status: BrowserReactStatus,
    events: Vec<BrowserReactEvent>,
    total: usize,
    dropped: u64,
    last_sequence: Option<u64>,
    has_more: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ElementContextWire {
    protocol_version: u32,
    context: BrowserElementContext,
}

fn classify_gate(signals: &GateSignals) -> BrowserGate {
    let title = signals.title.to_lowercase();
    let body = signals.body_text.to_lowercase();
    let evidence = || {
        signals
            .body_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(240)
            .collect()
    };
    if signals.visible_captcha
        && (body.contains("captcha")
            || body.contains("verify you are human")
            || body.contains("i'm not a robot")
            || body.contains("i am not a robot"))
    {
        BrowserGate::Captcha {
            evidence: evidence(),
        }
    } else if body.contains("please wait, you will be forwarded")
        || body.contains("checking your browser")
        || title.contains("just a moment")
        || body.contains("javascript is required")
    {
        BrowserGate::JsChallenge {
            evidence: evidence(),
        }
    } else if title.contains("access denied")
        || body.contains("access denied")
        || body.contains("request blocked")
        || body.contains("you have been blocked")
    {
        BrowserGate::AccessDenied {
            evidence: evidence(),
        }
    } else {
        BrowserGate::Clear
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotOptions<'a> {
    interactive: bool,
    compact: bool,
    depth: Option<u32>,
    selector: Option<&'a str>,
    include_urls: bool,
    reference_start: usize,
}

#[derive(Deserialize, Serialize)]
struct SnapshotWire {
    origin: String,
    snapshot: String,
    refs: Vec<SnapshotRefWire>,
}

#[derive(Deserialize, Serialize)]
struct SnapshotRefWire {
    reference: String,
    #[serde(rename = "selectorPath")]
    selector_path: Vec<String>,
    role: String,
    name: String,
    disabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBuildError {
    #[error("failed to prepare the browser's private runtime")]
    Io(#[from] io::Error),
    #[error("browser configuration is invalid: {message}")]
    Configuration { message: String },
    #[error(transparent)]
    BraveSession(#[from] BraveSessionError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("Chromium DevTools operation failed: {0}")]
    Cdp(#[from] CdpError),
    #[error("browser filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("browser JSON boundary failed")]
    Json(#[from] serde_json::Error),
    #[error("CrUX request failed: {source}")]
    CruxRequest {
        #[source]
        source: reqwest::Error,
    },
    #[error("CrUX response exceeded the {maximum}-byte limit")]
    CruxResponseTooLarge { maximum: usize },
    #[error("browser image artifact failed")]
    Image(#[from] image::ImageError),
    #[error("browser image artifact is {bytes} bytes, above the {maximum}-byte limit")]
    ImageArtifactTooLarge { bytes: u64, maximum: u64 },
    #[error("browser image artifact contains {pixels} pixels, above the {maximum}-pixel limit")]
    ImageArtifactPixels { pixels: u64, maximum: u64 },
    #[error("browser PDF is {bytes} bytes, above the {maximum}-byte limit")]
    PdfTooLarge { bytes: usize, maximum: usize },
    #[error(transparent)]
    BraveSession(#[from] BraveSessionError),
    #[error("browser configuration is invalid: {message}")]
    Configuration { message: String },
    #[error("browser action input is {bytes} bytes, above the {maximum}-byte limit")]
    ActionInputTooLarge { bytes: u64, maximum: u64 },
    #[error("browser action result is {bytes} bytes, above the {maximum}-byte limit")]
    ActionValueTooLarge { bytes: u64, maximum: u64 },
    #[error("browser navigation does not allow the `{scheme}` URL scheme")]
    UnsupportedUrlScheme { scheme: String },
    #[error("browser navigation requires an absolute URL")]
    Url(#[from] url::ParseError),
    #[error("browser session does not allow navigation to {url}")]
    OriginNotAllowed { url: Url },
    #[error("browser reference `{reference}` is not present in the latest snapshot")]
    UnknownReference { reference: String },
    #[error("strict selector `{selector}` matched {count} elements")]
    StrictSelectorViolation { selector: String, count: usize },
    #[error("selector `{selector}` is not actionable: {reason}")]
    Actionability { selector: String, reason: String },
    #[error("selector `{selector}` did not become actionable before timeout: {reason}")]
    ActionabilityTimeout { selector: String, reason: String },
    #[error("browser click count must be between 1 and 3, received {click_count}")]
    InvalidClickCount { click_count: u8 },
    #[error(
        "browser viewport {width}x{height} is invalid; each dimension must be within 1..={maximum}"
    )]
    InvalidViewport {
        width: u32,
        height: u32,
        maximum: u32,
    },
    #[error(
        "browser device scale factor must be finite and greater than zero, received {device_scale_factor}"
    )]
    InvalidDeviceScaleFactor { device_scale_factor: f64 },
    #[error("browser element capture cannot be combined with a full-page screenshot")]
    TargetWithFullPage,
    #[error("browser session could not be initialized")]
    SessionUnavailable,
    #[error("browser session is closed")]
    Closed,
    #[error("browser diagnostics collector is unavailable")]
    DiagnosticsUnavailable,
    #[error("browser source-map registry is unavailable")]
    SourceMapsUnavailable,
    #[error("browser source map is {bytes} bytes, above the {maximum}-byte limit")]
    SourceMapTooLarge { bytes: usize, maximum: usize },
    #[error("browser source map could not be parsed")]
    SourceMapParse(#[source] sourcemap::Error),
    #[error("browser source-map data URL is malformed")]
    InvalidSourceMapDataUrl,
    #[error("browser supports only base64 source-map data URLs")]
    UnsupportedSourceMapDataUrl,
    #[error("browser source map was not valid base64")]
    SourceMapBase64(#[source] base64::DecodeError),
    #[error("browser could not load source map {url}: {message}")]
    SourceMapNetwork { url: Url, message: String },
    #[error("React diagnostics were configured but are unavailable in the active document")]
    ReactDiagnosticsUnavailable,
    #[error("React diagnostics protocol mismatch: expected version {expected}, received {actual}")]
    ReactDiagnosticsProtocol { expected: u32, actual: u32 },
    #[error("Chromium returned an invalid DOM snapshot: {message}")]
    InvalidDomSnapshot { message: String },
    #[error(
        "browser semantic snapshot is {bytes} bytes with {refs} references; limits are {maximum_bytes} bytes and {maximum_refs} references"
    )]
    SnapshotTooLarge {
        bytes: u64,
        refs: usize,
        maximum_bytes: usize,
        maximum_refs: usize,
    },
    #[error(
        "browser DOM snapshot contains {nodes} nodes and {string_bytes} string bytes; limits are {maximum_nodes} nodes and {maximum_string_bytes} string bytes"
    )]
    DomSnapshotTooLarge {
        nodes: usize,
        string_bytes: usize,
        maximum_nodes: usize,
        maximum_string_bytes: usize,
    },
    #[error(
        "browser DOM snapshot requested {count} computed styles, above the {maximum}-style limit"
    )]
    DomComputedStylesLimit { count: usize, maximum: usize },
    #[error("network body is unavailable for request `{request_id}`")]
    NetworkBodyUnavailable { request_id: String },
    #[error("network body for request `{request_id}` did not arrive before the deadline")]
    NetworkBodyTimeout { request_id: String },
    #[error(
        "network body for request `{request_id}` is {bytes} bytes, above the {maximum}-byte limit"
    )]
    NetworkBodyTooLarge {
        request_id: String,
        bytes: usize,
        maximum: usize,
    },
    #[error("network observer failed: {message}")]
    NetworkObserver { message: String },
    #[error("browser network route is invalid: {message}")]
    InvalidNetworkRoute { message: String },
    #[error("this action is not yet supported for an element in a child frame")]
    FrameElementRequiresEvaluation,
    #[error("browser frame `{frame_id}` is not present in the active page")]
    UnknownFrame { frame_id: String },
    #[error("browser tab `{tab_id}` is not open")]
    UnknownTab { tab_id: String },
    #[error("the browser must retain at least one tab")]
    LastTab,
    #[error("browser upload actions require a configured file root")]
    FileRootNotConfigured,
    #[error("browser file path escapes the configured file root: {path:?}")]
    FileOutsideRoot { path: PathBuf },
    #[error("browser upload requested {count} files, above the {maximum}-file limit")]
    UploadFileLimit { count: usize, maximum: usize },
    #[error("browser upload path is not a regular file: {path:?}")]
    UploadFileInvalid { path: PathBuf },
    #[error("browser upload file {path:?} is {bytes} bytes, above the {maximum}-byte limit")]
    UploadFileTooLarge {
        path: PathBuf,
        bytes: u64,
        maximum: u64,
    },
    #[error("browser visual trace is already active")]
    VisualTraceActive,
    #[error("browser visual trace is not active")]
    VisualTraceNotActive,
    #[error("browser session trace is already active")]
    SessionTraceActive,
    #[error("browser session trace is not active")]
    SessionTraceNotActive,
    #[error("browser trace artifact path has no file name: {path:?}")]
    InvalidTraceArtifactPath { path: PathBuf },
    #[error("Lighthouse audit requires a harness-configured executable")]
    LighthouseNotConfigured,
    #[error("Lighthouse cannot attach to the browser DevTools endpoint `{endpoint}`")]
    LighthouseEndpoint { endpoint: String },
    #[error("Lighthouse did not finish before its deadline")]
    LighthouseTimeout,
    #[error("Lighthouse exited with status {status:?}; diagnostics are in {stderr_path:?}")]
    LighthouseFailed {
        status: Option<i32>,
        stderr_path: PathBuf,
    },
    #[error("Lighthouse report is {bytes} bytes, above the {maximum}-byte limit")]
    LighthouseReportTooLarge { bytes: u64, maximum: u64 },
    #[error("Lighthouse report read task failed")]
    LighthouseReadTask(#[source] tokio::task::JoinError),
    #[error("CrUX field data requires a harness-configured client")]
    CruxNotConfigured,
    #[error("CrUX field data requires an HTTP(S) page URL, received `{url}`")]
    CruxUrl { url: String },
    #[error("browser visual artifact `{artifact_id}` is unavailable")]
    VisualArtifactUnavailable { artifact_id: String },
    #[error("browser retains at most {maximum} visual artifacts per session")]
    VisualArtifactLimit { maximum: usize },
    #[error("browser performance trace is already active")]
    PerformanceTraceActive,
    #[error("browser performance trace is not active")]
    PerformanceTraceNotActive,
    #[error("browser performance trace did not flush before its deadline")]
    PerformanceTraceTimeout,
    #[error("browser performance trace collector stopped unexpectedly")]
    PerformanceTraceUnavailable,
    #[error("browser performance trace lost events ({dropped} locally dropped)")]
    PerformanceTraceDataLoss { dropped: u64 },
    #[error("a browser CPU profile is already active")]
    CpuProfileActive,
    #[error("a browser CPU profile is not active")]
    CpuProfileNotActive,
    #[error("browser JavaScript coverage is already active")]
    CoverageActive,
    #[error("browser JavaScript coverage is not active")]
    CoverageNotActive,
    #[error("browser heap snapshot did not finish before its deadline")]
    HeapSnapshotTimeout,
    #[error("browser heap snapshot is {bytes} bytes, above the {maximum}-byte limit")]
    HeapSnapshotTooLarge { bytes: u64, maximum: u64 },
    #[error("browser heap snapshot collector failed")]
    HeapCollectorTask(#[source] tokio::task::JoinError),
    #[error("browser heap analysis task failed")]
    HeapAnalysisTask(#[source] tokio::task::JoinError),
    #[error("browser heap snapshot could not be parsed")]
    HeapSnapshotParse(#[source] serde_json::Error),
    #[error("browser heap snapshot has an invalid format: {message}")]
    HeapSnapshotFormat { message: String },
    #[error("browser heap class at node {index} disappeared during analysis")]
    HeapClassUnavailable { index: usize },
    #[error("browser heap snapshot `{artifact_id}` is unavailable")]
    HeapSnapshotUnavailable { artifact_id: String },
    #[error("browser retains at most {maximum} heap snapshots per session")]
    HeapSnapshotLimit { maximum: usize },
    #[error("browser heap node {node_id} is unavailable in snapshot `{artifact_id}`")]
    HeapNodeUnavailable { artifact_id: String, node_id: u64 },
    #[error("a browser video recording is already active")]
    VideoActive,
    #[error("a browser video recording is not active")]
    VideoNotActive,
    #[error("browser video recording state is unavailable")]
    VideoUnavailable,
    #[error("failed to start the browser video encoder")]
    VideoEncoderStart(#[source] io::Error),
    #[error("browser video encoder did not expose standard input")]
    VideoEncoderStdin,
    #[error("browser video frame was not valid base64")]
    VideoFrameDecode(#[source] base64::DecodeError),
    #[error("browser video encoder failed with status {status:?}: {stderr}")]
    VideoEncoderFailed { status: Option<i32>, stderr: String },
    #[error("browser video encoder did not stop before its deadline")]
    VideoStopTimeout,
    #[error("browser video task failed")]
    VideoTask(#[source] tokio::task::JoinError),
    #[error("browser has no pending JavaScript dialog")]
    DialogNotPending,
    #[error("this browser was not configured with a virtual authenticator")]
    VirtualAuthenticatorNotConfigured,
    #[error("model-controlled passkeys require a configured virtual credential store")]
    VirtualCredentialStoreNotConfigured,
    #[error("virtual credential `{credential_id}` is not present in the persisted store")]
    UnknownVirtualCredential { credential_id: String },
    #[error(
        "virtual credential `{credential_id}` exists for multiple relying parties; provide relying_party_id"
    )]
    AmbiguousVirtualCredential { credential_id: String },
    #[error("virtual credential store {} is invalid: {message}", path.display())]
    VirtualCredentialStore { path: PathBuf, message: String },
    #[error("this browser was not configured with an authenticated Brave session")]
    BraveSessionNotConfigured,
    #[error("the virtual authenticator is not ready; navigate to a page first")]
    VirtualAuthenticatorNotReady,
    #[error("selector `{selector}` was not found before the browser timeout")]
    SelectorTimeout { selector: String },
    #[error("selector `{selector}` did not reach state {state:?} before the browser timeout")]
    SelectorStateTimeout {
        selector: String,
        state: crate::BrowserWaitForSelectorState,
    },
    #[error(
        "text `{text}` did not reach the requested visibility before timeout (hidden={hidden})"
    )]
    TextTimeout { text: String, hidden: bool },
    #[error("the browser URL did not contain `{expected}` before the browser timeout")]
    UrlTimeout { expected: String },
    #[error("the document did not reach load state {state:?} before the browser timeout")]
    LoadStateTimeout { state: crate::BrowserLoadState },
    #[error("the browser wait expression did not become truthy before the browser timeout")]
    FunctionTimeout,
    #[error("the requested browser history entry does not exist")]
    HistoryEntryUnavailable,
    #[error("browser does not recognize the keyboard key `{key}`")]
    UnknownKey { key: String },
    #[error(
        "browser source locations are one-based and must fit Chromium integers (line={line_number}, column={column_number})"
    )]
    InvalidSourceLocation {
        line_number: u64,
        column_number: u64,
    },
    #[error(
        "browser navigation to {url} did not produce a usable document within {milliseconds}ms"
    )]
    NavigationTimeout { url: String, milliseconds: u128 },
    #[error("browser wait of {milliseconds}ms exceeds the maximum of {maximum}ms")]
    WaitTooLong { milliseconds: u64, maximum: u128 },
    #[error("browser JavaScript evaluation exceeded the {maximum:?} deadline")]
    EvaluationTimeout { maximum: Duration },
}

const DETECT_GATE_SCRIPT: &str = r#"function() {
  const visible = (element) => {
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none" &&
      style.visibility !== "hidden" &&
      Number(style.opacity || 1) !== 0 &&
      rect.width > 0 &&
      rect.height > 0;
  };
  const captchaSelectors = [
    "iframe[src*='recaptcha']",
    "iframe[src*='hcaptcha']",
    "iframe[src*='turnstile']",
    "[data-sitekey]",
    "[class*='captcha' i]",
    "[id*='captcha' i]"
  ];
  return {
    title: document.title,
    bodyText: (document.body?.innerText || "").slice(0, 4000),
    visibleCaptcha: captchaSelectors.some((selector) =>
      Array.from(document.querySelectorAll(selector)).some(visible)
    )
  };
}"#;

const SNAPSHOT_SCRIPT: &str = r##"function(options) {
  const queryDeep = (root, selector) => {
    const direct = root.querySelector(selector);
    if (direct) return direct;
    for (const host of root.querySelectorAll("*")) {
      if (!host.shadowRoot) continue;
      const nested = queryDeep(host.shadowRoot, selector);
      if (nested) return nested;
    }
    return null;
  };
  const scope = options.selector
    ? queryDeep(document, options.selector)
    : document.body || document.documentElement;
  if (!scope) throw new Error("snapshot selector did not match an element");

  const roleOf = (element) => {
    const explicit = element.getAttribute("role");
    if (explicit) return explicit;
    const tag = element.tagName.toLowerCase();
    const type = (element.getAttribute("type") || "").toLowerCase();
    if (tag === "a" && element.hasAttribute("href")) return "link";
    if (tag === "button") return "button";
    if (tag === "textarea") return "textbox";
    if (tag === "select") return "combobox";
    if (tag === "input") {
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "range") return "slider";
      if (type === "button" || type === "submit" || type === "reset") return "button";
      return "textbox";
    }
    if (/^h[1-6]$/.test(tag)) return "heading";
    if (tag === "main") return "main";
    if (tag === "nav") return "navigation";
    if (tag === "article") return "article";
    if (tag === "li") return "listitem";
    if (element.isContentEditable) return "textbox";
    return "generic";
  };
  const normalizedText = (value) =>
    (value || "").trim().replace(/\s+/g, " ").slice(0, 160);
  const nameOf = (element) => {
    const root = element.getRootNode();
    const labelledBy = (element.getAttribute("aria-labelledby") || "")
      .split(/\s+/)
      .filter(Boolean)
      .map((id) => typeof root.getElementById === "function" ? root.getElementById(id) : null)
      .filter(Boolean)
      .map((label) => normalizedText(label.innerText || label.textContent))
      .filter(Boolean)
      .join(" ");
    const labels = element.labels
      ? Array.from(element.labels)
          .map((label) => normalizedText(label.innerText || label.textContent))
          .filter(Boolean)
          .join(" ")
      : "";
    return element.getAttribute("aria-label") ||
      labelledBy ||
      labels ||
      element.getAttribute("alt") ||
      element.getAttribute("title") ||
      element.getAttribute("placeholder") ||
      normalizedText(element.innerText || element.textContent);
  };
  const selectorWithin = (root, element) => {
    if (element.id) {
      const selector = "#" + CSS.escape(element.id);
      if (root.querySelectorAll(selector).length === 1) return selector;
    }
    const parts = [];
    let current = element;
    while (current && current.nodeType === Node.ELEMENT_NODE) {
      const tag = current.tagName.toLowerCase();
      let index = 1;
      for (
        let sibling = current.previousElementSibling;
        sibling;
        sibling = sibling.previousElementSibling
      ) {
        if (sibling.tagName === current.tagName) index++;
      }
      parts.unshift(`${tag}:nth-of-type(${index})`);
      current = current.parentElement;
    }
    return parts.join(" > ");
  };
  const selectorPathFor = (element) => {
    const path = [];
    let current = element;
    while (current) {
      const root = current.getRootNode();
      path.unshift(selectorWithin(root, current));
      current = root instanceof ShadowRoot ? root.host : null;
    }
    return path;
  };
  const interactiveRoles = new Set([
    "button", "link", "textbox", "checkbox", "radio", "combobox", "listbox",
    "menuitem", "option", "searchbox", "slider", "spinbutton", "switch", "tab", "treeitem"
  ]);
  const contentRoles = new Set(["heading", "main", "navigation", "article", "listitem"]);
  const candidates = [];
  const visit = (element, depth) => {
    if (!(element instanceof HTMLElement)) return;
    candidates.push({ element, depth });
    for (const child of element.children) visit(child, depth + 1);
    if (element.shadowRoot) {
      for (const child of element.shadowRoot.children) visit(child, depth + 1);
    }
  };
  visit(scope, 0);

  const selected = [];
  for (const candidate of candidates) {
    const { element, depth } = candidate;
    if (options.depth !== null && options.depth !== undefined && depth > options.depth) continue;
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    if (
      style.display === "none" ||
      style.visibility === "hidden" ||
      Number(style.opacity || 1) === 0 ||
      rect.width <= 0 ||
      rect.height <= 0
    ) continue;
    const role = roleOf(element);
    const parent = element.parentElement ||
      (element.getRootNode() instanceof ShadowRoot ? element.getRootNode().host : null);
    const pointerBoundary = style.cursor === "pointer" &&
      (!parent || getComputedStyle(parent).cursor !== "pointer");
    const actionable = interactiveRoles.has(role) ||
      element.onclick !== null ||
      element.hasAttribute("tabindex") ||
      pointerBoundary;
    if (options.interactive && !actionable && !contentRoles.has(role)) continue;
    const name = nameOf(element);
    const relevant = actionable || (contentRoles.has(role) && name.length > 0);
    if (options.interactive && !relevant) continue;
    if (!options.interactive && role === "generic" && !name) continue;
    selected.push({ element, role, name, depth, relevant, actionable });
  }

  const refs = [];
  const lines = [];
  for (const item of selected) {
    const reference = item.actionable ? `e${options.referenceStart + refs.length + 1}` : null;
    if (reference) {
      refs.push({
        reference,
        selectorPath: selectorPathFor(item.element),
        role: item.role,
        name: item.name,
        disabled: Boolean(
          item.element.disabled ||
          item.element.getAttribute("aria-disabled") === "true"
        )
      });
    }
    const name = item.name ? ` "${item.name.replaceAll('"', '\\"')}"` : "";
    const ref = reference ? ` [ref=${reference}]` : "";
    const href = options.includeUrls && item.element.href ? ` [url=${item.element.href}]` : "";
    lines.push(`${"  ".repeat(item.depth)}- ${item.role}${name}${ref}${href}`);
  }
  let snapshot = lines.join("\n");
  if (options.compact) snapshot = snapshot.replace(/\n(?:\s*- generic)+/g, "");
  if (!snapshot) snapshot = options.interactive ? "(no interactive elements)" : "(empty page)";
  return {
    origin: location.origin === "null" ? location.href : location.origin,
    snapshot,
    refs
  };
}"##;

const INSTALL_ANNOTATIONS_SCRIPT: &str = r#"function(refs) {
  document.getElementById("__nanocodex_annotations")?.remove();
  const root = document.createElement("div");
  root.id = "__nanocodex_annotations";
  root.style.cssText = "position:fixed;inset:0;pointer-events:none;z-index:2147483647";
  for (const [reference, path] of Object.entries(refs)) {
    let current = document;
    let element = null;
    let resolved = true;
    for (let index = 0; index < path.length; index++) {
      element = current.querySelector(path[index]);
      if (!element) {
        resolved = false;
        break;
      }
      if (index + 1 < path.length) {
        current = element.shadowRoot;
        if (!current) {
          resolved = false;
          break;
        }
      }
    }
    if (!resolved || !element) continue;
    const rect = element.getBoundingClientRect();
    const label = document.createElement("div");
    label.textContent = reference;
    label.style.cssText = [
      "position:absolute",
      `left:${Math.max(0, rect.left)}px`,
      `top:${Math.max(0, rect.top)}px`,
      "padding:1px 4px",
      "background:#ff2d55",
      "color:white",
      "font:12px/16px monospace",
      "border-radius:3px"
    ].join(";");
    root.appendChild(label);
  }
  document.documentElement.appendChild(root);
  return true;
}"#;

const REMOVE_ANNOTATIONS_SCRIPT: &str =
    r#"document.getElementById("__nanocodex_annotations")?.remove(); true"#;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use chromiumoxide::cdp::browser_protocol::network::{
        Cookie, CookieParam, CookiePriority, CookieSourceScheme, TimeSinceEpoch,
    };
    use futures_util::StreamExt;

    use super::{
        BrowserConfig, Chromium, Diagnostics, GateSignals, MAX_ACTION_INPUT_BYTES,
        MAX_CONSOLE_ENTRIES, MAX_DIAGNOSTIC_TEXT_BYTES, MAX_NETWORK_REQUESTS, NetworkSource,
        allowed_cookie_params, build_config, classify_gate, close_chromium, cookie_param,
        diagnostic_limit, profile_launch_config, trace_browser_configuration, validate_url,
    };
    use crate::{
        BraveSession, Browser, BrowserAction, BrowserActionResult, BrowserConsoleEntry,
        BrowserContext, BrowserCookie, BrowserCruxClient, BrowserError, BrowserGate,
        BrowserNetworkRequest, BrowserOriginStorage, BrowserStorageState,
    };

    #[derive(Clone, Default)]
    struct TraceLog(Arc<StdMutex<Vec<u8>>>);

    struct TraceWriter(Arc<StdMutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceLog {
        type Writer = TraceWriter;

        fn make_writer(&'a self) -> Self::Writer {
            TraceWriter(Arc::clone(&self.0))
        }
    }

    impl std::io::Write for TraceWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn full_fidelity_trace_retains_credential_bearing_configuration() {
        let storage_state = BrowserStorageState {
            cookies: vec![BrowserCookie {
                name: "session".to_owned(),
                value: "cookie-secret".to_owned(),
                domain: "example.com".to_owned(),
                path: "/".to_owned(),
                expires_epoch_seconds: None,
                http_only: true,
                secure: true,
                same_site: None,
            }],
            origins: vec![BrowserOriginStorage {
                origin: "https://example.com".to_owned(),
                local_storage: [("token".to_owned(), "storage-secret".to_owned())]
                    .into_iter()
                    .collect(),
                session_storage: std::collections::BTreeMap::default(),
            }],
        };
        let browser = Browser::builder()
            .context(
                BrowserContext::default()
                    .extra_header("x-private", "header-secret")
                    .http_credentials("trace-user", "password-secret")
                    .init_script("globalThis.traceSecret = 'script-secret';"),
            )
            .storage_state(storage_state)
            .crux_client(BrowserCruxClient::new("crux-secret"))
            .build()
            .unwrap();
        let logs = TraceLog::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(logs.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::callsite::rebuild_interest_cache();
        tracing::dispatcher::with_default(&dispatch, || {
            trace_browser_configuration(&browser.inner);
        });
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();

        for expected in [
            "header-secret",
            "password-secret",
            "script-secret",
            "cookie-secret",
            "storage-secret",
            "crux-secret",
        ] {
            assert!(logs.contains(expected), "trace omitted {expected}: {logs}");
        }
    }

    #[test]
    fn navigation_accepts_web_content_but_rejects_host_files() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("http://127.0.0.1:3000").is_ok());
        assert!(validate_url("data:text/html,<h1>test</h1>").is_ok());
        assert!(validate_url("about:blank").is_ok());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("/etc/passwd").is_err());
    }

    #[test]
    fn diagnostics_keep_a_bounded_recent_window() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..MAX_CONSOLE_ENTRIES + 2 {
            diagnostics.push_console(BrowserConsoleEntry {
                sequence: 0,
                level: "debug".to_owned(),
                text: index.to_string(),
                stack: Vec::new(),
            });
        }
        assert_eq!(diagnostics.console.len(), MAX_CONSOLE_ENTRIES);
        assert_eq!(diagnostics.dropped_console, 2);
        assert_eq!(
            diagnostics.console.front().map(|entry| entry.text.as_str()),
            Some("2")
        );

        for index in 0..MAX_NETWORK_REQUESTS + 2 {
            diagnostics.push_request(
                &index.to_string(),
                NetworkSource::Page,
                0.0,
                BrowserNetworkRequest {
                    url: format!("https://example.com/{index}"),
                    method: "GET".to_owned(),
                    resource_type: "Fetch".to_owned(),
                    ..BrowserNetworkRequest::default()
                },
            );
        }
        assert_eq!(diagnostics.requests.len(), MAX_NETWORK_REQUESTS);
        assert_eq!(diagnostics.dropped_requests, 2);
        assert!(diagnostics.request_entry_mut("0").is_none());
        diagnostics
            .request_entry_mut(&(MAX_NETWORK_REQUESTS + 1).to_string())
            .expect("newest request is retained")
            .request
            .status = Some(200);
        assert_eq!(
            diagnostics
                .requests
                .back()
                .map(|entry| entry.request.status),
            Some(Some(200))
        );
        assert_eq!(diagnostic_limit(None), 200);
        assert_eq!(diagnostic_limit(Some(u16::MAX)), 1_000);

        diagnostics.push_console(BrowserConsoleEntry {
            sequence: 0,
            level: "log".to_owned(),
            text: "x".repeat(MAX_DIAGNOSTIC_TEXT_BYTES + 1),
            stack: Vec::new(),
        });
        assert_eq!(diagnostics.console.len(), MAX_CONSOLE_ENTRIES);
        assert_eq!(diagnostics.dropped_console, 3);
        assert_eq!(
            diagnostics.next_console_sequence,
            u64::try_from(MAX_CONSOLE_ENTRIES).unwrap_or(u64::MAX) + 3
        );
    }

    #[tokio::test]
    async fn oversized_action_is_rejected_before_chromium_launch() {
        let browser = Browser::new().expect("browser handle");
        let error = browser
            .execute(BrowserAction::Evaluate {
                expression: "x".repeat(
                    usize::try_from(MAX_ACTION_INPUT_BYTES).expect("test limit fits usize") + 1,
                ),
            })
            .await
            .expect_err("oversized action must fail");
        assert!(matches!(error, BrowserError::ActionInputTooLarge { .. }));
    }

    #[test]
    fn diagnostics_reconcile_completion_that_arrives_before_its_request() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.finish_page_request("fast", 2.0, 17.0);
        diagnostics.push_request(
            "fast",
            NetworkSource::Page,
            1.0,
            BrowserNetworkRequest::default(),
        );

        let request = &diagnostics
            .request_entry_mut("fast")
            .expect("deferred completion is applied")
            .request;
        assert!(request.completed);
        assert_eq!(request.duration_ms, Some(1_000));
        assert_eq!(request.encoded_data_length, Some(17));
        assert!(diagnostics.pending_page_lifecycle.is_empty());
    }

    #[test]
    fn visible_forwarding_interstitial_is_a_javascript_challenge() {
        let gate = classify_gate(&GateSignals {
            title: "Expected page title".to_owned(),
            body_text: "Please wait, you will be forwarded to the requested page".to_owned(),
            visible_captcha: false,
        });
        assert!(matches!(gate, BrowserGate::JsChallenge { .. }));
    }

    #[test]
    fn invisible_captcha_support_code_does_not_create_a_gate() {
        let gate = classify_gate(&GateSignals {
            title: "Real content".to_owned(),
            body_text: "The requested content".to_owned(),
            visible_captcha: false,
        });
        assert_eq!(gate, BrowserGate::Clear);
    }

    #[test]
    fn remote_cookie_mapping_preserves_request_semantics() {
        let cookie = test_cookie(".example.com", false, None);
        let parameter = cookie_param(cookie).expect("ordinary cookie should be portable");

        assert_eq!(parameter.name, "session");
        assert_eq!(parameter.value, "secret");
        assert_eq!(parameter.domain.as_deref(), Some(".example.com"));
        assert_eq!(parameter.path.as_deref(), Some("/console"));
        assert_eq!(parameter.secure, Some(true));
        assert_eq!(parameter.http_only, Some(true));
        assert_eq!(
            parameter.expires.as_ref().map(|value| *value.inner()),
            Some(1_900_000_000.0)
        );
        assert_eq!(parameter.priority, Some(CookiePriority::High));
        assert_eq!(parameter.source_scheme, Some(CookieSourceScheme::Secure));
        assert_eq!(parameter.source_port, Some(443));
    }

    #[test]
    fn remote_cookie_mapping_filters_domains_and_opaque_partitions() {
        let cookies = vec![
            test_cookie(".example.com", false, None),
            test_cookie("sibling.example.net", false, None),
            test_cookie("console.example.com", false, Some(true)),
        ];
        let allowed = [url::Url::parse("https://console.example.com").expect("valid test origin")];

        let parameters = allowed_cookie_params(cookies, Some(&allowed));

        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].domain.as_deref(), Some(".example.com"));
    }

    #[test]
    fn remote_cookie_mapping_can_preserve_every_domain() {
        let cookies = vec![
            test_cookie(".example.com", false, None),
            test_cookie("sibling.example.net", false, None),
            test_cookie("console.example.com", false, Some(true)),
        ];

        let parameters = allowed_cookie_params(cookies, None);

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].domain.as_deref(), Some(".example.com"));
        assert_eq!(parameters[1].domain.as_deref(), Some("sibling.example.net"));
    }

    #[tokio::test]
    #[ignore = "requires the standard Brave installation and internet access"]
    async fn brave_cookie_broker_seeds_a_remote_browser() -> Result<(), Box<dyn std::error::Error>>
    {
        let executable = BraveSession::standard()?.executable().to_path_buf();
        let directory = tempfile::tempdir()?;
        let source_profile = directory.path().join("source");
        seed_brave_cookie(&executable, &source_profile, "present").await?;

        let (endpoint, mut local_destination) =
            if let Ok(endpoint) = std::env::var("NANOCODEX_TEST_REMOTE_CDP_ENDPOINT") {
                (url::Url::parse(&endpoint)?, None)
            } else {
                let destination_profile = directory.path().join("destination");
                let destination_config = BrowserConfig::builder()
                    .user_data_dir(destination_profile)
                    .new_headless_mode()
                    .chrome_executable(&executable)
                    .build()
                    .map_err(std::io::Error::other)?;
                let (destination, mut destination_events) =
                    Chromium::launch(destination_config).await?;
                let endpoint = url::Url::parse(destination.websocket_address())?;
                let destination_handler =
                    tokio::spawn(async move { while destination_events.next().await.is_some() {} });
                (endpoint, Some((destination, destination_handler)))
            };

        let brave = BraveSession::new(&executable, &source_profile)
            .allow_origin(url::Url::parse("https://example.com")?);
        let browser = Browser::builder()
            .cdp_endpoint(endpoint)
            .brave_session(brave)
            .build()?;
        browser
            .execute(BrowserAction::Open {
                url: "https://example.com".to_owned(),
            })
            .await?;
        let result = browser
            .execute(BrowserAction::Evaluate {
                expression: "document.cookie.includes('nanocodex_cookie_bridge=present')"
                    .to_owned(),
            })
            .await?;
        let BrowserActionResult::Evaluation { value, .. } = result else {
            return Err(std::io::Error::other("expected evaluation result").into());
        };
        assert_eq!(value.as_bool(), Some(true));

        seed_brave_cookie(&executable, &source_profile, "refreshed").await?;
        browser
            .inner
            .resume_auth_handoff(url::Url::parse("https://example.com")?)
            .await?;
        let result = browser
            .execute(BrowserAction::Evaluate {
                expression: "document.cookie.includes('nanocodex_cookie_bridge=refreshed')"
                    .to_owned(),
            })
            .await?;
        let BrowserActionResult::Evaluation { value, .. } = result else {
            return Err(std::io::Error::other("expected refreshed evaluation result").into());
        };
        assert_eq!(value.as_bool(), Some(true));

        browser.close().await?;
        if let Some((destination, destination_handler)) = local_destination.as_mut() {
            destination.wait().await?;
            destination_handler.abort();
        }
        Ok(())
    }

    async fn seed_brave_cookie(
        executable: &std::path::Path,
        profile: &std::path::Path,
        value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let config = profile_launch_config(
            BrowserConfig::builder()
                .user_data_dir(profile)
                .new_headless_mode(),
        )
        .chrome_executable(executable);
        let (mut browser, mut events) = Chromium::launch(build_config(config)?).await?;
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        let mut cookie = CookieParam::new("nanocodex_cookie_bridge", value);
        cookie.domain = Some(".example.com".to_owned());
        cookie.path = Some("/".to_owned());
        cookie.secure = Some(true);
        cookie.http_only = Some(false);
        cookie.expires = Some(TimeSinceEpoch::new(1_900_000_000.0));
        cookie.priority = Some(CookiePriority::High);
        cookie.source_scheme = Some(CookieSourceScheme::Secure);
        cookie.source_port = Some(443);
        browser.clear_cookies().await?;
        browser.set_cookies(vec![cookie]).await?;
        close_chromium(&mut browser, &handler).await?;
        Ok(())
    }

    fn test_cookie(domain: &str, session: bool, partition_key_opaque: Option<bool>) -> Cookie {
        Cookie {
            name: "session".to_owned(),
            value: "secret".to_owned(),
            domain: domain.to_owned(),
            path: "/console".to_owned(),
            expires: 1_900_000_000.0,
            size: 13,
            http_only: true,
            secure: true,
            session,
            same_site: None,
            priority: CookiePriority::High,
            source_scheme: CookieSourceScheme::Secure,
            source_port: 443,
            partition_key: None,
            partition_key_opaque,
        }
    }
}
