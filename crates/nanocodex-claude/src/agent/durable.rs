//! Provider-native state at the shared durability crate's execution boundaries.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    provider: String,
    version: u32,
    pub(super) conversation: Conversation,
    pub(super) discovered: HashSet<String>,
    pub(super) tasks: Option<Value>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) workspace: Option<String>,
    /// Provenance of the session that owns this state, so a reopened durable
    /// fork, side conversation or subagent keeps its lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lineage: Option<Lineage>,
    /// Conversation-tree identity shared with forks of the same session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) conversation_id: Option<String>,
    /// Thinking effort and fast-mode preference at this boundary, so a
    /// catalog checkpoint resumes with the session's actual policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) effort: Option<crate::Effort>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) fast_mode: bool,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            provider: "claude".into(),
            version: 1,
            conversation: Conversation::default(),
            discovered: HashSet::new(),
            tasks: None,
            model: None,
            workspace: None,
            lineage: None,
            conversation_id: None,
            effort: None,
            fast_mode: false,
        }
    }
}
impl Snapshot {
    pub(super) fn decode(value: Value) -> Result<Self> {
        serde_json::from_value::<Self>(value)
            .map_err(provider_error)?
            .validated()
    }
    pub(super) fn validated(self) -> Result<Self> {
        if self.provider != "claude" || self.version != 1 {
            return Err(NanocodexError::InvalidCheckpoint(
                "unsupported Claude checkpoint version/provider".into(),
            ));
        }
        Ok(self)
    }
    /// Whether this boundary contains at least one committed exchange.
    pub(super) const fn has_conversation(&self) -> bool {
        !self.conversation.messages.is_empty() || !self.conversation.summary.is_empty()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    // Process-local Code Mode admissions cannot survive a recovered boundary.
    // Settled receipts still replay; unreceipted calls at this index fail closed.
    #[serde(skip)]
    pub(super) recovered_code_index: Option<u32>,
    #[serde(default)]
    pub(super) lifecycle_turn_id: String,
    #[serde(default)]
    pub(super) stop_hook_active: bool,
    #[serde(default)]
    pub(super) instruction_revision: Option<u64>,
    pub(super) snapshot: Snapshot,
    pub(super) template: MessagesRequest,
    /// Only these host-owned definitions may change between requests. Static
    /// definitions remain frozen throughout the admitted durable operation.
    #[serde(default)]
    pub(super) dynamic_tool_names: HashSet<String>,
    #[serde(default)]
    pub(super) wire_profile: Option<crate::FrozenWireProfile>,
    pub(super) threshold: u64,
    pub(super) parallel: bool,
    pub(super) tool_search: bool,
    pub(super) operation: Option<String>,
    pub(super) prepared: bool,
    // Advancing the cursor retires settled effect receipts. Retain admitted
    // media here before retiring prompt-media so recovery never reopens a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) frozen_prompt: Option<Prompt>,
    pub(super) pending: Vec<Message>,
    pub(super) usage: Usage,
    pub(super) index: u32,
    #[serde(default)]
    pub(super) steers: u32,
    // Legacy continuations keep their zero-based effect identities and kind.
    #[serde(default)]
    pub(super) model_step_offset: u32,
    #[serde(default)]
    pub(super) model_receipt_start: Option<u32>,
    // The retry budget belongs to the admitted turn, including durable replay.
    #[serde(default)]
    pub(super) context_recovery_attempted: bool,
    #[serde(default)]
    pub(super) output_continuations: u32,
}
impl Cursor {
    pub(super) fn effect<'a>(&'a self, state: &'a State, step: &str) -> Option<Effect<'a>> {
        Some(Effect {
            policy: state.policy.as_deref()?,
            operation: self.operation.as_deref()?,
            step: step.to_owned(),
            model_call: step
                .strip_prefix("model-")
                .and_then(|index| index.parse::<u32>().ok())
                .zip(self.model_receipt_start)
                .is_some_and(|(index, start)| index >= start),
        })
    }
}
pub(super) struct Effect<'a> {
    policy: &'a dyn ClaudeExecutionPolicy,
    operation: &'a str,
    step: String,
    model_call: bool,
}
impl Effect<'_> {
    pub(super) fn scoped(&self, scope: &str) -> Effect<'_> {
        Effect {
            policy: self.policy,
            operation: self.operation,
            step: format!("{scope}-{}", self.step),
            model_call: self.model_call,
        }
    }

    pub(super) async fn begin(&self, kind: &str, input: Value) -> Result<Step> {
        self.policy
            .begin_step(
                self.operation.to_owned(),
                self.step.clone(),
                if kind == "model" && self.model_call {
                    "model_call"
                } else {
                    kind
                }
                .to_owned(),
                input,
            )
            .await
    }
    pub(super) async fn complete(&self, output: Value) -> Result<()> {
        self.policy
            .complete_step(self.operation.to_owned(), self.step.clone(), output)
            .await
    }
}
/// An interrupted old request is retired explicitly, never represented as a
/// successful model response. The replacement has an independent effect ID.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodeOnlyUpgrade {
    pub(super) code_only_tools: Vec<ClaudeToolSpec>,
    pub(super) notice: String,
}
impl CodeOnlyUpgrade {
    pub(super) fn new(code_only_tools: Vec<ClaudeToolSpec>, tools_disabled: bool) -> Self {
        Self {
            code_only_tools,
            notice: if tools_disabled {
                "Harness recovery notice: the interrupted summary request was reissued with the current tool catalog. Produce the requested text-only summary."
            } else {
                "Harness recovery notice: this operation was upgraded to Code Mode. The preceding direct-tool model request was retired with outcome unknown; provider-side effects may already have occurred. Do not automatically repeat them. Reconcile effects before continuing through exec and wait."
            }.into(),
        }
    }
}
pub(super) fn is_code_only_catalog(tools: &[ClaudeToolSpec]) -> bool {
    tools.len() == 2
        && ["exec", "wait"].iter().all(|name| {
            tools
                .iter()
                .any(|tool| matches!(tool, ClaudeToolSpec::Client(tool) if tool.name == *name))
        })
}
impl State {
    pub(super) fn code_only_tools(&self) -> Vec<ClaudeToolSpec> {
        self.available_tools().into_iter().filter(|tool| {
            matches!(tool, ClaudeToolSpec::Client(tool) if tool.name == "exec" || tool.name == "wait")
        }).collect()
    }
    pub(super) fn classify_code_only_tools(&self, cursor: &mut Cursor) {
        cursor.dynamic_tool_names = self
            .dynamic_catalog()
            .into_iter()
            .filter_map(|(definition, _)| {
                cursor.template.tools.iter().any(|tool| {
                    matches!(tool, ClaudeToolSpec::Client(admitted) if admitted == &definition)
                }).then_some(definition.name)
            })
            .collect();
    }
    #[cfg_attr(
        not(all(feature = "tools", not(target_family = "wasm"))),
        allow(clippy::missing_const_for_fn)
    )]
    fn task_snapshot(&self) -> Result<Option<Value>> {
        #[cfg(all(feature = "tools", not(target_family = "wasm")))]
        {
            self.task_board
                .as_ref()
                .map(|tasks| tasks.snapshot().map_err(provider_error))
                .transpose()
        }
        #[cfg(not(all(feature = "tools", not(target_family = "wasm"))))]
        {
            Ok(None)
        }
    }
    fn restore_tasks(&self, tasks: Option<Value>) -> Result<()> {
        if let Some(tasks) = tasks {
            #[cfg(all(feature = "tools", not(target_family = "wasm")))]
            self.task_board
                .as_ref()
                .ok_or_else(|| invalid("Claude task checkpoint requires a task board"))?
                .restore(tasks)
                .map_err(provider_error)?;
            #[cfg(not(all(feature = "tools", not(target_family = "wasm"))))]
            {
                let _ = tasks;
                return Err(invalid(
                    "Claude task checkpoint restoration requires a native target with tools and a task board",
                ));
            }
        }
        Ok(())
    }
    pub(super) async fn snapshot(&self, conversation: &Conversation) -> Result<Snapshot> {
        Ok(Snapshot {
            conversation: conversation.clone(),
            discovered: self.discovered.lock().await.clone(),
            tasks: self.task_snapshot()?,
            model: Some(self.model()),
            workspace: Some(self.workspace()),
            lineage: Some(self.lineage.clone()),
            conversation_id: Some(self.conversation_id.clone()),
            effort: self.effort(),
            fast_mode: self.fast_mode.load(Ordering::SeqCst),
            ..Snapshot::default()
        })
    }
    async fn restore_snapshot(
        &self,
        conversation: &mut Conversation,
        snapshot: Snapshot,
    ) -> Result<()> {
        self.restore_tasks(snapshot.tasks)?;
        *self.discovered.lock().await = snapshot.discovered;
        *conversation = snapshot.conversation;
        Ok(())
    }
    pub(super) async fn cursor(
        &self,
        conversation: &mut Conversation,
        operation: Option<&str>,
        speed: Option<crate::Speed>,
        prompt: Option<&Prompt>,
    ) -> Result<Cursor> {
        if let (Some(policy), Some(operation)) = (&self.policy, operation)
            && let Some(value) = policy.continuation(operation.to_owned()).await?
        {
            let mut cursor: Cursor = serde_json::from_value(value).map_err(recovery_error)?;
            cursor.recovered_code_index = Some(cursor.index);
            // Replay the old in-flight effect unchanged; subsequent model effects
            // can participate in the shared steering consumption contract.
            if cursor.model_receipt_start.is_none() {
                cursor.model_receipt_start = Some(cursor.index.saturating_add(1));
            }
            if cursor.operation.as_deref() != Some(operation)
                || cursor.snapshot.provider != "claude"
                || cursor.snapshot.version != 1
            {
                return Err(recovery_error("invalid Claude execution continuation"));
            }
            self.restore_snapshot(conversation, cursor.snapshot.clone())
                .await
                .map_err(recovery_error)?;
            return Ok(cursor);
        }
        let template = self.request_template(speed);
        let static_names = self
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<HashSet<_>>();
        let dynamic_tool_names = template
            .tools
            .iter()
            .filter_map(|tool| match tool {
                ClaudeToolSpec::Client(tool) if !static_names.contains(tool.name.as_str()) => {
                    Some(tool.name.clone())
                }
                _ => None,
            })
            .collect();
        let wire_profile = self
            .client
            .freeze_wire_profile(template.cache_control.is_some());
        let mut cursor = Cursor {
            recovered_code_index: None,
            lifecycle_turn_id: candidate_id("lifecycle"),
            stop_hook_active: false,
            instruction_revision: None,
            snapshot: if self.policy.is_some() && operation.is_some() {
                self.snapshot(conversation).await?
            } else {
                Snapshot::default()
            },
            template,
            dynamic_tool_names,
            wire_profile: Some(wire_profile),
            threshold: self.compaction_threshold(),
            parallel: self.parallel_tools,
            tool_search: self.client_tool_search,
            operation: operation.map(str::to_owned),
            prepared: false,
            frozen_prompt: prompt
                .filter(|prompt| {
                    matches!(
                        prompt.instruction,
                        nanocodex_agent::input::PromptInput::Content(_)
                    )
                })
                .cloned(),
            pending: Vec::new(),
            usage: Usage::default(),
            index: 0,
            steers: 0,
            model_step_offset: 1,
            model_receipt_start: Some(0),
            context_recovery_attempted: false,
            output_continuations: 0,
        };
        // Task state snapshots and receipts must advance in the same order.
        #[cfg(all(feature = "tools", not(target_family = "wasm")))]
        if self.policy.is_some() && self.task_board.is_some() {
            cursor.parallel = false;
        }
        self.advance_cursor(&mut cursor, conversation).await?;
        Ok(cursor)
    }
    /// Whether this cursor is journaled for durable replay.
    pub(super) fn persists(&self, cursor: &Cursor) -> bool {
        self.policy.is_some() && cursor.operation.is_some()
    }
    /// Records the pending round in a journaled cursor; ephemeral turns keep
    /// their single working copy.
    pub(super) fn retain_pending(&self, cursor: &mut Cursor, pending: &[Message]) {
        if self.persists(cursor) {
            cursor.pending = pending.to_vec();
        }
    }
    pub(super) async fn advance_cursor(
        &self,
        cursor: &mut Cursor,
        conversation: &Conversation,
    ) -> Result<()> {
        // Only settled boundaries may replace an admitted catalog. Pending
        // model requests are reconciled by response(), and tool receipts by
        // durable_tool(), before the journal permits this advance.
        if self.code_only && !is_code_only_catalog(&cursor.template.tools) {
            cursor.template.tools = self.code_only_tools();
            self.classify_code_only_tools(cursor);
            cursor.tool_search = false;
        }
        // Only a durable operation persists or replays its cursor. Without one,
        // another full conversation copy per round would only consume memory.
        if self.persists(cursor) {
            cursor.snapshot = self.snapshot(conversation).await?;
        }
        if let (Some(policy), Some(operation)) = (&self.policy, &cursor.operation) {
            policy
                .advance(
                    operation.clone(),
                    serde_json::to_value(&*cursor).map_err(provider_error)?,
                )
                .await?;
        }
        Ok(())
    }
    pub(super) async fn settle(
        &self,
        conversation: &Conversation,
        request: &BackendPrompt,
        result: &Result<TurnResult>,
    ) -> Result<()> {
        let (Some(policy), Some(operation)) = (&self.policy, &request.request_id) else {
            return Ok(());
        };
        // A store failure cannot be converted into an acknowledged terminal.
        if result
            .as_ref()
            .err()
            .is_some_and(|error| error.execution_policy_disposition().is_some())
        {
            self.stopped.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let checkpoint =
            serde_json::to_value(self.snapshot(conversation).await?).map_err(provider_error)?;
        let settled =
            match result {
                Ok(result) => policy
                    .complete(
                        operation.clone(),
                        checkpoint,
                        json!({"final_message": result.final_message(), "usage":result.usage()}),
                    )
                    .await,
                Err(NanocodexError::TurnCancelled) => {
                    policy.cancel(operation.clone(), checkpoint).await
                }
                Err(error) => {
                    policy
                        .fail(operation.clone(), checkpoint, error.to_string())
                        .await
                }
            };
        if settled.is_err() {
            self.stopped.store(true, Ordering::SeqCst);
        }
        settled
    }
    pub(super) async fn durable_tool(
        &self,
        control: (&Cursor, &Cancellation),
        id: &str,
        name: &str,
        input: &Value,
        handler: Option<&Handler>,
        events: &AgentEventPublisher,
    ) -> Result<ContentBlock> {
        let (cursor, cancel) = control;
        let index = cursor.index;
        let step = format!("tool-{index}-{id}");
        let effect = cursor.effect(self, &step);
        if let Some(effect) = &effect
            && let Step::Replay(value) = effect
                .begin("tool", json!({"id":id,"name":name,"input":input}))
                .await?
        {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Receipt {
                result: ContentBlock,
                tasks: Option<Value>,
                discovered: HashSet<String>,
            }
            let receipt: Receipt = serde_json::from_value(value).map_err(recovery_error)?;
            if !matches!(&receipt.result, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)
            {
                return Err(recovery_error(
                    "tool receipt does not match the admitted call",
                ));
            }
            self.restore_tasks(receipt.tasks).map_err(recovery_error)?;
            self.discovered.lock().await.extend(receipt.discovered);
            return Ok(receipt.result);
        }
        let unknown = || {
            ContentBlock::tool_result_content(id, ToolResultContent::Text("Tool execution interrupted; outcome unknown. Do not assume it did not run or automatically repeat it.".into()), true)
        };
        let result = if cancel.flag.load(Ordering::SeqCst) {
            unknown()
        } else if self.code_only
            && cursor.recovered_code_index == Some(index)
            && matches!(name, "exec" | "wait")
        {
            ContentBlock::tool_result_content(id, ToolResultContent::Text(
                "Code Mode admission was lost during recovery; prior effects may have outcome unknown. No code was executed in this attempt. Reconcile those effects before using a fresh cell from a new model request.".into()
            ), true)
        } else if let Some(handler) = handler {
            tokio::select! {
                biased;
                result = self.call_tool(id, name, input, handler, events, cursor) => result?,
                () = cancel.cancelled() => unknown(),
            }
        } else if self.code_only
            && name != "exec"
            && name != "wait"
            && cursor
                .template
                .tools
                .iter()
                .any(|tool| matches!(tool, ClaudeToolSpec::Client(tool) if tool.name == name))
        {
            ContentBlock::tool_result_content(
                id,
                ToolResultContent::Text(format!(
                    "Direct tool {name} was retired during Code Mode recovery. Its prior outcome is unknown; no handler was invoked in this attempt. Reconcile effects before repeating through exec."
                )),
                true,
            )
        } else {
            ContentBlock::tool_result_content(
                id,
                ToolResultContent::Text(
                    "Tool is not available in the admitted catalog or current host; no handler was invoked. Use an available tool."
                        .into(),
                ),
                true,
            )
        };
        if let Some(effect) = &effect {
            effect.complete(json!({"result":result,"tasks":self.task_snapshot()?,"discovered":self.discovered.lock().await.clone()})).await?;
        }
        Ok(result)
    }
}

