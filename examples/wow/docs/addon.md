# WoW addon

`addon/Nanocodex` contains the native Nanocodex side view. It browses projects and threads, loads paged history, creates chats, sends requests, renders streamed replies, and offers stop and companion reconnect controls. The external companion owns account authentication, networking and durable state. The addon does not embed a browser or open network sockets.

## Install

Run `bash scripts/install-addon.sh "/path/to/the/running/WoW/client"` from `examples/wow`. This copies all addon modules and the shared `addon/Transport.lua` into the target client's `Interface/AddOns/Nanocodex`, preserving a backup of an existing installation. All six TOCs load `Client.lua` after `Bridge.lua`. Enable the addon and `/reload` through the normal game UI.

The TOCs cover Mainline, Era, TBC, Mists and the previously targeted Classic Beta interface 16001. These declarations are compatibility targets. The new side view has **not** been visually verified in the running client; mocked Lua APIs do not prove actual rendering or protected API behavior.

## Controls

The panel initially anchors to the right edge and remains draggable. Its 460×700 layout scales down to fit the parent display, including when UI scale changes. The 114-pixel thread list reuses at most six row buttons while scrolling. Its saved position, visibility, minimized state and draft survive reload through account-wide SavedVariables. Escape or the close button hides it. Passive responses do not show it or acquire keyboard focus.

| Command/control | Action |
| --- | --- |
| `/nc`, `/nc toggle` | Toggle the side view |
| `/nc show`, `/nc hide` | Show or hide it |
| `/nc threads` | Open it and refresh the account roster |
| Refresh / search field | Refresh and filter all loaded chats, including locally closed chats |
| Chat row | Select chat and load recent history |
| Earlier | Load the previous 64 source events, prepending visible messages |
| New chat | Create in the selected project, or unassigned without a project |
| Ask / Enter | Send composer text to the selected chat; no selection starts a game conversation |
| Ask WoW / `/nc game <question>` | Send a separate game question with captured context |
| Stop | Request cancellation of a known turn started from this addon session |
| Reconnect | Restore missing companion subscriptions and check account connectivity |
| Projects / `/nc projects` | Open project/chat naming and selection controls |
| Answer / `/nc reply` | Expand the current answer view |
| `/nc capture` | Capture context in memory |
| `/nc reset` | Center and show the panel |
| `/nc clear` | Clear stored context in memory |
| `/nc settings` | Open the existing restricted settings workflow |
| `/nc bridge on`, `/nc bridge off` | Opt into/out of guarded automatic carrier startup |
| `/nc bridge-debug` | Show carrier diagnostics |

`/nanocodex` is an alternate slash prefix. No account credentials, browser tokens, shell commands or arbitrary model-generated Lua should be pasted into game chat.

Selecting a row while another outbound request is awaiting acknowledgement leaves the previous selection intact and reports busy. History requests carry a view identity; delayed pages for an old view are acknowledged but not displayed. Sending a message also invalidates pending history pages so that they cannot erase the new prompt. Page continuations and passive reply updates continue while the panel is hidden. Roster updates become visible only after every page has arrived and the complete snapshot validates. Streaming request identities map to backend thread/turn receipts, preventing background replies from replacing the selected chat.

**Stop** is limited to known send receipts in this addon session. It does not discover active turns started on another client. **Reconnect** is a companion account/stream action; it cannot recover a lost carrier session, resolve authentication, or restart an existing blocked durable stream. Use the companion's status and reconciliation workflow for those cases.

## Context and content limits

Context schema version 1 includes optional character/realm, location, target, specialization and quest fields captured through public WoW APIs. Unsupported API fields are omitted. `quests` remains an array; secret/restricted or unsupported values must not be invented. Context is included with an Ask request, subject to the existing 16 KiB serialized request limit.

Each carrier application message remains at most 16 KiB, with stream messages at most 4 KiB. History snapshots have at most 32 pages and 256 KiB total. The roster has at most 128 pages, 1 MiB and 4,096 combined project/chat rows (for example, 2,048 independent projects with one chat each). Larger catalogs produce an explicit capacity error and retain the previous complete roster. The display is bounded to 256 KiB; earlier-history accumulation beyond that limit produces an explicit message directing the user to the companion. Project snapshots use percent-encoded data; reply text escapes WoW markup pipes before display. No received payload is executable Lua.

The addon implements text thread workflows only. Attachments, approval cards, realtime voice and full mobile-client parity remain outside this integration. See [native-client validation](native-client-20260920.md) for reproducible evidence and the live gate.
