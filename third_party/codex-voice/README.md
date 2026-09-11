# Upstream native voice

Ported from openai/codex `818f1cca8ccf8899f0f4d59336baebaccf358eed` (2026-09-10).
Apache-2.0 source and upstream unit tests are retained. Audio processing, RTP,
playout, mute epochs, bounded control protocol, and session actor follow upstream.
Nanocodex adapts package discovery, binary names, and owned child-process I/O.
Transport test fixtures bind loopback to avoid local VPN/VM interface routing;
production binding is unchanged. The standalone
workspace keeps native build dependencies outside the public API and WASM graph.

Build the helper with `pnpm build:voice-native`. The native library does not link
GStreamer into the embedding process. Runtime libraries and required plugins live
beside the helper in `nanocodex-resources/voice`; microphone access begins only
after negotiation. The parent never sends provider credentials to the helper.


The development staging script copies the local macOS SDK closure, relocates only
those copies, and signs the resulting private libraries. Release builds require
`--runtime <prepared-runtime> --release`; receipts must be marked `publicRelease`
and match the selected target and source manifest. GNU Linux helper relocation
requires `patchelf`. The helper and embedding must use the same
`STABLE_GIT_COMMIT` (both default to `dev` in local builds). Keep the entire
`nanocodex-resources/voice` directory next to the embedding executable.
`NANOCODEX_VOICE_PACKAGE` can name an alternate package root containing that tree.

Validation:

```sh
cargo test --locked --manifest-path third_party/codex-voice/Cargo.toml -p nanocodex-voice-host -p nanocodex-voice-native
pnpm build:voice-native
NANOCODEX_TEST_VOICE_RUNTIME="$PWD/target/debug/nanocodex-resources/voice" cargo test --locked --manifest-path third_party/codex-voice/Cargo.toml -p nanocodex-voice-native --test packaged -- --ignored
```

The relocation test starts the physical helper from a path containing spaces,
initializes its private runtime, gathers a real audio SDP offer, and closes it.
It also checks duplicate initialization and build mismatch rejection. Device
processing tests use synthetic audio; these checks do not replace a live
microphone/speaker conversation against the authenticated Realtime service.
