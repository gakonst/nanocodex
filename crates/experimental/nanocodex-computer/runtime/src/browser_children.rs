//! Register only iframe descendants announced through an owned CDP session.
use super::{Browser, surface::Surface};
use crate::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
const LIMIT: usize = 128;
#[derive(Default)]
pub(super) struct State {
    unresolved: VecDeque<Value>,
    pending: VecDeque<(String, String, String)>, // tab, target, session
    draining: bool,
    parents: BTreeMap<String, String>,
}
impl State {
    pub(super) fn detached(&mut self, session: &str) -> std::collections::BTreeSet<String> {
        let mut removed = std::collections::BTreeSet::from([session.to_owned()]);
        for _ in 0..LIMIT {
            let mut changed = false;
            for (child, parent) in &self.parents {
                if removed.contains(parent) {
                    changed |= removed.insert(child.clone());
                }
            }
            for event in &self.unresolved {
                if event["sessionId"]
                    .as_str()
                    .is_some_and(|id| removed.contains(id))
                    && let Some(id) = event["params"]["sessionId"].as_str()
                {
                    changed |= removed.insert(id.into());
                }
            }
            if !changed {
                break;
            }
        }
        self.unresolved.retain(|event| {
            !event["params"]["sessionId"]
                .as_str()
                .is_some_and(|id| removed.contains(id))
        });
        self.pending.retain(|(_, _, id)| !removed.contains(id));
        self.parents.retain(|child, _| !removed.contains(child));
        removed
    }
}
impl Surface {
    pub(super) fn forget_child_session(&mut self, session: &str) {
        if let Some(tab) = self.frame_session_tab(session) {
            self.dialogs.lock().unwrap().remove_session(&tab, session);
        }
        for removed in self.children.detached(session) {
            self.choosers.detach(&removed);
            self.frame_sessions.retain(|_, id| id != &removed);
        }
        self.frame_parents
            .retain(|key, _| self.frame_sessions.contains_key(key));
    }
    pub(super) fn child_event(&mut self, event: &Value, sessions: &BTreeMap<String, String>) {
        if event["method"] != "Target.attachedToTarget"
            || event["params"]["targetInfo"]["type"] != "iframe"
        {
            return;
        }
        if self.children.unresolved.len() >= LIMIT {
            return;
        }
        let (Some(parent), Some(session), Some(target)) = (
            event["sessionId"].as_str(),
            event["params"]["sessionId"].as_str(),
            event["params"]["targetInfo"]["targetId"].as_str(),
        ) else {
            return;
        };
        if [parent, session, target]
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
        {
            return;
        }
        // Retain only bounded routing identity; target metadata may be arbitrarily large.
        self.children.unresolved.push_back(json!({"sessionId":parent,"params":{"sessionId":session,"targetInfo":{"type":"iframe","targetId":target}}}));
        self.resolve_children(sessions);
    }
    pub(super) fn resolve_children(&mut self, sessions: &BTreeMap<String, String>) {
        for _ in 0..LIMIT {
            let mut progress = false;
            let count = self.children.unresolved.len();
            for _ in 0..count {
                let Some(event) = self.children.unresolved.pop_front() else {
                    break;
                };
                let (Some(parent), Some(session), Some(target)) = (
                    event["sessionId"].as_str(),
                    event["params"]["sessionId"].as_str(),
                    event["params"]["targetInfo"]["targetId"].as_str(),
                ) else {
                    continue;
                };
                if [parent, session, target]
                    .iter()
                    .any(|value| value.len() > 256)
                {
                    continue;
                }
                let tab = sessions
                    .iter()
                    .find(|(_, session)| session.as_str() == parent)
                    .map(|(tab, _)| tab.clone())
                    .or_else(|| self.frame_session_tab(parent));
                let Some(tab) = tab else {
                    self.children.unresolved.push_back(event);
                    continue;
                };
                if sessions.values().any(|root| root == session)
                    || self
                        .frame_session_tab(session)
                        .is_some_and(|owner| owner != tab)
                {
                    continue;
                }
                if self.frame_sessions.len() >= LIMIT
                    && !self
                        .frame_sessions
                        .contains_key(&(tab.clone(), target.into()))
                {
                    self.choosers.fail_tab(&tab, "Child target limit exceeded");
                    continue;
                }
                let key = (tab.clone(), target.into());
                if self
                    .frame_sessions
                    .get(&key)
                    .is_some_and(|old| old == session)
                {
                    continue;
                }
                if let Some(previous) = self.frame_sessions.get(&key).cloned() {
                    self.forget_child_session(&previous);
                }
                self.frame_sessions.insert(key, session.into());
                self.dialogs
                    .lock()
                    .unwrap()
                    .child(&tab, session, target, parent);
                self.children.parents.insert(session.into(), parent.into());
                if self.children.pending.len() < LIMIT {
                    self.children
                        .pending
                        .push_back((tab, target.into(), session.into()));
                }
                progress = true;
            }
            if !progress {
                break;
            }
        }
    }
}
impl Browser {
    pub(super) fn maintain_child_choosers(&mut self) -> Result<()> {
        if self.surface.children.draining {
            return Ok(());
        }
        self.surface.children.draining = true;
        let result = (|| -> Result<()> {
            for _ in 0..LIMIT {
                let Some((tab, target, session)) = self.surface.children.pending.pop_front() else {
                    break;
                };
                if self
                    .surface
                    .frame_sessions
                    .get(&(tab.clone(), target.clone()))
                    != Some(&session)
                {
                    continue;
                }
                if !self.surface.choosers.has_watch(&tab) {
                    continue;
                }
                let initialized = (|| -> Result<()> {
                    self.chooser_session_attached(&tab, &session)?;
                    // Flattened attachment is recursive only when each child subscribes.
                    if self
                        .surface
                        .frame_sessions
                        .get(&(tab.clone(), target.clone()))
                        == Some(&session)
                    {
                        self.call("Target.setAutoAttach",json!({"autoAttach":true,"flatten":true,"waitForDebuggerOnStart":false,"filter":[{"type":"iframe","exclude":false},{"exclude":true}]}),Some(&session))?;
                    }
                    Ok(())
                })();
                if let Err(error) = initialized {
                    self.surface.choosers.fail_tab(&tab, &error.message);
                    return Err(error);
                }
            }
            Ok(())
        })();
        self.surface.children.draining = false;
        result
    }
}
