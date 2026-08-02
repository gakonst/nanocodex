# Experimental crates

This directory contains complete Nanocodex components whose APIs are still
being exercised and revised:

- [`nanocodex-audio`](nanocodex-audio/README.md): native PCM capture, channel
  mixing, bounded chunking, and streaming sample-rate conversion.
- [`nanocodex-dictation`](nanocodex-dictation/README.md): engine-neutral
  streaming dictation with native capture and OpenAI transcription.
- [`nanocodex-voice`](nanocodex-voice/README.md): default-device desktop audio
  and an owned GPT Realtime voice-to-agent lifecycle.
- [`nanocodex-vm`](nanocodex-vm/README.md): VM lifecycle and image preparation
  plus retained guest-backed workspace tools.
- [`nanocodex-browser`](nanocodex-browser/README.md): deterministic browser
  control, diagnostics, artifacts, and headed-browser VM composition.
- [`nanocodex-egress`](nanocodex-egress/README.md): authenticated loopback
  HTTP(S) forwarding, application-owned middleware, and host-owned secrets.

Experimental means API stability, not reduced engineering standards. These
packages remain workspace members and must pass the normal formatting, Clippy,
documentation, test, cancellation, tracing, and benchmark gates. They are not
published as part of the stable crates.io release.

Stable crates may not depend on experimental crates. Executables and examples
may consume them so that the APIs can mature against real workloads before
promotion into `crates/`.
