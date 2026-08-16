#![doc = include_str!("../README.md")]
#![deny(unsafe_code, rustdoc::broken_intra_doc_links)]

mod cookie_source;
mod features;
mod native;
mod session;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub mod vm;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_oai_api::ImageDetail;
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult,
    contract::ToolOutputContent,
    runtime::{DynamicToolProvider, schema_for},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const MAX_VIEWPORT_DIMENSION: u32 = 16_384;

pub(crate) fn trace_serialized<T>(kind: &'static str, value: &T)
where
    T: Serialize + ?Sized,
{
    if !tracing::enabled!(target: "nanocodex_browser", tracing::Level::INFO) {
        return;
    }
    match serde_json::to_string(value) {
        Ok(content) => {
            info!(
                target: "nanocodex_browser",
                browser_content_kind = kind,
                content,
                "browser observed ordered content"
            );
        }
        Err(error) => {
            warn!(
                target: "nanocodex_browser",
                browser_content_kind = kind,
                %error,
                "failed to serialize browser trace content"
            );
        }
    }
}

pub use cookie_source::{BrowserCookieSourceError, FirefoxCookieSource, SafariCookieSource};
pub use features::{
    BrowserAccessibilityAudit, BrowserAccessibilityImpact, BrowserAccessibilityViolation,
    BrowserAfterAction, BrowserAxeAudit, BrowserAxeFinding, BrowserAxeNode, BrowserBreakpoint,
    BrowserCache, BrowserCacheEntry, BrowserColorScheme, BrowserContext, BrowserCookie,
    BrowserCookieSameSite, BrowserCoverage, BrowserCpuFunction, BrowserCpuProfile,
    BrowserCruxClient, BrowserCruxCollectionPeriod, BrowserCruxFormFactor, BrowserCruxFraction,
    BrowserCruxHistogramBin, BrowserCruxMetric, BrowserCruxReport, BrowserCruxScope,
    BrowserCssProperty, BrowserCssRule, BrowserCssSourceRange, BrowserDebuggerFrame,
    BrowserDebuggerPause, BrowserDebuggerScope, BrowserDialog, BrowserDialogKind, BrowserDownload,
    BrowserEgressPolicy, BrowserEventListener, BrowserFrame, BrowserGeolocation,
    BrowserHarArtifact, BrowserHeapClass, BrowserHeapClassDelta, BrowserHeapComparison,
    BrowserHeapDuplicateString, BrowserHeapInspection, BrowserHeapNode, BrowserHeapRetainerNode,
    BrowserHeapRetainers, BrowserHeapSnapshot, BrowserImageArtifact, BrowserIndexedDbDatabase,
    BrowserLighthouseCategory, BrowserLighthouseCategoryScore, BrowserLighthouseFinding,
    BrowserLighthouseFormFactor, BrowserLighthouseReport, BrowserMatchedStyles, BrowserModelImage,
    BrowserNetworkConditions, BrowserOriginStorage, BrowserPauseOnExceptions, BrowserPdfArtifact,
    BrowserPerformanceInsight, BrowserPerformanceSource, BrowserPerformanceTrace,
    BrowserPermission, BrowserPseudoClass, BrowserReducedMotion, BrowserRouteHeader,
    BrowserRouteResponse, BrowserScriptCoverage, BrowserServiceWorker, BrowserSessionTrace,
    BrowserStorageReport, BrowserStorageState, BrowserTab, BrowserVideoArtifact, BrowserViewport,
    BrowserVisualAnomaly, BrowserVisualAnomalyKind, BrowserVisualDiff, BrowserVisualTrace,
    BrowserWebVitals,
};
pub use native::{BrowserBuildError, BrowserError};
pub use session::{BraveSession, BraveSessionError, BrowserProfileKind};

const TOOL_DESCRIPTION: &str = r#"Control one server-managed browser session.

Call this tool from Code Mode as `await tools.browser({ action: ..., ... })`.
Each call performs exactly one browser action against the session's active page.
Before guessing an action's fields, inspect the exact deferred input contract with
`toolSchema("browser").inputSchema` and filter its `oneOf` variants by the
single-value `properties.action.enum` discriminator. Keep the inspection inside Code Mode
and emit only the relevant variant.
Compose the complete browser investigation in one Code Mode cell whenever
possible. Do not return to the model after every browser action. Browser actions
are deliberately ordered within one session; use normal JavaScript loops and
branches to drive them, and use `Promise.all` only for work that does not depend
on browser action order.

Typical workflow:
1. `open` a URL.
2. `detect_gate` before trusting page content. A JavaScript forwarding challenge
   may be transient: wait briefly and check again before treating it as blocked.
3. `snapshot` the page (interactive elements only by default).
4. Use returned element references such as `@e1` with `click` or `fill`.
5. Take another `snapshot` after navigation or a meaningful page change.

Prefer element references from the latest snapshot over brittle CSS selectors.
When no snapshot exists, use typed `role`/accessible-name, text, label,
placeholder, alt-text, title, or test-ID targets. Semantic targets are strict by
default at the action boundary; use their explicit first/last/nth index only
when multiple matches are intentional.
Use `snapshot_find` to search a large semantic page without returning its
complete tree. Pointer and form actions enforce a single match and wait for
attachment, visibility, stable layout, enabled/editable state, and unobscured
hit targets. Successful mutating actions return page, action-caused network,
console, error, dialog, and download state; the host may also configure an
automatic compact post-action snapshot. Use the explicit selector, text, URL,
load-state, and JavaScript-condition waits for application state that is not
caused synchronously by one action.
Snapshots and raw CSS selectors traverse open shadow roots, so web-component
controls use the same references and actions as light-DOM controls. Local hot
updates remain visible in the owned page; use `reload` when the development
server requires a full document refresh.
Use targeted `get_text`, `get_html`, `get_value`, `get_attribute`, `get_box`,
and `get_styles` actions when the accessibility snapshot lacks required DOM
detail. Use `dom_snapshot` only when complete DOM, layout, iframe, template, or
shadow-tree state is required; it returns the flattened `DevTools` DOM and can
include explicitly requested computed styles. Filter that large typed result in
Code Mode before calling `text(...)`.
Use `screenshot`, `console`, `errors`, `network_requests`, and
`web_socket_messages` when debugging rendered behavior. Network reads are
ordered and cursor-based: start with `after: 0`, then pass the greatest returned
sequence until `has_more` is false. Read request or response content explicitly
with `network_body`; bodies are not retained in every request summary.
Screenshot-like results expose a file-backed artifact. At the Code Mode
boundary they also contain `modelImage`; call `image(result.image.modelImage)`
for a screenshot or baseline, `image(result.diff.modelImage)` for a visual
diff, or `image(anomaly.modelImage)` for a visual-trace anomaly. Do not include
the base64-bearing `modelImage` object in subsequent `text(...)` output.
For pixel calibration, first use `set_viewport` with an explicit
`device_scale_factor`, then pass the same element `target` to `screenshot`,
`visual_baseline`, and `visual_diff`. Targeted captures crop to the element's
rendered border box and cannot be combined with `full_page`.
Use `visual_baseline` plus `visual_diff` for deterministic before/after
comparison. Use `visual_trace_start` and `visual_trace_stop` around an
interaction to detect flashes, blank intermediate frames, and large visual
changes. Use `web_vitals` for lightweight metrics and a bounded
`performance_trace_start`/`performance_trace_stop` pair when a Chrome trace and
typed scripting/rendering/painting insights are needed. `cpu_profile_start`,
`coverage_start`, and their stop actions produce file-backed V8 artifacts plus
bounded typed summaries. Start coverage before loading the application when
unused initial modules matter. Use two `heap_snapshot` actions and
`heap_compare` for retained-memory growth, then `heap_retainers` with a class
summary's largest node ID to explain why one object is still reachable.
Use `heap_inspect` for bounded dominator-ranked objects, retained sizes, detached
state, and duplicate strings. Use `session_trace_start`/`session_trace_stop` to
retain an exact typed action/result JSONL stream with optional per-action PNG
and flattened-DOM evidence; the direct Rust API can replay that stream.
`video_start`/`video_stop` is an explicit optional WebM recording and requires
host-configured `ffmpeg`.
Console and page-error stacks retain generated locations and expose original
locations when the application publishes same-origin or inline source maps.
When the host enabled React diagnostics, use `react_events` to read React
renderer capabilities, commit trees, render timings, source locations, and
typed render causes. It is also cursor-based; start with `after: 0` after each
full document navigation and aggregate or rank the returned events in Code Mode.
Use `element_context` with a snapshot reference or CSS selector to map one
rendered element back to its React component, source location, owner stack,
stable selector, markup snippet, and scoped styles.
Diagnostic reads report retained and dropped counts and accept a limit up to
1,000. Cookie and authorization header values remain hidden.
Use `list_frames` and `evaluate_frame` for explicit frame work; snapshot
references already retain their containing frame for normal DOM actions. Use
`new_tab`, `list_tabs`, `select_tab`, and `close_tab` for multi-page workflows.
Pending JavaScript dialogs must be read with `dialog` and resolved with
`handle_dialog`. `downloads` reports files retained in the session-private
download directory.
The input surface includes scrolling, native select values, checked state,
drag/drop, uploads, raw mouse/touch events, independent key down/up, and IME
text insertion. Upload paths are relative to the host-configured file
root; the model cannot choose or widen that root.
Use deterministic `network_route` responses for fixtures, `set_offline` for
offline behavior, and `export_har` for a retained HAR artifact.
`accessibility_audit` is the embedded fast deterministic audit; it reports
typed findings across the main document and current child frames.
Use `axe_audit` for the complete pinned axe-core engine, `pdf` for Chromium
print output, `lighthouse_audit` for an exact explicitly configured Lighthouse
CLI attached to this Chrome session, and `crux` for configured field data.
Harness credentials and browser storage state never enter this action schema.
Use `matched_styles`, `force_pseudo_state`, and `event_listeners` for authored
CSS and listener provenance. The debugger actions expose source-mapped
breakpoints, exception policy, pause stacks/scopes, resume, and stepping;
`storage_inspect` covers service workers, Cache Storage, IndexedDB, and storage
keys.
A snapshot returns compact accessibility-tree text plus stable `eN` references,
following a familiar browser-agent convention.
Use `evaluate` only when the normal snapshot and interaction actions are
insufficient. The browser is owned by the host: do not launch a browser, connect
to a debugging port, or manage browser processes yourself."#;

/// One action against the active page of a browser session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserAction {
    /// Navigate the active page to a URL.
    Open {
        /// Absolute URL to open.
        url: String,
    },
    /// Reload the active page after a local build or hot-update failure.
    Reload,
    /// Classify a visible CAPTCHA, JavaScript challenge, or access denial.
    DetectGate,
    /// Capture a semantic representation of the current page.
    Snapshot {
        /// Return only elements useful for interaction. Defaults to true.
        #[serde(default = "default_true")]
        #[schemars(default = "default_true")]
        interactive: bool,
        /// Remove empty structural elements.
        #[serde(default)]
        compact: bool,
        /// Limit the returned accessibility-tree depth.
        depth: Option<u32>,
        /// Restrict the snapshot to a CSS selector when present.
        selector: Option<String>,
        /// Include href URLs on links.
        #[serde(default)]
        include_urls: bool,
    },
    /// Search a complete semantic snapshot without returning the whole tree.
    SnapshotFind {
        /// Case-insensitive text to find in the accessibility snapshot.
        query: String,
        /// Maximum matching windows. Defaults to 20 and is capped at 100.
        max_results: Option<u16>,
    },
    /// Capture the complete flattened DOM, including iframe, template, and shadow-tree content.
    DomSnapshot {
        /// Computed CSS property names to include for every layout node.
        #[serde(default)]
        computed_styles: Vec<String>,
        /// Include document-coordinate offset, scroll, and client rectangles.
        #[serde(default = "default_true")]
        #[schemars(default = "default_true")]
        include_dom_rects: bool,
        /// Include global layout paint-order indexes.
        #[serde(default)]
        include_paint_order: bool,
    },
    /// Click one element resolved by a typed target.
    Click {
        target: BrowserTarget,
        /// Optional button, click count, and keyboard modifiers.
        options: Option<BrowserClickOptions>,
    },
    /// Replace the value of an editable element.
    Fill {
        target: BrowserTarget,
        /// Text to place in the element.
        text: String,
    },
    /// Send a keyboard key to an element.
    Press {
        target: BrowserTarget,
        /// Key name such as `Enter`, `Tab`, or `ArrowDown`.
        key: String,
        /// Modifiers applied to the key event.
        #[serde(default)]
        modifiers: Vec<BrowserKeyModifier>,
    },
    /// Move the pointer over an element.
    Hover { target: BrowserTarget },
    /// Move the raw mouse pointer to viewport coordinates.
    MouseMove {
        x: i32,
        y: i32,
        /// Number of interpolated mouse-move events. Defaults to one and is capped at 100.
        steps: Option<u16>,
    },
    /// Press a mouse button at the current raw pointer position.
    MouseDown {
        #[serde(default)]
        button: BrowserMouseButton,
        #[serde(default)]
        modifiers: Vec<BrowserKeyModifier>,
    },
    /// Release a mouse button at the current raw pointer position.
    MouseUp {
        #[serde(default)]
        button: BrowserMouseButton,
        #[serde(default)]
        modifiers: Vec<BrowserKeyModifier>,
    },
    /// Dispatch a wheel event at the current raw pointer position.
    MouseWheel {
        #[serde(default)]
        delta_x: i32,
        #[serde(default)]
        delta_y: i32,
    },
    /// Dispatch one native touch tap at viewport coordinates.
    TouchTap { x: i32, y: i32 },
    /// Dispatch one native touch swipe between viewport coordinates.
    TouchSwipe {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
        /// Gesture duration. Defaults to 250 milliseconds and is capped at five seconds.
        duration_ms: Option<u64>,
        /// Number of interpolated touch-move events. Defaults to ten and is capped at 100.
        steps: Option<u16>,
    },
    /// Press a raw keyboard key without releasing it.
    KeyboardDown {
        key: String,
        #[serde(default)]
        modifiers: Vec<BrowserKeyModifier>,
    },
    /// Release a raw keyboard key.
    KeyboardUp {
        key: String,
        #[serde(default)]
        modifiers: Vec<BrowserKeyModifier>,
    },
    /// Insert text through Chromium's native IME boundary.
    InsertText { text: String },
    /// Scroll the page or one scrollable element by a relative amount.
    Scroll {
        /// Omit to scroll the page itself.
        target: Option<BrowserTarget>,
        /// Horizontal delta in CSS pixels.
        #[serde(default)]
        x: i64,
        /// Vertical delta in CSS pixels.
        #[serde(default)]
        y: i64,
    },
    /// Select one or more values in a native `select` element.
    SelectOption {
        target: BrowserTarget,
        values: Vec<String>,
    },
    /// Set the checked state of a checkbox or radio input.
    SetChecked {
        target: BrowserTarget,
        checked: bool,
    },
    /// Drag one element onto another element.
    Drag {
        source: BrowserTarget,
        destination: BrowserTarget,
    },
    /// Upload files from the caller-configured browser file root.
    UploadFiles {
        target: BrowserTarget,
        /// Paths relative to the browser's configured file root.
        paths: Vec<std::path::PathBuf>,
    },
    /// Set the active page's viewport in CSS pixels and its output pixel density.
    SetViewport {
        width: u32,
        height: u32,
        /// Device pixels per CSS pixel. Defaults to 1.0.
        device_scale_factor: Option<f64>,
    },
    /// Navigate to the preceding history entry when one exists.
    GoBack,
    /// Navigate to the following history entry when one exists.
    GoForward,
    /// Wait until an element reaches the requested state.
    WaitForSelector {
        target: BrowserTarget,
        /// Defaults to `visible`.
        state: Option<BrowserWaitForSelectorState>,
    },
    /// Wait for text to appear or disappear.
    WaitForText {
        text: String,
        /// Restrict the search to one element when present.
        target: Option<BrowserTarget>,
        /// Wait for the text to be absent instead of present.
        #[serde(default)]
        hidden: bool,
    },
    /// Wait until the current URL contains a substring.
    WaitForUrl { url_contains: String },
    /// Wait for the active document to reach a loading milestone.
    WaitForLoadState { state: BrowserLoadState },
    /// Wait until a JavaScript expression evaluates to a truthy value.
    WaitForFunction { expression: String },
    /// Wait for a bounded amount of wall-clock time.
    WaitForTimeout { milliseconds: u64 },
    /// Capture the rendered page to a session-private image file.
    Screenshot {
        /// Capture the complete scrollable page.
        #[serde(default)]
        full_page: bool,
        /// Overlay numbered labels for interactive elements.
        #[serde(default)]
        annotate: bool,
        /// Crop to one rendered element's border box.
        target: Option<BrowserTarget>,
    },
    /// Print the active page to a private file-backed PDF artifact.
    Pdf {
        #[serde(default)]
        landscape: bool,
        #[serde(default)]
        print_background: bool,
        #[serde(default)]
        prefer_css_page_size: bool,
        #[serde(default)]
        tagged: bool,
        #[serde(default)]
        document_outline: bool,
    },
    /// Capture a screenshot and retain it as a visual-comparison baseline.
    VisualBaseline {
        /// Capture the complete scrollable page.
        #[serde(default)]
        full_page: bool,
        /// Crop to one rendered element's border box.
        target: Option<BrowserTarget>,
    },
    /// Compare the current page against a retained visual baseline.
    VisualDiff {
        /// Opaque artifact identifier returned by `visual_baseline`.
        baseline_id: String,
        /// Per-channel change threshold from zero to 255. Defaults to 16.
        threshold: Option<u8>,
        /// Capture the complete scrollable page.
        #[serde(default)]
        full_page: bool,
        /// Crop to one rendered element's border box.
        target: Option<BrowserTarget>,
    },
    /// Begin a bounded rendered-frame trace for flash and instability detection.
    VisualTraceStart {
        /// Capture frequency. Defaults to 10 and is capped at 30.
        frames_per_second: Option<u8>,
        /// Maximum retained frames. Defaults to 120 and is capped at 600.
        max_frames: Option<u16>,
    },
    /// Stop the active rendered-frame trace and classify visual instability.
    VisualTraceStop,
    /// Begin an ordered, file-backed action/result trace suitable for replay.
    SessionTraceStart {
        /// Capture the rendered viewport after each retained action.
        #[serde(default = "default_true")]
        #[schemars(default = "default_true")]
        screenshots: bool,
        /// Capture a complete flattened DOM snapshot after each retained action.
        #[serde(default = "default_true")]
        #[schemars(default = "default_true")]
        dom_snapshots: bool,
        /// Maximum retained actions. Defaults to 500 and is capped at 2,000.
        max_actions: Option<u16>,
    },
    /// Stop the active action trace and flush its network and diagnostic indexes.
    SessionTraceStop,
    /// Read visible text from an element.
    GetText { target: BrowserTarget },
    /// Read inner HTML from an element.
    GetHtml { target: BrowserTarget },
    /// Read an input's current value.
    GetValue { target: BrowserTarget },
    /// Read one attribute from an element.
    GetAttribute {
        target: BrowserTarget,
        /// Attribute name.
        name: String,
    },
    /// Read the active page title.
    GetTitle,
    /// Read the active page URL.
    GetUrl,
    /// Count elements matching a reference or CSS selector.
    GetCount { target: BrowserTarget },
    /// Read an element's viewport-relative bounding box.
    GetBox { target: BrowserTarget },
    /// Read computed styles for an element.
    GetStyles { target: BrowserTarget },
    /// Read authored CSS cascade provenance for an element.
    MatchedStyles { target: BrowserTarget },
    /// Force a set of CSS pseudo-classes on an element; an empty list clears them.
    ForcePseudoState {
        target: BrowserTarget,
        pseudo_classes: Vec<BrowserPseudoClass>,
    },
    /// Read native event listeners on an element and, optionally, its pierced subtree.
    EventListeners {
        target: BrowserTarget,
        /// Child depth. Defaults to one and is capped at 32.
        depth: Option<u8>,
        /// Traverse iframes and shadow roots.
        #[serde(default)]
        pierce: bool,
    },
    /// Configure JavaScript exception pause behavior.
    DebuggerSetPauseOnExceptions { state: BrowserPauseOnExceptions },
    /// Install a URL-based JavaScript breakpoint.
    DebuggerSetBreakpoint {
        url: String,
        /// One-based source line.
        line_number: u64,
        /// One-based source column. Defaults to one.
        column_number: Option<u64>,
        condition: Option<String>,
    },
    /// Remove one installed JavaScript breakpoint.
    DebuggerRemoveBreakpoint { breakpoint_id: String },
    /// Read the latest JavaScript pause and its lexical scopes.
    DebuggerPaused,
    /// Resume JavaScript after a breakpoint or exception.
    DebuggerResume,
    /// Step over the next JavaScript statement.
    DebuggerStepOver,
    /// Step into the next JavaScript call.
    DebuggerStepInto,
    /// Step out of the current JavaScript call.
    DebuggerStepOut,
    /// Inspect service workers, Cache Storage, `IndexedDB`, and storage keys.
    StorageInspect,
    /// Read the newest captured browser console entries.
    Console {
        /// Maximum entries to return. Defaults to 200 and is capped at 1,000.
        limit: Option<u16>,
    },
    /// Read the newest uncaught page errors.
    Errors {
        /// Maximum errors to return. Defaults to 200 and is capped at 1,000.
        limit: Option<u16>,
    },
    /// Read captured network requests.
    NetworkRequests {
        /// Optional URL substring filter.
        filter: Option<String>,
        /// Return records with a sequence greater than this cursor. Use zero for the first page.
        after: Option<u64>,
        /// Maximum matching requests to return. Defaults to 200 and is capped at 1,000.
        limit: Option<u16>,
    },
    /// Read a request or response body on demand.
    NetworkBody {
        /// Stable request identifier returned by `network_requests`.
        request_id: String,
        /// Which side of the exchange to read.
        kind: BrowserNetworkBodyKind,
    },
    /// Read captured WebSocket messages in event order.
    WebSocketMessages {
        /// Restrict messages to one connection returned by `network_requests`.
        request_id: Option<String>,
        /// Return messages with a sequence greater than this cursor. Use zero for the first page.
        after: Option<u64>,
        /// Maximum messages to return. Defaults to 200 and is capped at 1,000.
        limit: Option<u16>,
    },
    /// Read React renderer, commit, scheduling, and render-cause diagnostics.
    ReactEvents {
        /// Return events with a sequence greater than this cursor. Use zero after navigation.
        after: Option<u64>,
        /// Maximum events to return. Defaults to 200 and is capped at 1,000.
        limit: Option<u16>,
    },
    /// Read React-aware source, owner, markup, selector, and style context for an element.
    ElementContext { target: BrowserTarget },
    /// Read Core Web Vitals and high-signal navigation performance metrics.
    WebVitals,
    /// Begin a bounded Chromium performance trace.
    PerformanceTraceStart,
    /// Stop the active performance trace and return a typed summary.
    PerformanceTraceStop,
    /// Begin a V8 sampling CPU profile.
    CpuProfileStart,
    /// Stop the active CPU profile and return a typed summary plus artifact.
    CpuProfileStop,
    /// Begin precise JavaScript execution coverage.
    CoverageStart,
    /// Stop precise JavaScript coverage and return a typed summary plus artifact.
    CoverageStop,
    /// Capture and analyze one V8 heap snapshot.
    HeapSnapshot {
        /// Ask V8 to collect garbage before taking the snapshot.
        #[serde(default)]
        collect_garbage: bool,
    },
    /// Compare two heap snapshot artifacts retained by this session.
    HeapCompare { before_id: String, after_id: String },
    /// Walk a bounded reverse-reference graph from one V8 heap node toward GC roots.
    HeapRetainers {
        /// Heap artifact identifier returned by `heap_snapshot`.
        artifact_id: String,
        /// V8 node identifier, such as a class summary's largest retained node.
        node_id: u64,
        /// Maximum reverse-reference distance. Defaults to 5 and is capped at 16.
        max_depth: Option<u8>,
        /// Maximum returned nodes. Defaults to 200 and is capped at 1,000.
        max_nodes: Option<u16>,
    },
    /// Query detailed objects and duplicate strings in a retained heap snapshot.
    HeapInspect {
        artifact_id: String,
        /// Restrict objects to one exact V8 class name.
        class_name: Option<String>,
        /// Ignore objects below this dominator-based retained size.
        minimum_retained_size: Option<u64>,
        /// Maximum returned objects. Defaults to 100 and is capped at 1,000.
        max_nodes: Option<u16>,
        /// Include the largest duplicated string payloads.
        #[serde(default)]
        include_duplicate_strings: bool,
    },
    /// Begin an optional `WebM` recording of the active page.
    VideoStart {
        /// Output frame rate. Defaults to 25 and is capped at 30.
        frames_per_second: Option<u8>,
        /// JPEG source-frame quality. Defaults to 80 and is capped at 95.
        quality: Option<u8>,
    },
    /// Stop the active `WebM` recording.
    VideoStop,
    /// List the main document and all current child frames.
    ListFrames,
    /// Evaluate JavaScript in one explicit frame.
    EvaluateFrame {
        /// Chromium frame identifier returned by `list_frames` or a snapshot reference.
        frame_id: String,
        /// JavaScript expression to evaluate.
        expression: String,
    },
    /// Open a new browser tab and make it active.
    NewTab {
        /// Absolute URL to open. Omit to retain an empty tab.
        url: Option<String>,
    },
    /// List every open tab.
    ListTabs,
    /// Make an existing tab active.
    SelectTab {
        /// Chromium target identifier returned by `list_tabs`.
        tab_id: String,
    },
    /// Close an existing tab.
    CloseTab {
        /// Chromium target identifier returned by `list_tabs`.
        tab_id: String,
    },
    /// Read the currently pending JavaScript dialog, if any.
    Dialog,
    /// Accept or dismiss the currently pending JavaScript dialog.
    HandleDialog {
        accept: bool,
        /// Text supplied to a prompt dialog when accepting it.
        prompt_text: Option<String>,
    },
    /// Add or replace a deterministic URL-substring network route.
    NetworkRoute {
        /// Stable route identifier used to remove or replace the route.
        route_id: String,
        /// URL substring matched against each request.
        url_contains: String,
        response: BrowserRouteResponse,
    },
    /// Remove one deterministic network route.
    RemoveNetworkRoute { route_id: String },
    /// Remove all deterministic network routes.
    ClearNetworkRoutes,
    /// Enable or disable browser network connectivity.
    SetOffline { offline: bool },
    /// Export retained request metadata as a HAR 1.2 artifact.
    ExportHar {
        /// Include available request and response bodies.
        #[serde(default)]
        include_bodies: bool,
    },
    /// Run the library's embedded deterministic accessibility audit.
    AccessibilityAudit,
    /// Run the pinned axe-core engine without adding it to the inspected app.
    AxeAudit,
    /// Run an exact external Lighthouse audit against the owned Chrome session.
    ///
    /// The harness must configure the Lighthouse executable explicitly.
    LighthouseAudit {
        /// Empty selects performance, accessibility, best practices, and SEO.
        #[serde(default)]
        categories: Vec<BrowserLighthouseCategory>,
        form_factor: Option<BrowserLighthouseFormFactor>,
    },
    /// Query Chrome UX Report field data for the active page.
    ///
    /// The harness must configure a `CrUX` client and API key explicitly.
    Crux {
        #[serde(default)]
        scope: BrowserCruxScope,
        form_factor: Option<BrowserCruxFormFactor>,
    },
    /// Read downloads observed by the browser session.
    Downloads,
    /// Evaluate JavaScript in the active page.
    Evaluate {
        /// JavaScript expression to evaluate.
        expression: String,
    },
}

