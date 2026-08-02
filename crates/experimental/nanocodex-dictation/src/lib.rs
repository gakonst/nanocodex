#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use nanocodex_oai_api::{
    OpenAi,
    auth::OpenAiAuthMode,
    realtime::{
        RealtimeAudio, RealtimeError, RealtimeEvent, RealtimeSession, RealtimeTransport,
        RealtimeVersion,
    },
};
use tokio::sync::{mpsc, oneshot};

#[doc(hidden)]
pub mod audio;
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[doc(hidden)]
pub mod capture;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[doc(hidden)]
#[path = "capture_unsupported.rs"]
pub mod capture;
mod reducer;

use capture::{CaptureConfig, CaptureGate, CaptureStream, Pcm16Chunk};
use reducer::TranscriptReducer;

const SAMPLE_RATE_HZ: u32 = 24_000;
const SAMPLES_PER_CHUNK: usize = 480;
const CHUNK_DURATION: Duration = Duration::from_millis(20);
const PRECONNECT_SECONDS: usize = 5;
const PRECONNECT_CHUNKS: usize = PRECONNECT_SECONDS * SAMPLE_RATE_HZ as usize / SAMPLES_PER_CHUNK;
const EARLY_FINISH_PREROLL_CHUNKS: usize = 15;
const FINISH_TIMEOUT: Duration = Duration::from_secs(8);
const NO_SPEECH_FINISH_TIMEOUT: Duration = Duration::from_secs(2);
const FINALIZATION_GRACE: Duration = Duration::from_millis(500);
const FINAL_SILENCE_CHUNKS: usize = SAMPLE_RATE_HZ as usize / SAMPLES_PER_CHUNK;
const AUDIO_LEVEL_INTERVAL: Duration = Duration::from_millis(60);

/// Stable and revisable composer text for one dictation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictationTranscript {
    /// Finalized and segment-stable text.
    pub stable: String,
    /// Current revisable partial hypothesis.
    pub unstable: String,
}

/// One normalized update from an independent dictation lifecycle.
#[derive(Debug)]
pub enum DictationEvent {
    /// Microphone startup is in progress.
    Connecting,
    /// The microphone is active and audio is retained while transport startup completes.
    Started,
    /// The authenticated remote transport is ready to consume retained and live audio.
    TransportReady,
    /// Latest microphone peak, coalesced when the consumer falls behind.
    AudioLevel(u16),
    /// The current stable and revisable transcript replacement.
    Transcript(DictationTranscript),
    /// Text selected for insertion after clean close or stable recovery.
    Finished(String),
    /// The attempt completed normally without detected speech.
    NoSpeech,
    /// The attempt failed without committable text.
    Failed(DictationError),
    /// The attempt stopped after cancellation or terminal settlement.
    Stopped,
}

/// Receiver for one transcript-coalescing dictation event stream.
pub struct DictationEvents {
    receiver: mpsc::UnboundedReceiver<QueuedEvent>,
    transcript_pending: Arc<AtomicBool>,
    latest_transcript: Arc<Mutex<DictationTranscript>>,
    audio_level_pending: Arc<AtomicBool>,
    latest_audio_level: Arc<AtomicU16>,
}

impl DictationEvents {
    /// Waits for the next lifecycle or transcript update.
    pub async fn recv(&mut self) -> Option<DictationEvent> {
        self.receiver.recv().await.map(|event| self.receive(event))
    }

    /// Attempts to receive an already-buffered update.
    pub fn try_recv(&mut self) -> Option<DictationEvent> {
        let event = self.receiver.try_recv().ok()?;
        Some(self.receive(event))
    }

    fn receive(&self, event: QueuedEvent) -> DictationEvent {
        match event {
            QueuedEvent::Lifecycle(event) => event,
            QueuedEvent::Transcript => {
                let latest = self
                    .latest_transcript
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let transcript = latest.clone();
                self.transcript_pending.store(false, Ordering::Release);
                DictationEvent::Transcript(transcript)
            }
            QueuedEvent::AudioLevel => {
                let level = self.latest_audio_level.load(Ordering::Acquire);
                self.audio_level_pending.store(false, Ordering::Release);
                DictationEvent::AudioLevel(level)
            }
        }
    }
}

struct EventSender {
    sender: mpsc::UnboundedSender<QueuedEvent>,
    transcript_pending: Arc<AtomicBool>,
    latest_transcript: Arc<Mutex<DictationTranscript>>,
    audio_level_pending: Arc<AtomicBool>,
    latest_audio_level: Arc<AtomicU16>,
}

