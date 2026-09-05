#![doc = include_str!("../README.md")]

use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use futures_util::StreamExt;
use nanocodex::{
    Nanocodex, NanocodexError, OpenAi, PromptRoute, TurnControl,
    agent::events::{AgentEvent, AgentEventData, AssistantEvent, RunEvent},
    oai::{
        auth::OpenAiAuthMode,
        realtime::{
            RealtimeAgentSteer, RealtimeError, RealtimeEvent, RealtimeSession,
            RealtimeTranscriptEntry,
        },
        responses::MessagePhase,
    },
};
use nanocodex_voice_protocol::{
    TranscriptEntry as ProtocolTranscriptEntry,
    realtime_delegation as protocol_realtime_delegation,
    realtime_tail_delegation as protocol_realtime_tail_delegation,
};
use tokio::sync::{mpsc, oneshot};

pub use nanocodex_voice_protocol::{REALTIME_END_INSTRUCTIONS, REALTIME_START_INSTRUCTIONS};

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod audio;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[allow(clippy::missing_const_for_fn)]
#[path = "audio_unsupported.rs"]
mod audio;
mod startup_context;

pub use nanocodex::oai::realtime::{
    CHATGPT_REALTIME_VOICE, CHATGPT_REALTIME_VOICES, PLATFORM_REALTIME_VOICE,
    PLATFORM_REALTIME_VOICES, RealtimeInitialItem, RealtimeInputTextRole, RealtimeOutputModality,
    RealtimeResponseHandoffMode, RealtimeSessionMode, RealtimeTextRole, RealtimeTransport,
    RealtimeVersion, RealtimeVoice,
};

use audio::VoiceAudio;

const CODEX_BACKEND_PROMPT: &str =
    nanocodex_voice_protocol::CHATGPT_REALTIME_BACKEND_PROMPT_TEMPLATE;
const USER_FIRST_NAME_PLACEHOLDER: &str = "{{ user_first_name }}";
const DEFAULT_USER_FIRST_NAME: &str = "there";
const HANDOFF_STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const VOICE_COMMAND_CAPACITY: usize = 64;
const REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET: usize = 1_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const HANDOFF_STREAM_TRUNCATION_MARKER: &str = "\n…output truncated…\n";
/// Desktop capture and playback policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioConfig {
    playback_prebuffer: Duration,
    maximum_playback_buffer: Duration,
}

impl AudioConfig {
    /// Creates a desktop audio policy with the requested playout buffering.
    #[must_use]
    pub const fn new(playback_prebuffer: Duration, maximum_playback_buffer: Duration) -> Self {
        Self {
            playback_prebuffer,
            maximum_playback_buffer,
        }
    }

    /// Returns the audio accumulated before playout begins or resumes.
    #[must_use]
    pub const fn playback_prebuffer(self) -> Duration {
        self.playback_prebuffer
    }

    /// Returns the maximum decoded audio retained for playout.
    #[must_use]
    pub const fn maximum_playback_buffer(self) -> Duration {
        self.maximum_playback_buffer
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self::new(Duration::from_millis(120), Duration::from_secs(8))
    }
}

/// The participant associated with a completed voice transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoiceSpeaker {
    /// The local microphone user.
    User,
    /// The Realtime voice assistant.
    Assistant,
}

impl std::fmt::Display for VoiceSpeaker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        })
    }
}

/// One typed update from an experimental desktop voice lifecycle.
#[derive(Debug)]
pub enum VoiceEvent {
    /// The Realtime transport is connecting.
    Connecting,
    /// The Realtime transport and default audio devices are active.
    Started {
        /// The selected output voice.
        voice: RealtimeVoice,
    },
    /// A participant's completed transcript.
    Transcript {
        /// The participant that produced the transcript.
        speaker: VoiceSpeaker,
        /// The complete transcript text.
        text: String,
    },
    /// The voice lifecycle failed and stopped.
    Failed {
        /// The terminal typed failure.
        error: VoiceFailure,
    },
    /// The voice lifecycle stopped cleanly.
    Stopped,
}

/// Receiver for one independent desktop voice event stream.
pub struct VoiceEvents {
    receiver: mpsc::UnboundedReceiver<VoiceEvent>,
}

impl VoiceEvents {
    /// Waits for the next lifecycle or transcript update.
    pub async fn recv(&mut self) -> Option<VoiceEvent> {
        self.receiver.recv().await
    }

    /// Attempts to receive an already-buffered update.
    pub fn try_recv(&mut self) -> Option<VoiceEvent> {
        self.receiver.try_recv().ok()
    }
}

/// A running desktop voice lifecycle.
pub struct VoiceSession {
    stop: Option<oneshot::Sender<()>>,
    finished: Option<oneshot::Receiver<Result<(), String>>>,
    agent_events: mpsc::UnboundedSender<AgentEvent>,
    commands: mpsc::Sender<VoiceCommand>,
    agent_control: VoiceAgentControl,
    task: Option<std::thread::JoinHandle<()>>,
}

impl VoiceSession {
    /// Returns whether the owned voice thread is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Requests a clean stop without blocking the caller.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Requests a clean stop and joins the owned voice lifecycle.
    ///
    /// This waits for media cleanup, the Realtime close handshake,
    /// transcript-tail routing, and the agent lifecycle end marker.
    ///
    /// # Errors
    ///
    /// Returns the terminal lifecycle failure or a voice-thread join failure.
    pub async fn shutdown(&mut self) -> Result<(), VoiceShutdownError> {
        self.stop();
        let outcome = match self.finished.take() {
            Some(finished) => finished
                .await
                .map_err(|_| VoiceShutdownError::CompletionChannel)?,
            None => Ok(()),
        };
        if let Some(task) = self.task.take() {
            task.join()
                .map_err(|_| VoiceShutdownError::ThreadPanicked)?;
        }
        outcome.map_err(VoiceShutdownError::Lifecycle)
    }

    /// Returns a reusable controller for coding turns started by this voice lifecycle.
    ///
    /// An embedding can retain this controller across voice reconnects and route its
    /// normal interrupt gesture through [`VoiceAgentControl::cancel`].
    #[must_use]
    pub fn agent_control(&self) -> VoiceAgentControl {
        self.agent_control.clone()
    }

