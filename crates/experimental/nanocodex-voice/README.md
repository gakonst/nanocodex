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
exists. In ChatGPT's default client-managed mode, eligible final answers are
spoken only when the latest input owns that coding turn. Provider-managed mode
also forwards standalone updates. The Nanocodex TUI wires this path automatically.

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
policy. ChatGPT defaults select native WebRTC, V3, client-managed handoffs, and no startup
context; Platform defaults retain provider-managed PCM behavior.

For ChatGPT subscriptions, `VoiceSessionBuilder::settings(VoiceSettings)` applies
the same preferences used by managed browser and Apple clients: the nine
built-in voices, extra speaking instructions, pace, background updates, handoff
routing, and acknowledgement filler. Pace and style append to the existing
instructions. Settings validate the subscription voice catalog and do not add
Platform audio options or custom voices. Non-default update preferences select
the appropriate handoff routing and take precedence over `handoff_mode`.

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
        VoiceEvent::UndeliveredAnswer { text } => println!("{text}"),
        VoiceEvent::AudioLevels { .. } | VoiceEvent::TranscriptDelta { .. }
        | VoiceEvent::Connecting | VoiceEvent::Started { .. } => {}
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

ChatGPT desktop sessions use an isolated native helper with CPAL capture,
Sonora echo cancellation/noise suppression/automatic gain control, Rubato
resampling, Opus RTP, and a 60 ms GStreamer jitter buffer. Build the development
runtime on macOS with `pnpm build:voice-native` (requires the GStreamer SDK).
Prepared native runtimes support macOS, Windows, and Linux. Explicit WebSocket
and Platform sessions retain the PCM path on macOS/Windows.

For ChatGPT, the TUI selects V3 client-managed handoffs with startup context disabled. Only
eligible completed coding answers are submitted as speech; typed input and new
voice input invalidate older speech ownership. The speech budget is 990 approximate
tokens. Unconfirmed answers are restored through `UndeliveredAnswer` on interruption
or shutdown. Consumers must render these events to preserve answers.

`/voice mute` and Ctrl-X toggle capture; `--voice-mute-key`, or
`NANOCODEX_VOICE_MUTE_KEY`, changes the binding (`none` disables it). Microphone and
speaker meters and interleaved partial captions render above the composer, with
split-flap animation. `--voice-animations false` disables motion.
`note_typed_input()` fences pending speech before submitting typed work. Native
mute applies immediately, including while the helper is connecting. Startup
requires both backend and peer readiness and retries one eligible negotiation
timeout after closing the first backend session.
