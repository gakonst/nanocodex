# Nanocodex for macOS

A SwiftUI and AppKit application for Apple Silicon Macs running macOS 14 or later.
This is the single native Mac app: its tiled workspace, agent activity menu bar,
and automatic background Hand share one app model and runtime. The
[`apple/`](../apple/README.md) app targets iPhone and iPad.
The interface uses native windows, menus, text editing, folder pickers, keyboard
shortcuts, and Keychain. It does not embed an Electron window or webview.

The managed-agent and Hand implementation comes from the explicit shared package
`js/desktop-runtime`. A private bundled Node executable hosts that package over a
small JSONL protocol. A built app needs no separate Node installation.

The local `apple/NanocodexUI` package owns the shared chat palette, Markdown
block rendering, and copy feedback. Both Apple apps render headings, lists,
links, quotes, tables, and code using Foundation’s Markdown parser. Copy
controls work on complete responses and individual code blocks. Desktop pane
arrangement, tiling, navigation shortcuts, and per-agent state remain owned by
the existing workspace.

## Build and open

Install workspace dependencies with `pnpm install` and prepare the pinned Node
binary once using [the bundled-runtime instructions](Resources/runtime/README.md).
Then, from the repository root:

```sh
pnpm build:macos
open macos/build/Build/Products/Release/Nanocodex.app
```

The Xcode project is `macos/Nanocodex.xcodeproj`, with the shared `Nanocodex`
scheme. Xcode copies the built shared helper, Node, its license, and the original
Nanocodex icon into the application. Bundle identity is
`xyz.paradigm.nanocodex.macos`; its display name, menu, icon, About panel, and
window title are **Nanocodex**. Local builds use ad hoc signing. Distribution
signing and notarization require the distributor's Apple Developer identity.

For development with the repository's `.env`:

```sh
pnpm --filter @nanocodex/desktop-runtime build
xcodebuild -project macos/Nanocodex.xcodeproj -scheme Nanocodex -configuration Debug -derivedDataPath macos/build build
open macos/build/Build/Products/Debug/Nanocodex.app
```

Debug builds find the repository `.env` automatically. Release builds accept an
explicit `NANOCODEX_ENV_FILE`, `NC_API_KEY`, or `NANOCODEX_API_KEY` at launch, or
phone sign-in. The first launch asks for a phone number and a six-digit SMS code;
it stores the resulting account securely in macOS Keychain. Settings offers
**Switch Account**, and **Advanced** in the sign-in form accepts an API key.
A successfully imported development account is
stored in macOS Keychain so later Finder/Dock launches reconnect automatically.
API keys are never stored in desktop preferences, displayed in the transcript,
or sent to native Hand subprocesses.

## Background Hands

The signed app installed in `/Applications` enables **Open Nanocodex at login**
on its first normal launch. macOS starts it after you sign into the computer;
the saved account, laptop Hand, and automatic screen sharing then reconnect.
Settings shows the actual login-item status and links to macOS Login Items when
approval is required. Explicit app or system opt-outs are preserved. Tests,
isolated development sessions, and builds outside `/Applications` never register
a login item.

**Hands → Remote Screens** opens the shared native WebRTC viewer. Desktop-enabled
factory VMs appear automatically once their publisher connects. The same screens
are available from the iPhone/iPad inbox and conversations. Mac screen sharing
starts automatically after sign-in when Screen Recording permission is available,
and remains owned by the app when its windows close. It remembers the selected
display and restores capture after system interruptions or display changes.
The Mac's screen identity is saved across reopening the picker and restarting
the app. Shell-only VM images do not publish a desktop; Cloudflare
sandbox desktops require the managed desktop feature flag. A disconnected viewer retries with the current publication generation,
retains the selected screen, and offers **Reconnect** after a 90-second recovery
window. Control must be acquired again after reconnecting.
Mac publishing also reconnects after temporary signaling outages while keeping
the selected display. **Stop sharing** disables automatic screen sharing across
relaunches. Re-enable **Share this Mac’s screen automatically** in Settings;
screen and control permission setup is available there and in Remote Screens.
Quitting ends sharing while preserving the preference for the next launch.

