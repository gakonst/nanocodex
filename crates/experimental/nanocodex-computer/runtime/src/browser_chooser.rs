//! Owned chooser watches and document-bound, single-consumption handles.
use super::{Browser, Browsers};
use crate::{Error, Result, engine::string};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const LIMIT: usize = 128;
const NODE_BINDING: &str = "function(){const d=this.ownerDocument,w=d?.defaultView;return this.isConnected&&this.localName==='input'&&this.type==='file'&&w?.document===d?{timeOrigin:w.performance.timeOrigin}:null;}";
fn id(prefix: &str) -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::action(format!("Chooser identifier: {error}")))?;
    Ok(format!(
        "{prefix}-{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}
fn chooser_timeout(args: &Value) -> (Duration, String) {
    // Captured De accepts any JS number and defaults non-numbers to 3000ms.
    // This boundary receives JSON: non-finite JS values arrive as null.
    let ms = args
        .get("timeoutMs")
        .and_then(Value::as_f64)
        .unwrap_or(3000.0)
        .clamp(0.0, 3000.0);
    let ms = if ms == 0.0 { 0.0 } else { ms };
    let label = if ms > 0.0 && ms < 1e-6 {
        format!("{ms:e}")
    } else {
        ms.to_string()
    };
    (Duration::from_secs_f64(ms / 1000.0), label)
}
fn unsupported_frame() -> Error {
    Error::action(
        "Browser Use rejected this action due to browser security policy. Reason: File uploads in out-of-process frames are not supported. The agent must not attempt to achieve the same outcome via workaround, indirect execution, raw CDP or browser commands, alternate browser surfaces, or policy circumvention. Proceed only with a materially safer alternative that does not require this blocked browser action; if none exists, stop and request user input.",
    )
}
#[derive(Clone)]
struct Observed {
    session: String,
    backend: u64,
    multiple: bool,
    frame: Option<String>,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) scope: String,
    pub(super) cell: u64,
}
struct Terminal {
    tab: String,
    owner: Option<Owner>,
    result: Result<Value>,
    at: Instant,
}
struct Watch {
    tab: String,
    root: String,
    sessions: BTreeSet<String>,
    order: u64,
    event: Option<Observed>,
    failure: Option<String>,
    owner: Option<Owner>,
    deadline: Instant,
    timeout_label: String,
    armed: bool,
}
#[derive(Clone)]
struct Entry {
    tab: String,
    event: Observed,
    binding: Value,
    owner: Option<Owner>,
}
#[derive(Default)]
pub(super) struct State {
    watches: BTreeMap<String, Watch>,
    entries: BTreeMap<String, Entry>,
    order: u64,
    pub(super) owner: Option<Owner>,
    terminal: BTreeMap<String, Terminal>,
    auto_roots: BTreeSet<String>,
}
impl State {
    pub(super) fn has_watch(&self, tab: &str) -> bool {
        self.watches.values().any(|watch| watch.tab == tab)
    }
    pub(super) fn fail_tab(&mut self, tab: &str, message: &str) {
        for watch in self.watches.values_mut().filter(|watch| watch.tab == tab) {
            watch.failure = Some(message.into());
        }
    }

    pub(super) fn observe(&mut self, event: &Value, tab: Option<&str>) {
        let Some(tab) = tab else { return };
        let session = event["sessionId"].as_str().unwrap_or("");
        match event["method"].as_str().unwrap_or("") {
            "Page.fileChooserOpened" => {
                let Some(backend) = event["params"]["backendNodeId"]
                    .as_u64()
                    .filter(|id| *id > 0)
                else {
                    return;
                };
                let observed = Observed {
                    session: session.into(),
                    backend,
                    multiple: event["params"]["mode"] == "selectMultiple",
                    frame: event["params"]["frameId"]
                        .as_str()
                        .filter(|id| id.len() <= 256)
                        .map(str::to_owned),
                };
                for watch in self.watches.values_mut().filter(|watch| {
                    watch.tab == tab
                        && watch.event.is_none()
                        && watch.failure.is_none()
                        && (!watch.armed || Instant::now() <= watch.deadline)
                }) {
                    watch.event = Some(observed.clone());
                }
            }
            "Page.frameNavigated" => self.invalidate(
                tab,
                if event["params"]["frame"]["parentId"].is_null() {
                    None
                } else {
                    event["params"]["frame"]["id"].as_str()
                },
                None,
            ),
            "Page.frameDetached" => self.invalidate(tab, event["params"]["frameId"].as_str(), None),
            _ => {}
        }
    }
    fn invalidate(&mut self, tab: &str, frame: Option<&str>, session: Option<&str>) {
        let affected = |event: &Observed| {
            session.is_none_or(|session| event.session == session)
                && frame.is_none_or(|frame| event.frame.as_deref().is_none_or(|id| id == frame))
        };
        self.entries
            .retain(|_, entry| entry.tab != tab || !affected(&entry.event));
        for watch in self.watches.values_mut().filter(|watch| watch.tab == tab) {
            if watch.event.as_ref().is_some_and(&affected) {
                watch.event = None;
                watch.failure = Some("File chooser document changed".into());
            }
        }
    }
    pub(super) fn detach(&mut self, session: &str) {
        self.auto_roots.remove(session);
        let tabs: BTreeSet<_> = self
            .watches
            .values()
            .filter(|watch| watch.sessions.contains(session))
            .map(|watch| watch.tab.clone())
            .chain(
                self.entries
                    .values()
                    .filter(|entry| entry.event.session == session)
                    .map(|entry| entry.tab.clone()),
            )
            .collect();
        for tab in tabs {
            self.invalidate(&tab, None, Some(session));
        }
    }
    pub(super) fn remove_tab(&mut self, tab: &str) {
        self.watches.retain(|_, watch| watch.tab != tab);
        self.entries.retain(|_, entry| entry.tab != tab);
        self.terminal.retain(|_, result| result.tab != tab);
    }
    pub(super) fn clear(&mut self) {
        self.watches.clear();
        self.entries.clear();
        self.terminal.clear();
        self.auto_roots.clear();
    }
}
impl Browsers {
    /// Trusted host context, never populated from model arguments.
    pub fn begin_chooser_cell(&mut self, scope: &str, cell: u64) {
        if let Some(previous) = self.chooser_owner.clone() {
            self.cancel_raw_wait_scope(&previous.scope, Some(previous.cell));
        }
        let owner = Some(Owner {
            scope: scope.into(),
            cell,
        });
        self.chooser_owner = owner.clone();
        for browser in self.providers.values_mut() {
            browser.surface.choosers.owner = owner.clone();
            browser.surface.downloads.lock().unwrap().owner = owner.clone();
        }
    }
    pub fn finish_chooser_cell(&mut self, scope: &str, cell: u64, cancelled: bool) {
        self.cancel_raw_wait_scope(scope, Some(cell));
        let owner = Owner {
            scope: scope.into(),
            cell,
        };
        for browser in self.providers.values_mut() {
            browser.contract.cancel_owner(scope, Some(cell));
            browser.cancel_downloads(None, Some(scope), Some(cell));
            let _ = browser.download_maintenance();
            browser.surface.downloads.lock().unwrap().owner = None;
            if cancelled {
                browser.cancel_chooser_owner(scope, Some(cell));
            }
            if browser.surface.choosers.owner.as_ref() == Some(&owner) {
                browser.surface.choosers.owner = None;
            }
        }
        if self.chooser_owner.as_ref() == Some(&owner) {
            self.chooser_owner = None;
        }
    }
    pub fn reset_chooser_scope(&mut self, scope: &str) {
        self.cancel_raw_wait_scope(scope, None);
        for browser in self.providers.values_mut() {
            browser.contract.cancel_owner(scope, None);
            browser.cancel_chooser_owner(scope, None);
            browser.cancel_downloads(None, Some(scope), None);
            let _ = browser.download_maintenance();
        }
    }
    pub fn tick_choosers(&mut self) {
        for browser in self.providers.values_mut() {
            browser.contract.tick();
            browser.tick_downloads();
            if !browser.surface.choosers.watches.is_empty() {
                let _ = browser.poll_events();
                browser.chooser_tick();
            }
        }
    }
}
impl Browser {
    fn cancel_chooser_owner(&mut self, scope: &str, cell: Option<u64>) {
        let keys: Vec<_> = self
            .surface
            .choosers
            .watches
            .iter()
            .filter(|(_, watch)| {
                watch.owner.as_ref().is_some_and(|owner| {
                    owner.scope == scope && cell.is_none_or(|cell| owner.cell == cell)
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for key in keys {
            self.chooser_complete(&key, Err(Error::new(-32800, "File chooser wait cancelled")));
        }
        // Completed handles belong to the retained browser service, not JS bindings.
    }
    fn chooser_complete(&mut self, key: &str, result: Result<Value>) {
        if let Some(watch) = self.surface.choosers.watches.get(key) {
            let terminal = Terminal {
                tab: watch.tab.clone(),
                owner: watch.owner.clone(),
                result,
                at: Instant::now(),
            };
            while self.surface.choosers.terminal.len() >= LIMIT {
                let oldest = self
                    .surface
                    .choosers
                    .terminal
                    .iter()
                    .min_by_key(|(_, entry)| entry.at)
                    .map(|(id, _)| id.clone())
                    .unwrap();
                self.surface.choosers.terminal.remove(&oldest);
            }
            self.surface.choosers.terminal.insert(key.into(), terminal);
        }
        self.chooser_finish(key);
    }
    fn chooser_tick(&mut self) {
        self.surface
            .choosers
            .terminal
            .retain(|_, entry| entry.at.elapsed() < Duration::from_secs(30));
        let keys: Vec<_> = self.surface.choosers.watches.keys().cloned().collect();
        for key in keys {
            let Some(watch) = self.surface.choosers.watches.get(&key) else {
                continue;
            };
            let result = if let Some(error) = &watch.failure {
                Some(Err(Error::action(error.clone())))
            } else if let Some(event) = watch.event.clone() {
                let (tab, owner, root) =
                    (watch.tab.clone(), watch.owner.clone(), watch.root.clone());
                Some((|| {
                    if event.session != root {
                        return Err(unsupported_frame());
                    }
                    let binding = self.chooser_binding(&event)?;
                    let id = id("chooser")?;
                    let result = json!({"id":id,"file_chooser_id":id,"is_multiple":event.multiple,"mode":if event.multiple{"selectMultiple"}else{"selectSingle"},"backendNodeId":event.backend,"frameId":event.frame});
                    self.surface.choosers.entries.insert(
                        id,
                        Entry {
                            tab,
                            event,
                            binding,
                            owner,
                        },
                    );
                    Ok(result)
                })())
            } else if Instant::now() >= watch.deadline {
                Some(Err(Error::action(format!(
                    "Timed out after {}ms waiting for file chooser.",
                    watch.timeout_label
                ))))
            } else {
                None
            };
            if let Some(result) = result {
                self.chooser_complete(&key, result);
            }
        }
    }
    fn chooser_poll(&mut self, tab: &str, key: &str) -> Result<Value> {
        let owner = self
            .surface
            .choosers
            .owner
            .as_ref()
            .map(|owner| &owner.scope);
        let check = self
            .surface
            .choosers
            .watches
            .get(key)
            .map(|w| (&w.tab, w.owner.as_ref()))
            .or_else(|| {
                self.surface
                    .choosers
                    .terminal
                    .get(key)
                    .map(|w| (&w.tab, w.owner.as_ref()))
            });
        if !check.is_some_and(|(bound, stored)| bound == tab && stored.map(|o| &o.scope) == owner) {
            return Err(Error::action("Unknown file chooser watch"));
        }
        self.poll_events()?;
        self.chooser_tick();
        if let Some(terminal) = self.surface.choosers.terminal.remove(key) {
            terminal
                .result
                .map(|value| json!({"pending":false,"value":value}))
        } else if self.surface.choosers.watches.contains_key(key) {
            Ok(json!({"pending":true}))
        } else {
            Err(Error::action("File chooser watch was invalidated"))
        }
    }
    pub(super) fn chooser_session_attached(&mut self, tab: &str, session: &str) -> Result<()> {
        if !self
            .surface
            .choosers
            .watches
            .values()
            .any(|watch| watch.tab == tab)
        {
            return Ok(());
        }
        for watch in self
            .surface
            .choosers
            .watches
            .values_mut()
            .filter(|watch| watch.tab == tab)
        {
            watch.sessions.insert(session.into());
        }
        self.call("Page.enable", json!({}), Some(session))?;
        if self.surface.frame_session_tab(session).as_deref() != Some(tab) {
            return Ok(());
        }
        self.call(
            "Page.setInterceptFileChooserDialog",
            json!({"enabled":true}),
            Some(session),
        )?;
        Ok(())
    }
    fn chooser_arm(&mut self, tab: &str, args: &Value) -> Result<String> {
        let (wait, timeout_label) = chooser_timeout(args);
        self.chooser_tick();
        self.page(tab)?;
        if self.surface.choosers.watches.len() + self.surface.choosers.entries.len() >= LIMIT {
            return Err(Error::action("File chooser handle limit exceeded"));
        }
        let root = self.session(tab)?;
        let mut sessions: BTreeSet<_> = self
            .surface
            .frame_sessions_for_tab(tab)
            .into_iter()
            .collect();
        sessions.insert(root.clone());
        let key = id("chooser-watch")?;
        self.surface.choosers.order += 1;
        let watch = Watch {
            tab: tab.into(),
            root: root.clone(),
            sessions: sessions.clone(),
            order: self.surface.choosers.order,
            event: None,
            failure: None,
            owner: self.surface.choosers.owner.clone(),
            deadline: Instant::now() + wait,
            timeout_label,
            armed: false,
        };
        self.surface.choosers.watches.insert(key.clone(), watch);
        let result = (|| -> Result<()> {
            if self.surface.choosers.auto_roots.insert(root.clone()) {
                self.tab(tab,"Target.setAutoAttach",json!({"autoAttach":true,"flatten":true,"waitForDebuggerOnStart":false,"filter":[{"type":"iframe","exclude":false},{"exclude":true}]}))?;
            }
            for session in sessions {
                self.call("Page.enable", json!({}), Some(&session))?;
                self.call(
                    "Page.setInterceptFileChooserDialog",
                    json!({"enabled":true}),
                    Some(&session),
                )?;
                if session != root {
                    self.call("Target.setAutoAttach",json!({"autoAttach":true,"flatten":true,"waitForDebuggerOnStart":false,"filter":[{"type":"iframe","exclude":false},{"exclude":true}]}),Some(&session))?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.surface.choosers.auto_roots.remove(&root);
            self.chooser_finish(&key);
            return Err(error);
        }
        if let Some(watch) = self.surface.choosers.watches.get_mut(&key) {
            watch.deadline = Instant::now() + wait;
            watch.armed = true;
        }
        Ok(key)
    }
    pub(super) fn cancel_choosers(&mut self, tab: Option<&str>) {
        let watches: Vec<_> = self
            .surface
            .choosers
            .watches
            .iter()
            .filter(|(_, watch)| tab.is_none_or(|tab| watch.tab == tab))
            .map(|(id, _)| id.clone())
            .collect();
        for watch in watches {
            self.chooser_finish(&watch);
        }
        self.surface
            .choosers
            .entries
            .retain(|_, entry| tab.is_some_and(|tab| entry.tab != tab));
    }
    fn chooser_finish(&mut self, watch: &str) {
        if let Some(watch) = self.surface.choosers.watches.remove(watch) {
            for session in watch.sessions {
                if !self.sessions.values().any(|id| id == &session)
                    && self.surface.frame_session_tab(&session).is_none()
                {
                    continue;
                }
                if !self
                    .surface
                    .choosers
                    .watches
                    .values()
                    .any(|other| other.sessions.contains(&session))
                {
                    let _ = self.call_maintenance(
                        "Page.setInterceptFileChooserDialog",
                        json!({"enabled":false}),
                        Some(&session),
                    );
                }
            }
        }
    }
    fn chooser_binding(&mut self, event: &Observed) -> Result<Value> {
        if let Some(frame) = &event.frame {
            let tree = self.call("Page.getFrameTree", json!({}), Some(&event.session))?;
            fn contains(tree: &Value, id: &str) -> bool {
                tree["frame"]["id"] == id
                    || tree["childFrames"]
                        .as_array()
                        .is_some_and(|frames| frames.iter().any(|frame| contains(frame, id)))
            }
            if !contains(&tree["frameTree"], frame) {
                return Err(Error::action("File chooser is stale or no longer attached"));
            }
        }
        let result = self.call(
            "DOM.resolveNode",
            json!({"backendNodeId":event.backend}),
            Some(&event.session),
        )?;
        let object = string(&result["object"], "objectId")?.to_owned();
        let result = self.call(
            "Runtime.callFunctionOn",
            json!({"objectId":object,"functionDeclaration":NODE_BINDING,"returnByValue":true}),
            Some(&event.session),
        );
        if !result.as_ref().is_err_and(|error| error.code == -32006) {
            let _ = self.call_maintenance(
                "Runtime.releaseObject",
                json!({"objectId":object}),
                Some(&event.session),
            );
        }
        let result = result?;
        if result.get("exceptionDetails").is_some()
            || !result["result"]["value"]["timeOrigin"].is_number()
        {
            return Err(Error::action("File chooser is stale or no longer attached"));
        }
        Ok(result["result"]["value"].clone())
    }
    pub(super) fn chooser_command(
        &mut self,
        method: &str,
        tab: &str,
        args: &Value,
    ) -> Result<Value> {
        match method {
            "file_chooser_enable" => Ok(json!({"watchId":self.chooser_arm(tab,args)?})),
            "file_chooser_poll" => self.chooser_poll(tab, string(args, "watchId")?),
            "wait_for_file_chooser" => {
                let key = if let Some(key) = args["watchId"].as_str() {
                    key.into()
                } else if let Some((id, _)) = self
                    .surface
                    .choosers
                    .watches
                    .iter()
                    .filter(|(_, watch)| watch.tab == tab)
                    .max_by_key(|(_, watch)| watch.order)
                {
                    id.clone()
                } else {
                    self.chooser_arm(tab, args)?
                };
                // Legacy direct callers can shorten an armed watch, never extend it.
                if args.get("timeoutMs").is_some()
                    && let Some(watch) = self.surface.choosers.watches.get_mut(&key)
                {
                    let (timeout, timeout_label) = chooser_timeout(args);
                    let deadline = Instant::now() + timeout;
                    if deadline < watch.deadline {
                        watch.deadline = deadline;
                        watch.timeout_label = timeout_label;
                    }
                }
                loop {
                    let polled = self.chooser_poll(tab, &key)?;
                    if polled["pending"] == false {
                        return Ok(polled["value"].clone());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            "file_chooser_set_files" => {
                let files = args["files"]
                    .as_array()
                    .ok_or_else(|| Error::invalid("files must be an array"))?;
                if files.iter().any(|file| !file.is_string()) {
                    return Err(Error::invalid("File path must be a string"));
                }
                let key = string(args, "fileChooserId")?;
                let entry = self
                    .surface
                    .choosers
                    .entries
                    .get(key)
                    .ok_or_else(|| Error::action(format!("Unknown file chooser id \"{key}\"")))?
                    .clone();
                if entry.owner.as_ref().map(|owner| &owner.scope)
                    != self
                        .surface
                        .choosers
                        .owner
                        .as_ref()
                        .map(|owner| &owner.scope)
                {
                    return Err(Error::action(format!("Unknown file chooser id \"{key}\"")));
                }
                if entry.tab != tab {
                    return Err(Error::action(format!(
                        "File chooser \"{key}\" belongs to tab {}",
                        entry.tab
                    )));
                }
                if files.is_empty() {
                    return Err(Error::action(
                        "fileChooser.setFiles requires at least one file",
                    ));
                }
                if !entry.event.multiple && files.len() > 1 {
                    return Err(Error::action("File chooser does not accept multiple files"));
                }
                for file in files {
                    let path = std::path::Path::new(file.as_str().unwrap());
                    if !path.is_absolute() || !path.is_file() {
                        return Err(Error::invalid(
                            "File chooser requires absolute regular file paths",
                        ));
                    }
                }
                let binding = self.chooser_binding(&entry.event);
                if binding.as_ref().is_err()
                    || binding
                        .as_ref()
                        .is_ok_and(|binding| *binding != entry.binding)
                {
                    self.surface.choosers.entries.remove(key);
                    return Err(Error::action("File chooser is stale or no longer attached"));
                }
                let result = self.call(
                    "DOM.setFileInputFiles",
                    json!({"backendNodeId":entry.event.backend,"files":files}),
                    Some(&entry.event.session),
                )?;
                self.surface.choosers.entries.remove(key);
                Ok(result)
            }
            _ => Err(Error::unsupported("Unknown file chooser operation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn watch() -> Watch {
        Watch {
            tab: "tab".into(),
            root: "root".into(),
            sessions: BTreeSet::from(["root".into(), "child".into()]),
            order: 1,
            event: None,
            failure: None,
            owner: None,
            deadline: Instant::now() + Duration::from_secs(3),
            timeout_label: "3000".into(),
            armed: true,
        }
    }
    #[test]
    fn chooser_timeout_normalization_matches_captured_helper() {
        let rows: Vec<Value> = serde_json::from_str(include_str!(
            "../tests/oracles/browser_chooser_timeouts.json"
        ))
        .unwrap();
        for row in rows {
            let args = match row["input"].as_str().unwrap() {
                "NaN" | "Infinity" | "-Infinity" => json!({"timeoutMs":null}),
                "undefined" => json!({}),
                "null" => json!({"timeoutMs":null}),
                "7" => json!({"timeoutMs":"7"}),
                number => json!({"timeoutMs":number.parse::<f64>().unwrap()}),
            };
            let (duration, label) = chooser_timeout(&args);
            assert_eq!(label, row["wireTimeout"], "{row}");
            assert!(duration <= Duration::from_secs(3));
        }
    }
    #[test]
    fn child_session_events_keep_the_emitting_session_and_wrong_tabs_are_ignored() {
        let mut state = State::default();
        state.watches.insert("watch".into(), watch());
        let event = json!({"sessionId":"child","method":"Page.fileChooserOpened","params":{"backendNodeId":77,"mode":"selectMultiple","frameId":"frame"}});
        state.observe(&event, Some("other"));
        assert!(state.watches["watch"].event.is_none());
        state.observe(&event, Some("tab"));
        let seen = state.watches["watch"].event.as_ref().unwrap();
        assert_eq!(seen.session, "child");
        assert_eq!(seen.backend, 77);
        assert!(seen.multiple);
        state.detach("child");
        assert!(state.watches["watch"].event.is_none());
        assert_eq!(
            state.watches["watch"].failure.as_deref(),
            Some("File chooser document changed")
        );
    }
    #[test]
    fn event_after_armed_deadline_is_not_recorded_but_setup_event_is_retained() {
        let mut state = State::default();
        let mut expired = watch();
        expired.deadline = Instant::now() - Duration::from_millis(1);
        state.watches.insert("expired".into(), expired);
        let mut initializing = watch();
        initializing.deadline = Instant::now() - Duration::from_millis(1);
        initializing.armed = false;
        state.watches.insert("setup".into(), initializing);
        state.observe(&json!({"sessionId":"root","method":"Page.fileChooserOpened","params":{"backendNodeId":77}}),Some("tab"));
        assert!(state.watches["expired"].event.is_none());
        assert!(state.watches["setup"].event.is_some());
    }
    #[test]
    fn event_before_watch_and_invalid_backend_id_are_never_reused() {
        let mut state = State::default();
        let mut event = json!({"sessionId":"root","method":"Page.fileChooserOpened","params":{"backendNodeId":77}});
        state.observe(&event, Some("tab"));
        state.watches.insert("watch".into(), watch());
        assert!(state.watches["watch"].event.is_none());
        for invalid in [json!(0), json!(-1), json!(1.5), json!("77"), Value::Null] {
            event["params"]["backendNodeId"] = invalid;
            state.observe(&event, Some("tab"));
            assert!(state.watches["watch"].event.is_none());
        }
    }
}
