//! Managed voice host. Shared Rust policy owns captions, handoffs and speech fencing;
//! this actor owns native media, authenticated transports, and session lifetime.
use futures_util::{StreamExt, future::AbortHandle};
use nanocodex_managed::{
    EventCursor, ManagedClient, ManagedError, ManagedEventData, ManagedVoiceSocket,
};
use nanocodex_voice_native::{RealtimeWebrtcSession, RealtimeWebrtcSessionHandle};
use nanocodex_voice_protocol::{BrowserVoiceEffects, ManagedVoiceProtocol, format_delegation};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Continue voice in an existing conversation; otherwise create one.
    #[arg(long)]
    pub agent: Option<String>,
    /// Realtime voice.
    #[arg(long, default_value = "cove", value_parser = clap::builder::PossibleValuesParser::new(nanocodex_voice_protocol::CHATGPT_REALTIME_VOICES.iter().copied()))]
    pub voice: String,
    /// Output provider; microphone and conversation remain on ChatGPT realtime.
    #[arg(long, default_value = "chatgpt", value_parser = ["chatgpt", "elevenlabs"])]
    pub provider: String,
    /// ElevenLabs output voice ID.
    #[arg(long)]
    pub elevenlabs_voice: Option<String>,
    /// Start with microphone muted.
    #[arg(long)]
    pub muted: bool,
    /// Stop after this many seconds (useful for connection checks).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub duration: Option<u64>,
    #[command(flatten)]
    pub observability: nanocodex_observability::ObservabilityOutputArgs,
}

#[path = "voice_command.rs"]
mod command;
pub(crate) use command::{Command, HELP, Provider, Selection};
#[path = "voice_elevenlabs.rs"]
pub(crate) mod elevenlabs;
#[path = "voice_playback.rs"]
mod playback;
use nanocodex_voice_protocol::{VoiceOutputProvider, VoiceSettings};

enum Input {
    Typed,
}
use super::voice_state::{Phase, Status};

#[derive(Default)]
struct MediaControl {
    media: Option<RealtimeWebrtcSessionHandle>,
    ready: bool,
    muted: bool,
    stopped: bool,
}
impl MediaControl {
    fn microphone_muted(&self) -> bool {
        !self.ready || self.muted || self.stopped
    }
    fn apply_microphone(&self) -> Result<(), ManagedError> {
        if let Some(media) = &self.media {
            media
                .set_microphone_muted(self.microphone_muted())
                .map_err(|e| error(e.to_string()))?;
        }
        Ok(())
    }
}

