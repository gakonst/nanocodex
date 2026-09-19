# NC1 public-surface transport prototype

> Historical prototype notes below. The integrated chord receiver, automatic discovery, durable dispatcher, and streaming path are now wired into this source. See [current integration validation](../docs/bridge-integration-20260919.md) for current evidence and remaining live gates.

Status: source/API evidence plus local simulation tests. **Not installed or live-proven.** No model connection, authenticated backend operation, screen capture, keyboard injection or real WoW roundtrip was exercised by this work. Existing game session preserved; no desktop input performed.

## Ownership and integration

Only new `addon/Transport.lua` and this `transport/` directory were changed. Existing namespace is `local ADDON, NS = ...`, with methods such as `NS.Encode`, `NS.Notify`, `NS.Snapshot`, `NS.Ask`. The active addon currently lives under `addon/Nanocodex/`; the requested new module deliberately remains at `addon/Transport.lua`. Parent must coordinate copying it into the deployed addon and adding it to TOCs. No TOC, installer, existing UI, backend, binding or user home was modified.

The module attaches `NS.Transport`; it does nothing on load. Parent enables application mode with `NS.Transport.Enable(sessionID)`. A dynamic `NS.OnTransportMessage(kind,text)` hook is required for inbound completed messages. `Enable(sessionID, rawCallback)` still provides the old low-level probe mode, but the public application Send/Status contract is disabled in raw mode.

### Minimal public API

```lua
NS.OnTransportMessage = function(kind, text)
    -- kind is reply, projects, ack or error; text is a Lua string, never Lua code.
    -- Existing Bridge.lua owns this hook; do not overwrite it at integration time.
end
local link, err = NS.Transport.Enable(sessionID)
local acknowledged, err = NS.TransportSend(payload, newRequest)
-- false,"pending": SUBMITTED, pending ACK. NOT a rejection; do not requeue.
-- false,"busy": another payload is outstanding; this payload was not submitted.
-- true,nil: this exact payload was fully ACKed by peer transport.
local status = NS.TransportStatus()
-- {connected,state,message,pending,acknowledged,ready,session,sequence,ack,
--  received,message_id,bytes_acked,bytes_total,error}
```

An explicit `newRequest=true` submits a fresh user action with a fresh application ID after the previous request is acknowledged; a pending request still returns busy. Without this argument, first call submits; repeating identical bytes only polls and never requeues, including after success. A different payload after acknowledgment begins a new request. For an intentional identical new request, set `newRequest=true`; caller-supplied idempotency keys in JSON must also identify the new intent. Payload max 16384 bytes, one outstanding message, no unbounded queue. Invalid/oversize/unavailable/busy/exhausted errors do not submit. Partial ACK never means success; completion emits `OnTransportMessage("transport_ack", receiptText)` once. An outbound request can remain pending indefinitely if no peer ACK arrives; abandoning it requires explicit new session and backend reconciliation, never automatic replay. Peer ACK proves carrier acceptance, not backend acceptance or model success.

`connected` becomes true only after a valid peer frame is accepted, and expires after 10 seconds. It does not represent account/model connectivity. `state` is disabled/idle/pending/acknowledged; byte progress is acknowledged bytes, not bytes emitted. A missing/rejecting/throwing incoming hook refuses its final chunk without advancing ACK. Integration must treat false,"pending" as queued awaiting acknowledgment, NOT "not queued"; existing UI owners were notified.

### Application message envelope

Each NC1 payload is `M` (1 byte), kind (1 byte), message ID (u16), total bytes (u16), zero-based offset (u16), then <=88 raw data bytes; all integers big endian. IDs start at 1 independently each direction and never wrap within a session. Max16384 bytes/message, contiguous chunks, one assembly. Kind Q=request JSON, R=reply text, P=ncw1 project snapshot, A=receipt, E=error. Incoming R/P/A/E dispatch reply/projects/ack/error respectively. UTF-8 may split inside a chunk and is reassembled before dispatch. Unknown kinds, gaps, interleaving, replayed IDs and oversize totals are rejected. Full inbound messages never execute code or gameplay actions.

