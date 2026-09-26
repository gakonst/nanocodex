mod continuation;
mod lifecycle;
mod responses;
mod state;
mod tool_calls;
mod turn;

use continuation::ExecutionPhase;
use lifecycle::*;
use responses::*;
use state::*;
use tool_calls::*;

use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    panic::AssertUnwindSafe,
    path::Path,
    sync::{Arc, Mutex},
};

use futures_util::{FutureExt, StreamExt, stream::FuturesOrdered};
use nanocodex_oai_api::{
    __private::{
        EventSink, ManagedSessionState, ModelConfig, ResponsesAttemptFactory,
        assign_missing_response_item_id, compaction, responses_lite_request_prefix,
        with_code_mode_tool_names,
    },
    Model, Prompt, Thinking,
    events::AgentEventKind,
    pricing::{ServiceTier, estimate_for_model},
    responses::{
        ContentItem, FunctionOutputBody, MessageRole, RequestProfile, ResponseItem, ResponseItemId,
        ToolDefinition, Usage,
    },
    tower::{
        CodeCall, CodeCallKind, GenerationOutput as TurnResult, ResponsesAttempt, ResponsesClient,
        ResponsesOutput, ResponsesServiceResponse,
    },
    transport::{ResponsesError, ResponsesTransport, TransportStats, TransportStatsSnapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use tokio::sync::{RwLock, watch};
use tower::Service;
use tracing::{Instrument, info, info_span};
use web_time::Instant;

use super::{
    CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted, ModelCallFailed,
    ModelCallStarted, RunError, RunStarted, RunStats, RunSteered, ToolCallArguments, ToolCallEvent,
    ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted,
    context::{ContextBaseline, ContextSnapshot, ContextState},
    display_endpoint, elapsed_ns,
    input::{
        custom_tool_notification, custom_tool_output, developer_context, function_tool_output,
        prompt_messages, task_input, tool_search_output, turn_aborted,
    },
    terminal_payload,
};
use crate::{
    NanocodexError, Result,
    agent::{AgentSend, ContextSource, ExecutionSteps, execution::QueuedSteer},
    prompt_cache::ModelPromptCache,
    usage::TurnUsage,
};
use nanocodex_tools::{
    __private::model_contract as model_tool_contract,
    ToolContext, Tools,
    code_mode::{CodeModeExecution, CodeModeObserver, CodeModeUpdate},
    contract::{DEFAULT_TOOL_OUTPUT_TOKENS, ToolInput, ToolOutput, ToolOutputBody},
    image::{prepare_output_images, prepare_user_input},
    runtime::{
        ImageGenerationConfig, OwnedToolContext, ToolRuntime, ToolRuntimeControl, WebSearchConfig,
    },
};

pub(crate) struct ModelRun<S> {
    events: EventSink,
    provider_session_id: Arc<str>,
    config: Arc<ModelConfig>,
    model: Model,
    thinking: Thinking,
    fast_mode: bool,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    started_at: Instant,
    stats: RunStats,
    transport_baseline: TransportStatsSnapshot,
    session: Option<ModelSessionState>,
    active_tools: Option<ToolRuntimeControl>,
    active_tool_calls: Vec<ActiveToolCall>,
    active_tool_batch_started_at: Option<Instant>,
    tool_call_indices: HashMap<Box<str>, u32>,
    tools: Tools,
    prompt_cache: ModelPromptCache,
    context_source: ContextSource,
    host_context: Option<Arc<str>>,
    // Execution-local authority is deliberately absent from inheritable ModelCheckpoint.
    instruction_revision: Option<u64>,
    global_instructions: Option<Arc<str>>,
    force_compaction: bool,
    pending_developer_messages: Vec<ResponseItem>,
    execution_steps: Option<ExecutionSteps>,
    before_compaction: Option<Arc<dyn crate::execution::BeforeCompaction>>,
}

/// One trusted terminal output accepted during a running turn. The durable
/// operation remains authoritative; this queue only accelerates delivery.
#[derive(Clone)]
pub(crate) struct QueuedBoundaryOutput {
    pub(crate) call_id: String,
    pub(crate) output: FunctionOutputBody,
    pub(crate) operation_id: String,
    pub(crate) durable_index: u32,
    pub(crate) accepted_after_model_call_index: u32,
    pub(crate) model_call_index: Option<u32>,
}

pub(crate) type BoundaryOutputQueue = Arc<tokio::sync::Mutex<VecDeque<QueuedBoundaryOutput>>>;

pub(crate) struct TurnSteering {
    pub(crate) boundary_outputs: BoundaryOutputQueue,
    pub(crate) retained_boundary_outputs: Vec<QueuedBoundaryOutput>,
    pub(crate) receiver: crate::agent::execution::SteerQueue,
    pub(crate) retained: Vec<QueuedSteer>,
    pub(crate) model_call_index: Arc<tokio::sync::Mutex<u32>>,
}

pub(crate) enum ModelTurnOutcome {
    Completed(CompletedModelTurn),
    Cancelled(ModelCheckpoint),
    Failed {
        error: NanocodexError,
        checkpoint: ModelCheckpoint,
    },
}

pub(crate) enum ModelCompactOutcome {
    Completed(ModelCheckpoint),
    Cancelled(ModelCheckpoint),
    Failed {
        error: NanocodexError,
        checkpoint: ModelCheckpoint,
    },
}

pub(crate) struct CompletedModelTurn {
    pub(crate) final_message: String,
    pub(crate) usage: TurnUsage,
    pub(crate) checkpoint: ModelCheckpoint,
}

#[derive(Clone)]
pub(crate) struct ModelCheckpoint {
    workspace: String,
    provider_session_id: Arc<str>,
    conversation: ConversationState,
    request_prefix: Arc<[ResponseItem]>,
    prompt_cache_key: Arc<str>,
    preserve_inherited_delta: bool,
    global_instructions: Option<Arc<str>>,
    context_baseline: ContextBaseline,
    pending_late_wake: Option<String>,
}

pub(crate) struct PreparedCheckpoint {
    pub(crate) checkpoint: ModelCheckpoint,
    pub(crate) runtime: ToolRuntime,
    pub(crate) context_source: ContextSource,
    selected_agents_md: Option<Arc<str>>,
}

pub(crate) struct HistoryCheckpoint {
    pub(crate) workspace: String,
    pub(crate) provider_session_id: Arc<str>,
    pub(crate) canonical_context: ResponseItem,
    pub(crate) history: Vec<ResponseItem>,
    pub(crate) client_authored: std::collections::BTreeSet<String>,
    pub(crate) unreal_function_outputs: bool,
    pub(crate) prompt_cache_key: Arc<str>,
    pub(crate) context_baseline: Option<ContextBaseline>,
}

impl ModelCheckpoint {
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }
    pub(crate) fn history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.conversation.shared_history()
    }

    #[allow(dead_code, reason = "consumed by the native rollout boundary only")]
    pub(crate) const fn history_revision(&self) -> u64 {
        self.conversation.history_revision()
    }

    pub(crate) fn request_prefix(&self) -> &[ResponseItem] {
        &self.request_prefix
    }

    pub(crate) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    pub(crate) fn canonical_context(&self) -> &ResponseItem {
        &self.conversation.canonical_context
    }

    pub(crate) const fn client_authored(&self) -> &std::collections::BTreeSet<String> {
        self.conversation.managed.client_authored()
    }

    pub(crate) fn context_usage(&self) -> crate::session::ContextUsage {
        let (usage, server_reasoning_included) = self.conversation.managed.context_usage();
        crate::session::ContextUsage {
            usage: usage.cloned(),
            server_reasoning_included,
            is_estimate: self.conversation.managed.context_usage_is_estimate(),
        }
    }

    pub(crate) fn restore_context_usage(&mut self, usage: &crate::session::ContextUsage) {
        self.conversation.managed.restore_context_usage(
            usage.usage.as_ref(),
            usage.server_reasoning_included,
            usage.is_estimate,
        );
    }

    pub(crate) const fn unreal_function_outputs(&self) -> bool {
        self.conversation.managed.unreal_function_outputs()
    }

    /// A durable wake marker is independent of the transcript tail: compaction,
    /// developer context, and replay may all rewrite that tail before admission.
    pub(crate) fn late_wake_id(&self) -> Option<&str> {
        self.pending_late_wake.as_deref()
    }

    pub(crate) fn restore_late_wake(&mut self, wake: Option<String>) {
        self.pending_late_wake = wake;
    }

    pub(crate) fn snapshot_history(&self) -> Vec<ResponseItem> {
        self.conversation.flattened_history()
    }

    pub(crate) const fn context_baseline(&self) -> &ContextBaseline {
        &self.context_baseline
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "checkpoint restoration keeps each retained boundary explicit"
    )]
    pub(crate) fn resume(
        workspace: String,
        provider_session_id: Arc<str>,
        request_prefix: Vec<ResponseItem>,
        prompt_cache_key: Arc<str>,
        canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
        client_authored: std::collections::BTreeSet<String>,
        unreal_function_outputs: bool,
        global_instructions: Option<Arc<str>>,
        context_baseline: Option<ContextBaseline>,
    ) -> Result<Self> {
        let context_baseline =
            context_baseline.unwrap_or_else(|| ContextBaseline::reconstruct(&history));
        let mut conversation =
            ConversationState::resume(canonical_context, history, unreal_function_outputs)?;
        conversation
            .managed
            .restore_client_authored(client_authored);
        Ok(Self {
            workspace,
            provider_session_id,
            conversation,
            request_prefix: Arc::from(request_prefix),
            prompt_cache_key,
            preserve_inherited_delta: false,
            global_instructions,
            context_baseline,
            pending_late_wake: None,
        })
    }
}

