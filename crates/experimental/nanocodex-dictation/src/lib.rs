#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::{
    future::Future,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use nanocodex_audio::{CaptureConfig, CaptureError, CaptureGate, CaptureStream, Pcm16Chunk};
use tokio::sync::{mpsc, oneshot};

pub mod openai;

const RETAINED_AUDIO_SECONDS: usize = 5;
const AUDIO_LEVEL_INTERVAL: Duration = Duration::from_millis(60);
const CAPTURE_RELEASE_TAIL: Duration = Duration::from_millis(100);
const ENGINE_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

/// Stable and revisable composer text for one dictation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictationTranscript {
    /// Finalized and segment-stable text.
    pub stable: String,
    /// Current revisable partial hypothesis.
    pub unstable: String,
}

/// Capture-format-independent microphone peak for presentation and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MicrophoneLevel(u16);

impl MicrophoneLevel {
    /// Complete silence.
    pub const SILENCE: Self = Self(0);
    /// The largest representable microphone level.
    pub const MAX: Self = Self(i16::MAX as u16);

    /// Returns the linear peak normalized to `0.0..=1.0`.
    #[must_use]
    pub fn normalized(self) -> f64 {
        f64::from(self.0) / f64::from(i16::MAX)
    }

    const fn from_pcm16_peak(peak: u16) -> Self {
        Self(if peak > i16::MAX as u16 {
            i16::MAX as u16
        } else {
            peak
        })
    }
}

/// One normalized update from an independent dictation lifecycle.
#[derive(Debug)]
pub enum DictationEvent {
    /// Microphone startup is in progress.
    Connecting,
    /// The microphone is active and audio is retained while engine startup completes.
    Started,
    /// The speech-to-text engine is ready to consume retained and live audio.
    EngineReady,
    /// Latest normalized microphone peak, coalesced when the consumer falls behind.
    AudioLevel(MicrophoneLevel),
    /// The current stable and revisable transcript replacement.
    Transcript(DictationTranscript),
    /// Text selected for insertion after clean close or stable recovery.
    Finished(String),
    /// The attempt completed normally and detected silence.
    NoSpeech,
    /// The attempt ended with an engine or capture failure.
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
                // Clear the marker before reading the latest value. A producer
                // racing this read is either observed here or enqueues the next
                // marker, preserving delivery of the latest replacement.
                self.transcript_pending.store(false, Ordering::Release);
                let latest = self
                    .latest_transcript
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                DictationEvent::Transcript(latest.clone())
            }
            QueuedEvent::AudioLevel => {
                self.audio_level_pending.store(false, Ordering::Release);
                DictationEvent::AudioLevel(MicrophoneLevel(
                    self.latest_audio_level.load(Ordering::Acquire),
                ))
            }
        }
    }
}

#[derive(Clone)]
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

/// One fixed-size mono PCM16 chunk supplied to a speech-to-text engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechAudio {
    /// Sample rate of `samples`.
    pub sample_rate_hz: u32,
    /// Native signed mono samples.
    pub samples: Box<[i16]>,
}

/// Mono PCM16 format requested by a speech-to-text engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpeechAudioFormat {
    /// Capture sample rate after conversion from the native device format.
    pub sample_rate_hz: u32,
    /// Number of samples delivered in each chunk.
    pub samples_per_chunk: usize,
}

impl SpeechAudioFormat {
    /// Creates an engine capture format.
    #[must_use]
    pub const fn new(sample_rate_hz: u32, samples_per_chunk: usize) -> Self {
        Self {
            sample_rate_hz,
            samples_per_chunk,
        }
    }
}

/// Ordered control for one speech-to-text engine attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpeechToTextControl {
    /// No more microphone audio will arrive; finalize the available speech.
    Finish,
    /// Discard the attempt and release its resources within the bounded grace period.
    Cancel,
}

/// Receiver for bounded microphone audio.
///
/// Capture remains realtime through a bounded queue that discards newer chunks
/// during engine backpressure.
pub struct SpeechAudioStream {
    receiver: mpsc::Receiver<SpeechAudio>,
}

impl SpeechAudioStream {
    /// Waits for the next audio chunk, or returns `None` after capture closes.
    pub async fn recv(&mut self) -> Option<SpeechAudio> {
        self.receiver.recv().await
    }

    /// Receives an already-buffered audio chunk without waiting.
    pub fn try_recv(&mut self) -> Option<SpeechAudio> {
        self.receiver.try_recv().ok()
    }
}

/// Receiver for finish and cancellation control.
///
/// Control remains independent from audio backpressure.
pub struct SpeechToTextControls {
    receiver: mpsc::UnboundedReceiver<SpeechToTextControl>,
}

impl SpeechToTextControls {
    /// Waits for the next control request.
    pub async fn recv(&mut self) -> Option<SpeechToTextControl> {
        self.receiver.recv().await
    }
}