The Hands page, chat picker, and menu bar include devices connected elsewhere on
the same account. Previously observed devices remain listed as offline after
disconnecting. This requires the managed `/v1/account/hands` endpoint and its
account Worker route. Only this Mac's local Hands have start/stop controls.
iPhone availability remains limited by iOS background execution; retaining its
row does not wake the phone or keep its socket connected indefinitely.

The Hand item in the macOS menu bar shows **Nanocodex · N Hands**, adds the running
agent count when tasks are active, and stays
available after closing the window. Click it for the agent control panel: live
counts, open agents with running/queued/review status, and Hands with active call
counts and Connect/Stop controls. Running agents appear first; click an agent to
open it, or stop its current turn in place. The agent counts cover the workspace's
open agents. **Agent → Agent Control Panel** (`⌘⇧P`) opens the same panel.
**Open Nanocodex** or clicking the Dock icon restores the workspace; **Manage**
opens the full Hands page. Closing or minimizing the window keeps
the app and its Hands running. **Quit Nanocodex** (`⌘Q`) stops the local runtime
and its Hands.

**Make this Mac available as a Hand** in Settings and the control panel is on
by default. Turning it off, stopping the automatic device Hand, or removing it
persists an opt-out across reconnects, relaunches, and account switches. Starting
that Hand or turning the setting back on re-enables it. Ordinary window close
and app shutdown preserve the preference; private workspace grants and drafts
remain account-scoped.

**Keep Mac awake while Hands are running**, in the menu bar and Settings, is
on by default and saved on this Mac; an existing explicit choice is preserved. When enabled, a connecting or connected
Hand prevents idle system sleep even with no windows open. Stopping all Hands,
signing out, a runtime failure, or quitting releases the activity. The display
can still turn off. Closing the lid, choosing Sleep, or low battery can still
suspend the Mac; this option does not run Hands through forced sleep or after
quitting. Keeping the Mac awake uses more battery.

## Inbox, swiping, and optional panes

