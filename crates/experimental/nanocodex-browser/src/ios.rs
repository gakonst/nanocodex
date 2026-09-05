use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Client, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{process::Command, sync::Mutex};
use url::Url;

use crate::{
    BrowserAction, BrowserActionName, BrowserActionResult, BrowserImageArtifact,
    BrowserMobileAudit, BrowserMobileAuditSample, BrowserMobileFinding,
    BrowserMobileFindingSeverity, BrowserMobileState,
};

/// Exact iOS device selection for an Appium/XCUITest Safari session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserIosDeviceSelector {
    Udid(String),
    ExactName(String),
}

/// Harness-owned connection policy for real Mobile Safari automation.
#[derive(Clone, Debug)]
pub struct BrowserIosConfig {
    endpoint: Url,
    device: BrowserIosDeviceSelector,
    platform_version: Option<String>,
}

impl BrowserIosConfig {
    /// Configures an already-running Appium server and an exact device selector.
    ///
    /// Appium is intentionally external: this library never enables relaxed
    /// security, owns an ambiguous process, or kills an operator's server.
    pub fn new(endpoint: Url, device: BrowserIosDeviceSelector) -> Result<Self, BrowserIosError> {
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(BrowserIosError::Configuration(
                "Appium endpoint must use HTTP or HTTPS".to_owned(),
            ));
        }
        let identity = match &device {
            BrowserIosDeviceSelector::Udid(value) | BrowserIosDeviceSelector::ExactName(value) => {
                value
            }
        };
        if identity.trim().is_empty() {
            return Err(BrowserIosError::Configuration(
                "iOS device identity cannot be empty".to_owned(),
            ));
        }
        let mut endpoint = endpoint;
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        Ok(Self {
            endpoint,
            device,
            platform_version: None,
        })
    }

    /// Pins the requested iOS runtime version.
    #[must_use]
    pub fn platform_version(mut self, version: impl Into<String>) -> Self {
        self.platform_version = Some(version.into());
        self
    }
}

/// Kind of iOS target returned by Xcode discovery.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserIosDeviceKind {
    Simulator,
    Real,
}

/// One Xcode-visible iOS device with stable identity.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserIosDevice {
    pub udid: String,
    pub name: String,
    pub kind: BrowserIosDeviceKind,
    pub runtime: Option<String>,
    pub state: Option<String>,
    pub available: bool,
}

/// Device inventory with discovery failures kept distinct from an empty list.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserIosDeviceInventory {
    pub devices: Vec<BrowserIosDevice>,
    pub simulator_error: Option<String>,
    pub real_device_error: Option<String>,
}

impl BrowserIosDeviceInventory {
    /// Probes Xcode simulators and USB devices without starting either.
    pub async fn discover() -> Self {
        let (simulators, real_devices) =
            tokio::join!(discover_simulators(), discover_real_devices());
        let simulator_error = simulators.as_ref().err().map(ToString::to_string);
        let real_device_error = real_devices.as_ref().err().map(ToString::to_string);
        let mut devices = simulators.unwrap_or_default();
        devices.extend(real_devices.unwrap_or_default());
        devices.sort_by(|left, right| {
            left.kind
                .sort_key()
                .cmp(&right.kind.sort_key())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.udid.cmp(&right.udid))
        });
        Self {
            devices,
            simulator_error,
            real_device_error,
        }
    }
}

impl BrowserIosDeviceKind {
    const fn sort_key(self) -> u8 {
        match self {
            Self::Real => 0,
            Self::Simulator => 1,
        }
    }
}

#[derive(Clone)]
pub struct IosBrowser {
    inner: Arc<IosBrowserInner>,
}

struct IosBrowserInner {
    config: BrowserIosConfig,
    client: Client,
    output_dir: TempDir,
    state: Mutex<IosState>,
}

#[derive(Default)]
struct IosState {
    session_id: Option<String>,
    next_sequence: u64,
    closed: bool,
}

impl IosBrowser {
    /// Creates a lazy Appium/XCUITest Mobile Safari handle.
    pub fn new(config: BrowserIosConfig) -> Result<Self, BrowserIosError> {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        Ok(Self {
            inner: Arc::new(IosBrowserInner {
                config,
                client: Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?,
                output_dir: tempfile::tempdir()?,
                state: Mutex::new(IosState::default()),
            }),
        })
    }

