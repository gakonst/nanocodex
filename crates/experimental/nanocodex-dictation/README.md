# nanocodex-dictation

Experimental, native streaming dictation for Nanocodex consumers. The generic
dictation lifecycle owns microphone capture, bounded engine-selected PCM
delivery, interaction control, cancellation, and normalized events. A
`SpeechToTextEngine` owns model or service setup, authentication, transcript
reduction, finalization, and engine-specific cleanup.

`ChatGptSpeechToText` is the current engine. It owns the direct ChatGPT Realtime
WebRTC media call and authenticated sideband WebSocket, keeping OpenAI wire
types and authorization out of the TUI-facing lifecycle. Other engines can use
the same typed PCM, control, output, and terminal-outcome contract without a
provider registry or changes to `DictationSession`.

Streaming engines may publish stable/revisable transcript replacements as they
arrive. Batch engines may retain the bounded PCM stream until `Finish`, perform
one inference, and return the same normalized terminal outcome.

This crate is unpublished while the ChatGPT Realtime and terminal interaction
are validated by opt-in live probes.
