use super::{
    Browser,
    artifacts::Artifacts,
    clipboard::{self, Clipboard},
};
use crate::{Error, Result, engine::string};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Surface {
    pub events: VecDeque<(u64, Value)>,
    sequence: u64,
    event_bytes: usize,
    pub(super) dialogs: Arc<Mutex<super::dialog::State>>,
    pub(super) raw_events: Arc<Mutex<super::raw_events::Log>>,
    pub(super) choosers: super::chooser::State,
    pub(super) children: super::children::State,
    pub(super) screencast: super::screencast::State,
    pub(super) frame_sessions: BTreeMap<(String, String), String>,
    pub(super) frame_parents: BTreeMap<(String, String), String>,
    page_enabled: BTreeSet<String>,
    clipboard_scripts: BTreeMap<String, String>,
    pub clipboard: Arc<Mutex<Clipboard>>,
    pub artifacts: Artifacts,
    pub(super) downloads: super::downloads::Shared,
    pub(super) webmcp: Arc<Mutex<super::webmcp::State>>,
    pub(super) webmcp_responses: BTreeMap<String, Value>,
    marked: BTreeSet<String>,
    selected: Option<String>,
}
impl Surface {
    pub(super) fn frame_sessions_for_tab(&self, tab: &str) -> Vec<String> {
        self.frame_sessions
            .iter()
            .filter(|((owner, _), _)| owner == tab)
            .map(|(_, session)| session.clone())
            .collect()
    }
    pub(super) fn frame_session_tab(&self, session: &str) -> Option<String> {
        self.frame_sessions
            .iter()
            .find(|(_, id)| id.as_str() == session)
            .map(|((tab, _), _)| tab.clone())
    }

    pub fn event(
        &mut self,
        event: Value,
        sessions: &mut BTreeMap<String, String>,
        internal_capture: bool,
    ) {
        self.child_event(&event, sessions);
        let session = event["sessionId"].as_str();
        let tab = session.and_then(|s| {
            sessions
                .iter()
                .find(|(_, v)| v.as_str() == s)
                .map(|(k, _)| k.clone())
                .or_else(|| self.frame_session_tab(s))
        });
        self.choosers.observe(&event, tab.as_deref());
        match event["method"].as_str().unwrap_or("") {
            "WebMCP.toolResponded" => {
                if let Some(id) = event["params"]["invocationId"].as_str() {
                    self.webmcp_responses
                        .insert(id.into(), event["params"].clone());
                }
            }
            "Page.frameDetached" => {
                if let (Some(tab), Some(frame)) = (&tab, event["params"]["frameId"].as_str())
                    && let Some(session) = self
                        .frame_sessions
                        .get(&(tab.clone(), frame.to_owned()))
                        .cloned()
                {
                    self.forget_child_session(&session);
                }
            }
            "Target.detachedFromTarget" => {
                let sid = event["params"]["sessionId"].as_str();
                if let Some(sid) = sid {
                    self.forget_child_session(sid);
                }
                self.frame_sessions
                    .retain(|_, value| Some(value.as_str()) != sid);
                self.frame_parents
                    .retain(|key, _| self.frame_sessions.contains_key(key));
                if let Some(tab) = sessions
                    .iter()
                    .find(|(_, s)| Some(s.as_str()) == sid)
                    .map(|(t, _)| t.clone())
                {
                    sessions.remove(&tab);
                    self.remove_tab(&tab);
                }
            }
            "Target.targetDestroyed" => {
                if let Some(tab) = event["params"]["targetId"].as_str() {
                    let children: Vec<_> = self
                        .frame_sessions
                        .iter()
                        .filter(|((_, target), _)| target == tab)
                        .map(|(_, session)| session.clone())
                        .collect();
                    for session in children {
                        self.forget_child_session(&session);
                    }
                    sessions.remove(tab);
                    self.remove_tab(tab);
                }
            }
            _ => {}
        }
        if internal_capture {
            return;
        }
        self.sequence += 1;
        let size = event.to_string().len();
        if size > 4 * 1024 * 1024 {
            return;
        }
        while self.events.len() >= 1024 || self.event_bytes + size > 4 * 1024 * 1024 {
            if let Some((_, old)) = self.events.pop_front() {
                self.event_bytes = self.event_bytes.saturating_sub(old.to_string().len());
            } else {
                self.event_bytes = 0;
                break;
            }
        }
        self.event_bytes += size;
        self.events.push_back((self.sequence, event));
    }
    pub fn remove_tab(&mut self, tab: &str) {
        self.screencast.forget(tab);
        self.raw_events.lock().unwrap().remove(tab);
        self.webmcp.lock().unwrap().remove(tab);
        self.frame_sessions.retain(|(t, _), _| t != tab);
        self.frame_parents.retain(|(t, _), _| t != tab);
        self.dialogs.lock().unwrap().remove(tab);
        self.choosers.remove_tab(tab);
        self.downloads.lock().unwrap().remove_tab(tab);
        self.page_enabled.remove(tab);
        self.clipboard_scripts.remove(tab);
        self.marked.remove(tab);
        if self.selected.as_deref() == Some(tab) {
            self.selected = None;
        }
    }
    pub fn disconnect(&mut self) {
        self.screencast.disconnect();
        self.raw_events.lock().unwrap().disconnect();
        self.webmcp.lock().unwrap().disconnect();
        self.webmcp_responses.clear();
        self.children = Default::default();
        self.frame_sessions.clear();
        self.frame_parents.clear();
        self.dialogs.lock().unwrap().clear();
        self.choosers.clear();
        self.downloads.lock().unwrap().disconnect();
        self.page_enabled.clear();
        self.clipboard_scripts.clear();
        self.selected = None;
    }
}