    /// Cancels the unfinished coding turn started by this voice lifecycle, if any.
    ///
    /// Returns `true` when cancellation was accepted and `false` when no voice-owned
    /// turn remains. A turn completing concurrently with cancellation is treated as
    /// already settled rather than as an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent driver rejects cancellation for another reason.
    pub async fn cancel_agent_turn(&self) -> Result<bool, NanocodexError> {
        self.agent_control.cancel().await
    }

    /// Appends role-bearing text to the active realtime conversation.
    ///
    /// # Errors
    ///
    /// Returns an error when the voice lifecycle or bounded Realtime queue has
    /// closed.
    pub async fn append_text(
        &self,
        role: RealtimeInputTextRole,
        text: impl Into<String>,
    ) -> Result<(), RealtimeError> {
        let (result, completed) = oneshot::channel();
        self.commands
            .send(VoiceCommand::AppendText {
                role,
                text: text.into(),
                result,
            })
            .await
            .map_err(|_| RealtimeError::Closed)?;
        completed.await.map_err(|_| RealtimeError::Closed)?
    }

    /// Appends text that the realtime model should treat as directly speakable.
    ///
    /// # Errors
    ///
    /// Returns an error when the voice lifecycle or bounded Realtime queue has
    /// closed.
    pub async fn append_speech(&self, text: impl Into<String>) -> Result<(), RealtimeError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Ok(());
        }
        let (result, completed) = oneshot::channel();
        self.commands
            .send(VoiceCommand::AppendSpeech { text, result })
            .await
            .map_err(|_| RealtimeError::Closed)?;
        completed.await.map_err(|_| RealtimeError::Closed)?
    }

    /// Mirrors one session-wide event from work started outside this lifecycle.
    ///
    /// Embeddings with typed input should pass the agent's normal [`AgentEvent`]
    /// stream here while voice is active. Output joins an active handoff when
    /// one exists, or is sent to Realtime as a standalone result. Events for
    /// turns started by this voice session are already mirrored internally.
    #[must_use]
    pub fn observe_agent_event(&self, event: AgentEvent) -> bool {
        self.agent_events.send(event).is_ok()
    }
}

/// Reusable interrupt capability for coding turns launched by a voice session.
///
/// Share one controller across replacement [`VoiceSession`]s when the embedding
/// wants its normal interrupt action to keep targeting work started before a
/// Realtime reconnect.
#[derive(Clone, Default)]
pub struct VoiceAgentControl {
    state: Arc<Mutex<VoiceAgentControlState>>,
}

impl VoiceAgentControl {
    /// Returns whether a voice-started coding turn is currently retained.
    #[must_use]
    pub fn has_active_turn(&self) -> bool {
        self.active().is_some()
    }

    /// Cancels the retained voice-started coding turn, if any.
    ///
    /// Returns `true` when cancellation was accepted and `false` when no turn
    /// remains. Completion racing with cancellation is an idempotent success.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent driver rejects cancellation for another reason.
    pub async fn cancel(&self) -> Result<bool, NanocodexError> {
        let Some(active) = self.active() else {
            return Ok(false);
        };
        match active.control.cancel().await {
            Ok(()) => Ok(true),
            Err(NanocodexError::TurnNotCancellable) => {
                self.clear(active.generation);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn active(&self) -> Option<ActiveVoiceAgentTurn> {
        self.lock().active.clone()
    }

    fn install(&self, control: TurnControl) -> u64 {
        let mut state = self.lock();
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        state.active = Some(ActiveVoiceAgentTurn {
            generation,
            control,
        });
        generation
    }

    fn clear(&self, generation: u64) {
        let mut state = self.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            state.active = None;
        }
    }

    fn lock(&self) -> MutexGuard<'_, VoiceAgentControlState> {
        match self.state.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Clone)]
struct ActiveVoiceAgentTurn {
    generation: u64,
    control: TurnControl,
}

#[derive(Default)]
struct VoiceAgentControlState {
    active: Option<ActiveVoiceAgentTurn>,
    next_generation: u64,
}

impl Drop for VoiceSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Builder for one reusable desktop voice-to-agent lifecycle.
pub struct VoiceSessionBuilder {
    openai: OpenAi,
    agent: Nanocodex,
    instructions: Arc<str>,
    session_id: Option<Arc<str>>,
    attestation_header: Option<Arc<str>>,
    voice: Option<RealtimeVoice>,
    model: Option<String>,
    version: Option<RealtimeVersion>,
    transport: Option<RealtimeTransport>,
    session_mode: RealtimeSessionMode,
    output_modality: RealtimeOutputModality,
    client_managed_handoffs: bool,
    delegation_ack_filler: Option<bool>,
    codex_responses_as_items: bool,
    codex_response_item_prefix: Option<String>,
    codex_response_handoff_mode: RealtimeResponseHandoffMode,
    codex_response_handoff_channel_prefixes: BTreeMap<String, Vec<String>>,
    initial_items: Vec<RealtimeInitialItem>,
    include_startup_context: bool,
    flush_transcript_tail_on_session_end: bool,
    audio: AudioConfig,
    agent_control: VoiceAgentControl,
}

impl VoiceSessionBuilder {
    /// Creates a voice lifecycle over an existing OpenAI recipe and agent.
    #[must_use]
    pub fn new(openai: OpenAi, agent: Nanocodex) -> Self {
        Self {
            openai,
            agent,
            instructions: Arc::from(codex_voice_instructions()),
            session_id: None,
            attestation_header: None,
            voice: None,
            model: None,
            version: None,
            transport: None,
            session_mode: RealtimeSessionMode::Conversational,
            output_modality: RealtimeOutputModality::Audio,
            client_managed_handoffs: false,
            delegation_ack_filler: None,
            codex_responses_as_items: false,
            codex_response_item_prefix: None,
            codex_response_handoff_mode: RealtimeResponseHandoffMode::Thinking,
            codex_response_handoff_channel_prefixes: BTreeMap::new(),
            initial_items: Vec::new(),
            include_startup_context: true,
            flush_transcript_tail_on_session_end: true,
            audio: AudioConfig::default(),
            agent_control: VoiceAgentControl::default(),
        }
    }

    /// Replaces the voice model's developer instructions.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.instructions = instructions.into();
        self
    }