impl BrowserAction {
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive action-to-name mapping is intentionally kept in one auditable match"
    )]
    const fn name(&self) -> BrowserActionName {
        match self {
            Self::Open { .. } => BrowserActionName::Open,
            Self::Reload => BrowserActionName::Reload,
            Self::DetectGate => BrowserActionName::DetectGate,
            Self::Snapshot { .. } => BrowserActionName::Snapshot,
            Self::SnapshotFind { .. } => BrowserActionName::SnapshotFind,
            Self::DomSnapshot { .. } => BrowserActionName::DomSnapshot,
            Self::Click { .. } => BrowserActionName::Click,
            Self::Fill { .. } => BrowserActionName::Fill,
            Self::Press { .. } => BrowserActionName::Press,
            Self::Hover { .. } => BrowserActionName::Hover,
            Self::MouseMove { .. } => BrowserActionName::MouseMove,
            Self::MouseDown { .. } => BrowserActionName::MouseDown,
            Self::MouseUp { .. } => BrowserActionName::MouseUp,
            Self::MouseWheel { .. } => BrowserActionName::MouseWheel,
            Self::TouchTap { .. } => BrowserActionName::TouchTap,
            Self::TouchSwipe { .. } => BrowserActionName::TouchSwipe,
            Self::KeyboardDown { .. } => BrowserActionName::KeyboardDown,
            Self::KeyboardUp { .. } => BrowserActionName::KeyboardUp,
            Self::InsertText { .. } => BrowserActionName::InsertText,
            Self::Scroll { .. } => BrowserActionName::Scroll,
            Self::SelectOption { .. } => BrowserActionName::SelectOption,
            Self::SetChecked { .. } => BrowserActionName::SetChecked,
            Self::Drag { .. } => BrowserActionName::Drag,
            Self::UploadFiles { .. } => BrowserActionName::UploadFiles,
            Self::SetViewport { .. } => BrowserActionName::SetViewport,
            Self::GoBack => BrowserActionName::GoBack,
            Self::GoForward => BrowserActionName::GoForward,
            Self::WaitForSelector { .. } => BrowserActionName::WaitForSelector,
            Self::WaitForText { .. } => BrowserActionName::WaitForText,
            Self::WaitForUrl { .. } => BrowserActionName::WaitForUrl,
            Self::WaitForLoadState { .. } => BrowserActionName::WaitForLoadState,
            Self::WaitForFunction { .. } => BrowserActionName::WaitForFunction,
            Self::WaitForTimeout { .. } => BrowserActionName::WaitForTimeout,
            Self::Screenshot { .. } => BrowserActionName::Screenshot,
            Self::Pdf { .. } => BrowserActionName::Pdf,
            Self::VisualBaseline { .. } => BrowserActionName::VisualBaseline,
            Self::VisualDiff { .. } => BrowserActionName::VisualDiff,
            Self::VisualTraceStart { .. } => BrowserActionName::VisualTraceStart,
            Self::VisualTraceStop => BrowserActionName::VisualTraceStop,
            Self::SessionTraceStart { .. } => BrowserActionName::SessionTraceStart,
            Self::SessionTraceStop => BrowserActionName::SessionTraceStop,
            Self::GetText { .. } => BrowserActionName::GetText,
            Self::GetHtml { .. } => BrowserActionName::GetHtml,
            Self::GetValue { .. } => BrowserActionName::GetValue,
            Self::GetAttribute { .. } => BrowserActionName::GetAttribute,
            Self::GetTitle => BrowserActionName::GetTitle,
            Self::GetUrl => BrowserActionName::GetUrl,
            Self::GetCount { .. } => BrowserActionName::GetCount,
            Self::GetBox { .. } => BrowserActionName::GetBox,
            Self::GetStyles { .. } => BrowserActionName::GetStyles,
            Self::MatchedStyles { .. } => BrowserActionName::MatchedStyles,
            Self::ForcePseudoState { .. } => BrowserActionName::ForcePseudoState,
            Self::EventListeners { .. } => BrowserActionName::EventListeners,
            Self::DebuggerSetPauseOnExceptions { .. } => {
                BrowserActionName::DebuggerSetPauseOnExceptions
            }
            Self::DebuggerSetBreakpoint { .. } => BrowserActionName::DebuggerSetBreakpoint,
            Self::DebuggerRemoveBreakpoint { .. } => BrowserActionName::DebuggerRemoveBreakpoint,
            Self::DebuggerPaused => BrowserActionName::DebuggerPaused,
            Self::DebuggerResume => BrowserActionName::DebuggerResume,
            Self::DebuggerStepOver => BrowserActionName::DebuggerStepOver,
            Self::DebuggerStepInto => BrowserActionName::DebuggerStepInto,
            Self::DebuggerStepOut => BrowserActionName::DebuggerStepOut,
            Self::StorageInspect => BrowserActionName::StorageInspect,
            Self::Console { .. } => BrowserActionName::Console,
            Self::Errors { .. } => BrowserActionName::Errors,
            Self::NetworkRequests { .. } => BrowserActionName::NetworkRequests,
            Self::NetworkBody { .. } => BrowserActionName::NetworkBody,
            Self::WebSocketMessages { .. } => BrowserActionName::WebSocketMessages,
            Self::ReactEvents { .. } => BrowserActionName::ReactEvents,
            Self::ElementContext { .. } => BrowserActionName::ElementContext,
            Self::WebVitals => BrowserActionName::WebVitals,
            Self::PerformanceTraceStart => BrowserActionName::PerformanceTraceStart,
            Self::PerformanceTraceStop => BrowserActionName::PerformanceTraceStop,
            Self::CpuProfileStart => BrowserActionName::CpuProfileStart,
            Self::CpuProfileStop => BrowserActionName::CpuProfileStop,
            Self::CoverageStart => BrowserActionName::CoverageStart,
            Self::CoverageStop => BrowserActionName::CoverageStop,
            Self::HeapSnapshot { .. } => BrowserActionName::HeapSnapshot,
            Self::HeapCompare { .. } => BrowserActionName::HeapCompare,
            Self::HeapRetainers { .. } => BrowserActionName::HeapRetainers,
            Self::HeapInspect { .. } => BrowserActionName::HeapInspect,
            Self::VideoStart { .. } => BrowserActionName::VideoStart,
            Self::VideoStop => BrowserActionName::VideoStop,
            Self::ListFrames => BrowserActionName::ListFrames,
            Self::EvaluateFrame { .. } => BrowserActionName::EvaluateFrame,
            Self::NewTab { .. } => BrowserActionName::NewTab,
            Self::ListTabs => BrowserActionName::ListTabs,
            Self::SelectTab { .. } => BrowserActionName::SelectTab,
            Self::CloseTab { .. } => BrowserActionName::CloseTab,
            Self::Dialog => BrowserActionName::Dialog,
            Self::HandleDialog { .. } => BrowserActionName::HandleDialog,
            Self::NetworkRoute { .. } => BrowserActionName::NetworkRoute,
            Self::RemoveNetworkRoute { .. } => BrowserActionName::RemoveNetworkRoute,
            Self::ClearNetworkRoutes => BrowserActionName::ClearNetworkRoutes,
            Self::SetOffline { .. } => BrowserActionName::SetOffline,
            Self::ExportHar { .. } => BrowserActionName::ExportHar,
            Self::AccessibilityAudit => BrowserActionName::AccessibilityAudit,
            Self::AxeAudit => BrowserActionName::AxeAudit,
            Self::LighthouseAudit { .. } => BrowserActionName::LighthouseAudit,
            Self::Crux { .. } => BrowserActionName::Crux,
            Self::Downloads => BrowserActionName::Downloads,
            Self::Evaluate { .. } => BrowserActionName::Evaluate,
        }
    }
}

