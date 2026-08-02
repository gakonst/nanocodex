//! OpenAI Realtime speech-to-text implementation.

mod reducer;

use std::{collections::VecDeque, future::Future, time::Duration};

use nanocodex_oai_api::{
    OpenAi,
    auth::OpenAiAuthMode,
    realtime::{
        RealtimeAudio, RealtimeError, RealtimeEvent, RealtimeSession, RealtimeSessionMode,
        RealtimeTranscriptEntry, RealtimeTransport, RealtimeVersion,
    },
};

use crate::{
    DictationBuilder, DictationError, DictationErrorKind, SpeechAudio, SpeechAudioFormat,
    SpeechAudioStream, SpeechToTextControl, SpeechToTextControls, SpeechToTextEngine,
    SpeechToTextOutcome, SpeechToTextOutput,
};
use reducer::TranscriptReducer;

const SAMPLE_RATE_HZ: u32 = 24_000;
const SAMPLES_PER_CHUNK: usize = 480;
const CHUNK_DURATION: Duration = Duration::from_millis(20);
const RETAINED_AUDIO_CHUNKS: usize = 5 * SAMPLE_RATE_HZ as usize / SAMPLES_PER_CHUNK;
const EARLY_FINISH_PREROLL_CHUNKS: usize = 15;
const FINISH_TIMEOUT: Duration = Duration::from_secs(8);
const NO_SPEECH_FINISH_TIMEOUT: Duration = Duration::from_secs(2);
const FINALIZATION_GRACE: Duration = Duration::from_millis(500);
const FINAL_SILENCE_CHUNKS: usize = SAMPLE_RATE_HZ as usize / SAMPLES_PER_CHUNK;

/// OpenAI Realtime speech-to-text engine using the application's configured authorization.
pub struct Engine {
    openai: OpenAi,
}

impl Engine {
    /// Creates the OpenAI engine from the application's existing client recipe.
    #[must_use]
    pub const fn new(openai: OpenAi) -> Self {
        Self { openai }
    }
}

/// Dictation builder configured with the OpenAI Realtime engine.
pub type Builder = DictationBuilder<Engine>;

impl DictationBuilder<Engine> {
    /// Creates a builder from the application's existing OpenAI recipe.
    #[must_use]
    pub const fn new(openai: OpenAi) -> Self {
        Self::with_engine(Engine::new(openai))
    }
}

impl SpeechToTextEngine for Engine {
    fn audio_format(&self) -> SpeechAudioFormat {
        SpeechAudioFormat::new(SAMPLE_RATE_HZ, SAMPLES_PER_CHUNK)
    }

    fn run(
        self,
        audio: SpeechAudioStream,
        controls: SpeechToTextControls,
        output: SpeechToTextOutput,
    ) -> impl Future<Output = Result<SpeechToTextOutcome, DictationError>> {
        run(self, audio, controls, output)
    }
}

