//! Independent IAB route authority. The route comes from trusted host configuration,
//! never from model-supplied command parameters. Renderer/window effects are explicit
//! delegate calls so an unavailable embedding shell cannot manufacture success.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteConfig {
    pub conversation_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// An independently named route window, not a vendor/native window identity.
    pub window_id: String,
}
impl RouteConfig {
    pub fn session_id(&self) -> &str {
        self.thread_id.as_deref().unwrap_or(&self.conversation_id)
    }
    pub fn validate(&self) -> Result<()> {
        for value in [&self.conversation_id, &self.window_id]
            .into_iter()
            .chain(self.thread_id.iter())
        {
            if value.is_empty() || value.len() > 256 {
                return Err(Error::invalid(
                    "IAB route identifiers must contain 1–256 bytes",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionParams {
    pub session_id: String,
    pub turn_id: String,
    pub session_context: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Viewport {
    pub width: u64,
    pub height: u64,
}
impl Viewport {
    fn parse(payload: &Value) -> Result<Self> {
        let read = |key| {
            payload[key]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::invalid(format!("{key} must be a positive integer")))
        };
        Ok(Self {
            width: read("width")?,
            height: read("height")?,
        })
    }
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.clamp(240, 4096),
            height: self.height.clamp(160, 4096),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tab {
    pub id: String,
    pub logical_id: String,
    pub active: bool,
    pub mark: Option<String>,
    pub mark_turn: Option<String>,
    /// A retained handoff already cleaned during this trusted turn.
    pub completed_turn: Option<String>,
}
/// Host callbacks distinguish stored route intent from actual renderer effects.
/// The supplied CDP host implements detached owned windows. An embedding host can
/// supply sidebar bounds/visibility without changing authority or fallback order.
pub trait Host {
    fn ensure_available(&mut self) -> Result<()>;
    fn record_turn(&mut self, params: &SessionParams) -> Result<()>;
    fn active(&mut self, value: bool, tab: &str) -> Result<()>;
    fn viewport(&mut self, value: Option<Viewport>, tab: Option<&str>) -> Result<()>;
    fn wait_sync(&mut self, tab: Option<&str>) -> Result<()>;
    fn visible(&mut self, value: bool, tab: Option<&str>) -> Result<()>;
    fn is_visible(&mut self, tab: Option<&str>) -> Result<bool>;
    fn open(&mut self, logical_id: &str, url: &str) -> Result<String>;
    fn close(&mut self, logical_id: &str) -> Result<()>;
    fn unfinished_target(&self, logical_id: &str) -> Option<String>;
    fn cleanup(&mut self, logical_id: &str) -> Result<()>;
    fn release(&mut self, logical_id: &str) -> Result<()>;
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Authority {
    pub route: RouteConfig,
    cached: Option<SessionParams>,
    retired: BTreeSet<String>,
    route_available: bool,
    pub tabs: BTreeMap<String, Tab>,
    selected: Option<String>,
    pending_viewport: Option<Viewport>,
    pending_show: bool,
    pub generation: u64,
}
impl Authority {
    pub fn new(route: RouteConfig) -> Result<Self> {
        route.validate()?;
        Ok(Self {
            route,
            cached: None,
            retired: BTreeSet::new(),
            route_available: true,
            tabs: BTreeMap::new(),
            selected: None,
            pending_viewport: None,
            pending_show: false,
            generation: 0,
        })
    }
    /// Mirrors Gm live/cached metadata selection; stricter retired-turn rejection
    /// is an independent authority rule, not attributed to captured Electron code.
    pub fn set_context(&mut self, metadata: Option<&Value>) -> Result<SessionParams> {
        let Some(metadata) = metadata else {
            let mut cached = self
                .cached
                .clone()
                .ok_or_else(|| Error::action("Missing required browser session_id"))?;
            cached.session_context = "cached".into();
            self.ensure_route()?;
            self.cached = Some(cached.clone());
            return Ok(cached);
        };
        let session = if metadata["thread_source"] == "subagent" {
            metadata["thread_id"]
                .as_str()
                .or_else(|| metadata["session_id"].as_str())
        } else {
            metadata["session_id"].as_str()
        }
        .ok_or_else(|| Error::action("Missing required browser session_id"))?;
        let turn = metadata["turn_id"]
            .as_str()
            .ok_or_else(|| Error::action("Missing required browser turn_id"))?;
        if session != self.route.session_id() {
            return Err(Error::action(format!(
                "No ChatGPT browser route is available for browser session {session}"
            )));
        }
        if turn.is_empty() || turn.len() > 256 {
            return Err(Error::invalid(
                "IAB turn identifiers must contain 1–256 bytes",
            ));
        }
        self.ensure_route()?;
        if self.retired.contains(turn) {
            return Err(Error::action("IAB browser turn is stale"));
        }
        if self
            .cached
            .as_ref()
            .is_some_and(|previous| previous.turn_id != turn)
        {
            if self.retired.len() >= 10000 {
                return Err(Error::action("IAB turn history limit exceeded"));
            }
            self.retired
                .insert(self.cached.as_ref().unwrap().turn_id.clone());
            self.generation += 1;
        }
        let current = SessionParams {
            session_id: session.into(),
            turn_id: turn.into(),
            session_context: "live".into(),
        };
        self.cached = Some(current.clone());
        Ok(current)
    }
    pub fn context(&self) -> Result<&SessionParams> {
        self.ensure_route()?;
        self.cached
            .as_ref()
            .ok_or_else(|| Error::action("Missing required browser turn_id"))
    }
    pub fn set_route_available(&mut self, available: bool) {
        if self.route_available != available {
            self.generation += 1;
        }
        self.route_available = available;
    }
    fn ensure_route(&self) -> Result<()> {
        if !self.route_available {
            return Err(Error::action(format!(
                "No ChatGPT browser route is available for browser session {}",
                self.route.session_id()
            )));
        }
        Ok(())
    }
    pub fn require_tab(&self, tab: &str) -> Result<&Tab> {
        self.context()?;
        self.tabs.get(tab).ok_or_else(|| {
            Error::action(format!(
                "Tab {tab} is not controlled by this IAB browser route"
            ))
        })
    }
    pub fn selected(&self) -> Option<&Tab> {
        self.selected
            .as_ref()
            .and_then(|id| self.tabs.get(id))
            .or_else(|| self.tabs.values().find(|tab| tab.active))
            .or_else(|| self.tabs.values().next())
    }
    pub fn select(&mut self, tab: &str) -> Result<()> {
        self.require_tab(tab)?;
        self.selected = Some(tab.into());
        self.generation += 1;
        Ok(())
    }
    pub fn remove(&mut self, tab: &str) {
        if self.tabs.remove(tab).is_some() {
            self.generation += 1;
        }
        if self.selected.as_deref() == Some(tab) {
            self.selected = None;
        }
    }
    pub fn mark(&mut self, tab: &str, mark: &str) -> Result<()> {
        self.require_tab(tab)?;
        if !["deliverable", "handoff"].contains(&mark) {
            return Err(Error::invalid("Invalid tab mark"));
        }
        let turn = self.context()?.turn_id.clone();
        let record = self.tabs.get_mut(tab).unwrap();
        record.mark = Some(mark.into());
        record.mark_turn = Some(turn);
        record.completed_turn = None;
        Ok(())
    }
    pub fn pending(&self) -> Value {
        json!({"viewport":self.pending_viewport,"show":self.pending_show})
    }
    pub fn fallback(&mut self, host: &mut impl Host, payload: &Value) -> Result<Value> {
        let context = self.context()?.clone();
        let command = payload["type"]
            .as_str()
            .ok_or_else(|| Error::invalid("Missing IAB fallback command type"))?;
        if command == "tabs_content" {
            return Err(Error::action(
                "browser.tabs.content is not supported in Codex in-app browser. Open tabs with browser.tabs.new(), navigate them, then read each tab directly.",
            ));
        }
        host.ensure_available()?;
        let tab = self.selected().map(|tab| tab.logical_id.clone());
        if tab.is_none() {
            match command {
                "browser_visibility_set" => {
                    self.pending_show = payload["visible"]
                        .as_bool()
                        .ok_or_else(|| Error::invalid("visible must be a boolean"))?;
                    return Ok(json!({}));
                }
                "browser_viewport_set" => {
                    self.pending_viewport = Some(Viewport::parse(payload)?);
                    return Ok(json!({}));
                }
                "browser_viewport_reset" => {
                    self.pending_viewport = None;
                    return Ok(json!({}));
                }
                _ => {}
            }
        }
        host.record_turn(&context)?;
        match command {
            "browser_visibility_set" => {
                let visible = payload["visible"]
                    .as_bool()
                    .ok_or_else(|| Error::invalid("visible must be a boolean"))?;
                self.set_visible(host, visible, tab.as_deref())?;
                Ok(json!({}))
            }
            "browser_visibility_get" => Ok(json!({"visible":host.is_visible(tab.as_deref())?})),
            "browser_viewport_set" | "browser_viewport_reset" => {
                let viewport = if command.ends_with("reset") {
                    None
                } else {
                    Some(Viewport::parse(payload)?)
                };
                host.viewport(viewport.map(Viewport::clamped), tab.as_deref())?;
                host.wait_sync(tab.as_deref())?;
                Ok(json!({}))
            }
            _ => Err(Error::action(format!(
                "Codex in-app browser does not support command \"{command}\"."
            ))),
        }
    }
    fn set_visible(&self, host: &mut impl Host, visible: bool, tab: Option<&str>) -> Result<()> {
        if self.route.thread_id.is_some() {
            return Err(Error::action(
                "IAB visibility is not supported in a subagent thread",
            ));
        }
        host.visible(visible, tab)
    }
    pub fn create(&mut self, host: &mut impl Host, url: &str) -> Result<String> {
        let context = self.context()?.clone();
        host.ensure_available()?;
        if self.tabs.len() >= 10000 {
            return Err(Error::action("IAB tab limit exceeded"));
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| Error::action("Cannot allocate IAB tab identity"))?;
        let logical = format!(
            "skyre-iab:{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let preparation = (|| {
            host.active(true, &logical)?;
            // Consumption precedes effects, including failure. Negative intents
            // cancel pending positives instead of scheduling hide/reset work.
            if let Some(viewport) = self.pending_viewport.take() {
                host.viewport(Some(viewport.clamped()), Some(&logical))?;
            }
            if self.pending_show {
                self.pending_show = false;
                self.set_visible(host, true, Some(&logical))?;
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            let _ = host.active(false, &logical);
            let _ = host.close(&logical);
            return Err(error);
        }
        // Original open-await failure does not close an unresolved asynchronous
        // page. Our synchronous host reports failure without claiming rollback.
        let id = match host.open(&logical, url) {
            Ok(id) => id,
            Err(error) => {
                // A known renderer whose cleanup failed remains owned and
                // visible to the next cleanup attempt; never lose its handle.
                if let Some(id) = host.unfinished_target(&logical) {
                    self.tabs.insert(
                        id.clone(),
                        Tab {
                            id,
                            logical_id: logical.clone(),
                            active: true,
                            mark: None,
                            mark_turn: None,
                            completed_turn: None,
                        },
                    );
                }
                if host.unfinished_target(&logical).is_none() {
                    let _ = host.active(false, &logical);
                    let _ = host.close(&logical);
                }
                return Err(error);
            }
        };
        self.tabs.insert(
            id.clone(),
            Tab {
                id: id.clone(),
                logical_id: logical.clone(),
                active: true,
                mark: None,
                mark_turn: None,
                completed_turn: None,
            },
        );
        if let Err(error) = host.record_turn(&context) {
            let _ = host.active(false, &logical);
            if host.close(&logical).is_ok() {
                self.remove(&id);
            }
            return Err(error);
        }
        self.selected = Some(id.clone());
        self.generation += 1;
        Ok(id)
    }
    /// Trusted host scope completion, not an agent-command metadata override.
    /// Per-tab failures retain authority and are reported; later tabs still run.
    pub fn finish(&mut self, host: &mut impl Host) -> Vec<Error> {
        let turn = self.cached.as_ref().map(|context| context.turn_id.clone());
        let tabs: Vec<_> = self.tabs.values().cloned().collect();
        let mut errors = vec![];
        for tab in tabs {
            if turn.is_some() && tab.completed_turn == turn {
                continue;
            }
            let result = (|| {
                host.cleanup(&tab.logical_id)?;
                host.active(false, &tab.logical_id)?;
                let mark = if tab.mark_turn == turn {
                    tab.mark.as_deref()
                } else {
                    None
                };
                match mark {
                    Some("handoff") => {
                        let tab = self.tabs.get_mut(&tab.id).unwrap();
                        tab.mark = None;
                        tab.mark_turn = None;
                        tab.active = false;
                        tab.completed_turn = turn.clone();
                    }
                    Some("deliverable") => {
                        host.release(&tab.logical_id)?;
                        self.remove(&tab.id);
                    }
                    _ => {
                        host.close(&tab.logical_id)?;
                        self.remove(&tab.id);
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                errors.push(error);
            }
        }
        self.pending_show = false;
        self.pending_viewport = None;
        self.invalidate_inputs();
        errors
    }
    pub fn invalidate_inputs(&mut self) {
        self.generation += 1;
    }
}
