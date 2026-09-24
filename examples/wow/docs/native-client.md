# Native thread client

The native addon and external companion provide thread browsing, history, sending, streaming, stop and reconnect. Local integration fixtures exercise these paths with mocked game APIs and account responses. They do not establish delivery or rendering in a running WoW client.

## Implementation

`Core.lua` provides a draggable native right-side panel with searchable account threads, selection/history, composer, new chat, earlier history, stop and reconnect controls. `Projects.lua` retains project management and exposes the flat thread roster. New `Client.lua` handles bounded page assembly, view generations, thread-specific stream routing and companion actions. All six TOCs load it. The 114-pixel thread viewport reuses six buttons regardless of catalog size; the 460×700 panel scales to fit UIParent on short displays. An independent event frame pumps page continuations and passive rendering even while the panel is hidden.

`transport/dispatch.py` adds strict `refresh_projects` pagination, `load_history`, `stop_turn`, `connection_status` and `reconnect` requests. Creation can omit a selected project. Paged refresh performs one authorized `GET /api/workspace`, whose project/thread projection shares one upstream `/v1/agents` read regardless of the number of projects. Subsequent pages read only the immutable journal snapshot. Requests still pass through the real SQLite intent journal before backend operations; replay never repeats an uncertain mutation. Existing backend routes and `DurableBackend.resume()` provide the operations. The companion continues to own credentials and account authorization.

The daemon ownership lock now uses a private, regular 0600 `.lock` sidecar. Locking SQLite's database file with `flock` conflicts with SQLite writes on macOS. The sidecar remains in place across shutdown to avoid unlink/recreate races, and a second owner, symlink, unsafe mode or nonregular file is rejected. Before SQLite opens, a nonblocking probe rejects an existing legacy database-file owner with an upgrade message. The probe releases immediately so it cannot conflict with subsequent SQLite writes; a separate-process regression verifies rejection before EventStore initialization and successful writes after the old owner exits. Stop all old daemons before upgrading; old and new lock conventions must not run concurrently.

## Application envelopes

NC1 framing, retransmission, carrier ACKs, 88-byte message fragments, 16 KiB application messages and 4 KiB stream envelopes are unchanged. All fields below except raw history body use percent encoding where needed.

| Kind | Envelope |
| --- | --- |
| A | `ncm1 TAB send TAB request TAB thread TAB turn TAB local_queued-or-remoteaccepted` |
| A | `ncm1 TAB reply TAB request TAB thread TAB turn` before legacy nonstreaming R completion |
| A | `ncm1 TAB created TAB request TAB project TAB thread TAB title` |
| A | `ncm1 TAB projects TAB snapshot TAB page TAB pages`, followed by a P `ncw1` page |
| A | `ncm1 TAB connection TAB connected-or-disconnected` |
| A | `ncm1 TAB stop TAB thread TAB turn TAB requested` |
| R | `nch1 TAB snapshot TAB thread TAB page TAB pages TAB before TAB has_more TAB view LF raw text` |

Page indexes begin at zero. The companion journals immutable snapshots under the first request identity. Subsequent page requests refer to that identity and never refetch mutable account data. Roster pages are combined and validated atomically. History snapshots additionally bind to thread and view identity. The addon only schedules bounded read-only continuation requests; it does not automatically retry mutations.

The roster includes open and locally closed threads, within 4,096 combined project/chat rows / 1 MiB / 128 pages. A project with one chat consumes two rows; the limit supports 2,048 such projects. History retains its separate 256 KiB / 32-page cap. One history fetch reads 64 source events; earlier batches use the backend's cursor. Oversized snapshots are explicit failures, not silent truncation. Earlier history exceeding display retention leaves the existing text and reports the limit.

## Test evidence

The native integration fixture in `tests/test_addon_bridge_integration.py` runs a Lua peer loading real Context/Projects/Bridge/Client/Transport modules. It traverses the actual painter, chord receiver, message assembler, Python raster decoder/desktop adapter, carrier pump, bridge journal and dispatcher. Only game/desktop APIs and account responses are mocked.

The integration journey covers:

1. Multi-page project/thread roster refresh and a separate account-status receipt.
2. Multi-page UTF-8 history from one immutable backend read, and an earlier-history cursor.
3. A native send with a pending second click rejected; partial streamed reply delivery before completion.
4. Stop using the exact thread/turn from the accepted send receipt.
5. Selecting another chat while the original turn completes without overwriting the selected history.
6. Chat creation, refreshed roster and automatic selection/history; repeated carrier bursts do not duplicate creation.
7. Reconnect invoking the supported companion resume path; fixture evidence still reports `model_roundtrip_proven=false`.

`tests/test_dispatch_client.py` exercises the real dispatcher/journal and real backend history/cancel adapters with synthetic upstream IO. Lua `client_test.lua` verifies stale-view rejection, atomic page assembly, background reply isolation, stop completion semantics and status; `ui_test.lua` exercises actual native widget callbacks, search, focus and minimize behavior, 4,095 chats with a six-row pool, short display bounds and hidden-panel continuation delivery. Synthetic backend and Lua fixtures cover 600 independent projects and 1,200 catalog rows. A send invalidates pending history views and queued history reads; late pages cannot overwrite the new message.

Run from `examples/wow`:

```sh
python3 -m unittest discover -s tests -v
python3 -m unittest discover -s transport -p 'test_*.py' -v
bash addon/tests/runall.sh
```

## Live validation and limits

A live check must establish installation of the current addon, texture capture and calibration, guarded keyboard input reaching the addon, authenticated request arrival at the backend, and visible incremental then final text in the in-game panel. Local fixture success does not establish those results.

Reconnect restores missing companion subscriptions and rechecks account access. Existing blocked durable clients remain blocked until their underlying condition is resolved and the companion is restarted. Carrier sessions are not reset automatically. Stop requires a turn receipt seen by this addon session; other-client active turns remain a companion workflow. Account connection status reflects the last explicit check, separate from carrier liveness and stream state.

This is a native Lua client plus external companion. WoW cannot host the browser client or its HTTP/WebSocket networking. Attachments, approval cards, realtime voice and complete mobile feature parity are not implemented by this integration.