enum QueuedEvent {
    Lifecycle(DictationEvent),
    Transcript,
    AudioLevel,
}

fn event_channel() -> (EventSender, DictationEvents) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let transcript_pending = Arc::new(AtomicBool::new(false));
    let latest_transcript = Arc::new(Mutex::new(DictationTranscript {
        stable: String::new(),
        unstable: String::new(),
    }));
    let audio_level_pending = Arc::new(AtomicBool::new(false));
    let latest_audio_level = Arc::new(AtomicU16::new(0));
    (
        EventSender {
            sender,
            transcript_pending: Arc::clone(&transcript_pending),
            latest_transcript: Arc::clone(&latest_transcript),
            audio_level_pending: Arc::clone(&audio_level_pending),
            latest_audio_level: Arc::clone(&latest_audio_level),
        },
        DictationEvents {
            receiver,
            transcript_pending,
            latest_transcript,
            audio_level_pending,
            latest_audio_level,
        },
    )
}

/// Concrete builder for ChatGPT-authenticated streaming dictation.
pub struct ChatGptDictationBuilder {
    openai: OpenAi,
}

impl ChatGptDictationBuilder {
    /// Creates a builder from the application's existing OpenAI recipe.
    #[must_use]
    pub const fn new(openai: OpenAi) -> Self {
        Self { openai }
    }

    /// Spawns one owned runtime, capture stream, and authenticated session.
    ///
    /// # Errors
    ///
    /// Returns an error only when the lifecycle thread cannot be created.
    /// Runtime, device, and transport failures arrive as [`DictationEvent::Failed`].
    pub fn spawn(self) -> Result<(DictationSession, DictationEvents), DictationError> {
        let (events, receiver) = event_channel();
        let (control, commands) = mpsc::unbounded_channel();
        let (finished, completion) = oneshot::channel();
        let capture_gate = CaptureGate::enabled();
        let task_gate = capture_gate.clone();
        let task = std::thread::Builder::new()
            .name("nanocodex-dictation".to_owned())
            .spawn(move || {
                run_thread(self, events, commands, finished, task_gate);
            })
            .map_err(|error| {
                DictationError::new(
                    DictationErrorKind::Runtime,
                    format!("failed to create dictation thread: {error}"),
                )
            })?;
        Ok((
            DictationSession {
                control,
                completion: Some(completion),
                task: Some(task),
                capture_gate,
            },
            receiver,
        ))
    }
}

/// Cheap synchronous control handle for one dictation lifecycle.
pub struct DictationSession {
    control: mpsc::UnboundedSender<Control>,
    completion: Option<oneshot::Receiver<Result<(), DictationError>>>,
    task: Option<std::thread::JoinHandle<()>>,
    capture_gate: CaptureGate,
}

impl DictationSession {
    /// Stops capture synchronously and requests ordered flush and close.
    pub fn finish(&self) {
        self.capture_gate.stop();
        let _ = self.control.send(Control::Finish);
    }

    /// Stops capture synchronously, clears retained audio, and closes.
    pub fn cancel(&self) {
        self.capture_gate.stop();
        let _ = self.control.send(Control::Cancel);
    }

    /// Returns whether the owned lifecycle thread remains active.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Stops and joins every resource owned by this attempt.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle failure, completion-channel failure, or thread panic.
    pub async fn shutdown(&mut self) -> Result<(), DictationShutdownError> {
        if self.is_running() {
            self.cancel();
        }
        let outcome = match self.completion.take() {
            Some(completion) => completion
                .await
                .map_err(|_| DictationShutdownError::CompletionChannel)?,
            None => Ok(()),
        };
        if let Some(task) = self.task.take() {
            task.join()
                .map_err(|_| DictationShutdownError::ThreadPanicked)?;
        }
        outcome.map_err(DictationShutdownError::Lifecycle)
    }
}

impl Drop for DictationSession {
    fn drop(&mut self) {
        if self.is_running() {
            self.cancel();
        }
    }
}

#[derive(Clone, Copy)]
enum Control {
    Finish,
    Cancel,
}

