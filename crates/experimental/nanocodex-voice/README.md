# nanocodex-voice

`nanocodex-voice` is the experimental reusable desktop consumer for
Nanocodex's device-neutral GPT Realtime API. It connects the default microphone
and speaker, delegates repository work to an existing retained `Nanocodex`
agent, and exposes lifecycle and transcript updates as typed events.

Realtime handoffs use the agent's atomic live-input router. The first coding
request starts an independently awaitable turn; follow-up speech received while
that turn is running is admitted to its bounded steering queue and joins at the
next safe model boundary, including after an in-flight tool result. Realtime V2
acknowledges the steering tool call immediately; Frameless retargets the open
delegation to the newest request. Neither path waits behind the active turn as
a second queued request.

While voice is active, mirror work started by typed input through
`VoiceSession::observe_agent_event`. The result joins an open handoff when one
exists and otherwise reaches Realtime as a standalone update, so the voice
model stays synchronized with typed work. The Nanocodex TUI wires this path
automatically.

`VoiceSession::append_text` and `VoiceSession::append_speech` expose Codex's
bounded live control queues to other inputs. They return after queue acceptance
instead of waiting for a provider round trip; transport failures remain typed
Realtime events. Empty speech is ignored and speakable output uses Codex's
1,000-token bound.

Voice-started coding work remains independently controllable when the audio
session stops or reconnects. Retain a `VoiceAgentControl`, pass it to each
replacement session with `VoiceSessionBuilder::agent_control`, and route the
embedding's normal interrupt gesture through `VoiceAgentControl::cancel`.
Stopping voice disconnects Realtime; cancelling the controller interrupts the
coding turn.

The crate owns Codex's Realtime policy: lifecycle developer markers, bounded
startup context, transcript-tail flushing, typed-turn mirroring, delegation
markers, tool descriptions, handoff routing, and protocol-specific steering.
`nanocodex-agent` only supplies protocol-neutral live-input, developer-context,
and read-only session-context capabilities.

The builder exposes V1/V2/V3, WebSocket/WebRTC, conversation/transcription,
audio/text output, initial items, client-managed handoffs, responses-as-items,
item prefixes, thinking/commentary/BEM routing, configurable BEM prefixes,
delegation acknowledgement filler, startup-context policy, and tail-flush
policy. Defaults follow Codex for the selected authentication mode.

```rust,no_run
use nanocodex::{Nanocodex, OpenAi};
use nanocodex_voice::{VoiceAgentControl, VoiceEvent, VoiceSessionBuilder};

# async fn example(openai: OpenAi, agent: Nanocodex) -> Result<(), Box<dyn std::error::Error>> {
let agent_control = VoiceAgentControl::default();
let (mut voice, mut events) = VoiceSessionBuilder::new(openai, agent)
    .agent_control(agent_control.clone())
    .spawn()?;
while let Some(event) = events.recv().await {
    match event {
        VoiceEvent::Transcript { speaker, text } => println!("{speaker}: {text}"),
        VoiceEvent::Failed { error } => return Err(error.into()),
        VoiceEvent::Stopped => break,
        VoiceEvent::Connecting | VoiceEvent::Started { .. } => {}
    }
}
voice.shutdown().await?;
let _cancelled = agent_control.cancel().await?;
# Ok(())
# }
```

The lower `nanocodex-oai-api::realtime` module remains the transport contract
for custom devices, pipes, and non-desktop embeddings. This crate deliberately
packages one opinionated native lifecycle rather than moving audio-device
policy into the public OpenAI boundary. The Nanocodex Ratatui `/voice` command
is a thin consumer of this crate.

Default-device capture and playback are currently implemented on macOS and
Windows. Other native targets return a typed unsupported-platform failure and
can continue to use the raw PCM Realtime API.
