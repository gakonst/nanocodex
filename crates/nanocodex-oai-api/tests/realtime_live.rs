//! Opt-in real ChatGPT WebRTC transport evidence. No microphone or speaker is opened.
#![cfg(all(feature = "realtime", not(target_family = "wasm")))]

use nanocodex_oai_api::{
    OpenAi,
    auth::load_chatgpt_auth,
    realtime::{RealtimeAudio, RealtimeEvent, RealtimeVersion},
};
use std::{path::PathBuf, time::Duration};

/// Supply signed-in ChatGPT auth and a 24 kHz mono PCM16LE recording asking
/// "What is my sample project called?" through NANOCODEX_REALTIME_TEST_PCM.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires ChatGPT auth and NANOCODEX_REALTIME_TEST_PCM; uses the real OpenAI backend"]
async fn chatgpt_webrtc_streams_speech_and_transcripts() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("nanocodex_oai_api::realtime=debug,nanocodex_oai_api::realtime::wire=off")
        .try_init();
    let auth_path = std::env::var_os("NANOCODEX_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".codex/auth.json")
        });
    let recording = std::fs::read(std::env::var("NANOCODEX_REALTIME_TEST_PCM")?)?;
    assert!(!recording.is_empty() && recording.len().is_multiple_of(2));
    let openai = OpenAi::new(load_chatgpt_auth(&auth_path)?)?;
    let started = tokio::time::Instant::now();
    let (session, mut events) = openai.realtime(
        "You are a voice assistant. The user's sample project is named Juniper. When asked its name, say only Juniper. Do not delegate."
    ).version(RealtimeVersion::V3).session_id(uuid::Uuid::now_v7().to_string()).connect().await?;
    eprintln!(
        "native ChatGPT WebRTC connected in {} ms",
        started.elapsed().as_millis()
    );
    let input = session.clone();
    let sending = tokio::spawn(async move {
        let pcm = [vec![0; 48_000], recording, vec![0; 24_000 * 2 * 20]].concat();
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for frame in pcm.chunks(960) {
            clock.tick().await;
            input.send_audio(RealtimeAudio::pcm16_le(frame)?).await?;
        }
        Ok::<_, nanocodex_oai_api::realtime::RealtimeError>(())
    });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (mut input_partials, mut output_partials, mut audio_bytes) = (0, 0, 0);
        let mut heard = false;
        let mut answered = false;
        while let Some(event) = events.recv().await {
            match event {
                RealtimeEvent::InputTranscriptDelta(_) => input_partials += 1,
                RealtimeEvent::OutputTranscriptDelta(_) => output_partials += 1,
                RealtimeEvent::InputTranscriptDone(text) => {
                    heard |= text.to_lowercase().contains("project");
                }
                RealtimeEvent::OutputTranscriptDone(text) => {
                    answered |= text.to_lowercase().contains("juniper");
                }
                RealtimeEvent::Audio(audio) => audio_bytes += audio.as_bytes().len(),
                RealtimeEvent::Error(error) => return Err(error),
                _ => {}
            }
            if heard && answered && audio_bytes > 0 {
                eprintln!("native media verified: input_partials={input_partials} output_partials={output_partials} audio_bytes={audio_bytes}");
                assert!(input_partials > 0 && output_partials > 0);
                return Ok(());
            }
        }
        Err("voice ended before the recorded request was answered".to_owned())
    }).await;
    sending.abort();
    let _ = sending.await;
    session.close().await?;
    result??;
    Ok(())
}