async fn run(
    engine: Engine,
    mut audio: SpeechAudioStream,
    mut controls: SpeechToTextControls,
    output: SpeechToTextOutput,
) -> Result<SpeechToTextOutcome, DictationError> {
    let auth_mode = engine.openai.auth_mode();
    let realtime = engine.openai.realtime(
        "Transcribe microphone speech verbatim, preserving punctuation and software-development terminology.",
    );
    // OpenAiAuthMode is the shared OpenAI boundary. Platform API keys use the
    // documented transcription session; managed ChatGPT credentials use the
    // private Codex Frameless call. The normalized speech-to-text lifecycle
    // spans both products, and each product uses its native protocol and model.
    let connect = match auth_mode {
        OpenAiAuthMode::ApiKey => realtime
            .version(RealtimeVersion::V2)
            .transport(RealtimeTransport::WebSocket)
            .session_mode(RealtimeSessionMode::Transcription)
            .connect(),
        OpenAiAuthMode::ChatGpt => realtime
            .version(RealtimeVersion::V3)
            .transport(RealtimeTransport::WebRtc)
            .client_managed_handoffs(true)
            .connect(),
    };
    let platform_transcription = auth_mode == OpenAiAuthMode::ApiKey;
    tokio::pin!(connect);
    let mut queued = VecDeque::with_capacity(RETAINED_AUDIO_CHUNKS);
    let mut requested = None;
    let (connection, mut server_events) = loop {
        tokio::select! {
            result = &mut connect => break result.map_err(map_realtime)?,
            command = controls.recv() => {
                let command = command.unwrap_or(SpeechToTextControl::Cancel);
                requested = Some(command);
                if matches!(command, SpeechToTextControl::Cancel) {
                    return Ok(SpeechToTextOutcome::Cancelled);
                }
            }
            chunk = audio.recv(), if requested.is_none() => {
                let Some(chunk) = chunk else {
                    return Err(DictationError::new(
                        DictationErrorKind::Transport,
                        "dictation audio stream stopped".to_owned(),
                    ));
                };
                retain_audio(&mut queued, chunk);
            }
        }
    };
    output.ready();
    let mut reducer = TranscriptReducer::default();
    let mut finishing = matches!(requested, Some(SpeechToTextControl::Finish));
    let mut heard_speech = false;
    if finishing {
        drain_available_audio(&mut audio, &mut queued);
    }
    let mut silence_chunks = 0_usize;
    send_preconnect_audio(&connection, &mut queued, finishing, platform_transcription).await?;
    if finishing && platform_transcription {
        commit_platform_audio(&connection).await?;
        silence_chunks = FINAL_SILENCE_CHUNKS;
    }
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
            command = controls.recv() => {
                match command.unwrap_or(SpeechToTextControl::Cancel) {
                    SpeechToTextControl::Finish if !finishing => {
                        while let Some(chunk) = audio.try_recv() {
                            send_chunk(&connection, chunk).await?;
                        }
                        finishing = true;
                        if platform_transcription {
                            commit_platform_audio(&connection).await?;
                            silence_chunks = FINAL_SILENCE_CHUNKS;
                        } else {
                            silence_chunks = 0;
                        }
                        let timeout = if heard_speech {
                            FINISH_TIMEOUT
                        } else {
                            NO_SPEECH_FINISH_TIMEOUT
                        };
                        finish_deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                        settle_grace.as_mut().reset(tokio::time::Instant::now() + FINALIZATION_GRACE);
                    }
                    SpeechToTextControl::Finish => {}
                    SpeechToTextControl::Cancel => {
                        let _ = connection.close().await;
                        return Ok(SpeechToTextOutcome::Cancelled);
                    }
                }
            }
            chunk = audio.recv(), if !finishing => {
                let Some(chunk) = chunk else {
                    return Err(DictationError::new(
                        DictationErrorKind::Transport,
                        "dictation audio stream stopped".to_owned(),
                    ));
                };
                send_chunk(&connection, chunk).await?;
            }
            event = server_events.recv() => {
                let event = match event {
                    Some(event) => event,
                    None => return settle_recovery(&reducer, RealtimeError::Closed),
                };
                match event {
                    RealtimeEvent::InputTranscriptDelta(delta) => {
                        heard_speech |= !delta.trim().is_empty();
                        if reducer.push_delta(&delta) {
                            output.transcript(reducer.transcript());
                            if finishing {
                                settle_grace.as_mut().reset(
                                    tokio::time::Instant::now() + FINALIZATION_GRACE,
                                );
                            }
                        }
                    }
                    RealtimeEvent::InputTranscriptDone(text) => {
                        heard_speech |= !text.trim().is_empty();
                        if reducer.finish_utterance(&text) {
                            output.transcript(reducer.transcript());
                        }
                        if finishing && silence_chunks >= FINAL_SILENCE_CHUNKS {
                            return settle_finished(&mut reducer, &connection, heard_speech).await;
                        }
                    }
                    RealtimeEvent::TranscriptTail(entries) => {
                        if recover_tail(&mut reducer, entries) {
                            output.transcript(reducer.transcript());
                        }
                    }
                    RealtimeEvent::Error(message) => {
                        return settle_recovery(&reducer, RealtimeError::WebSocket(message));
                    }
                    RealtimeEvent::SpeechStarted => {
                        reducer.speech_started();
                        if finishing && !heard_speech {
                            finish_deadline
                                .as_mut()
                                .reset(tokio::time::Instant::now() + FINISH_TIMEOUT);
                        }
                        heard_speech = true;
                    }
                    RealtimeEvent::AgentRequest { call_id, .. }
                    | RealtimeEvent::RemainSilent { call_id } => {
                        if let Err(error) = connection.complete_silent_request(call_id).await {
                            return settle_recovery(&reducer, error);
                        }
                    }
                    RealtimeEvent::SessionReady { .. }
                    | RealtimeEvent::OutputTranscriptDelta(_)
                    | RealtimeEvent::OutputTranscriptDone(_)
                    | RealtimeEvent::Audio(_)
                    | RealtimeEvent::ResponseStarted
                    | RealtimeEvent::ResponseDone => {}
                }
            }
            _ = final_silence.tick(),
                if finishing
                    && !platform_transcription
                    && silence_chunks < FINAL_SILENCE_CHUNKS =>
            {
                send_silence(&connection).await?;
                silence_chunks += 1;
            }
            () = &mut settle_grace,
                if finishing
                    && !platform_transcription
                    && silence_chunks >= FINAL_SILENCE_CHUNKS
                    && !reducer.committable_text().is_empty() =>
            {
                return settle_finished(&mut reducer, &connection, heard_speech).await;
            }
            () = &mut finish_deadline, if finishing => {
                let _ = recover_close_tail(&mut reducer, &connection).await;
                let text = reducer.committable_text();
                if text.is_empty() {
                    return settle_empty(heard_speech, true);
                }
                return Ok(SpeechToTextOutcome::Finished(text));
            }
        }
    }
}