pub(super) fn normalize(method: &str, args: &Value) -> Result<(String, Value)> {
    if !args.is_object() {
        return Err(Error::invalid("Browser arguments must be an object"));
    }
    if let Some(request) = super::webmcp::normalize(method, args)? {
        return Ok(request);
    }
    let mut a = args.clone();
    if let Some(selector) = a["selector"].as_str() {
        let (parsed, frame) = super::selectors::parse(selector)?;
        a["selector"] = parsed;
        if let Some(frame) = frame {
            if a.get("frame").is_some() {
                return Err(Error::invalid("Selector and explicit frame conflict"));
            }
            a["frame"] = json!(frame);
        }
    }
    if method == "tab_ax_action" {
        let action = a["action"]
            .as_object()
            .ok_or_else(|| Error::invalid("AX action must be an object"))?
            .clone();
        let kind = action["kind"]
            .as_str()
            .ok_or_else(|| Error::invalid("AX action kind missing"))?
            .to_owned();
        if kind == "tab_ax_action" {
            return Err(Error::invalid(
                "Recursive AX action envelopes are not allowed",
            ));
        }
        a.as_object_mut().unwrap().extend(action);
        if let Some(target) = a.get("target").cloned() {
            if target.is_number() {
                a["element_index"] = target;
            } else {
                a["point"] = target;
            }
        }
        return normalize(&kind, &a);
    }

    for (from, to) in [
        ("browser_id", "browser"),
        ("tab_id", "tab"),
        ("timeout_ms", "timeoutMs"),
        ("node_id", "element_index"),
        ("button", "mouseButton"),
        ("mouse_button", "mouseButton"),
        ("relative_selector", "relativeSelector"),
        ("click_count", "clickCount"),
        ("selections", "values"),
        ("prompt_text", "promptText"),
        ("file_chooser_id", "fileChooserId"),
        ("script", "expression"),
        ("tool_name", "name"),
        ("registration_id", "registrationId"),
        ("input", "arguments"),
        ("selection_type", "selectionType"),
    ] {
        if a.get(to).is_none()
            && let Some(v) = a.get(from)
        {
            a[to] = v.clone();
        }
    }
    if let Some(relative) = a["relativeSelector"].as_str() {
        let (parsed, frame) = super::selectors::parse(relative)?;
        if frame.is_some() {
            return Err(Error::invalid("Relative selectors cannot cross frames"));
        }
        a["relativeSelector"] = parsed;
    }
    if let Some(button) = a.get("mouseButton").cloned() {
        a["mouseButton"] = match button.as_str() {
            Some("l") => json!("left"),
            Some("r") => json!("right"),
            Some("m") => json!("middle"),
            _ => match button.as_u64() {
                Some(1) => json!("left"),
                Some(2) => json!("middle"),
                Some(3) => json!("right"),
                Some(4) => json!("back"),
                Some(5) => json!("forward"),
                _ => button,
            },
        };
    }
    if let Some(direction) = a["direction"].as_str() {
        a["direction"] = json!(match direction {
            "u" => "up",
            "d" => "down",
            "l" => "left",
            "r" => "right",
            other => other,
        });
    }
    if method.starts_with("cua_")
        && method != "cua_keypress"
        && let Some(keys) = a.get("keys").cloned()
    {
        a["modifiers"] = keys;
    }
    if let Some(keys) = a["keys"].as_array() {
        a["key"] = json!(
            keys.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("+")
        );
    }
    if method.starts_with("dom_cua_") && a.get("node_id").is_some() {
        a["selector"] = json!({"kind":"node","value":a["node_id"]});
    }
    if method == "tab_handle_js_dialog" {
        super::dialog::normalize_handle(&mut a)?;
    }
    let renamed = match method {
        "list_browsers" => "list",
        "create_tab" => "new_tab",
        "navigate_tab_url" => "navigate",
        "navigate_tab_reload" => "reload",
        "navigate_tab_back" => "back",
        "navigate_tab_forward" => "forward",
        "cua_click" => "click",
        "cua_double_click" => {
            a["clickCount"] = json!(2);
            "click"
        }
        "cua_drag" => "drag_path",
        "cua_keypress" => "press_key",
        "cua_move" => "move",
        "cua_scroll" => "scroll_pixels",
        "cua_type" => "type_text",
        "tab_screenshot" => "screenshot",
        "tab_ax_get_state" => "snapshot",
        "tab_cdp_call" => "cdp_call",
        "tab_cdp_events" => "cdp_events",
        "tab_get_js_dialog" => "dialog_get",
        "tab_handle_js_dialog" => "dialog_handle",
        "tab_clipboard_read" => "clipboard_read",
        "tab_clipboard_read_text" => "clipboard_read_text",
        "tab_clipboard_write" => "clipboard_write",
        "tab_clipboard_write_text" => "clipboard_write_text",
        "cua_download_media" => {
            a["selector"] = json!({"kind":"point","x":a["x"],"y":a["y"]});
            "locator_download_media"
        }
        "dom_cua_download_media" => "locator_download_media",
        "dom_cua_click" => "locator_click",
        "dom_cua_double_click" => {
            a["clickCount"] = json!(2);
            "locator_click"
        }
        "dom_cua_keypress" => "press_key",
        "dom_cua_type" => "type_text",
        "dom_cua_scroll" => "scroll_pixels",
        "playwright_evaluate" => "readonly_evaluate",
        "visible_dom" | "playwright_dom_snapshot" | "dom_cua_get_visible_dom" => "dom_snapshot",
        "browser_viewport_set" => "viewport_set",
        "browser_viewport_reset" => "viewport_reset",
        "browser_visibility_get" => "visibility_get",
        "browser_visibility_set" => "visibility_set",
        "playwright_wait_for_load_state" => "wait_for_load_state",
        "playwright_wait_for_timeout" => "wait_for_timeout",
        "playwright_wait_for_url" => "wait_for_url",
        "playwright_element_info" => "element_info",
        "playwright_element_screenshot" => "element_screenshot",
        "playwright_wait_for_download" => "wait_for_download",
        "playwright_download_path" => "download_path",
        "playwright_wait_for_file_chooser" => "wait_for_file_chooser",
        "playwright_file_chooser_set_files" => "file_chooser_set_files",
        "tab_dev_logs" => "dev_logs",
        "tab_content_export" => "content_export",
        "tab_content_export_gsuite" => "export_gsuite",
        "tab_content_export_youtube_transcript" => "export_youtube",
        "tab_page_assets_list" => "assets_list",
        "tab_page_assets_bundle" => "assets_bundle",
        "webmcp_list_tools" => "webmcp_list",
        "webmcp_invoke_tool" => "webmcp_invoke",
        s => s.strip_prefix("playwright_").unwrap_or(s),
    };
    if renamed == "locator_dblclick" {
        a["clickCount"] = json!(2);
        return Ok(("locator_click".into(), a));
    }
    Ok((renamed.into(), a))
}
pub(super) fn timeout(a: &Value, default: u64) -> Result<Duration> {
    let ms = a
        .get("timeoutMs")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error::invalid("timeoutMs must be an integer"))
        })
        .transpose()?
        .unwrap_or(default);
    if ms > 120000 {
        return Err(Error::invalid("timeoutMs exceeds 120000"));
    }
    Ok(Duration::from_millis(ms))
}
pub(super) fn eval_value(v: Value) -> Result<Value> {
    if v.get("exceptionDetails").is_some() {
        return Err(Error::action(v["exceptionDetails"].to_string()));
    }
    Ok(v["result"]["value"].clone())
}