    /// Supplies a stable caller-owned identity for transport correlation.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<Arc<str>>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Supplies a host-generated ChatGPT device-attestation value.
    #[must_use]
    pub fn attestation_header(mut self, value: impl Into<Arc<str>>) -> Self {
        self.attestation_header = Some(value.into());
        self
    }

    /// Selects an explicit output voice.
    #[must_use]
    pub const fn voice(mut self, voice: RealtimeVoice) -> Self {
        self.voice = Some(voice);
        self
    }

    /// Selects the realtime model explicitly.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Selects realtime protocol V1, V2, or V3.
    #[must_use]
    pub const fn version(mut self, version: RealtimeVersion) -> Self {
        self.version = Some(version);
        self
    }

    /// Selects WebSocket or owned WebRTC transport.
    #[must_use]
    pub const fn transport(mut self, transport: RealtimeTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Selects conversational or transcription-only operation.
    #[must_use]
    pub const fn session_mode(mut self, mode: RealtimeSessionMode) -> Self {
        self.session_mode = mode;
        self
    }

    /// Selects text or synthesized-audio model output.
    #[must_use]
    pub const fn output_modality(mut self, modality: RealtimeOutputModality) -> Self {
        self.output_modality = modality;
        self
    }

    /// Lets the embedding manage all coding-agent handoff output itself.
    #[must_use]
    pub const fn client_managed_handoffs(mut self, managed: bool) -> Self {
        self.client_managed_handoffs = managed;
        self
    }

    /// Controls the provider's Frameless delegation acknowledgement filler.
    ///
    /// Omitted policy preserves the Realtime API default. Realtime V1 and V2
    /// ignore it.
    #[must_use]
    pub const fn delegation_ack_filler(mut self, enabled: bool) -> Self {
        self.delegation_ack_filler = Some(enabled);
        self
    }

    /// Sends automatic coding-agent responses as realtime conversation items.
    #[must_use]
    pub const fn codex_responses_as_items(mut self, as_items: bool) -> Self {
        self.codex_responses_as_items = as_items;
        self
    }

    /// Prefixes automatic coding-agent response items.
    #[must_use]
    pub fn codex_response_item_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.codex_response_item_prefix = Some(prefix.into());
        self
    }

    /// Selects thinking, commentary, or BEM-tag handoff routing.
    #[must_use]
    pub const fn codex_response_handoff_mode(mut self, mode: RealtimeResponseHandoffMode) -> Self {
        self.codex_response_handoff_mode = mode;
        self
    }

    /// Replaces BEM prefixes keyed by `analysis`, `commentary`, and `final`.
    #[must_use]
    pub fn codex_response_handoff_channel_prefixes(
        mut self,
        prefixes: BTreeMap<String, Vec<String>>,
    ) -> Self {
        self.codex_response_handoff_channel_prefixes = prefixes;
        self
    }

    /// Replaces the role-bearing text history used to seed ChatGPT voice.
    #[must_use]
    pub fn initial_items(mut self, items: impl IntoIterator<Item = RealtimeInitialItem>) -> Self {
        self.initial_items = items.into_iter().collect();
        self
    }

    /// Appends one role-bearing text item to the ChatGPT voice bootstrap.
    #[must_use]
    pub fn initial_item(mut self, role: RealtimeTextRole, text: impl Into<String>) -> Self {
        self.initial_items
            .push(RealtimeInitialItem::new(role, text));
        self
    }

    /// Enables or disables Codex's bounded realtime startup context.
    ///
    /// This defaults to `true` and includes current-thread history, recent
    /// rollout work, and a shallow workspace map in the voice model prompt.
    #[must_use]
    pub const fn include_startup_context(mut self, include: bool) -> Self {
        self.include_startup_context = include;
        self
    }

    /// Enables or disables routing an unconsumed transcript tail when voice ends.
    #[must_use]
    pub const fn flush_transcript_tail_on_session_end(mut self, flush: bool) -> Self {
        self.flush_transcript_tail_on_session_end = flush;
        self
    }

    /// Replaces desktop capture and playback policy.
    #[must_use]
    pub const fn audio_config(mut self, audio: AudioConfig) -> Self {
        self.audio = audio;
        self
    }

    /// Shares a voice-started turn controller with the embedding.
    ///
    /// Reuse the same controller for replacement sessions so an interrupt can
    /// still cancel a coding turn launched before the voice transport changed.
    #[must_use]
    pub fn agent_control(mut self, control: VoiceAgentControl) -> Self {
        self.agent_control = control;
        self
    }

    /// Spawns the owned desktop lifecycle and its independent event stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the lifecycle thread cannot be created. Runtime,
    /// transport, and audio-device failures are delivered through
    /// [`VoiceEvent::Failed`].
    pub fn spawn(self) -> Result<(VoiceSession, VoiceEvents), VoiceError> {
        let (events, receiver) = mpsc::unbounded_channel();
        let (agent_events, observed_agent_events) = mpsc::unbounded_channel();
        let (commands, voice_commands) = mpsc::channel(VOICE_COMMAND_CAPACITY);
        let (stop, stopped) = oneshot::channel();
        let (finished, completion) = oneshot::channel();
        let agent_control = self.agent_control.clone();
        let task = std::thread::Builder::new()
            .name("nanocodex-voice".to_owned())
            .spawn(move || {
                run_thread(
                    self,
                    events,
                    observed_agent_events,
                    voice_commands,
                    stopped,
                    finished,
                );
            })
            .map_err(VoiceError::Spawn)?;
        Ok((
            VoiceSession {
                stop: Some(stop),
                finished: Some(completion),
                agent_events,
                commands,
                agent_control,
                task: Some(task),
            },
            VoiceEvents { receiver },
        ))
    }
}

/// Renders Codex's default Realtime backend prompt for the local user.
#[must_use]
pub fn codex_voice_instructions() -> String {
    CODEX_BACKEND_PROMPT
        .trim_end()
        .replace(USER_FIRST_NAME_PLACEHOLDER, &current_user_first_name())
}

fn current_user_first_name() -> String {
    [whoami::realname(), whoami::username()]
        .into_iter()
        .filter_map(|name| name.split_whitespace().next().map(str::to_owned))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| DEFAULT_USER_FIRST_NAME.to_owned())
}

