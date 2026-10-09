//! The local, non-durable agent behind `ncl` / `nanocodex --local`.
//!
//! [`LocalBackend`] owns every runtime piece a `ConfiguredAgent` carries. Feature
//! modules take the optional pieces they drive (Claude interactions, the Claude
//! scheduler, MCP, Realtime voice, subagent updates) through [`LocalParts`] when
//! [`super::super::features::Features::attach`] runs; the driver keeps the handle,
//! the event bridge and the lifecycle resources, and shuts them down on exit.

use std::{path::PathBuf, sync::Arc};

use eyre::Result;
use nanocodex::{HarnessModel, Nanocodex, OpenAi, tools::mcp::McpHandle};

use crate::config::{AgentArgs, ConfiguredAgent, InteractionReceiver, SessionScheduler};
use crate::nanocodex2::tui::backend::Capabilities;
use crate::subagents::ChildAgents;
use crate::vm::VmArgs;

/// The optional runtime pieces feature modules may take ownership of.
#[derive(Default)]
pub(crate) struct LocalParts {
    pub(crate) claude_scheduler: Option<Arc<SessionScheduler>>,
    pub(crate) claude_interactions: Option<InteractionReceiver>,
    pub(crate) realtime: Option<OpenAi>,
    pub(crate) mcp: Option<McpHandle>,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) subagent_updates:
        Option<tokio::sync::mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>>,
}

/// How `ncl` was launched. Feature modules that rebuild the agent (harness
/// switch, /clear, local /btw, resume) start from these arguments.
#[derive(Clone)]
pub(crate) struct LocalLaunch {
    pub(crate) args: AgentArgs,
    pub(crate) vm: VmArgs,
    /// Whether the backend may still be rebuilt with a different harness.
    pub(crate) replaceable: bool,
    /// `--prompt`: submitted once, as soon as the agent is ready.
    pub(crate) initial_prompt: Option<String>,
    /// Private agent instruction for `initial_prompt`, which is then only its
    /// transcript label (`nanocodex eval benchmark` shows `/benchmark PROFILE`).
    pub(crate) initial_instruction: Option<String>,
    /// A saved Codex thread to reopen (`ncl resume`, /attach).
    pub(crate) resume: Option<super::sessions::Resume>,
}

/// A running local agent and the resources it must release on exit.
pub(crate) struct LocalBackend {
    pub(crate) launch: LocalLaunch,
    pub(crate) handle: Nanocodex,
    pub(crate) model: HarnessModel,
    pub(crate) workspace: PathBuf,
    pub(crate) parts: LocalParts,
    capabilities: Capabilities,
    /// Visible history of a resumed session, replayed once on connect.
    pub(crate) transcript: Vec<nanocodex::agent::session::TranscriptItem>,
    mpp_adapter: Option<crate::mpp::MppAdapter>,
    browser: Option<crate::browser::ConfiguredBrowser>,
    vm: Option<crate::vm::ConfiguredVm>,
    child_agents: Option<Arc<ChildAgents>>,
}

impl LocalBackend {
    /// Splits a built agent into the backend and its event stream.
    pub(crate) fn new(
        launch: LocalLaunch,
        workspace: PathBuf,
        agent: ConfiguredAgent,
    ) -> (Self, nanocodex::AgentEvents) {
        let ConfiguredAgent {
            host:
                crate::config::HostChannels {
                    interactions: claude_interactions,
                    scheduler: claude_scheduler,
                },
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter,
            mcp,
            browser,
            vm,
            model,
        } = agent;
        let mut capabilities = Capabilities::LOCAL;
        capabilities.voice_realtime = realtime.is_some();
        capabilities.claude_host = matches!(model.family(), nanocodex::HarnessFamily::Claude);
        let backend = Self {
            capabilities,
            launch,
            handle,
            model,
            workspace,
            parts: LocalParts {
                claude_scheduler,
                claude_interactions,
                realtime,
                mcp,
                child_agents: child_agents.clone(),
                subagent_updates,
            },
            transcript: Vec::new(),
            mpp_adapter,
            browser,
            vm,
            child_agents,
        };
        (backend, events)
    }

    /// Builds the local agent off the input loop, reopening a saved session if any.
    pub(crate) async fn build(launch: LocalLaunch) -> Result<(Self, nanocodex::AgentEvents)> {
        // A branch switch names a saved session of either harness. Keep the resolved
        // launch so settings, /clear and later switches follow the reopened harness.
        let launch = super::sessions::resolve(launch).await?;
        let built = super::sessions::build(&launch).await?;
        let (mut backend, events) = Self::new(launch, built.workspace, built.agent);
        backend.transcript = built.transcript;
        Ok((backend, events))
    }

    /// Feature visibility for this agent.
    pub(crate) fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    /// Releases subagents, browser, VM and MPP resources (legacy shutdown_runtime).
    pub(crate) async fn shutdown(self) -> Result<()> {
        if let Some(child_agents) = self.child_agents {
            child_agents.shutdown().await;
        }
        drop(self.handle);
        let browser = match self.browser {
            Some(browser) => browser.shutdown().await,
            None => Ok(()),
        };
        let vm = match self.vm {
            Some(vm) => vm.shutdown().await,
            None => Ok(()),
        };
        let mpp = match self.mpp_adapter {
            Some(adapter) => adapter.shutdown().await,
            None => Ok(()),
        };
        browser?;
        vm?;
        mpp
    }
}