/// Stable action name included in recording output.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserActionName {
    Open,
    Reload,
    DetectGate,
    Snapshot,
    SnapshotFind,
    DomSnapshot,
    Click,
    Fill,
    Press,
    Hover,
    MouseMove,
    MouseDown,
    MouseUp,
    MouseWheel,
    TouchTap,
    TouchSwipe,
    KeyboardDown,
    KeyboardUp,
    InsertText,
    Scroll,
    SelectOption,
    SetChecked,
    Drag,
    UploadFiles,
    SetViewport,
    GoBack,
    GoForward,
    WaitForSelector,
    WaitForText,
    WaitForUrl,
    WaitForLoadState,
    WaitForFunction,
    WaitForTimeout,
    Screenshot,
    Pdf,
    VisualBaseline,
    VisualDiff,
    VisualTraceStart,
    VisualTraceStop,
    SessionTraceStart,
    SessionTraceStop,
    GetText,
    GetHtml,
    GetValue,
    GetAttribute,
    GetTitle,
    GetUrl,
    GetCount,
    GetBox,
    GetStyles,
    MatchedStyles,
    ForcePseudoState,
    EventListeners,
    DebuggerSetPauseOnExceptions,
    DebuggerSetBreakpoint,
    DebuggerRemoveBreakpoint,
    DebuggerPaused,
    DebuggerResume,
    DebuggerStepOver,
    DebuggerStepInto,
    DebuggerStepOut,
    StorageInspect,
    Console,
    Errors,
    NetworkRequests,
    NetworkBody,
    WebSocketMessages,
    ReactEvents,
    ElementContext,
    WebVitals,
    PerformanceTraceStart,
    PerformanceTraceStop,
    CpuProfileStart,
    CpuProfileStop,
    CoverageStart,
    CoverageStop,
    HeapSnapshot,
    HeapCompare,
    HeapRetainers,
    HeapInspect,
    VideoStart,
    VideoStop,
    ListFrames,
    EvaluateFrame,
    NewTab,
    ListTabs,
    SelectTab,
    CloseTab,
    Dialog,
    HandleDialog,
    NetworkRoute,
    RemoveNetworkRoute,
    ClearNetworkRoutes,
    SetOffline,
    ExportHar,
    AccessibilityAudit,
    AxeAudit,
    LighthouseAudit,
    Crux,
    Downloads,
    Evaluate,
}

/// One deterministic way to locate an element.
///
/// Semantic locators use the browser's rendered DOM, accessible names, and
/// implicit roles. They traverse open shadow roots and are resolved across the
/// current frame tree. Snapshot references remain the cheapest target after a
/// snapshot and retain their exact frame and shadow path.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserLocator {
    /// Stable reference returned by the latest semantic snapshot.
    Ref { reference: String },
    /// CSS selector evaluated through open shadow roots.
    Css { selector: String },
    /// ARIA or implicit role, optionally restricted by accessible name.
    Role { role: String, name: Option<String> },
    /// Rendered text content.
    Text { text: String },
    /// Form control associated with a rendered label.
    Label { label: String },
    /// Form control placeholder.
    Placeholder { placeholder: String },
    /// Image or input alternative text.
    AltText { text: String },
    /// Element title attribute.
    Title { title: String },
    /// `data-testid` attribute.
    TestId { id: String },
}

/// Optional deterministic selection from a locator's ordered matches.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserTargetIndex {
    First,
    Last,
    /// Zero-based index in DOM order.
    Nth {
        index: u32,
    },
}

/// Typed target shared by browser interaction and inspection actions.
#[derive(Clone, Debug, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserTarget {
    #[serde(flatten)]
    pub locator: BrowserLocator,
    /// Require a case-sensitive exact semantic-name/text match.
    #[serde(default)]
    pub exact: bool,
    /// Select one ordered match. Without this field, strict actions reject
    /// multiple matches.
    pub index: Option<BrowserTargetIndex>,
}

impl<'de> Deserialize<'de> for BrowserTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = BrowserTargetInput::deserialize(deserializer)?;
        let (locator, exact, index) = match input {
            BrowserTargetInput::Ref {
                reference,
                exact,
                index,
            } => (BrowserLocator::Ref { reference }, exact, index),
            BrowserTargetInput::Css {
                selector,
                exact,
                index,
            } => (BrowserLocator::Css { selector }, exact, index),
            BrowserTargetInput::Role {
                role,
                name,
                exact,
                index,
            } => (BrowserLocator::Role { role, name }, exact, index),
            BrowserTargetInput::Text { text, exact, index } => {
                (BrowserLocator::Text { text }, exact, index)
            }
            BrowserTargetInput::Label {
                label,
                exact,
                index,
            } => (BrowserLocator::Label { label }, exact, index),
            BrowserTargetInput::Placeholder {
                placeholder,
                exact,
                index,
            } => (BrowserLocator::Placeholder { placeholder }, exact, index),
            BrowserTargetInput::AltText { text, exact, index } => {
                (BrowserLocator::AltText { text }, exact, index)
            }
            BrowserTargetInput::Title {
                title,
                exact,
                index,
            } => (BrowserLocator::Title { title }, exact, index),
            BrowserTargetInput::TestId { id, exact, index } => {
                (BrowserLocator::TestId { id }, exact, index)
            }
        };
        Ok(Self {
            locator,
            exact,
            index,
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
enum BrowserTargetInput {
    Ref {
        reference: String,
        #[serde(default = "default_true")]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Css {
        selector: String,
        #[serde(default = "default_true")]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Role {
        role: String,
        name: Option<String>,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Text {
        text: String,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Label {
        label: String,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Placeholder {
        placeholder: String,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    AltText {
        text: String,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    Title {
        title: String,
        #[serde(default)]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
    TestId {
        id: String,
        #[serde(default = "default_true")]
        exact: bool,
        index: Option<BrowserTargetIndex>,
    },
}

impl BrowserTarget {
    pub fn reference(reference: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Ref {
                reference: reference.into(),
            },
            exact: true,
            index: None,
        }
    }

    pub fn css(selector: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Css {
                selector: selector.into(),
            },
            exact: true,
            index: None,
        }
    }

    pub fn role(role: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Role {
                role: role.into(),
                name: None,
            },
            exact: false,
            index: None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Text { text: text.into() },
            exact: false,
            index: None,
        }
    }

    pub fn label(label: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Label {
                label: label.into(),
            },
            exact: false,
            index: None,
        }
    }

    pub fn placeholder(placeholder: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Placeholder {
                placeholder: placeholder.into(),
            },
            exact: false,
            index: None,
        }
    }

    pub fn alt_text(text: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::AltText { text: text.into() },
            exact: false,
            index: None,
        }
    }

    pub fn title(title: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::Title {
                title: title.into(),
            },
            exact: false,
            index: None,
        }
    }

    pub fn test_id(id: impl Into<String>) -> Self {
        Self {
            locator: BrowserLocator::TestId { id: id.into() },
            exact: true,
            index: None,
        }
    }

    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        if let BrowserLocator::Role {
            name: target_name, ..
        } = &mut self.locator
        {
            *target_name = Some(name.into());
        }
        self
    }

    #[must_use]
    pub const fn exact(mut self) -> Self {
        self.exact = true;
        self
    }

    #[must_use]
    pub const fn first(mut self) -> Self {
        self.index = Some(BrowserTargetIndex::First);
        self
    }

    #[must_use]
    pub const fn last(mut self) -> Self {
        self.index = Some(BrowserTargetIndex::Last);
        self
    }

    #[must_use]
    pub const fn nth(mut self, index: u32) -> Self {
        self.index = Some(BrowserTargetIndex::Nth { index });
        self
    }

    fn display(&self) -> String {
        let locator = match &self.locator {
            BrowserLocator::Ref { reference } => reference.clone(),
            BrowserLocator::Css { selector } => selector.clone(),
            BrowserLocator::Role { role, name } => name.as_ref().map_or_else(
                || format!("role={role}"),
                |name| format!("role={role}, name={name:?}"),
            ),
            BrowserLocator::Text { text } => format!("text={text:?}"),
            BrowserLocator::Label { label } => format!("label={label:?}"),
            BrowserLocator::Placeholder { placeholder } => {
                format!("placeholder={placeholder:?}")
            }
            BrowserLocator::AltText { text } => format!("alt={text:?}"),
            BrowserLocator::Title { title } => format!("title={title:?}"),
            BrowserLocator::TestId { id } => format!("testid={id:?}"),
        };
        match self.index {
            None => locator,
            Some(BrowserTargetIndex::First) => format!("{locator}.first()"),
            Some(BrowserTargetIndex::Last) => format!("{locator}.last()"),
            Some(BrowserTargetIndex::Nth { index }) => format!("{locator}.nth({index})"),
        }
    }
}

/// Mouse button and repetition policy for a click.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserClickOptions {
    #[serde(default)]
    pub button: BrowserMouseButton,
    /// One performs a normal click and two performs a double click.
    #[serde(default = "default_click_count")]
    #[schemars(default = "default_click_count")]
    pub click_count: u8,
    #[serde(default)]
    pub modifiers: Vec<BrowserKeyModifier>,
}

impl Default for BrowserClickOptions {
    fn default() -> Self {
        Self {
            button: BrowserMouseButton::Left,
            click_count: default_click_count(),
            modifiers: Vec::new(),
        }
    }
}

const fn default_click_count() -> u8 {
    1
}

/// Mouse button used by a native browser click.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Keyboard modifier applied to one native input event.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKeyModifier {
    Alt,
    Control,
    Meta,
    Shift,
}