/// Failure to create the owned desktop voice lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    /// The lifecycle thread could not be created.
    #[error("failed to spawn desktop voice thread: {0}")]
    Spawn(#[source] io::Error),
}

/// Failure while joining an owned desktop voice lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum VoiceShutdownError {
    /// The lifecycle stopped with a runtime, transport, audio, or agent failure.
    #[error("voice lifecycle failed: {0}")]
    Lifecycle(String),
    /// The lifecycle thread panicked.
    #[error("voice lifecycle thread panicked")]
    ThreadPanicked,
    /// The lifecycle exited without publishing its terminal result.
    #[error("voice lifecycle completion channel closed")]
    CompletionChannel,
}

/// Terminal failure from an active desktop voice lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum VoiceFailure {
    /// The dedicated async runtime could not be initialized.
    #[error("failed to create voice runtime: {0}")]
    Runtime(String),
    /// The coding-agent session rejected a lifecycle context update.
    #[error(transparent)]
    Agent(#[from] NanocodexError),
    /// The GPT Realtime transport failed.
    #[error(transparent)]
    Realtime(#[from] RealtimeError),
    /// The Realtime session reported a provider-side failure event.
    #[error("GPT Realtime reported an error: {0}")]
    Provider(String),
    /// Default-device capture or playback failed.
    #[error(transparent)]
    Audio(#[from] AudioError),
    /// The default microphone stream ended unexpectedly.
    #[error("microphone stream stopped")]
    MicrophoneStopped,
}

/// Failure to configure or operate native audio devices.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// No default microphone was available.
    #[error("no default microphone is available")]
    NoInputDevice,
    /// No default speaker was available.
    #[error("no default audio output is available")]
    NoOutputDevice,
    /// The requested audio policy was invalid.
    #[error("invalid desktop audio policy: {0}")]
    InvalidConfig(&'static str),
    /// The platform audio backend rejected an operation.
    #[error("{operation}: {message}")]
    Backend {
        /// The failed operation.
        operation: &'static str,
        /// Backend diagnostic text.
        message: String,
    },
    /// Default-device ownership is not implemented on this target.
    #[error(
        "default microphone/speaker capture is currently supported on macOS and Windows; use nanocodex-oai-api's PCM Realtime API with a platform adapter"
    )]
    UnsupportedPlatform,
}

fn run_thread(
    builder: VoiceSessionBuilder,
    events: mpsc::UnboundedSender<VoiceEvent>,
    observed_agent_events: mpsc::UnboundedReceiver<AgentEvent>,
    voice_commands: mpsc::Receiver<VoiceCommand>,
    stopped: oneshot::Receiver<()>,
    finished: oneshot::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let message = error.to_string();
            send_event(
                &events,
                VoiceEvent::Failed {
                    error: VoiceFailure::Runtime(message.clone()),
                },
            );
            drop(finished.send(Err(message)));
            return;
        }
    };
    let result = runtime.block_on(run_voice(
        builder,
        &events,
        observed_agent_events,
        voice_commands,
        stopped,
    ));
    let completion = result.as_ref().map_err(ToString::to_string).copied();
    let terminal = match result {
        Ok(()) => VoiceEvent::Stopped,
        Err(error) => VoiceEvent::Failed { error },
    };
    send_event(&events, terminal);
    drop(finished.send(completion));
}

async fn run_voice(
    mut builder: VoiceSessionBuilder,
    events: &mpsc::UnboundedSender<VoiceEvent>,
    observed_agent_events: mpsc::UnboundedReceiver<AgentEvent>,
    voice_commands: mpsc::Receiver<VoiceCommand>,
    stopped: oneshot::Receiver<()>,
) -> Result<(), VoiceFailure> {
    let lifecycle_agent = builder.agent.clone();
    let context = lifecycle_agent
        .append_developer_message(REALTIME_START_INSTRUCTIONS)
        .await?;
    if builder.include_startup_context {
        let rollout = lifecycle_agent
            .rollout()
            .map(|rollout| rollout.path().to_path_buf());
        if let Some(startup) = startup_context::build(&context, rollout.as_deref()) {
            builder.instructions = Arc::from(format!("{}\n\n{startup}", builder.instructions));
        }
    }
    let result = run_active_voice(
        builder,
        events,
        observed_agent_events,
        voice_commands,
        stopped,
    )
    .await;
    let ended = lifecycle_agent
        .append_developer_message(REALTIME_END_INSTRUCTIONS)
        .await
        .map(|_| ());
    match (result, ended) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
    }
}