Python `messages.fragments(kind,id,text)` produces bounded payload chunks (str encodes UTF-8; bytes preserved). `messages.Assembler(callback, accepted_kinds=('Q',))` consumes **new** Link payloads and invokes `callback(kindName, bytes)` when complete. Python `Link` suppresses retransmitted outer sequence packets before assembly. Parent owns backend/UI integration, durable idempotency and session bootstrap. CRC is corruption detection, **not authentication**. Generate a fresh nonzero 32-bit session ID locally; do not reuse after UI reload.

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

Outbound pixels: 128 bytes padded with zero, MSB first, monochrome 32x32 cells, each 4x4 UI units at UIParent TOPLEFT. Actual physical cell size depends on UI scaling and capture scaling. Use a lossless screenshot with calibrated top-left/cell size; `decode_pixels(sample, left, top, cell_size)` samples RGB interiors, rejects ambiguous values and padding, then validates CRC. Collect two independently captured matching frames. There is no automatic screen locator or scale calibration. No antialiasing or compression tolerance is claimed.

Inbound public keyboard: F21 begins/reset frame; F13..F20 encode octal digits 0..7; each byte is exactly three digits, first <=3. F22 ends frame. F23 and F24 abort/reset, reserved for future control/probe use. Maximum 333 data keypresses + two delimiters. Each key must be pressed AND released. Partial stream resets on >2 seconds inter-key gap, >30 seconds total duration, invalid octet, overflow, abort, blocked addon readiness, or a fresh F21. No text input, paste, focus calls, binding writes/overrides or secure action buttons.

The addon uses an ordinary nonsecure frame, OnKeyDown, EnableKeyboard and SetPropagateKeyboardInput(true), leaving all input propagation intact. It refuses to enable with occupied F13-F24 bindings, combat, keyboard focus, held modifiers or unavailable guard APIs. It rechecks those conditions before each key and while drawing readiness. Disable is deferred during combat because EnableKeyboard is a protected API. A dynamically installed game binding causes the receiver to stop accepting; it is never removed. The external sender must separately verify OS/compositor bindings are unused.

`driver.Pump` accepts foreground/reserved-key checks, clock and either `send_key` or `send_keys(batch)`. **Default interval is zero.** Fast path emits an entire frame (up to335 press+release key events) in one local batch; no sleep per symbol and no remote tool per key. A new ACK permits the next chunk immediately; only retransmission waits150ms. Explicit interval/burst parameters support measured fallback pacing; there is no hardcoded low-bandwidth throttle. Three attempts per wire frame; exhausted retries report unknown delivery. Input ambiguity stops without retry. Never infer delivery from successful emission.

The Lua receiver uses table CRC (native bit.bxor when available), caches unchanged serialized packets, and paints only changed cells once per game frame. It never recomputes/redraws the pixel carrier per received key symbol. Bandwidth must be measured from real ACKed payload bytes over wall time, including capture and game rendering; CPU throughput is only a ceiling.

`wayland.Desktop(exact_window_address, exact_window_class,left,top,cell_size)` is the local adapter. Parent must run it as the gaming desktop user and supply observed window identity/calibration. `capture()` uses lossless grim PNG and Pillow then returns a CRC-validated packet; feed two **independent captures** to Pump.observe. `send_keys()` checks compositor bindings and exact foreground, emits only bounded F13-F24 press/release events with a single wtype process and zero sleeps, and checks foreground afterward. The wtype man page confirms `-k KEY` is press+release. Adapter import performs no desktop actions; no focus commands or binding writes exist. Parent owns daemon loop/backend composition.

Pump requires a ready frame captured <=0.5sec ago and checks foreground/reservation before a burst. Focus loss resets partial stream so the next starts with F21. Pre/post checks cannot atomically prevent a user switching focus during a burst; a detected race stops as ambiguous. Live validation must cover this. No game-side input, screenshot calibration or event-loss rate has been measured by this thread.