/// Element state accepted by [`BrowserAction::WaitForSelector`].
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWaitForSelectorState {
    Attached,
    #[default]
    Visible,
    Hidden,
    Detached,
}

/// Active-document loading milestone.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoadState {
    DomContentLoaded,
    Load,
}

/// Metadata for one stable reference in an accessibility snapshot.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserElementReference {
    /// Accessibility role, such as `button`, `link`, or `textbox`.
    pub role: String,
    /// Accessible name exposed to the user.
    pub name: String,
    /// Whether the element currently reports itself disabled.
    pub disabled: bool,
    /// URL of the containing frame for cross-origin iframe elements.
    pub frame_url: Option<String>,
    /// Chromium frame identifier for explicit frame-scoped operations.
    pub frame_id: Option<String>,
}

/// Viewport-relative element bounds in CSS pixels.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserBoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Document-relative rectangle returned by a complete DOM snapshot.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserDomRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Rendered layout state attached to one node in a complete DOM snapshot.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserDomLayout {
    pub bounds: BrowserDomRect,
    pub text: String,
    /// Requested computed property names mapped to their computed values.
    pub styles: BTreeMap<String, String>,
    pub paint_order: Option<i64>,
    pub offset_rect: Option<BrowserDomRect>,
    pub scroll_rect: Option<BrowserDomRect>,
    pub client_rect: Option<BrowserDomRect>,
}

/// One node from Chromium's flattened DOM snapshot.
///
/// `node_type` uses the standard DOM `Node.nodeType` constants. Parent and
/// content-document indexes refer to the containing document's `nodes` array
/// and the top-level `documents` array respectively.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDomNode {
    pub index: usize,
    pub parent_index: Option<usize>,
    pub node_type: i64,
    pub node_name: String,
    pub node_value: String,
    pub backend_node_id: i64,
    pub attributes: BTreeMap<String, String>,
    pub shadow_root_type: Option<String>,
    pub text_value: Option<String>,
    pub input_value: Option<String>,
    pub input_checked: Option<bool>,
    pub option_selected: Option<bool>,
    pub content_document_index: Option<usize>,
    pub pseudo_type: Option<String>,
    pub pseudo_identifier: Option<String>,
    pub is_clickable: bool,
    pub current_source_url: Option<String>,
    pub origin_url: Option<String>,
    pub layout: Option<BrowserDomLayout>,
}

/// One root document or child-frame document in a complete DOM snapshot.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDomDocument {
    pub document_url: String,
    pub title: String,
    pub base_url: String,
    pub content_language: String,
    pub encoding_name: String,
    pub public_id: String,
    pub system_id: String,
    pub frame_id: String,
    pub scroll_offset_x: Option<f64>,
    pub scroll_offset_y: Option<f64>,
    pub content_width: Option<f64>,
    pub content_height: Option<f64>,
    pub nodes: Vec<BrowserDomNode>,
}

/// Complete typed DOM state captured atomically by Chromium `DevTools`.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDomSnapshot {
    pub documents: Vec<BrowserDomDocument>,
    pub node_count: usize,
    pub element_count: usize,
    pub text_node_count: usize,
    pub comment_count: usize,
    pub attribute_count: usize,
    /// Nodes Chromium reports as belonging to an open, closed, or user-agent shadow tree.
    pub shadow_tree_node_count: usize,
}

/// High-signal computed CSS fields used during rendered-layout debugging.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserComputedStyles {
    pub display: String,
    pub position: String,
    pub color: String,
    pub background_color: String,
    pub font_family: String,
    pub font_size: String,
    pub font_weight: String,
    pub visibility: String,
    pub opacity: String,
    pub z_index: String,
    pub overflow: String,
    pub overflow_x: String,
    pub overflow_y: String,
    pub box_sizing: String,
    pub width: String,
    pub height: String,
    pub margin: String,
    pub padding: String,
    pub border: String,
    pub transform: String,
    pub transition: String,
    pub animation: String,
    pub pointer_events: String,
    pub flex_direction: String,
    pub align_items: String,
    pub justify_content: String,
    pub grid_template_columns: String,
    pub gap: String,
}

/// One browser console entry captured by the active session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConsoleEntry {
    /// Monotonic cursor within this browser session.
    pub sequence: u64,
    pub level: String,
    pub text: String,
    pub stack: Vec<BrowserStackFrame>,
}

/// One uncaught page error captured by the active session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPageError {
    /// Monotonic cursor within this browser session.
    pub sequence: u64,
    pub text: String,
    pub url: Option<String>,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub stack: Vec<BrowserStackFrame>,
}

/// Generated or source-mapped JavaScript location.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSourceLocation {
    pub url: String,
    /// One-based source line for human-facing output.
    pub line_number: u64,
    /// One-based source column for human-facing output.
    pub column_number: u64,
    pub function_name: Option<String>,
}

/// One JavaScript stack frame with its optional original source location.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserStackFrame {
    pub script_id: String,
    pub function_name: String,
    pub generated: BrowserSourceLocation,
    pub original: Option<BrowserSourceLocation>,
    /// Description of the asynchronous parent boundary preceding this frame.
    pub async_parent: Option<String>,
}

/// Active document state captured after a mutating action.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserPageState {
    pub url: String,
    pub title: Option<String>,
    pub ready_state: BrowserDocumentReadyState,
}

/// `document.readyState` normalized into a closed typed set.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDocumentReadyState {
    Loading,
    Interactive,
    Complete,
    Unavailable,
}

/// Action-caused request activity observed before returning control.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserActionNetwork {
    pub request_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub navigation: bool,
    pub timed_out: bool,
    pub waited_ms: u64,
}

/// Optional semantic snapshot captured after a mutating action.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserPostActionSnapshot {
    Captured {
        origin: String,
        snapshot: String,
        refs: BTreeMap<String, BrowserElementReference>,
    },
    Unavailable {
        error: String,
    },
}

/// Complete state delta attached to a successful real-browser action.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserActionOutcome {
    pub page: BrowserPageState,
    pub network: BrowserActionNetwork,
    pub console: Vec<BrowserConsoleEntry>,
    pub errors: Vec<BrowserPageError>,
    pub dialog: Option<BrowserDialog>,
    pub downloads: Vec<BrowserDownload>,
    pub snapshot: Option<BrowserPostActionSnapshot>,
}

/// One compact matching window from a semantic snapshot search.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSnapshotMatch {
    /// One-based line number in the complete snapshot.
    pub line_number: usize,
    pub text: String,
    /// Structural ancestors from outermost to innermost.
    pub ancestors: Vec<String>,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// One HTTP header. Sensitive cookie and authorization values are deliberately
/// omitted while retaining their names and presence.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHttpHeader {
    pub name: String,
    pub value: Option<String>,
    pub sensitive: bool,
}

/// JavaScript source location responsible for a network request.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserNetworkCallFrame {
    pub function_name: String,
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
}

/// Request initiator information captured by Chromium.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserNetworkInitiator {
    pub kind: String,
    pub url: Option<String>,
    pub line_number: Option<f64>,
    pub column_number: Option<f64>,
    pub request_id: Option<String>,
    pub stack: Vec<BrowserNetworkCallFrame>,
}

/// Browser-provided request timing. Values other than `request_time` are
/// milliseconds relative to `request_time`; unavailable phases are negative.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserNetworkTiming {
    pub request_time: f64,
    pub proxy_start: f64,
    pub proxy_end: f64,
    pub dns_start: f64,
    pub dns_end: f64,
    pub connect_start: f64,
    pub connect_end: f64,
    pub ssl_start: f64,
    pub ssl_end: f64,
    pub send_start: f64,
    pub send_end: f64,
    pub receive_headers_start: f64,
    pub receive_headers_end: f64,
}

/// `DevTools` target that observed a request.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserNetworkContext {
    #[default]
    Page,
    ChildTarget,
}

/// Full lifecycle summary for one captured HTTP request or WebSocket connection.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent DevTools lifecycle and cache flags are part of the typed wire contract"
)]
pub struct BrowserNetworkRequest {
    /// Monotonic cursor assigned by the browser session.
    pub sequence: u64,
    /// Session-stable opaque identifier accepted by `network_body`.
    pub request_id: String,
    pub context: BrowserNetworkContext,
    /// Whether Chromium can retrieve this request's body on demand.
    pub body_available: bool,
    pub url: String,
    pub method: String,
    pub document_url: String,
    pub resource_type: String,
    pub started_at_epoch_ms: u64,
    pub duration_ms: Option<u64>,
    pub initiator: Option<BrowserNetworkInitiator>,
    pub request_headers: Vec<BrowserHttpHeader>,
    pub has_post_data: bool,
    pub status: Option<i64>,
    pub status_text: Option<String>,
    pub response_headers: Vec<BrowserHttpHeader>,
    pub mime_type: Option<String>,
    pub charset: Option<String>,
    pub protocol: Option<String>,
    pub remote_ip_address: Option<String>,
    pub remote_port: Option<i64>,
    pub from_disk_cache: bool,
    pub from_service_worker: bool,
    pub encoded_data_length: Option<u64>,
    pub timing: Option<BrowserNetworkTiming>,
    pub completed: bool,
    /// `DevTools` failure text when the request did not receive a response.
    pub failure: Option<String>,
}

/// Side of a network exchange read by `network_body`.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserNetworkBodyKind {
    Request,
    Response,
}

/// Direction of one captured WebSocket message.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWebSocketDirection {
    Sent,
    Received,
}

/// One complete WebSocket message captured by Chromium.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserWebSocketMessage {
    pub sequence: u64,
    pub request_id: String,
    pub direction: BrowserWebSocketDirection,
    pub timestamp_ms: u64,
    pub opcode: u8,
    pub payload: String,
    pub base64_encoded: bool,
}

/// React instrumentation installed before application code in each document.
///
/// Instrumentation is opt-in for library consumers because it hooks React's
/// `DevTools` boundary. The Nanocodex CLI enables it for managed browser sessions
/// so application code does not need to import or configure React Scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReactDiagnostics {
    include_profiling_hooks: bool,
}

impl Default for ReactDiagnostics {
    fn default() -> Self {
        Self::full()
    }
}

impl ReactDiagnostics {
    /// Creates full React diagnostics, including renderer profiling hooks when
    /// the page's React build exposes them.
    #[must_use]
    pub const fn full() -> Self {
        Self {
            include_profiling_hooks: true,
        }
    }

    /// Controls React's optional profiling-hook attachment.
    ///
    /// Commit trees and render causes remain available when this is disabled.
    #[must_use]
    pub const fn profiling_hooks(mut self, enabled: bool) -> Self {
        self.include_profiling_hooks = enabled;
        self
    }

    const fn include_profiling_hooks(self) -> bool {
        self.include_profiling_hooks
    }
}

/// Why renderer-level React profiling hooks are unavailable.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserReactProfilingHooksUnavailableReason {
    NoInjectMethod,
    Threw,
    OptedOut,
}

/// React renderer capabilities discovered in the active document.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactRenderer {
    pub version: String,
    /// React bundle type: zero is production and one is development.
    pub bundle_type: Option<i64>,
    pub profiling_hooks_available: Option<bool>,
    pub profiling_hooks_unavailable_reason: Option<BrowserReactProfilingHooksUnavailableReason>,
    pub profiling_hooks_error: Option<String>,
}

/// Current React instrumentation status for the active document.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactStatus {
    pub enabled: bool,
    pub active: bool,
    pub renderer_count: usize,
    pub renderers: Vec<BrowserReactRenderer>,
    pub document_url: String,
    pub time_origin_ms: Option<f64>,
}

/// Source location associated with a React fiber.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactSource {
    pub file_name: String,
    pub line_number: Option<u32>,
    pub column_number: Option<u32>,
    pub function_name: Option<String>,
}

/// One symbolicated owner-stack frame associated with a rendered element.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactOwnerFrame {
    pub file_name: Option<String>,
    pub line_number: Option<u32>,
    pub column_number: Option<u32>,
    pub function_name: Option<String>,
    pub source: Option<String>,
    pub is_server: bool,
    pub is_symbolicated: bool,
    pub is_ignore_listed: bool,
}

/// Bounded source and rendering context for one DOM element.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserElementContext {
    /// React Grab's compact component/markup context.
    pub snippet: String,
    /// Bounded HTML preview of the selected element.
    pub html_preview: String,
    /// Owning React component, when the element belongs to a React tree.
    pub component_name: Option<String>,
    /// Best resolved component source location.
    pub source: Option<BrowserReactSource>,
    /// Symbolicated component owner stack, nearest owner first.
    pub owner_stack: Vec<BrowserReactOwnerFrame>,
    /// Bounded formatted owner context, including fallbacks from an app-owned React Grab runtime.
    pub owner_stack_text: String,
    /// Stable selector, including shadow-root and same-origin-frame boundaries.
    pub selector: Option<String>,
    /// Bounded CSS needed to reproduce the element's rendered appearance.
    pub styles: String,
}

/// Typed explanation of why one React fiber rendered.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent React render causes are part of the typed wire contract"
)]
pub struct BrowserReactChangeDescription {
    pub is_first_mount: bool,
    pub props: Option<Vec<String>>,
    pub state: bool,
    pub context: bool,
    pub hooks: Vec<u32>,
    pub parent: bool,
}

/// One React fiber summarized at commit time.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactFiber {
    pub name: String,
    pub depth: u32,
    pub tag: i64,
    pub actual_duration_ms: f64,
    pub actual_start_time_ms: f64,
    pub self_base_duration_ms: f64,
    pub tree_base_duration_ms: f64,
    pub fiber_id: Option<u64>,
    pub source: Option<BrowserReactSource>,
    pub owner_name: Option<String>,
    pub change_description: Option<BrowserReactChangeDescription>,
}

/// Stable kind for one React instrumentation event.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserReactEventKind {
    RendererInjected,
    ProfilingHooksStatus,
    Commit,
    PostCommit,
    FiberUnmount,
    CommitStart,
    CommitStop,
    RenderStart,
    RenderYield,
    RenderStop,
    RenderScheduled,
    LayoutEffectsStart,
    LayoutEffectsStop,
    PassiveEffectsStart,
    PassiveEffectsStop,
    ComponentRenderStart,
    ComponentRenderStop,
    ComponentLayoutEffectMountStart,
    ComponentLayoutEffectMountStop,
    ComponentLayoutEffectUnmountStart,
    ComponentLayoutEffectUnmountStop,
    ComponentPassiveEffectMountStart,
    ComponentPassiveEffectMountStop,
    ComponentPassiveEffectUnmountStart,
    ComponentPassiveEffectUnmountStop,
    StateUpdate,
    ForceUpdate,
    ComponentSuspended,
    ComponentErrored,
}

