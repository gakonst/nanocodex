//! Native user interaction. Answers only arrive from the terminal owner; model
//! inputs cannot approve a plan. The guard is also installed without a UI so a
//! resumed planning session cannot escape its restrictions in headless mode.
use super::permissions::{Decision, Policy, WorkspaceResolver};
use super::*;
use nanocodex::claude::{
    ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation,
};
use nanocodex::claude_tools::host::UserQuestion;
use nanocodex::claude_tools::{
    ClaudeHost, ClaudeHostTools, HostContext, HostRequest, HostTool, ToolOutput,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Write},
    sync::Mutex,
};
use tokio::sync::{mpsc, oneshot};

pub(crate) type InteractionReceiver = mpsc::Receiver<PendingInteraction>;

pub(crate) use crate::tui::interaction::PendingInteraction;

#[derive(Clone, Default, Serialize, Deserialize)]
struct PlanState {
    planning: bool,
    #[serde(default)]
    policy: Policy,
}

/// Preserve the current host restrictions before publishing a rewound session.
/// Conversation checkpoints do not contain permission or planning state, so
/// looking up the new UUID must never silently broaden the source policy.
pub(super) fn prepare_rewind_branch(home: &Path, source: &str, target: &str) -> eyre::Result<()> {
    use eyre::WrapErr as _;
    let directory = home.join("claude/plan-mode");
    let source_path = directory.join(format!("{}.json", hex::encode(source.as_bytes())));
    let target_path = directory.join(format!("{}.json", hex::encode(target.as_bytes())));
    let state: PlanState = match std::fs::read(&source_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .wrap_err("cannot read source permission and planning state for rewind")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PlanState::default(),
        Err(error) => {
            return Err(error)
                .wrap_err("cannot read source permission and planning state for rewind");
        }
    };
    state
        .policy
        .validate()
        .wrap_err("invalid source permission policy for rewind")?;
    std::fs::create_dir_all(&directory)?;
    let temporary = target_path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> eyre::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&state)?)?;
        file.sync_all()?;
        // A hard link publishes the complete state atomically without replacing
        // restrictions belonging to an existing target session.
        std::fs::hard_link(&temporary, &target_path)
            .wrap_err("cannot publish rewind permission and planning state")?;
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

