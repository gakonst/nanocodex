# nanocodex-audio

Experimental native audio primitives shared by Nanocodex consumers. The crate
owns default-device microphone capture, channel mixing, bounded PCM chunking,
and streaming linear sample-rate conversion.

Consumers select their output sample rate and chunk size through
`CaptureConfig`. `CaptureStream` publishes fixed-size mono PCM16 chunks and
uses `CaptureGate` for synchronous callback shutdown. Voice playback policy,
speech recognition, provider configuration, and interaction lifecycle remain
with their owning consumers.