/// One normalized event from React Scan's lightweight instrumentation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserReactEvent {
    pub sequence: u64,
    pub kind: BrowserReactEventKind,
    pub timestamp_ms: f64,
    pub component_name: Option<String>,
    pub lanes: Option<u32>,
    pub lane_labels: Vec<String>,
    pub renderer_id: Option<u32>,
    pub priority_level: Option<u8>,
    pub priority_name: Option<String>,
    pub did_error: bool,
    pub tree: Vec<BrowserReactFiber>,
    pub message: Option<String>,
    pub renderer_version: Option<String>,
    pub renderer_bundle_type: Option<i64>,
    pub profiling_hooks_available: Option<bool>,
    pub profiling_hooks_unavailable_reason: Option<BrowserReactProfilingHooksUnavailableReason>,
    pub profiling_hooks_error: Option<String>,
}

/// Visible gate currently replacing or blocking the requested page.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserGate {
    /// No supported visible gate signal was found.
    Clear,
    /// A CAPTCHA or explicit human-verification prompt is visible.
    Captcha { evidence: String },
    /// A JavaScript/browser-check interstitial is visible.
    JsChallenge { evidence: String },
    /// The page visibly denies access.
    AccessDenied { evidence: String },
}

/// A virtual platform passkey authenticator owned by the browser session.
///
/// This is harness policy rather than a model-callable browser action. The
/// driver installs it after the first navigation so `WebAuthn` requests never
/// fall through to the host's passkey UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualAuthenticator {
    credential_store: Option<std::path::PathBuf>,
}

impl VirtualAuthenticator {
    /// Creates a CTAP2 platform authenticator suitable for unattended passkey
    /// registration and authentication.
    #[must_use]
    pub const fn platform_passkey() -> Self {
        Self {
            credential_store: None,
        }
    }

    /// Persists virtual passkeys at the supplied path and restores them in
    /// later browser sessions.
    ///
    /// The file contains credential private keys. The file and a newly created
    /// immediate parent directory are restricted to the current user on Unix.
    #[must_use]
    pub fn credential_store(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.credential_store = Some(path.into());
        self
    }

    pub(crate) fn credential_store_path(&self) -> Option<&std::path::Path> {
        self.credential_store.as_deref()
    }
}

/// Public, non-secret metadata for a credential in the virtual authenticator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualCredential {
    /// Base64-encoded `WebAuthn` credential identifier.
    pub credential_id: String,
    /// Relying-party identifier that owns the credential.
    pub relying_party_id: Option<String>,
    /// Whether the authenticator retained this discoverable credential.
    pub is_resident: bool,
    /// `WebAuthn` user name, when the relying party supplied one.
    pub user_name: Option<String>,
    /// `WebAuthn` user display name, when the relying party supplied one.
    pub user_display_name: Option<String>,
    /// Number of successful assertions performed with this credential.
    pub sign_count: i64,
}

/// Typed result of one browser action.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserActionResult {
    /// A mutating action was accepted by the browser backend.
    Action {
        sequence: u64,
        action: BrowserActionName,
        executed: bool,
        /// Present for a real browser and absent for the recording backend.
        outcome: Option<Box<BrowserActionOutcome>>,
    },
    /// Accessibility-tree text and its stable element references.
    Snapshot {
        sequence: u64,
        executed: bool,
        origin: String,
        snapshot: String,
        refs: BTreeMap<String, BrowserElementReference>,
    },
    /// Bounded matching windows from a complete semantic snapshot.
    SnapshotFind {
        sequence: u64,
        executed: bool,
        origin: String,
        query: String,
        matches: Vec<BrowserSnapshotMatch>,
        refs: BTreeMap<String, BrowserElementReference>,
        truncated: bool,
    },
    /// Complete DOM, shadow-tree, iframe, and layout state.
    DomSnapshot {
        sequence: u64,
        executed: bool,
        snapshot: Box<BrowserDomSnapshot>,
    },
    Gate {
        sequence: u64,
        executed: bool,
        gate: BrowserGate,
    },
    Screenshot {
        sequence: u64,
        executed: bool,
        path: std::path::PathBuf,
        image: Option<BrowserImageArtifact>,
    },
    Pdf {
        sequence: u64,
        executed: bool,
        pdf: BrowserPdfArtifact,
    },
    VisualBaseline {
        sequence: u64,
        executed: bool,
        image: BrowserImageArtifact,
    },
    VisualDiff {
        sequence: u64,
        executed: bool,
        diff: BrowserVisualDiff,
    },
    VisualTrace {
        sequence: u64,
        executed: bool,
        trace: BrowserVisualTrace,
    },
    SessionTrace {
        sequence: u64,
        executed: bool,
        trace: BrowserSessionTrace,
    },
    Text {
        sequence: u64,
        executed: bool,
        text: String,
    },
    Html {
        sequence: u64,
        executed: bool,
        html: String,
    },
    Value {
        sequence: u64,
        executed: bool,
        value: Option<String>,
    },
    Attribute {
        sequence: u64,
        executed: bool,
        value: Option<String>,
    },
    Title {
        sequence: u64,
        executed: bool,
        title: String,
    },
    Url {
        sequence: u64,
        executed: bool,
        url: String,
    },
    Count {
        sequence: u64,
        executed: bool,
        count: u64,
    },
    Box {
        sequence: u64,
        executed: bool,
        bounds: Option<BrowserBoundingBox>,
    },
    Styles {
        sequence: u64,
        executed: bool,
        styles: Box<BrowserComputedStyles>,
    },
    MatchedStyles {
        sequence: u64,
        executed: bool,
        styles: BrowserMatchedStyles,
    },
    EventListeners {
        sequence: u64,
        executed: bool,
        listeners: Vec<BrowserEventListener>,
    },
    Breakpoint {
        sequence: u64,
        executed: bool,
        breakpoint: BrowserBreakpoint,
    },
    DebuggerPause {
        sequence: u64,
        executed: bool,
        pause: Option<BrowserDebuggerPause>,
    },
    Storage {
        sequence: u64,
        executed: bool,
        storage: BrowserStorageReport,
    },
    Console {
        sequence: u64,
        executed: bool,
        entries: Vec<BrowserConsoleEntry>,
        /// Number of retained entries before applying the requested limit.
        total: usize,
        /// Older entries discarded by bounded session retention.
        dropped: u64,
    },
    Errors {
        sequence: u64,
        executed: bool,
        errors: Vec<BrowserPageError>,
        /// Number of retained errors before applying the requested limit.
        total: usize,
        /// Older errors discarded by bounded session retention.
        dropped: u64,
    },
    NetworkRequests {
        sequence: u64,
        executed: bool,
        requests: Vec<BrowserNetworkRequest>,
        /// Number of retained matching requests, independent of the cursor and limit.
        total: usize,
        /// Older requests discarded by bounded session retention.
        dropped: u64,
        /// Sequence of the final returned request, suitable as the next `after` cursor.
        last_sequence: Option<u64>,
        /// Whether more matching retained requests follow the returned page.
        has_more: bool,
    },
    NetworkBody {
        sequence: u64,
        executed: bool,
        request_id: String,
        kind: BrowserNetworkBodyKind,
        body: String,
        base64_encoded: bool,
    },
    WebSocketMessages {
        sequence: u64,
        executed: bool,
        messages: Vec<BrowserWebSocketMessage>,
        /// Number of retained matching messages, independent of the cursor and limit.
        total: usize,
        /// Older messages discarded by bounded session retention.
        dropped: u64,
        /// Sequence of the final returned message, suitable as the next `after` cursor.
        last_sequence: Option<u64>,
        /// Whether more matching retained messages follow the returned page.
        has_more: bool,
    },
    ReactEvents {
        sequence: u64,
        executed: bool,
        status: BrowserReactStatus,
        events: Vec<BrowserReactEvent>,
        /// Number of retained events, independent of the cursor and limit.
        total: usize,
        /// Older events discarded by bounded document retention.
        dropped: u64,
        /// Sequence of the final returned event, suitable as the next `after` cursor.
        last_sequence: Option<u64>,
        /// Whether more retained events follow the returned page.
        has_more: bool,
    },
    ElementContext {
        sequence: u64,
        executed: bool,
        context: BrowserElementContext,
    },
    WebVitals {
        sequence: u64,
        executed: bool,
        vitals: BrowserWebVitals,
    },
    PerformanceTrace {
        sequence: u64,
        executed: bool,
        trace: BrowserPerformanceTrace,
    },
    CpuProfile {
        sequence: u64,
        executed: bool,
        profile: BrowserCpuProfile,
    },
    Coverage {
        sequence: u64,
        executed: bool,
        coverage: BrowserCoverage,
    },
    HeapSnapshot {
        sequence: u64,
        executed: bool,
        snapshot: BrowserHeapSnapshot,
    },
    HeapComparison {
        sequence: u64,
        executed: bool,
        comparison: BrowserHeapComparison,
    },
    HeapRetainers {
        sequence: u64,
        executed: bool,
        retainers: BrowserHeapRetainers,
    },
    HeapInspection {
        sequence: u64,
        executed: bool,
        inspection: BrowserHeapInspection,
    },
    Video {
        sequence: u64,
        executed: bool,
        video: BrowserVideoArtifact,
    },
    Frames {
        sequence: u64,
        executed: bool,
        frames: Vec<BrowserFrame>,
    },
    Tabs {
        sequence: u64,
        executed: bool,
        tabs: Vec<BrowserTab>,
    },
    Dialog {
        sequence: u64,
        executed: bool,
        dialog: Option<BrowserDialog>,
    },
    Har {
        sequence: u64,
        executed: bool,
        har: BrowserHarArtifact,
    },
    Accessibility {
        sequence: u64,
        executed: bool,
        audit: BrowserAccessibilityAudit,
    },
    Axe {
        sequence: u64,
        executed: bool,
        audit: BrowserAxeAudit,
    },
    Lighthouse {
        sequence: u64,
        executed: bool,
        report: BrowserLighthouseReport,
    },
    Crux {
        sequence: u64,
        executed: bool,
        report: BrowserCruxReport,
    },
    Downloads {
        sequence: u64,
        executed: bool,
        downloads: Vec<BrowserDownload>,
    },
    /// `evaluate_frame` is an intentionally dynamic frame-scoped boundary.
    FrameEvaluation {
        sequence: u64,
        executed: bool,
        frame_id: String,
        value: serde_json::Value,
    },
    /// `evaluate` is the sole intentionally dynamic result boundary.
    Evaluation {
        sequence: u64,
        executed: bool,
        value: serde_json::Value,
    },
}

impl BrowserActionResult {
    const fn action_name(&self) -> BrowserActionName {
        match self {
            Self::Action { action, .. } => *action,
            Self::Snapshot { .. } => BrowserActionName::Snapshot,
            Self::SnapshotFind { .. } => BrowserActionName::SnapshotFind,
            Self::DomSnapshot { .. } => BrowserActionName::DomSnapshot,
            Self::Gate { .. } => BrowserActionName::DetectGate,
            Self::Screenshot { .. } => BrowserActionName::Screenshot,
            Self::Pdf { .. } => BrowserActionName::Pdf,
            Self::VisualBaseline { .. } => BrowserActionName::VisualBaseline,
            Self::VisualDiff { .. } => BrowserActionName::VisualDiff,
            Self::VisualTrace { .. } => BrowserActionName::VisualTraceStop,
            Self::SessionTrace { .. } => BrowserActionName::SessionTraceStop,
            Self::Text { .. } => BrowserActionName::GetText,
            Self::Html { .. } => BrowserActionName::GetHtml,
            Self::Value { .. } => BrowserActionName::GetValue,
            Self::Attribute { .. } => BrowserActionName::GetAttribute,
            Self::Title { .. } => BrowserActionName::GetTitle,
            Self::Url { .. } => BrowserActionName::GetUrl,
            Self::Count { .. } => BrowserActionName::GetCount,
            Self::Box { .. } => BrowserActionName::GetBox,
            Self::Styles { .. } => BrowserActionName::GetStyles,
            Self::MatchedStyles { .. } => BrowserActionName::MatchedStyles,
            Self::EventListeners { .. } => BrowserActionName::EventListeners,
            Self::Breakpoint { .. } => BrowserActionName::DebuggerSetBreakpoint,
            Self::DebuggerPause { .. } => BrowserActionName::DebuggerPaused,
            Self::Storage { .. } => BrowserActionName::StorageInspect,
            Self::Console { .. } => BrowserActionName::Console,
            Self::Errors { .. } => BrowserActionName::Errors,
            Self::NetworkRequests { .. } => BrowserActionName::NetworkRequests,
            Self::NetworkBody { .. } => BrowserActionName::NetworkBody,
            Self::WebSocketMessages { .. } => BrowserActionName::WebSocketMessages,
            Self::ReactEvents { .. } => BrowserActionName::ReactEvents,
            Self::ElementContext { .. } => BrowserActionName::ElementContext,
            Self::WebVitals { .. } => BrowserActionName::WebVitals,
            Self::PerformanceTrace { .. } => BrowserActionName::PerformanceTraceStop,
            Self::CpuProfile { .. } => BrowserActionName::CpuProfileStop,
            Self::Coverage { .. } => BrowserActionName::CoverageStop,
            Self::HeapSnapshot { .. } => BrowserActionName::HeapSnapshot,
            Self::HeapComparison { .. } => BrowserActionName::HeapCompare,
            Self::HeapRetainers { .. } => BrowserActionName::HeapRetainers,
            Self::HeapInspection { .. } => BrowserActionName::HeapInspect,
            Self::Video { .. } => BrowserActionName::VideoStop,
            Self::Frames { .. } => BrowserActionName::ListFrames,
            Self::Tabs { .. } => BrowserActionName::ListTabs,
            Self::Dialog { .. } => BrowserActionName::Dialog,
            Self::Har { .. } => BrowserActionName::ExportHar,
            Self::Accessibility { .. } => BrowserActionName::AccessibilityAudit,
            Self::Axe { .. } => BrowserActionName::AxeAudit,
            Self::Lighthouse { .. } => BrowserActionName::LighthouseAudit,
            Self::Crux { .. } => BrowserActionName::Crux,
            Self::Downloads { .. } => BrowserActionName::Downloads,
            Self::FrameEvaluation { .. } => BrowserActionName::EvaluateFrame,
            Self::Evaluation { .. } => BrowserActionName::Evaluate,
        }
    }

    fn image_paths(&self) -> Vec<&std::path::Path> {
        match self {
            Self::Screenshot {
                image: Some(image), ..
            }
            | Self::VisualBaseline { image, .. } => vec![image.path.as_path()],
            Self::VisualDiff { diff, .. } => vec![diff.diff_path.as_path()],
            Self::VisualTrace { trace, .. } => trace
                .anomalies
                .iter()
                .take(3)
                .map(|anomaly| anomaly.frame_path.as_path())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn attach_model_images(&mut self, images: &BTreeMap<std::path::PathBuf, BrowserModelImage>) {
        match self {
            Self::Screenshot {
                image: Some(image), ..
            }
            | Self::VisualBaseline { image, .. } => {
                image.model_image = images.get(&image.path).cloned();
            }
            Self::VisualDiff { diff, .. } => {
                diff.model_image = images.get(&diff.diff_path).cloned();
            }
            Self::VisualTrace { trace, .. } => {
                for anomaly in &mut trace.anomalies {
                    anomaly.model_image = images.get(&anomaly.frame_path).cloned();
                }
            }
            _ => {}
        }
    }
}

/// One ordered entry in a replayable [`BrowserSessionTrace`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSessionTraceEntry {
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub action: BrowserAction,
    pub outcome: BrowserSessionTraceOutcome,
    pub screenshot_path: Option<std::path::PathBuf>,
    pub dom_snapshot_path: Option<std::path::PathBuf>,
    pub capture_errors: Vec<String>,
}

/// Recorded outcome of one browser action.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserSessionTraceOutcome {
    Success { result: Box<BrowserActionResult> },
    Failure { error: String },
}

