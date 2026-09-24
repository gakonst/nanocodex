# NC1 public-surface transport

The addon and external companion exchange bounded NC1 messages through a pixel carrier and guarded keyboard input. `addon/Transport.lua` is copied into the addon by the installer and package builder; the client manifests load it after `Client.lua`. [Addon controls](../docs/addon.md) describe opt-in startup, and the [native client contract](../docs/native-client.md) describes application envelopes, integration checks and live validation requirements.

## Components

- `protocol.py` implements frame encoding, CRC validation, acknowledgement and retransmission state. `messages.py` fragments and assembles application messages with at most 16 KiB of content and 88 content bytes per fragment.
- `driver.py` pumps captured frames and bounded input batches. It requires two independently captured matching frames and observes peer acknowledgement before treating bytes as delivered.
- `wayland.py` and `x11.py` provide desktop adapters; `session.py` validates desktop-session ownership. Foreground and reserved-key checks guard input.
- `autoconnect.py` discovers the visible carrier and coordinates startup. `calibrate.py` and `probe.py` provide calibration and diagnostic entrypoints.
- `daemon.py` journals requests, carrier state and receipts in SQLite. `dispatch.py` connects application requests to the companion backend, and `streaming.py` carries incremental replies.

Carrier acknowledgement confirms transport acceptance. It does not establish authenticated backend acceptance or a model response. Uncertain external operations require reconciliation through the durable journal.

## Wire format

Big endian, 15..111 bytes, max payload 96 bytes:

| Field | Bytes | Meaning |
|---|---:|---|
| Magic | 3 | ASCII NC1 |
| Flags | 1 | 0 or 1; bit 0 = addon receiver ready |
| Session | 4 | Nonzero local session identifier |
| Sequence | 2 | 0 = ACK-only; 1..65535 data, no wrap |
| ACK | 2 | Last accepted peer sequence |
| Length | 1 | 0..96 payload bytes |
| Payload | Length | Opaque bytes, UTF-8 may span packets |
| CRC | 2 | CRC-16/CCITT-FALSE over preceding bytes; init FFFF, polynomial 1021 |

Stop-and-wait, one outstanding payload each direction; ACK piggybacked. Reject mismatched session, future ACK, sequence gaps, conflicting duplicate payloads, oversize, malformed and corrupted frames. Duplicate accepted data is ACKed without callback redelivery. A callback returning false or throwing does not advance receive state. Callbacks with external effects require their own durable idempotency: carrier ACK is not proof of backend execution. Sequence exhaustion requires a new session. Lua keeps the current outgoing frame visible until ACK; no growing queue.

## Local validation

Run from `examples/wow`:

```sh
python3 -m unittest discover -s tests -v
python3 -m unittest discover -s transport -p 'test_*.py' -v
bash addon/tests/runall.sh
lua transport/test_transport.lua
lua transport/test_application.lua
lua transport/benchmark.lua  # CPU throughput only
```

These suites exercise codecs, malformed frames, acknowledgement/replay behavior, application assembly, desktop guards and the addon/bridge journey using synthetic data. CPU throughput and mocked desktop APIs do not establish real game throughput or live delivery. See the [live validation requirements](../docs/native-client.md#live-validation-and-limits).

## API source references

The original API review used the pinned Blizzard source version 1.60.1.69913:

- [Frame API](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_APIDocumentationGenerated/SimpleFrameAPIDocumentation.lua): EnableKeyboard (protected), SetPropagateKeyboardInput (restricted).
- [Texture API](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_APIDocumentationGenerated/SimpleTextureBaseAPIDocumentation.lua): SetColorTexture.
- [Blizzard keyboard helper](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_SharedXML/CustomBindingButtonMixin.lua): OnKeyDown and EnableKeyboard usage. This source is evidence of API use, not a proposal to copy its binding mutations.

These references document public APIs. Current client compatibility, keyboard forwarding, capture calibration and visible reply delivery require live validation.
