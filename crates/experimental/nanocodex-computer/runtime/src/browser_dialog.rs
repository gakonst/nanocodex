//! Dialog identity and target ownership are independent of command completion.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

struct Dialog {
    value: Value,
    session: Option<String>,
}

#[derive(Clone)]
struct Route {
    tab: String,
    parent: Option<String>,
    target: String,
    frame: Option<String>,
}

struct Pending {
    tab: String,
    session: Option<String>,
    wire_session: Option<String>,
    closed: bool,
}

#[derive(Default)]
pub(super) struct State {
    sequence: u64,
    current: BTreeMap<String, Dialog>,
    routes: BTreeMap<String, Route>,
    // At most one retained route proof per remembered orphaned dialog. It can
    // justify deletion by a late event, never command dispatch or child attach.
    retired: BTreeMap<String, Route>,
    pending: Option<Pending>,
}

impl State {
    /// A remembered record blocks ordinary commands for its top-level tab,
    /// including commands directed at an owned child of that tab.
    pub(super) fn check_method(&self, tab: &str, method: &str) -> Result<()> {
        if method != "Page.handleJavaScriptDialog"
            && let Some(dialog) = self.current.get(tab)
        {
            return Err(Error::action(format!(
                "A {} JavaScript dialog is active in this tab. Use `tab.getJsDialog()` to get it and dismiss it first.",
                dialog.value["type"].as_str().unwrap_or("unknown")
            )));
        }
        Ok(())
    }

    /// This lookup can only add a refusal. Retired provenance never grants
    /// attachment or dispatch authority.
    pub(super) fn modal_tab_for_session(&self, session: &str) -> Option<&str> {
        self.routes
            .get(session)
            .or_else(|| self.retired.get(session))
            .map(|route| route.tab.as_str())
    }

    /// Data-only snapshot from active native routes. Retired dialog cleanup
    /// proofs cannot admit new raw records or confer query/dispatch authority.
    pub(super) fn raw_event_context(&self, event: &Value) -> super::raw_events::Context {
        let source = event["sessionId"].as_str().and_then(|session| {
            self.routes.get(session).map(|route| {
                let top_level = route.parent.is_none();
                let mut source = json!({"sessionId":session});
                if top_level {
                    source["targetId"] = json!(route.target);
                }
                super::raw_events::Source {
                    tab: route.tab.clone(),
                    source,
                    tracked_target: Some(route.target.clone()),
                    top_level,
                }
            })
        });
        let discard = match event["method"].as_str() {
            Some("Target.detachedFromTarget") => event["params"]["sessionId"]
                .as_str()
                .and_then(|session| self.routes.get(session))
                .filter(|route| {
                    route.parent.is_none()
                        && self.removal_source_matches(event["sessionId"].as_str(), route)
                })
                .map(|route| route.tab.clone()),
            Some("Target.targetDestroyed") => event["params"]["targetId"]
                .as_str()
                .and_then(|target| {
                    self.routes
                        .values()
                        .find(|route| route.parent.is_none() && route.target == target)
                })
                .map(|route| route.tab.clone()),
            _ => None,
        };
        super::raw_events::Context { source, discard }
    }

    /// Only current root routes can support the separate native before-unload
    /// sender. Retired dialog proof remains deletion-only.
    pub(super) fn root_matches(&self, tab: &str, session: &str) -> bool {
        self.routes
            .get(session)
            .is_some_and(|route| route.parent.is_none() && route.tab == tab && route.target == tab)
            && !self.retired.contains_key(session)
    }
    pub(super) fn beforeunload_id(&self, tab: &str, session: &str) -> Option<&str> {
        if !self.root_matches(tab, session) {
            return None;
        }
        let dialog = self.current.get(tab)?;
        // Existing storage normalizes root dialogs to a tab-only owner (None).
        // The separate current root route above proves the captured wire session.
        if dialog.session.is_some() || dialog.value["type"] != "beforeunload" {
            return None;
        }
        dialog.value["id"].as_str()
    }

