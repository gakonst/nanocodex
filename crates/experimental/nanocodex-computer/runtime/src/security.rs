use crate::{Error, Result, process_rpc::Program};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allowed_apps: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub denied_origins: Vec<String>,
    #[serde(default)]
    pub broker: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub review_instructions: String,
    /// Explicit host authorization; app allowlists alone do not grant approval.
    #[serde(default)]
    pub preapproved_apps: Vec<String>,
    #[serde(default)]
    pub preapproved_audio: bool,
    /// Explicit host download grants, separate from the network allowlist.
    #[serde(default)]
    pub preapproved_download_origins: Vec<String>,
    /// Host-owned opt-in; network and download allowlists do not grant access.
    #[serde(default)]
    pub require_origin_approval: bool,
    #[serde(default)]
    pub preapproved_origin_access: Vec<String>,
}
#[derive(Default)]
struct DownloadApprovals {
    epoch: Arc<()>,
    origins: BTreeMap<(String, String), bool>,
}
/// A trusted embedding host's correlated approval channel. This capability is
/// never accepted from model arguments and must be bound to its originating cell.
pub trait DownloadApproval: Send + Sync {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
    fn suspended_duration(&self) -> Result<Duration> {
        Ok(Duration::ZERO)
    }
    /// Background maintenance may retain a paused response, but a host can
    /// require an active provider call before starting a human prompt.
    fn ready_to_prompt(&self) -> Result<bool> {
        self.validate()?;
        Ok(true)
    }
    fn request(&self, request: Value, deadline: Option<Instant>) -> Result<Value>;
}
#[derive(Clone, Default)]
pub struct Security {
    config: SecurityConfig,
    native_control_authorized: bool,
    // Trusted immutable configuration identity; clones share the same policy.
    raw_wait_policy_epoch: Arc<()>,
    allowed: BTreeSet<String>,
    denied: BTreeSet<String>,
    preapproved_downloads: BTreeSet<String>,
    preapproved_access: BTreeSet<String>,
    origin_grants: BTreeMap<String, BTreeSet<String>>,
    download_approvals: Arc<Mutex<DownloadApprovals>>,
    download_approval: Option<Arc<dyn DownloadApproval>>,
    download_scope: String,
    pub broker: Option<Program>,
    pub reviewer: Option<Program>,
}
/// A native admission snapshot. No serde implementation or public constructor.
/// The initial wait domain has no document-specific read permission to retain.
#[derive(Clone)]
pub(crate) struct RawWaitPolicy {
    epoch: Arc<()>,
}
impl Security {
    /// Host-only capability: the embedding tool attachment already authorizes
    /// native control. App/origin restrictions and OS permissions still apply.
    pub fn authorize_native_control(&mut self) {
        self.native_control_authorized = true;
    }
    pub(crate) fn raw_wait_policy(&self) -> Option<RawWaitPolicy> {
        (!self.browser_restricted()).then(|| RawWaitPolicy {
            epoch: self.raw_wait_policy_epoch.clone(),
        })
    }
    pub(crate) fn validates_raw_wait(&self, policy: &RawWaitPolicy) -> bool {
        !self.browser_restricted() && Arc::ptr_eq(&self.raw_wait_policy_epoch, &policy.epoch)
    }
    pub fn app_preapproved(&self, app: &crate::native::App) -> bool {
        self.native_control_authorized
            || self
                .config
                .preapproved_apps
                .iter()
                .any(|value| value == &app.id || value == &app.path || value == &app.name)
    }
    pub fn audio_preapproved(&self) -> bool {
        self.config.preapproved_audio
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::invalid("Security config exceeds 1 MiB"));
        }
        Self::new(serde_json::from_slice(&bytes)?)
    }
    pub fn new(config: SecurityConfig) -> Result<Self> {
        let allowed = config
            .allowed_origins
            .iter()
            .map(|s| origin(s))
            .collect::<Result<_>>()?;
        let denied = config
            .denied_origins
            .iter()
            .map(|s| origin(s))
            .collect::<Result<_>>()?;
        let preapproved_downloads = config
            .preapproved_download_origins
            .iter()
            .map(|s| origin(s))
            .collect::<Result<_>>()?;
        let preapproved_access = config
            .preapproved_origin_access
            .iter()
            .map(|s| origin(s))
            .collect::<Result<_>>()?;
        let broker = config
            .broker
            .as_ref()
            .map(|p| Program::new(p, Duration::from_secs(30)))
            .transpose()?;
        let reviewer = config
            .reviewer
            .as_ref()
            .map(|p| Program::new(p, Duration::from_secs(30)))
            .transpose()?;
        Ok(Self {
            config,
            native_control_authorized: false,
            raw_wait_policy_epoch: Arc::new(()),
            allowed,
            denied,
            preapproved_downloads,
            preapproved_access,
            origin_grants: Default::default(),
            download_approvals: Default::default(),
            download_approval: None,
            download_scope: String::new(),
            broker,
            reviewer,
        })
    }
    pub fn check_app(&self, identifier: &str) -> Result<()> {
        if self.config.allowed_apps.is_empty()
            || self.config.allowed_apps.iter().any(|a| a == identifier)
        {
            Ok(())
        } else {
            Err(Error::new(
                -32010,
                "Application denied by configured policy",
            ))
        }
    }
    pub fn set_download_approval(&mut self, approval: Option<Arc<dyn DownloadApproval>>) {
        self.download_approval = approval;
    }
    /// Trusted conversation/subagent selection. Existing policy clones retain
    /// the scope they observed, including a download waiting on a host response.
    pub fn set_download_scope(&mut self, scope: &str) {
        self.download_scope = scope.into();
    }
    pub fn origin_approval_required(&self) -> bool {
        self.config.require_origin_approval
    }
    pub(crate) fn origin_access(&self, scope: &str, url: &str) -> Result<Option<String>> {
        self.check_url(url)?;
        if !self.origin_approval_required() || url == "about:blank" {
            return Ok(None);
        }
        let origin = origin(url)?;
        if self.preapproved_access.contains(&origin)
            || self
                .origin_grants
                .get(scope)
                .is_some_and(|grants| grants.contains(&origin))
        {
            Ok(None)
        } else {
            Ok(Some(origin))
        }
    }
    pub(crate) fn grant_origin_access(&mut self, scope: &str, canonical: &str) -> Result<()> {
        let current = origin(canonical)?;
        if current != canonical {
            return Err(Error::invalid("Origin grant must be canonical"));
        }
        self.check_url(canonical)?;
        if self
            .origin_grants
            .values()
            .map(BTreeSet::len)
            .sum::<usize>()
            >= 1024
            && !self
                .origin_grants
                .get(scope)
                .is_some_and(|grants| grants.contains(&current))
        {
            return Err(Error::action("Origin grant limit exceeded"));
        }
        self.origin_grants
            .entry(scope.into())
            .or_default()
            .insert(current);
        Ok(())
    }
    pub(crate) fn clear_origin_scope(&mut self, scope: &str) {
        self.origin_grants.remove(scope);
    }
    pub(crate) fn clear_origin_grants(&mut self) {
        self.origin_grants.clear();
    }
    pub fn check_url(&self, url: &str) -> Result<()> {
        if url == "about:blank" && self.allowed.is_empty() {
            return Ok(());
        }
        let current = origin(url)?;
        if self.denied.contains(&current)
            || (!self.allowed.is_empty() && !self.allowed.contains(&current))
        {
            return Err(Error::new(-32010, "Origin denied by configured policy"));
        }
        Ok(())
    }
    /// Approve the actual paused response, not merely its initiating page.
    /// Callers supply host-observed URLs and revalidate the pending request
    /// after this call; a review result never authorizes a different response.
    pub fn check_download(&self, tab_url: &str, response_url: &str) -> Result<()> {
        self.check_download_with_deadline(tab_url, response_url, None)
    }
    pub fn check_download_until(
        &self,
        tab_url: &str,
        response_url: &str,
        deadline: Instant,
    ) -> Result<()> {
        self.check_download_with_deadline(tab_url, response_url, Some(deadline))
    }
    fn check_download_with_deadline(
        &self,
        tab_url: &str,
        response_url: &str,
        deadline: Option<Instant>,
    ) -> Result<()> {
        let suspended_before = self.download_suspended_duration()?;
        Self::check_download_deadline(deadline)?;
        if let Some(approval) = &self.download_approval {
            approval.validate()?;
        }
        let epoch = self
            .download_approvals
            .lock()
            .map_err(|_| Error::new(-32011, "Download approval state unavailable"))?
            .epoch
            .clone();
        self.check_url(response_url)?;
        self.check_url(tab_url)?;
        let response_origin = origin(response_url)?;
        let tab_origin = origin(tab_url)?;
        self.approve_download_origin(tab_url, &tab_origin, &epoch, deadline, suspended_before)?;
        if response_origin != tab_origin {
            self.approve_download_origin(
                response_url,
                &response_origin,
                &epoch,
                deadline,
                suspended_before,
            )?;
        }
        Self::check_download_deadline(self.download_deadline(deadline, suspended_before)?)?;
        if let Some(approval) = &self.download_approval {
            approval.validate()?;
        }
        Ok(())
    }
    /// Only trusted approval controls advance this cumulative cell-local clock.
    pub fn download_approval_ready(&self) -> Result<bool> {
        self.download_approval
            .as_ref()
            .map_or(Ok(true), |approval| approval.ready_to_prompt())
    }
    pub fn download_suspended_duration(&self) -> Result<Duration> {
        self.download_approval
            .as_ref()
            .map(|approval| approval.suspended_duration())
            .transpose()
            .map(|duration| duration.unwrap_or(Duration::ZERO))
    }
    fn download_deadline(
        &self,
        deadline: Option<Instant>,
        before: Duration,
    ) -> Result<Option<Instant>> {
        let elapsed = self
            .download_suspended_duration()?
            .checked_sub(before)
            .ok_or_else(|| Error::action("Download suspension clock moved backwards"))?;
        deadline
            .map(|deadline| {
                deadline
                    .checked_add(elapsed)
                    .ok_or_else(|| Error::action("Download suspension deadline overflow"))
            })
            .transpose()
    }
    fn check_download_deadline(deadline: Option<Instant>) -> Result<()> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Err(Error::new(-32008, "Download approval timed out"))
        } else {
            Ok(())
        }
    }
    fn approve_download_origin(
        &self,
        url: &str,
        origin: &str,
        epoch: &Arc<()>,
        deadline: Option<Instant>,
        suspended_before: Duration,
    ) -> Result<()> {
        Self::check_download_deadline(self.download_deadline(deadline, suspended_before)?)?;
        let key = (self.download_scope.clone(), origin.to_owned());
        {
            let approvals = self
                .download_approvals
                .lock()
                .map_err(|_| Error::new(-32011, "Download approval state unavailable"))?;
            if !Arc::ptr_eq(epoch, &approvals.epoch) {
                return Err(Error::new(
                    -32014,
                    "Download approval session changed during review",
                ));
            }
            if self.preapproved_downloads.contains(origin) {
                return Ok(());
            }
            if let Some(accepted) = approvals.origins.get(&key) {
                return if *accepted {
                    Ok(())
                } else {
                    Err(Error::new(-32012, "guardian_denied"))
                };
            }
        }
        let (accepted, persist) = if let Some(approval) = &self.download_approval
            && self.reviewer.is_none()
        {
            // Match the installed session-only transfer prompt. Durable grants
            // require a separate trusted preference store; never imply one here.
            let response = approval.request(
                json!({
                    "message":format!("Allow download from {url}?"),
                    "meta":{
                        "codex_approval_kind":"mcp_tool_call",
                        "connector_id":"browser-use", "connector_name":"Browser Use",
                        "persist":"session", "tool_name":"download_browser_files",
                        "tool_title":"Download browser files", "tool_params":{"origin":origin},
                        "file_transfer":"download", "origin":origin
                    }
                }),
                self.download_deadline(deadline, suspended_before)?,
            )?;
            let accepted = match response["action"].as_str() {
                Some("accept") => true,
                Some("decline") => false,
                Some("cancel") => return Err(Error::new(-32013, "approval_cancelled")),
                _ => return Err(Error::new(-32011, "Invalid download approval response")),
            };
            // Original eB checks _meta before content; absent persistence is a
            // one-off transfer. Without a durable preference store, an `always`
            // acceptance also remains one-off (original XX + VD behavior).
            let persistence = [&response["_meta"], &response["content"]]
                .iter()
                .filter_map(|value| value["persist"].as_str())
                .find(|value| ["session", "always"].contains(value));
            let automated_denial = !accepted
                && response["_meta"]["approvals_reviewer"]
                    .as_str()
                    .is_some_and(|reviewer| {
                        ["guardian_subagent", "auto_review"].contains(&reviewer)
                    });
            (
                accepted,
                persistence == Some("session") && !automated_denial,
            )
        } else {
            self.review_request_until(
                json!({
                    "operation":"file_download",
                    "url":url,
                    "origin":origin,
                    "tool_name":"download_browser_files",
                    "file_transfer":"download"
                }),
                "Download",
                self.download_deadline(deadline, suspended_before)?,
            )?;
            (true, true)
        };
        Self::check_download_deadline(self.download_deadline(deadline, suspended_before)?)?;
        if let Some(approval) = &self.download_approval {
            approval.validate()?;
        }
        let mut approvals = self
            .download_approvals
            .lock()
            .map_err(|_| Error::new(-32011, "Download approval state unavailable"))?;
        if !Arc::ptr_eq(epoch, &approvals.epoch) {
            return Err(Error::new(
                -32014,
                "Download approval session changed during review",
            ));
        }
        if persist && approvals.origins.len() >= 1024 && !approvals.origins.contains_key(&key) {
            return Err(Error::new(
                -32011,
                "Download approval origin limit exceeded",
            ));
        }
        if persist {
            approvals.origins.insert(key, accepted);
        }
        if accepted {
            Ok(())
        } else {
            Err(Error::new(-32012, "guardian_denied"))
        }
    }
    /// Connection teardown revokes transient grants, including pending reviews.
    /// Trusted configuration remains the embedding host's explicit authority.
    pub fn clear_download_approvals(&self) {
        if let Ok(mut approvals) = self.download_approvals.lock() {
            *approvals = DownloadApprovals::default();
        }
    }
    pub fn check_browser_command(&self, method: &str) -> Result<()> {
        if self.browser_restricted()
            && [
                "evaluate",
                "cdp_call",
                "back",
                "forward",
                "tabs_content",
                // These routes dispatch to the currently focused control, a
                // viewport point, or an opaque browser-managed target. A main
                // document origin does not establish the recipient's origin.
                "click",
                "drag",
                "drag_path",
                "move",
                "scroll",
                "scroll_pixels",
                "press_key",
                "type_text",
                "paste",
                "set_value",
                "select_text",
                "locator_click",
                "locator_set_checked",
                "locator_press",
                "locator_press_sequentially",
                "locator_type",
                "locator_screenshot",
                "element_info",
                "element_screenshot",
                "locator_download_media",
                "file_chooser_set_files",
                "dialog_handle",
                "webmcp_invoke",
                // Installing the clipboard bridge affects future documents and
                // child frames without a per-document origin gate.
                "clipboard_read",
                "clipboard_read_text",
                "clipboard_write",
                "clipboard_write_text",
            ]
            .contains(&method)
        {
            return Err(Error::new(
                -32010,
                "Command unavailable under restricted origin policy",
            ));
        }
        Ok(())
    }
    /// These operations validate URL and time origin in the same renderer
    /// callback that reads or changes the selected document.
    pub fn browser_document_bound(method: &str) -> bool {
        [
            "locator_fill",
            "locator_select_option",
            "locator_count",
            "locator_all_text_contents",
            "locator_read_all",
            "locator_text_content",
            "locator_inner_text",
            "locator_get_attribute",
            "locator_is_enabled",
            "locator_is_visible",
            "locator_element_info",
            "locator_inspect",
            "locator_wait_for",
            "readonly_evaluate",
            "dom_snapshot",
        ]
        .contains(&method)
    }
    pub fn check_browser_frame(&self, method: &str, args: &Value) -> Result<()> {
        if self.browser_restricted()
            && let Some(frame) = args.get("frame")
            && (!frame.as_str().is_some_and(|s| !s.is_empty())
                || (!Self::browser_document_bound(method) && method != "document_context"))
        {
            return Err(Error::new(
                -32010,
                "Target frame cannot be bound under restricted origin policy",
            ));
        }
        Ok(())
    }
    /// Accept only actual HTTP(S) document identities returned by the provider.
    /// Opaque/inherited origins are deliberately unsupported under restrictions.
    pub fn check_browser_document(&self, context: &Value) -> Result<()> {
        let unknown = || {
            Error::new(
                -32010,
                "Target document identity is unknown under restricted origin policy",
            )
        };
        let url = context["url"].as_str().ok_or_else(unknown)?;
        origin(url).map_err(|_| unknown())?;
        self.check_url(url)?;
        if !context["frameId"].as_str().is_some_and(|s| !s.is_empty())
            || !context["documentToken"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        {
            return Err(unknown());
        }
        Ok(())
    }
    pub fn browser_restricted(&self) -> bool {
        self.origin_approval_required() || !self.allowed.is_empty() || !self.denied.is_empty()
    }
    pub fn snapshot(&self) -> Value {
        json!({"allowedApps":self.config.allowed_apps,"allowedOrigins":self.allowed,"deniedOrigins":self.denied,"preapprovedDownloadOrigins":self.preapproved_downloads,"authenticationBrokerAvailable":self.broker.is_some(),"automatedReviewerAvailable":self.reviewer.is_some()})
    }
    pub fn review(&self, context: Value) -> Result<()> {
        self.review_request_until(context, "Authentication", None)
    }
    fn review_request_until(
        &self,
        context: Value,
        purpose: &str,
        deadline: Option<Instant>,
    ) -> Result<()> {
        if self.config.review_instructions.trim().is_empty() {
            return Err(Error::new(
                -32011,
                format!("{purpose} review instructions are not configured"),
            ));
        }
        let reviewer = self
            .reviewer
            .as_ref()
            .ok_or_else(|| Error::new(-32011, format!("{purpose} reviewer unavailable")))?;
        let request = json!({"type":"review","instructions":self.config.review_instructions,"context":context,"metadata":{"automated":true,"sensitive":true,"strict":true}});
        let result = if let Some(deadline) = deadline {
            reviewer.request_until(&request, deadline)?
        } else {
            reviewer.request(&request)?
        };
        automated_decision(&result)
    }
}
pub fn origin(value: &str) -> Result<String> {
    let url = url::Url::parse(value).map_err(|_| Error::invalid("Invalid URL"))?;
    if !["https", "http"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::invalid("Expected HTTP(S) URL without userinfo"));
    }
    Ok(url.origin().ascii_serialization())
}
pub fn automated_decision(result: &Value) -> Result<()> {
    let recognized = result["reviewer"]
        .as_str()
        .is_some_and(|r| ["auto_review", "guardian_subagent"].contains(&r));
    match result["action"].as_str() {
        Some("accept") if recognized => Ok(()),
        Some("decline") if recognized || result.get("reviewer").is_none_or(Value::is_null) => {
            Err(Error::new(-32012, "guardian_denied"))
        }
        Some("cancel") if recognized => Err(Error::new(-32013, "approval_cancelled")),
        _ => Err(Error::new(
            -32011,
            "Automated review did not authorize this action",
        )),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Document {
    pub browser: String,
    pub tab: String,
    pub url: String,
    #[serde(rename = "documentToken")]
    pub token: String,
    #[serde(rename = "frameId")]
    pub frame: String,
}
impl Document {
    pub fn validate(&self, current: &Self) -> Result<()> {
        if self.browser != current.browser || self.tab != current.tab {
            return Err(Error::new(-32014, "target_changed"));
        }
        if origin(&self.url)? != origin(&current.url)? {
            return Err(Error::new(-32014, "origin_changed"));
        }
        if self.token.is_empty()
            || self.frame.is_empty()
            || self.frame != current.frame
            || self.token != current.token
            || self.url != current.url
        {
            return Err(Error::new(-32014, "page_changed"));
        }
        Ok(())
    }
    pub fn args(&self) -> Value {
        json!({"browser":self.browser,"tab":self.tab,"expectedDocumentToken":self.token,"expectedUrl":self.url})
    }
}

#[cfg(test)]
mod origin_access_tests {
    use super::*;
    #[test]
    fn host_native_authorization_preserves_app_and_origin_restrictions() {
        let mut security = Security::new(SecurityConfig {
            allowed_apps: vec!["owned.app".into()],
            allowed_origins: vec!["https://owned.example".into()],
            ..Default::default()
        })
        .unwrap();
        security.authorize_native_control();
        assert!(security.check_app("owned.app").is_ok());
        assert!(security.check_app("other.app").is_err());
        assert!(security.check_url("https://owned.example/").is_ok());
        assert!(security.check_url("https://other.example/").is_err());
    }
    #[test]
    fn origin_access_grants_are_canonical_scoped_and_separate_from_downloads() {
        let mut security = Security::new(SecurityConfig {
            require_origin_approval: true,
            preapproved_download_origins: vec!["https://owned.example".into()],
            preapproved_origin_access: vec!["https://EXAMPLE.COM:443/path".into()],
            ..Default::default()
        })
        .unwrap();
        assert!(
            security
                .origin_access("a", "https://example.com/x")
                .unwrap()
                .is_none()
        );
        for url in [
            "https://owned.example/x",
            "https://example.com.attacker.test/",
            "http://example.com/",
            "https://example.com:444/",
        ] {
            assert!(security.origin_access("a", url).unwrap().is_some());
        }
        security
            .grant_origin_access("a", "https://owned.example")
            .unwrap();
        assert!(
            security
                .origin_access("a", "https://OWNED.EXAMPLE:443/x")
                .unwrap()
                .is_none()
        );
        assert!(
            security
                .origin_access("b", "https://owned.example")
                .unwrap()
                .is_some()
        );
        security.clear_origin_scope("a");
        assert!(
            security
                .origin_access("a", "https://owned.example")
                .unwrap()
                .is_some()
        );
        assert!(
            security
                .origin_access("a", "about:blank")
                .unwrap()
                .is_none()
        );
        let denied = Security::new(SecurityConfig {
            require_origin_approval: true,
            denied_origins: vec!["https://owned.example".into()],
            preapproved_origin_access: vec!["https://owned.example".into()],
            ..Default::default()
        })
        .unwrap();
        assert!(denied.origin_access("a", "https://owned.example").is_err());
    }
}
