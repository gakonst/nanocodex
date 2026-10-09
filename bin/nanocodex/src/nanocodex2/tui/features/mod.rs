//! Feature extension points of the unified TUI.
//!
//! Every local feature lives in exactly one file under
//! this directory and implements [`Feature`]. The driver (`super::run_inner`)
//! owns one [`Features`] set and calls it at four fixed points:
//!
//! 1. [`Features::attach`] once a local agent is built (and again after it
//!    is rebuilt): features take the runtime pieces they drive from
//!    [`LocalParts`] and spawn their tasks.
//! 2. [`Features::command`] for a parsed [`FeatureCommand`] (see `super::commands`).
//! 3. [`Features::key`] before default key handling while no overlay is open.
//! 4. [`Features::turn_state`] whenever the main agent becomes idle or busy.
//!
//! Features never touch driver state directly. They report through
//! [`FeatureHost`], whose [`FeatureUpdate`]s the driver applies in one select! arm:
//! notices, prompt submissions, transcript records, modal overlays, agent
//! replacement and side panes. Adding a feature therefore never edits the
//! driver, the root component, the composer, the keybindings or the actions
//! menu.

use std::{path::PathBuf, sync::Arc};

use crossterm::event::KeyEvent;
use nanocodex::Nanocodex;
use ratatui::{Frame, layout::Rect};
use tokio::sync::{mpsc, oneshot};

use super::{
    backend::Capabilities,
    local::agent::{LocalBackend, LocalLaunch, LocalParts},
    pane::PaneId,
    theme::Theme,
    transcript::LocalEvent,
};

pub(crate) mod benchmark;
pub(crate) mod branches;
pub(crate) mod btw_local;
pub(crate) mod claude_interaction;
pub(crate) mod claude_scheduler;
pub(crate) mod harness;
pub(crate) mod mcp;
pub(crate) mod realtime_voice;
pub(crate) mod split;
pub(crate) mod subagents;
// Terminal detection and launch commands for /split.
mod split_launch;

/// Slash commands whose behaviour belongs to a feature module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FeatureCommand {
    /// `/mcp login <server>`
    McpLogin(String),
    /// `/mcp reload [server]`
    McpReload(Option<String>),
    /// `/benchmark <args>`
    Benchmark(String),
    /// `/btw <prompt>` against the local agent.
    Btw(String),
    /// `/collapse`: fold the open local /btw thread into main.
    Collapse,
    /// `/split`: open the local /btw thread in a tmux/zellij pane.
    Split,
    /// `/close` the local /btw pane.
    CloseBtw,
    /// `/branches` or Ctrl+Alt+B: open the branch navigator.
    Branches,
    /// Local Realtime `/voice ...` with its argument string.
    RealtimeVoice(String),
    /// A `/model` selection that may cross harness families.
    SwitchModel(String),
}

/// What the driver should do after a feature saw a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyOutcome {
    Ignored,
    Consumed,
}

/// What a modal feature overlay wants after a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OverlayOutcome {
    /// The overlay handled the key and stays open.
    Consumed,
    /// The overlay is finished; the driver closes it.
    Close,
}

/// A modal surface owned by a feature (Claude interaction, branch navigator).
pub(crate) trait FeatureOverlay: Send {
    /// Stable name for diagnostics and tests.
    fn name(&self) -> &'static str;
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme);
    fn key(&mut self, key: KeyEvent) -> OverlayOutcome;
    /// Bracketed paste while open; ignored by default.
    fn paste(&mut self, _text: &str) {}
    /// Whether opening this overlay discards the main composer draft.
    fn discards_draft(&self) -> bool {
        false
    }
}

/// A prompt a feature submits on the user's behalf (scheduler fire, /benchmark).
pub(crate) struct FeaturePrompt {
    pub(crate) pane: Option<PaneId>,
    /// Shown in the transcript as the user turn.
    pub(crate) display: String,
    /// Sent to the agent instead of display when present (private workflow text).
    pub(crate) instruction: Option<String>,
    /// Resolved true when the turn completes, false when it fails or is
    /// cancelled; dropped when the submission is rejected.
    pub(crate) completion: Option<oneshot::Sender<bool>>,
}