    pub(super) fn get(&self, tab: &str) -> Option<&Value> {
        self.current.get(tab).map(|dialog| &dialog.value)
    }

    pub(super) fn contains_key(&self, tab: &str) -> bool {
        self.current.contains_key(tab)
    }

    pub(super) fn session(&self, tab: &str) -> Option<&str> {
        self.current
            .get(tab)
            .and_then(|dialog| dialog.session.as_deref())
    }

    pub(super) fn opening(&mut self, tab: &str, session: Option<&str>, params: &Value) {
        if !matches!(
            params["type"].as_str(),
            Some("alert" | "confirm" | "prompt" | "beforeunload")
        ) {
            return;
        }
        // Refuse exhausted identity space rather than ever reusing an old ID.
        let Some(next) = self.sequence.checked_add(1) else {
            return;
        };
        self.sequence = next;
        self.retired.retain(|_, route| route.tab != tab);
        let defaulted = |name| {
            params
                .get(name)
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!(""))
        };
        let value = json!({
            "id": next.to_string(),
            "type": params["type"],
            "message": defaulted("message"),
            "promptText": defaulted("defaultPrompt"),
            "url": defaulted("url"),
        });
        self.current.insert(
            tab.to_owned(),
            Dialog {
                value,
                session: session.map(str::to_owned),
            },
        );
    }

    pub(super) fn closed(&mut self, tab: &str, session: Option<&str>) {
        if let Some(pending) = &mut self.pending
            && pending.tab == tab
            && pending.session.as_deref() == session
        {
            pending.closed = true;
        }
        if self
            .current
            .get(tab)
            .is_some_and(|dialog| dialog.session.as_deref() == session)
        {
            self.forget_record(tab);
        }
    }

    pub(super) fn delete_id(&mut self, tab: &str, id: &Value) {
        if self.get(tab).is_some_and(|dialog| dialog["id"] == *id) {
            self.forget_record(tab);
        }
    }

    fn forget_record(&mut self, tab: &str) {
        self.current.remove(tab);
        self.retired.retain(|_, route| route.tab != tab);
    }

    pub(super) fn dispatch_allowed(&self, tab: &str) -> bool {
        let Some(dialog) = self.current.get(tab) else {
            return false;
        };
        if let Some(session) = &dialog.session {
            self.routes
                .get(session)
                .is_some_and(|route| route.tab == tab)
                && !self
                    .retired
                    .get(session)
                    .is_some_and(|route| route.tab == tab)
        } else {
            self.routes
                .values()
                .any(|route| route.tab == tab && route.parent.is_none())
        }
    }

    pub(super) fn remove_session(&mut self, tab: &str, session: &str) {
        let mut removed = std::collections::BTreeSet::from([session.to_owned()]);
        for _ in 0..128 {
            let before = removed.len();
            for (id, route) in &self.routes {
                if route.tab == tab
                    && route
                        .parent
                        .as_ref()
                        .is_some_and(|parent| removed.contains(parent))
                {
                    removed.insert(id.clone());
                }
            }
            if removed.len() == before {
                break;
            }
        }
        if self.session(tab) == Some(session) {
            self.forget_record(tab);
        } else if let Some(owner) = self.session(tab)
            && removed.contains(owner)
            && let Some(route) = self.routes.get(owner)
        {
            self.retired.insert(owner.to_owned(), route.clone());
        }
        self.routes
            .retain(|session, route| route.tab != tab || !removed.contains(session));
    }

    pub(super) fn remove(&mut self, tab: &str) {
        self.forget_record(tab);
        self.routes.retain(|_, route| route.tab != tab);
    }

    pub(super) fn clear(&mut self) {
        self.current.clear();
        self.routes.clear();
        self.retired.clear();
    }

    pub(super) fn root(&mut self, tab: &str, session: &str) {
        self.routes.insert(
            session.to_owned(),
            Route {
                tab: tab.to_owned(),
                parent: None,
                target: tab.to_owned(),
                frame: None,
            },
        );
    }

    pub(super) fn child(&mut self, tab: &str, session: &str, target: &str, parent: &str) {
        if [session, target, parent]
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
            || self
                .routes
                .get(session)
                .is_some_and(|route| route.tab != tab || route.parent.is_none())
            || self.routes.get(parent).is_none_or(|route| route.tab != tab)
            || self
                .retired
                .get(session)
                .is_some_and(|route| route.tab != tab)
        {
            return;
        }
        let previous = self
            .routes
            .iter()
            .find(|(_, route)| route.tab == tab && route.target == target)
            .map(|(id, _)| id.clone());
        if let Some(previous) = previous {
            if previous == session {
                return;
            }
            self.remove_session(tab, &previous);
        }
        if let Some(route) = self.routes.get_mut(session) {
            route.target = target.to_owned();
            route.parent = Some(parent.to_owned());
            return;
        }
        if self
            .routes
            .values()
            .filter(|route| route.parent.is_some())
            .count()
            >= 128
        {
            return;
        }
        self.routes.insert(
            session.to_owned(),
            Route {
                tab: tab.to_owned(),
                parent: Some(parent.to_owned()),
                target: target.to_owned(),
                frame: None,
            },
        );
    }

    /// Runs while receiving the actual CDP message, before best-effort history
    /// retention can evict it. Only previously owned root/child routes count.
    pub(super) fn event(&mut self, event: &Value) {
        let session = event["sessionId"].as_str();
        let owner = session.and_then(|session| {
            self.routes.get(session).map(|route| {
                (
                    route.tab.clone(),
                    route.parent.is_some().then(|| session.to_owned()),
                )
            })
        });
        match event["method"].as_str().unwrap_or("") {
            "Page.javascriptDialogOpening" => {
                if let Some((tab, session)) = owner {
                    self.opening(&tab, session.as_deref(), &event["params"]);
                }
            }
            "Page.javascriptDialogClosed" => {
                if let Some((tab, session)) = owner {
                    self.closed(&tab, session.as_deref());
                } else if let Some(tab) = session
                    .and_then(|session| self.retired.get(session))
                    .map(|route| route.tab.clone())
                {
                    self.closed(&tab, session);
                } else if let Some(pending) = &mut self.pending
                    && pending.wire_session.is_some()
                    && pending.wire_session.as_deref() == session
                {
                    // A verified handler can still receive its own close after
                    // that session's route was removed. This cannot create or
                    // change any dialog/route authority.
                    pending.closed = true;
                }
            }
            "Target.attachedToTarget" if event["params"]["targetInfo"]["type"] == "iframe" => {
                if let (Some((tab, _)), Some(parent), Some(child), Some(target)) = (
                    owner,
                    session,
                    event["params"]["sessionId"].as_str(),
                    event["params"]["targetInfo"]["targetId"].as_str(),
                ) {
                    self.child(&tab, child, target, parent);
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(session) = event["params"]["sessionId"].as_str()
                    && let Some(route) = self
                        .routes
                        .get(session)
                        .or_else(|| self.retired.get(session))
                    && self.removal_source_matches(event["sessionId"].as_str(), route)
                {
                    let tab = route.tab.clone();
                    if route.parent.is_none() {
                        self.remove(&tab);
                    } else {
                        self.remove_session(&tab, session);
                    }
                }
            }
            "Page.frameNavigated" => {
                if let Some(session) = session
                    && let Some(route) = self.routes.get_mut(session)
                    && route.parent.is_some()
                    && route.frame.is_none()
                    && let Some(frame) = event["params"]["frame"]["id"].as_str()
                {
                    route.frame = Some(frame.to_owned());
                }
            }
            "Page.frameDetached" => {
                if let Some(frame) = event["params"]["frameId"].as_str() {
                    let removed = self
                        .routes
                        .iter()
                        .chain(self.retired.iter())
                        .find(|(_, route)| {
                            route.parent.is_some()
                                && (route.target == frame || route.frame.as_deref() == Some(frame))
                                && self.removal_source_matches(session, route)
                        })
                        .map(|(id, route)| (id.clone(), route.tab.clone()));
                    if let Some((session, tab)) = removed {
                        self.remove_session(&tab, &session);
                    }
                }
            }
            "Target.targetDestroyed" => {
                if let Some(target) = event["params"]["targetId"].as_str() {
                    let removed: Vec<_> = self
                        .routes
                        .iter()
                        .filter(|(_, route)| route.target == target)
                        .map(|(id, route)| (id.clone(), route.parent.is_none(), route.tab.clone()))
                        .collect();
                    for (session, root, tab) in removed {
                        if root {
                            self.remove(&tab);
                        } else {
                            self.remove_session(&tab, &session);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn removal_source_matches(&self, source: Option<&str>, route: &Route) -> bool {
        match source {
            // Browser-level target detach can retire its own root session.
            None => route.parent.is_none(),
            Some(source) => self.routes.get(source).map_or_else(
                || route.parent.as_deref() == Some(source),
                |source| source.tab == route.tab,
            ),
        }
    }

    pub(super) fn watches_request(&self, session: Option<&str>) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.wire_session.is_some() && pending.wire_session.as_deref() == session
        })
    }

    pub(super) fn matching_close_observed(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.closed)
    }
}

/// Canonical service fields must win over any unrecognized internal aliases.
/// Schema admission happens before the current-dialog lookup, as in the client
/// command contract. Type-dependent action validation follows that lookup.
pub(super) fn normalize_handle(args: &mut Value) -> Result<()> {
    let browser = args["browser_id"]
        .as_str()
        .ok_or_else(|| Error::invalid("browser_id must be a string"))?
        .to_owned();
    let tab = args["tab_id"]
        .as_str()
        .ok_or_else(|| Error::invalid("tab_id must be a string"))?
        .to_owned();
    let id = args["dialog_id"]
        .as_str()
        .ok_or_else(|| Error::invalid("dialog_id must be a string"))?
        .to_owned();
    if !matches!(args["action"].as_str(), Some("accept" | "dismiss")) {
        return Err(Error::invalid("action must be accept or dismiss"));
    }
    if args
        .get("prompt_text")
        .is_some_and(|text| !text.is_string())
    {
        return Err(Error::invalid("prompt_text must be a string"));
    }
    args["dialogId"] = json!(id);
    args["browser"] = json!(browser);
    args["tab"] = json!(tab);
    let text = args.get("prompt_text").cloned();
    let object = args.as_object_mut().unwrap();
    object.remove("accept");
    object.remove("promptText");
    if let Some(text) = text {
        object.insert("promptText".into(), text);
    }
    Ok(())
}

pub(super) fn requested_id(args: &Value) -> Result<&str> {
    args["dialogId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::action("handleJsDialog requires a dialog_id"))
}

/// Lower a semantic handle action only after its captured identity is current.
/// The legacy boolean envelope remains an alias for these same typed actions;
/// it does not bypass prompt or alert validation.
pub(super) fn action_params(dialog: &Value, args: &Value) -> Result<Value> {
    let action = match args.get("action") {
        Some(value) => value
            .as_str()
            .ok_or_else(|| Error::invalid("action must be accept or dismiss"))?,
        None => match args["accept"].as_bool() {
            Some(true) => "accept",
            Some(false) => "dismiss",
            None => return Err(Error::invalid("accept must be boolean")),
        },
    };
    let kind = dialog["type"].as_str().unwrap_or("");
    match action {
        "dismiss" => Ok(json!({"accept": kind == "alert"})),
        "accept" => match kind {
            "alert" | "beforeunload" => Err(Error::action(format!(
                "Dialog type {kind} does not support accept()"
            ))),
            "confirm" => {
                if args.get("promptText").is_some_and(|text| !text.is_null()) {
                    return Err(Error::action("Confirm dialogs do not accept prompt text"));
                }
                Ok(json!({"accept": true}))
            }
            "prompt" => {
                let text = args["promptText"]
                    .as_str()
                    .ok_or_else(|| Error::action("Prompt dialogs require prompt text"))?;
                Ok(json!({"accept": true, "promptText": text}))
            }
            _ => Err(Error::action("Unsupported JavaScript dialog type")),
        },
        _ => Err(Error::action(format!(
            "Unsupported dialog action: {action}"
        ))),
    }
}

pub(super) struct Watch(Arc<Mutex<State>>);
impl Watch {
    pub(super) fn start(state: Arc<Mutex<State>>, tab: &str, session: Option<String>) -> Self {
        let mut owner = state.lock().unwrap();
        let wire_session = session.clone().or_else(|| {
            owner
                .routes
                .iter()
                .find(|(_, route)| route.tab == tab && route.parent.is_none())
                .map(|(id, _)| id.clone())
        });
        owner.pending = Some(Pending {
            tab: tab.to_owned(),
            session,
            wire_session,
            closed: false,
        });
        drop(owner);
        Self(state)
    }
    pub(super) fn closed(&self) -> bool {
        self.0.lock().unwrap().matching_close_observed()
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.0.lock().unwrap().pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_modal_gate_matches_original_caller_partitions() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_dialog_modal.json"))
                .unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let mut state = State::default();
            state.root("1", "root");
            state.root("2", "foreign");
            state.child("1", "child", "frame", "root");
            if !case["type"].is_null() {
                state.opening(
                    &case["owner"].to_string(),
                    None,
                    &json!({"type":case["type"]}),
                );
            }
            let actual = state.check_method("1", case["method"].as_str().unwrap());
            let value = match actual {
                Ok(()) => json!({"ok":true}),
                Err(error) => json!({"error":error.message}),
            };
            assert_eq!(value, case["gate"], "{case}");
            assert_eq!(state.modal_tab_for_session("child"), Some("1"));
            assert_eq!(state.modal_tab_for_session("unknown"), None);
        }
    }

    #[test]
    fn dialog_opening_projection_matches_source_pinned_original() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_dialog_actions.json"))
                .unwrap();
        for case in oracle["opening_cases"].as_array().unwrap() {
            let mut state = State::default();
            state.root("1", "root");
            state.child("1", "child", "frame", "root");
            for (event, expected) in case["events"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["rows"].as_array().unwrap())
            {
                state.opening("1", case["session"].as_str(), event);
                assert_eq!(state.get("1"), Some(&expected["value"]), "{case}");
                assert_eq!(json!(state.session("1")), expected["session"], "{case}");
                assert!(state.dispatch_allowed("1"));
            }
        }
    }

    #[test]
    fn dialog_deletion_partitions_match_source_pinned_original() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_dialog.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let mut state = State::default();
            state.root("1", "root-1");
            state.root("2", "root-2");
            for (tab, session, target, parent) in [
                ("1", "child-a", "frame-a", "root-1"),
                ("1", "child-b", "frame-b", "root-1"),
                ("1", "child-nested", "frame-nested", "child-a"),
                ("2", "child-other", "frame-other", "root-2"),
            ] {
                state.child(tab, session, target, parent);
            }
            let mut frames = BTreeMap::from([
                (("1".to_owned(), "frame-a".to_owned()), "child-a".to_owned()),
                (("1".to_owned(), "frame-b".to_owned()), "child-b".to_owned()),
                (
                    ("1".to_owned(), "frame-nested".to_owned()),
                    "child-nested".to_owned(),
                ),
                (
                    ("2".to_owned(), "frame-other".to_owned()),
                    "child-other".to_owned(),
                ),
            ]);
            for (action, expected) in case["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["rows"].as_array().unwrap())
            {
                let tab = action["tab"].as_str().unwrap_or("1");
                let session = action["session"].as_str();
                match action["op"].as_str().unwrap() {
                    "open" => state.opening(
                        tab,
                        session,
                        &json!({"type":action["kind"].as_str().unwrap_or("prompt")}),
                    ),
                    "close" => state.closed(tab, session),
                    "delete" => state.delete_id(tab, &action["id"]),
                    "removeSession" => {
                        if let Some(((tab, _), _)) =
                            frames.iter().find(|(_, s)| Some(s.as_str()) == session)
                        {
                            state.remove_session(tab, session.unwrap());
                            frames.retain(|_, s| Some(s.as_str()) != session);
                        }
                    }
                    "removeFrame" => {
                        if let Some(session) = frames
                            .remove(&(tab.to_owned(), action["frame"].as_str().unwrap().to_owned()))
                        {
                            state.remove_session(tab, &session);
                        }
                    }
                    "removeTab" => state.remove(tab),
                    _ => panic!("Unknown dialog oracle action"),
                }
                let observed: Vec<_> = ["1", "2"].iter().map(|tab| {
                    state.get(tab).map(|value| json!({"id":value["id"],"type":value["type"],"session":state.session(tab)})).unwrap_or(Value::Null)
                }).collect();
                assert_eq!(json!(observed), *expected, "{}: {action}", case["name"]);
            }
        }
    }

    #[test]
    fn dialog_disconnect_preserves_identity_space_and_refuses_wraparound() {
        let mut state = State::default();
        state.opening("tab", None, &json!({"type":"prompt"}));
        let previous = state.get("tab").unwrap()["id"].clone();
        state.clear();
        state.opening("tab", Some("nested"), &json!({"type":"confirm"}));
        state.delete_id("tab", &previous);
        assert_eq!(state.get("tab").unwrap()["id"], "2");
        state.sequence = u64::MAX;
        state.opening("tab", None, &json!({"type":"alert"}));
        assert_eq!(state.get("tab").unwrap()["id"], "2");
    }

    #[test]
    fn dialog_handle_watch_is_target_bound_and_released_on_scope_exit() {
        let state = Arc::new(Mutex::new(State::default()));
        {
            let watch = Watch::start(state.clone(), "tab", Some("child".to_owned()));
            state.lock().unwrap().closed("other", Some("child"));
            assert!(!watch.closed());
            state.lock().unwrap().closed("tab", None);
            assert!(!watch.closed());
            state.lock().unwrap().closed("tab", Some("child"));
            assert!(watch.closed());
        }
        assert!(state.lock().unwrap().pending.is_none());
        {
            let watch = Watch::start(state.clone(), "tab", None);
            assert!(!watch.closed());
            state.lock().unwrap().clear();
            assert!(!watch.closed());
        }
        assert!(state.lock().unwrap().pending.is_none());
    }

    #[test]
    fn dialog_retired_provenance_cannot_be_claimed_by_a_foreign_tab() {
        let mut state = State::default();
        state.root("one", "root-one");
        state.root("two", "root-two");
        state.child("one", "parent", "frame-parent", "root-one");
        state.child("one", "nested", "frame-nested", "parent");
        state.opening("one", Some("nested"), &json!({"type":"prompt"}));
        let remembered = state.get("one").unwrap().clone();
        state.remove_session("one", "parent");
        assert_eq!(state.retired.len(), 1);
        assert!(!state.routes.contains_key("parent"));
        assert!(!state.routes.contains_key("nested"));
        assert!(!state.dispatch_allowed("one"));
        state.child("two", "nested", "foreign-target", "root-two");
        assert!(!state.routes.contains_key("nested"));
        for event in [
            json!({"sessionId":"root-two","method":"Target.detachedFromTarget","params":{"sessionId":"nested"}}),
            json!({"sessionId":"root-two","method":"Page.frameDetached","params":{"frameId":"frame-nested"}}),
            json!({"sessionId":"root-two","method":"Page.javascriptDialogClosed","params":{}}),
        ] {
            state.event(&event);
            assert_eq!(state.get("one"), Some(&remembered));
            assert!(!state.dispatch_allowed("one"));
        }
        state.event(
            &json!({"sessionId":"nested","method":"Page.javascriptDialogClosed","params":{}}),
        );
        assert!(state.get("one").is_none());
        assert!(state.retired.is_empty());
        state.child("two", "nested", "foreign-target", "root-two");
        state.opening("two", Some("nested"), &json!({"type":"confirm"}));
        assert!(state.dispatch_allowed("two"));
        assert_ne!(state.get("two").unwrap()["id"], remembered["id"]);
        state.clear();
        assert!(state.retired.is_empty());
    }
}