impl<S> ModelRun<S> {
    #[allow(
        clippy::too_many_arguments,
        reason = "the private run owns each injected lifecycle component directly"
    )]
    pub(crate) fn new(
        events: EventSink,
        provider_session_id: Arc<str>,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        context_source: ContextSource,
        host_context: Option<Arc<str>>,
    ) -> Self {
        let model = config.model;
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            provider_session_id,
            config,
            model,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            transport_baseline: TransportStatsSnapshot::default(),
            session: None,
            active_tools: None,
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            host_context,
            instruction_revision: None,
            global_instructions,
            force_compaction: false,
            pending_developer_messages: Vec::new(),
            execution_steps: None,
            before_compaction: None,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the private run owns each injected lifecycle component directly"
    )]
    pub(crate) fn from_checkpoint(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        prepared: PreparedCheckpoint,
        host_context: Option<Arc<str>>,
    ) -> Self {
        let PreparedCheckpoint {
            checkpoint,
            runtime,
            context_source,
            selected_agents_md,
        } = prepared;
        let provider_session_id = Arc::clone(&checkpoint.provider_session_id);
        let active_tools = runtime.control();
        let (_, code_mode_tool_names) = model_tool_contract(&runtime, events.request_id());
        let factory = ResponsesAttemptFactory::new(
            with_code_mode_tool_names(
                RequestProfile::new(
                    checkpoint.provider_session_id.to_string(),
                    checkpoint.prompt_cache_key.to_string(),
                    Arc::clone(&checkpoint.request_prefix),
                )
                .with_thread_id(events.request_id()),
                code_mode_tool_names,
            ),
            events.clone(),
            Arc::clone(&transport_stats),
        );
        let model = config.model;
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let context_source =
            context_source.with_fallback_global(checkpoint.global_instructions.clone());
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            provider_session_id,
            config,
            model,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            transport_baseline: TransportStatsSnapshot::default(),
            session: Some(ModelSessionState {
                workspace: checkpoint.workspace,
                tools: runtime,
                factory,
                conversation: checkpoint.conversation,
                context: ContextState::new(selected_agents_md, checkpoint.context_baseline),
                preserve_inherited_delta: checkpoint.preserve_inherited_delta,
                pending_late_wake: checkpoint.pending_late_wake,
            }),
            active_tools: Some(active_tools),
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            host_context,
            instruction_revision: None,
            global_instructions,
            force_compaction: false,
            pending_developer_messages: Vec::new(),
            execution_steps: None,
            before_compaction: None,
        }
    }

    pub(crate) fn set_before_compaction(
        &mut self,
        hook: Option<Arc<dyn crate::execution::BeforeCompaction>>,
    ) {
        self.before_compaction = hook;
    }

    pub(crate) fn set_host_context(&mut self, host_context: Option<Arc<str>>) {
        self.host_context = host_context;
    }

    pub(crate) fn set_events(&mut self, events: EventSink) {
        if let Some(session) = &mut self.session {
            session.factory.set_events(events.clone());
        }
        self.events = events;
    }

    pub(crate) fn replace_client(&mut self, client: ResponsesClient<S>) {
        self.client = client;
    }

    pub(crate) async fn shutdown(&mut self) {
        if let Some(tools) = &self.active_tools {
            tools.cancel().await;
        }
    }

    pub(crate) fn append_developer_message(
        &mut self,
        text: String,
        requested_workspace: Option<&str>,
    ) -> Result<ModelCheckpoint> {
        let item = ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: text.into_boxed_str(),
            }],
        );
        if self.session.is_none() {
            self.session = Some(self.empty_session(requested_workspace)?);
        }
        let session = self.session.as_mut().ok_or_else(|| {
            NanocodexError::InvalidSessionSnapshot(
                "developer context did not establish a model session".to_owned(),
            )
        })?;
        if !self.pending_developer_messages.is_empty() {
            session
                .conversation
                .append_client(self.pending_developer_messages.drain(..));
        }
        session.conversation.append_client([item]);
        session.conversation.commit_tail();
        session.preserve_inherited_delta = true;
        Ok(ModelCheckpoint {
            workspace: session.workspace.clone(),
            provider_session_id: Arc::from(session.factory.profile().session_id()),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: true,
            global_instructions: self.global_instructions.clone(),
            context_baseline: session.context.baseline(),
            pending_late_wake: session.pending_late_wake.clone(),
        })
    }

    /// Finishes a previously staged Unreal function call at an idle model boundary.
    /// A sent pending item is immutable; its terminal output is a second typed
    /// output with the same call ID and will be replayed from the checkpoint.
    pub(crate) fn submit_late_function_output(
        &mut self,
        call_id: &str,
        output: FunctionOutputBody,
        operation_id: &str,
        requested_workspace: Option<&str>,
    ) -> Result<(ModelCheckpoint, bool)>
    where
        S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
        S::Error: Into<nanocodex_oai_api::ResponseError>,
        S::Future: AgentSend,
    {
        let session = self.session.as_mut().ok_or_else(|| {
            NanocodexError::InvalidRequest("no model session for late function output".into())
        })?;
        session.validate_workspace(requested_workspace)?;
        Self::append_late_output_to_session(
            session,
            call_id,
            output,
            operation_id,
            true,
            self.global_instructions.clone(),
        )
    }

    /// Shared transcript mutation for an idle wake and an active model boundary.
    /// The active caller must serialize this with model-request admission and
    /// persist the returned checkpoint before acknowledging model uptake.
    fn append_late_output_to_session(
        session: &mut ModelSessionState,
        call_id: &str,
        output: FunctionOutputBody,
        operation_id: &str,
        idle_wake: bool,
        global_instructions: Option<Arc<str>>,
    ) -> Result<(ModelCheckpoint, bool)>
    where
        S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
        S::Error: Into<nanocodex_oai_api::ResponseError>,
        S::Future: AgentSend,
    {
        if !session.conversation.managed.unreal_function_outputs() {
            return Err(NanocodexError::InvalidRequest(
                "Unreal function outputs are not enabled for this session".into(),
            ));
        }
        // Deterministic operation identity is retained on the typed output
        // itself, surviving snapshots. The raw ID never enters the transcript.
        const RECEIPT_NAMESPACE: uuid::Uuid =
            uuid::Uuid::from_u128(0x0d46ca1e_90ac_4c9a_ab23_14f66b19b5ea);
        let receipt_id = ResponseItemId::from_server(format!(
            "late:{}",
            uuid::Uuid::new_v5(&RECEIPT_NAMESPACE, operation_id.as_bytes()),
        ));
        for item in session.conversation.managed.flattened_history() {
            if let ResponseItem::FunctionCallOutput {
                id: Some(id),
                call_id: stored_call,
                output: stored_output,
                ..
            } = item
                && id == receipt_id
            {
                if stored_call.as_ref() != call_id
                    || serde_json::to_value(stored_output).ok()
                        != serde_json::to_value(&output).ok()
                {
                    return Err(NanocodexError::InvalidRequest(
                        "late output operation ID reused with different call or body".into(),
                    ));
                }
                return Ok((
                    Self::checkpoint_from_session(session, true, global_instructions),
                    true,
                ));
            }
        }
        // Validate against an isolated clone before sealing the original tail.
        // Do not risk partially changing the driver's mutable session on error.
        let mut conversation = session.conversation.clone();
        // At an active boundary, an unsent placeholder is replaceable rather
        // than a second same-ID output. The idle path lacks that request fence
        // and conservatively seals its existing tail before completion.
        if idle_wake {
            conversation.commit_tail();
        }
        conversation
            .managed
            .complete_unreal_function_output_with_id(call_id, output, Some(receipt_id.clone()))
            .map_err(|error| NanocodexError::InvalidRequest(error.to_string()))?;
        conversation.commit_tail();
        session.conversation = conversation;
        // Chain all unconsumed outputs into the same wake. Replay of the same
        // receipt returns above without advancing the identity a second time.
        if idle_wake {
            session.pending_late_wake = Some(advance_late_wake(
                session.pending_late_wake.as_deref(),
                receipt_id.as_ref(),
            ));
        }
        session.preserve_inherited_delta = true;
        Ok((
            Self::checkpoint_from_session(session, true, global_instructions),
            false,
        ))
    }

    fn empty_session(&mut self, requested_workspace: Option<&str>) -> Result<ModelSessionState> {
        let workspace = requested_workspace.map_or_else(
            || self.context_source.resolve_workspace(None),
            |workspace| Ok(workspace.to_owned()),
        )?;
        let selected_agents_md = self
            .context_source
            .project_instructions(&workspace)
            .map(Arc::<str>::from);
        let tools = tool_runtime(&workspace, &self.config, &self.tools);
        let tool_control = tools.control();
        self.active_tools = Some(tool_control);
        let factory = self.attempt_factory(&tools)?;
        let context = ContextState::new(selected_agents_md, ContextBaseline::Missing);
        let canonical_context = context
            .capture(
                tools.working_directory(),
                tools.default_shell_name(),
                self.context_source.execution_environment(),
            )
            .full_item();
        Ok(ModelSessionState {
            workspace,
            tools,
            factory,
            conversation: ConversationState::empty(canonical_context),
            context,
            preserve_inherited_delta: false,
            pending_late_wake: None,
        })
    }

    fn attempt_factory(&self, tools: &ToolRuntime) -> Result<ResponsesAttemptFactory> {
        attempt_factory(
            &self.events,
            &self.transport_stats,
            &self.provider_session_id,
            self.prompt_cache.key(),
            tools,
            &self.config.system_prompt(),
        )
    }

    fn responses_endpoint(&self) -> &str {
        match self.config.responses_transport {
            ResponsesTransport::WebSocket => &self.config.websocket_url,
            ResponsesTransport::Https => &self.config.api_base_url,
        }
    }
}