/// A request from a feature task to the driver.
pub(crate) enum FeatureUpdate {
    /// Informational footer/transcript notice.
    Notice {
        pane: Option<PaneId>,
        message: String,
    },
    /// Error notice (the same rendering as NotifyError).
    Error {
        pane: Option<PaneId>,
        message: String,
    },
    /// Submit a prompt as if typed (scheduler fires, interaction follow-ups).
    Submit { pane: Option<PaneId>, text: String },
    /// Append a local transcript record.
    Record {
        pane: Option<PaneId>,
        event: LocalEvent,
    },
    /// Show a modal overlay; replaces any open feature overlay.
    OpenOverlay(Box<dyn FeatureOverlay>),
    /// Close the open feature overlay.
    CloseOverlay,
    /// Close a side pane.
    ClosePane(PaneId),
    /// Steer text into the running main turn exactly as a typed steer would be
    /// (local /collapse while main is busy).
    Steer {
        pane: Option<PaneId>,
        /// Shown in the steer queue and transcript.
        display: String,
        /// Sent to the agent instead of display when present (private workflow text).
        instruction: Option<String>,
    },
    /// Re-read capabilities (a feature became available or unavailable).
    Capabilities(Capabilities),
    /// Submit a prompt with an optional private instruction and completion receipt.
    SubmitPrompt(FeaturePrompt),
    /// Rebuild the local agent from new launch arguments (harness/model switch
    /// before the first prompt); on failure the current agent stays.
    Relaunch(Box<LocalLaunch>),
    /// Footer voice status of local Realtime voice; None hides it.
    VoiceStatus(Option<crate::nanocodex2::voice_state::Status>),
    /// A local child-agent update for the shared subagent tree.
    Subagent(nanocodex_subagents::AgentUpdate),
}

/// The channel features report through. Cheap to clone into tasks.
#[derive(Clone)]
pub(crate) struct FeatureHost {
    sender: mpsc::UnboundedSender<FeatureUpdate>,
}

impl FeatureHost {
    pub(crate) fn send(&self, update: FeatureUpdate) {
        let _ = self.sender.send(update);
    }

    pub(crate) fn notice(&self, pane: Option<PaneId>, message: impl Into<String>) {
        self.send(FeatureUpdate::Notice {
            pane,
            message: message.into(),
        });
    }

    pub(crate) fn error(&self, pane: Option<PaneId>, message: impl Into<String>) {
        self.send(FeatureUpdate::Error {
            pane,
            message: message.into(),
        });
    }
}

/// Read-only driver state passed to synchronous feature callbacks.
pub(crate) struct FeatureContext<'a> {
    pub(crate) host: &'a FeatureHost,
    pub(crate) capabilities: Capabilities,
    pub(crate) workspace: &'a std::path::Path,
    /// The current local agent; None in managed mode or while connecting.
    pub(crate) agent: Option<&'a Nanocodex>,
    pub(crate) launch: Option<&'a LocalLaunch>,
    /// Whether the main agent is running a turn.
    pub(crate) busy: bool,
    /// Whether any prompt has been submitted in this session.
    pub(crate) prompted: bool,
}