pub(super) fn replay(operation: String, output: Value) -> Result<TurnResult> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        final_message: String,
        usage: Option<TurnUsage>,
    }
    let output: Output = serde_json::from_value(output).map_err(recovery_error)?;
    Ok(TurnResult::from_backend(
        Some(operation),
        output.final_message,
        output.usage,
        None,
    ))
}
pub(super) fn candidate_id(kind: &str) -> String {
    format!("claude-{kind}-{}", uuid::Uuid::new_v4())
}

pub(super) fn recovery_error(error: impl std::fmt::Display) -> NanocodexError {
    NanocodexError::execution_policy_with_disposition(
        "Claude recovery",
        nanocodex_agent::ExecutionPolicyDisposition::Reopen,
        provider_error(error),
    )
}

/// Prepares a settled historical checkpoint for a new conversation branch.
///
/// The host selects the checkpoint immediately before a user turn through its
/// owned durable journal. This function never truncates current messages or
/// replays historical tools. Effect identity and recovery warnings survive the
/// branch even when later transcript content is forgotten. Provider containers
/// are not reused because their filesystem may contain later effects.
///
/// The branch records [`Origin::Branch`] lineage: its parent is
/// `source_session_id`, it stays in the source's conversation tree (same
/// root), and it sits one level deeper than the source.
pub fn rewind_checkpoint(
    source_session_id: &str,
    previous: Option<Value>,
    latest: Value,
) -> Result<Value> {
    let latest = Snapshot::decode(latest)?;
    // The source's own lineage, as recorded by its newest boundary; a legacy
    // snapshot without one belonged to a root session.
    let source_lineage = latest
        .lineage
        .clone()
        .unwrap_or_else(|| Lineage::root(source_session_id));
    if latest.conversation.pending_continuation {
        return Err(invalid(
            "conversation rewind refuses a pending tool/provider continuation",
        ));
    }
    let mut selected = match previous {
        Some(value) => Snapshot::decode(value)?,
        None => Snapshot {
            model: latest.model.clone(),
            workspace: latest.workspace.clone(),
            effort: latest.effort,
            fast_mode: latest.fast_mode,
            ..Snapshot::default()
        },
    };
    if selected.conversation.pending_continuation {
        return Err(invalid(
            "selected checkpoint has a pending tool/provider continuation",
        ));
    }
    selected
        .conversation
        .admitted_tool_ids
        .extend(latest.conversation.admitted_tool_ids);
    for notice in latest.conversation.recovery_notices {
        if !selected.conversation.recovery_notices.contains(&notice) {
            selected.conversation.recovery_notices.push(notice);
        }
    }
    let notice = "This conversation was explicitly rewound into a new session. External effects from discarded turns may still exist; reconcile their current state before repeating any action. Historical tool calls must not be replayed.".to_owned();
    if !selected.conversation.recovery_notices.contains(&notice) {
        selected.conversation.recovery_notices.push(notice);
    }
    selected.conversation.lifecycle_started = false;
    selected.conversation.container = None;
    selected.conversation.previous_message_id = None;
    // The rewound branch is a new session in the source's tree: it adopts its
    // own identity and prompt-cache lineage, derived from the source session.
    selected.lineage = Some(Lineage::child_of(
        &source_lineage,
        source_session_id,
        Origin::Branch,
    ));
    selected.conversation_id = None;
    serde_json::to_value(selected).map_err(provider_error)
}