pub(crate) fn prepare_checkpoint(
    checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    context_source: ContextSource,
) -> PreparedCheckpoint {
    let runtime = tool_runtime(checkpoint.workspace(), config, tools);
    let selected_agents_md = context_source
        .project_instructions(checkpoint.workspace())
        .map(Arc::from);
    PreparedCheckpoint {
        checkpoint,
        runtime,
        context_source,
        selected_agents_md,
    }
}

pub(crate) fn prepare_resumed_checkpoint(
    mut checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    session_id: &str,
    context_source: ContextSource,
) -> Result<PreparedCheckpoint> {
    if checkpoint.conversation.prepare_replay_images() {
        checkpoint.preserve_inherited_delta = false;
    }
    // Unstored response IDs are scoped to the live transport connection. A fork
    // owns a fresh client, so it must replay client-owned history instead.
    if !config.store_responses {
        checkpoint.conversation.reset_for_full_request();
    }
    checkpoint.global_instructions = context_source
        .global_instructions()
        .or(checkpoint.global_instructions);
    let runtime = tool_runtime(checkpoint.workspace(), config, tools);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&runtime, session_id);
    checkpoint.request_prefix = Arc::from(
        request_profile(
            session_id,
            checkpoint.prompt_cache_key(),
            tool_specs,
            code_mode_tool_names,
            &config.system_prompt(),
        )?
        .prefix()
        .to_vec(),
    );
    let selected_agents_md = context_source
        .project_instructions(checkpoint.workspace())
        .map(Arc::from);
    Ok(PreparedCheckpoint {
        checkpoint,
        runtime,
        context_source,
        selected_agents_md,
    })
}