pub(crate) struct Session {
    stop: CancellationToken,
    native_stop: AbortHandle,
    playback: Option<Arc<playback::Playback>>,
    control: Arc<Mutex<MediaControl>>,
    input: mpsc::Sender<Input>,
    muted: watch::Sender<bool>,
    pub status: watch::Receiver<Status>,
    pub transcripts: mpsc::Receiver<super::voice_state::Transcript>,
    presentation: watch::Sender<Status>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Session {
    pub(crate) fn start_with_settings(
        client: ManagedClient,
        agent: String,
        settings: VoiceSettings,
        muted: bool,
    ) -> Result<Self, ManagedError> {
        settings.validate_chatgpt().map_err(error)?;
        let eleven = if settings.output_provider == VoiceOutputProvider::Elevenlabs {
            Some(elevenlabs::Client::from_env()?)
        } else {
            None
        };
        if !RealtimeWebrtcSession::is_supported() {
            return Err(error(
                "Voice runtime missing. Install the matching nightly voice package beside nanocodex2.",
            ));
        }
        let mut protocol = ManagedVoiceProtocol::new(&settings.voice).map_err(error)?;
        protocol
            .dispatch(&json!({"op":"configure","settings":settings}))
            .map_err(error)?;
        protocol.enable_client_managed_handoffs();
        let session = uuid::Uuid::now_v7().to_string();
        protocol.bind_session(&session);
        let stop = CancellationToken::new();
        let (native_stop, native_registration) = AbortHandle::new_pair();
        let (input, commands) = mpsc::channel(16);
        let (muted, microphone) = watch::channel(muted);
        let (status_tx, status) = watch::channel(Status {
            text: "Voice connecting…".into(),
            muted: *microphone.borrow(),
            ..Status::default()
        });
        let (transcript_tx, transcripts) = mpsc::channel(128);
        let control = Arc::new(Mutex::new(MediaControl {
            muted: *microphone.borrow(),
            ..Default::default()
        }));
        let playback_control = control.clone();
        let playback = eleven.map(|client| {
            Arc::new(playback::Playback::new(
                client,
                settings.eleven_labs_voice_id.clone().unwrap(),
                status_tx.clone(),
                Arc::new(move || {
                    let control = playback_control
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if control.stopped || !control.ready {
                        return Err(error("Native voice output is not ready"));
                    }
                    control
                        .media
                        .clone()
                        .ok_or_else(|| error("Native voice output is unavailable"))
                }),
            ))
        });
        let output_label = if settings.output_provider == VoiceOutputProvider::Elevenlabs {
            format!(
                "ElevenLabs {}",
                settings.eleven_labs_voice_id.as_deref().unwrap()
            )
        } else {
            format!("ChatGPT {}", settings.voice)
        };
        let actor_playback = playback.clone();
        let presentation = status_tx.clone();
        let actor_control = control.clone();
        let owner_stop = stop.clone();
        let owner_native_stop = native_stop.clone();
        let task = tokio::spawn(async move {
            let mut actor = Actor {
                output_label,
                playback: actor_playback,
                captions: SpeechCaptions::default(),
                client,
                agent,
                session,
                protocol,
                media: None,
                control: actor_control,
                status: status_tx,
                transcripts: transcript_tx,
                started: Instant::now(),
                prefetch: None,
                event_reader: None,
                speech_sent: None,
            };
            let result = tokio::select! {
                biased;
                () = owner_stop.cancelled() => Ok(()),
                result = actor.run(native_registration, commands, microphone) => result,
            };
            // Fence restoration callbacks before terminating native media.
            {
                let mut control = actor
                    .control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                control.stopped = true;
                let _ = control.apply_microphone();
            }
            // Native capture/playback stops before any network cleanup.
            owner_native_stop.abort();
            if let Some(playback) = &actor.playback {
                playback.cancel();
            }
            if let Some(media) = actor.media.take() {
                media.close();
            }
            if let Some(task) = actor.event_reader.take() {
                task.abort();
            }
            if let Some(task) = actor.prefetch.take() {
                task.abort();
            }
            let final_status = match &result {
                Ok(()) => "Voice stopped".to_owned(),
                Err(error) => format!("Voice failed: {error}"),
            };
            actor.status.send_modify(|status| {
                status.text = final_status.clone();
                status.phase = Phase::Stopping;
                status.microphone = 0;
                status.speaker = 0;
                status.speaking = false;
            });
            // Cleanup uses stable identities and remains bounded even when a call was
            // cancelled during admission. A stale stop cannot close a newer session.
            let cleanup = tokio::time::timeout(Duration::from_secs(10), actor.cleanup()).await;
            let text = match cleanup {
                Ok(Ok(())) => final_status,
                _ => format!("{final_status}; remote cleanup unconfirmed"),
            };
            actor.status.send_modify(|status| {
                status.text = text;
                status.finished = true;
            });
        });
        Ok(Self {
            stop,
            native_stop,
            playback,
            control,
            input,
            muted,
            status,
            presentation,
            transcripts,
            task: Some(task),
        })
    }
    pub(crate) fn stop(&self) {
        {
            let mut control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.stopped = true;
            let _ = control.apply_microphone();
        }
        if let Some(playback) = &self.playback {
            playback.cancel();
        }
        self.native_stop.abort();
        self.stop.cancel();
        self.presentation.send_modify(|status| {
            status.phase = Phase::Stopping;
            status.microphone = 0;
            status.speaker = 0;
            status.speaking = false;
        });
    }
    pub(crate) fn accepting_transcripts(&self) -> bool {
        !self.stop.is_cancelled()
    }
    pub(crate) fn is_muted(&self) -> bool {
        *self.muted.borrow()
    }
    pub(crate) fn mute(&self, muted: bool) {
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.muted.send_replace(muted);
        self.presentation.send_modify(|status| {
            status.muted = muted;
        });
        control.muted = muted;
        let failed = control.apply_microphone().is_err();
        drop(control);
        if failed {
            self.stop();
        }
    }
    pub(crate) fn typed(&self) {
        if let Some(playback) = &self.playback {
            playback.cancel();
        }
        if let Some(media) = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .media
            .as_ref()
        {
            media.set_speaker_suppressed(true);
        }
        if self.input.try_send(Input::Typed).is_err() {
            self.stop();
        }
    }
    pub(crate) async fn finish(mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
// Caption IDs are monotonic for a session. An interrupted caption stays suppressed
// even if its delayed final arrives after playback is enabled again.
struct SpeechCaptions {
    generation: u64,
    caption: Option<u64>,
    suppressed: Option<u64>,
    completed: Option<u64>,
    speech_error: Option<&'static str>,
    enabled: bool,
}
impl Default for SpeechCaptions {
    fn default() -> Self {
        Self {
            generation: 0,
            caption: None,
            suppressed: None,
            completed: None,
            speech_error: None,
            enabled: true,
        }
    }
}
impl SpeechCaptions {
    fn update(&mut self, generation: Option<u64>, enabled: Option<bool>) -> bool {
        if generation.is_some_and(|g| g < self.generation) {
            return false;
        }
        let changed = generation.is_some_and(|g| g > self.generation);
        if let Some(g) = generation {
            self.generation = g;
        }
        let interrupted = changed || enabled == Some(false);
        if interrupted {
            self.suppressed = self.suppressed.max(self.caption);
        }
        if let Some(enabled) = enabled {
            self.enabled = enabled;
        }
        interrupted
    }
    /// Suppress audio only; the complete caption remains visible.
    fn suppress_remainder(&mut self, id: u64) {
        self.completed = self.completed.max(Some(id));
    }

    fn consume(&mut self, speaker: &str, id: u64, text: &str, is_partial: bool) -> Option<String> {
        if speaker != "assistant" || self.caption.is_some_and(|previous| id < previous) {
            return None;
        }
        self.caption = Some(id);
        if !self.enabled {
            self.suppressed = self.suppressed.max(Some(id));
            return None;
        }
        if self.suppressed.is_some_and(|previous| id <= previous)
            || self.completed.is_some_and(|previous| id <= previous)
        {
            return None;
        }
        // Bound retained state and requests without truncating the visible text.
        if text.len() > 16000 {
            self.suppress_remainder(id);
            self.speech_error = Some(
                "ElevenLabs caption exceeds 16000 bytes; remaining reply is available as text",
            );
            return None;
        }
        // Keep each reply in one synthesis request so sentence boundaries do not
        // restart the provider's prosody and native audio stream.
        if is_partial {
            return None;
        }
        self.completed = Some(id);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}
struct Actor {
    output_label: String,
    captions: SpeechCaptions,
    playback: Option<Arc<playback::Playback>>,
    client: ManagedClient,
    agent: String,
    session: String,
    protocol: ManagedVoiceProtocol,
    media: Option<RealtimeWebrtcSessionHandle>,
    control: Arc<Mutex<MediaControl>>,
    status: watch::Sender<Status>,
    transcripts: mpsc::Sender<super::voice_state::Transcript>,
    started: Instant,
    prefetch: Option<tokio::task::JoinHandle<()>>,
    event_reader: Option<tokio::task::JoinHandle<()>>,
    speech_sent: Option<Instant>,
}
impl Actor {
    fn status(&self, text: impl Into<String>) {
        let text = output_status(text.into(), &self.output_label);
        self.status.send_modify(|status| {
            status.text = text;
        });
    }
    fn timing(&self, stage: &str) {
        tracing::info!(target: "nanocodex2::voice", stage, elapsed_ms = self.started.elapsed().as_millis() as u64, "voice timing");
    }
    async fn run(
        &mut self,
        registration: futures_util::future::AbortRegistration,
        mut commands: mpsc::Receiver<Input>,
        mut muted: watch::Receiver<bool>,
    ) -> Result<(), ManagedError> {
        let client = self.client.clone();
        let agent = self.agent.clone();
        let session = self.session.clone();
        let start_id = uuid::Uuid::new_v4().to_string();
        let start = async {
            let (state, admitted) = tokio::try_join!(
                client.state(&agent),
                client.voice_operation(
                    &agent,
                    &session,
                    "start",
                    json!({"operation_id": start_id})
                )
            )?;
            Ok::<_, ManagedError>((state, admitted))
        };
        let prepare_media = async {
            let started =
                tokio::task::spawn_blocking(move || RealtimeWebrtcSession::start(registration))
                    .await
                    .map_err(|_| error("Voice startup task failed"))?
                    .map_err(|e| error(e.to_string()))?;
            {
                let mut control = self
                    .control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                started
                    .handle
                    .set_speaker_suppressed(self.playback.is_some());
                control.media = Some(started.handle.clone());
                control.apply_microphone()?;
            }
            self.media = Some(started.handle.clone());
            self.timing("native.offer");
            Ok::<_, ManagedError>(started)
        };
        // Match the desktop app: context belongs in call creation, before
        // speech starts. Appending startup fragments to a running conversation
        // can provoke unsolicited responses while the user begins speaking.
        let ((state, admitted), started) =
            tokio::try_join!(connection_step("agent admission", start), prepare_media)?;
        self.timing("agent.ready");
        let settings = initial_call_settings(&mut self.protocol, &admitted["context"])?;
        let call = connection_step(
            "realtime call creation",
            self.client
                .voice_call(&self.agent, &self.session, &started.offer_sdp, settings),
        )
        .await?;
        self.timing("call.answer");
        let handle = started.handle;
        let answer = async {
            tokio::task::spawn_blocking(move || handle.apply_answer_sdp(call.sdp))
                .await
                .map_err(|_| error("Voice answer task failed"))?
                .map_err(|e| error(e.to_string()))?;
            self.timing("native.connected");
            Ok::<_, ManagedError>(())
        };
        let sideband = async {
            let socket = self
                .client
                .voice_sideband(&self.agent, &self.session, &call.call_id)
                .await?;
            self.timing("sideband.ready");
            Ok::<_, ManagedError>(socket)
        };
        let ((), mut socket) = tokio::try_join!(answer, sideband)?;
        let call = call.call_id;
        self.timing("media.ready");
        let events = self
            .client
            .events(&self.agent, EventCursor::parse(state.latest_event_cursor)?)?;
        // Keep connection establishment alive while the realtime channel is busy.
        // Polling SSE next() directly in select would cancel its HTTP handshake
        // every time an audio/control event wins, starving agent output.
        let (event_sender, mut agent_events) = mpsc::channel(128);
        self.event_reader = Some(spawn_event_reader(events, event_sender));
        let effects = self.protocol.sideband_opened();
        self.apply(&mut socket, effects).await?;
        let mut connected = Instant::now();
        let mut active_turn: Option<String> = None;
        let mut flush = tokio::time::interval(Duration::from_millis(100));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut first_audio = false;
        let mut first_input = false;
        let mut last_speech: Option<Instant> = None;
        self.status("Voice connecting · waiting for realtime session…");
        let ready_deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(ready_deadline);
        loop {
            tokio::select! {
                biased;
                () = &mut ready_deadline, if self.status.borrow().phase == Phase::Connecting => {
                    return Err(error("Timed out waiting for realtime session readiness; stop and retry voice"));
                }
                changed = muted.changed() => {
                    changed.map_err(|_| error("Voice controls closed"))?;
                    let connecting = self.status.borrow().phase == Phase::Connecting;
                    self.status(if connecting {
                        "Voice connecting · waiting for realtime session…"
                    } else if *muted.borrow() {
                        "Voice active · microphone muted"
                    } else {
                        "Voice active · listening"
                    });
                }
                command = commands.recv() => match command {
                    Some(Input::Typed) => { active_turn = None; let effects = self.protocol.note_typed_input(); self.apply(&mut socket, effects).await?; }
                    None => return Ok(()),
                },
                received = socket.next() => {
                    let event = match received {
                        Ok(event) => event,
                        Err(_) => {
                            let effects = self.protocol.sideband_closed(connected.elapsed().as_millis() as u64);
                            self.status("Voice reconnecting…");
                            tokio::time::sleep(Duration::from_millis(effects.reconnect_after_ms.unwrap_or(200))).await;
                            socket = self.client.voice_sideband(&self.agent, &self.session, &call).await?;
                            connected = Instant::now();
                            let effects = self.protocol.sideband_opened(); self.apply(&mut socket, effects).await?;
                            continue;
                        }
                    };
                    if let Some(kind @ ("session.started" | "delegation.created" | "turn.done" | "error")) = event["type"].as_str() { self.timing(&format!("realtime.{kind}")); }
                    let update = self.protocol.realtime_message(&event.to_string());
                    self.apply(&mut socket, update.effects).await?;
                    if let Some(prefetch) = update.prefetch {
                        if let Some(task) = self.prefetch.take() { task.abort(); }
                        let client = self.client.clone(); let agent = self.agent.clone(); let session = self.session.clone();
                        self.prefetch = Some(tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(u64::from(prefetch.debounce_ms))).await;
                            let _ = tokio::time::timeout(Duration::from_secs(10), client.voice_operation(&agent, &session, "prefetch", json!({"query":prefetch.query}))).await;
                        }));
                    }
                    if let Some(delegation) = update.delegation {
                        if let Some(task) = self.prefetch.take() { task.abort(); }
                        self.timing("delegation.received");
                        let route = self.client.voice_operation(&self.agent, &self.session, "delegate", json!({"operation_id":uuid::Uuid::new_v4().to_string(), "input":format_delegation(&delegation)})).await?;
                        active_turn = route["turn_id"].as_str().map(str::to_owned);
                        self.timing("delegation.admitted");
                    }
                }
                event = agent_events.recv() => {
                    let event = event.ok_or_else(|| error("Voice agent event stream closed"))??;
                    if let ManagedEventData::StreamFailed { .. } = &event.data { return Err(error("Voice agent event stream failed")); }
                    let envelope = serde_json::to_value(&event).map_err(|e| error(e.to_string()))?;
                    let context = self.protocol.managed_event(&envelope);
                    self.apply(&mut socket, context).await?;
                    if event.turn_id.as_ref() == active_turn.as_ref() && active_turn.is_some() {
                        if let ManagedEventData::Event {event, agent_id: None} = &event.data {
                            let effects = self.protocol.agent_event(event.get()); self.apply(&mut socket, effects).await?;
                        }
                        if matches!(&event.data, ManagedEventData::TurnFailed {..}) {
                            let effects = self.protocol.agent_event(&envelope.to_string()); self.apply(&mut socket, effects).await?;
                        }
                        if matches!(&event.data, ManagedEventData::TurnCancelled {..}) {
                            let effects = self.protocol.note_typed_input(); self.apply(&mut socket, effects).await?;
                        }
                        if matches!(&event.data, ManagedEventData::TurnCompleted {..} | ManagedEventData::TurnFailed {..} | ManagedEventData::TurnCancelled {..}) { active_turn = None; }
                    }
                }
                _ = flush.tick() => {
                    let media = self.media.as_ref().unwrap();
                    if let Some(error_text) = media.take_error() { return Err(error(error_text)); }
                    let microphone = media.take_microphone_peak();
                    let speaker = media.take_speaker_peak();
                    if speaker >= 512 { last_speech = Some(Instant::now()); }
                    let speaking = last_speech.is_some_and(|last| last.elapsed() < Duration::from_millis(500));
                    self.status.send_if_modified(|status| {
                        let changed = status.microphone != microphone || status.speaker != speaker || status.speaking != speaking;
                        status.microphone = microphone; status.speaker = speaker; status.speaking = speaking; changed
                    });
                    if microphone > 200 && !first_input { first_input = true; self.timing("input.first_energy"); }
                    if speaker > 200 {
                        if !first_audio { first_audio = true; self.timing("audio.first_energy"); }
                        if let Some(sent) = self.speech_sent.take() {
                            self.timing("speech.first_energy");
                            tracing::info!(target: "nanocodex2::voice", stage = "speech.latency", elapsed_ms = sent.elapsed().as_millis() as u64, "voice timing");
                        }
                    }
                    let effects = self.protocol.flush(false); self.apply(&mut socket, effects).await?;
                }
            }
        }
    }
    async fn apply(
        &mut self,
        socket: &mut ManagedVoiceSocket,
        effects: BrowserVoiceEffects,
    ) -> Result<(), ManagedError> {
        let stale_speech = effects
            .input_generation
            .is_some_and(|g| g < self.captions.generation);
        if !stale_speech
            && let Some(enabled) = effects.playback_enabled
            && let Some(media) = &self.media
        {
            media.set_speaker_suppressed(self.playback.is_some() || !enabled);
        }
        if self
            .captions
            .update(effects.input_generation, effects.playback_enabled)
            && let Some(playback) = &self.playback
        {
            playback.cancel();
        }
        if let Some(status) = effects.status {
            self.status(status);
        }
        for transcript in effects.transcripts {
            if stale_speech {
                continue;
            }
            if let Some(segment) = self.captions.consume(
                &transcript.speaker,
                transcript.id,
                &transcript.text,
                transcript.is_partial,
            ) && let Some(playback) = &self.playback
                && let Err(error) = playback.enqueue(segment)
            {
                self.captions.suppress_remainder(transcript.id);
                self.status(error.to_string());
            }
            if let Some(message) = self.captions.speech_error.take()
                && self.playback.is_some()
            {
                self.status(message.to_owned());
            }
            if transcript.speaker == "assistant"
                && (!self.captions.enabled
                    || self
                        .captions
                        .suppressed
                        .is_some_and(|id| transcript.id <= id))
            {
                continue;
            }
            self.transcripts
                .send(super::voice_state::Transcript {
                    session: self.session.clone(),
                    speaker: transcript.speaker,
                    id: transcript.id,
                    text: transcript.text,
                    is_partial: transcript.is_partial,
                })
                .await
                .map_err(|_| error("Voice transcript consumer closed"))?;
        }
        if effects.ready == Some(true) {
            {
                let mut control = self
                    .control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                control.ready = true;
                control.apply_microphone()?;
            }
            self.status
                .send_modify(|status| status.phase = Phase::Active);
            self.timing("protocol.ready");
        }
        for frame in &effects.frames {
            socket.send(frame).await?;
            if frame.contains("speakable") {
                if let Some(media) = &self.media {
                    media.take_speaker_peak();
                }
                self.speech_sent = Some(Instant::now());
                self.timing("speech.frame.sent");
            }
            if effects.acknowledge_frames {
                self.protocol.frames_sent(1);
            }
        }
        for answer in effects.undelivered_answers {
            self.status(format!("Voice reply available as text: {answer}"));
        }
        if let Some(reason) = effects.terminate {
            return Err(error(reason));
        }
        Ok(())
    }
    async fn cleanup(&mut self) -> Result<(), ManagedError> {
        let tail = self
            .protocol
            .dispatch(&json!({"op":"tail"}))
            .map_err(error)?;
        let mut tail_error = None;
        if let Some(input) = tail.as_str().filter(|text| !text.trim().is_empty()) {
            tail_error = self
                .client
                .voice_operation(
                    &self.agent,
                    &self.session,
                    "delegate",
                    json!({"operation_id":uuid::Uuid::new_v4().to_string(),"input":input}),
                )
                .await
                .err();
        }
        self.client
            .voice_operation(
                &self.agent,
                &self.session,
                "stop",
                json!({"operation_id":uuid::Uuid::new_v4().to_string()}),
            )
            .await?;
        if let Some(error) = tail_error {
            return Err(error);
        }
        Ok(())
    }
}
fn output_status(text: String, output_label: &str) -> String {
    if text.starts_with("Voice active") {
        let detail = text.split_once('·').map(|(_, detail)| detail.trim());
        match detail {
            Some(detail) => format!("Voice active · {output_label} · {detail}"),
            None => format!("Voice active · {output_label}"),
        }
    } else {
        text
    }
}

fn initial_call_settings(
    protocol: &mut ManagedVoiceProtocol,
    context: &serde_json::Value,
) -> Result<serde_json::Value, ManagedError> {
    let mut instructions = nanocodex_voice_protocol::chatgpt_realtime_instructions("there");
    if let Some(context) = nanocodex_voice_protocol::managed_startup_context(context) {
        instructions.push_str("\n\n");
        instructions.push_str(&context);
    }
    protocol
        .dispatch(&json!({"op":"session","instructions":instructions}))
        .map_err(error)
}

fn spawn_event_reader(
    mut events: nanocodex_managed::ManagedEventStream,
    sender: mpsc::Sender<Result<nanocodex_managed::ManagedEvent, ManagedError>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let event = events.next().await;
            let failed = event.is_err();
            if sender.send(event).await.is_err() || failed {
                break;
            }
        }
    })
}
impl Drop for Actor {
    fn drop(&mut self) {
        if let Some(playback) = &self.playback {
            playback.cancel();
        }
        if let Some(task) = &self.event_reader {
            task.abort();
        }
        if let Some(task) = &self.prefetch {
            task.abort();
        }
        if let Some(media) = &self.media {
            media.close();
        }
    }
}
// Bound admission including retries and call creation including authentication.
// Native offer/answer and sideband transport have their own deadlines.
async fn connection_step<T>(
    stage: &str,
    future: impl std::future::Future<Output = Result<T, ManagedError>>,
) -> Result<T, ManagedError> {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .map_err(|_| error(format!("Timed out during {stage}; stop and retry voice")))?
}

