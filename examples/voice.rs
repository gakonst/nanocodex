mod auth;

use eyre::{Result, WrapErr};
use nanocodex::{Nanocodex, OpenAi};
use nanocodex_voice::{VoiceEvent, VoiceSessionBuilder};

use self::auth::load_codex_auth;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let openai = OpenAi::new(load_codex_auth()?)?;
    let (agent, mut agent_events) = Nanocodex::builder(openai.clone())
        .instructions("Inspect the repository carefully and report only verified facts.")
        .workspace(std::env::current_dir()?)
        .build()?;

    let session_id = agent_events.request_id().to_owned();
    let agent_log = tokio::spawn(async move {
        while let Some(event) = agent_events.recv().await {
            eprintln!("agent {}: {:?}", event.seq, event.kind);
        }
    });

    // This is the same default-microphone/default-speaker lifecycle used by /voice.
    let (mut voice, mut voice_events) = VoiceSessionBuilder::new(openai, agent.clone())
        .session_id(session_id)
        .spawn()?;

    eprintln!("Starting voice. Speak normally; press Ctrl-C to stop.");
    let voice_result: Result<()> = loop {
        tokio::select! {
            event = voice_events.recv() => match event {
                Some(VoiceEvent::Connecting) => eprintln!("connecting..."),
                Some(VoiceEvent::Started { voice }) => {
                    eprintln!("listening with {voice}");
                }
                Some(VoiceEvent::Transcript { speaker, text }) => {
                    println!("{speaker}: {text}");
                }
                Some(VoiceEvent::Failed { error }) => break Err(error.into()),
                Some(VoiceEvent::Stopped) | None => break Ok(()),
            },
            signal = tokio::signal::ctrl_c() => {
                signal.wrap_err("failed to listen for Ctrl-C")?;
                eprintln!("stopping voice...");
                voice.stop();
                break Ok(());
            }
        }
    };

    let voice_shutdown = voice.shutdown().await;
    let shutdown_result = agent.shutdown().await;
    agent_log.await.wrap_err("agent event logger failed")?;
    voice_result?;
    voice_shutdown?;
    shutdown_result?;
    Ok(())
}
