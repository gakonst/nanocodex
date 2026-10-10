//! Branches of a local conversation: Ctrl+Alt+B (or /branches) opens the
//! navigator, editing an earlier prompt starts a new branch from the turns
//! before it, and switching reopens another branch in place. Ctrl+Alt+Up/Down
//! cycles branches without the navigator.
//!
//! Branches are durable Codex threads: an edit copies the current
//! rollout through the completed turns before the edited prompt
//! (`rollout_fork`), so every branch can also be resumed later with `ncl resume`.

use std::sync::{Arc, Mutex, PoisonError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Feature, FeatureCommand, FeatureContext, FeatureUpdate, KeyOutcome, btw_local};
use crate::nanocodex2::tui::{
    components::BranchNavigator,
    local::{
        agent::{LocalLaunch, LocalParts},
        sessions,
    },
    pane::PaneId,
};

/// One branch of the current conversation family.
#[derive(Clone, Debug)]
pub(crate) struct Branch {
    pub(crate) thread: String,
    pub(crate) parent: Option<String>,
    pub(crate) label: String,
}

/// Branches known to this TUI session.
#[derive(Default)]
pub(crate) struct Registry {
    pub(crate) branches: Vec<Branch>,
    pub(crate) current: Option<String>,
    /// (parent, label) of a fresh branch whose thread id is known only after it connects.
    pub(crate) pending: Option<(Option<String>, String)>,
}

impl Registry {
    fn adopt(&mut self, thread: String) {
        if let Some((parent, label)) = self.pending.take() {
            if !self.branches.iter().any(|branch| branch.thread == thread) {
                self.branches.push(Branch {
                    thread: thread.clone(),
                    parent,
                    label,
                });
            }
        } else if !self.branches.iter().any(|branch| branch.thread == thread) {
            // A different conversation (/attach, /clear, startup) starts a new family.
            self.branches = vec![Branch {
                thread: thread.clone(),
                parent: None,
                label: "main".to_owned(),
            }];
        }
        self.current = Some(thread);
    }
}

pub(crate) type SharedRegistry = Arc<Mutex<Registry>>;

#[derive(Default)]
pub(crate) struct Branches {
    registry: SharedRegistry,
}

impl Feature for Branches {
    fn name(&self) -> &'static str {
        "branches"
    }

    fn attach(&mut self, _parts: &mut LocalParts, cx: &FeatureContext<'_>) {
        if let Some(agent) = cx.agent {
            let thread = agent
                .persistence()
                .and_then(|persistence| persistence.rollout)
                .map_or_else(
                    || agent.session_id().to_owned(),
                    |rollout| rollout.thread_id().to_owned(),
                );
            self.registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .adopt(thread);
        }
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        if !matches!(command, FeatureCommand::Branches) {
            return false;
        }
        self.open(pane, cx);
        true
    }

    fn key(&mut self, key: &KeyEvent, cx: &FeatureContext<'_>) -> KeyOutcome {
        if !key
            .modifiers
            .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return KeyOutcome::Ignored;
        }
        match key.code {
            KeyCode::Char('b' | 'B') => {
                self.open(PaneId::Main, cx);
                KeyOutcome::Consumed
            }
            KeyCode::Up => {
                self.cycle(-1, cx);
                KeyOutcome::Consumed
            }
            KeyCode::Down => {
                self.cycle(1, cx);
                KeyOutcome::Consumed
            }
            _ => KeyOutcome::Ignored,
        }
    }
}