    /// Executes the supported real-iOS browser actions.
    pub async fn execute(
        &self,
        action: BrowserAction,
    ) -> Result<BrowserActionResult, BrowserIosError> {
        let mut state = self.inner.state.lock().await;
        if state.closed {
            return Err(BrowserIosError::Closed);
        }
        let session_id = self.ensure_session(&mut state).await?;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        self.execute_in_session(&session_id, sequence, action).await
    }

    /// Deletes the Appium session. The external Appium server and pre-existing
    /// simulator/device remain operator-owned.
    pub async fn close(&self) -> Result<(), BrowserIosError> {
        let mut state = self.inner.state.lock().await;
        if state.closed {
            return Ok(());
        }
        if let Some(session_id) = state.session_id.take() {
            self.request_value::<Value>(Method::DELETE, &format!("session/{session_id}"), None)
                .await?;
        }
        state.closed = true;
        Ok(())
    }

    async fn ensure_session(&self, state: &mut IosState) -> Result<String, BrowserIosError> {
        if let Some(session_id) = &state.session_id {
            return Ok(session_id.clone());
        }
        let status: Value = self.request_value(Method::GET, "status", None).await?;
        if status.get("ready").and_then(Value::as_bool) == Some(false) {
            return Err(BrowserIosError::AppiumNotReady(status.to_string()));
        }
        let mut capabilities = serde_json::Map::from_iter([
            ("platformName".to_owned(), json!("iOS")),
            ("browserName".to_owned(), json!("Safari")),
            ("appium:automationName".to_owned(), json!("XCUITest")),
        ]);
        match &self.inner.config.device {
            BrowserIosDeviceSelector::Udid(udid) => {
                capabilities.insert("appium:udid".to_owned(), json!(udid));
            }
            BrowserIosDeviceSelector::ExactName(name) => {
                capabilities.insert("appium:deviceName".to_owned(), json!(name));
            }
        }
        if let Some(version) = &self.inner.config.platform_version {
            capabilities.insert("appium:platformVersion".to_owned(), json!(version));
        }
        let response: Value = self
            .request_raw(
                Method::POST,
                "session",
                Some(json!({ "capabilities": { "alwaysMatch": capabilities } })),
            )
            .await?;
        let session_id = response
            .get("sessionId")
            .or_else(|| {
                response
                    .get("value")
                    .and_then(|value| value.get("sessionId"))
            })
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserIosError::Protocol("session response omitted sessionId".to_owned())
            })?
            .to_owned();
        state.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    async fn execute_in_session(
        &self,
        session_id: &str,
        sequence: u64,
        action: BrowserAction,
    ) -> Result<BrowserActionResult, BrowserIosError> {
        let path = |suffix: &str| format!("session/{session_id}/{suffix}");
        match action {
            BrowserAction::Open { url } => {
                Url::parse(&url).map_err(|error| BrowserIosError::Protocol(error.to_string()))?;
                self.request_value::<Value>(Method::POST, &path("url"), Some(json!({ "url": url })))
                    .await?;
                Ok(action_result(sequence, BrowserActionName::Open))
            }
            BrowserAction::Reload => {
                self.request_value::<Value>(Method::POST, &path("refresh"), Some(json!({})))
                    .await?;
                Ok(action_result(sequence, BrowserActionName::Reload))
            }
            BrowserAction::TouchTap { x, y } => {
                let actions = pointer_actions(vec![
                    json!({"type":"pointerMove","duration":0,"x":x,"y":y,"origin":"viewport"}),
                    json!({"type":"pointerDown","button":0}),
                    json!({"type":"pause","duration":80}),
                    json!({"type":"pointerUp","button":0}),
                ]);
                self.request_value::<Value>(Method::POST, &path("actions"), Some(actions)).await?;
                Ok(action_result(sequence, BrowserActionName::TouchTap))
            }
            BrowserAction::TouchSwipe { from_x, from_y, to_x, to_y, duration_ms, .. } => {
                let duration = duration_ms.unwrap_or(250);
                if duration == 0 || duration > 5_000 {
                    return Err(BrowserIosError::Protocol("swipe duration must be 1..=5000ms".to_owned()));
                }
                let actions = pointer_actions(vec![
                    json!({"type":"pointerMove","duration":0,"x":from_x,"y":from_y,"origin":"viewport"}),
                    json!({"type":"pointerDown","button":0}),
                    json!({"type":"pointerMove","duration":duration,"x":to_x,"y":to_y,"origin":"viewport"}),
                    json!({"type":"pointerUp","button":0}),
                ]);
                self.request_value::<Value>(Method::POST, &path("actions"), Some(actions)).await?;
                Ok(action_result(sequence, BrowserActionName::TouchSwipe))
            }
            BrowserAction::InsertText { text } => {
                let active: Value = self.request_value(Method::GET, &path("element/active"), None).await?;
                let element = active
                    .get("element-6066-11e4-a52e-4f735466cecf")
                    .or_else(|| active.get("ELEMENT"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| BrowserIosError::Protocol("active element response omitted its ID".to_owned()))?;
                let value = text.chars().map(|character| character.to_string()).collect::<Vec<_>>();
                self.request_value::<Value>(
                    Method::POST,
                    &path(&format!("element/{element}/value")),
                    Some(json!({ "text": text, "value": value })),
                ).await?;
                Ok(action_result(sequence, BrowserActionName::InsertText))
            }
            BrowserAction::GetUrl => Ok(BrowserActionResult::Url {
                sequence,
                executed: true,
                url: self.request_value(Method::GET, &path("url"), None).await?,
            }),
            BrowserAction::GetTitle => Ok(BrowserActionResult::Title {
                sequence,
                executed: true,
                title: self.request_value(Method::GET, &path("title"), None).await?,
            }),
            BrowserAction::Evaluate { expression } => Ok(BrowserActionResult::Evaluation {
                sequence,
                executed: true,
                value: self.execute_script(session_id, &format!("return ({expression});")).await?,
            }),
            BrowserAction::MobileState => Ok(BrowserActionResult::MobileState {
                sequence,
                executed: true,
                state: self.execute_script(session_id, IOS_MOBILE_STATE_SCRIPT).await
                    .and_then(|value| serde_json::from_value(value).map_err(BrowserIosError::Json))?,
            }),
            BrowserAction::MobileAudit {
                devices,
                orientations,
                ready,
            } => {
                if !devices.is_empty() || !orientations.is_empty() || ready.is_some() {
                    return Err(BrowserIosError::Unsupported {
                        action: BrowserActionName::MobileAudit,
                        reason: "a physical Appium session cannot switch Chromium presets or use a Chromium readiness target".to_owned(),
                    });
                }
                let state: BrowserMobileState = serde_json::from_value(
                    self.execute_script(session_id, IOS_MOBILE_STATE_SCRIPT).await?,
                )?;
                let wire: IosAuditWire = serde_json::from_value(
                    self.execute_script(session_id, IOS_MOBILE_AUDIT_SCRIPT).await?,
                )?;
                let error_count = wire.findings.iter().filter(|finding| finding.severity == BrowserMobileFindingSeverity::Error).count();
                let warning_count = wire.findings.iter().filter(|finding| finding.severity == BrowserMobileFindingSeverity::Warning).count();
                Ok(BrowserActionResult::MobileAudit {
                    sequence,
                    executed: true,
                    audit: BrowserMobileAudit {
                        url: state.url.clone(),
                        provider: "ios_webdriver".to_owned(),
                        samples: vec![BrowserMobileAuditSample {
                            device: None,
                            device_name: match &self.inner.config.device {
                                BrowserIosDeviceSelector::Udid(udid) => udid.clone(),
                                BrowserIosDeviceSelector::ExactName(name) => name.clone(),
                            },
                            state,
                            document_width: wire.document_width,
                            horizontal_overflow: wire.horizontal_overflow,
                            interactive_elements: wire.interactive_elements,
                            findings: wire.findings,
                        }],
                        error_count,
                        warning_count,
                        passed: error_count == 0,
                    },
                })
            }
            BrowserAction::Screenshot { full_page, annotate, target } if !full_page && !annotate && target.is_none() => {
                let encoded: String = self.request_value(Method::GET, &path("screenshot"), None).await?;
                let bytes = STANDARD.decode(encoded)?;
                let image = image::load_from_memory(&bytes)?;
                let output = self.inner.output_dir.path().join(format!("ios-{sequence}.png"));
                tokio::fs::write(&output, &bytes).await?;
                Ok(BrowserActionResult::Screenshot {
                    sequence,
                    executed: true,
                    path: output.clone(),
                    image: Some(BrowserImageArtifact {
                        artifact_id: format!("ios-{sequence}"),
                        path: output,
                        mime_type: "image/png".to_owned(),
                        width: image.width(),
                        height: image.height(),
                        model_image: None,
                    }),
                })
            }
            action => Err(BrowserIosError::Unsupported {
                action: action.name(),
                reason: "this action requires Chromium/CDP or has not been implemented for Appium/XCUITest".to_owned(),
            }),
        }
    }

    async fn execute_script(
        &self,
        session_id: &str,
        script: &str,
    ) -> Result<Value, BrowserIosError> {
        self.request_value(
            Method::POST,
            &format!("session/{session_id}/execute/sync"),
            Some(json!({ "script": script, "args": [] })),
        )
        .await
    }

    async fn request_value<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, BrowserIosError> {
        let raw = self.request_raw(method, path, body).await?;
        let value = raw.get("value").cloned().unwrap_or(raw);
        Ok(serde_json::from_value(value)?)
    }

    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BrowserIosError> {
        let url = self.inner.config.endpoint.join(path)?;
        let mut request = self.inner.client.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let status = request.send().await?;
        let code = status.status();
        let value: Value = status.json().await?;
        if !code.is_success()
            || value
                .get("value")
                .and_then(|value| value.get("error"))
                .is_some()
        {
            return Err(BrowserIosError::WebDriver(value.to_string()));
        }
        Ok(value)
    }
}