impl BrowserSessionTrace {
    /// Reads the trace's ordered, fully typed action/result stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the trace file cannot be read or contains an
    /// invalid entry.
    pub async fn entries(&self) -> Result<Vec<BrowserSessionTraceEntry>, BrowserError> {
        let encoded = tokio::fs::read_to_string(&self.events_path).await?;
        encoded
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(BrowserError::from)
    }

    /// Copies this trace out of the session-private runtime directory.
    ///
    /// Paths embedded in the typed event stream are rewritten to the retained
    /// destination. The destination must not already exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination exists, an artifact path is
    /// malformed, or any trace file cannot be read, encoded, or copied.
    pub async fn persist(
        &self,
        destination: impl AsRef<std::path::Path>,
    ) -> Result<Self, BrowserError> {
        use tokio::io::AsyncWriteExt as _;

        let destination = destination.as_ref();
        let mut entries = self.entries().await?;
        tokio::fs::create_dir(destination).await?;
        for entry in &mut entries {
            entry.screenshot_path =
                persist_trace_artifact(entry.screenshot_path.take(), destination).await?;
            entry.dom_snapshot_path =
                persist_trace_artifact(entry.dom_snapshot_path.take(), destination).await?;
        }

        let events_path = destination.join("events.jsonl");
        let mut events = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&events_path)
            .await?;
        for entry in entries {
            let mut encoded = serde_json::to_vec(&entry)?;
            encoded.push(b'\n');
            events.write_all(&encoded).await?;
        }
        events.flush().await?;
        let network_path = destination.join("network.json");
        let diagnostics_path = destination.join("diagnostics.json");
        tokio::fs::copy(&self.network_path, &network_path).await?;
        tokio::fs::copy(&self.diagnostics_path, &diagnostics_path).await?;
        Ok(Self {
            directory: destination.to_owned(),
            events_path,
            network_path,
            diagnostics_path,
            action_count: self.action_count,
            screenshot_count: self.screenshot_count,
            dom_snapshot_count: self.dom_snapshot_count,
            duration_ms: self.duration_ms,
            truncated: self.truncated,
        })
    }
}

async fn persist_trace_artifact(
    source: Option<std::path::PathBuf>,
    destination: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, BrowserError> {
    let Some(source) = source else {
        return Ok(None);
    };
    let file_name = source
        .file_name()
        .ok_or_else(|| BrowserError::InvalidTraceArtifactPath {
            path: source.clone(),
        })?;
    let retained = destination.join(file_name);
    tokio::fs::copy(source, &retained).await?;
    Ok(Some(retained))
}

/// One action retained by [`BrowserRecording`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedBrowserAction {
    pub sequence: u64,
    pub action: BrowserAction,
}

#[derive(Default)]
struct RecordingState {
    next_sequence: u64,
    actions: Vec<RecordedBrowserAction>,
    current_url: String,
}

/// Read handle for actions received by a recording [`BrowserTool`].
#[derive(Clone, Default)]
pub struct BrowserRecording {
    state: Arc<Mutex<RecordingState>>,
}

impl BrowserRecording {
    /// Returns every recorded action in call order.
    ///
    /// # Errors
    ///
    /// Returns an error if another thread panicked while holding the recording
    /// lock.
    pub fn actions(&self) -> Result<Vec<RecordedBrowserAction>, BrowserRecordingError> {
        let state = self
            .state
            .lock()
            .map_err(|_| BrowserRecordingError::Poisoned)?;
        Ok(state.actions.clone())
    }

    fn record(&self, action: BrowserAction) -> Result<BrowserActionResult, BrowserRecordingError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BrowserRecordingError::Poisoned)?;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let action_name = action.name();
        if let BrowserAction::Open { url } = &action {
            state.current_url.clone_from(url);
        }
        let result = recording_result(sequence, &action, &state.current_url);
        trace_serialized("recording.action.input", &action);
        info!(
            target: "nanocodex_browser",
            sequence,
            action = ?action,
            "recorded no-op browser action"
        );
        trace_serialized("recording.action.result", &result);
        state
            .actions
            .push(RecordedBrowserAction { sequence, action });
        debug_assert_eq!(result.action_name(), action_name);
        Ok(result)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive one-action-to-one-result recording contract is clearest as one match"
)]
fn recording_result(
    sequence: u64,
    action: &BrowserAction,
    current_url: &str,
) -> BrowserActionResult {
    let executed = false;
    match action {
        BrowserAction::Snapshot { .. } => BrowserActionResult::Snapshot {
            sequence,
            executed,
            origin: current_url.to_owned(),
            snapshot: String::new(),
            refs: BTreeMap::new(),
        },
        BrowserAction::SnapshotFind { query, .. } => BrowserActionResult::SnapshotFind {
            sequence,
            executed,
            origin: current_url.to_owned(),
            query: query.clone(),
            matches: Vec::new(),
            refs: BTreeMap::new(),
            truncated: false,
        },
        BrowserAction::DomSnapshot { .. } => BrowserActionResult::DomSnapshot {
            sequence,
            executed,
            snapshot: Box::default(),
        },
        BrowserAction::DetectGate => BrowserActionResult::Gate {
            sequence,
            executed,
            gate: BrowserGate::Clear,
        },
        BrowserAction::GetText { .. } => BrowserActionResult::Text {
            sequence,
            executed,
            text: String::new(),
        },
        BrowserAction::GetHtml { .. } => BrowserActionResult::Html {
            sequence,
            executed,
            html: String::new(),
        },
        BrowserAction::GetValue { .. } => BrowserActionResult::Value {
            sequence,
            executed,
            value: None,
        },
        BrowserAction::GetAttribute { .. } => BrowserActionResult::Attribute {
            sequence,
            executed,
            value: None,
        },
        BrowserAction::GetTitle => BrowserActionResult::Title {
            sequence,
            executed,
            title: String::new(),
        },
        BrowserAction::GetUrl => BrowserActionResult::Url {
            sequence,
            executed,
            url: current_url.to_owned(),
        },
        BrowserAction::GetCount { .. } => BrowserActionResult::Count {
            sequence,
            executed,
            count: 0,
        },
        BrowserAction::GetBox { .. } => BrowserActionResult::Box {
            sequence,
            executed,
            bounds: None,
        },
        BrowserAction::GetStyles { .. } => BrowserActionResult::Styles {
            sequence,
            executed,
            styles: Box::default(),
        },
        BrowserAction::MatchedStyles { .. } => BrowserActionResult::MatchedStyles {
            sequence,
            executed,
            styles: BrowserMatchedStyles {
                inline_style: Vec::new(),
                attributes_style: Vec::new(),
                rules: Vec::new(),
            },
        },
        BrowserAction::EventListeners { .. } => BrowserActionResult::EventListeners {
            sequence,
            executed,
            listeners: Vec::new(),
        },
        BrowserAction::DebuggerSetBreakpoint {
            url,
            line_number,
            column_number,
            ..
        } => BrowserActionResult::Breakpoint {
            sequence,
            executed,
            breakpoint: BrowserBreakpoint {
                breakpoint_id: String::new(),
                url: url.clone(),
                line_number: *line_number,
                column_number: column_number.unwrap_or(1),
                resolved_locations: Vec::new(),
            },
        },
        BrowserAction::DebuggerPaused => BrowserActionResult::DebuggerPause {
            sequence,
            executed,
            pause: None,
        },
        BrowserAction::StorageInspect => BrowserActionResult::Storage {
            sequence,
            executed,
            storage: BrowserStorageReport {
                origin: current_url.to_owned(),
                service_workers: Vec::new(),
                caches: Vec::new(),
                indexed_db: Vec::new(),
                local_storage_keys: Vec::new(),
                session_storage_keys: Vec::new(),
            },
        },
        BrowserAction::Screenshot { .. } => BrowserActionResult::Screenshot {
            sequence,
            executed,
            path: std::path::PathBuf::new(),
            image: None,
        },
        BrowserAction::Pdf {
            tagged,
            document_outline,
            ..
        } => BrowserActionResult::Pdf {
            sequence,
            executed,
            pdf: BrowserPdfArtifact {
                path: std::path::PathBuf::new(),
                bytes: 0,
                tagged: *tagged,
                document_outline: *document_outline,
            },
        },
        BrowserAction::VisualBaseline { .. } => BrowserActionResult::VisualBaseline {
            sequence,
            executed,
            image: BrowserImageArtifact {
                artifact_id: String::new(),
                path: std::path::PathBuf::new(),
                mime_type: "image/png".to_owned(),
                width: 0,
                height: 0,
                model_image: None,
            },
        },
        BrowserAction::VisualDiff { baseline_id, .. } => BrowserActionResult::VisualDiff {
            sequence,
            executed,
            diff: BrowserVisualDiff {
                baseline_id: baseline_id.clone(),
                current_id: String::new(),
                changed_pixel_ratio: 0.0,
                mean_channel_delta: 0.0,
                maximum_channel_delta: 0,
                dimensions_match: true,
                diff_path: std::path::PathBuf::new(),
                model_image: None,
            },
        },
        BrowserAction::VisualTraceStop => BrowserActionResult::VisualTrace {
            sequence,
            executed,
            trace: BrowserVisualTrace {
                frame_count: 0,
                duration_ms: 0,
                dropped_frames: 0,
                maximum_changed_pixel_ratio: 0.0,
                cumulative_layout_shift: 0.0,
                anomalies: Vec::new(),
            },
        },
        BrowserAction::SessionTraceStop => BrowserActionResult::SessionTrace {
            sequence,
            executed,
            trace: BrowserSessionTrace {
                directory: std::path::PathBuf::new(),
                events_path: std::path::PathBuf::new(),
                network_path: std::path::PathBuf::new(),
                diagnostics_path: std::path::PathBuf::new(),
                action_count: 0,
                screenshot_count: 0,
                dom_snapshot_count: 0,
                duration_ms: 0,
                truncated: false,
            },
        },
        BrowserAction::Console { .. } => BrowserActionResult::Console {
            sequence,
            executed,
            entries: Vec::new(),
            total: 0,
            dropped: 0,
        },
        BrowserAction::Errors { .. } => BrowserActionResult::Errors {
            sequence,
            executed,
            errors: Vec::new(),
            total: 0,
            dropped: 0,
        },
        BrowserAction::NetworkRequests { .. } => BrowserActionResult::NetworkRequests {
            sequence,
            executed,
            requests: Vec::new(),
            total: 0,
            dropped: 0,
            last_sequence: None,
            has_more: false,
        },
        BrowserAction::NetworkBody {
            request_id, kind, ..
        } => BrowserActionResult::NetworkBody {
            sequence,
            executed,
            request_id: request_id.clone(),
            kind: *kind,
            body: String::new(),
            base64_encoded: false,
        },
        BrowserAction::WebSocketMessages { .. } => BrowserActionResult::WebSocketMessages {
            sequence,
            executed,
            messages: Vec::new(),
            total: 0,
            dropped: 0,
            last_sequence: None,
            has_more: false,
        },
        BrowserAction::ReactEvents { .. } => BrowserActionResult::ReactEvents {
            sequence,
            executed,
            status: BrowserReactStatus {
                enabled: false,
                active: false,
                renderer_count: 0,
                renderers: Vec::new(),
                document_url: current_url.to_owned(),
                time_origin_ms: None,
            },
            events: Vec::new(),
            total: 0,
            dropped: 0,
            last_sequence: None,
            has_more: false,
        },
        BrowserAction::ElementContext { target } => BrowserActionResult::ElementContext {
            sequence,
            executed,
            context: BrowserElementContext {
                snippet: String::new(),
                html_preview: String::new(),
                component_name: None,
                source: None,
                owner_stack: Vec::new(),
                owner_stack_text: String::new(),
                selector: Some(target.display()),
                styles: String::new(),
            },
        },
        BrowserAction::WebVitals => BrowserActionResult::WebVitals {
            sequence,
            executed,
            vitals: BrowserWebVitals {
                url: current_url.to_owned(),
                first_contentful_paint_ms: None,
                largest_contentful_paint_ms: None,
                cumulative_layout_shift: 0.0,
                interaction_to_next_paint_ms: None,
                time_to_first_byte_ms: None,
                dom_content_loaded_ms: None,
                load_ms: None,
                long_task_count: 0,
                total_blocking_time_ms: 0.0,
                resource_count: 0,
                transferred_bytes: 0,
            },
        },
        BrowserAction::PerformanceTraceStop => BrowserActionResult::PerformanceTrace {
            sequence,
            executed,
            trace: BrowserPerformanceTrace {
                path: std::path::PathBuf::new(),
                event_count: 0,
                duration_ms: 0.0,
                scripting_ms: 0.0,
                rendering_ms: 0.0,
                painting_ms: 0.0,
                long_task_count: 0,
                longest_task_ms: 0.0,
                insights: Vec::new(),
            },
        },
        BrowserAction::CpuProfileStop => BrowserActionResult::CpuProfile {
            sequence,
            executed,
            profile: BrowserCpuProfile {
                path: std::path::PathBuf::new(),
                duration_ms: 0.0,
                sample_count: 0,
                functions: Vec::new(),
            },
        },
        BrowserAction::CoverageStop => BrowserActionResult::Coverage {
            sequence,
            executed,
            coverage: BrowserCoverage {
                path: std::path::PathBuf::new(),
                scripts: Vec::new(),
                total_bytes: 0,
                used_bytes: 0,
                unused_bytes: 0,
            },
        },
        BrowserAction::HeapSnapshot { .. } => BrowserActionResult::HeapSnapshot {
            sequence,
            executed,
            snapshot: BrowserHeapSnapshot {
                artifact_id: String::new(),
                path: std::path::PathBuf::new(),
                node_count: 0,
                total_self_size: 0,
                classes: Vec::new(),
            },
        },
        BrowserAction::HeapCompare {
            before_id,
            after_id,
        } => BrowserActionResult::HeapComparison {
            sequence,
            executed,
            comparison: BrowserHeapComparison {
                before_id: before_id.clone(),
                after_id: after_id.clone(),
                node_count_delta: 0,
                self_size_delta: 0,
                growing_classes: Vec::new(),
            },
        },
        BrowserAction::HeapRetainers {
            artifact_id,
            node_id,
            ..
        } => BrowserActionResult::HeapRetainers {
            sequence,
            executed,
            retainers: BrowserHeapRetainers {
                artifact_id: artifact_id.clone(),
                target_node_id: *node_id,
                nodes: Vec::new(),
                truncated: false,
            },
        },
        BrowserAction::HeapInspect { artifact_id, .. } => BrowserActionResult::HeapInspection {
            sequence,
            executed,
            inspection: BrowserHeapInspection {
                artifact_id: artifact_id.clone(),
                matching_node_count: 0,
                nodes: Vec::new(),
                duplicate_strings: Vec::new(),
                truncated: false,
            },
        },
        BrowserAction::VideoStop => BrowserActionResult::Video {
            sequence,
            executed,
            video: BrowserVideoArtifact {
                path: std::path::PathBuf::new(),
                frame_count: 0,
                duration_ms: 0,
                width: 0,
                height: 0,
            },
        },
        BrowserAction::ListFrames => BrowserActionResult::Frames {
            sequence,
            executed,
            frames: Vec::new(),
        },
        BrowserAction::EvaluateFrame { frame_id, .. } => BrowserActionResult::FrameEvaluation {
            sequence,
            executed,
            frame_id: frame_id.clone(),
            value: serde_json::Value::Null,
        },
        BrowserAction::ListTabs => BrowserActionResult::Tabs {
            sequence,
            executed,
            tabs: Vec::new(),
        },
        BrowserAction::Dialog => BrowserActionResult::Dialog {
            sequence,
            executed,
            dialog: None,
        },
        BrowserAction::ExportHar { .. } => BrowserActionResult::Har {
            sequence,
            executed,
            har: BrowserHarArtifact {
                path: std::path::PathBuf::new(),
                entry_count: 0,
                body_count: 0,
            },
        },
        BrowserAction::AccessibilityAudit => BrowserActionResult::Accessibility {
            sequence,
            executed,
            audit: BrowserAccessibilityAudit {
                url: current_url.to_owned(),
                checked_elements: 0,
                violations: Vec::new(),
            },
        },
        BrowserAction::AxeAudit => BrowserActionResult::Axe {
            sequence,
            executed,
            audit: BrowserAxeAudit {
                url: current_url.to_owned(),
                engine_version: String::new(),
                violations: Vec::new(),
                incomplete: Vec::new(),
                pass_count: 0,
                inapplicable_count: 0,
                truncated: false,
            },
        },
        BrowserAction::LighthouseAudit { .. } => BrowserActionResult::Lighthouse {
            sequence,
            executed,
            report: BrowserLighthouseReport {
                path: std::path::PathBuf::new(),
                lighthouse_version: String::new(),
                requested_url: current_url.to_owned(),
                final_url: current_url.to_owned(),
                fetch_time: String::new(),
                categories: Vec::new(),
                findings: Vec::new(),
                omitted_finding_count: 0,
            },
        },
        BrowserAction::Crux { form_factor, .. } => BrowserActionResult::Crux {
            sequence,
            executed,
            report: BrowserCruxReport {
                requested_url: current_url.to_owned(),
                record_url: None,
                record_origin: None,
                form_factor: *form_factor,
                collection_period: None,
                metrics: Vec::new(),
                normalized_url: None,
            },
        },
        BrowserAction::Downloads => BrowserActionResult::Downloads {
            sequence,
            executed,
            downloads: Vec::new(),
        },
        BrowserAction::Evaluate { .. } => BrowserActionResult::Evaluation {
            sequence,
            executed,
            value: serde_json::Value::Null,
        },
        BrowserAction::Open { .. }
        | BrowserAction::Reload
        | BrowserAction::Click { .. }
        | BrowserAction::Fill { .. }
        | BrowserAction::Press { .. }
        | BrowserAction::Hover { .. }
        | BrowserAction::MouseMove { .. }
        | BrowserAction::MouseDown { .. }
        | BrowserAction::MouseUp { .. }
        | BrowserAction::MouseWheel { .. }
        | BrowserAction::TouchTap { .. }
        | BrowserAction::TouchSwipe { .. }
        | BrowserAction::KeyboardDown { .. }
        | BrowserAction::KeyboardUp { .. }
        | BrowserAction::InsertText { .. }
        | BrowserAction::Scroll { .. }
        | BrowserAction::ForcePseudoState { .. }
        | BrowserAction::DebuggerSetPauseOnExceptions { .. }
        | BrowserAction::DebuggerRemoveBreakpoint { .. }
        | BrowserAction::DebuggerResume
        | BrowserAction::DebuggerStepOver
        | BrowserAction::DebuggerStepInto
        | BrowserAction::DebuggerStepOut
        | BrowserAction::SelectOption { .. }
        | BrowserAction::SetChecked { .. }
        | BrowserAction::Drag { .. }
        | BrowserAction::UploadFiles { .. }
        | BrowserAction::SetViewport { .. }
        | BrowserAction::GoBack
        | BrowserAction::GoForward
        | BrowserAction::WaitForSelector { .. }
        | BrowserAction::WaitForText { .. }
        | BrowserAction::WaitForUrl { .. }
        | BrowserAction::WaitForLoadState { .. }
        | BrowserAction::WaitForFunction { .. }
        | BrowserAction::WaitForTimeout { .. }
        | BrowserAction::VisualTraceStart { .. }
        | BrowserAction::SessionTraceStart { .. }
        | BrowserAction::PerformanceTraceStart
        | BrowserAction::CpuProfileStart
        | BrowserAction::CoverageStart
        | BrowserAction::VideoStart { .. }
        | BrowserAction::NewTab { .. }
        | BrowserAction::SelectTab { .. }
        | BrowserAction::CloseTab { .. }
        | BrowserAction::HandleDialog { .. }
        | BrowserAction::NetworkRoute { .. }
        | BrowserAction::RemoveNetworkRoute { .. }
        | BrowserAction::ClearNetworkRoutes
        | BrowserAction::SetOffline { .. } => BrowserActionResult::Action {
            sequence,
            action: action.name(),
            executed,
            outcome: None,
        },
    }
}