/// Model-visible content of a Claude checkpoint, decoded for session listing,
/// previews and transcript replay. Signed thinking and binary payloads are
/// never exposed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeCheckpointView {
    /// Claude model pinned by the checkpoint, when recorded.
    pub model: Option<HarnessModel>,
    /// Session workspace recorded by the checkpoint, when any.
    pub workspace: Option<String>,
    /// Provenance recorded by the checkpoint; older checkpoints omit it.
    pub lineage: Option<Lineage>,
    /// User and assistant text plus tool invocations, in conversation order.
    pub transcript: Vec<nanocodex_agent::session::TranscriptItem>,
}

/// Decodes a durable Claude checkpoint (the provider-native state committed
/// by the execution policy) without opening or owning the session.
///
/// # Errors
///
/// Returns [`NanocodexError::InvalidCheckpoint`] when the value is not a
/// supported Claude checkpoint or records a non-Claude model.
pub fn decode_checkpoint(checkpoint: Value) -> Result<ClaudeCheckpointView> {
    let snapshot: Snapshot = serde_json::from_value::<Snapshot>(checkpoint)
        .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?
        .validated()?;
    snapshot.view()
}

/// Encodes a durable Claude checkpoint (the provider-native state committed by
/// the execution policy) as a portable [`SessionCheckpoint`] for this session,
/// keeping its recorded model, thinking effort and fast-mode preference.
///
/// Request limits that only the live builder knows use the model defaults:
/// the documented output maximum and context window, with caching and
/// diagnostics off. [`crate::ClaudeBuilder::resume`] accepts the
/// result, so a stored session resumes through the same portable path as a
/// live checkpoint.
///
/// # Errors
///
/// Returns [`NanocodexError::InvalidCheckpoint`] when the value is not a
/// supported Claude checkpoint or records no Claude model.
pub fn session_checkpoint(
    session_id: &str,
    lineage: Lineage,
    checkpoint: Value,
) -> Result<SessionCheckpoint> {
    let mut snapshot = Snapshot::decode(checkpoint)?;
    let model = snapshot
        .model
        .clone()
        .filter(|model| {
            model
                .parse::<HarnessModel>()
                .is_ok_and(|model| model.family() == HarnessFamily::Claude)
        })
        .ok_or_else(|| {
            NanocodexError::InvalidCheckpoint("checkpoint records no Claude model".into())
        })?;
    let conversation_id = snapshot
        .conversation_id
        .clone()
        .unwrap_or_else(|| session_id.to_owned());
    snapshot.lineage = Some(lineage.clone());
    let policy = NativePolicy {
        context_window_tokens: default_context_window_tokens(&model),
        max_tokens: None,
        effort: snapshot.effort,
        adaptive_thinking: snapshot.effort.is_some(),
        automatic_cache: false,
        cache_one_hour: false,
        keep_thinking: false,
        fast_mode: snapshot.fast_mode,
        message_diagnostics: false,
        auto_compact_window_tokens: None,
        model,
    };
    ClaudeBoundary {
        snapshot: Arc::new(snapshot),
        policy,
        session_id: session_id.to_owned(),
        lineage,
        conversation_id,
    }
    .checkpoint()
}