fn pointer_actions(actions: Vec<Value>) -> Value {
    json!({"actions":[{"type":"pointer","id":"finger","parameters":{"pointerType":"touch"},"actions":actions}]})
}

const fn action_result(sequence: u64, action: BrowserActionName) -> BrowserActionResult {
    BrowserActionResult::Action {
        sequence,
        action,
        executed: true,
        outcome: None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IosAuditWire {
    document_width: f64,
    horizontal_overflow: f64,
    interactive_elements: usize,
    findings: Vec<BrowserMobileFinding>,
}

async fn discover_simulators() -> Result<Vec<BrowserIosDevice>, BrowserIosError> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available", "--json"])
        .output()
        .await?;
    if !output.status.success() {
        return Err(BrowserIosError::Discovery(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let mut devices = Vec::new();
    if let Some(runtimes) = value.get("devices").and_then(Value::as_object) {
        for (runtime, rows) in runtimes {
            for row in rows.as_array().into_iter().flatten() {
                let Some(udid) = row.get("udid").and_then(Value::as_str) else {
                    continue;
                };
                let Some(name) = row.get("name").and_then(Value::as_str) else {
                    continue;
                };
                devices.push(BrowserIosDevice {
                    udid: udid.to_owned(),
                    name: name.to_owned(),
                    kind: BrowserIosDeviceKind::Simulator,
                    runtime: Some(
                        runtime
                            .trim_start_matches("com.apple.CoreSimulator.SimRuntime.")
                            .replace('-', "."),
                    ),
                    state: row.get("state").and_then(Value::as_str).map(str::to_owned),
                    available: row
                        .get("isAvailable")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                });
            }
        }
    }
    Ok(devices)
}

async fn discover_real_devices() -> Result<Vec<BrowserIosDevice>, BrowserIosError> {
    let output = Command::new("xcrun")
        .args(["xctrace", "list", "devices"])
        .output()
        .await?;
    if !output.status.success() {
        return Err(BrowserIosError::Discovery(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut in_devices = false;
    let mut devices = Vec::new();
    for line in text.lines() {
        if line.starts_with("== Devices ==") {
            in_devices = true;
            continue;
        }
        if line.starts_with("== Simulators ==") {
            break;
        }
        if !in_devices
            || !(line.contains("iPhone") || line.contains("iPad") || line.contains("iPod"))
        {
            continue;
        }
        let Some(udid_start) = line.rfind('(') else {
            continue;
        };
        let Some(udid_end) = line.rfind(')') else {
            continue;
        };
        let udid = line[udid_start + 1..udid_end].trim();
        let prefix = line[..udid_start].trim();
        let (name, runtime) = prefix
            .rsplit_once(" (")
            .map_or((prefix, None), |(name, version)| {
                (name, Some(version.trim_end_matches(')').to_owned()))
            });
        devices.push(BrowserIosDevice {
            udid: udid.to_owned(),
            name: name.to_owned(),
            kind: BrowserIosDeviceKind::Real,
            runtime,
            state: None,
            available: true,
        });
    }
    Ok(devices)
}

const IOS_MOBILE_STATE_SCRIPT: &str = r#"return (() => {
  const ua = navigator.userAgent;
  const mismatches = [];
  if (!/iPhone|iPad|iPod/.test(ua)) mismatches.push('WebDriver session is not exposing an iOS user agent');
  if ((navigator.maxTouchPoints || 0) === 0) mismatches.push('touch capability is not page-visible');
  return {
    provider:'ios_webdriver', engine:'webkit', url:location.href,
    viewportWidth:innerWidth, viewportHeight:innerHeight,
    visualViewportWidth:visualViewport?.width ?? null,
    visualViewportHeight:visualViewport?.height ?? null,
    visualViewportScale:visualViewport?.scale ?? null,
    screenWidth:screen.width, screenHeight:screen.height,
    devicePixelRatio:devicePixelRatio, userAgent:ua, platform:navigator.platform,
    maxTouchPoints:navigator.maxTouchPoints || 0,
    coarsePointer:matchMedia('(pointer:coarse)').matches,
    noHover:matchMedia('(hover:none)').matches,
    orientation:screen.orientation?.type || (innerWidth > innerHeight ? 'landscape' : 'portrait'),
    metaViewport:document.querySelector('meta[name="viewport" i]')?.content ?? null,
    verified:mismatches.length === 0, mismatches
  };
})()"#;

const IOS_MOBILE_AUDIT_SCRIPT: &str = r#"return (() => {
  const findings=[]; const visible=e=>{const s=getComputedStyle(e),r=e.getBoundingClientRect();return s.display!=='none'&&s.visibility!=='hidden'&&r.width>0&&r.height>0};
  const sel=e=>e.id?'#'+CSS.escape(e.id):e.tagName.toLowerCase();
  if(!document.querySelector('meta[name="viewport" i]')) findings.push({rule:'meta-viewport',severity:'error',message:'Missing <meta name="viewport">.',selector:'head',measuredWidth:null,measuredHeight:null});
  const viewportWidth=visualViewport?.width??innerWidth,documentWidth=Math.max(document.documentElement?.scrollWidth||0,document.body?.scrollWidth||0),horizontalOverflow=Math.max(0,documentWidth-viewportWidth);
  if(horizontalOverflow>1)findings.push({rule:'horizontal-overflow',severity:'error',message:`Document is ${Math.round(horizontalOverflow)} CSS px wider than the viewport.`,selector:'html',measuredWidth:documentWidth,measuredHeight:null});
  const interactive=Array.from(document.querySelectorAll('a[href],button,input,select,textarea,[role="button"],[role="link"],[tabindex]')).filter(visible);
  for(const e of interactive.slice(0,60)){const r=e.getBoundingClientRect();if(r.width<44||r.height<44)findings.push({rule:'touch-target-size',severity:'warning',message:`Interactive target is ${Math.round(r.width)}x${Math.round(r.height)} CSS px; expected at least 44x44.`,selector:sel(e),measuredWidth:r.width,measuredHeight:r.height});if(e.matches('input:not([type="checkbox"]):not([type="radio"]):not([type="range"]),textarea,select')&&parseFloat(getComputedStyle(e).fontSize)<16)findings.push({rule:'input-font-size',severity:'warning',message:'Editable control text is below 16px; iOS may zoom on focus.',selector:sel(e),measuredWidth:r.width,measuredHeight:r.height})}
  return {documentWidth,horizontalOverflow,interactiveElements:interactive.length,findings:findings.slice(0,80)};
})()"#;

#[derive(Debug, thiserror::Error)]
pub enum BrowserIosError {
    #[error("invalid iOS browser configuration: {0}")]
    Configuration(String),
    #[error("Appium is not ready: {0}")]
    AppiumNotReady(String),
    #[error("WebDriver rejected the operation: {0}")]
    WebDriver(String),
    #[error("invalid WebDriver response: {0}")]
    Protocol(String),
    #[error("iOS device discovery failed: {0}")]
    Discovery(String),
    #[error("browser action {action:?} is unsupported by iOS WebDriver: {reason}")]
    Unsupported {
        action: BrowserActionName,
        reason: String,
    },
    #[error("the iOS browser session is closed")]
    Closed,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}