fn error(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

pub(crate) async fn run(client: &ManagedClient, args: Args) -> Result<(), ManagedError> {
    let _logs = args
        .observability
        .install(
            "nanocodex2-voice",
            env!("CARGO_PKG_VERSION"),
            "warn,nanocodex2::voice=info",
            "warn,nanocodex2::voice=info",
            nanocodex_observability::LogOutput::Stderr,
        )
        .map_err(|e| error(e.to_string()))?;
    let settings = VoiceSettings {
        voice: args.voice,
        output_provider: if args.provider == "elevenlabs" {
            VoiceOutputProvider::Elevenlabs
        } else {
            VoiceOutputProvider::Openai
        },
        eleven_labs_voice_id: args.elevenlabs_voice,
        ..Default::default()
    };
    settings.validate_chatgpt().map_err(error)?;
    if settings.output_provider == VoiceOutputProvider::Elevenlabs {
        elevenlabs::Client::from_env()?;
    }
    let (agent, mut workspace_events, id, _) =
        super::open_workspace_agent_from(client, args.agent, None, None).await?;
    eprintln!("Managed agent: {id}");
    let mut session = Session::start_with_settings(client.clone(), id, settings, args.muted)?;
    let mut status = session.status.clone();
    let deadline = tokio::time::sleep(Duration::from_secs(args.duration.unwrap_or(86400)));
    tokio::pin!(deadline);
    let mut failure = None;
    let mut previous_text = String::new();
    loop {
        tokio::select! {
            Some(transcript) = session.transcripts.recv() => {
                println!("{}", json!({"type":"voice.transcript","transcript":transcript}));
            }
            _ = workspace_events.next() => {},
            _ = tokio::signal::ctrl_c() => break,
            _ = &mut deadline => break,
            changed = status.changed() => {
                if changed.is_err() { break; }
                let value = status.borrow_and_update().clone();
                if value.text != previous_text || value.finished {
                    println!("{}", json!({"type":"voice.status","text":value.text,"finished":value.finished}));
                    previous_text.clone_from(&value.text);
                }
                if value.finished { if value.text.starts_with("Voice failed") { failure = Some(error(value.text)); } break; }
            }
        }
    }
    session.finish().await;
    let final_status = status.borrow().clone();
    println!(
        "{}",
        json!({"type":"voice.status","text":final_status.text,"finished":final_status.finished})
    );
    if failure.is_none()
        && (final_status.text.starts_with("Voice failed")
            || final_status.text.contains("cleanup unconfirmed"))
    {
        failure = Some(error(final_status.text));
    }
    agent.disconnect().await.map_err(super::agent_error)?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_connection_reports_stage_and_allows_retry() {
        let failure = connection_step::<()>("agent admission", std::future::pending())
            .await
            .unwrap_err()
            .to_string();
        assert!(failure.contains("Timed out during agent admission"));
        assert!(failure.contains("retry voice"));
        assert_eq!(
            connection_step("agent admission", async { Ok(42) })
                .await
                .unwrap(),
            42
        );
    }

    #[tokio::test]
    async fn connection_preserves_underlying_errors() {
        let failure = connection_step::<()>("realtime sideband", async { Err(error("denied")) })
            .await
            .unwrap_err()
            .to_string();
        assert!(failure.contains("denied"));
        assert!(!failure.contains("Timed out"));
    }

    #[test]
    fn active_status_names_selected_output_provider_and_voice() {
        assert_eq!(
            output_status("Voice active (cove)".into(), "ElevenLabs synthetic_voice"),
            "Voice active · ElevenLabs synthetic_voice"
        );
        assert_eq!(
            output_status("Voice active · microphone muted".into(), "ChatGPT maple"),
            "Voice active · ChatGPT maple · microphone muted"
        );
        assert_eq!(
            output_status("Voice reconnecting…".into(), "ElevenLabs synthetic_voice"),
            "Voice reconnecting…"
        );
    }
    #[test]
    fn captions_dedupe_finals_and_permanently_suppress_interrupted_ids() {
        let mut captions = SpeechCaptions::default();
        let mut id = 1;
        let mut partial = true;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_none()
        );
        assert!(captions.update(Some(1), Some(false)));
        assert!(!captions.update(Some(0), Some(true)));
        assert!(!captions.enabled);
        captions.update(Some(1), Some(true));
        partial = false;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_none()
        );
        id = 2;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_some()
        );
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_none()
        );
        id = 3;
        partial = true;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_none()
        );
        assert!(captions.update(Some(2), None));
        partial = false;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_none()
        );
        id = 4;
        assert!(
            captions
                .consume("assistant", id, "hello", partial)
                .is_some()
        );
    }
    #[test]
    fn captions_emit_one_complete_reply_after_long_partial_burst() {
        let mut captions = SpeechCaptions::default();
        let mut text = String::new();
        for sentence in 0..100 {
            text.push_str(&format!("Sentence {sentence}. More detail! Next? "));
            assert_eq!(captions.consume("assistant", 1, &text, true), None);
        }
        assert_eq!(
            captions.consume("assistant", 1, &text, false).as_deref(),
            Some(text.trim())
        );
        assert_eq!(captions.consume("assistant", 1, &text, false), None);
        assert_eq!(captions.consume("assistant", 1, &text, true), None);
        assert_eq!(
            captions
                .consume("assistant", 2, "No punctuation", false)
                .as_deref(),
            Some("No punctuation")
        );
    }

    #[test]
    fn captions_accept_corrected_partials_and_preserve_utf8_final() {
        let mut captions = SpeechCaptions::default();
        assert_eq!(captions.consume("assistant", 1, "Old. More", true), None);
        assert_eq!(captions.consume("assistant", 1, "Café! 世界", true), None);
        assert_eq!(
            captions
                .consume("assistant", 1, "Café! 世界", false)
                .as_deref(),
            Some("Café! 世界")
        );
        assert_eq!(captions.consume("assistant", 1, "Old. More", false), None);
    }

    #[test]
    fn captions_interrupt_and_queue_failure_fence_finals() {
        let mut captions = SpeechCaptions::default();
        assert_eq!(captions.consume("assistant", 1, "First. tail", true), None);
        assert!(captions.update(Some(1), Some(false)));
        captions.update(Some(1), Some(true));
        assert_eq!(captions.consume("assistant", 1, "First. tail", false), None);
        assert_eq!(
            captions
                .consume("assistant", 2, "Next. tail", false)
                .as_deref(),
            Some("Next. tail")
        );
        captions.suppress_remainder(2);
        assert_eq!(captions.consume("assistant", 2, "Next. tail", false), None);
        assert!(captions.enabled);
        assert_eq!(
            captions.suppressed,
            Some(1),
            "queue failure must not hide UI captions"
        );
        assert_eq!(
            captions.consume("assistant", 3, "Fresh", false).as_deref(),
            Some("Fresh")
        );
        assert_eq!(captions.consume("user", 4, "User.", false), None);
    }

    #[test]
    fn captions_bound_requests_and_report_oversized_speech() {
        let mut captions = SpeechCaptions::default();
        assert_eq!(
            captions.consume("assistant", 1, &"é".repeat(8001), true),
            None
        );
        assert!(captions.speech_error.take().unwrap().contains("16000"));
        assert_eq!(
            captions.consume("assistant", 1, "shorter final", false),
            None
        );
        let limit = "é".repeat(8000);
        assert_eq!(
            captions.consume("assistant", 2, &limit, false).as_deref(),
            Some(limit.as_str())
        );
        assert_eq!(
            captions.consume("assistant", 3, &"x".repeat(16001), false),
            None
        );
        assert!(captions.speech_error.take().unwrap().contains("16000"));
    }

    #[tokio::test]
    async fn busy_realtime_events_do_not_cancel_agent_stream_connection() {
        use axum::{Router, http::header, routing::get};
        use nanocodex_managed::ManagedApiKey;
        let app = Router::new().route("/v1/agents/agent-test/events", get(|| async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            ([(header::CONTENT_TYPE, "text/event-stream")], "id: 1\nevent: event\ndata: {\"cursor\":\"1\",\"type\":\"event\",\"turn_id\":\"turn-1\",\"event\":{\"type\":\"assistant.message\",\"payload\":{\"text\":\"answer\"}}}\n\n")
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
            .unwrap();
        let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
        let events = client
            .events("agent-test", EventCursor::parse("0").unwrap())
            .unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let reader = spawn_event_reader(events, sender);
        let mut audio = tokio::time::interval(Duration::from_millis(2));
        let mut ticks = 0;
        let event = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = audio.tick() => ticks += 1,
                    event = receiver.recv() => break event.unwrap().unwrap(),
                }
            }
        })
        .await
        .unwrap();
        assert!(ticks > 10);
        assert_eq!(event.cursor, "1");
        reader.abort();
        server.abort();
    }
    #[test]
    fn startup_context_is_in_call_instructions_without_live_context_frames() {
        let mut protocol = ManagedVoiceProtocol::new("cove").unwrap();
        protocol
            .dispatch(&json!({"op":"configure","settings":{
                "voice":"maple","instructions":"Speak briefly."
            }}))
            .unwrap();
        let settings = initial_call_settings(
            &mut protocol,
            &json!({
                "workspace":"/omarchy-desktop",
                "prepared_personalization":"Prefers Rust.",
                "history":[
                    {"role":"user","content":[{"text":"Help me test voice."}]},
                    {"role":"developer","content":[{"text":"Private host state."}]}
                ]
            }),
        )
        .unwrap();
        let instructions = settings["instructions"].as_str().unwrap();
        assert!(instructions.contains("/omarchy-desktop"));
        assert!(instructions.contains("Prefers Rust."));
        assert!(instructions.contains("Help me test voice."));
        assert!(!instructions.contains("Private host state."));
        assert_eq!(instructions.matches("Speak briefly.").count(), 1);
        assert_eq!(settings["audio"]["output"]["voice"], "maple");
        assert!(protocol.sideband_opened().frames.is_empty());
    }

    #[test]
    fn microphone_waits_for_backend_and_preserves_startup_mute() {
        let mut control = MediaControl::default();
        assert!(control.microphone_muted());
        control.muted = true;
        control.muted = false;
        assert!(control.microphone_muted(), "unmute cannot bypass startup");
        control.muted = true;
        control.ready = true;
        assert!(
            control.microphone_muted(),
            "startup cannot override user mute"
        );
        control.muted = false;
        assert!(!control.microphone_muted());
    }

    #[test]
    fn native_playback_preserves_user_mute_and_stop_fence() {
        let mut control = MediaControl {
            ready: true,
            ..Default::default()
        };
        assert!(
            !control.microphone_muted(),
            "native echo cancellation allows barge-in"
        );
        control.muted = true;
        assert!(control.microphone_muted());
        control.muted = false;
        assert!(!control.microphone_muted());
        control.stopped = true;
        assert!(
            control.microphone_muted(),
            "late playback cannot reopen capture"
        );
    }

    #[test]
    fn voice_controls_have_explicit_start_stop_and_privacy_transitions() {
        assert_eq!(Command::parse("").unwrap(), Command::Toggle);
        for (text, command) in [
            ("start", Command::Start(None)),
            ("on", Command::Start(None)),
            ("off", Command::Stop),
            ("cove", Command::Start(Some("cove"))),
            ("voices", Command::List),
            ("stop", Command::Stop),
            ("mute", Command::ToggleMute),
            ("unmute", Command::Unmute),
            ("status", Command::Status),
        ] {
            assert_eq!(Command::parse(text).unwrap(), command);
        }
        assert!(Command::parse("mute now").is_err());
    }
}