/// Backend-neutral updates published during recognition.
#[derive(Clone)]
pub struct SpeechToTextOutput {
    events: EventSender,
}

impl SpeechToTextOutput {
    /// Reports that the engine can consume retained and live audio.
    pub fn ready(&self) {
        send_event(&self.events, DictationEvent::EngineReady);
    }

    /// Replaces the current stable and revisable transcript.
    pub fn transcript(&self, transcript: DictationTranscript) {
        send_event(&self.events, DictationEvent::Transcript(transcript));
    }
}

/// Terminal result from one speech-to-text engine attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechToTextOutcome {
    /// Normalized text selected for insertion.
    Finished(String),
    /// The engine detected no speech.
    NoSpeech,
    /// The caller cancelled the attempt.
    Cancelled,
}

/// A streaming speech-to-text implementation.
///
/// Implementations own model or service setup, authentication, transcript
/// reduction, finalization, and engine-specific cleanup. The dictation
/// lifecycle independently owns microphone capture and user interaction. An
/// implementation must join any child work before returning and must not
/// publish output after its terminal outcome.
pub trait SpeechToTextEngine: Send + 'static {
    /// Returns the mono PCM16 format this engine consumes.
    fn audio_format(&self) -> SpeechAudioFormat;

    /// Runs one engine attempt until it finishes, is cancelled, or fails.
    fn run(
        self,
        audio: SpeechAudioStream,
        controls: SpeechToTextControls,
        output: SpeechToTextOutput,
    ) -> impl Future<Output = Result<SpeechToTextOutcome, DictationError>>;
}

/// Builder for one dictation lifecycle backed by `E`.
pub struct DictationBuilder<E> {
    engine: E,
}

impl<E> DictationBuilder<E> {
    /// Creates a builder around one speech-to-text engine attempt.
    #[must_use]
    pub const fn with_engine(engine: E) -> Self {
        Self { engine }
    }
}