/// Failure while accessing a no-op browser recording.
#[derive(Debug, thiserror::Error)]
pub enum BrowserRecordingError {
    #[error("browser action recording lock was poisoned")]
    Poisoned,
}

enum BrowserBackend {
    Managed(Browser),
    Recording(BrowserRecording),
}

/// A cloneable handle to one owned in-process Chromium session.
#[derive(Clone)]
pub struct Browser {
    inner: Arc<native::NativeBrowser>,
}

/// A validated authentication handoff that has not opened the user's Brave yet.
///
/// This is a harness operation, not a model-callable [`BrowserAction`]. Opening
/// consumes this value so callers cannot accidentally open arbitrary pages or
/// resume a handoff that was never presented to the user.
pub struct BraveAuthHandoff {
    browser: Browser,
    url: url::Url,
}

/// An authentication page opened in the user's ordinary Brave profile.
///
/// After the user completes the passkey or other authentication gate, call
/// [`resume`](Self::resume) to take a fresh allowlisted cookie snapshot and
/// reopen the protected URL in the dedicated browser. A local browser is
/// relaunched with the new snapshot; a remote CDP browser receives refreshed
/// cookies without restarting.
pub struct OpenedBraveAuthHandoff {
    browser: Browser,
    url: url::Url,
}

impl BraveAuthHandoff {
    /// Opens only the validated protected URL in the user's ordinary Brave.
    ///
    /// The passkey ceremony remains entirely in the user's browser. Nanocodex
    /// does not receive credential material or control the visible page.
    ///
    /// # Errors
    ///
    /// Returns an error if Brave cannot be started or the URL is no longer
    /// allowed by this browser's session policy.
    pub fn open(self) -> Result<OpenedBraveAuthHandoff, BrowserError> {
        self.browser.inner.open_auth_handoff(&self.url)?;
        Ok(OpenedBraveAuthHandoff {
            browser: self.browser,
            url: self.url,
        })
    }
}

impl OpenedBraveAuthHandoff {
    /// Refreshes the private headless session after the caller observes that
    /// the user has completed authentication.
    ///
    /// A fresh filtered cookie snapshot is copied from Brave and the protected
    /// URL is reopened before this future completes. A locally launched
    /// browser discards its old private process and profile. A remote CDP
    /// browser remains alive and receives a replacement cookie set.
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be refreshed, the fresh snapshot
    /// cannot be prepared, or the protected page cannot be reopened.
    pub async fn resume(self) -> Result<(), BrowserError> {
        self.browser.inner.resume_auth_handoff(self.url).await
    }
}

/// Configuration for one isolated [`Browser`] session.
#[derive(Clone, Debug, Default)]
pub struct BrowserBuilder {
    executable: Option<std::path::PathBuf>,
    cdp_endpoint: Option<url::Url>,
    brave_session: Option<BraveSession>,
    launch_brave_executable: bool,
    virtual_authenticator: Option<VirtualAuthenticator>,
    react_diagnostics: Option<ReactDiagnostics>,
    egress_policy: Option<BrowserEgressPolicy>,
    file_root: Option<std::path::PathBuf>,
    context: BrowserContext,
    storage_state: Option<BrowserStorageState>,
    after_action: BrowserAfterAction,
    ffmpeg_executable: Option<std::path::PathBuf>,
    lighthouse_executable: Option<std::path::PathBuf>,
    crux_client: Option<BrowserCruxClient>,
}

impl BrowserBuilder {
    /// Uses an explicit Chrome or Chromium executable instead of auto-detection.
    #[must_use]
    pub fn executable(mut self, executable: impl Into<std::path::PathBuf>) -> Self {
        self.executable = Some(executable.into());
        self
    }

    /// Connects to a dedicated Chromium instance through its `DevTools` endpoint.
    ///
    /// This is the boundary for Chrome running in a VM, virtual display, or
    /// managed browser service. The endpoint must use HTTP(S) or WebSocket(S).
    #[must_use]
    pub fn cdp_endpoint(mut self, endpoint: url::Url) -> Self {
        self.cdp_endpoint = Some(endpoint);
        self
    }

    /// Seeds the dedicated browser with allowlisted cookies from Brave.
    ///
    /// The source profile is never launched or mutated. Its live cookie
    /// database is copied through `SQLite`'s online backup API and filtered to
    /// the explicitly allowed origins. A locally launched browser opens that
    /// private profile directly. A remote CDP browser receives decrypted,
    /// typed cookies from a short-lived invisible Brave broker.
    #[must_use]
    pub fn brave_session(mut self, session: BraveSession) -> Self {
        self.brave_session = Some(session);
        self.launch_brave_executable = true;
        self
    }

    /// Seeds the selected browser with cookies from a Chromium-family profile.
    ///
    /// Unlike [`Self::brave_session`], this does not select Brave as the
    /// browser executable. An explicitly configured Chrome or Chromium binary,
    /// or the normal auto-detected browser, launches with the private cookie
    /// snapshot instead.
    #[must_use]
    pub fn cookie_source(mut self, session: BraveSession) -> Self {
        self.brave_session = Some(session);
        self.launch_brave_executable = false;
        self
    }

    /// Enables a harness-owned virtual authenticator for passkey flows.
    #[must_use]
    pub fn virtual_authenticator(mut self, virtual_authenticator: VirtualAuthenticator) -> Self {
        self.virtual_authenticator = Some(virtual_authenticator);
        self
    }

    /// Instruments React before application scripts without modifying the app.
    ///
    /// The active page can then expose typed renderer, commit, timing, source,
    /// and render-cause events through [`BrowserAction::ReactEvents`].
    #[must_use]
    pub const fn react_diagnostics(mut self, diagnostics: ReactDiagnostics) -> Self {
        self.react_diagnostics = Some(diagnostics);
        self
    }

    /// Restricts every page, frame, and worker request to an explicit policy.
    ///
    /// WebRTC is disabled for restricted sessions so it cannot bypass the
    /// request interceptor. A browser seeded from Brave defaults to the exact
    /// cookie origins when no explicit policy is supplied.
    #[must_use]
    pub fn egress_policy(mut self, policy: BrowserEgressPolicy) -> Self {
        self.egress_policy = Some(policy);
        self
    }

    /// Allows upload actions to read relative paths beneath one host directory.
    ///
    /// The root is canonicalized at build time and is never exposed as a
    /// model-callable setting. Uploads are unsupported across remote CDP
    /// because those paths would refer to another machine.
    #[must_use]
    pub fn file_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.file_root = Some(root.into());
        self
    }

    /// Installs deterministic browser policy before every page's first
    /// navigation.
    #[must_use]
    pub fn context(mut self, context: BrowserContext) -> Self {
        self.context = context;
        self
    }

    /// Seeds cookies and origin storage from harness-owned state.
    ///
    /// The values never enter the model-callable browser tool schema.
    #[must_use]
    pub fn storage_state(mut self, state: BrowserStorageState) -> Self {
        self.storage_state = Some(state);
        self
    }

    /// Selects whether mutating actions also refresh semantic page state.
    ///
    /// Real browsers always return a compact page/network/diagnostic summary.
    /// `Snapshot` additionally captures fresh interactive references.
    #[must_use]
    pub const fn after_action(mut self, after_action: BrowserAfterAction) -> Self {
        self.after_action = after_action;
        self
    }

    /// Selects the `ffmpeg` executable used only by explicit video actions.
    ///
    /// When omitted, `video_start` resolves `ffmpeg` from the process `PATH`.
    #[must_use]
    pub fn ffmpeg_executable(mut self, executable: impl Into<std::path::PathBuf>) -> Self {
        self.ffmpeg_executable = Some(executable.into());
        self
    }

    /// Selects the exact Lighthouse CLI used by explicit Lighthouse actions.
    ///
    /// Lighthouse attaches to the already-owned Chrome session and preserves
    /// its authenticated storage. It is never discovered or spawned unless the
    /// model or direct caller requests the audit.
    #[must_use]
    pub fn lighthouse_executable(mut self, executable: impl Into<std::path::PathBuf>) -> Self {
        self.lighthouse_executable = Some(executable.into());
        self
    }

    /// Enables explicit Chrome UX Report field-data queries.
    #[must_use]
    pub fn crux_client(mut self, client: BrowserCruxClient) -> Self {
        self.crux_client = Some(client);
        self
    }

    /// Builds a lazy browser handle without starting Chromium yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the private runtime directory cannot be created.
    pub fn build(self) -> Result<Browser, BrowserBuildError> {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        if self.cdp_endpoint.is_some() && self.executable.is_some() {
            return Err(BrowserBuildError::Configuration {
                message: "`cdp_endpoint` cannot be combined with `executable`".to_owned(),
            });
        }
        if let Some(endpoint) = &self.cdp_endpoint
            && !matches!(endpoint.scheme(), "http" | "https" | "ws" | "wss")
        {
            return Err(BrowserBuildError::Configuration {
                message: format!(
                    "`cdp_endpoint` requires http, https, ws, or wss; got {}",
                    endpoint.scheme()
                ),
            });
        }
        if let Some(session) = &self.brave_session {
            session.validate()?;
            if self.cdp_endpoint.is_some() && session.includes_site_data() {
                return Err(BrowserBuildError::Configuration {
                    message:
                        "`include_site_data` is not supported across a remote CDP boundary; cookies remain supported"
                            .to_owned(),
                });
            }
        }
        if self.brave_session.is_some() && self.storage_state.is_some() {
            return Err(BrowserBuildError::Configuration {
                message: "`brave_session` and `storage_state` cannot both seed one browser"
                    .to_owned(),
            });
        }
        if self.cdp_endpoint.is_some() && self.file_root.is_some() {
            return Err(BrowserBuildError::Configuration {
                message: "`file_root` is not supported across a remote CDP boundary".to_owned(),
            });
        }
        if self
            .virtual_authenticator
            .as_ref()
            .and_then(VirtualAuthenticator::credential_store_path)
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(BrowserBuildError::Configuration {
                message: "the virtual credential store path cannot be empty".to_owned(),
            });
        }
        if let Some(client) = &self.crux_client {
            if client.api_key.is_empty() {
                return Err(BrowserBuildError::Configuration {
                    message: "CrUX API key cannot be empty".to_owned(),
                });
            }
            if let Some(endpoint) = &client.endpoint
                && !matches!(endpoint.scheme(), "http" | "https")
            {
                return Err(BrowserBuildError::Configuration {
                    message: "CrUX endpoint requires HTTP or HTTPS".to_owned(),
                });
            }
        }
        validate_browser_context(&self.context)?;
        if let Some(state) = &self.storage_state {
            validate_storage_state(state)
                .map_err(|message| BrowserBuildError::Configuration { message })?;
        }
        let file_root = self
            .file_root
            .map(std::fs::canonicalize)
            .transpose()
            .map_err(BrowserBuildError::Io)?;
        let egress_policy = self.egress_policy.or_else(|| {
            self.brave_session.as_ref().and_then(|session| {
                if session.copies_all_cookies() {
                    return None;
                }
                Some(
                    session
                        .allowed_origins()
                        .iter()
                        .cloned()
                        .fold(BrowserEgressPolicy::deny_by_default(), |policy, origin| {
                            policy.allow_origin(origin)
                        }),
                )
            })
        });
        Ok(Browser {
            inner: native::NativeBrowser::new(
                self.executable,
                self.cdp_endpoint,
                self.brave_session,
                self.launch_brave_executable,
                self.virtual_authenticator,
                self.react_diagnostics,
                egress_policy,
                file_root,
                self.context,
                self.storage_state,
                self.after_action,
                self.ffmpeg_executable,
                self.lighthouse_executable,
                self.crux_client,
            )?,
        })
    }
}

