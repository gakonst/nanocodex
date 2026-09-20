# Nanocodex WoW

A native WoW side panel backed by the existing desktop companion. Browse account threads, select a conversation, load its history, create a chat, send a message and read incremental replies without leaving the addon panel. The companion owns authentication and the connection to Nanocodex. No account key belongs in Lua, SavedVariables, or game chat.

The native client and bridge are covered by local integration tests. These tests do **not** establish delivery or rendering in a running WoW client. See [current validation](docs/native-client-20260920.md) for evidence and the live access gate, and [earlier bridge validation](docs/bridge-integration-20260919.md) for the previous baseline.

## Native side view

`/nc show` opens the movable right-side panel. `/nc threads` also requests a fresh account roster. Use **Refresh** to retrieve the bounded account roster of projects and open and locally closed chats, then search by chat or project name. Selecting a row loads history; **Earlier** loads an older batch. The existing **Projects** window still provides project selection and naming controls.

- **New chat** creates a real Luna conversation in the selected project, or an unassigned conversation if no project is selected. The confirmed result refreshes the roster and selects the new chat.
- **Ask** sends to the selected chat. Existing conversations retain their model configuration. With no selection, it starts a separate game conversation. **Ask WoW** explicitly starts a separate game conversation with captured game context.
- **Stop** targets a turn whose send receipt was received in this addon session. A stop receipt means cancellation was requested; completion remains separate. Turns started elsewhere must currently be managed in the companion.
- **Reconnect** asks the companion to restore missing durable stream subscriptions and recheck account connectivity. It does not reset carrier sessions, sign in, or unblock a failed stream client. Those cases require the companion and its existing reconciliation workflow.

History and live text stay associated with the selected conversation. Background completions do not replace another chat's history. Passive replies do not open the panel or take keyboard focus. Minimize and hide preserve the composer draft.

The account roster is bounded to 4,096 combined project/chat rows, 1 MiB and 128 pages. History loads 64 source events per batch and supports earlier batches; a retained snapshot is limited to 256 KiB. Snapshots travel in pages no larger than the existing 16 KiB application-message limit, using the existing carrier chunking. Oversized data produces an explicit error, never a silent claim that all content was loaded. Attachments, approval cards and realtime voice are not implemented in the addon.

## Install on the gaming desktop

Requires Python 3.10+, Chromium for the companion, and the Nanocodex CLI. From `examples/wow`, run as the **gaming desktop user**:

```sh
bash scripts/install-all.sh "/path/to/the/running/WoW/client"
```

When prompted, enter `/reload` in WoW, then return to the installer and press Enter. This updates the companion, addon and automatic connection service together. Sign in through the official CLI as that same desktop user:

```sh
/opt/nanocodex/current/nanocodex2 login
~/.local/bin/nanocodex-wow
```

The companion binds to loopback at `http://127.0.0.1:17840`. Desktop Alt+Shift+N/P/V opens the companion, project selection or dictation; installation preserves occupied shortcuts. Voice remains in the companion; see [voice setup](docs/voice.md).

The addon starts its guarded chord receiver after loading. `/nc bridge off` saves an opt-out; `/nc bridge on` enables automatic startup. The existing bridge waits when WoW is unfocused, its carrier is obscured, an edit box has focus, or input guards fail. Ambiguous delivery stops with its journal retained. Session/window geometry changes require reconciliation; seamless carrier reconnection across `/reload` is not implemented. Do not run old and new companion daemons concurrently during an update.

## Architecture and boundaries

WoW addons cannot run an HTTP/WebSocket client or embed the Nanocodex browser app. This is a native Lua client using the existing public pixel/keyboard carrier. The external Python companion performs authenticated account operations and durable streaming. Carrier ACK, backend admission, account connectivity and model completion are separate states.

The bridge does not read game memory or automate combat. Its guarded carrier preserves gameplay bindings. User questions may include explicitly captured character, target, location and quest context. Project names and selected IDs use account-wide SavedVariables; these are written by WoW on logout or UI reload. Conversation history is retained in addon memory for display.

Settings changes use the existing limited preview/apply/undo planner in the companion and addon. They do not execute arbitrary Lua from model responses. Account organization names/grouping/closed state remain companion-local; they do not sync to mobile. Messages and account conversations use the real Nanocodex backend.

## Development

Run from `examples/wow`, with Python, Lua 5.1+ and the existing carrier image dependencies available:

```sh
python3 -m unittest discover -s tests -v
python3 -m unittest discover -s transport -p 'test_*.py' -v
bash addon/tests/runall.sh
```

The Lua/Python integration fixture traverses the actual addon request encoder, client state, carrier, assembler, bridge journals and dispatcher with mocked game/desktop APIs and synthetic backend responses. [Addon controls](docs/addon.md) and [protocol/validation details](docs/native-client-20260920.md) describe the limits. Local tests do not prove live account connectivity, in-game compatibility or microphone quality.