## Source evidence

The mirror branch named `classic_beta` was inspected first but is 5.5.0, not the running 1.60.1 client. Matching source is branch `forever`, commit **70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e**, version **1.60.1.69913**. This matches the previous in-game build record in docs/validation.md; `WowB.exe` was observed still running as the desktop user, but the current game build was not re-read on screen.

Pinned extracted Blizzard API declarations and keyboard helper source are in `research/`:

- [Frame API](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_APIDocumentationGenerated/SimpleFrameAPIDocumentation.lua): EnableKeyboard (protected), SetPropagateKeyboardInput (restricted).
- [Texture API](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_APIDocumentationGenerated/SimpleTextureBaseAPIDocumentation.lua): SetColorTexture.
- [Blizzard keyboard helper](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_SharedXML/CustomBindingButtonMixin.lua): OnKeyDown and EnableKeyboard usage. This source is evidence of API use, not a proposal to copy its binding mutations.

Public source does not establish F13-F24 forwarding through this Hyprland/Wine/client setup or frame delivery precedence. Those are live acceptance gates.

Existing helpers inspected: scripts/install-addon.sh, scripts/install-desktop.sh, scripts/launch.py, addon Core/Bridge/TOCs, workspace OMARCHY_HAND.md. `wtype`, `grim`, `hyprctl` exist, but shell user nanocodex is not the gaming desktop user and does not own its compositor session. No arbitrary sudo or the desktop user-home reads attempted. Screen relay may rescale 1600x900 to 1280x720, making lossless local capture preferable for carrier proof. The new local adapter exists in source but has not been executed against the gaming session by this thread.

## Test and live handoff

From repository root:

```sh
python3 -m unittest discover -s transport -p 'test_*.py' -v
lua transport/test_transport.lua
lua transport/test_application.lua
lua transport/benchmark.lua  # CPU-only; not live throughput
```

Seventeen Python tests include the Lua codec/lifecycle/key-stream assertions, cross-language binary golden frame, all byte values through synthetic raster, all 888 single-bit corruptions of a maximum frame, invalid fields, bounded stop-and-wait, rejection, deduplication, lost-ACK retries, focus/readiness/reservation gates, ambiguous input stop and key stream completion. Application tests cover multi-chunk requests/replies, maximum message, UTF-8 split, partial ACK vs success, callbacks and replay/rejection; adapter tests verify one local process per packet and decode an actual synthetic PNG buffer. Synthetic pixels and mocked APIs do not establish actual screen rendering or live input. CPU-only receiver benchmark on this host: 1000 max frames /335000 symbols in0.2411sec (~389KiB/s payload parsing); 1M cached packet reads in0.0535sec. These are not real game throughput figures.

Parent's coordinated live proof must:

1. Deploy/load the new module in the existing 16001 addon while preserving the running session; no shell permission workaround. Coordinate necessary UI reload with the user/session owner.
2. Verify F13-F24 unbound in both WoW and compositor without changing bindings; enable outside combat and with no edit box focused.
3. Establish one fresh session ID on both sides. Emit a short fixed challenge using `link:Send`; capture/measure physical cells, decode the exact bytes and CRC twice from independent public screenshots.
4. With WoW already foreground, send a framed ACK + independent response via reserved keys, checking focus before each press/release; observe addon receive ACK in pixels and exactly one delivery in a native addon label. Capture evidence including session/sequence/ACK and payload hash, never credentials.
5. Prove corrupted, dropped, repeated keys cause no wrong delivery; retry correct frame without duplicate callback; verify ordinary input/bindings still work, and focus/combat/readiness loss halts sender. Measure timing.
6. Only then integrate actual backend requests and responses. A successful carrier exchange is not a successful authenticated model operation.

Remaining blockers are exact: module not loaded by existing TOCs; local adapter not yet exercised in the gaming session; no calibrated live pixels; F13-F24 input not observed; no session bootstrap/full-client integration; backend/model connectivity outside this module's proof.