pub(super) struct Interaction {
    sender: Option<mpsc::Sender<PendingInteraction>>,
    state_dir: PathBuf,
    states: Mutex<BTreeMap<String, PlanState>>,
    hooks: Option<Arc<dyn ClaudeToolHooks>>,
    policy: Policy,
    workspace: WorkspaceResolver,
}
impl Interaction {
    pub(super) fn available(tui: bool) -> bool {
        tui || (std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
    }
    pub(super) fn new_with_policy_and_workspace(
        available: bool,
        state_dir: PathBuf,
        hooks: Option<Arc<dyn ClaudeToolHooks>>,
        policy: Policy,
        workspace: WorkspaceResolver,
    ) -> (Arc<Self>, Option<InteractionReceiver>) {
        let (sender, receiver) = mpsc::channel(8);
        (
            Arc::new(Self {
                sender: available.then_some(sender),
                state_dir,
                states: Mutex::new(BTreeMap::new()),
                hooks,
                policy,
                workspace,
            }),
            available.then_some(receiver),
        )
    }
    fn state<T>(
        &self,
        session: &str,
        update: impl FnOnce(&mut PlanState) -> Result<T, String>,
    ) -> Result<T, String> {
        // Session IDs are opaque; encode rather than accepting path components.
        let path = self
            .state_dir
            .join(format!("{}.json", hex::encode(session.as_bytes())));
        let mut states = self.states.lock().map_err(|_| "plan state lock poisoned")?;
        let first_access = !states.contains_key(session);
        let state = if first_access {
            let state: PlanState = match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| format!("cannot read saved plan state: {e}"))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => PlanState::default(),
                Err(e) => return Err(e.to_string()),
            };
            state
                .policy
                .validate()
                .map_err(|e| format!("invalid saved permission policy: {e}"))?;
            state
        } else {
            states.get(session).expect("cached state").clone()
        };
        let before = serde_json::to_vec(&state).map_err(|e| e.to_string())?;
        let mut proposed = state;
        if first_access && self.policy.explicit {
            if self.policy.replace_rules {
                proposed.policy = self.policy.clone();
            } else {
                proposed.policy.mode = self.policy.mode.clone();
            }
            // Explicitly selecting a mode takes precedence over a saved mode.
            proposed.planning = self.policy.planning();
        }
        let result = update(&mut proposed)?;
        let after = serde_json::to_vec(&proposed).map_err(|e| e.to_string())?;
        if before != after || (first_access && self.policy.explicit) {
            std::fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
            let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&after).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        }
        // Never cache default/partially-applied state after persistence failure:
        // the next dispatch must retry the same explicit restrictive policy.
        states.insert(session.to_owned(), proposed);
        Ok(result)
    }
    /// Called before a child gets tools. New children inherit the parent's
    /// effective policy and planning state; restored children keep their saved
    /// state. Loading the child into the cache also avoids reapplying root flags.
    pub(super) fn initialize_child(&self, parent: &str, child: &str) -> Result<(), String> {
        let inherited = self.state(parent, |state| Ok(state.clone()))?;
        let path = self
            .state_dir
            .join(format!("{}.json", hex::encode(child.as_bytes())));
        let mut states = self.states.lock().map_err(|_| "plan state lock poisoned")?;
        if states.contains_key(child) {
            return Ok(());
        }
        let state = match std::fs::read(&path) {
            Ok(bytes) => {
                let saved: PlanState = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("cannot read saved child plan state: {e}"))?;
                saved
                    .policy
                    .validate()
                    .map_err(|e| format!("invalid saved child permission policy: {e}"))?;
                saved
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
                let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
                let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
                file.write_all(&serde_json::to_vec(&inherited).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
                inherited
            }
            Err(e) => return Err(e.to_string()),
        };
        states.insert(child.to_owned(), state);
        Ok(())
    }
    /// Snapshot the actual parent policy, including policy restored from disk.
    /// Children receive an explicit complete policy so they cannot fall back to
    /// the default or to an older, more permissive saved child configuration.
    pub(super) fn resolved_policy(&self, session: &str) -> Result<Policy, String> {
        self.state(session, |state| {
            let mut policy = state.policy.clone();
            policy.explicit = true;
            policy.replace_rules = true;
            if state.planning {
                policy.mode = Some("plan".into());
            } else if policy.planning() {
                policy.mode = Some("default".into());
            }
            Ok(policy)
        })
    }
    async fn ask(
        &self,
        context: HostContext<'_>,
        question: Option<UserQuestion>,
        prompt: String,
    ) -> Result<Value, String> {
        self.ask_call(
            format!(
                "{}:{}",
                context.turn_id().unwrap_or_default(),
                context.call_id()
            ),
            question,
            prompt,
        )
        .await
    }
    async fn ask_call(
        &self,
        id: String,
        question: Option<UserQuestion>,
        prompt: String,
    ) -> Result<Value, String> {
        let sender = self
            .sender
            .as_ref()
            .ok_or("interactive terminal unavailable")?;
        let (reply, receive) = oneshot::channel();
        sender
            .send(PendingInteraction {
                id,
                question,
                prompt,
                reply: Some(reply),
            })
            .await
            .map_err(|_| "user interface closed")?;
        receive
            .await
            .map_err(|_| "user interface cancelled the request")?
    }
    async fn permission(
        &self,
        policy: &Policy,
        name: &str,
        input: &Value,
        invocation: &ClaudeToolInvocation,
    ) -> Result<(), String> {
        match policy
            .evaluate(name, input, &(self.workspace)(&invocation.session_id)?)
            .map_err(|e| e.to_string())?
        {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => Err(reason),
            Decision::Ask => {
                if policy.mode.as_deref() == Some("dontAsk") {
                    return Err(format!("permission denied for {name}: dontAsk mode"));
                }
                let prompt = format!(
                    "Tool permission required: {name}\nCall: {}\nWorkspace: {}\nExact input:\n{}\nType approve to run this call once, or deny. /cancel cancels.",
                    invocation.call_id,
                    (self.workspace)(&invocation.session_id)?.display(),
                    serde_json::to_string_pretty(input).map_err(|e| e.to_string())?
                );
                let approved = tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    self.ask_call(
                        format!("{}:{}:permission", invocation.turn_id, invocation.call_id),
                        None,
                        prompt,
                    ),
                )
                .await
                .map_err(|_| {
                    format!("permission denied for {name}: approval timed out after 300 seconds")
                })?
                .map_err(|e| format!("permission denied for {name}: {e}"))?;
                if approved == json!(true) {
                    Ok(())
                } else {
                    Err(format!("permission denied by user for {name}"))
                }
            }
        }
    }
}
struct InteractionHost(Arc<Interaction>);
impl std::ops::Deref for InteractionHost {
    type Target = Interaction;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl ClaudeHost for InteractionHost {
    async fn execute(
        &self,
        request: HostRequest,
        context: HostContext<'_>,
    ) -> Result<ToolOutput, String> {
        let result = match request {
            HostRequest::AskUserQuestion(request) => {
                let mut answers = serde_json::Map::new();
                for question in request.questions {
                    let mut prompt = format!("{}: {}\n", question.header, question.question);
                    for (i, option) in question.options.iter().enumerate() {
                        prompt.push_str(&format!(
                            "{}. {} — {}\n",
                            i + 1,
                            option.label,
                            option.description
                        ));
                        if let Some(markdown) = &option.markdown {
                            prompt.push_str(markdown);
                            prompt.push('\n');
                        }
                    }
                    prompt.push_str(if question.multi_select { "Choose numbers separated by commas, or other: your answer. /cancel cancels." } else { "Choose one number, or other: your answer. /cancel cancels." });
                    let key = question.question.clone();
                    let answer = self.ask(context, Some(question), prompt).await?;
                    answers.insert(key, answer);
                }
                json!({"answers":answers})
            }
            HostRequest::EnterPlanMode => {
                self.state(context.session_id(), |state| {
                    state.planning = true;
                    Ok(())
                })?;
                json!({"mode":"plan","model_tool_mutations_blocked":true})
            }
            HostRequest::ExitPlanMode(request) => {
                if !self.state(context.session_id(), |state| Ok(state.planning))? {
                    return Err("not currently in plan mode".into());
                }
                let prompt = format!(
                    "Plan approval required. Review the assistant's plan in the transcript.\nRequested actions (requests are not grants):\n{}\nType approve to leave planning, or deny to keep planning. /cancel cancels.",
                    serde_json::to_string_pretty(&request.allowed_prompts)
                        .map_err(|e| e.to_string())?
                );
                let approved = self.ask(context, None, prompt).await? == json!(true);
                if approved {
                    self.state(context.session_id(), |state| {
                        state.planning = false;
                        Ok(())
                    })?;
                }
                json!({"approved":approved,"mode":if approved {"execute"} else {"plan"}})
            }
            _ => return Err("unsupported interaction capability".into()),
        };
        Ok(ToolOutput::text(result.to_string()).with_structured_result(result))
    }
}
fn allowed_in_plan(name: &str) -> bool {
    matches!(
        name,
        "Read"
            | "Glob"
            | "Grep"
            | "WebSearch"
            | "WebFetch"
            | "Skill"
            | "ProjectContext"
            | "TaskGet"
            | "TaskList"
            | "TaskCreate"
            | "TaskUpdate"
            | "TodoWrite"
            | "AskUserQuestion"
            | "EnterPlanMode"
            | "ExitPlanMode"
    )
}
impl ClaudeToolHooks for Interaction {
    fn handles_lifecycle(&self, event: &nanocodex::claude::ClaudeLifecycleEvent) -> bool {
        self.hooks
            .as_ref()
            .is_some_and(|hooks| hooks.handles_lifecycle(event))
    }
    fn lifecycle<'a>(
        &'a self,
        invocation: &'a nanocodex::claude::ClaudeLifecycleInvocation,
    ) -> ClaudeHookFuture<'a, Result<nanocodex::claude::ClaudeLifecycleOutcome, String>> {
        Box::pin(async move {
            match &self.hooks {
                Some(hooks) => hooks.lifecycle(invocation).await,
                None => Ok(nanocodex::claude::ClaudeLifecycleOutcome::default()),
            }
        })
    }

    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>> {
        Box::pin(async move {
            let (planning, policy) = self.state(&invocation.session_id, |state| {
                Ok((state.planning, state.policy.clone()))
            })?;
            // Plan mode restricts new model dispatch, not the OS. Previously
            // admitted work and explicitly trusted host command hooks may run.
            // Deny before hooks/checkpoints so blocked model calls have no effects.
            if planning && !allowed_in_plan(name) {
                return Ok(ClaudeToolDecision::Deny(format!(
                    "{name} is unavailable in plan mode; user approval through ExitPlanMode is required"
                )));
            }
            // Deny known disallowed calls before trusted command hooks run.
            // Ask only after hooks, so the user sees the exact final input.
            if let Decision::Deny(reason) = policy
                .evaluate(name, input, &(self.workspace)(&invocation.session_id)?)
                .map_err(|e| e.to_string())?
            {
                return Ok(ClaudeToolDecision::Deny(reason));
            }
            let decision = match &self.hooks {
                Some(hooks) => hooks.before(name, input, invocation).await?,
                None => ClaudeToolDecision::Allow,
            };
            let actual = match &decision {
                ClaudeToolDecision::Deny(_) => return Ok(decision),
                ClaudeToolDecision::UpdateInput(updated) => updated,
                ClaudeToolDecision::Allow => input,
            };
            if let Err(reason) = self.permission(&policy, name, actual, invocation).await {
                return Ok(ClaudeToolDecision::Deny(reason));
            }
            Ok(decision)
        })
    }
    fn after<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
        reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(hooks) = &self.hooks {
                hooks.after(name, input, invocation, reply).await?;
            }
            Ok(())
        })
    }
}

