mod auth;

use eyre::{Result, eyre};
use nanocodex::{
    OpenAi,
    oai::realtime::{RealtimeEvent, RealtimeInputTextRole, RealtimeSession, RealtimeVersion},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use self::auth::load_codex_auth;

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments
        .next()
        .ok_or_else(|| eyre!("usage: realtime-external <offer SDP_FILE|attach CALL_ID>"))?;
    let value = arguments
        .next()
        .ok_or_else(|| eyre!("usage: realtime-external <offer SDP_FILE|attach CALL_ID>"))?;
    if arguments.next().is_some() {
        return Err(eyre!(
            "usage: realtime-external <offer SDP_FILE|attach CALL_ID>"
        ));
    }

    let openai = OpenAi::new(load_codex_auth()?)?;
    let version = std::env::var_os("NANOCODEX_REALTIME_V3").map(|_| RealtimeVersion::V3);
    let websocket_url = std::env::var("NANOCODEX_REALTIME_WEBSOCKET_URL").ok();
    let attestation = std::env::var("NANOCODEX_REALTIME_ATTESTATION").ok();
    let (session, mut events) = match mode.as_str() {
        "offer" => {
            let offer = tokio::fs::read_to_string(value).await?;
            let mut builder = openai
                .realtime("Help the user while a caller-owned WebRTC peer carries the media.")
                .client_managed_handoffs(true);
            if let Some(version) = version {
                builder = builder.version(version);
            }
            if let Some(url) = websocket_url {
                builder = builder.websocket_url(url);
            }
            if let Some(attestation) = attestation {
                builder = builder.attestation_header(attestation);
            }
            let connection = builder.connect_with_sdp(offer).await?;
            let (answer, session, events) = connection.into_parts();
            let mut stdout = tokio::io::stdout();
            stdout.write_all(answer.as_bytes()).await?;
            stdout.flush().await?;
            (session, events)
        }
        "attach" => {
            let mut builder = openai
                .attach_realtime_call(value)
                .client_managed_handoffs(true);
            if let Some(version) = version {
                builder = builder.version(version);
            }
            if let Some(url) = websocket_url {
                builder = builder.websocket_url(url);
            }
            if let Some(attestation) = attestation {
                builder = builder.attestation_header(attestation);
            }
            builder.connect().await?
        }
        _ => {
            return Err(eyre!("unknown mode {mode:?}; expected `offer` or `attach`"));
        }
    };

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    break;
                };
                if let Some(speech) = line.strip_prefix("/say ") {
                    session.append_speech(speech).await?;
                } else {
                    session.send_text(RealtimeInputTextRole::User, line).await?;
                }
            }
            event = events.recv() => {
                let Some(event) = event else {
                    break;
                };
                print_event(&session, event).await?;
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        }
    }
    session.close().await?;
    Ok(())
}

async fn print_event(session: &RealtimeSession, event: RealtimeEvent) -> Result<()> {
    match event {
        RealtimeEvent::SessionReady { session_id } => eprintln!("session: {session_id}"),
        RealtimeEvent::InputTranscriptDone(text) => eprintln!("user: {text}"),
        RealtimeEvent::OutputTranscriptDone(text) => eprintln!("assistant: {text}"),
        RealtimeEvent::AgentRequest {
            call_id,
            prompt,
            transcript,
        } => eprintln!(
            "agent request {call_id}: {prompt} ({} transcript entries)",
            transcript.len()
        ),
        RealtimeEvent::RemainSilent { call_id } => {
            session.complete_silent_request(call_id).await?;
        }
        RealtimeEvent::TranscriptTail(tail) => {
            for entry in tail {
                eprintln!("{}: {}", entry.role, entry.text);
            }
        }
        RealtimeEvent::Error(error) => return Err(eyre!(error)),
        RealtimeEvent::SpeechStarted
        | RealtimeEvent::InputTranscriptDelta(_)
        | RealtimeEvent::OutputTranscriptDelta(_)
        | RealtimeEvent::Audio(_)
        | RealtimeEvent::ResponseStarted
        | RealtimeEvent::ResponseDone => {}
    }
    Ok(())
}