pub(crate) fn prepare_history_checkpoint(
    resume: HistoryCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    session_id: &str,
    context_source: ContextSource,
) -> Result<PreparedCheckpoint> {
    let HistoryCheckpoint {
        workspace,
        provider_session_id,
        canonical_context,
        history,
        client_authored,
        unreal_function_outputs,
        prompt_cache_key,
        context_baseline,
    } = resume;
    let selected_agents_md = context_source
        .project_instructions(&workspace)
        .map(Arc::from);
    let runtime = tool_runtime(&workspace, config, tools);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&runtime, session_id);
    let request_prefix = request_profile(
        session_id,
        prompt_cache_key.as_ref(),
        tool_specs,
        code_mode_tool_names,
        &config.system_prompt(),
    )?
    .prefix()
    .to_vec();
    let checkpoint = ModelCheckpoint::resume(
        workspace,
        provider_session_id,
        request_prefix,
        prompt_cache_key,
        canonical_context,
        history,
        client_authored,
        unreal_function_outputs,
        context_source.global_instructions(),
        context_baseline,
    )?;
    Ok(PreparedCheckpoint {
        checkpoint,
        runtime,
        context_source,
        selected_agents_md,
    })
}

/// Stable identity across admission, replay and transcript rewrites. A model
/// completion clears the marker; only new terminal receipts advance it.
fn advance_late_wake(previous: Option<&str>, receipt_id: &str) -> String {
    const WAKE_NAMESPACE: uuid::Uuid =
        uuid::Uuid::from_u128(0xa01b8f32_68bf_49a0_b138_99ec17efca31);
    let wake_input = format!("{}:{receipt_id}", previous.unwrap_or(""));
    uuid::Uuid::new_v5(&WAKE_NAMESPACE, wake_input.as_bytes()).to_string()
}