fn validate_browser_context(context: &BrowserContext) -> Result<(), BrowserBuildError> {
    if let Some(viewport) = context.viewport
        && (viewport.width == 0
            || viewport.height == 0
            || viewport.width > MAX_VIEWPORT_DIMENSION
            || viewport.height > MAX_VIEWPORT_DIMENSION
            || !viewport.device_scale_factor.is_finite()
            || viewport.device_scale_factor <= 0.0
            || viewport.max_touch_points > 16
            || (viewport.touch && viewport.max_touch_points == 0)
            || (!viewport.touch && viewport.max_touch_points != 0))
    {
        return Err(BrowserBuildError::Configuration {
            message: "browser context viewport dimensions, scale, or touch count are invalid"
                .to_owned(),
        });
    }
    if let Some(geolocation) = context.geolocation
        && (!geolocation.latitude.is_finite()
            || !geolocation.longitude.is_finite()
            || !geolocation.accuracy_meters.is_finite()
            || !(-90.0..=90.0).contains(&geolocation.latitude)
            || !(-180.0..=180.0).contains(&geolocation.longitude)
            || geolocation.accuracy_meters < 0.0)
    {
        return Err(BrowserBuildError::Configuration {
            message: "browser context geolocation is invalid".to_owned(),
        });
    }
    if context
        .cpu_throttle_rate
        .is_some_and(|rate| !rate.is_finite() || rate < 1.0)
    {
        return Err(BrowserBuildError::Configuration {
            message: "browser context CPU throttle rate must be finite and at least one".to_owned(),
        });
    }
    if let Some(network) = context.network
        && (!network.latency_ms.is_finite()
            || network.latency_ms < 0.0
            || !network.download_bytes_per_second.is_finite()
            || network.download_bytes_per_second < -1.0
            || !network.upload_bytes_per_second.is_finite()
            || network.upload_bytes_per_second < -1.0)
    {
        return Err(BrowserBuildError::Configuration {
            message: "browser context network conditions are invalid".to_owned(),
        });
    }
    if context.extra_headers.iter().any(|(name, value)| {
        name.is_empty()
            || name.contains('\r')
            || name.contains('\n')
            || value.contains('\r')
            || value.contains('\n')
    }) {
        return Err(BrowserBuildError::Configuration {
            message: "browser context HTTP headers contain an invalid name or newline".to_owned(),
        });
    }
    Ok(())
}

fn validate_storage_state(state: &BrowserStorageState) -> Result<(), String> {
    let mut origins = std::collections::BTreeSet::new();
    for origin in &state.origins {
        let url = url::Url::parse(&origin.origin)
            .map_err(|error| format!("storage-state origin is invalid: {error}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.origin().ascii_serialization() != origin.origin
            || !origins.insert(origin.origin.as_str())
        {
            return Err(format!(
                "storage-state origin must be a unique exact HTTP(S) origin: {}",
                origin.origin
            ));
        }
    }
    if state.cookies.iter().any(|cookie| {
        cookie.name.is_empty()
            || cookie.domain.is_empty()
            || !cookie.path.starts_with('/')
            || cookie
                .expires_epoch_seconds
                .is_some_and(|expires| !expires.is_finite() || expires < 0.0)
    }) {
        return Err("storage-state contains an invalid cookie".to_owned());
    }
    Ok(())
}

impl Browser {
    /// Starts configuring an isolated browser session.
    #[must_use]
    pub fn builder() -> BrowserBuilder {
        BrowserBuilder::default()
    }

    /// Creates a lazy, isolated headless Chromium session.
    ///
    /// Chromium starts on the first action. All clones address the same page,
    /// reference map, diagnostics, and monotonic action sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the private runtime directory cannot be created.
    pub fn new() -> Result<Self, BrowserBuildError> {
        Self::builder().build()
    }

    /// Creates a browser using an explicit Chrome or Chromium executable.
    ///
    /// # Errors
    ///
    /// Returns an error when the private runtime directory cannot be created.
    pub fn with_executable(
        executable: impl Into<std::path::PathBuf>,
    ) -> Result<Self, BrowserBuildError> {
        Self::builder().executable(executable).build()
    }

    /// Starts Chromium without navigating away from its private blank page.
    ///
    /// The browser also starts lazily on the first action, so callers only need
    /// this when they want to overlap cold startup with other work. Repeated
    /// calls are idempotent.
    ///
    /// # Errors
    ///
    /// Returns a typed browser launch or connection error.
    pub async fn start(&self) -> Result<(), BrowserError> {
        self.inner.start().await
    }

    /// Executes one typed action against the owned session.
    ///
    /// # Errors
    ///
    /// Returns a typed browser, navigation, selector, or filesystem error.
    pub async fn execute(
        &self,
        action: BrowserAction,
    ) -> Result<BrowserActionResult, BrowserError> {
        self.inner.execute(action).await
    }

    /// Replays every recorded browser action in order against this session.
    ///
    /// Trace-control actions are skipped so replay does not recursively record
    /// another trace. Both originally successful and originally failed actions
    /// are attempted because the file is an exact action stream rather than a
    /// success-only script.
    ///
    /// # Errors
    ///
    /// Returns the first trace-read or browser-action error.
    pub async fn replay(
        &self,
        trace: &BrowserSessionTrace,
    ) -> Result<Vec<BrowserActionResult>, BrowserError> {
        let mut results = Vec::new();
        for entry in trace.entries().await? {
            if matches!(
                &entry.action,
                BrowserAction::SessionTraceStart { .. } | BrowserAction::SessionTraceStop
            ) {
                continue;
            }
            results.push(self.execute(entry.action).await?);
        }
        Ok(results)
    }

    /// Captures credential-bearing cookies and storage for currently open
    /// origins at the harness boundary.
    ///
    /// Enabled browser tracing records the complete returned state. Protect
    /// that backend with the same access and retention policy as a cookie jar.
    ///
    /// # Errors
    ///
    /// Returns a typed browser or serialization error.
    pub async fn storage_state(&self) -> Result<BrowserStorageState, BrowserError> {
        self.inner.storage_state().await
    }

    /// Replaces cookies and installs per-origin storage before future
    /// navigations. This method is deliberately absent from the browser tool.
    /// Enabled browser tracing records the complete input state.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed state or a failed browser operation.
    pub async fn restore_storage_state(
        &self,
        state: BrowserStorageState,
    ) -> Result<(), BrowserError> {
        validate_storage_state(&state)
            .map_err(|message| BrowserError::Configuration { message })?;
        self.inner.restore_storage_state(state).await
    }

    /// Prepares an explicit user authentication handoff for an allowlisted URL.
    ///
    /// This is available only for browsers configured with
    /// [`BrowserBuilder::brave_session`]. It never becomes part of the browser
    /// tool schema, so a model cannot open arbitrary pages in the user's
    /// browser. The caller decides when to open the page and when authentication
    /// has completed.
    ///
    /// # Errors
    ///
    /// Returns an error when no Brave session is configured or the URL's exact
    /// origin is outside that session's allowlist.
    pub fn auth_handoff(&self, url: url::Url) -> Result<BraveAuthHandoff, BrowserError> {
        self.inner.validate_auth_handoff(&url)?;
        Ok(BraveAuthHandoff {
            browser: self.clone(),
            url,
        })
    }

    /// Returns public metadata for credentials in the virtual authenticator.
    ///
    /// The authenticator is installed after the first successful navigation.
    /// Private keys and user handles never cross this API boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when virtual passkeys were not configured, the browser
    /// has not navigated yet, or Chromium rejects the query.
    pub async fn virtual_credentials(&self) -> Result<Vec<VirtualCredential>, BrowserError> {
        self.inner.virtual_credentials().await
    }

    /// Gracefully closes Chromium and permanently closes this session.
    ///
    /// # Errors
    ///
    /// Returns an error if Chromium rejects or interrupts graceful shutdown.
    pub async fn close(&self) -> Result<(), BrowserError> {
        self.inner.close().await
    }
}

/// A normal Nanocodex tool controlling one isolated, managed browser session.
pub struct BrowserTool {
    backend: BrowserBackend,
}

fn browser_tool_builder() -> BrowserBuilder {
    Browser::builder().virtual_authenticator(VirtualAuthenticator::platform_passkey())
}

impl BrowserTool {
    /// Creates an isolated in-process headless Chromium session.
    ///
    /// Chrome is launched lazily on the first action and closed when the tool
    /// is dropped. Each tool receives an independent browser session.
    ///
    /// # Errors
    ///
    /// Returns an error when the private runtime configuration cannot be
    /// created. A missing Chrome or Chromium installation is reported by the
    /// first tool call.
    pub fn new() -> Result<Self, BrowserBuildError> {
        Ok(Self::from_browser(browser_tool_builder().build()?))
    }

    /// Creates a managed browser tool using an explicit Chromium executable.
    ///
    /// # Errors
    ///
    /// Returns an error when the private runtime configuration cannot be
    /// created.
    pub fn with_executable(
        executable: impl Into<std::path::PathBuf>,
    ) -> Result<Self, BrowserBuildError> {
        Ok(Self::from_browser(
            browser_tool_builder().executable(executable).build()?,
        ))
    }

    /// Wraps an existing browser handle as a Nanocodex tool.
    #[must_use]
    pub const fn from_browser(browser: Browser) -> Self {
        Self {
            backend: BrowserBackend::Managed(browser),
        }
    }

    /// Creates a no-op browser tool and a handle for inspecting its calls.
    #[must_use]
    pub fn recording() -> (Self, BrowserRecording) {
        let recording = BrowserRecording::default();
        (
            Self {
                backend: BrowserBackend::Recording(recording.clone()),
            },
            recording,
        )
    }
}

fn browser_tool_definition() -> ToolDefinition {
    static DEFINITION: OnceLock<ToolDefinition> = OnceLock::new();
    DEFINITION
        .get_or_init(|| {
            ToolDefinition::function("browser", TOOL_DESCRIPTION, schema_for::<BrowserAction>())
                .with_output_schema(schema_for::<BrowserActionResult>())
        })
        .clone()
}

#[async_trait]
impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        browser_tool_definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let action = input.decode_json::<BrowserAction>()?;
        let result = match &self.backend {
            BrowserBackend::Managed(browser) => browser.execute(action).await?,
            BrowserBackend::Recording(recording) => recording.record(action)?,
        };
        let mut content = Vec::new();
        let mut model_images = BTreeMap::new();
        let mut total_image_bytes = 0_u64;
        for path in result.image_paths() {
            let bytes = tokio::fs::metadata(path).await?.len();
            total_image_bytes = total_image_bytes.saturating_add(bytes);
            if total_image_bytes > native::MAX_IMAGE_ARTIFACT_BYTES {
                return Err(Box::new(BrowserError::ImageArtifactTooLarge {
                    bytes: total_image_bytes,
                    maximum: native::MAX_IMAGE_ARTIFACT_BYTES,
                }));
            }
            let bytes = tokio::fs::read(path).await?;
            let image_url = format!("data:image/png;base64,{}", STANDARD.encode(bytes));
            content.push(ToolOutputContent::InputImage {
                image_url: image_url.clone(),
                detail: ImageDetail::High,
            });
            model_images.insert(path.to_owned(), BrowserModelImage { image_url });
        }
        if content.is_empty() {
            Ok(ToolOutput::json(&result))
        } else {
            let mut code_mode_result = result.clone();
            code_mode_result.attach_model_images(&model_images);
            let structured_result = match serde_json::to_value(code_mode_result) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(ToolOutput::error(format!(
                        "failed to encode tool result: {error}"
                    )));
                }
            };
            let encoded = match serde_json::to_string(&result) {
                Ok(encoded) => encoded,
                Err(error) => {
                    return Ok(ToolOutput::error(format!(
                        "failed to encode tool result: {error}"
                    )));
                }
            };
            content.insert(0, ToolOutputContent::InputText { text: encoded });
            Ok(ToolOutput::content(content).with_structured_result(structured_result))
        }
    }
}

#[async_trait]
impl DynamicToolProvider for BrowserTool {
    fn start(&self) {}

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        vec![browser_tool_definition()]
    }

    fn code_mode_tool_summaries(&self) -> Vec<(String, String)> {
        vec![(
            "browser".to_owned(),
            "Control the host-managed browser session one typed action at a time.".to_owned(),
        )]
    }

    fn contains(&self, name: &str) -> bool {
        name == "browser"
    }

    async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        if name != "browser" {
            return None;
        }
        let input = match serde_json::value::to_raw_value(&input) {
            Ok(input) => ToolInput::Function(input),
            Err(error) => {
                return Some(ToolOutput::error(format!(
                    "failed to encode browser input: {error}"
                )));
            }
        };
        Some(match Tool::execute(self, input, context).await {
            Ok(output) => output,
            Err(error) => ToolOutput::error(error.to_string()),
        })
    }
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests;
