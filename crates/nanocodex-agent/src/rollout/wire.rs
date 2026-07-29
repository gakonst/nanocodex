use super::*;

#[derive(Serialize)]
pub(super) struct RolloutLine<T> {
    pub(super) timestamp: String,
    #[serde(flatten)]
    pub(super) item: T,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub(super) enum RolloutItem<'a> {
    SessionMeta(&'a SessionMeta),
    #[serde(rename = "event_msg")]
    Event(&'a CodexEvent<'a>),
    ResponseItem(&'a ResponseItem),
    WorldState(&'a WorldStateItem<'a>),
    Compacted(&'a CompactedItem),
}

#[derive(Clone, Serialize)]
pub(super) struct UserMessage {
    pub(super) message: String,
    pub(super) images: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) image_details: Vec<Option<ImageDetail>>,
    pub(super) local_images: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) local_image_details: Vec<Option<ImageDetail>>,
    pub(super) text_elements: Vec<serde_json::Value>,
}

impl UserMessage {
    pub(super) fn from_prompt(prompt: &Prompt) -> Self {
        let mut message = String::new();
        let mut images = Vec::new();
        let mut image_details = Vec::new();
        let mut local_images = Vec::new();
        let mut local_image_details = Vec::new();
        match &prompt.instruction {
            PromptInput::Text(text) => message.push_str(text),
            PromptInput::Content(items) => {
                for item in items {
                    match item {
                        UserInput::Text { text } => message.push_str(text),
                        UserInput::Image { image_url, detail } => {
                            images.push(image_url.clone());
                            image_details.push(*detail);
                        }
                        UserInput::LocalImage { path, detail } => {
                            local_images.push(path.clone());
                            local_image_details.push(*detail);
                        }
                        UserInput::Audio { audio_url } => {
                            message.push_str("[Audio: ");
                            message.push_str(audio_url);
                            message.push(']');
                        }
                        UserInput::LocalAudio { path } => {
                            message.push_str("[Audio: ");
                            message.push_str(&path.display().to_string());
                            message.push(']');
                        }
                    }
                }
            }
        }
        Self {
            message,
            images,
            image_details,
            local_images,
            local_image_details,
            text_elements: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum CodexEvent<'a> {
    TaskStarted {
        turn_id: &'a str,
        started_at: i64,
        model_context_window: Option<i64>,
        collaboration_mode_kind: &'static str,
    },
    UserMessage(&'a UserMessage),
    AgentMessage {
        message: &'a str,
        phase: &'static str,
        memory_citation: Option<serde_json::Value>,
    },
    TaskComplete {
        turn_id: &'a str,
        last_agent_message: Option<&'a str>,
        started_at: i64,
        completed_at: Option<i64>,
        duration_ms: Option<i64>,
        time_to_first_token_ms: Option<i64>,
    },
    TurnAborted {
        turn_id: &'a str,
        reason: &'static str,
        started_at: i64,
        completed_at: Option<i64>,
        duration_ms: Option<i64>,
    },
}

#[derive(Serialize)]
pub(super) struct BaseInstructions {
    pub(super) text: String,
}

#[derive(Serialize)]
pub(super) struct SessionContextWindow {
    pub(super) window_id: String,
}

#[derive(Serialize)]
pub(super) struct SessionMeta {
    pub(super) session_id: String,
    pub(super) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parent_thread_id: Option<String>,
    pub(super) timestamp: String,
    pub(super) cwd: PathBuf,
    pub(super) originator: String,
    pub(super) cli_version: String,
    pub(super) source: &'static str,
    pub(super) thread_source: &'static str,
    pub(super) model_provider: &'static str,
    pub(super) base_instructions: BaseInstructions,
    pub(super) history_mode: &'static str,
    pub(super) context_window: SessionContextWindow,
}

#[derive(Serialize)]
pub(super) struct CompactedItem {
    pub(super) message: String,
    pub(super) replacement_history: Vec<ResponseItem>,
    pub(super) window_number: u64,
    pub(super) first_window_id: String,
    pub(super) previous_window_id: String,
    pub(super) window_id: String,
}

#[derive(Serialize)]
pub(super) struct WorldStateItem<'a> {
    pub(super) full: bool,
    pub(super) state: PersistedContextState<'a>,
}

#[derive(Serialize)]
pub(super) struct PersistedContextState<'a> {
    pub(super) nanocodex_context: &'a ContextBaseline,
}
