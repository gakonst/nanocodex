# Upstream native voice

Ported from openai/codex `818f1cca8ccf8899f0f4d59336baebaccf358eed` (2026-09-10).
Apache-2.0 source and upstream unit tests are retained. Audio processing, RTP,
playout, mute epochs, bounded control protocol, and session actor follow upstream.
Nanocodex adapts package discovery, binary names, and owned child-process I/O.
The speaker format refresh fix from upstream
`7ef70f95d5c07976f3c992e413a02a6db238b0d0` (2026-09-11) is also included:
Bluetooth microphone activation can change the output sample rate, so speaker
restarts requery the device and rebuild render/playback conversion.
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

Stable, nightly, and PR distribution jobs build `nanocodex-voice-<target>.tar.gz`
with `scripts/build-voice-release.py` and checksum it alongside the executables.
The script uses the pinned upstream native build tools, verifies every source
archive, and seals a public-release runtime receipt; Homebrew development
libraries are not used for release packages. CI extracts the final archive and
tests the relocated helper before publishing it. The helper and both CLIs share
the release's `STABLE_GIT_COMMIT`.

The installer and updater place this archive's files inside the selected version
directory before activation. Cached runtimes are checked file by file; missing
or corrupt resources trigger repair. On first voice use, managed stable installations
created by older updaters fetch the matching checksummed runtime automatically.
Custom development packages remain caller-owned. Releases predating the runtime asset remain
installable. An advertised runtime without a matching checksum or downloadable
asset fails installation, retaining the previously active version.

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
