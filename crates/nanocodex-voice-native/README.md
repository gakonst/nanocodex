# Nanocodex Native Voice

Client for the retained native realtime voice host. The host executable owns audio capture, playback, and WebRTC; this library owns the client connection and typed session lifecycle.

The native host remains under `third_party/codex-voice/voice-host`. This adapter retains its Apache-2.0 license and the original source attribution.

The async session PCM API (`begin_pcm_stream`, `write_pcm`, `drain_pcm`, `cancel_pcm`)
feeds the macOS host's existing libWebRTC mixer and output/AEC path. It supports
signed mono samples at 16/24/48 kHz, chunks of at most `MAX_PCM_SAMPLES` (960), and
strictly increasing nonzero stream generations. Writes apply bounded backpressure;
call cancel when abandoning a stream. Receiver suppression stays independent from
external PCM and microphone mute. See `third_party/codex-voice/README.md` for native
queue bounds, drain limitations, tests and package instructions. Other native
helper engines currently report unsupported rather than opening another player.