/// Failure category suitable for application recovery and presentation policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DictationErrorKind {
    /// The owned runtime thread could not be created or initialized.
    Runtime,
    /// The OpenAI recipe does not use managed ChatGPT authorization.
    UnsupportedAuth,
    /// The operating system denied microphone access.
    Permission,
    /// Microphone configuration, startup, or streaming failed.
    Capture,
    /// DNS, proxy, TLS, or WebSocket setup failed.
    Connect,
    /// Managed authorization was rejected after its recovery budget.
    Authorization,
    /// A known service event was malformed or reported an engine failure.
    Protocol,
    /// An active WebSocket failed.
    Transport,
    /// Finalization exceeded its bounded deadline without stable text.
    FinishTimeout,
}

/// Typed terminal dictation failure.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DictationError {
    kind: DictationErrorKind,
    message: String,
}

impl DictationError {
    const fn new(kind: DictationErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    /// Returns the caller-actionable failure category.
    #[must_use]
    pub const fn kind(&self) -> DictationErrorKind {
        self.kind
    }
}

/// Failure while explicitly joining an owned dictation lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum DictationShutdownError {
    /// The attempt ended with a typed lifecycle failure.
    #[error("dictation lifecycle failed: {0}")]
    Lifecycle(DictationError),
    /// The lifecycle thread panicked.
    #[error("dictation lifecycle thread panicked")]
    ThreadPanicked,
    /// The lifecycle exited without publishing its terminal result.
    #[error("dictation lifecycle completion channel closed")]
    CompletionChannel,
}

/// Failure to configure or operate the default microphone.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// No default microphone exists.
    #[error("no default microphone is available")]
    NoInputDevice,
    /// Capture policy is internally inconsistent.
    #[error("invalid microphone capture policy: {0}")]
    InvalidConfig(&'static str),
    /// The native audio backend rejected an operation.
    #[error("{operation}: {message}")]
    Backend {
        /// Failed operation.
        operation: &'static str,
        /// Backend diagnostic.
        message: String,
    },
    /// Default-device capture is unavailable on this target.
    #[error("default microphone capture is currently supported on macOS and Windows")]
    UnsupportedPlatform,
}

fn run_thread(
    builder: ChatGptDictationBuilder,
    events: EventSender,
    commands: mpsc::UnboundedReceiver<Control>,
    finished: oneshot::Sender<Result<(), DictationError>>,
    capture_gate: CaptureGate,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run_dictation(builder, &events, commands, capture_gate)),
        Err(error) => Err(DictationError::new(
            DictationErrorKind::Runtime,
            format!("failed to create dictation runtime: {error}"),
        )),
    };
    if let Err(error) = &result {
        send_event(
            &events,
            DictationEvent::Failed(DictationError::new(error.kind, error.to_string())),
        );
    }
    send_event(&events, DictationEvent::Stopped);
    drop(finished.send(result));
}

