# nanocodex-dictation

Experimental, native streaming dictation for Nanocodex consumers. The generic
dictation lifecycle owns microphone capture, bounded engine-selected PCM
delivery, interaction control, cancellation, and normalized events. A
`SpeechToTextEngine` owns model or service setup, authentication, transcript
reduction, finalization, and engine-specific cleanup.

`openai::Engine` is the current engine. It selects the native Platform Realtime
transcription WebSocket for API keys and the ChatGPT Realtime WebRTC call for
managed subscription credentials. It presents both paths to the TUI through a
normalized lifecycle. Other engines implement the same typed PCM, control,
output, and terminal-outcome contract through `SpeechToTextEngine`.

Streaming engines may publish stable/revisable transcript replacements as they
arrive. Batch engines may retain the bounded PCM stream until `Finish`, perform
one inference, and return the same normalized terminal outcome.

This experimental crate uses opt-in live probes to validate its OpenAI Realtime
paths and terminal interaction.