impl<E: SpeechToTextEngine> DictationBuilder<E> {
    /// Spawns one owned runtime, capture stream, and engine attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when lifecycle thread creation fails.
    /// Runtime, device, and engine failures arrive as [`DictationEvent::Failed`].
    pub fn spawn(self) -> Result<(DictationSession, DictationEvents), DictationError> {
        let (events, receiver) = event_channel();
        let (control, commands) = mpsc::unbounded_channel();
        let (finished, completion) = oneshot::channel();
        let capture_gate = CaptureGate::enabled();
        let task_gate = capture_gate.clone();
        let task = std::thread::Builder::new()
            .name("nanocodex-dictation".to_owned())
            .spawn(move || {
                run_thread(self.engine, events, commands, finished, task_gate);
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
    /// Requests a short microphone tail followed by ordered flush and close.
    pub fn finish(&self) {
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
    /// The operating system denied microphone access.
    Permission,
    /// Microphone configuration, startup, or streaming failed.
    Capture,
    /// Engine initialization or a required remote connection failed.
    Connect,
    /// Engine authorization was rejected after its recovery budget.
    Authorization,
    /// An engine update or service event was malformed.
    Protocol,
    /// An active engine or its transport failed.
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
    /// Creates a typed engine or lifecycle failure.
    #[must_use]
    pub fn new(kind: DictationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
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

fn run_thread<E: SpeechToTextEngine>(
    engine: E,
    events: EventSender,
    commands: mpsc::UnboundedReceiver<Control>,
    finished: oneshot::Sender<Result<(), DictationError>>,
    capture_gate: CaptureGate,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run_lifecycle(engine, &events, commands, capture_gate)),
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

fn retained_audio_chunks(format: SpeechAudioFormat) -> usize {
    (format.sample_rate_hz as usize)
        .saturating_mul(RETAINED_AUDIO_SECONDS)
        .div_ceil(format.samples_per_chunk)
        .max(1)
}

async fn run_lifecycle<E: SpeechToTextEngine>(
    engine: E,
    events: &EventSender,
    mut commands: mpsc::UnboundedReceiver<Control>,
    capture_gate: CaptureGate,
) -> Result<(), DictationError> {
    send_event(events, DictationEvent::Connecting);
    let audio_format = engine.audio_format();
    let capture_config = CaptureConfig {
        sample_rate_hz: audio_format.sample_rate_hz,
        samples_per_chunk: audio_format.samples_per_chunk,
    };
    let (capture, mut microphone) =
        CaptureStream::open_with_gate(capture_config, capture_gate).map_err(map_capture)?;
    send_event(events, DictationEvent::Started);
    let (audio_sender, audio) = mpsc::channel(retained_audio_chunks(audio_format));
    let (control_sender, controls) = mpsc::unbounded_channel();
    let attempt = engine.run(
        SpeechAudioStream { receiver: audio },
        SpeechToTextControls { receiver: controls },
        SpeechToTextOutput {
            events: events.clone(),
        },
    );
    tokio::pin!(attempt);
    let cancel_deadline = tokio::time::sleep(ENGINE_CANCEL_TIMEOUT);
    let capture_tail_deadline = tokio::time::sleep(CAPTURE_RELEASE_TAIL);
    tokio::pin!(cancel_deadline);
    tokio::pin!(capture_tail_deadline);
    let mut finish_pending = false;
    let mut finishing = false;
    let mut cancelling = false;
    let mut next_audio_level = Instant::now();
    loop {
        tokio::select! {
            biased;
            command = commands.recv(), if !cancelling => {
                match command.unwrap_or(Control::Cancel) {
                    Control::Finish if !finishing && !finish_pending => {
                        finish_pending = true;
                        capture_tail_deadline
                            .as_mut()
                            .reset(tokio::time::Instant::now() + CAPTURE_RELEASE_TAIL);
                    }
                    Control::Finish => {}
                    Control::Cancel => {
                        capture.stop();
                        finish_pending = false;
                        finishing = true;
                        cancelling = true;
                        let _ = control_sender.send(SpeechToTextControl::Cancel);
                        cancel_deadline
                            .as_mut()
                            .reset(tokio::time::Instant::now() + ENGINE_CANCEL_TIMEOUT);
                    }
                }
            }
            result = &mut attempt => {
                if cancelling {
                    return Ok(());
                }
                match result? {
                    SpeechToTextOutcome::Finished(text) if !text.trim().is_empty() => {
                        send_event(events, DictationEvent::Finished(text));
                    }
                    SpeechToTextOutcome::Finished(_) => {
                        return Err(DictationError::new(
                            DictationErrorKind::Protocol,
                            "speech-to-text engine returned an empty transcript".to_owned(),
                        ));
                    }
                    SpeechToTextOutcome::NoSpeech => {
                        send_event(events, DictationEvent::NoSpeech);
                    }
                    SpeechToTextOutcome::Cancelled => {}
                }
                return Ok(());
            }
            chunk = microphone.recv(), if !finishing => {
                let Some(chunk) = chunk else {
                    return Err(DictationError::new(
                        DictationErrorKind::Capture,
                        "microphone stream stopped".to_owned(),
                    ));
                };
                publish_audio_level(events, &chunk, &mut next_audio_level);
                queue_engine_audio(&audio_sender, chunk);
            }
            () = &mut capture_tail_deadline, if finish_pending => {
                capture.stop();
                while let Ok(chunk) = microphone.try_recv() {
                    queue_engine_audio(&audio_sender, chunk);
                }
                finish_pending = false;
                finishing = true;
                let _ = control_sender.send(SpeechToTextControl::Finish);
            }
            () = &mut cancel_deadline, if cancelling => {
                tracing::warn!("speech-to-text engine cancellation deadline elapsed");
                return Ok(());
            }
        }
    }
}

fn queue_engine_audio(sender: &mpsc::Sender<SpeechAudio>, chunk: Pcm16Chunk) {
    // The engine queue retains five seconds in addition to the callback queue,
    // bounding allocation during engine backpressure.
    drop(sender.try_send(SpeechAudio {
        sample_rate_hz: chunk.sample_rate_hz,
        samples: chunk.samples,
    }));
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
            events.latest_audio_level.store(level.0, Ordering::Release);
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
    send_event(
        events,
        DictationEvent::AudioLevel(MicrophoneLevel::from_pcm16_peak(peak)),
    );
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

#[cfg(test)]
mod tests {
    use super::{
        DictationEvent, DictationTranscript, MicrophoneLevel, SpeechAudioFormat, event_channel,
        retained_audio_chunks, send_event,
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
        send_event(
            &events,
            DictationEvent::AudioLevel(MicrophoneLevel::from_pcm16_peak(10)),
        );
        send_event(
            &events,
            DictationEvent::AudioLevel(MicrophoneLevel::from_pcm16_peak(20)),
        );

        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::AudioLevel(MicrophoneLevel(20)))
        ));

        send_event(
            &events,
            DictationEvent::AudioLevel(MicrophoneLevel::from_pcm16_peak(30)),
        );
        assert!(matches!(
            receiver.try_recv(),
            Some(DictationEvent::AudioLevel(MicrophoneLevel(30)))
        ));
    }

    #[test]
    fn microphone_level_normalizes_and_clamps_the_capture_sample_scale() {
        assert_eq!(MicrophoneLevel::SILENCE.normalized(), 0.0);
        assert_eq!(MicrophoneLevel::MAX.normalized(), 1.0);
        assert_eq!(
            MicrophoneLevel::from_pcm16_peak(i16::MIN.unsigned_abs()),
            MicrophoneLevel::MAX
        );
    }

    #[test]
    fn retained_audio_capacity_follows_the_engine_format() {
        assert_eq!(
            retained_audio_chunks(SpeechAudioFormat::new(16_000, 320)),
            250
        );
        assert_eq!(
            retained_audio_chunks(SpeechAudioFormat::new(16_000, 1_600)),
            50
        );
    }
}