async fn run_dictation(
    builder: ChatGptDictationBuilder,
    events: &EventSender,
    mut commands: mpsc::UnboundedReceiver<Control>,
    capture_gate: CaptureGate,
) -> Result<(), DictationError> {
    if builder.openai.auth_mode() != OpenAiAuthMode::ChatGpt {
        return Err(DictationError::new(
            DictationErrorKind::UnsupportedAuth,
            "ChatGPT dictation requires managed ChatGPT authorization".to_owned(),
        ));
    }
    send_event(events, DictationEvent::Connecting);
    let capture_config = CaptureConfig {
        sample_rate_hz: SAMPLE_RATE_HZ,
        samples_per_chunk: SAMPLES_PER_CHUNK,
    };
    let (capture, mut microphone) =
        CaptureStream::open_with_gate(capture_config, capture_gate).map_err(map_capture)?;
    send_event(events, DictationEvent::Started);
    let connect = builder
        .openai
        .realtime("Transcribe the user's microphone input. Do not respond or speak.")
        .version(RealtimeVersion::V3)
        .transport(RealtimeTransport::WebRtc)
        .client_managed_handoffs(true)
        .connect();
    tokio::pin!(connect);
    let mut queued = VecDeque::with_capacity(PRECONNECT_CHUNKS);
    let mut next_audio_level = Instant::now();
    let mut requested = None;
    let (connection, mut server_events) = loop {
        tokio::select! {
            result = &mut connect => break result.map_err(map_realtime)?,
            command = commands.recv() => {
                let command = command.unwrap_or(Control::Cancel);
                requested = Some(command);
                capture.stop();
                if matches!(command, Control::Cancel) {
                    return Ok(());
                }
            }
            chunk = microphone.recv(), if requested.is_none() => {
                let Some(chunk) = chunk else {
                    return Err(DictationError::new(
                        DictationErrorKind::Capture,
                        "microphone stream stopped".to_owned(),
                    ));
                };
                publish_audio_level(events, &chunk, &mut next_audio_level);
                if queued.len() == PRECONNECT_CHUNKS {
                    let _ = queued.pop_front();
                }
                queued.push_back(chunk);
            }
        }
    };
    send_event(events, DictationEvent::TransportReady);
    let mut reducer = TranscriptReducer::default();
    let mut finishing = matches!(requested, Some(Control::Finish));
    let mut heard_speech = false;
    send_preconnect_audio(&connection, &mut queued, finishing).await?;
    let mut silence_chunks = 0_usize;
    let finish_deadline = tokio::time::sleep(FINISH_TIMEOUT);
    let settle_grace = tokio::time::sleep(FINALIZATION_GRACE);
    let mut final_silence = tokio::time::interval(CHUNK_DURATION);
    final_silence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(finish_deadline);
    tokio::pin!(settle_grace);
    if finishing {
        finish_deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + NO_SPEECH_FINISH_TIMEOUT);
        settle_grace
            .as_mut()
            .reset(tokio::time::Instant::now() + FINALIZATION_GRACE);
    }
    loop {
        tokio::select! {
            command = commands.recv() => {
                match command.unwrap_or(Control::Cancel) {
                    Control::Finish if !finishing => {
                        capture.stop();
                        while let Ok(chunk) = microphone.try_recv() {
                            send_chunk(&connection, chunk).await?;
                        }
                        finishing = true;
                        silence_chunks = 0;
                        let timeout = if heard_speech {
                            FINISH_TIMEOUT
                        } else {
                            NO_SPEECH_FINISH_TIMEOUT
                        };
                        finish_deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                        settle_grace.as_mut().reset(tokio::time::Instant::now() + FINALIZATION_GRACE);
                    }
                    Control::Finish => {}
                    Control::Cancel => {
                        capture.stop();
                        let _ = connection.close().await;
                        return Ok(());
                    }
                }
            }
            chunk = microphone.recv(), if !finishing => {
                let Some(chunk) = chunk else {
                    return Err(DictationError::new(
                        DictationErrorKind::Capture,
                        "microphone stream stopped".to_owned(),
                    ));
                };
                publish_audio_level(events, &chunk, &mut next_audio_level);
                send_chunk(&connection, chunk).await?;
            }
            event = server_events.recv() => {
                let event = match event {
                    Some(event) => event,
                    None => return settle_recovery(events, &reducer, RealtimeError::Closed),
                };
                match event {
                    RealtimeEvent::InputTranscriptDelta(delta) => {
                        if reducer.push_delta(&delta) {
                            send_event(events, DictationEvent::Transcript(reducer.transcript()));
                            if finishing {
                                settle_grace.as_mut().reset(
                                    tokio::time::Instant::now() + FINALIZATION_GRACE,
                                );
                            }
                        }
                    }
                    RealtimeEvent::InputTranscriptDone(text) => {
                        if reducer.finish_utterance(&text) {
                            send_event(events, DictationEvent::Transcript(reducer.transcript()));
                        }
                        if finishing {
                            return settle_finished(
                                events,
                                &mut reducer,
                                &connection,
                                heard_speech,
                            )
                            .await;
                        }
                    }
                    RealtimeEvent::TranscriptTail(entries) => {
                        if reducer.recover_tail(entries) {
                            send_event(events, DictationEvent::Transcript(reducer.transcript()));
                        }
                    }
                    RealtimeEvent::Error(message) => {
                        return settle_recovery(
                            events,
                            &reducer,
                            RealtimeError::WebSocket(message),
                        );
                    }
                    RealtimeEvent::SpeechStarted => {
                        if finishing && !heard_speech {
                            finish_deadline
                                .as_mut()
                                .reset(tokio::time::Instant::now() + FINISH_TIMEOUT);
                        }
                        heard_speech = true;
                    }
                    RealtimeEvent::SessionReady { .. }
                    | RealtimeEvent::OutputTranscriptDelta(_)
                    | RealtimeEvent::OutputTranscriptDone(_)
                    | RealtimeEvent::Audio(_)
                    | RealtimeEvent::AgentRequest { .. }
                    | RealtimeEvent::RemainSilent { .. }
                    | RealtimeEvent::ResponseStarted
                    | RealtimeEvent::ResponseDone => {}
                }
            }
            _ = final_silence.tick(), if finishing && silence_chunks < FINAL_SILENCE_CHUNKS => {
                send_silence(&connection).await?;
                silence_chunks += 1;
            }
            () = &mut settle_grace,
                if finishing
                    && !reducer.committable_text().is_empty() =>
            {
                return settle_finished(events, &mut reducer, &connection, heard_speech).await;
            }
            () = &mut finish_deadline, if finishing => {
                let _ = recover_close_tail(&mut reducer, &connection).await;
                let text = reducer.committable_text();
                if text.is_empty() {
                    return settle_empty(events, heard_speech, true);
                }
                send_event(events, DictationEvent::Finished(text));
                return Ok(());
            }
        }
    }
}