/// One feature module.
pub(crate) trait Feature: Send {
    fn name(&self) -> &'static str;
    /// Take runtime pieces from a freshly built local agent and start tasks.
    fn attach(&mut self, _parts: &mut LocalParts, _cx: &FeatureContext<'_>) {}
    /// Handle a command; return false when it belongs to another feature.
    fn command(
        &mut self,
        _pane: PaneId,
        _command: &FeatureCommand,
        _cx: &FeatureContext<'_>,
    ) -> bool {
        false
    }
    fn key(&mut self, _key: &KeyEvent, _cx: &FeatureContext<'_>) -> KeyOutcome {
        KeyOutcome::Ignored
    }
    /// The main agent became busy (true) or idle (false).
    fn turn_state(&mut self, _busy: bool, _cx: &FeatureContext<'_>) {}
    /// A feature overlay is open (true) or closed (false); schedulers pause.
    fn overlay_state(&mut self, _open: bool) {}
    /// Called at most once per second while the main agent is idle, no feature
    /// overlay is open, the composer is empty and nothing is pending.
    fn idle_tick(&mut self, _cx: &FeatureContext<'_>) {}
    /// A prompt the user typed is about to be submitted to the local agent.
    /// A returned sender is resolved with that turn's outcome; Err rejects it.
    fn user_prompt(
        &mut self,
        _text: &str,
        _cx: &FeatureContext<'_>,
    ) -> Result<Option<oneshot::Sender<bool>>, String> {
        Ok(None)
    }
    /// Stop tasks before the agent is dropped or replaced.
    fn shutdown(&mut self) {}
}

/// Every feature of one TUI session plus their shared update channel.
pub(crate) struct Features {
    host: FeatureHost,
    updates: Option<mpsc::UnboundedReceiver<FeatureUpdate>>,
    features: Vec<Box<dyn Feature>>,
}

impl Features {
    pub(crate) fn new() -> Self {
        let (sender, updates) = mpsc::unbounded_channel();
        Self {
            host: FeatureHost { sender },
            updates: Some(updates),
            features: vec![
                Box::new(claude_interaction::ClaudeInteraction::default()),
                Box::new(claude_scheduler::ClaudeScheduler::default()),
                Box::new(harness::Harness),
                Box::new(branches::Branches::default()),
                Box::new(btw_local::LocalBtw),
                Box::new(split::Split),
                Box::new(mcp::Mcp::default()),
                Box::new(realtime_voice::RealtimeVoice::default()),
                Box::new(benchmark::Benchmark),
                Box::new(subagents::Subagents::default()),
            ],
        }
    }

    pub(crate) fn host(&self) -> &FeatureHost {
        &self.host
    }

    pub(crate) fn attach(&mut self, backend: &mut LocalBackend, cx_base: ContextBase<'_>) {
        let LocalBackend {
            handle,
            parts,
            launch,
            workspace,
            ..
        } = backend;
        let cx = FeatureContext {
            host: &self.host,
            capabilities: cx_base.capabilities,
            workspace,
            agent: Some(handle),
            launch: Some(launch),
            busy: cx_base.busy,
            prompted: cx_base.prompted,
        };
        for feature in &mut self.features {
            feature.attach(parts, &cx);
        }
    }

    /// Routes a command; returns false when no feature handled it.
    pub(crate) fn command(
        &mut self,
        pane: PaneId,
        command: &FeatureCommand,
        cx: &FeatureContext<'_>,
    ) -> bool {
        self.features
            .iter_mut()
            .any(|feature| feature.command(pane, command, cx))
    }

    pub(crate) fn key(&mut self, key: &KeyEvent, cx: &FeatureContext<'_>) -> KeyOutcome {
        for feature in &mut self.features {
            if feature.key(key, cx) == KeyOutcome::Consumed {
                return KeyOutcome::Consumed;
            }
        }
        KeyOutcome::Ignored
    }

    pub(crate) fn turn_state(&mut self, busy: bool, cx: &FeatureContext<'_>) {
        for feature in &mut self.features {
            feature.turn_state(busy, cx);
        }
    }

    pub(crate) fn idle_tick(&mut self, cx: &FeatureContext<'_>) {
        for feature in &mut self.features {
            feature.idle_tick(cx);
        }
    }

    /// Completion receipts of every feature tracking this typed prompt.
    pub(crate) fn user_prompt(
        &mut self,
        text: &str,
        cx: &FeatureContext<'_>,
    ) -> Result<Vec<oneshot::Sender<bool>>, String> {
        let mut completions = Vec::new();
        for feature in &mut self.features {
            completions.extend(feature.user_prompt(text, cx)?);
        }
        Ok(completions)
    }

    pub(crate) fn overlay_state(&mut self, open: bool) {
        for feature in &mut self.features {
            feature.overlay_state(open);
        }
    }

    pub(crate) fn shutdown(&mut self) {
        for feature in &mut self.features {
            feature.shutdown();
        }
    }

    /// Hands the update receiver to the driver's select loop (once).
    pub(crate) fn take_updates(&mut self) -> Option<mpsc::UnboundedReceiver<FeatureUpdate>> {
        self.updates.take()
    }
}

/// Driver state copied into [`FeatureContext`] by [`Features::attach`].
#[derive(Clone, Copy)]
pub(crate) struct ContextBase<'a> {
    pub(crate) capabilities: Capabilities,
    pub(crate) busy: bool,
    pub(crate) prompted: bool,
    pub(crate) _marker: std::marker::PhantomData<&'a ()>,
}

#[allow(
    dead_code,
    reason = "kept for feature modules that persist per-workspace state"
)]
pub(crate) fn feature_state_dir(workspace: &std::path::Path) -> PathBuf {
    workspace.join(".nanocodex")
}

#[allow(dead_code, reason = "shared by feature modules")]
pub(crate) type SharedAgent = Arc<Nanocodex>;