#[cfg(test)]
mod context_accounting_snapshot_tests {
    use super::*;
    use crate::session::{CommittedSession, SessionSnapshot};

    #[test]
    fn unreal_pending_terminal_snapshot_replays_only_when_opted_in() {
        let history: Vec<ResponseItem> = serde_json::from_value(serde_json::json!([
            {"type":"message", "role":"user", "content":[{"type":"input_text", "text":"task"}]},
            {"type":"function_call", "call_id":"job-1", "name":"job", "arguments":"{}"},
            {"type":"function_call_output", "call_id":"job-1", "output": "Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."}
        ]))
        .unwrap();
        let prefix = serde_json::from_value(serde_json::json!([
            {"type":"additional_tools", "role":"developer", "tools":[]},
            {"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"instructions"}]}
        ])).unwrap();
        let mut checkpoint = ModelCheckpoint::resume(
            ".".into(),
            Arc::from("lineage"),
            prefix,
            Arc::from("cache"),
            history[0].clone(),
            history,
            Default::default(),
            true,
            None,
            None,
        )
        .unwrap();
        checkpoint.conversation.commit_tail();
        checkpoint
            .conversation
            .managed
            .complete_unreal_function_output(
                "job-1",
                nanocodex_oai_api::responses::FunctionOutputBody::Text("done".into()),
            )
            .unwrap();
        let snapshot =
            CommittedSession::new(Arc::from("lineage"), Model::Astra, checkpoint).snapshot();
        let encoded = serde_json::to_value(snapshot).unwrap();
        assert_eq!(encoded["unreal_function_outputs"], true);
        let restored: SessionSnapshot = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            restored
                .into_resume()
                .unwrap()
                .checkpoint
                .unwrap()
                .snapshot_history()
                .len(),
            4
        );
        let mut ordinary = encoded;
        ordinary
            .as_object_mut()
            .unwrap()
            .remove("unreal_function_outputs");
        let ordinary: SessionSnapshot = serde_json::from_value(ordinary).unwrap();
        assert!(ordinary.into_resume().is_err());
    }