async fn send_preconnect_audio(
    connection: &RealtimeSession,
    queued: &mut VecDeque<Pcm16Chunk>,
    finishing: bool,
) -> Result<(), DictationError> {
    if !finishing {
        while let Some(chunk) = queued.pop_front() {
            send_chunk(connection, chunk).await?;
        }
        return Ok(());
    }

    let mut pace = tokio::time::interval(CHUNK_DURATION);
    pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for _ in 0..EARLY_FINISH_PREROLL_CHUNKS {
        pace.tick().await;
        send_silence(connection).await?;
    }
    while let Some(chunk) = queued.pop_front() {
        pace.tick().await;
        send_chunk(connection, chunk).await?;
    }
    Ok(())
}

async fn send_silence(connection: &RealtimeSession) -> Result<(), DictationError> {
    connection
        .send_audio(RealtimeAudio::from_samples([0_i16; SAMPLES_PER_CHUNK]))
        .await
        .map_err(map_realtime)
}

async fn send_chunk(connection: &RealtimeSession, chunk: Pcm16Chunk) -> Result<(), DictationError> {
    if chunk.sample_rate_hz != SAMPLE_RATE_HZ {
        return Err(DictationError::new(
            DictationErrorKind::Capture,
            format!("microphone produced {} Hz audio", chunk.sample_rate_hz),
        ));
    }
    connection
        .send_audio(RealtimeAudio::from_samples(chunk.samples))
        .await
        .map_err(map_realtime)
}

async fn settle_finished(
    events: &EventSender,
    reducer: &mut TranscriptReducer,
    connection: &RealtimeSession,
    heard_speech: bool,
) -> Result<(), DictationError> {
    recover_close_tail(reducer, connection).await?;
    let text = reducer.committable_text();
    if text.is_empty() {
        return settle_empty(events, heard_speech, false);
    }
    send_event(events, DictationEvent::Finished(text));
    Ok(())
}

fn settle_empty(
    events: &EventSender,
    heard_speech: bool,
    timed_out: bool,
) -> Result<(), DictationError> {
    match (heard_speech, timed_out) {
        (false, _) => {
            send_event(events, DictationEvent::NoSpeech);
            Ok(())
        }
        (true, true) => Err(DictationError::new(
            DictationErrorKind::FinishTimeout,
            "dictation finalization timed out after speech was detected".to_owned(),
        )),
        (true, false) => Err(DictationError::new(
            DictationErrorKind::Protocol,
            "dictation detected speech but finished without transcript text".to_owned(),
        )),
    }
}

async fn recover_close_tail(
    reducer: &mut TranscriptReducer,
    connection: &RealtimeSession,
) -> Result<(), DictationError> {
    let tail = connection
        .close_with_transcript_tail()
        .await
        .map_err(map_realtime)?;
    reducer.recover_tail(tail);
    Ok(())
}

fn settle_recovery(
    events: &EventSender,
    reducer: &TranscriptReducer,
    error: RealtimeError,
) -> Result<(), DictationError> {
    let text = reducer.committable_text();
    if text.is_empty() {
        Err(map_realtime(error))
    } else {
        send_event(events, DictationEvent::Finished(text));
        Ok(())
    }
}

fn send_event(events: &EventSender, event: DictationEvent) {
    match event {
        DictationEvent::Transcript(transcript) => {
            let mut latest = events
                .latest_transcript
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *latest = transcript;
            if events.transcript_pending.swap(true, Ordering::AcqRel) {
                return;
            }
            if events.sender.send(QueuedEvent::Transcript).is_err() {
                events.transcript_pending.store(false, Ordering::Release);
            }
        }
        DictationEvent::AudioLevel(level) => {
            events.latest_audio_level.store(level, Ordering::Release);
            if events.audio_level_pending.swap(true, Ordering::AcqRel) {
                return;
            }
            if events.sender.send(QueuedEvent::AudioLevel).is_err() {
                events.audio_level_pending.store(false, Ordering::Release);
            }
        }
        event => drop(events.sender.send(QueuedEvent::Lifecycle(event))),
    }
}