fn drain_available_audio(audio: &mut SpeechAudioStream, retained: &mut VecDeque<SpeechAudio>) {
    while let Some(chunk) = audio.try_recv() {
        retain_audio(retained, chunk);
    }
}

fn retain_audio(retained: &mut VecDeque<SpeechAudio>, chunk: SpeechAudio) {
    if retained.len() == RETAINED_AUDIO_CHUNKS {
        let _ = retained.pop_front();
    }
    retained.push_back(chunk);
}

async fn send_preconnect_audio(
    connection: &RealtimeSession,
    queued: &mut VecDeque<SpeechAudio>,
    finishing: bool,
    platform_transcription: bool,
) -> Result<(), DictationError> {
    if !finishing || platform_transcription {
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

async fn commit_platform_audio(connection: &RealtimeSession) -> Result<(), DictationError> {
    // A silence chunk gives an unusually short caller-delimited attempt a
    // valid explicit commit.
    send_silence(connection).await?;
    connection.commit_audio().await.map_err(map_realtime)
}

async fn send_silence(connection: &RealtimeSession) -> Result<(), DictationError> {
    connection
        .send_audio(RealtimeAudio::from_samples([0_i16; SAMPLES_PER_CHUNK]))
        .await
        .map_err(map_realtime)
}

async fn send_chunk(
    connection: &RealtimeSession,
    chunk: SpeechAudio,
) -> Result<(), DictationError> {
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
    reducer: &mut TranscriptReducer,
    connection: &RealtimeSession,
    heard_speech: bool,
) -> Result<SpeechToTextOutcome, DictationError> {
    if let Err(error) = recover_close_tail(reducer, connection).await {
        return settle_error(reducer, error);
    }
    let text = reducer.committable_text();
    if text.is_empty() {
        return settle_empty(heard_speech, false);
    }
    Ok(SpeechToTextOutcome::Finished(text))
}

fn settle_empty(
    heard_speech: bool,
    timed_out: bool,
) -> Result<SpeechToTextOutcome, DictationError> {
    match (heard_speech, timed_out) {
        (false, _) => Ok(SpeechToTextOutcome::NoSpeech),
        (true, true) => Err(DictationError::new(
            DictationErrorKind::FinishTimeout,
            "dictation finalization timed out after speech was detected".to_owned(),
        )),
        (true, false) => Err(DictationError::new(
            DictationErrorKind::Protocol,
            "dictation protocol ended between speech detection and transcript delivery".to_owned(),
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
    recover_tail(reducer, tail);
    Ok(())
}

fn recover_tail(reducer: &mut TranscriptReducer, entries: Vec<RealtimeTranscriptEntry>) -> bool {
    reducer.replace_finalized(
        entries
            .into_iter()
            .filter(|entry| entry.role == "user")
            .map(|entry| entry.text),
    )
}

fn settle_recovery(
    reducer: &TranscriptReducer,
    error: RealtimeError,
) -> Result<SpeechToTextOutcome, DictationError> {
    settle_error(reducer, map_realtime(error))
}

fn settle_error(
    reducer: &TranscriptReducer,
    error: DictationError,
) -> Result<SpeechToTextOutcome, DictationError> {
    let text = reducer.committable_text();
    if text.is_empty() {
        Err(error)
    } else {
        Ok(SpeechToTextOutcome::Finished(text))
    }
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
    use std::collections::VecDeque;

    use nanocodex_oai_api::realtime::{RealtimeError, RealtimeTranscriptEntry};
    use tokio::sync::mpsc;

    use super::{
        SAMPLE_RATE_HZ, drain_available_audio, recover_tail, reducer::TranscriptReducer,
        settle_empty, settle_recovery,
    };
    use crate::{DictationErrorKind, SpeechAudio, SpeechAudioStream, SpeechToTextOutcome};

    #[test]
    fn finish_outcome_reflects_speech_detection_and_timeout() {
        assert_eq!(
            settle_empty(false, true).unwrap(),
            SpeechToTextOutcome::NoSpeech
        );
        assert_eq!(
            settle_empty(true, true).unwrap_err().kind(),
            DictationErrorKind::FinishTimeout
        );
        assert_eq!(
            settle_empty(true, false).unwrap_err().kind(),
            DictationErrorKind::Protocol
        );
    }

    #[test]
    fn transport_close_commits_all_available_text() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("stable"));
        assert!(reducer.push_delta("latest words"));

        assert_eq!(
            settle_recovery(&reducer, RealtimeError::Closed).unwrap(),
            SpeechToTextOutcome::Finished("stable latest words".to_owned())
        );
    }

    #[test]
    fn early_finish_drains_audio_already_handed_to_the_engine() {
        let (sender, receiver) = mpsc::channel(2);
        for sample in [1_i16, 2] {
            sender
                .try_send(SpeechAudio {
                    sample_rate_hz: SAMPLE_RATE_HZ,
                    samples: Box::new([sample]),
                })
                .unwrap();
        }
        let mut audio = SpeechAudioStream { receiver };
        let mut retained = VecDeque::new();

        drain_available_audio(&mut audio, &mut retained);

        assert_eq!(
            retained
                .iter()
                .map(|chunk| chunk.samples[0])
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn tail_recovery_selects_user_transcripts() {
        let mut reducer = TranscriptReducer::default();
        assert!(recover_tail(
            &mut reducer,
            vec![
                RealtimeTranscriptEntry {
                    role: "assistant".to_owned(),
                    text: "ignored".to_owned(),
                },
                RealtimeTranscriptEntry {
                    role: "user".to_owned(),
                    text: "recovered".to_owned(),
                },
            ],
        ));
        assert_eq!(reducer.committable_text(), "recovered");
    }
}
