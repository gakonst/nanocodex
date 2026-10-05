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
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            provider: "claude".into(),
            version: 1,
            conversation: Conversation::default(),
            discovered: HashSet::new(),
            tasks: None,
        }
    }
}
impl Snapshot {
    pub(super) fn decode(value: Value) -> Result<Self> {
        let snapshot: Self = serde_json::from_value(value).map_err(provider_error)?;
        if snapshot.provider != "claude" || snapshot.version != 1 {
            return Err(unsupported(
                "unsupported Claude checkpoint version/provider",
            ));
        }
        Ok(snapshot)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    #[serde(default)]
    pub(super) instruction_revision: Option<u64>,
    pub(super) snapshot: Snapshot,
    pub(super) template: MessagesRequest,
    #[serde(default)]
    pub(super) wire_profile: Option<crate::FrozenWireProfile>,
    pub(super) threshold: u64,
    pub(super) parallel: bool,
    pub(super) tool_search: bool,
    pub(super) operation: Option<String>,
    pub(super) prepared: bool,
    pub(super) pending: Vec<Message>,
    pub(super) usage: Usage,
    pub(super) index: u32,
    #[serde(default)]
    pub(super) steers: u32,
    // The retry budget belongs to the admitted turn, including durable replay.
    #[serde(default)]
    pub(super) context_recovery_attempted: bool,
}
impl Cursor {
    pub(super) fn effect<'a>(&'a self, state: &'a State, step: &str) -> Option<Effect<'a>> {
        Some(Effect {
            policy: state.policy.as_deref()?,
            operation: self.operation.as_deref()?,
            step: step.to_owned(),
        })
    }
}
pub(super) struct Effect<'a> {
    policy: &'a dyn ClaudeExecutionPolicy,
    operation: &'a str,
    step: String,
}
impl Effect<'_> {
    pub(super) async fn begin(&self, kind: &str, input: Value) -> Result<Step> {
        self.policy
            .begin_step(
                self.operation.to_owned(),
                self.step.clone(),
                kind.to_owned(),
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
impl State {
    #[cfg_attr(not(feature = "tools"), allow(clippy::missing_const_for_fn))]
    fn task_snapshot(&self) -> Result<Option<Value>> {
        #[cfg(feature = "tools")]
        {
            self.task_board
                .as_ref()
                .map(|tasks| tasks.snapshot().map_err(provider_error))
                .transpose()
        }
        #[cfg(not(feature = "tools"))]
        {
            Ok(None)
        }
    }
    fn restore_tasks(&self, tasks: Option<Value>) -> Result<()> {
        if let Some(tasks) = tasks {
            #[cfg(feature = "tools")]
            self.task_board
                .as_ref()
                .ok_or_else(|| unsupported("Claude task checkpoint requires a task board"))?
                .restore(tasks)
                .map_err(provider_error)?;
            #[cfg(not(feature = "tools"))]
            {
                let _ = tasks;
                return Err(unsupported(
                    "Claude task checkpoint restoration requires tools and a task board",
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
    ) -> Result<Cursor> {
        if let (Some(policy), Some(operation)) = (&self.policy, operation)
            && let Some(value) = policy.continuation(operation.to_owned()).await?
        {
            let cursor: Cursor = serde_json::from_value(value).map_err(recovery_error)?;
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
        let mut cursor = Cursor {
            instruction_revision: None,
            snapshot: self.snapshot(conversation).await?,
            template: self.request_template(speed),
            wire_profile: Some(self.client.freeze_wire_profile()),
            threshold: self.compaction_threshold(),
            parallel: self.parallel_tools,
            tool_search: self.client_tool_search,
            operation: operation.map(str::to_owned),
            prepared: false,
            pending: Vec::new(),
            usage: Usage::default(),
            index: 0,
            steers: 0,
            context_recovery_attempted: false,
        };
        // Task state snapshots and receipts must advance in the same order.
        #[cfg(feature = "tools")]
        if self.policy.is_some() && self.task_board.is_some() {
            cursor.parallel = false;
        }
        self.advance_cursor(&mut cursor, conversation).await?;
        Ok(cursor)
    }
    pub(super) async fn advance_cursor(
        &self,
        cursor: &mut Cursor,
        conversation: &Conversation,
    ) -> Result<()> {
        cursor.snapshot = self.snapshot(conversation).await?;
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
        } else if let Some(handler) = handler {
            tokio::select! {
                biased;
                result = self.call_tool(id, name, input, handler, events, cursor) => result,
                () = cancel.cancelled() => unknown(),
            }
        } else {
            ContentBlock::tool_result_content(
                id,
                ToolResultContent::Text(format!(
                    "Tool {name} is not available in the recovered host; no handler was invoked."
                )),
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
