# Addon and bridge integration validation — 2026-09-19

This standalone source preserves the independent addon/companion project. No deployment, game input, game reload, publisher restart, service restart or account access was performed for this change. An earlier backend stream was reported working; that report is not evidence of this source running in WoW.

## Behavior

- A deliberate repeated Ask receives a fresh NC1 application identity after carrier acknowledgement. A second click while pending returns busy. Status polling never resubmits.
- Carrier delivery receipts use `transport_ack`; backend `ack` messages retain the distinction between local queueing, remote acceptance and completed organization actions. None of these proves a model reply.
- Rejected project snapshots, invalid replies and unknown message kinds return false to the receiver. The final carrier chunk remains unacknowledged, including on replay.
- `/nc bridge on` and `/nc bridge auto` opt into automatic startup. Startup waits for saved variables, world entry, chat focus, combat and binding guards. Repeated opt-in preserves an active session and its pending bytes. Teardown in combat must finish before re-enabling.
- Existing stream projection delivers incremental UTF-8 text and a canonical final reply, with replay deduplication and bounded retention. Display escapes WoW markup pipes.

## Reproducible checks

Run at repository root with Python, Lua and the existing Python image dependencies available:

```sh
.venv/bin/python -m unittest discover -s tests -v
.venv/bin/python -m unittest discover -s transport -p 'test_*.py' -v
bash addon/tests/runall.sh
lua transport/test_autostart_transport.lua
lua transport/test_chord_transport.lua
```

135 application Python tests and 77 transport Python tests passed. The four addon Lua suites and explicit auto-start/chord suites passed. The stdin-driven `transport/test_streaming_pipeline.lua` is invoked with generated frames by `tests/test_streaming.py`; it must not be run standalone without its input fixture.

`tests/test_addon_bridge_integration.py` drives the real Lua request encoder, carrier painter, chord receiver, application assembler and stream receiver through the Python raster decoder, desktop adapter, bridge journals and dispatcher. Only external desktop/WoW APIs and the backend are mocked. It proves:

1. Identical explicit questions produce separate durable identities and exactly one backend send per identity; pending clicks are rejected.
2. Backend acceptance reaches addon status without claiming a reply.
3. Duplicate carrier bursts do not duplicate text.
4. A multi-chunk UTF-8 stream survives daemon restart while Lua retains partial assembly. The final partial text is displayed before completion, then replaced with the canonical final answer and Completed status.
5. Transport evidence continues to report `model_roundtrip_proven=false` for the fixture.

The integration fixture observes the display callback with mocked WoW APIs. The separate UI suite checks the actual panel answer widget. Neither is a screenshot or live-rendering proof.

The 40 restored static companion assets and their manifest passed exact-file allowlist, no-symlink, byte-size and SHA-256 checks. Their original provenance and private-use limitations are recorded in [visual-assets.md](visual-assets.md). `SOURCE-MANIFEST.json` records imported original bytes, not post-edit hashes. Runtime databases, account configuration, caches, dependencies and release archives are excluded from source.

## Remaining live validation

A future authorized live check must establish actual game texture capture/calibration, guarded chord input reaching the addon, request arrival at the connected backend, and visible incremental then final text in the in-game panel. Timing, modifier delivery and the supported client APIs remain unproven in the real running client. This change leaves the live game and streaming services untouched.