impl Branches {
    fn open(&self, pane: PaneId, cx: &FeatureContext<'_>) {
        let prepared = (|| -> Result<_, String> {
            let launch = ready(cx)?.clone();
            let agent = cx.agent.ok_or("wait for the local agent to connect")?;
            BRANCH_HOST.set(cx.host.clone());
            // Edits branch the session through its rollout mirror, for either harness.
            let rollout = agent
                .persistence()
                .and_then(|persistence| persistence.rollout)
                .map(|rollout| (rollout.thread_id().to_owned(), rollout.path().to_path_buf()));
            Ok((launch, rollout))
        })();
        let (launch, rollout) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                cx.host.error(Some(pane), error);
                return;
            }
        };
        let registry = Arc::clone(&self.registry);
        let host = cx.host.clone();
        let workspace = cx.workspace.to_path_buf();
        // Prompts of the current session, Codex or Claude, from the session catalog.
        let session = rollout
            .as_ref()
            .map(|(thread, _)| thread.clone())
            .or_else(|| cx.agent.map(|agent| agent.session_id().to_owned()));
        // Reading the transcript touches the disk; keep it off the input loop.
        tokio::spawn(async move {
            let prompts = tokio::task::spawn_blocking(move || {
                session
                    .as_deref()
                    .map(sessions::prompts)
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            let navigator =
                BranchNavigator::new(registry, host.clone(), launch, workspace, rollout, prompts);
            host.send(FeatureUpdate::OpenOverlay(Box::new(navigator)));
        });
    }

    fn cycle(&self, direction: isize, cx: &FeatureContext<'_>) {
        let target = {
            let registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            let count = registry.branches.len();
            if count < 2 {
                cx.host.error(
                    None,
                    "This conversation has no other branch; edit an earlier prompt with Ctrl+Alt+B",
                );
                return;
            }
            let position = registry
                .current
                .as_ref()
                .and_then(|current| {
                    registry
                        .branches
                        .iter()
                        .position(|branch| &branch.thread == current)
                })
                .unwrap_or(0);
            let next = position
                .cast_signed()
                .saturating_add(direction)
                .rem_euclid(count.cast_signed());
            registry.branches[next.cast_unsigned()].thread.clone()
        };
        match ready(cx) {
            Ok(launch) => {
                BRANCH_HOST.set(cx.host.clone());
                switch(launch, &target);
            }
            Err(error) => cx.host.error(None, error),
        }
    }
}

/// Branch switches and edits replace the main agent; legacy required the same.
fn ready<'a>(cx: &'a FeatureContext<'_>) -> Result<&'a LocalLaunch, String> {
    if cx.busy {
        return Err("finish or interrupt the main turn before switching branches".to_owned());
    }
    if btw_local::active().is_some() {
        return Err("close /btw before editing history or switching branches".to_owned());
    }
    cx.launch
        .ok_or_else(|| "branches need a local agent (run ncl)".to_owned())
}

/// Reopens the branch `thread` in place without disk access here; the connection
/// task finds the Codex thread or Claude session and loads it.
pub(crate) fn switch(launch: &LocalLaunch, thread: &str) {
    // Branches continue in the harness that recorded them; the connection task
    // resolves it.
    let launch = sessions::session_launch(launch, sessions::Resume::Session(thread.to_owned()));
    BRANCH_HOST.with_host(|host| host.send(FeatureUpdate::Relaunch(Box::new(launch))));
}

/// Starts a branch whose history ends before prompt `index` (0-based) and
/// submits `prompt` there. The rollout copy happens in the connection task.
pub(crate) fn edit(
    registry: &SharedRegistry,
    launch: &LocalLaunch,
    workspace: &std::path::Path,
    rollout: Option<(&str, &std::path::Path)>,
    index: usize,
    prompt: String,
) -> Result<LocalLaunch, String> {
    let label = branch_label(&prompt, index);
    let branch = if index == 0 {
        // Nothing precedes the first prompt: the branch is a fresh session.
        let mut fresh = sessions::fresh(launch);
        fresh.replaceable = false;
        fresh.resume = Some(sessions::Resume::Branch {
            thread: None,
            fork: None,
            prompt,
        });
        fresh
    } else {
        let (session, source) = rollout
            .ok_or("editing history needs a saved session rollout; this session has none")?;
        sessions::session_launch(
            launch,
            sessions::Resume::Branch {
                thread: None,
                fork: Some(sessions::Fork {
                    session: session.to_owned(),
                    source: source.to_path_buf(),
                    turns: index,
                    workspace: workspace.to_path_buf(),
                }),
                prompt,
            },
        )
    };
    let mut registry = registry.lock().unwrap_or_else(PoisonError::into_inner);
    let parent = registry.current.clone();
    // The new thread id is known once the branch connects (Feature::attach).
    registry.pending = Some((parent, label));
    Ok(branch)
}

fn branch_label(prompt: &str, index: usize) -> String {
    let first = prompt.lines().next().unwrap_or_default().trim();
    let mut label = first.chars().take(40).collect::<String>();
    if first.chars().count() > 40 {
        label.push_str("...");
    }
    format!("{label} (edit of prompt {})", index + 1)
}

/// The host of the session that opened the navigator, for branch switches.
struct HostCell(Mutex<Option<super::FeatureHost>>);

impl HostCell {
    fn set(&self, host: super::FeatureHost) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(host);
    }

    fn with_host(&self, send: impl FnOnce(&super::FeatureHost)) {
        if let Some(host) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            send(host);
        }
    }
}

static BRANCH_HOST: HostCell = HostCell(Mutex::new(None));