Start with one spacious conversation. Swipe horizontally through the open agents
in Inbox, Running, or All; each agent retains its draft and reading position.
The focused view uses AppKit's
[horizontal page transitions](https://developer.apple.com/documentation/appkit/nspagecontroller):
macOS owns gesture tracking, cancellation, edge resistance, and transition
snapshots. Sidebar, keyboard, and button selection switches immediately, without
queuing animations through other threads. An explicit cache retains up to eight
recent conversation views, including their native editors and viewports. Pages
finish layout before AppKit reveals or snapshots them.
Navigation never marks an update seen, stops a turn, or deletes history.
With a mouse, drag a pane's header left or right to switch. Short or vertical
drags stay put; text selection and the composer keep their normal mouse behavior.
Existing threads load inside the conversation surface without showing the new-chat
welcome screen. Failed loads show a pane-local retry state, preserve the draft,
and leave other agents usable. Tool and reasoning disclosures retain their expansion per pane.
The optimistic user message keeps its identity when accepted; final responses
update the streamed row in place. Titles retain the initial prompt until the
service supplies a title. Tiled content is rebuilt only when its arrangement or
width changes, and streaming preserves the reading position without per-token
scroll commands.

**Open beside** is the explicit entry into tiling. Choose an existing agent or
create a new one beside the current conversation. Two panes fit the window;
additional chosen panes extend horizontally. Opening a normal new tab or selecting
an agent in the sidebar replaces the focused conversation without adding panes.
In a tiled layout, selecting an already visible agent focuses its pane; selecting
another replaces the focused pane. Pane-specific composers and controls retain
their agent identity throughout.

- Remove a pane with its **×** to keep the agent in the sidebar. Removing the
  second pane returns to the single conversation. **Focus this agent** temporarily
  expands one pane; **Resume layout** restores the chosen arrangement.
- **Inbox / Running / All** returns to the single-agent review flow and preserves
  the chosen layout for explicit resumption. Inbox prioritizes completed and
  failed unseen updates on selection. Live output does not reorder conversations.
- **Seen** (checkmark / `⌘D`) records the current update and advances the Inbox.
  **Later** (clock / `⌘⇧D`) defers it without marking it seen. New events bring
  agents back. Inbox Zero retains agents and drafts in All.
- The active pane has an accent border and an **Active · Navigate / Writing**
  badge. **Escape** leaves the composer for navigation; repeated Escape stays
  there and preserves the draft. **Tab / Shift-Tab**, **Left / Right**, and
  **Up / Down** move between conversations or chosen panes. **Enter** returns
  to the active composer without sending. While writing, arrows edit text normally.
  Clicking a header also enters navigation; clicking a composer enters writing.
- `⌘⌥←` / `⌘⌥→` navigates to the adjacent conversation or chosen pane and
  preserves the keyboard mode. Navigation stops at either end. Swiping enters
  navigation when it changes the active agent.
  `⌘⌥⇧←` / `⌘⌥⇧→` reorders the focused pane; `⌘⇧F` focuses or resumes a layout.
- The tiled **Layout** menu offers Fit two panes, Compact, and Wide.
- `⌘\` opens a new agent immediately to the right and focuses its composer.
  `⌘⇧\` opens the existing-agent picker: type to filter, use Up/Down to choose,
  and Return to open it to the right. Escape dismisses the picker.
- `⌘T` / `⌘N` starts a conversation; `⌘W` closes a tab; `⌘⇧T` reopens it;
  `⌘K` searches durable threads; type to filter, use arrows to select, and Return
  to open the result. `⌘,` opens Settings. Sidebar/top tabs retain
  drag reordering and renaming, with **Open Beside** in their context menu.
- Drafts, Hands, folders, model controls, sending, steering, and history loading
  target their own agent. Order, drafts, review cursors, selected pane IDs, widths,
  and focused/tiled layout persist in the existing account-scoped preferences.
  Legacy layouts open in the single-agent view until tiling is explicitly chosen.

## Compute and conversation controls

- **Voice** (the waveform beside Send) opens the same native WebRTC conversation
  used by the iPhone app. It stays attached to the originating agent when you
  switch panes. A central spinner stays visible until the call is ready, then
  becomes a violet and copper contour orb. Both speakers' transcripts stream directly
  into chat; saved history replaces matching live speech without changing the
  composer. End voice with the dark X button. Closing its tab, changing accounts, and
  quitting stop capture. Microphone permission is requested only when starting.
- Return sends; Shift Return inserts a newline. While an agent is running,
  send queues a durable follow-up above the composer, as in iOS Inbox.
  **Steer now** stops that message's captured predecessor; it never sends the
  follow-up twice. Queue rows support cancellation and retain unconfirmed
  messages for retry with the same ID and exact payload. They survive navigation
  and relaunch in account-scoped storage. Stop / `⌘.` stops the current running
  turn without cancelling the waiting follow-ups. Cancelling a waiting message
  removes it quietly, including the service’s cancellation-draining events;
  it cannot move the viewport or clear a completed answer’s Inbox attention.
- New prompts sit at the top and replies grow downward. Streaming never jumps
  to the bottom; **Latest** is an explicit jump. Each turn retains its message
  identities even when another message is accepted during its response.
- Thinking, tools, progress commentary, and subagent updates coalesce into one
  compact **Activity** row per response. Open it for a bounded timeline, then
  open a step for its full details. Final answers and errors stay visible. Step
  counts and a live status update without growing the closed transcript; issue
  counts remain visible. Each pane retains its own disclosure choices.
- Model, effort, Pro reasoning, and fast mode are available in the
  composer. Model and Pro are fixed after the first accepted turn; effort and
  Fast mode can still change.
- Signing in automatically connects this Mac as an account-wide Hand and creates
  `~/Nanocodex` if needed. Relaunch reuses and reconnects the same Hand; temporary
  connection failures retry automatically. **Stop** disables the automatic Hand
  until explicitly re-enabled, including across relaunches.
  Choosing a
  folder for a tab and then sending automatically prepares a Hand scoped to that
  thread. Merely choosing a folder does not start compute.
- Native Hands run commands with the macOS user's permissions. Their processes
  use a filtered environment. Closing a window keeps Nanocodex and its Hands
  running; quitting disconnects the Hands and stops owned processes.
- VM Hands use the existing nanocodex2 VM lifecycle, with discovered defaults
  when available. Advanced controls select an existing VM image/runtime and
  CPU/memory/network settings. A Cloud Hand is created through the agent's real
  `mount` tool.
- Another Mac connects by opening Nanocodex with the same account.
  Advanced server instructions use the documented nanocodex2 VM
  command. Account connections, provider access, MCP, and SSH are managed through
  the existing account page.

## Verification

Current evidence (2026-09-06):

- **23 native protocol, policy, and rendering checks pass**, including two
  visible AppKit editors, the single-agent default, native page transitions,
  explicit pane selection, pane-specific Send routing, keyboard focus transfer,
  repeated Escape, Tab/Shift-Tab and arrow navigation, Enter to write, and typing
  immediately after a page switch,
  stable live order, Seen/Later, Inbox Zero and restoration, retained drafts,
  reordering, focus mode, and wide/narrow light/dark rendering. The existing
  transcript check preserves manual scroll position while output streams and
  reuses the actual viewport after a distant thread switch. Acceptance and final
  message checks verify stable row identity, text, and title continuity. Rapid
  selection checks verify the requested pane is visible on the next sampled frame.
  Failed thread loads stay within their pane, and a failed Hand connection stops
  the working indicator while retaining the message for retry or cancellation.
- **30 shared-runtime tests pass**, including automatic default Hand creation and
  reconnection, exact review cursor persistence,
  bounded pane widths, compatibility with legacy layouts, account isolation,
  managed event replay, and the native JSONL credential boundary.
- **The real native Hand journey passes both durable turns across restart**:
  the account-wide Hand connects automatically on launch and reconnects with
  the same identity after restart;
  the first turn writes and reads a file, the second reads it after reconnect,
  and the test explicitly stops its Hand and removes its own managed thread.
  This journey also queues a follow-up, stops its captured predecessor, and
  verifies a single durable acceptance and completion.
- Manual native dogfooding runs two real agents in separate panes, edits independent
  drafts, shares a temporary folder, writes and reads a real file, queues two
  follow-ups, cancels one, steers the other, and stops the scoped Hand. Service
  evidence is retained in `build/evidence/native-dogfood-live.json`.
  The final UI pass verifies keyboard search, mouse header dragging, focus/resume,
  pane reordering, Seen/Later, close/reopen, and retained reading positions and drafts.
- In the native preview, a real long conversation retained its reading position
  and expanded tool result after jumping to a distant thread and back.

Native screenshots are under `macos/build/evidence`, including
`native-inbox-default.png`, `native-inbox-tiles-light.png`, `native-inbox-tiles-narrow.png`,
`native-inbox-focus.png`, and `native-inbox-zero.png`. The hosted UI fixtures do
not contact a service. The real service journey requires the development `.env`
and uses isolated preferences and its own managed thread.

The isolated native rendering benchmark captures before/after chat, Hands,
Settings, and narrow-window screenshots, plus editor and tab timings in
`native-performance-before.json` and `native-performance-after.json`. It measures
actual AppKit editing and SwiftUI layout in a Debug test host, not process launch
or network latency. The thread-continuity changes reduced median switch work
from 35.8 ms to 15.0 ms, with p95 moving from 38.0 ms to 23.2 ms, in the local
before/after runs (`native-thread-continuity-before.json` and
`native-thread-continuity-after.json`). Typed event decoding and unchanged-event replay suppression
cut the 800-event JSONL snapshot benchmark from 111 ms to about 47 ms. Draft
serialization is deferred until the save debounce expires. Streaming preserves
the reading position; a **Latest** button jumps down explicitly. Queued messages
are persisted before network submission, independently of the draft debounce.

The September 7 performance audit records fresh-process fixture results in
`native-performance-perf-audit-before.json` and
`native-performance-perf-audit-after-isolated.json`. Across 80 completed turns,
live snapshot processing fell from 115.5 ms to 45.6 ms median (118.7 ms to
47.5 ms p95); unchanged 800-event snapshots fell from 51.5 ms to 19.5 ms.
JSONL framing and decoding now run on a serial background queue with ordered
main-actor delivery. Unchanged account snapshots do not republish UI state, and
unchanged turns retain their projected messages. Corrected events, replay,
history prepends and account resets retain the canonical reducer's behavior.
All 33 native protocol tests pass, including main-queue responsiveness during
large fragmented-frame decoding, final-response delivery before child exit,
cursor-only snapshot updates, and the existing AppKit editor, tab, and
retained-scroll journeys. The isolated test bundle and mocked runtime leave
running user sessions and account credentials untouched. These measurements
cover local native processing and layout, not service latency or end-to-end
process startup.

Earlier messages load automatically when a user scrolls within 240 points of
the transcript's top. Initial layout and streamed output never fetch pages.
Only one page loads at a time, and native message geometry preserves the visible
row across prepends even when the current response grows below it. The hosted
AppKit history journey checks short pages, scrolling during a pending request,
frame-by-frame position retention, exhaustion, and retry after leaving and
returning to the boundary; `native-automatic-history.png` records that viewport.

Generated code-mode output is rendered inline outside Activity, using the shared
`NanocodexUI` media renderer. Both raw and structured tool results are projected:
emitted text, images, audio, video, and file links remain visible, while embedded
binary data is removed from diagnostics. Stable content identities deduplicate
nested tool results and their outer exec copies. Parsed output is retained per
result while the same turn streams; the native fixture measured 0.11 ms median
with retained output versus 4.34 ms when reparsing it on each of 24 snapshots.
The protocol and hosted-window fixtures exercise the actual JSON-string
`input_text`/`input_image` shape plus MCP images and structured file links;
`native-generated-code-output.png` and `native-generated-output-performance.json`
record the visible result and local projection measurements without network use.

```sh
xcodebuild -project macos/Nanocodex.xcodeproj -scheme Nanocodex -configuration Debug -destination 'platform=macOS,arch=arm64' -derivedDataPath macos/build -only-testing:NanocodexTests -parallel-testing-enabled NO test
```

`ProtocolTests` separately cover durable event replay, tool-result projection,
Astra settings, and compatibility with the shared state/tab contract. To run only those tests,
use `-only-testing:NanocodexTests/ProtocolTests`.

`VoiceTests` covers conversation ownership and saved voice transcript projection.
Its opt-in native speech journey feeds recorded speech through an installed
BlackHole 2ch input into the actual Swift/WebRTC session and managed OpenAI backend.
It interrupts spoken counting, asks a replacement question and a follow-up, and
stops during output. It restores the original input device and deletes its agent.
Prepare its synthetic clips and run it explicitly:

```sh
say -v Samantha -r 175 -o /tmp/nanocodex-voice-count.wav --file-format=WAVE --data-format=LEI16@24000 'Please count slowly from one to thirty, with a short pause between each number.'
say -v Daniel -r 185 -o /tmp/nanocodex-native-voice-interrupt.wav --file-format=WAVE --data-format=LEI16@24000 'Stop counting. What is six plus seven? Just say the answer.'
say -v Samantha -r 185 -o /tmp/nanocodex-native-voice-followup.wav --file-format=WAVE --data-format=LEI16@24000 'What color is a clear daytime sky? Just say the color.'
TEST_RUNNER_NANOCODEX_DESKTOP_VOICE_LIVE=1 TEST_RUNNER_NANOCODEX_VOICE_SOX="$(command -v sox)" xcodebuild -project macos/Nanocodex.xcodeproj -scheme Nanocodex -configuration Debug -destination 'platform=macOS,arch=arm64' -derivedDataPath macos/build -only-testing:NanocodexTests/VoiceTests -parallel-testing-enabled NO test
```

For a signed-in subscription account, set
`TEST_RUNNER_NANOCODEX_DESKTOP_VOICE_KEYCHAIN=1` to use the app's existing
Keychain credential instead of the development environment. Sign the test host
with the same development identity as the installed app to preserve its
Keychain access; isolated test preferences remain separate.

`NANOCODEX_VOICE_TIMING=1` enables timestamped stage and transport diagnostics
in Debug or Release. The bounded log is `Documents/voice-timing.log` in the app
container, or the explicit `NANOCODEX_VOICE_TIMING_LOG` path. It records HTTP
status/timing, transcript event types, delegation, model-event delivery, playback
enablement and audio energy; it does not record speech, credentials, or SDP.
The speech test retains milestone and audio-state evidence even when a call fails.

`testNativeGreetingAndPersonalMemory` separately checks a brief greeting followed
by an unknown personal fact. Enable it with
`TEST_RUNNER_NANOCODEX_DESKTOP_MEMORY_VOICE_LIVE=1`, the same account/SoX flags,
and an explicit timing-log path. Prepare the fixed synthetic fixtures first:

```sh
say -v Samantha -r 175 -o /tmp/nanocodex-native-voice-greeting.wav --file-format=WAVE --data-format=LEI16@24000 'Hi, say hello briefly.'
say -v Samantha -r 175 -o /tmp/nanocodex-native-voice-personal.wav --file-format=WAVE --data-format=LEI16@24000 "What secret passphrase did I choose for the fictional Project Cedar Comet? Check my stored memory. If you cannot find it, tell me you don't know."
```

This journey requires a new managed delegation for the personal question and an
explicit unknown answer. It writes `native-memory-voice-live.json`, including
voice settings and timestamps, restores the original audio input, and removes
its test agent. Run acoustic phone tests separately to avoid mixing their input
with the Mac's audible output.

Before deleting a failed memory-test agent, the fixture saves
`native-memory-voice-failure-state.json`: durable cursors, event types/timestamps,
active turn IDs, and turn status. It excludes message/tool payloads and credentials.
To inspect a retained owned test agent without starting voice or sending a turn,
run `testNativeOwnedAgentDiagnostics` with
`TEST_RUNNER_NANOCODEX_DIAGNOSTIC_AGENT_TITLE` set to its exact unique title (or
`TEST_RUNNER_NANOCODEX_DIAGNOSTIC_AGENT_ID` for a known owned test-agent ID). This
read-only test uses the existing Keychain account and saves the same metadata file.

`testNativeApplicationSceneHasContent` with
`TEST_RUNNER_NANOCODEX_DESKTOP_WINDOW_LIVE=1` verifies that the actual hosted
app scene creates a main window with content. It is independent of external
accessibility automation and does not prove visibility on the user's current desktop.

Voice evidence is saved in `macos/build/evidence/native-voice-live.json` and
`native-voice-live.png`. These timings include the real provider and local audio
device; a cold connection still takes seconds. They do not measure an iPhone's
physical microphone, Bluetooth route, or cellular network.

`NanocodexUITests` additionally uses macOS UI automation for keyboard/menu and
navigation checks. It requires a Mac with Xcode UI automation enabled; the
current development machine rejected that runner with **“Timed out while
enabling automation mode.”** Hosted native tests do not require changing that
machine-wide setting.

Use `NANOCODEX_DESKTOP_DATA` for isolated development sessions. Such sessions
never read, write, or delete the normal account's Keychain entry. Normal
preferences live in `~/Library/Application Support/Nanocodex/Native`, and are
scoped to the connected account.
