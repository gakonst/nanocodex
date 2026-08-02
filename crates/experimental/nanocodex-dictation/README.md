# nanocodex-dictation

Experimental, native streaming dictation for Nanocodex consumers. The crate
owns microphone capture, a direct ChatGPT Realtime WebRTC media call, its
authenticated sideband WebSocket, transcript reduction, cancellation, and
cleanup. Consumers receive normalized lifecycle and transcript events while
the crate owns credentials and wire messages.

This crate is unpublished while the ChatGPT Realtime and terminal interaction
are validated by opt-in live probes.
