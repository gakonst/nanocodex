# Owned Linux audio validation

`tests/linux_audio.rs` contains three pure tests and an opt-in real PulseAudio test. `run.py` exercises a complete Linux `skyre` binary through public MCP: native default audio, stereo content, private media output, consumption, cross-kernel ownership, reset and disconnect cleanup. Both use only a generated tone, a private null sink and a separate silent default source.

Build this directory's Dockerfile as `skyre-owned-linux-audio:20260907-clippy`. Its base image is the task-owned `skyre-owned-linux-x11:20260907`; on a fresh machine that base can be built from `tests/linux/Dockerfile`. The fixture image adds PulseAudio development/runtime packages and an unprivileged test user. Build the Linux binary and tests inside this image using an isolated source copy and task-specific Cargo target/cache directories. The complete source and exact commands used for the retained run are sealed under `evidence/linux-audio/2026-09-07`.

Run the compiled full binary with the fixture script in a **new** container, using `--network none --user 17072:17072`. Mount only the Linux binary and `run.py`, both read-only; do not mount host audio devices, Pulse sockets, personal profiles or the host X11 socket. Invoke `python3 /fixture/run.py --binary /fixture/skyre`. The script creates its own `/tmp/skyre-linux-audio-integration` directory and writes its transcript, PCM WAV, independent frequency measurements and pass report there. Retain the stopped container until `docker cp` has copied that directory to a new artifact directory, then remove only that owned container.

For the provider test, invoke its compiled `linux_audio-*` test executable with `--test-threads=1` in another fresh container with the same network/user restrictions and these environment variables:

```text
SKYRE_RUN_LINUX_AUDIO_NATIVE=1
SKYRE_LINUX_AUDIO_FIXTURE_DIR=/tmp/skyre-linux-audio-owned
PULSE_SERVER=unix:/tmp/skyre-linux-audio-owned/pulse.sock
```

An ordinary Cargo test run leaves that live test inactive. The native test also proves no daemon autospawn when the selected socket is absent, source-move rejection, owner checks, consume-once behavior, cancellation, Drop cleanup and failure on loss of its owned server. Fixture `pactl` and `paplay` calls are oracle setup/inspection; production audio code makes native `libpulse` calls only. Recorded duration is capped, not padded when a cold device supplies fewer samples.
