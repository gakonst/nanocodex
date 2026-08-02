use std::{io, path::PathBuf, time::Instant};

use nanocodex_dictation::{DictationEvent, openai as openai_dictation};
use nanocodex_oai_api::{OpenAi, auth::load_chatgpt_auth};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = match std::env::args_os().nth(1).map(PathBuf::from) {
        Some(auth_file) => OpenAi::new(load_chatgpt_auth(auth_file)?)?,
        None => OpenAi::new(std::env::var("OPENAI_API_KEY").map_err(
            |_| "usage: OPENAI_API_KEY=... dictation_probe, or dictation_probe /path/to/auth.json",
        )?)?,
    };
    let (mut session, mut events) = openai_dictation::Builder::new(openai).spawn()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let (release, mut released) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            eprintln!("Speak now, then press Enter to flush and close.");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            let _ = release.send(());
        });
        let started_at = Instant::now();
        let mut saw_started = false;
        let mut release_requested = false;
        let mut finished = None;
        loop {
            tokio::select! {
                result = &mut released, if !release_requested => {
                    result.map_err(|_| "release input thread stopped")?;
                    release_requested = true;
                    session.finish();
                    eprintln!("{:?} audio.flush + session.close", started_at.elapsed());
                }
                event = events.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    match event {
                        DictationEvent::Connecting => {
                            eprintln!("{:?} connecting", started_at.elapsed());
                        }
                        DictationEvent::Started => {
                            saw_started = true;
                            eprintln!(
                                "{:?} microphone capture started",
                                started_at.elapsed()
                            );
                        }
                        DictationEvent::EngineReady => {
                            eprintln!("{:?} speech-to-text engine ready", started_at.elapsed());
                        }
                        DictationEvent::Transcript(transcript) => {
                            eprintln!(
                                "{:?} transcript stable_bytes={} unstable_bytes={}",
                                started_at.elapsed(),
                                transcript.stable.len(),
                                transcript.unstable.len()
                            );
                        }
                        DictationEvent::AudioLevel(_) => {}
                        DictationEvent::Finished(text) => {
                            eprintln!("{:?} transcript.final bytes={}", started_at.elapsed(), text.len());
                            finished = Some(text);
                        }
                        DictationEvent::NoSpeech => break,
                        DictationEvent::Failed(error) => return Err(error.to_string().into()),
                        DictationEvent::Stopped => break,
                    }
                }
            }
        }
        assert!(saw_started, "microphone capture did not start");
        assert!(
            finished.as_ref().is_some_and(|text| !text.is_empty()),
            "no final transcript arrived after flush"
        );
        session.shutdown().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
