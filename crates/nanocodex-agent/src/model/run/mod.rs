mod lifecycle;
mod responses;
mod state;
mod tool_calls;
mod turn;

use lifecycle::*;
use responses::*;
use state::*;
use tool_calls::*;

use std::{
    any::Any,
    collections::HashMap,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{Arc, Mutex},
};

use futures_util::{FutureExt, StreamExt, stream::FuturesOrdered};
use nanocodex_oai_api::{
    __private::{
        EventSink, ManagedSessionState, ModelConfig, ResponsesAttemptFactory,
        assign_missing_response_item_id, compaction, with_code_mode_tool_names,
    },
    CONTEXT_WINDOW_TOKENS, MODEL, Prompt, Thinking,
    events::AgentEventKind,
    pricing::{ServiceTier, estimate},
    responses::{ContentItem, MessageRole, RequestProfile, ResponseItem, ToolDefinition, Usage},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput as TurnResult, ResponsesAttempt, ResponsesClient,
        ResponsesOutput, ResponsesServiceResponse,
    },
    transport::{ResponsesError, ResponsesTransport, TransportStats},
};
use serde::Serialize;
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
        task_input, tool_search_output, turn_aborted,
    },
    terminal_payload,
};
use crate::{
    NanocodexError, Result,
    agent::{AgentSend, ContextSource},
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
    config: Arc<ModelConfig>,
    thinking: Thinking,
    fast_mode: bool,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    started_at: Instant,
    stats: RunStats,
    session: Option<ModelSessionState>,
    active_tools: Option<ToolRuntimeControl>,
    active_tool_calls: Vec<ActiveToolCall>,
    active_tool_batch_started_at: Option<Instant>,
    tool_call_indices: HashMap<Box<str>, u32>,
    tools: Tools,
    prompt_cache: ModelPromptCache,
    context_source: ContextSource,
    global_instructions: Option<Arc<str>>,
    force_compaction: bool,
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
    conversation: ConversationState,
    request_prefix: Arc<[ResponseItem]>,
    prompt_cache_key: Arc<str>,
    preserve_inherited_delta: bool,
    global_instructions: Option<Arc<str>>,
    context_baseline: ContextBaseline,
}

pub(crate) struct PreparedCheckpoint {
    pub(crate) checkpoint: ModelCheckpoint,
    pub(crate) runtime: ToolRuntime,
    pub(crate) context_source: ContextSource,
    selected_agents_md: Option<Arc<str>>,
}

pub(crate) struct HistoryCheckpoint {
    pub(crate) workspace: String,
    pub(crate) canonical_context: ResponseItem,
    pub(crate) history: Vec<ResponseItem>,
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

    #[allow(dead_code, reason = "consumed by the native durability boundary only")]
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

    pub(crate) fn snapshot_history(&self) -> Vec<ResponseItem> {
        self.conversation.flattened_history()
    }

    pub(crate) const fn context_baseline(&self) -> &ContextBaseline {
        &self.context_baseline
    }

    pub(crate) fn resume(
        workspace: String,
        mut request_prefix: Vec<ResponseItem>,
        prompt_cache_key: Arc<str>,
        canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
        global_instructions: Option<Arc<str>>,
        context_baseline: Option<ContextBaseline>,
    ) -> Result<Self> {
        assign_request_prefix_ids(&mut request_prefix);
        let context_baseline =
            context_baseline.unwrap_or_else(|| ContextBaseline::reconstruct(&history));
        Ok(Self {
            workspace,
            conversation: ConversationState::resume(canonical_context, history)?,
            request_prefix: Arc::from(request_prefix),
            prompt_cache_key,
            preserve_inherited_delta: false,
            global_instructions,
            context_baseline,
        })
    }
}