    #[test]
    fn wake_marker_survives_snapshot_replay_and_transcript_tail_changes() {
        let history: Vec<ResponseItem> = serde_json::from_value(serde_json::json!([
            {"type":"message", "role":"user", "content":[{"type":"input_text", "text":"task"}]},
            {"type":"function_call", "call_id":"job-1", "name":"job", "arguments":"{}"},
            {"type":"function_call_output", "call_id":"job-1", "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."}
        ]))
        .unwrap();
        let prefix = serde_json::from_value(serde_json::json!([
            {"type":"additional_tools", "role":"developer", "tools":[]},
            {"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"instructions"}]}
        ])).unwrap();
        let mut checkpoint = ModelCheckpoint::resume(
            ".".into(),
            Arc::from("lineage"),
            prefix,
            Arc::from("cache"),
            history[0].clone(),
            history,
            Default::default(),
            true,
            None,
            None,
        )
        .unwrap();
        checkpoint.conversation.commit_tail();
        checkpoint
            .conversation
            .managed
            .complete_unreal_function_output_with_id(
                "job-1",
                FunctionOutputBody::Text("finished".into()),
                Some(ResponseItemId::from_server("late:first")),
            )
            .unwrap();
        let first = advance_late_wake(None, "late:first");
        let second = advance_late_wake(Some(&first), "late:second");
        assert_ne!(first, second);
        assert_eq!(second, advance_late_wake(Some(&first), "late:second"));
        checkpoint.restore_late_wake(Some(second.clone()));
        let snapshot =
            CommittedSession::new(Arc::from("lineage"), Model::Astra, checkpoint).snapshot();
        let encoded = serde_json::to_value(snapshot).unwrap();
        assert_eq!(encoded["pending_late_wake"], second);
        let restored: SessionSnapshot = serde_json::from_value(encoded).unwrap();
        assert!(
            restored
                .clone()
                .into_replayed_checkpoint("other", Model::Astra, Some("."))
                .is_err()
        );
        assert!(
            restored
                .clone()
                .into_replayed_checkpoint("lineage", Model::Sol, Some("."))
                .is_err()
        );
        assert!(
            restored
                .clone()
                .into_replayed_checkpoint("lineage", Model::Astra, Some("/other"))
                .is_err()
        );
        let mut replay = restored
            .into_replayed_checkpoint("lineage", Model::Astra, Some("."))
            .unwrap();
        assert_eq!(replay.late_wake_id(), Some(second.as_str()));
        replay.conversation.append([ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: "later developer context".into(),
            }],
        )]);
        replay.conversation.commit_tail();
        assert_eq!(replay.late_wake_id(), Some(second.as_str()));
        let replayed = CommittedSession::new(Arc::from("lineage"), Model::Astra, replay).snapshot();
        assert_eq!(
            serde_json::to_value(replayed).unwrap()["pending_late_wake"],
            second
        );
    }

    #[test]
    fn snapshot_preserves_context_accounting_and_accepts_legacy_snapshots() {
        let history: Vec<ResponseItem> = serde_json::from_value(serde_json::json!([
            {"type":"reasoning", "summary":[], "encrypted_content":"x".repeat(1200)},
            {"type":"message", "role":"user", "content":[{"type":"input_text", "text":"synthetic task"}]},
            {"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"synthetic answer"}]}
        ])).unwrap();
        let prefix = serde_json::from_value(serde_json::json!([
            {"type":"additional_tools", "role":"developer", "tools":[]},
            {"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"synthetic instructions"}]}
        ])).unwrap();
        let mut checkpoint = ModelCheckpoint::resume(
            ".".into(),
            Arc::from("synthetic-lineage"),
            prefix,
            Arc::from("synthetic-cache"),
            history[1].clone(),
            history,
            Default::default(),
            false,
            None,
            None,
        )
        .unwrap();
        checkpoint.conversation.update_token_info(Some(&Usage {
            total_tokens: 265639,
            ..Usage::default()
        }));
        checkpoint.conversation.observe_server_reasoning(true);
        let snapshot =
            CommittedSession::new(Arc::from("synthetic-lineage"), Model::Astra, checkpoint)
                .snapshot();
        let encoded = serde_json::to_value(snapshot).unwrap();
        let restored: SessionSnapshot = serde_json::from_value(encoded.clone()).unwrap();
        let restored = restored.into_resume().unwrap().checkpoint.unwrap();
        assert_eq!(restored.conversation.active_context_tokens(), 265639);
        assert!(restored.conversation.managed.context_usage().1);
        assert!(restored.conversation.previous_response_id().is_none());
        assert!(restored.conversation.active_context_tokens() >= 244800);

        // History replacement stores an all-history estimate, not a provider baseline.
        let mut estimated = restored;
        estimated
            .conversation
            .managed
            .replace_prepared_history(vec![
                ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("answer")]),
                ResponseItem::message(
                    MessageRole::User,
                    [ContentItem::input_text("x".repeat(600000))],
                ),
            ]);
        let expected = estimated.conversation.active_context_tokens();
        let snapshot =
            CommittedSession::new(Arc::from("synthetic-lineage"), Model::Astra, estimated)
                .snapshot();
        let decoded: SessionSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let restored = decoded.into_resume().unwrap().checkpoint.unwrap();
        assert!(restored.conversation.managed.context_usage_is_estimate());
        assert_eq!(restored.conversation.active_context_tokens(), expected);
        assert!(expected < 244800);
        let mut legacy = encoded;
        legacy.as_object_mut().unwrap().remove("context_usage");
        let legacy: SessionSnapshot = serde_json::from_value(legacy).unwrap();
        assert!(legacy.into_resume().is_ok());
    }
}