pub(super) fn install(mut native: ClaudeTools, interaction: Arc<Interaction>) -> ClaudeTools {
    if interaction.sender.is_none() {
        return native;
    }
    let host = Arc::new(ClaudeHostTools::new(
        InteractionHost(interaction),
        [
            HostTool::AskUserQuestion,
            HostTool::EnterPlanMode,
            HostTool::ExitPlanMode,
        ],
    ));
    for schema in host.definitions() {
        let definition: ToolDefinition =
            serde_json::from_value(schema).expect("interaction schema");
        let name = definition.name.clone();
        let host = host.clone();
        native = native.tool_with_context(definition, move |input, invocation| {
            let host = host.clone();
            let name = name.clone();
            async move {
                let context = HostContext::new(
                    &invocation.model,
                    &invocation.session_id,
                    &invocation.call_id,
                    8192,
                )
                .with_turn_id(Some(&invocation.turn_id));
                output_reply(host.execute(&name, input, context).await?)
            }
        });
    }
    native
}

/// `run` keeps stdout as JSONL. Human questions/answers use its actual terminal.
/// A plain OS reader thread avoids leaving an uncancellable Tokio stdin worker
/// alive after Ctrl-C while a question is unanswered.
pub(crate) async fn serve_terminal(mut receiver: InteractionReceiver) {
    let (lines_tx, mut lines) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    while let Some(mut request) = receiver.recv().await {
        if request.is_closed() {
            continue;
        }
        while lines.try_recv().is_ok() {}
        eprintln!("\n[{}]\n{}", request.id, request.prompt());
        loop {
            eprint!("> ");
            let _ = std::io::stderr().flush();
            let line = tokio::select! {
                line = lines.recv() => line,
                () = request.reply.as_mut().expect("unanswered request").closed() => break,
            };
            let Some(Ok(line)) = line else {
                return;
            };
            match request.respond(&line) {
                Ok(()) => break,
                Err(error) => {
                    eprintln!("{error}");
                    if request.is_closed() {
                        break;
                    }
                }
            }
        }
    }
}