impl<S> ModelRun<S> {
    pub(crate) fn new(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        context_source: ContextSource,
    ) -> Self {
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            config,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            session: None,
            active_tools: None,
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            global_instructions,
            force_compaction: false,
        }
    }

    pub(crate) fn from_checkpoint(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        prepared: PreparedCheckpoint,
    ) -> Self {
        let PreparedCheckpoint {
            checkpoint,
            runtime,
            context_source,
            selected_agents_md,
        } = prepared;
        let active_tools = runtime.control();
        let (_, code_mode_tool_names) = model_tool_contract(&runtime, events.request_id());
        let factory = ResponsesAttemptFactory::new(
            with_code_mode_tool_names(
                RequestProfile::new(
                    events.request_id(),
                    checkpoint.prompt_cache_key.to_string(),
                    Arc::clone(&checkpoint.request_prefix),
                ),
                code_mode_tool_names,
            ),
            events.clone(),
            Arc::clone(&transport_stats),
        );
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let context_source =
            context_source.with_fallback_global(checkpoint.global_instructions.clone());
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            config,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            session: Some(ModelSessionState {
                workspace: checkpoint.workspace,
                tools: runtime,
                factory,
                conversation: checkpoint.conversation,
                context: ContextState::new(selected_agents_md, checkpoint.context_baseline),
                preserve_inherited_delta: checkpoint.preserve_inherited_delta,
            }),
            active_tools: Some(active_tools),
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            global_instructions,
            force_compaction: false,
        }
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
        let factory = self.attempt_factory(&tools);
        let context = ContextState::new(selected_agents_md, ContextBaseline::Missing);
        let canonical_context = context
            .capture(tools.working_directory(), tools.default_shell_name())
            .full_item();
        Ok(ModelSessionState {
            workspace,
            tools,
            factory,
            conversation: ConversationState::empty(canonical_context),
            context,
            preserve_inherited_delta: false,
        })
    }

    fn attempt_factory(&self, tools: &ToolRuntime) -> ResponsesAttemptFactory {
        attempt_factory(
            &self.events,
            &self.transport_stats,
            self.prompt_cache.key(),
            tools,
            self.config.system_prompt(),
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
    checkpoint.global_instructions = context_source
        .global_instructions()
        .or(checkpoint.global_instructions);
    let prepared = prepare_checkpoint(checkpoint, config, tools, context_source);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&prepared.runtime, session_id);
    let expected = request_profile(
        "resume-validation",
        "resume-validation",
        tool_specs,
        code_mode_tool_names,
        config.system_prompt(),
    );
    let expected =
        serde_json::to_vec(&without_response_item_ids(expected.prefix())).map_err(|error| {
            NanocodexError::InvalidSessionSnapshot(format!(
                "failed to validate the request prefix: {error}"
            ))
        })?;
    let stored = serde_json::to_vec(&without_response_item_ids(
        prepared.checkpoint.request_prefix(),
    ))
    .map_err(|error| {
        NanocodexError::InvalidSessionSnapshot(format!(
            "failed to validate the stored request prefix: {error}"
        ))
    })?;
    if expected != stored {
        return Err(NanocodexError::InvalidSessionSnapshot(
            "instructions or tool definitions do not match the resumed session".to_owned(),
        ));
    }
    Ok(prepared)
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
        canonical_context,
        history,
        prompt_cache_key,
        context_baseline,
    } = resume;
    let selected_agents_md = context_source
        .project_instructions(&workspace)
        .map(Arc::from);
    let runtime = tool_runtime(&workspace, config, tools);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&runtime, session_id);
    let request_prefix = request_profile(
        "history-resume",
        "history-resume",
        tool_specs,
        code_mode_tool_names,
        config.system_prompt(),
    )
    .prefix()
    .to_vec();
    let checkpoint = ModelCheckpoint::resume(
        workspace,
        request_prefix,
        prompt_cache_key,
        canonical_context,
        history,
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

fn without_response_item_ids(items: &[ResponseItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            item.strip_id();
            item
        })
        .collect()
}
