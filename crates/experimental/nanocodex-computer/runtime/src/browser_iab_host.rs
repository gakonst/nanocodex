//! Detached CDP implementation of the IAB host callbacks. This is a real owned
//! renderer/window backend, not an implementation of an attached Electron sidebar.
use super::{
    Browser,
    iab::{Host, RouteConfig, SessionParams, Viewport},
};
use crate::{Error, Result, engine::string};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct State {
    context_id: Option<String>,
    released: BTreeSet<String>,
    released_context: bool,
    logical: BTreeMap<String, Page>,
    #[serde(skip)]
    pub binding: Option<InputBinding>,
    #[serde(skip)]
    pub diagnostics: Vec<String>,
    recorded_turn: Option<String>,
    creating: Option<String>,
    #[serde(default)]
    disposing: Option<String>,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Page {
    target: Option<String>,
    viewport: Option<Viewport>,
    metrics_revision: u64,
    applied_revision: u64,
    show: Option<bool>,
    active: bool,
}
pub(super) struct InputBinding {
    pub token: String,
    tab: String,
    document: Value,
    generation: u64,
}
impl State {
    pub(super) fn context_id(&self) -> Option<&str> {
        self.context_id.as_deref()
    }
    pub(super) fn resolve_context(&mut self, context: &str) {
        self.context_id = Some(context.into());
        self.binding = None;
    }
    pub(super) fn observe_intent(&mut self, method: &str) {
        if method == "Target.disposeBrowserContext" {
            self.disposing = self.context_id.clone();
        }
    }
    pub(super) fn observe_failure(&mut self, method: &str, error: &Error) {
        if method == "Target.disposeBrowserContext" && error.code != -32006 {
            self.disposing = None;
        }
    }
    pub(super) fn restored(&mut self) {
        self.binding = None;
        for page in self.logical.values_mut() {
            page.metrics_revision = page.metrics_revision.saturating_add(1);
            page.applied_revision = 0;
        }
    }
    pub(super) fn observe_result(&mut self, method: &str, result: &Value) {
        if method == "Target.disposeBrowserContext" {
            *self = Self::default();
        }
        if method == "Target.createBrowserContext"
            && let Some(context) = result["browserContextId"].as_str()
        {
            self.context_id = Some(context.into());
        }
        if method == "Target.createTarget"
            && let Some(logical) = self.creating.take()
            && let Some(target) = result["targetId"].as_str()
        {
            self.logical.entry(logical).or_default().target = Some(target.into());
        }
    }
}
struct CdpHost<'a> {
    browser: &'a mut Browser,
    route: RouteConfig,
}
impl CdpHost<'_> {
    fn missing_window(&self) -> Error {
        Error::action(format!(
            "ChatGPT browser window {} is no longer available for browser session {}",
            self.route.window_id,
            self.route.session_id()
        ))
    }
    fn target(&self, logical: &str) -> Option<String> {
        self.browser
            .iab_host
            .logical
            .get(logical)
            .and_then(|page| page.target.clone())
    }
    fn window(&mut self, target: &str) -> Result<Value> {
        self.browser
            .call(
                "Browser.getWindowForTarget",
                json!({"targetId":target}),
                None,
            )
            .map_err(|_| self.missing_window())
    }
    fn apply_visibility(&mut self, target: &str, visible: bool) -> Result<()> {
        let window = self.window(target)?;
        self.browser.call("Browser.setWindowBounds",json!({"windowId":window["windowId"],"bounds":{"windowState":if visible {"normal"}else{"minimized"}}}),None)?;
        Ok(())
    }
}
impl Host for CdpHost<'_> {
    fn ensure_available(&mut self) -> Result<()> {
        self.browser.client().map(|_| ()).map_err(|_| {
            Error::action("No in-app Browser host is available for this browser-use session")
        })
    }
    fn record_turn(&mut self, params: &SessionParams) -> Result<()> {
        self.browser.iab_host.recorded_turn = Some(params.turn_id.clone());
        Ok(())
    }
    fn active(&mut self, value: bool, tab: &str) -> Result<()> {
        if value {
            self.browser
                .iab_host
                .logical
                .entry(tab.into())
                .or_default()
                .active = true;
        } else if let Some(page) = self.browser.iab_host.logical.get_mut(tab) {
            page.active = false;
        }
        Ok(())
    }
    fn viewport(&mut self, value: Option<Viewport>, tab: Option<&str>) -> Result<()> {
        let Some(tab) = tab else {
            return if value.is_some() {
                Err(Error::action(
                    "A browser tab id is required to set the viewport",
                ))
            } else {
                Ok(())
            };
        };
        let page = self.browser.iab_host.logical.entry(tab.into()).or_default();
        if page.viewport != value {
            page.viewport = value;
            page.metrics_revision += 1;
        }
        Ok(())
    }
    fn wait_sync(&mut self, tab: Option<&str>) -> Result<()> {
        let Some(logical) = tab else { return Ok(()) };
        let Some(page) = self.browser.iab_host.logical.get(logical) else {
            return Ok(());
        };
        let Some(target) = page.target.clone() else {
            return Ok(());
        };
        if page.metrics_revision == page.applied_revision {
            return Ok(());
        }
        let (revision, viewport) = (page.metrics_revision, page.viewport);
        let result=match viewport {
            Some(viewport)=>self.browser.tab(&target,"Emulation.setDeviceMetricsOverride",json!({"width":viewport.width,"height":viewport.height,"deviceScaleFactor":1,"mobile":false})),
            None=>self.browser.tab(&target,"Emulation.clearDeviceMetricsOverride",json!({})),
        };
        // Captured ZB/waitForPendingDebuggerSync log and swallow metric errors.
        // Keep an observable diagnostic and never describe this as visual success.
        if let Err(error) = result {
            let diagnostics = &mut self.browser.iab_host.diagnostics;
            if diagnostics.len() >= 128 {
                diagnostics.remove(0);
            }
            diagnostics.push(format!(
                "Viewport synchronization failed: {}",
                error.message
            ));
        }
        if let Some(page) = self.browser.iab_host.logical.get_mut(logical) {
            page.applied_revision = revision;
        }
        Ok(())
    }
    fn visible(&mut self, value: bool, tab: Option<&str>) -> Result<()> {
        let Some(tab) = tab else {
            return if value {
                Err(Error::action(
                    "A browser tab id is required to show the browser",
                ))
            } else {
                Ok(())
            };
        };
        let page = self.browser.iab_host.logical.entry(tab.into()).or_default();
        page.show = Some(value);
        if value {
            page.active = true;
        }
        if let Some(target) = page.target.clone() {
            self.apply_visibility(&target, value)?;
        }
        Ok(())
    }
    fn is_visible(&mut self, tab: Option<&str>) -> Result<bool> {
        let Some(target) = tab.and_then(|tab| self.target(tab)) else {
            return Ok(false);
        };
        let window = self.window(&target)?;
        Ok(window["bounds"]["windowState"] != "minimized")
    }
    fn open(&mut self, logical: &str, url: &str) -> Result<String> {
        if self.browser.iab_host.context_id.is_none() {
            let response = self.browser.call(
                "Target.createBrowserContext",
                json!({"disposeOnDetach":false}),
                None,
            )?;
            self.browser.iab_host.context_id = Some(string(&response, "browserContextId")?.into());
        }
        let context = self.browser.iab_host.context_id.clone().unwrap();
        let first = !self
            .browser
            .iab_host
            .logical
            .values()
            .any(|page| page.target.is_some());
        self.browser.iab_host.creating = Some(logical.into());
        let created = self.browser.call(
            "Target.createTarget",
            json!({"url":"about:blank","browserContextId":context,"newWindow":first,"background":true}),
            None,
        )?;
        let target = string(&created, "targetId")?.to_owned();
        self.browser
            .iab_host
            .logical
            .entry(logical.into())
            .or_default()
            .target = Some(target.clone());
        let initialized = (|| {
            self.browser.page(&target)?;
            if url != "about:blank" {
                self.browser.navigate_url(&target, url)?;
            }
            self.wait_sync(Some(logical))?;
            if let Some(visible) = self
                .browser
                .iab_host
                .logical
                .get(logical)
                .and_then(|page| page.show)
            {
                self.apply_visibility(&target, visible)?;
            }
            Ok(())
        })();
        if let Err(error) = initialized {
            if let Err(cleanup) = self.close(logical) {
                self.browser.iab_host.diagnostics.push(format!(
                    "Failed to close incomplete IAB page: {}",
                    cleanup.message
                ));
            }
            return Err(error);
        }
        Ok(target)
    }
    fn unfinished_target(&self, logical: &str) -> Option<String> {
        self.target(logical)
    }
    fn cleanup(&mut self, logical: &str) -> Result<()> {
        if let Some(target) = self.target(logical) {
            self.browser.cancel_choosers(Some(&target));
            self.browser.shutdown_downloads(Some(&target))?;
            self.browser.cleanup_clipboard(&target)?;
            for session in self.browser.surface.frame_sessions_for_tab(&target) {
                self.browser.call(
                    "Target.detachFromTarget",
                    json!({"sessionId":session}),
                    None,
                )?;
            }
            if let Some(session) = self.browser.sessions.get(&target).cloned() {
                self.browser.call(
                    "Target.detachFromTarget",
                    json!({"sessionId":session}),
                    None,
                )?;
                self.browser.sessions.remove(&target);
            }
            self.browser.surface.remove_tab(&target);
            self.browser.contract.remove_tab(&target);
        }
        Ok(())
    }
    fn release(&mut self, logical: &str) -> Result<()> {
        if let Some(target) = self.target(logical) {
            self.browser.iab_host.released.insert(target);
            self.browser.iab_host.released_context = true;
        }
        self.browser.iab_host.logical.remove(logical);
        Ok(())
    }
    fn close(&mut self, logical: &str) -> Result<()> {
        if let Some(target) = self.target(logical) {
            let result =
                self.browser
                    .call("Target.closeTarget", json!({"targetId":target}), None)?;
            if result["success"] == false {
                return Err(Error::action("IAB target close was rejected"));
            }
            self.browser.sessions.remove(&target);
            self.browser.surface.remove_tab(&target);
            self.browser.contract.remove_tab(&target);
        }
        self.browser.iab_host.logical.remove(logical);
        Ok(())
    }
}
impl Browser {
    fn iab_reconcile(&mut self, authority: &mut super::iab::Authority) -> Result<Vec<Value>> {
        let response = self.call("Target.getTargets", json!({}), None)?;
        let targets = response["targetInfos"]
            .as_array()
            .ok_or_else(|| Error::action("CDP target list is missing"))?;
        let missing: Vec<_> = authority
            .tabs
            .keys()
            .filter(|id| {
                !targets
                    .iter()
                    .any(|target| target["targetId"].as_str() == Some(id.as_str()))
            })
            .cloned()
            .collect();
        for id in missing {
            authority.remove(&id);
            self.iab_host
                .logical
                .retain(|_, page| page.target.as_deref() != Some(&id));
            self.sessions.remove(&id);
            self.surface.remove_tab(&id);
            self.contract.remove_tab(&id);
        }
        // Released pages may create children after control ends, including
        // descendants whose opener already closed. Preserve that lineage before
        // pruning dead IDs. Once a context contains released pages, anonymous
        // new targets are quarantined unless traced to a still-owned opener.
        if let Some(context) = self.iab_host.context_id.as_deref() {
            let mut controlled: BTreeSet<String> = authority.tabs.keys().cloned().collect();
            loop {
                let mut changed = false;
                for target in targets {
                    if target["type"] != "page"
                        || target["browserContextId"].as_str() != Some(context)
                    {
                        continue;
                    }
                    let Some(id) = target["targetId"].as_str() else {
                        continue;
                    };
                    if let Some(opener) = target["openerId"].as_str() {
                        if self.iab_host.released.contains(opener) {
                            changed |= self.iab_host.released.insert(id.into());
                        } else if controlled.contains(opener)
                            && !self.iab_host.released.contains(id)
                        {
                            changed |= controlled.insert(id.into());
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            if self.iab_host.released_context {
                for target in targets {
                    if target["type"] == "page"
                        && target["browserContextId"].as_str() == Some(context)
                        && let Some(id) = target["targetId"].as_str()
                        && !controlled.contains(id)
                    {
                        self.iab_host.released.insert(id.into());
                    }
                }
            }
        }
        self.iab_host.released.retain(|id| {
            targets
                .iter()
                .any(|target| target["targetId"].as_str() == Some(id))
        });
        // A route has a private BrowserContext. Popups and a create response lost
        // after dispatch belong to that context too, excluding released results.
        if let Some(context) = self.iab_host.context_id.clone() {
            for target in targets {
                if target["type"] != "page"
                    || target["browserContextId"].as_str() != Some(context.as_str())
                {
                    continue;
                }
                let id = string(target, "targetId")?;
                if self.iab_host.released.contains(id) || authority.tabs.contains_key(id) {
                    continue;
                }
                let logical = self
                    .iab_host
                    .logical
                    .iter()
                    .find(|(_, page)| page.target.as_deref() == Some(id))
                    .map(|(logical, _)| logical.clone())
                    .unwrap_or_else(|| format!("recovered:{id}"));
                self.iab_host
                    .logical
                    .entry(logical.clone())
                    .or_default()
                    .target = Some(id.into());
                authority.tabs.insert(
                    id.into(),
                    super::iab::Tab {
                        id: id.into(),
                        logical_id: logical,
                        active: false,
                        mark: None,
                        mark_turn: None,
                        completed_turn: None,
                    },
                );
            }
        }
        Ok(targets
            .iter()
            .filter(|target| {
                target["targetId"]
                    .as_str()
                    .is_some_and(|id| authority.tabs.contains_key(id))
            })
            .cloned()
            .collect())
    }
    pub(super) fn recover_iab(&mut self) -> Result<()> {
        // Trusted recovery may reconcile or replace the route-owned context.
        // Pending and frozen reads never cross that authority transition.
        self.cancel_raw_waits();
        let Some(context) = self.iab_host.context_id.clone() else {
            // No route-owned context can contain surviving controlled targets.
            let authority = self.iab.as_mut().unwrap();
            for id in authority.tabs.keys().cloned().collect::<Vec<_>>() {
                authority.remove(&id);
            }
            self.durable_save()?;
            return Ok(());
        };
        let contexts = self.call("Target.getBrowserContexts", json!({}), None)?;
        if !contexts["browserContextIds"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(context)))
        {
            if self.iab_host.disposing.as_deref() == Some(&context) {
                // Context deletion was sent but its acknowledgement was lost.
                // Its absence confirms completion without replaying deletion.
                self.iab_host = State::default();
                return self.recover_iab();
            }
            return Err(Error::action(
                "Recovered IAB browser context is unavailable; no replacement was created",
            ));
        }
        let mut authority = self.iab.take().unwrap();
        let reconciled = self.iab_reconcile(&mut authority);
        self.iab = Some(authority);
        reconciled?;
        let route = self.iab.as_ref().unwrap().route.clone();
        let tabs: Vec<_> = self.iab_host.logical.keys().cloned().collect();
        for logical in tabs {
            CdpHost {
                browser: self,
                route: route.clone(),
            }
            .wait_sync(Some(&logical))?;
        }
        self.durable_save()
    }
    pub(super) fn end_iab_session(&mut self) -> Vec<Error> {
        let Some(mut authority) = self.iab.take() else {
            return vec![];
        };
        let mut errors = vec![];
        if self.iab_host.context_id.is_some()
            && let Err(error) = self.iab_reconcile(&mut authority)
        {
            errors.push(error);
        }
        let route = authority.route.clone();
        errors.extend(authority.finish(&mut CdpHost {
            browser: self,
            route,
        }));
        if authority.tabs.is_empty()
            && self.iab_host.released.is_empty()
            && let Some(context) = self.iab_host.context_id.clone()
        {
            match self.call(
                "Target.disposeBrowserContext",
                json!({"browserContextId":context}),
                None,
            ) {
                Ok(_) => self.iab_host = State::default(),
                Err(error) => errors.push(error),
            }
        }
        self.iab = Some(authority);
        self.iab_host.binding = None;
        if let Err(error) = self.durable_save() {
            errors.push(error);
        }
        errors
    }
    pub(super) fn iab_command(&mut self, method: &str, args: &Value) -> Result<Option<Value>> {
        let Some(authority) = self.iab.as_ref() else {
            return Ok(None);
        };
        authority.context()?;
        // A request cannot choose its trusted route/session/turn. Metadata setter
        // APIs are available only to the embedding host, not browser RPC commands.
        self.iab_host.binding = None;
        let fallback = match method {
            "visibility_get" => Some("browser_visibility_get"),
            "visibility_set" => Some("browser_visibility_set"),
            "viewport_set" => Some("browser_viewport_set"),
            "viewport_reset" => Some("browser_viewport_reset"),
            "tabs_content" => Some("tabs_content"),
            "element_info" => Some("playwright_element_info"),
            "element_screenshot" => Some("playwright_element_screenshot"),
            "content_export" => Some("tab_content_export"),
            "download_media" => Some("cua_download_media"),
            "management_call" => Some("browser_management_call"),
            "management_get_audit_trail" => Some("browser_management_get_audit_trail"),
            "runtime_config" => Some("runtime_config"),
            _ => None,
        };
        if matches!(
            method,
            "user_claim_tab" | "user_open_tabs" | "user_history" | "cdp_call"
        ) {
            return Err(Error::unsupported(format!(
                "IAB provider API is unavailable: {method}"
            )));
        }
        if fallback.is_some()
            || method == "iab_fallback"
            || method == "new_tab"
            || method == "close_tab"
        {
            let mut authority = self.iab.take().unwrap();
            let route = authority.route.clone();
            let result = (|| {
                let mut host = CdpHost {
                    browser: self,
                    route,
                };
                if method == "new_tab" {
                    let id = authority
                        .create(&mut host, args["url"].as_str().unwrap_or("about:blank"))?;
                    return Ok(json!({"targetId":id}));
                }
                if method == "close_tab" {
                    let tab = string(args, "tab")?;
                    let logical = authority.require_tab(tab)?.logical_id.clone();
                    host.close(&logical)?;
                    authority.remove(tab);
                    return Ok(json!({}));
                }
                let mut payload = args.clone();
                if let Some(command) = fallback {
                    payload["type"] = json!(command);
                }
                authority.fallback(&mut host, &payload)
            })();
            self.iab = Some(authority);
            return result.map(Some);
        }
        if let Some(tab) = args["tab"].as_str() {
            self.iab.as_ref().unwrap().require_tab(tab)?;
        }
        match method {
            "mark_tab" => {
                let tab = string(args, "tab")?;
                self.iab
                    .as_mut()
                    .unwrap()
                    .mark(tab, string(args, "status")?)?;
                Ok(Some(json!({"tab":tab})))
            }
            "selected_tab" => Ok(Some(
                json!({"tab":self.iab.as_ref().unwrap().selected().map(|tab|tab.id.clone())}),
            )),
            "list_tabs" => {
                let mut authority = self.iab.take().unwrap();
                let result = self.iab_reconcile(&mut authority);
                self.iab = Some(authority);
                result.map(|targets| Some(json!(targets)))
            }
            _ => Ok(None),
        }
    }
    pub(super) fn iab_cdp_guard(
        &mut self,
        method: &str,
        args: &mut Value,
        session: Option<&str>,
    ) -> Result<()> {
        if self.iab.is_some() && method == "Browser.setDownloadBehavior" {
            let context =
                self.iab_host.context_id.as_ref().ok_or_else(|| {
                    Error::action("IAB downloads require an owned browser context")
                })?;
            args["browserContextId"] = json!(context);
        }
        if self.iab.is_none() || !method.starts_with("Input.") {
            return Ok(());
        }
        let session =
            session.ok_or_else(|| Error::action("IAB input requires an owned renderer session"))?;
        let tab = self
            .sessions
            .iter()
            .find(|(_, id)| id.as_str() == session)
            .map(|(tab, _)| tab.clone())
            .or_else(|| self.surface.frame_session_tab(session))
            .ok_or_else(|| Error::action("IAB input renderer is detached"))?;
        self.iab.as_ref().unwrap().require_tab(&tab)?;
        self.call(
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled":true}),
            Some(session),
        )?;
        if !matches!(method, "Input.dispatchKeyEvent" | "Input.insertText") {
            return Ok(());
        }
        let frame = self.focused_frame(&tab)?;
        let document =
            self.document_context(&tab, &json!({"frame":frame["id"],"isolated":true}))?;
        let generation = self.iab.as_ref().unwrap().generation;
        let expected = args
            .as_object_mut()
            .and_then(|args| args.remove("__codexIabExpectedInputTargetToken"));
        if let Some(binding) = self.iab_host.binding.as_ref() {
            if binding.tab != tab
                || binding.document != document
                || binding.generation != generation
                || expected
                    .as_ref()
                    .is_some_and(|value| value.as_str() != Some(&binding.token))
            {
                return Err(Error::action("IAB input target changed or detached"));
            }
        } else {
            if expected.is_some() {
                return Err(Error::action("IAB input target token is stale"));
            }
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes)
                .map_err(|_| Error::action("Cannot allocate IAB input target token"))?;
            self.iab_host.binding = Some(InputBinding {
                token: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                tab,
                document,
                generation,
            });
        }
        Ok(())
    }
    pub(super) fn iab_event(&mut self, event: &Value) {
        let Some(authority) = self.iab.as_mut() else {
            return;
        };
        if matches!(
            event["method"].as_str(),
            Some(
                "Page.frameNavigated"
                    | "Page.frameDetached"
                    | "Runtime.executionContextsCleared"
                    | "Target.detachedFromTarget"
            )
        ) {
            authority.invalidate_inputs();
        }
        if event["method"] == "Target.targetDestroyed"
            && let Some(tab) = event["params"]["targetId"].as_str()
        {
            authority.remove(tab);
        }
    }
    pub(super) fn iab_info(&self, id: &str) -> Value {
        let authority = self.iab.as_ref().unwrap();
        let known: Value = serde_json::from_str(include_str!("browser_capabilities.json")).unwrap();
        json!({"id":id,"name":"Skyre In-app Browser","type":"iab","version":env!("CARGO_PKG_VERSION"),"apiSupportOverrides":{"Browser.user":false,"Browser.history":false,"Tab.markDeliverable":true,"Tab.markHandoff":true,"Tab.ax":true,"Tabs.content":false},"capabilities":{"browser":[known["browser"]["visibility"],known["browser"]["viewport"]],"tab":[]},"metadata":{"codexSessionId":authority.route.session_id(),"implementation":"independent-detached-cdp","conversationId":authority.route.conversation_id,"threadId":authority.route.thread_id,"windowId":authority.route.window_id}})
    }
}