fn publish_audio_level(events: &EventSender, chunk: &Pcm16Chunk, next_audio_level: &mut Instant) {
    let now = Instant::now();
    if now < *next_audio_level {
        return;
    }
    *next_audio_level = now + AUDIO_LEVEL_INTERVAL;
    let peak = chunk
        .samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap_or(0);
    send_event(events, DictationEvent::AudioLevel(peak));
}

fn map_capture(error: CaptureError) -> DictationError {
    let kind = match &error {
        CaptureError::Backend { message, .. }
            if message.to_ascii_lowercase().contains("permission") =>
        {
            DictationErrorKind::Permission
        }
        CaptureError::NoInputDevice
        | CaptureError::InvalidConfig(_)
        | CaptureError::Backend { .. }
        | CaptureError::UnsupportedPlatform => DictationErrorKind::Capture,
    };
    DictationError::new(kind, error.to_string())
}

fn map_realtime(error: RealtimeError) -> DictationError {
    let kind = match error {
        RealtimeError::Authentication(_) | RealtimeError::InvalidAuthorization(_) => {
            DictationErrorKind::Authorization
        }
        RealtimeError::InvalidConfiguration(_)
        | RealtimeError::InvalidInstructions
        | RealtimeError::InvalidModel
        | RealtimeError::InvalidVoice(_)
        | RealtimeError::InvalidAudio(_)
        | RealtimeError::InvalidInitialItems(_)
        | RealtimeError::InvalidSessionId(_)
        | RealtimeError::Message(_) => DictationErrorKind::Protocol,
        RealtimeError::InvalidUrl(_)
        | RealtimeError::ConnectTimeout
        | RealtimeError::Http(_)
        | RealtimeError::WebRtc(_) => DictationErrorKind::Connect,
        RealtimeError::SendTimeout | RealtimeError::Closed | RealtimeError::WebSocket(_) => {
            DictationErrorKind::Transport
        }
    };
    DictationError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use nanocodex_oai_api::realtime::RealtimeError;

    use super::{
        DictationErrorKind, DictationEvent, DictationTranscript, TranscriptReducer, event_channel,
        send_event, settle_empty, settle_recovery,
    };

    #[test]
    fn transcript_backlog_is_coalesced_without_dropping_terminal_events() {
        let (events, mut receiver) = event_channel();
        send_event(
            &events,
            DictationEvent::Transcript(DictationTranscript {
                stable: String::new(),
                unstable: "first".to_owned(),
            }),
        );
        send_event(
            &events,
            DictationEvent::Transcript(DictationTranscript {
                stable: String::new(),
                unstable: "coalesced".to_owned(),
            }),
        );
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::Transcript(transcript)) if transcript.unstable == "coalesced"
        ));
        send_event(
            &events,
            DictationEvent::Transcript(DictationTranscript {
                stable: String::new(),
                unstable: "latest".to_owned(),
            }),
        );
        send_event(&events, DictationEvent::Finished("final".to_owned()));
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::Transcript(transcript)) if transcript.unstable == "latest"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::Finished(text)) if text == "final"
        ));
    }

    #[test]
    fn audio_level_backlog_is_coalesced() {
        let (events, mut receiver) = event_channel();
        send_event(&events, DictationEvent::AudioLevel(10));
        send_event(&events, DictationEvent::AudioLevel(20));

        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::AudioLevel(20))
        ));

        send_event(&events, DictationEvent::AudioLevel(30));
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::AudioLevel(30))
        ));
    }

    #[test]
    fn empty_finish_distinguishes_no_speech_from_failed_transcription() {
        let (events, mut receiver) = event_channel();
        assert!(settle_empty(&events, false, true).is_ok());
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::NoSpeech)
        ));
        assert_eq!(
            settle_empty(&events, true, true).unwrap_err().kind(),
            DictationErrorKind::FinishTimeout
        );
        assert_eq!(
            settle_empty(&events, true, false).unwrap_err().kind(),
            DictationErrorKind::Protocol
        );
    }

    #[test]
    fn transport_close_commits_all_available_text() {
        let (events, mut receiver) = event_channel();
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("stable"));
        assert!(reducer.push_delta("latest words"));

        assert!(settle_recovery(&events, &reducer, RealtimeError::Closed).is_ok());
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::Finished(text)) if text == "stable latest words"
        ));
    }
}