async fn run_active_voice(
    builder: VoiceSessionBuilder,
    events: &mpsc::UnboundedSender<VoiceEvent>,
    mut observed_agent_events: mpsc::UnboundedReceiver<AgentEvent>,
    mut voice_commands: mpsc::Receiver<VoiceCommand>,
    mut stopped: oneshot::Receiver<()>,
) -> Result<(), VoiceFailure> {
    send_event(events, VoiceEvent::Connecting);
    let default_version = match builder.openai.auth_mode() {
        OpenAiAuthMode::ChatGpt => RealtimeVersion::V3,
        OpenAiAuthMode::ApiKey => RealtimeVersion::V2,
    };
    let voice = builder
        .voice
        .unwrap_or(match builder.version.unwrap_or(default_version) {
            RealtimeVersion::V1 | RealtimeVersion::V3 => CHATGPT_REALTIME_VOICE,
            RealtimeVersion::V2 => PLATFORM_REALTIME_VOICE,
        });
    let mut realtime = builder
        .openai
        .realtime(builder.instructions)
        .voice(voice)
        .initial_items(builder.initial_items)
        .session_mode(builder.session_mode)
        .output_modality(builder.output_modality)
        .client_managed_handoffs(builder.client_managed_handoffs)
        .codex_responses_as_items(builder.codex_responses_as_items)
        .codex_response_handoff_mode(builder.codex_response_handoff_mode)
        .codex_response_handoff_channel_prefixes(builder.codex_response_handoff_channel_prefixes);
    if let Some(enabled) = builder.delegation_ack_filler {
        realtime = realtime.delegation_ack_filler(enabled);
    }
    if let Some(model) = builder.model {
        realtime = realtime.model(model);
    }
    if let Some(version) = builder.version {
        realtime = realtime.version(version);
    }
    if let Some(transport) = builder.transport {
        realtime = realtime.transport(transport);
    }
    if let Some(prefix) = builder.codex_response_item_prefix {
        realtime = realtime.codex_response_item_prefix(prefix);
    }
    if let Some(session_id) = builder.session_id {
        realtime = realtime.session_id(session_id.as_ref());
    }
    if let Some(attestation) = builder.attestation_header {
        realtime = realtime.attestation_header(attestation);
    }
    let connect = realtime.connect();
    let (session, mut realtime_events) = tokio::select! {
        result = connect => result?,
        _ = &mut stopped => return Ok(()),
    };
    let (mut audio, mut microphone) = VoiceAudio::open(builder.audio)?;
    let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel();
    let mut agent_bridge = AgentBridge {
        agent: builder.agent.clone(),
        updates: bridge_tx,
        active: None,
        next_generation: 0,
        external_output: HandoffStream::default(),
        external_error: None,
        observing_external_turn: false,
        control: builder.agent_control,
    };
    let mut external_flush = tokio::time::interval(HANDOFF_STREAM_FLUSH_INTERVAL);
    external_flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    external_flush.tick().await;
    send_event(events, VoiceEvent::Started { voice });

    let mut transport_closed = false;
    let result = loop {
        tokio::select! {
            _ = &mut stopped => break Ok(()),
            frame = microphone.recv() => {
                let Some(frame) = frame else {
                    break Err(VoiceFailure::MicrophoneStopped);
                };
                if let Err(error) = session.send_audio(frame).await {
                    break Err(error.into());
                }
            }
            event = realtime_events.recv() => {
                let Some(event) = event else {
                    transport_closed = true;
                    break Ok(());
                };
                if let Err(error) = handle_realtime_event(
                    event,
                    &session,
                    &mut audio,
                    events,
                    &mut agent_bridge,
                    builder.flush_transcript_tail_on_session_end,
                ).await {
                    break Err(error);
                }
            }
            update = bridge_rx.recv() => {
                let Some(update) = update else {
                    break Ok(());
                };
                match update {
                    AgentBridgeUpdate::Output { generation, text, phase } => {
                        if let Some(active) = &mut agent_bridge.active
                            && active.generation == generation
                        {
                            if !session.client_managed_handoffs() {
                                session
                                    .append_agent_output_with_phase(&active.call_id, text, phase)
                                    .await?;
                            }
                            active.streamed_output = true;
                        }
                    }
                    AgentBridgeUpdate::Completed {
                        generation,
                        call_id,
                        output,
                    } => {
                        let completed = match agent_bridge.active.take() {
                            Some(active) if active.generation == generation => {
                                Some((active.call_id, active.streamed_output))
                            }
                            Some(active) => {
                                agent_bridge.active = Some(active);
                                None
                            }
                            None => None,
                        };
                        let Some((call_id, streamed_output)) = completed else {
                            drop(call_id);
                            continue;
                        };
                        if !session.client_managed_handoffs()
                            && !streamed_output
                            && !output.trim().is_empty()
                        {
                            session.append_agent_output(&call_id, output).await?;
                        }
                        if !session.client_managed_handoffs() {
                            session.complete_agent_run(call_id).await?;
                        }
                    }
                }
            }
            event = observed_agent_events.recv() => {
                let Some(event) = event else {
                    continue;
                };
                handle_observed_agent_event(event, &session, &mut agent_bridge).await?;
            }
            command = voice_commands.recv() => {
                let Some(command) = command else {
                    continue;
                };
                match command {
                    VoiceCommand::AppendText { role, text, result } => {
                        drop(result.send(session.send_text(role, text).await));
                    }
                    VoiceCommand::AppendSpeech { text, result } => {
                        drop(result.send(session.append_speech(text).await));
                    }
                }
            }
            _ = external_flush.tick(), if agent_bridge.has_external_stream_output() => {
                flush_observed_agent_output(&session, &mut agent_bridge).await?;
            }
        }
    };
    if !transport_closed {
        let tail = session.close_with_transcript_tail().await?;
        if builder.flush_transcript_tail_on_session_end {
            route_transcript_tail(&agent_bridge.agent, &tail).await?;
        }
    }
    result
}

enum VoiceCommand {
    AppendText {
        role: RealtimeInputTextRole,
        text: String,
        result: oneshot::Sender<Result<(), RealtimeError>>,
    },
    AppendSpeech {
        text: String,
        result: oneshot::Sender<Result<(), RealtimeError>>,
    },
}

enum AgentBridgeUpdate {
    Output {
        generation: u64,
        text: String,
        phase: Option<MessagePhase>,
    },
    Completed {
        generation: u64,
        call_id: String,
        output: String,
    },
}

struct ActiveAgentRequest {
    generation: u64,
    call_id: String,
    streamed_output: bool,
    external: bool,
}

struct AgentBridge {
    agent: Nanocodex,
    updates: mpsc::UnboundedSender<AgentBridgeUpdate>,
    active: Option<ActiveAgentRequest>,
    next_generation: u64,
    external_output: HandoffStream,
    external_error: Option<String>,
    observing_external_turn: bool,
    control: VoiceAgentControl,
}

impl AgentBridge {
    fn has_external_stream_output(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active.external)
            && !self.external_output.is_empty()
    }
}