/// Decodes a portable Claude [`SessionCheckpoint`] into its model-visible view.
///
/// # Errors
///
/// Returns [`NanocodexError::CheckpointFamilyMismatch`] for another family's
/// checkpoint and [`NanocodexError::InvalidCheckpoint`] for an invalid one.
pub fn decode_session_checkpoint(checkpoint: &SessionCheckpoint) -> Result<ClaudeCheckpointView> {
    checkpoint.validate()?;
    checkpoint.require_family(HarnessFamily::Claude)?;
    let stored: NativeChildState = serde_json::from_value(checkpoint.payload().clone())
        .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?;
    let mut view = stored.snapshot.validated()?.view()?;
    view.model = Some(checkpoint.model());
    view.lineage = Some(checkpoint.lineage().clone());
    Ok(view)
}

impl Snapshot {
    fn view(&self) -> Result<ClaudeCheckpointView> {
        let model = self
            .model
            .as_deref()
            .map(|model| {
                model
                    .parse::<HarnessModel>()
                    .ok()
                    .filter(|model| model.family() == HarnessFamily::Claude)
                    .ok_or_else(|| {
                        NanocodexError::InvalidCheckpoint(
                            "checkpoint model is not a Claude model".into(),
                        )
                    })
            })
            .transpose()?;
        Ok(ClaudeCheckpointView {
            model,
            workspace: self
                .workspace
                .clone()
                .filter(|workspace| !workspace.is_empty()),
            lineage: self.lineage.clone(),
            transcript: self.transcript(),
        })
    }
    fn transcript(&self) -> Vec<nanocodex_agent::session::TranscriptItem> {
        use nanocodex_agent::session::TranscriptItem;
        let mut items = Vec::new();
        if !self.conversation.summary.is_empty() {
            items.push(TranscriptItem::Assistant(format!(
                "Retained conversation summary:\n{}",
                self.conversation.summary
            )));
        }
        for message in &self.conversation.messages {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text, .. } => items.push(match message.role {
                        Role::Assistant => TranscriptItem::Assistant(text.clone()),
                        Role::User => TranscriptItem::User(text.clone()),
                    }),
                    ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                        items.push(TranscriptItem::Reasoning(thinking.clone()));
                    }
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    }
                    | ContentBlock::ServerToolUse {
                        id, name, input, ..
                    } => {
                        items.push(TranscriptItem::Tool {
                            call_id: id.clone(),
                            name: name.clone(),
                            arguments: input.to_string(),
                        });
                    }
                    ContentBlock::McpToolUse {
                        id,
                        name,
                        server_name,
                        input,
                        ..
                    } => items.push(TranscriptItem::Tool {
                        call_id: id.clone(),
                        name: format!("mcp__{server_name}__{name}"),
                        arguments: input.to_string(),
                    }),
                    _ => {}
                }
            }
        }
        items
    }
}