impl Browser {
    pub(super) fn oopif_frame(
        &mut self,
        tab: &str,
        id: &str,
        parent: Option<&str>,
    ) -> Result<Value> {
        self.check_dialog_method(tab, "Page.getFrameTree")?;
        let key = (tab.to_owned(), id.to_owned());
        let session = if let Some(session) = self.surface.frame_sessions.get(&key) {
            session.clone()
        } else {
            let targets = self.call_for_tab(tab, "Target.getTargets", json!({}), None)?;
            if !targets["targetInfos"].as_array().is_some_and(|targets| {
                targets
                    .iter()
                    .any(|target| target["type"] == "iframe" && target["targetId"] == id)
            }) {
                return Err(Error::action("Frame detached during selection"));
            }
            let attached = self.call_for_tab(
                tab,
                "Target.attachToTarget",
                json!({"targetId":id,"flatten":true}),
                None,
            )?;
            let session = string(&attached, "sessionId")?.to_owned();
            self.surface
                .frame_sessions
                .insert(key.clone(), session.clone());
            if let Some(parent) = self.sessions.get(tab) {
                self.surface
                    .dialogs
                    .lock()
                    .unwrap()
                    .child(tab, &session, id, parent);
            }
            self.call("Runtime.enable", json!({}), Some(&session))?;
            self.chooser_session_attached(tab, &session)?;
            session
        };
        let result = self.call("Page.getFrameTree", json!({}), Some(&session))?;
        let mut frame = result["frameTree"]["frame"].clone();
        if frame["id"] != id {
            return Err(Error::action("Frame changed during selection"));
        }
        // A child target's root omits its embedding parent. The parent here is
        // obtained from an actual DOM frame owner, never inferred from its URL.
        if let Some(parent) = parent {
            self.surface
                .frame_parents
                .insert(key.clone(), parent.to_owned());
        }
        if let Some(parent) = self.surface.frame_parents.get(&key) {
            frame["parentId"] = json!(parent);
        }
        Ok(frame)
    }
    pub(super) fn page(&mut self, tab: &str) -> Result<()> {
        if self.surface.page_enabled.contains(tab) {
            return Ok(());
        }
        self.tab(tab, "Page.enable", json!({}))?;
        // Runtime.enable can wait on JavaScript execution while a modal prompt
        // is open. Page events suffice for dialog lifetime; console inspection
        // separately enables Runtime when it is requested.
        if !self.surface.dialogs.lock().unwrap().contains_key(tab) {
            self.tab(tab, "Runtime.enable", json!({}))?;
        }
        self.surface.page_enabled.insert(tab.into());
        Ok(())
    }
    pub(super) fn frame(&mut self, tab: &str, args: &Value) -> Result<Value> {
        let tree = self.tab(tab, "Page.getFrameTree", json!({}))?;
        fn find(tree: &Value, id: &str) -> Option<Value> {
            let f = &tree["frame"];
            if f["id"] == id || f["name"] == id || f["url"] == id {
                return Some(f.clone());
            }
            tree["childFrames"]
                .as_array()?
                .iter()
                .find_map(|c| find(c, id))
        }
        if let Some(frame) = args.get("frame").and_then(Value::as_str) {
            if let Some(found) = find(&tree["frameTree"], frame) {
                return Ok(found);
            }
            if self
                .surface
                .frame_sessions
                .contains_key(&(tab.to_owned(), frame.to_owned()))
            {
                return self.oopif_frame(tab, frame, None);
            }
            let mut current = tree["frameTree"]["frame"].clone();
            for selector in frame.split(" >> ") {
                let (session, context) = self.world(tab, &current)?;
                let result=self.call("Runtime.evaluate",json!({"contextId":context,"expression":format!("(()=>{{const e=document.querySelectorAll({});if(e.length!==1)throw new Error('Frame selector must match one element');return e[0];}})()",json!(selector)),"returnByValue":false}),Some(&session))?;
                if result.get("exceptionDetails").is_some() {
                    return Err(Error::action(result["exceptionDetails"].to_string()));
                }
                let object = string(&result["result"], "objectId")?.to_owned();
                let node = self.call(
                    "DOM.describeNode",
                    json!({"objectId":object,"depth":1}),
                    Some(&session),
                );
                if !node.as_ref().is_err_and(|e| e.code == -32006) {
                    let _ = self.call_maintenance(
                        "Runtime.releaseObject",
                        json!({"objectId":object}),
                        Some(&session),
                    );
                }
                let node = node?;
                let id = node["node"]["frameId"]
                    .as_str()
                    .or_else(|| node["node"]["contentDocument"]["frameId"].as_str())
                    .ok_or_else(|| Error::action("Locator is not an attached frame"))?;
                current = match find(&tree["frameTree"], id) {
                    Some(frame) => frame,
                    None => self.oopif_frame(tab, id, current["id"].as_str())?,
                };
            }
            Ok(current)
        } else {
            Ok(tree["frameTree"]["frame"].clone())
        }
    }
    pub(super) fn world(&mut self, tab: &str, frame: &Value) -> Result<(String, Value)> {
        let frame_id = string(frame, "id")?;
        self.check_dialog_method(tab, "Page.createIsolatedWorld")?;
        let root_session = self.session(tab)?;
        let session = self
            .surface
            .frame_sessions
            .get(&(tab.into(), frame_id.into()))
            .cloned()
            .unwrap_or(root_session.clone());
        let params =
            json!({"frameId":frame_id,"worldName":"skyre-dom","grantUniveralAccess":false});
        match self.call("Page.createIsolatedWorld", params.clone(), Some(&session)) {
            Ok(value) => Ok((session, value["executionContextId"].clone())),
            Err(original) if original.code != -32006 && session == root_session => {
                let targets = self.call_for_tab(tab, "Target.getTargets", json!({}), None)?;
                let exists = targets["targetInfos"].as_array().is_some_and(|v| {
                    v.iter()
                        .any(|t| t["type"] == "iframe" && t["targetId"] == frame_id)
                });
                if !exists {
                    return Err(original);
                }
                let attached = self.call_for_tab(
                    tab,
                    "Target.attachToTarget",
                    json!({"targetId":frame_id,"flatten":true}),
                    None,
                )?;
                let session = string(&attached, "sessionId")?.to_owned();
                self.surface
                    .frame_sessions
                    .insert((tab.into(), frame_id.into()), session.clone());
                self.call("Runtime.enable", json!({}), Some(&session))?;
                self.chooser_session_attached(tab, &session)?;
                let value = self.call("Page.createIsolatedWorld", params, Some(&session))?;
                Ok((session, value["executionContextId"].clone()))
            }
            Err(error) => Err(error),
        }
    }
    pub(super) fn evaluate(&mut self, tab: &str, expression: &str, args: &Value) -> Result<Value> {
        let mut params = json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"timeout":timeout(args,10000)?.as_millis()});
        // Locator timeout zero means a single state inspection. CDP's zero
        // evaluation timeout can abort immediately with "Internal error".
        if params["timeout"] == 0 {
            params.as_object_mut().unwrap().remove("timeout");
        }
        if args.get("frame").is_some() || args.get("isolated") == Some(&json!(true)) {
            let frame = self.frame(tab, args)?;
            let (session, context) = self.world(tab, &frame)?;
            params["contextId"] = context;
            return eval_value(self.call("Runtime.evaluate", params, Some(&session))?);
        }
        eval_value(self.tab(tab, "Runtime.evaluate", params)?)
    }
    pub(super) fn document_context(&mut self, tab: &str, args: &Value) -> Result<Value> {
        let frame = self.frame(tab, args)?;
        let loader = string(&frame, "loaderId")?;
        let mut selected = args.clone();
        selected["frame"] = frame["id"].clone();
        let value = self.evaluate(
            tab,
            "({url:location.href,timeOrigin:performance.timeOrigin})",
            &selected,
        )?;
        let token = json!([frame["id"], loader, value["timeOrigin"]]).to_string();
        Ok(
            json!({"browser":args["browser"],"tab":tab,"url":value["url"],"documentToken":token,"frameId":frame["id"]}),
        )
    }
    fn native_roles(&mut self, tab: &str, selector: &mut Value, args: &Value) -> Result<()> {
        if selector["kind"] == "role" {
            let frame = self.frame(tab, args)?;
            let (session, context) = self.world(tab, &frame)?;
            let role = string(selector, "value")?;
            let (query, document_object) = if args.get("frame").is_some() {
                // DOM.getDocument addresses a target's root, which is not the
                // selected same-process iframe. Query that world's document.
                let document = self.call(
                    "Runtime.evaluate",
                    json!({"contextId":context,"expression":"document","returnByValue":false}),
                    Some(&session),
                )?;
                let object = string(&document["result"], "objectId")?.to_owned();
                (json!({"objectId":object,"role":role}), Some(object))
            } else {
                let document = self.call("DOM.getDocument", json!({"depth":0}), Some(&session))?;
                (
                    json!({"nodeId":document["root"]["nodeId"],"role":role}),
                    None,
                )
            };
            let result = self.call("Accessibility.queryAXTree", query, Some(&session));
            if !result.as_ref().is_err_and(|e| e.code == -32006)
                && let Some(object) = document_object
            {
                let _ = self.call_maintenance(
                    "Runtime.releaseObject",
                    json!({"objectId":object}),
                    Some(&session),
                );
            }
            let result = result?;
            let nodes = result["nodes"]
                .as_array()
                .ok_or_else(|| Error::action("AX query returned no node list"))?;
            if nodes.len() > 10000 {
                return Err(Error::action("AX locator query exceeded 10000 nodes"));
            }
            let mut ids = vec![];
            for node in nodes {
                if node["role"]["value"] != role
                    || (selector["includeHidden"] != true && node["ignored"] == true)
                {
                    continue;
                }
                if let Some(name) = selector.get("name")
                    && !text_matches(
                        node["name"]["value"].as_str().unwrap_or(""),
                        name,
                        selector["exact"] == true,
                    )?
                {
                    continue;
                }
                let properties = node["properties"].as_array();
                if ["checked", "disabled", "expanded", "selected", "pressed"]
                    .iter()
                    .any(|name| {
                        selector.get(*name).is_some_and(|wanted| {
                            properties
                                .and_then(|v| v.iter().find(|p| p["name"] == *name))
                                .is_none_or(|p| p["value"]["value"] != *wanted)
                        })
                    })
                {
                    continue;
                }
                let Some(backend) = node["backendDOMNodeId"].as_u64() else {
                    continue;
                };
                let resolved = self.call(
                    "DOM.resolveNode",
                    json!({"backendNodeId":backend,"executionContextId":context}),
                    Some(&session),
                )?;
                let object = string(&resolved["object"], "objectId")?.to_owned();
                let result=self.call("Runtime.callFunctionOn",json!({"objectId":object,"functionDeclaration":"function(){const r=globalThis.__skyre_dom_registry??={next:0,nonce:globalThis.crypto?.randomUUID?.()??Math.random().toString(36).slice(2),ids:new WeakMap(),nodes:new Map()};let id=r.ids.get(this);if(!id){if(r.nodes.size>=20000)throw new Error('DOM reference limit exceeded');id=String(performance.timeOrigin)+':'+r.nonce+':'+(++r.next);r.ids.set(this,id);r.nodes.set(id,new WeakRef(this));}return id;}","returnByValue":true}),Some(&session));
                if !result.as_ref().is_err_and(|e| e.code == -32006) {
                    let _ = self.call_maintenance(
                        "Runtime.releaseObject",
                        json!({"objectId":object}),
                        Some(&session),
                    );
                }
                ids.push(eval_value(result?)?);
            }
            *selector = json!({"kind":"nodes","values":ids});
            return Ok(());
        }
        if let Some(object) = selector.as_object_mut() {
            for key in ["base", "left", "right", "has", "hasNot"] {
                if let Some(child) = object.get_mut(key) {
                    self.native_roles(tab, child, args)?;
                }
            }
            if let Some(steps) = object.get_mut("steps").and_then(Value::as_array_mut) {
                for child in steps {
                    self.native_roles(tab, child, args)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn dom(&mut self, tab: &str, operation: &str, args: &Value) -> Result<Value> {
        let mut payload = args.clone();
        payload["operation"] = json!(operation);
        if let Some(selector) = payload.get_mut("selector") {
            self.native_roles(tab, selector, args)?;
        }
        if payload.get("selector").is_none()
            && !["snapshot", "serialize", "point_info"].contains(&operation)
        {
            return Err(Error::invalid("selector is required"));
        }
        let mut options = args.clone();
        options["isolated"] = json!(true);
        if let Some(expected) = args["expectedDocumentToken"].as_str() {
            let token: Value = serde_json::from_str(expected)
                .map_err(|_| Error::invalid("Invalid document token"))?;
            let frame = self.frame(tab, args)?;
            if token[0] != frame["id"] || token[1] != frame["loaderId"] {
                return Err(Error::action("Document changed before action"));
            }
            payload["expectedTimeOrigin"] = token[2].clone();
        }
        let expression = format!("({})({})", include_str!("browser_dom.js"), payload);
        self.evaluate(tab, &expression, &options)
    }
    // Retry only errors known to occur before a DOM mutation. Transport failure,
    // strict ambiguity and reviewed identity/document changes are never replayed.
    fn locator_dom(&mut self, tab: &str, operation: &str, args: &Value) -> Result<Value> {
        let budget = timeout(args, 3000)?.min(Duration::from_secs(3));
        let deadline = Instant::now() + budget;
        loop {
            let mut options = args.clone();
            options["timeoutMs"] = json!(if budget.is_zero() {
                0
            } else {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .max(1) as u64
            });
            let result = self.dom(tab, operation, &options);
            let error = match result {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let retryable = error.code == -10005
                && [
                    "Strict locator expected one element; found 0",
                    "Element is disabled",
                    "Element is not visible",
                    "Element is not editable",
                    "Option not found",
                    "error:optionsnotfound",
                    "error:optionnotenabled",
                    "Element does not have a clickable bounding box",
                ]
                .iter()
                .any(|text| error.message.contains(text));
            if !retryable
                || args.get("expectedNodeIdentity").is_some()
                || Instant::now() >= deadline
            {
                return Err(error);
            }
            std::thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    fn locator(&mut self, tab: &str, operation: &str, args: &Value) -> Result<Value> {
        if ![
            "wait_for",
            "press",
            "press_sequentially",
            "type",
            "click",
            "set_checked",
            "screenshot",
            "download_media",
            "fill",
            "select_option",
            "count",
            "all_text_contents",
            "read_all",
            "text_content",
            "inner_text",
            "get_attribute",
            "is_enabled",
            "is_visible",
            "element_info",
            "inspect",
        ]
        .contains(&operation)
        {
            return Err(Error::unsupported(format!(
                "Unknown locator operation: {operation}"
            )));
        }
        if operation == "fill" || operation == "type" {
            string(args, "value")?;
        }
        if operation == "get_attribute" {
            string(args, "name")?;
        }

        if args.get("expectedNodeIdentity").is_some() && matches!(operation, "click" | "press") {
            return self.dom(
                tab,
                if operation == "click" {
                    "bound_click"
                } else {
                    "bound_press"
                },
                args,
            );
        }
        if operation == "wait_for" {
            let state = args["state"].as_str().unwrap_or("visible");
            if !["attached", "detached", "visible", "hidden"].contains(&state) {
                return Err(Error::invalid("Invalid locator wait state"));
            }
            let deadline = Instant::now() + timeout(args, 3000)?.min(Duration::from_secs(3));
            loop {
                let value = self.dom(tab, "state", args)?;
                let count = value["count"].as_u64().unwrap_or(0);
                let visible = value["visible"].as_bool().unwrap_or(false);
                if match state {
                    "attached" => count > 0,
                    "detached" => count == 0,
                    "visible" => visible,
                    _ => !visible,
                } {
                    return Ok(Value::Null);
                }
                if Instant::now() >= deadline {
                    return Err(Error::action("Locator wait timed out"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        if operation == "type" {
            self.locator_dom(tab, "focus", &{
                let mut options = args.clone();
                options["requireEditable"] = json!(true);
                options
            })?;
            return self.tab(
                tab,
                "Input.insertText",
                json!({"text":string(args,"value")?}),
            );
        }
        if operation == "press" || operation == "press_sequentially" {
            // Validate the entire input before changing focus.
            if operation == "press" {
                crate::keys::cdp_key(string(args, "key")?)?;
            } else {
                string(args, "text")?;
            }
            let mut focus = args.clone();
            if operation == "press_sequentially" {
                focus["requireEditable"] = json!(true);
                focus["captureFocusTarget"] = json!(true);
            }
            let focused_target = self.locator_dom(tab, "focus", &focus)?;
            if operation == "press" {
                return self.key(tab, string(args, "key")?);
            }
            focus["sequentialTarget"] = focused_target;
            let mut characters = string(args, "text")?.chars().peekable();
            while let Some(ch) = characters.next() {
                // Key handlers may replace the control or move focus. Never
                // refocus and continue sending the rest of the text elsewhere.
                self.dom(tab, "focus", &focus)?;
                if let Some(event) = crate::keys::cdp_text_character(ch) {
                    self.dispatch_key(tab, event)?;
                } else {
                    self.tab(tab, "Input.insertText", json!({"text":ch.to_string()}))?;
                }
                if characters.peek().is_some() {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            return Ok(Value::Null);
        }
        if operation == "click" || operation == "set_checked" {
            let button = args["mouseButton"].as_str().unwrap_or("left");
            if !["left", "right", "middle"].contains(&button) {
                return Err(Error::invalid("Invalid mouse button"));
            }
            let count = args
                .get("clickCount")
                .map(|v| {
                    v.as_u64()
                        .ok_or_else(|| Error::invalid("clickCount must be an integer"))
                })
                .transpose()?
                .unwrap_or(1);
            if !(1..=3).contains(&count) {
                return Err(Error::invalid("clickCount must be 1..3"));
            }
            if operation == "set_checked" && !args["checked"].is_boolean() {
                return Err(Error::invalid("checked must be boolean"));
            }
            let modifiers = modifiers(args)?;
            let mut last_error = None;
            let mut point = None;
            let alignments: &[&str] = if args["force"] == true {
                &["center"]
            } else {
                &["center", "end", "start"]
            };
            for alignment in alignments {
                let mut options = args.clone();
                options["scrollAlignment"] = json!(alignment);
                let prepared = self.locator_dom(tab, operation, &options).and_then(|p| {
                    if p["unchanged"] == true {
                        Ok(p)
                    } else {
                        self.frame_point(tab, &options, &p)
                    }
                });
                match prepared {
                    Ok(p) => {
                        point = Some(p);
                        break;
                    }
                    Err(error) if error.message.contains("is obscured") => last_error = Some(error),
                    Err(error) => return Err(error),
                }
            }
            let p = point.ok_or_else(|| {
                last_error.unwrap_or_else(|| Error::action("No actionable point"))
            })?;
            if p["unchanged"] == true {
                return Ok(Value::Null);
            }
            self.tab(
                tab,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":p["x"],"y":p["y"],"modifiers":modifiers}),
            )?;
            for click in 1..=count {
                self.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":p["x"],"y":p["y"],"button":button,"clickCount":click,"modifiers":modifiers}))?;
                self.tab_maintenance(tab,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":p["x"],"y":p["y"],"button":button,"clickCount":click,"modifiers":modifiers}))?;
            }
            if operation == "set_checked" {
                let observed = self.dom(tab, "element_info", args)?;
                if observed["checked"] != args["checked"] {
                    return Err(Error::action("Checkbox did not reach requested state"));
                }
            }
            return Ok(Value::Null);
        }
        if operation == "screenshot" {
            let n = self.dom(tab, "element_info", args)?;
            if n["visible"] != true {
                return Err(Error::action("Element is not visible"));
            }
            let mut options = args.clone();
            options["operation"] = json!("screenshot");
            let bounds = self.frame_point(tab, &options, &n["bounds"])?;
            let metrics = self.tab(tab, "Page.getLayoutMetrics", json!({}))?;
            let viewport = metrics
                .get("cssLayoutViewport")
                .or_else(|| metrics.get("layoutViewport"))
                .ok_or_else(|| Error::action("Screenshot layout viewport unavailable"))?;
            let coordinate = |key: &str, offset: &str| -> Result<f64> {
                Ok(bounds[key]
                    .as_f64()
                    .ok_or_else(|| Error::action("Screenshot bounds unavailable"))?
                    + viewport[offset]
                        .as_f64()
                        .ok_or_else(|| Error::action("Screenshot scroll offset unavailable"))?)
            };
            let r=self.tab(tab,"Page.captureScreenshot",json!({"format":"png","clip":{"x":coordinate("x","pageX")?,"y":coordinate("y","pageY")?,"width":bounds["width"],"height":bounds["height"],"scale":1},"captureBeyondViewport":true}))?;
            return Ok(json!({"mime_type":"image/png","data":r["data"]}));
        }
        if operation == "download_media" {
            let watch = self.download_command("download_arm", tab, args)?;
            if let Err(error) = self.dom(tab, operation, args) {
                let _ = self.download_command("download_cancel", tab, &watch);
                return Err(error);
            }
            return Ok(watch);
        }
        if matches!(
            operation,
            "fill" | "select_option" | "text_content" | "inner_text" | "get_attribute"
        ) {
            self.locator_dom(tab, operation, args)
        } else {
            self.dom(tab, operation, args)
        }
    }
    pub(super) fn key(&mut self, tab: &str, key: &str) -> Result<Value> {
        if key.eq_ignore_ascii_case("ctrl+v")
            || key.eq_ignore_ascii_case("control+v")
            || key.eq_ignore_ascii_case("meta+v")
        {
            return self.virtual_paste(tab);
        }
        self.dispatch_key(tab, crate::keys::cdp_key(key)?)
    }
    fn dispatch_key(&mut self, tab: &str, mut down: Value) -> Result<Value> {
        let mut up = down.clone();
        down["type"] = json!("keyDown");
        up["type"] = json!("keyUp");
        for key in ["text", "unmodifiedText", "commands", "isKeypad"] {
            up.as_object_mut().unwrap().remove(key);
        }
        let mut attempted = false;
        let result = self.tab_attempt(tab, "Input.dispatchKeyEvent", down, &mut attempted);
        if !attempted {
            return result;
        }
        if result.as_ref().is_err_and(|e| e.code == -32006) {
            return result;
        }
        let release = self.tab_maintenance(tab, "Input.dispatchKeyEvent", up);
        result?;
        release
    }
    fn ensure_clipboard(&mut self, tab: &str) -> Result<()> {
        if !self.surface.clipboard_scripts.contains_key(tab) {
            self.check_dialog_method(tab, "Runtime.addBinding")?;
        }
        let session = self.session(tab)?;
        if self.surface.clipboard_scripts.contains_key(tab) {
            return Ok(());
        }
        let store = self.surface.clipboard.clone();
        let client = self.client()?;
        client.clipboard = Some(store);
        client.clipboard_sessions.insert(session.clone());
        self.tab(
            tab,
            "Runtime.addBinding",
            json!({"name":"__skyre_clipboard"}),
        )?;
        let script = match self.tab(
            tab,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":clipboard::INSTALL,"runImmediately":true}),
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = self.tab_maintenance(
                    tab,
                    "Runtime.removeBinding",
                    json!({"name":"__skyre_clipboard"}),
                );
                return Err(e);
            }
        };
        let script_id = string(&script, "identifier")?.to_owned();
        let installed = self.evaluate(tab, clipboard::INSTALL, &json!({}));
        if installed.as_ref().ok() != Some(&json!(true)) {
            let _ = self.tab_maintenance(
                tab,
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier":script_id}),
            );
            let _ = self.tab_maintenance(
                tab,
                "Runtime.removeBinding",
                json!({"name":"__skyre_clipboard"}),
            );
            return Err(Error::action("Virtual clipboard installation failed"));
        }
        self.surface.clipboard_scripts.insert(tab.into(), script_id);
        Ok(())
    }
    /// Native host teardown owns these exact installed resources. The public
    /// clipboard_cleanup command checks the ordinary gate before entering here.
    pub(super) fn cleanup_clipboard(&mut self, tab: &str) -> Result<()> {
        if let Some(id) = self.surface.clipboard_scripts.remove(tab) {
            let result = self.tab_maintenance(
                tab,
                "Runtime.evaluate",
                json!({"expression":"globalThis.__skyre_clipboard_state?.cleanup()",
                    "returnByValue":true,"awaitPromise":true,"timeout":10000}),
            );
            let remove = self.tab_maintenance(
                tab,
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier":id}),
            );
            let binding = self.tab_maintenance(
                tab,
                "Runtime.removeBinding",
                json!({"name":"__skyre_clipboard"}),
            );
            eval_value(result?)?;
            remove?;
            binding?;
        }
        Ok(())
    }
    pub(super) fn pump(&mut self, tab: &str) -> Result<()> {
        self.tab(
            tab,
            "Runtime.evaluate",
            json!({"expression":"void 0","returnByValue":true}),
        )?;
        Ok(())
    }
    pub(super) fn surface_command(&mut self, method: &str, args: &Value) -> Result<Option<Value>> {
        let supported = method.starts_with("locator_")
            || [
                "drag_path",
                "readonly_evaluate",
                "scroll_pixels",
                "document_context",
                "frames",
                "get_tab",
                "selected_tab",
                "mark_tab",
                "back",
                "forward",
                "move",
                "dialog_get",
                "dialog_handle",
                "clipboard_read",
                "clipboard_read_text",
                "clipboard_write",
                "clipboard_write_text",
                "clipboard_cleanup",
                "viewport_set",
                "viewport_reset",
                "visibility_get",
                "visibility_set",
                "dom_snapshot",
                "cdp_call",
                "cdp_events",
                "dev_logs",
                "wait_for_load_state",
                "wait_for_timeout",
                "wait_for_url",
                "wait_for_download",
                "downloads_enable",
                "download_arm",
                "download_poll",
                "download_cancel",
                "downloads_disable",
                "download_path",
                "wait_for_file_chooser",
                "file_chooser_enable",
                "file_chooser_poll",
                "file_chooser_set_files",
                "content_export",
                "export_gsuite",
                "export_youtube",
                "assets_list",
                "assets_bundle",
                "webmcp_list",
                "webmcp_invoke",
                "tabs_content",
            ]
            .contains(&method);
        if !supported {
            return Ok(None);
        }
        if [
            "download_path",
            "downloads_enable",
            "downloads_disable",
            "download_arm",
            "download_poll",
            "download_cancel",
            "wait_for_download",
        ]
        .contains(&method)
        {
            return self
                .download_command(method, string(args, "tab")?, args)
                .map(Some);
        }
        if method == "selected_tab" {
            return Ok(Some(json!(self.surface.selected)));
        }
        if method == "tabs_content" {
            let urls = args["urls"]
                .as_array()
                .filter(|v| v.len() <= 1000)
                .ok_or_else(|| {
                    Error::invalid("tabs_content requires at most 1000 explicit URLs")
                })?;
            let urls = urls
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| Error::invalid("Content URL must be a string"))
                })
                .collect::<Result<Vec<_>>>()?;
            for url in &urls {
                let parsed =
                    url::Url::parse(url).map_err(|_| Error::invalid("Invalid content URL"))?;
                if !["https", "http", "file", "data", "about"].contains(&parsed.scheme()) {
                    return Err(Error::invalid("Unsupported content URL scheme"));
                }
            }
            let content_type = args["contentType"].as_str().unwrap_or("text");
            if !["text", "html", "domSnapshot"].contains(&content_type) {
                return Err(Error::invalid("Unsupported contentType"));
            }
            let mut out = vec![];
            for url in urls {
                let target = self.call(
                    "Target.createTarget",
                    json!({"url":"about:blank","background":true}),
                    None,
                )?;
                let tab = string(&target, "targetId")?.to_owned();
                let result = (|| {
                    self.page(&tab)?;
                    self.navigate_url(&tab, url)?;
                    let deadline = Instant::now() + timeout(args, 10000)?;
                    loop {
                        let value=self.evaluate(&tab,"({url:location.href,title:document.title,state:document.readyState,text:(document.body?.innerText??'').slice(0,500000),truncated:(document.body?.innerText??'').length>500000})",&json!({}))?;
                        if (value["state"] == "complete" || value["state"] == "interactive")
                            && (url == "about:blank" || value["url"] != "about:blank")
                        {
                            let content = match content_type {
                                "html" => self.evaluate(
                                    &tab,
                                    "document.documentElement.outerHTML",
                                    &json!({}),
                                )?,
                                "domSnapshot" => {
                                    json!(self.dom(&tab, "snapshot", &json!({}))?.to_string())
                                }
                                _ => value["text"].clone(),
                            };
                            return Ok(json!({"url":url,"title":value["title"],"content":content}));
                        }
                        if Instant::now() >= deadline {
                            return Err(Error::action("Temporary content tab load timed out"));
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                })();
                let closed = self.call("Target.closeTarget", json!({"targetId":tab}), None);
                self.sessions.remove(&tab);
                self.surface.remove_tab(&tab);
                match (result, closed) {
                    (Ok(value), Ok(_)) => out.push(value),
                    (Err(_), Ok(_)) => out.push(json!({"url":url,"title":null,"content":null})),
                    (Err(error), Err(_)) => return Err(error),
                    (Ok(_), Err(error)) => {
                        return Err(Error::action(format!(
                            "Content extracted but temporary tab cleanup failed: {}",
                            error.message
                        )));
                    }
                }
            }
            return Ok(Some(json!(out)));
        }
        let tab = string(args, "tab")?;
        if let Some(operation) = method.strip_prefix("locator_") {
            return self.locator(tab, operation, args).map(Some);
        }
        let result = match method {
            "drag_path" => {
                let points = args["path"]
                    .as_array()
                    .filter(|v| v.len() >= 2 && v.len() <= 1000)
                    .ok_or_else(|| Error::invalid("Drag path requires 1..1000 points"))?;
                let points = points
                    .iter()
                    .map(|p| crate::engine::point(&json!({"point":p}), "point", "x", "y"))
                    .collect::<Result<Vec<_>>>()?;
                let modifiers = modifiers(args)?;
                let first = points[0];
                self.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":first[0],"y":first[1],"button":"left","buttons":1,"clickCount":1,"modifiers":modifiers}))?;
                let mut last = first;
                let mut result = Ok(Value::Null);
                for point in points.into_iter().skip(1) {
                    last = point;
                    result=self.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":point[0],"y":point[1],"buttons":1,"modifiers":modifiers}));
                    if result.is_err() {
                        break;
                    }
                }
                if result.as_ref().is_err_and(|e| e.code == -32006) {
                    return result.map(Some);
                }
                let release=self.tab_maintenance(tab,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":last[0],"y":last[1],"button":"left","buttons":0,"clickCount":1,"modifiers":modifiers}));
                result?;
                release?
            }
            "readonly_evaluate" => {
                let mut capture = args.clone();
                capture.as_object_mut().unwrap().remove("expression");
                capture.as_object_mut().unwrap().remove("arg");
                let snapshot = self.dom(tab, "serialize", &capture)?;
                readonly_evaluate(
                    &snapshot,
                    string(args, "expression")?,
                    args.get("arg").cloned(),
                    args.get("selector"),
                    args["all"].as_bool().unwrap_or(false),
                )?
            }
            "document_context" => self.document_context(tab, args)?,
            "frames" => self.tab(tab, "Page.getFrameTree", json!({}))?,
            "get_tab" => {
                self.page(tab)?;
                self.call_for_tab(tab, "Target.getTargetInfo", json!({"targetId":tab}), None)?["targetInfo"]
                    .clone()
            }
            "mark_tab" => {
                if self.extension {
                    self.call(
                        "Skyre.markTab",
                        json!({"tab":tab,"status":args["status"]}),
                        None,
                    )?;
                }
                self.surface.marked.insert(tab.into());
                self.surface.selected = Some(tab.into());
                json!({"tab":tab})
            }
            "scroll_pixels" => {
                let x = args["scroll_x"]
                    .as_f64()
                    .ok_or_else(|| Error::invalid("scroll_x must be a number"))?;
                let y = args["scroll_y"]
                    .as_f64()
                    .ok_or_else(|| Error::invalid("scroll_y must be a number"))?;
                let p = if args.get("selector").is_some() {
                    let v = self.dom(tab, "point", args)?;
                    [
                        v["x"]
                            .as_f64()
                            .ok_or_else(|| Error::action("Element coordinates unavailable"))?,
                        v["y"]
                            .as_f64()
                            .ok_or_else(|| Error::action("Element coordinates unavailable"))?,
                    ]
                } else if args.get("point").is_some() || args.get("x").is_some() {
                    crate::engine::point(args, "point", "x", "y")?
                } else {
                    let size = self.evaluate(tab, "({x:innerWidth/2,y:innerHeight/2})", args)?;
                    [
                        size["x"]
                            .as_f64()
                            .ok_or_else(|| Error::action("Viewport unavailable"))?,
                        size["y"]
                            .as_f64()
                            .ok_or_else(|| Error::action("Viewport unavailable"))?,
                    ]
                };
                self.tab(
                    tab,
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseWheel","x":p[0],"y":p[1],"deltaX":x,"deltaY":y,"modifiers":modifiers(args)?}),
                )?
            }
            "move" => {
                let p = crate::engine::point(args, "point", "x", "y")?;
                self.tab(
                    tab,
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":p[0],"y":p[1],"modifiers":modifiers(args)?}),
                )?
            }
            "back" | "forward" => {
                let history = self.tab(tab, "Page.getNavigationHistory", json!({}))?;
                let current = history["currentIndex"]
                    .as_i64()
                    .ok_or_else(|| Error::action("Navigation history unavailable"))?;
                let index = current + if method == "back" { -1 } else { 1 };
                let entry = history["entries"]
                    .as_array()
                    .and_then(|v| usize::try_from(index).ok().and_then(|i| v.get(i)))
                    .ok_or_else(|| Error::action("No navigation entry in requested direction"))?;
                self.tab(
                    tab,
                    "Page.navigateToHistoryEntry",
                    json!({"entryId":entry["id"]}),
                )?
            }
            "dom_snapshot" => self.dom(tab, "snapshot", args)?,
            "dialog_get" => {
                self.poll_events()?;
                if !self.surface.dialogs.lock().unwrap().contains_key(tab) {
                    self.page(tab)?;
                    self.poll_events()?;
                }
                json!(
                    self.surface
                        .dialogs
                        .lock()
                        .unwrap()
                        .get(tab)
                        .map(|dialog| json!({"id": dialog["id"], "type": dialog["type"]}))
                )
            }
            "dialog_handle" => {
                let requested_id = super::dialog::requested_id(args)?;
                self.poll_events()?;
                let dialog = self
                    .surface
                    .dialogs
                    .lock()
                    .unwrap()
                    .get(tab)
                    .cloned()
                    .ok_or_else(|| Error::action("JavaScript dialog is no longer active"))?;
                let id = dialog["id"].clone();
                if id != requested_id {
                    return Err(Error::action("JavaScript dialog is no longer active"));
                }
                if !self.surface.dialogs.lock().unwrap().dispatch_allowed(tab) {
                    return Err(Error::action(
                        "JavaScript dialog session is no longer active",
                    ));
                }
                let params = super::dialog::action_params(&dialog, args)?;
                let session = self
                    .surface
                    .dialogs
                    .lock()
                    .unwrap()
                    .session(tab)
                    .map(str::to_owned);
                let watch =
                    super::dialog::Watch::start(self.surface.dialogs.clone(), tab, session.clone());
                let deadline = Instant::now() + Duration::from_secs(60);
                let r = if let Some(session) = session {
                    self.call("Page.handleJavaScriptDialog", params, Some(&session))?
                } else {
                    self.tab(tab, "Page.handleJavaScriptDialog", params)?
                };
                while !watch.closed() {
                    if Instant::now() >= deadline {
                        return Err(Error::action(
                            "Timed out waiting for JavaScript dialog to close.",
                        ));
                    }
                    self.poll_events()?;
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Events received during dispatch can already have installed a
                // replacement. Completing this handler retires only its own ID.
                self.surface.dialogs.lock().unwrap().delete_id(tab, &id);
                r
            }
            "clipboard_read"
            | "clipboard_read_text"
            | "clipboard_write"
            | "clipboard_write_text" => {
                let write = if method == "clipboard_write" {
                    Some(Clipboard::validate_public(args.get("items"))?)
                } else if method == "clipboard_write_text" {
                    Some(Clipboard::validate_text(args.get("text"))?)
                } else {
                    None
                };
                self.ensure_clipboard(tab)?;
                let mut store = self
                    .surface
                    .clipboard
                    .lock()
                    .map_err(|_| Error::action("Clipboard store unavailable"))?;
                if let Some(items) = write {
                    store.items = items;
                    Value::Null
                } else if method == "clipboard_read_text" {
                    json!(store.text())
                } else {
                    json!(store.items)
                }
            }
            "clipboard_cleanup" => {
                self.check_dialog_method(tab, "Runtime.evaluate")?;
                self.cleanup_clipboard(tab)?;
                Value::Null
            }
            "viewport_set" => {
                let width = args["width"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 16384)
                    .ok_or_else(|| Error::invalid("Invalid viewport width"))?;
                let height = args["height"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 16384)
                    .ok_or_else(|| Error::invalid("Invalid viewport height"))?;
                self.tab(tab,"Emulation.setDeviceMetricsOverride",json!({"width":width,"height":height,"deviceScaleFactor":args["deviceScaleFactor"].as_f64().unwrap_or(1.),"mobile":false}))?
            }
            "viewport_reset" => self.tab(tab, "Emulation.clearDeviceMetricsOverride", json!({}))?,
            "visibility_get" | "visibility_set" => {
                let window = self.call_for_tab(
                    tab,
                    "Browser.getWindowForTarget",
                    json!({"targetId":tab}),
                    None,
                )?;
                if method == "visibility_get" {
                    json!({"visible":window["bounds"]["windowState"]!="minimized"})
                } else {
                    let visible = args["visible"]
                        .as_bool()
                        .ok_or_else(|| Error::invalid("visible must be boolean"))?;
                    self.call_for_tab(tab, "Browser.setWindowBounds",json!({"windowId":window["windowId"],"bounds":{"windowState":if visible{"normal"}else{"minimized"}}}),None)?
                }
            }
            "cdp_call" => {
                let method = string(args, "method")?;
                if method.starts_with("Skyre.download") {
                    return Err(Error::new(
                        -32003,
                        "Download ownership commands are internal provider APIs",
                    ));
                }
                if method.starts_with("Skyre.host") {
                    return Err(Error::new(
                        -32003,
                        "Trusted host lifecycle is not a model CDP API",
                    ));
                }
                let capture = (args.get("target").is_none_or(Value::is_null)
                    && ["Page.startScreencast", "Page.stopScreencast"].contains(&method))
                .then(|| self.surface.screencast.acquire(tab));
                self.check_dialog_method(tab, method)?;
                let session = if let Some(target) = args.get("target").filter(|v| !v.is_null()) {
                    match (target["sessionId"].as_str(), target["targetId"].as_str()) {
                        (Some(session), None) => session.to_owned(),
                        (None, Some(target)) => self.command_session(target, method)?,
                        _ => {
                            return Err(Error::invalid(
                                "CDP target requires exactly one sessionId or targetId",
                            ));
                        }
                    }
                } else {
                    self.command_session(tab, method)?
                };
                if [
                    "Fetch.enable",
                    "Fetch.disable",
                    "Fetch.continueRequest",
                    "Fetch.continueResponse",
                    "Fetch.failRequest",
                    "Fetch.fulfillRequest",
                ]
                .contains(&method)
                    && self.surface_download_interception_owned(&session)
                {
                    return Err(Error::action(
                        "Raw Fetch command conflicts with the owned document-response interceptor",
                    ));
                }
                let result = self.call(
                    string(args, "method")?,
                    args.get("params")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or(json!({})),
                    Some(&session),
                );
                if result.is_ok()
                    && let Some(capture) = &capture
                {
                    capture.raw_succeeded(method);
                }
                result?
            }
            "cdp_events" => self.raw_events_start(args, None)?,
            "dev_logs" => {
                self.page(tab)?;
                self.tab(tab, "Runtime.enable", json!({}))?;
                self.tab(tab, "Log.enable", json!({}))?;
                self.pump(tab)?;
                let session = if let Some(target) = args.get("target").filter(|v| !v.is_null()) {
                    match (target["sessionId"].as_str(), target["targetId"].as_str()) {
                        (Some(session), None) => session.to_owned(),
                        (None, Some(target)) => self.session(target)?,
                        _ => {
                            return Err(Error::invalid(
                                "CDP target requires exactly one sessionId or targetId",
                            ));
                        }
                    }
                } else {
                    self.session(tab)?
                };
                let after = args["after_sequence"].as_u64().unwrap_or(0);
                let limit = args["limit"].as_u64().unwrap_or(100).min(1024) as usize;
                let logs = self
                    .surface
                    .events
                    .iter()
                    .filter(|(seq, event)| {
                        *seq > after
                            && event["sessionId"] == session
                            && args["methods"]
                                .as_array()
                                .is_none_or(|methods| methods.contains(&event["method"]))
                    })
                    .filter_map(|(_, event)| super::contract::log_entry(event))
                    .filter(|entry| {
                        args["levels"]
                            .as_array()
                            .is_none_or(|levels| levels.contains(&entry["level"]))
                            && args["filter"].as_str().is_none_or(|filter| {
                                entry["message"].as_str().unwrap_or("").contains(filter)
                            })
                    })
                    .take(limit)
                    .collect::<Vec<_>>();
                json!(logs)
            }
            "wait_for_timeout" => {
                std::thread::sleep(timeout(args, 0)?);
                Value::Null
            }
            "wait_for_load_state" | "wait_for_url" => {
                let deadline = Instant::now() + timeout(args, 3000)?;
                let state = args["state"].as_str().unwrap_or("load");
                if method == "wait_for_load_state" && !["load", "domcontentloaded"].contains(&state)
                {
                    return Err(Error::unsupported(
                        "Only load and domcontentloaded are available; networkidle is not inferred",
                    ));
                }
                loop {
                    let value = self.evaluate(
                        tab,
                        "({url:location.href,state:document.readyState})",
                        args,
                    )?;
                    let matches = if method == "wait_for_url" {
                        url_matches(string(args, "url")?, value["url"].as_str().unwrap_or(""))
                    } else {
                        value["state"] == "complete"
                            || (state == "domcontentloaded" && value["state"] == "interactive")
                    };
                    if matches {
                        break Value::Null;
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::action("Browser wait timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            "file_chooser_enable"
            | "file_chooser_poll"
            | "wait_for_file_chooser"
            | "file_chooser_set_files" => self.chooser_command(method, tab, args)?,
            "content_export" => {
                let format = args["format"].as_str().unwrap_or("html");
                let title = self.evaluate(tab, "document.title", args)?;
                let bytes = match format {
                    "html" => self
                        .evaluate(tab, "document.documentElement.outerHTML", args)?
                        .as_str()
                        .unwrap_or("")
                        .as_bytes()
                        .to_vec(),
                    "txt" => self
                        .evaluate(tab, "document.body?.innerText??''", args)?
                        .as_str()
                        .unwrap_or("")
                        .as_bytes()
                        .to_vec(),
                    "pdf" => {
                        let v =
                            self.tab(tab, "Page.printToPDF", json!({"printBackground":true}))?;
                        base64::engine::general_purpose::STANDARD
                            .decode(string(&v, "data")?)
                            .map_err(|_| Error::action("Invalid PDF data"))?
                    }
                    "mhtml" => {
                        self.tab(tab, "Page.captureSnapshot", json!({"format":"mhtml"}))?["data"]
                            .as_str()
                            .unwrap_or("")
                            .as_bytes()
                            .to_vec()
                    }
                    _ => {
                        return Err(Error::unsupported(
                            "Content export supports html, txt, pdf, mhtml",
                        ));
                    }
                };
                self.surface
                    .artifacts
                    .export(title.as_str().unwrap_or("Export"), format, &bytes)?
            }
            "export_gsuite" => self.export_gsuite(tab, args)?,
            "export_youtube" => self.export_youtube(tab, args)?,
            "assets_list" | "assets_bundle" => {
                let tree = self.tab(tab, "Page.getResourceTree", json!({}))?;
                if method == "assets_list" {
                    tree
                } else {
                    let frame = string(&tree["frameTree"]["frame"], "id")?;
                    let resources = tree["frameTree"]["resources"]
                        .as_array()
                        .ok_or_else(|| Error::action("Resource list missing"))?;
                    let mut out = vec![];
                    let mut total = 0;
                    for r in resources.iter().take(1000) {
                        let content = self.tab(
                            tab,
                            "Page.getResourceContent",
                            json!({"frameId":frame,"url":r["url"]}),
                        )?;
                        total += content.to_string().len();
                        if total > 32 * 1024 * 1024 {
                            return Err(Error::action("Asset bundle exceeds 32 MiB"));
                        }
                        out.push(json!({"resource":r,"content":content}));
                    }
                    self.surface.artifacts.export(
                        "page-assets",
                        "json",
                        &serde_json::to_vec(&out)?,
                    )?
                }
            }
            "webmcp_list" | "webmcp_invoke" => self.webmcp(tab, method, args)?,
            _ => {
                return Err(Error::unsupported(format!(
                    "Browser method unavailable: {method}"
                )));
            }
        };
        Ok(Some(result))
    }
    fn export_youtube(&mut self, tab: &str, args: &Value) -> Result<Value> {
        let context = self.document_context(tab, args)?;
        let url = url::Url::parse(string(&context, "url")?)
            .map_err(|_| Error::action("Invalid video page URL"))?;
        let video = url
            .query_pairs()
            .find(|(k, _)| k == "v")
            .map(|(_, v)| v.into_owned())
            .filter(|v| {
                v.len() == 11
                    && v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
            .ok_or_else(|| Error::action("YouTube video ID missing"))?;
        if url.scheme() != "https"
            || ![Some("youtube.com"), Some("www.youtube.com")].contains(&url.host_str())
            || url.path() != "/watch"
        {
            return Err(Error::action(
                "Transcript export requires an HTTPS YouTube watch page",
            ));
        }
        let mut options = args.clone();
        options["timeoutMs"] = json!(15000);
        let capture = self.evaluate(
            tab,
            &format!("({})({})", include_str!("browser_youtube.js"), json!(video)),
            &options,
        )?;
        let Some(text) = transcript(&capture, &video)? else {
            return Ok(Value::Null);
        };
        let after = self.document_context(tab, args)?;
        if after["documentToken"] != context["documentToken"] || after["url"] != context["url"] {
            return Err(Error::action("Video changed during transcript export"));
        }
        self.surface
            .artifacts
            .export(&format!("youtube-{video}"), "txt", text.as_bytes())
    }
    fn export_gsuite(&mut self, tab: &str, args: &Value) -> Result<Value> {
        let format = string(args, "format")?;
        let context = self.document_context(tab, args)?;
        let url = url::Url::parse(string(&context, "url")?)
            .map_err(|_| Error::action("Invalid page URL"))?;
        if url.host_str() != Some("docs.google.com") || url.port().is_some() {
            return Err(Error::action("Not a Workspace document"));
        }
        let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        let kind = parts.first().copied().unwrap_or("");
        let formats: &[&str] = match kind {
            "document" => &["pdf", "md", "docx"],
            "spreadsheets" => &["pdf", "xlsx", "csv"],
            "presentation" => &["pdf", "pptx"],
            _ => return Err(Error::action("Unsupported Workspace document type")),
        };
        if !formats.contains(&format) {
            return Err(Error::invalid("Unsupported document export format"));
        }
        let id = parts
            .windows(2)
            .find(|p| p[0] == "d")
            .map(|p| p[1])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::action("Workspace document ID missing"))?;
        if parts.last() == Some(&"pub") {
            return Err(Error::action("Published document export is unsupported"));
        }
        let mut export = url::Url::parse(&format!("https://docs.google.com/{kind}/d/{id}/export"))
            .map_err(|_| Error::action("Invalid export URL"))?;
        export.query_pairs_mut().append_pair("format", format);
        for (k, v) in url.query_pairs() {
            if k == "tab" {
                export.query_pairs_mut().append_pair(&k, &v);
            }
        }
        export.set_fragment(url.fragment());
        let expression = format!(
            "(async()=>{{const response=await fetch({},{{signal:AbortSignal.timeout(10000)}});if(!response.ok)throw new Error('Export HTTP '+response.status);const reader=response.body.getReader();let length=0,chunks=[];for(;;){{const {{done,value}}=await reader.read();if(done)break;length+=value.length;if(length>32*1024*1024){{await reader.cancel();throw new Error('Export exceeds 32 MiB');}}chunks.push(value);}}let s='';for(const c of chunks)for(let i=0;i<c.length;i+=32768)s+=String.fromCharCode(...c.subarray(i,i+32768));return {{title:document.title,data:btoa(s),url:location.href}};}})()",
            json!(export.as_str())
        );
        let result = self.evaluate(tab, &expression, args)?;
        if result["url"] != context["url"] {
            return Err(Error::action("Document changed during export"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(string(&result, "data")?)
            .map_err(|_| Error::action("Invalid export data"))?;
        self.surface.artifacts.export(
            result["title"].as_str().unwrap_or("Workspace"),
            format,
            &bytes,
        )
    }
}
fn modifiers(args: &Value) -> Result<u64> {
    let Some(values) = args.get("modifiers") else {
        return Ok(0);
    };
    let mut mask = 0;
    for value in values
        .as_array()
        .ok_or_else(|| Error::invalid("modifiers must be an array"))?
    {
        mask |= match value.as_str() {
            Some("Alt" | "ALT") => 1,
            Some("Control" | "Ctrl" | "CTRL") => 2,
            Some("Meta" | "META" | "CMD") => 4,
            Some("Shift" | "SHIFT") => 8,
            _ => return Err(Error::invalid("Invalid modifier")),
        };
    }
    Ok(mask)
}
pub(super) fn url_matches(pattern: &str, value: &str) -> bool {
    // Original nG: one star cannot cross '/', two stars can. Every other
    // character is literal, including regex punctuation and Unicode.
    let mut expression = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                expression.push_str(r"[^\n\r\x{2028}\x{2029}]*");
            } else {
                expression.push_str("[^/]*");
            }
        } else {
            expression.push_str(&regex::escape(&ch.to_string()));
        }
    }
    expression.push('$');
    regex::Regex::new(&expression).is_ok_and(|regex| regex.is_match(value))
}

fn readonly_evaluate(
    snapshot: &Value,
    expression: &str,
    arg: Option<Value>,
    selector: Option<&Value>,
    all: bool,
) -> Result<Value> {
    let runtime = rquickjs::Runtime::new().map_err(|e| Error::action(e.to_string()))?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + Duration::from_secs(5);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = rquickjs::Context::full(&runtime).map_err(|e| Error::action(e.to_string()))?;
    let captured = super::snapshot::Snapshot::new(snapshot).map_err(Error::action)?;
    context.with(|ctx| {
        captured
            .install(&ctx, deadline)
            .map_err(|e| Error::action(e.to_string()))?;
        let code = format!(
            "({})({},({}),{},{},{})",
            include_str!("browser_readonly.js"),
            snapshot,
            expression,
            arg.map(|value| value.to_string())
                .unwrap_or("undefined".into()),
            selector.unwrap_or(&Value::Null),
            all
        );
        let result = ctx
            .eval::<rquickjs::Promise, _>(code)
            .and_then(|p| p.finish::<rquickjs::Value>());
        let value = result.map_err(|e| {
            let caught = ctx.catch();
            let message = caught
                .as_object()
                .and_then(|o| o.get::<_, String>("message").ok())
                .unwrap_or_else(|| e.to_string());
            Error::action(format!("Read-only evaluation: {message}"))
        })?;
        let encoded = ctx
            .json_stringify(value)
            .map_err(|e| Error::action(e.to_string()))?
            .map(|s| s.to_string())
            .transpose()
            .map_err(|e| Error::action(e.to_string()))?
            .unwrap_or("null".into());
        Ok(serde_json::from_str(&encoded)?)
    })
}

fn text_matches(value: &str, wanted: &Value, exact: bool) -> Result<bool> {
    if let Some(pattern) = wanted["regex"].as_str() {
        let rt = rquickjs::Runtime::new().map_err(|e| Error::action(e.to_string()))?;
        rt.set_memory_limit(8 * 1024 * 1024);
        let deadline = Instant::now() + Duration::from_millis(100);
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let cx = rquickjs::Context::full(&rt).map_err(|e| Error::action(e.to_string()))?;
        return cx.with(|ctx| {
            ctx.eval::<bool, _>(format!(
                "new RegExp({},{}).test({})",
                json!(pattern),
                json!(wanted["flags"].as_str().unwrap_or("")),
                json!(value)
            ))
            .map_err(|e| Error::invalid(format!("Invalid or over-budget name matcher: {e}")))
        });
    }
    let wanted = wanted
        .as_str()
        .ok_or_else(|| Error::invalid("Name matcher must be string or regex descriptor"))?;
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let wanted = wanted.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(if exact {
        value == wanted
    } else {
        value.to_lowercase().contains(&wanted.to_lowercase())
    })
}

fn transcript(capture: &Value, video: &str) -> Result<Option<String>> {
    if capture.is_null() {
        return Ok(None);
    }
    if capture["videoId"] != video
        || capture["language"]
            .as_str()
            .is_none_or(|s| s.chars().count() > 100)
        || capture["captionKind"]
            .as_str()
            .is_none_or(|s| s.chars().count() > 100)
    {
        return Err(Error::action("Malformed transcript metadata"));
    }
    let events = capture["transcript"]["events"]
        .as_array()
        .ok_or_else(|| Error::action("Malformed transcript events"))?;
    let mut text = String::new();
    let mut count = 0;
    for event in events {
        let Some(start) = event["tStartMs"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.)
        else {
            continue;
        };
        let line = event["segs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["utf8"].as_str())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if line.is_empty() {
            continue;
        }
        let seconds = (start / 1000.).floor() as u64;
        let line = format!(
            "[{:02}:{:02}:{:02}] {line}\n",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
        if count >= 5000 || text.len() + line.len() > 500000 {
            text.push_str("[Transcript truncated]\n");
            break;
        }
        text.push_str(&line);
        count += 1;
    }
    Ok((count > 0).then_some(text))
}