async fn handle_realtime_event(
    event: RealtimeEvent,
    session: &RealtimeSession,
    audio: &mut VoiceAudio,
    events: &mpsc::UnboundedSender<VoiceEvent>,
    agent_bridge: &mut AgentBridge,
    flush_transcript_tail_on_session_end: bool,
) -> Result<(), VoiceFailure> {
    match event {
        RealtimeEvent::SessionReady { .. }
        | RealtimeEvent::InputTranscriptDelta(_)
        | RealtimeEvent::OutputTranscriptDelta(_)
        | RealtimeEvent::ResponseStarted
        | RealtimeEvent::ResponseDone => {}
        RealtimeEvent::TranscriptTail(tail) => {
            if flush_transcript_tail_on_session_end {
                route_transcript_tail(&agent_bridge.agent, &tail).await?;
            }
        }
        RealtimeEvent::SpeechStarted => audio.interrupt(),
        RealtimeEvent::InputTranscriptDone(text) => {
            send_transcript(events, VoiceSpeaker::User, text);
        }
        RealtimeEvent::OutputTranscriptDone(text) => {
            send_transcript(events, VoiceSpeaker::Assistant, text);
        }
        RealtimeEvent::Audio(frame) => audio.play(&frame),
        RealtimeEvent::AgentRequest {
            call_id,
            prompt,
            transcript,
        } => {
            let agent = agent_bridge.agent.clone();
            let updates = agent_bridge.updates.clone();
            let streams_agent_output = session.streams_agent_output();
            match agent
                .route_prompt(codex_realtime_delegation_with_transcript(
                    &prompt,
                    &transcript,
                ))
                .await
            {
                Ok(PromptRoute::Started(turn)) => {
                    let generation = agent_bridge.control.install(turn.control());
                    agent_bridge.next_generation = agent_bridge.next_generation.max(generation);
                    agent_bridge.active = Some(ActiveAgentRequest {
                        generation,
                        call_id: call_id.clone(),
                        streamed_output: false,
                        external: false,
                    });
                    let agent_control = agent_bridge.control.clone();
                    drop(tokio::spawn(async move {
                        let mut turn = turn;
                        let mut output = HandoffStream::default();
                        let mut flush = tokio::time::interval(HANDOFF_STREAM_FLUSH_INTERVAL);
                        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        flush.tick().await;
                        loop {
                            tokio::select! {
                                event = turn.next() => {
                                    let Some(event) = event else {
                                        break;
                                    };
                                    match event.data() {
                                        Ok(AgentEventData::Assistant(AssistantEvent::Delta(delta)))
                                            if streams_agent_output =>
                                        {
                                            output.phase = delta.phase;
                                            output.push_text(&delta.text);
                                        }
                                        Ok(AgentEventData::Assistant(AssistantEvent::Message(message)))
                                            if streams_agent_output =>
                                        {
                                            output.phase = message.phase;
                                            if !output.has_output() {
                                                output.push_text(&message.text);
                                            }
                                            if let Some(text) = output.drain_final_chunk()
                                                && updates.send(AgentBridgeUpdate::Output {
                                                    generation,
                                                    text,
                                                    phase: output.phase,
                                                }).is_err()
                                            {
                                                break;
                                            }
                                            output = HandoffStream::default();
                                        }
                                        Ok(AgentEventData::Assistant(AssistantEvent::Message(message)))
                                            if !streams_agent_output
                                                && !message.text.is_empty()
                                                && updates.send(AgentBridgeUpdate::Output {
                                                    generation,
                                                    text: truncate_realtime_output(&message.text),
                                                    phase: message.phase,
                                                }).is_err() =>
                                        {
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                _ = flush.tick(), if streams_agent_output && !output.is_empty() => {
                                    if let Some(text) = output.drain_stream_chunk()
                                        && updates.send(AgentBridgeUpdate::Output {
                                            generation,
                                            text,
                                            phase: output.phase,
                                        }).is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(text) = output.drain_final_chunk() {
                            drop(updates.send(AgentBridgeUpdate::Output {
                                generation,
                                text,
                                phase: output.phase,
                            }));
                        }
                        let output = match turn.result().await {
                            Ok(result) => result.final_message().to_owned(),
                            Err(error) => format!("The coding agent failed: {error}"),
                        };
                        agent_control.clear(generation);
                        drop(updates.send(AgentBridgeUpdate::Completed {
                            generation,
                            call_id,
                            output,
                        }));
                    }));
                }
                Ok(PromptRoute::Steered) => {
                    if agent_bridge.active.is_none() {
                        agent_bridge.next_generation =
                            agent_bridge.next_generation.saturating_add(1);
                        agent_bridge.active = Some(ActiveAgentRequest {
                            generation: agent_bridge.next_generation,
                            call_id: call_id.clone(),
                            streamed_output: false,
                            external: true,
                        });
                        agent_bridge.external_output = HandoffStream::default();
                        agent_bridge.external_error = None;
                    }
                    if session.steer_agent_request(&call_id).await?
                        == RealtimeAgentSteer::ReplacedDelegation
                        && let Some(active) = &mut agent_bridge.active
                    {
                        active.call_id = call_id;
                    }
                }
                Err(error) => {
                    if !session.client_managed_handoffs() {
                        session
                            .append_agent_output(
                                &call_id,
                                format!("The coding agent rejected the request: {error}"),
                            )
                            .await?;
                        session.complete_agent_run(call_id).await?;
                    }
                }
            }
        }
        RealtimeEvent::RemainSilent { call_id } => {
            session.complete_silent_request(call_id).await?;
        }
        RealtimeEvent::Error(error) => {
            return Err(VoiceFailure::Provider(error));
        }
    }
    Ok(())
}

async fn handle_observed_agent_event(
    event: AgentEvent,
    session: &RealtimeSession,
    agent_bridge: &mut AgentBridge,
) -> Result<(), VoiceFailure> {
    if session.client_managed_handoffs() {
        return Ok(());
    }
    match event.data() {
        Ok(AgentEventData::Run(RunEvent::Started(_))) => {
            if agent_bridge.active.is_none() {
                agent_bridge.observing_external_turn = true;
                agent_bridge.external_output = HandoffStream::default();
                agent_bridge.external_error = None;
            }
        }
        Ok(AgentEventData::Assistant(AssistantEvent::Delta(delta)))
            if session.streams_agent_output()
                && agent_bridge
                    .active
                    .as_ref()
                    .is_some_and(|active| active.external) =>
        {
            agent_bridge.external_output.phase = delta.phase;
            agent_bridge.external_output.push_text(&delta.text);
        }
        Ok(AgentEventData::Assistant(AssistantEvent::Message(message))) => {
            if agent_bridge.active.is_none() && agent_bridge.observing_external_turn {
                if !message.text.trim().is_empty() {
                    session
                        .append_standalone_agent_output_with_phase(
                            truncate_realtime_output(&message.text),
                            message.phase,
                        )
                        .await?;
                }
                return Ok(());
            }
            if !agent_bridge
                .active
                .as_ref()
                .is_some_and(|active| active.external)
            {
                return Ok(());
            }
            agent_bridge.external_output.phase = message.phase;
            let output = if session.streams_agent_output() {
                if !agent_bridge.external_output.has_output() {
                    agent_bridge.external_output.push_text(&message.text);
                }
                let output = agent_bridge.external_output.drain_final_chunk();
                agent_bridge.external_output = HandoffStream::default();
                output
            } else if message.text.is_empty() {
                None
            } else {
                Some(truncate_realtime_output(&message.text))
            };
            if let Some(output) = output {
                append_observed_agent_output(session, agent_bridge, output, message.phase).await?;
            }
        }
        Ok(AgentEventData::Run(RunEvent::Error(error))) => {
            agent_bridge.external_error = Some(error.message);
        }
        Ok(AgentEventData::Run(RunEvent::Completed(_))) => {
            if agent_bridge
                .active
                .as_ref()
                .is_some_and(|active| active.external)
            {
                complete_observed_agent_run(session, agent_bridge, false).await?;
            }
            agent_bridge.observing_external_turn = false;
        }
        Ok(AgentEventData::Run(RunEvent::Failed(_))) => {
            if agent_bridge
                .active
                .as_ref()
                .is_some_and(|active| active.external)
            {
                complete_observed_agent_run(session, agent_bridge, true).await?;
            }
            agent_bridge.observing_external_turn = false;
        }
        Ok(_) | Err(_) => {}
    }
    Ok(())
}

async fn route_transcript_tail(
    agent: &Nanocodex,
    tail: &[RealtimeTranscriptEntry],
) -> Result<(), VoiceFailure> {
    let Some(prompt) = codex_realtime_tail_delegation(tail) else {
        return Ok(());
    };
    drop(agent.route_prompt(prompt).await?);
    Ok(())
}

/// Wraps an unconsumed session tail in Codex's tail-flush delegation markers.
#[must_use]
pub fn codex_realtime_tail_delegation(tail: &[RealtimeTranscriptEntry]) -> Option<String> {
    let transcript = tail
        .iter()
        .map(|entry| ProtocolTranscriptEntry::new(&entry.role, &entry.text))
        .collect::<Vec<_>>();
    protocol_realtime_tail_delegation(&transcript)
}

async fn flush_observed_agent_output(
    session: &RealtimeSession,
    agent_bridge: &mut AgentBridge,
) -> Result<(), RealtimeError> {
    if let Some(output) = agent_bridge.external_output.drain_stream_chunk() {
        let phase = agent_bridge.external_output.phase;
        append_observed_agent_output(session, agent_bridge, output, phase).await?;
    }
    Ok(())
}

async fn append_observed_agent_output(
    session: &RealtimeSession,
    agent_bridge: &mut AgentBridge,
    output: String,
    phase: Option<MessagePhase>,
) -> Result<(), RealtimeError> {
    let Some(call_id) = agent_bridge
        .active
        .as_ref()
        .filter(|active| active.external)
        .map(|active| active.call_id.clone())
    else {
        return Ok(());
    };
    session
        .append_agent_output_with_phase(&call_id, output, phase)
        .await?;
    if let Some(active) = &mut agent_bridge.active
        && active.external
        && active.call_id == call_id
    {
        active.streamed_output = true;
    }
    Ok(())
}

async fn complete_observed_agent_run(
    session: &RealtimeSession,
    agent_bridge: &mut AgentBridge,
    failed: bool,
) -> Result<(), RealtimeError> {
    if let Some(output) = agent_bridge.external_output.drain_final_chunk() {
        let phase = agent_bridge.external_output.phase;
        append_observed_agent_output(session, agent_bridge, output, phase).await?;
    }
    agent_bridge.external_output = HandoffStream::default();

    let Some(active) = agent_bridge.active.take().filter(|active| active.external) else {
        return Ok(());
    };
    if failed && !active.streamed_output {
        let error = agent_bridge
            .external_error
            .take()
            .unwrap_or_else(|| "The coding agent failed.".to_owned());
        session.append_agent_output(&active.call_id, error).await?;
    } else {
        agent_bridge.external_error = None;
    }
    session.complete_agent_run(active.call_id).await
}

#[derive(Default)]
struct HandoffStream {
    phase: Option<MessagePhase>,
    sent_bytes: usize,
    buffered_text: String,
    tail_text: String,
    truncated: bool,
}

impl HandoffStream {
    const fn has_output(&self) -> bool {
        self.sent_bytes > 0 || !self.is_empty()
    }

    const fn is_empty(&self) -> bool {
        self.buffered_text.is_empty() && self.tail_text.is_empty()
    }

    const fn stream_head_byte_limit(&self) -> usize {
        realtime_output_byte_limit().saturating_sub(HANDOFF_STREAM_TRUNCATION_MARKER.len()) / 2
    }

    const fn tail_byte_limit(&self) -> usize {
        realtime_output_byte_limit()
            .saturating_sub(self.stream_head_byte_limit())
            .saturating_sub(HANDOFF_STREAM_TRUNCATION_MARKER.len())
    }

    const fn streamable_text_bytes(&self) -> usize {
        self.stream_head_byte_limit()
            .saturating_sub(self.sent_bytes)
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.truncated {
            self.tail_text.push_str(text);
            self.tail_text = take_last_bytes(&self.tail_text, self.tail_byte_limit()).to_owned();
            return;
        }

        self.buffered_text.push_str(text);
        let remaining = realtime_output_byte_limit().saturating_sub(self.sent_bytes);
        if self.buffered_text.len() <= remaining {
            return;
        }

        let head_bytes = take_first_bytes(&self.buffered_text, self.streamable_text_bytes()).len();
        self.tail_text = take_last_bytes(&self.buffered_text, self.tail_byte_limit()).to_owned();
        self.buffered_text.truncate(head_bytes);
        self.truncated = true;
    }

    fn drain_stream_chunk(&mut self) -> Option<String> {
        let requested = self.streamable_text_bytes().min(self.buffered_text.len());
        let split_at = take_first_bytes(&self.buffered_text, requested).len();
        if split_at == 0 {
            return None;
        }
        let text = self.buffered_text.drain(..split_at).collect::<String>();
        self.sent_bytes = self.sent_bytes.saturating_add(text.len());
        Some(text)
    }

    fn drain_final_chunk(&mut self) -> Option<String> {
        if !self.truncated {
            if self.buffered_text.is_empty() {
                return None;
            }
            let text = std::mem::take(&mut self.buffered_text);
            self.sent_bytes = self.sent_bytes.saturating_add(text.len());
            return Some(text);
        }

        let head = std::mem::take(&mut self.buffered_text);
        let tail = std::mem::take(&mut self.tail_text);
        let text = format!("{head}{HANDOFF_STREAM_TRUNCATION_MARKER}{tail}");
        self.sent_bytes = self.sent_bytes.saturating_add(text.len());
        Some(text)
    }
}

const fn realtime_output_byte_limit() -> usize {
    REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

fn truncate_realtime_output(text: &str) -> String {
    let mut output = HandoffStream::default();
    output.push_text(text);
    output.drain_final_chunk().unwrap_or_default()
}

fn take_first_bytes(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn take_last_bytes(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn send_transcript(
    events: &mpsc::UnboundedSender<VoiceEvent>,
    speaker: VoiceSpeaker,
    text: String,
) {
    if !text.trim().is_empty() {
        send_event(events, VoiceEvent::Transcript { speaker, text });
    }
}

/// Wraps delegated speech in Codex's model-visible Realtime input markers.
#[must_use]
pub fn codex_realtime_delegation(input: &str) -> String {
    codex_realtime_delegation_with_transcript(input, &[])
}

/// Wraps delegated speech and its new conversation transcript using Codex's markers.
#[must_use]
pub fn codex_realtime_delegation_with_transcript(
    input: &str,
    transcript: &[RealtimeTranscriptEntry],
) -> String {
    let transcript = transcript
        .iter()
        .map(|entry| ProtocolTranscriptEntry::new(&entry.role, &entry.text))
        .collect::<Vec<_>>();
    protocol_realtime_delegation(input, &transcript)
}

fn send_event(events: &mpsc::UnboundedSender<VoiceEvent>, event: VoiceEvent) {
    drop(events.send(event));
}

#[cfg(test)]
mod tests {
    use super::{
        AudioConfig, HandoffStream, REALTIME_END_INSTRUCTIONS, REALTIME_START_INSTRUCTIONS,
        RealtimeTranscriptEntry, VoiceAgentControl, VoiceSpeaker, codex_realtime_delegation,
        codex_realtime_delegation_with_transcript, codex_realtime_tail_delegation,
        codex_voice_instructions, realtime_output_byte_limit, truncate_realtime_output,
    };
    use std::time::Duration;

    #[test]
    fn desktop_audio_policy_is_explicit_and_stable() {
        let config = AudioConfig::default();
        assert_eq!(config.playback_prebuffer(), Duration::from_millis(120));
        assert_eq!(config.maximum_playback_buffer(), Duration::from_secs(8));
    }

    #[test]
    fn transcript_speakers_have_stable_labels() {
        assert_eq!(VoiceSpeaker::User.to_string(), "user");
        assert_eq!(VoiceSpeaker::Assistant.to_string(), "assistant");
    }

    #[test]
    fn unused_agent_control_is_an_idempotent_interrupt() {
        let control = VoiceAgentControl::default();
        assert!(!control.has_active_turn());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime should build");
        assert!(
            !runtime
                .block_on(control.cancel())
                .expect("cancel should be idle")
        );
    }

    #[test]
    fn codex_backend_prompt_is_rendered_for_the_local_user() {
        let prompt = codex_voice_instructions();
        assert!(prompt.starts_with("## Identity, tone, and role"));
        assert!(prompt.contains("Running backend work remains steerable."));
        assert!(!prompt.contains("{{ user_first_name }}"));
    }

    #[test]
    fn delegated_input_uses_codex_markers_and_xml_escaping() {
        assert_eq!(
            codex_realtime_delegation("fix <x> & ship"),
            "<realtime_delegation>\n  <input>fix &lt;x&gt; &amp; ship</input>\n</realtime_delegation>"
        );
        assert_eq!(
            codex_realtime_delegation_with_transcript(
                "ship it",
                &[
                    RealtimeTranscriptEntry {
                        role: "assistant".to_owned(),
                        text: "Use <main>".to_owned(),
                    },
                    RealtimeTranscriptEntry {
                        role: "user".to_owned(),
                        text: "yes & now".to_owned(),
                    },
                ],
            ),
            "<realtime_delegation>\n  <input>ship it</input>\n  <transcript_delta>assistant: Use &lt;main&gt;\nuser: yes &amp; now</transcript_delta>\n</realtime_delegation>"
        );
    }

    #[test]
    fn lifecycle_and_tail_flush_markers_match_codex() {
        assert!(
            REALTIME_START_INSTRUCTIONS
                .starts_with("<realtime_conversation>\n\nRealtime conversation started.")
        );
        assert!(REALTIME_END_INSTRUCTIONS.contains("Reason: inactive"));
        assert_eq!(
            codex_realtime_tail_delegation(&[RealtimeTranscriptEntry {
                role: "user".to_owned(),
                text: "ship <it>".to_owned(),
            }])
            .unwrap(),
            concat!(
                "<realtime_delegation>\n",
                "  <source>transcript_tail_flush</source>\n",
                "  <input>The user just ended their realtime session. Here is the remaining handoff/transcript tail. You probably do not have to do anything; acknowledge the handoff unless the transcript itself asks for something.</input>\n",
                "  <transcript_delta>user: ship &lt;it&gt;</transcript_delta>\n",
                "</realtime_delegation>"
            )
        );
    }

    #[test]
    fn codex_handoff_stream_is_bounded_and_preserves_head_and_tail() {
        let text = format!("HEAD{}TAIL", "x".repeat(8_000));
        let truncated = truncate_realtime_output(&text);
        assert!(truncated.len() <= realtime_output_byte_limit());
        assert!(truncated.starts_with("HEAD"));
        assert!(truncated.ends_with("TAIL"));
        assert!(truncated.contains("\n…output truncated…\n"));

        let mut stream = HandoffStream::default();
        let short = "é".repeat(1_500);
        stream.push_text(&short);
        let head = stream.drain_stream_chunk().unwrap();
        let tail = stream.drain_final_chunk().unwrap();
        assert_eq!(format!("{head}{tail}"), short);
    }
}
