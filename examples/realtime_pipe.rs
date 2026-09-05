mod auth;

use nanocodex::{
    Nanocodex, OpenAi, PromptRoute,
    oai::realtime::{RealtimeAgentSteer, RealtimeAudio, RealtimeEvent},
};
use nanocodex_voice::{codex_realtime_delegation_with_transcript, codex_voice_instructions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

use self::auth::load_codex_auth;

struct CompletedRequest {
    generation: u64,
    call_id: String,
    output: String,
}

struct ActiveRequest {
    generation: u64,
    call_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAi::new(load_codex_auth()?)?;
    let (agent, mut agent_events) = Nanocodex::builder(openai.clone())
        .instructions("Act as a coding agent. Inspect before claiming and preserve unrelated work.")
        .workspace(std::env::current_dir()?)
        .build()?;
    let session_id = agent_events.request_id().to_owned();
    let agent_log = tokio::spawn(async move {
        while let Some(event) = agent_events.recv().await {
            eprintln!("agent {}: {:?}", event.seq, event.kind);
        }
    });
    let mut realtime = openai
        .realtime(codex_voice_instructions())
        .session_id(session_id);
    if let Ok(attestation) = std::env::var("NANOCODEX_REALTIME_ATTESTATION") {
        realtime = realtime.attestation_header(attestation);
    }
    let (realtime, mut events) = realtime.connect().await?;

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut input = [0_u8; 1_920];
    let mut trailing_byte = None;
    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel();
    let mut active_request = None;
    let mut next_request = 0_u64;

    loop {
        tokio::select! {
            read = stdin.read(&mut input) => {
                let read = read?;
                if read == 0 {
                    break;
                }
                let mut pcm = Vec::with_capacity(read + usize::from(trailing_byte.is_some()));
                if let Some(byte) = trailing_byte.take() {
                    pcm.push(byte);
                }
                pcm.extend_from_slice(&input[..read]);
                if pcm.len() % 2 != 0 {
                    trailing_byte = pcm.pop();
                }
                realtime.send_audio(RealtimeAudio::pcm16_le(pcm)?).await?;
            }
            event = events.recv() => {
                let Some(event) = event else {
                    break;
                };
                match event {
                    RealtimeEvent::Audio(audio) => {
                        stdout.write_all(audio.as_bytes()).await?;
                        stdout.flush().await?;
                    }
                    RealtimeEvent::AgentRequest { call_id, prompt, transcript } => {
                        match agent.route_prompt(
                            codex_realtime_delegation_with_transcript(&prompt, &transcript)
                        ).await {
                            Ok(PromptRoute::Started(turn)) => {
                                next_request = next_request.saturating_add(1);
                                let generation = next_request;
                                active_request = Some(ActiveRequest {
                                    generation,
                                    call_id: call_id.clone(),
                                });
                                let completed_tx = completed_tx.clone();
                                tokio::spawn(async move {
                                    let output = match turn.await {
                                        Ok(result) => result.final_message().to_owned(),
                                        Err(error) => {
                                            format!("The coding agent failed: {error}")
                                        }
                                    };
                                    drop(completed_tx.send(CompletedRequest {
                                        generation,
                                        call_id,
                                        output,
                                    }));
                                });
                            }
                            Ok(PromptRoute::Steered) => {
                                if realtime.steer_agent_request(&call_id).await?
                                    == RealtimeAgentSteer::ReplacedDelegation
                                    && let Some(active) = &mut active_request
                                {
                                    active.call_id = call_id;
                                }
                            }
                            Err(error) => {
                                realtime
                                    .append_agent_output(
                                        &call_id,
                                        format!(
                                            "The coding agent rejected the request: {error}"
                                        ),
                                    )
                                    .await?;
                                realtime.complete_agent_run(call_id).await?;
                            }
                        }
                    }
                    RealtimeEvent::RemainSilent { call_id } => {
                        realtime.complete_silent_request(call_id).await?;
                    }
                    RealtimeEvent::InputTranscriptDone(text) => {
                        eprintln!("user: {text}");
                    }
                    RealtimeEvent::OutputTranscriptDone(text) => {
                        eprintln!("assistant: {text}");
                    }
                    RealtimeEvent::TranscriptTail(tail) => {
                        for entry in tail {
                            eprintln!("{}: {}", entry.role, entry.text);
                        }
                    }
                    RealtimeEvent::Error(error) => return Err(error.into()),
                    RealtimeEvent::SessionReady { .. }
                    | RealtimeEvent::SpeechStarted
                    | RealtimeEvent::InputTranscriptDelta(_)
                    | RealtimeEvent::OutputTranscriptDelta(_)
                    | RealtimeEvent::ResponseStarted
                    | RealtimeEvent::ResponseDone => {}
                }
            }
            completed = completed_rx.recv() => {
                let Some(CompletedRequest { generation, call_id, output }) = completed else {
                    break;
                };
                let call_id = match active_request.take() {
                    Some(active) if active.generation == generation => active.call_id,
                    Some(active) => {
                        active_request = Some(active);
                        call_id
                    }
                    None => call_id,
                };
                if !output.trim().is_empty() {
                    realtime.append_agent_output(&call_id, output).await?;
                }
                realtime.complete_agent_run(call_id).await?;
            }
        }
    }

    realtime.close().await?;
    agent.shutdown().await?;
    agent_log.await?;
    Ok(())
}
