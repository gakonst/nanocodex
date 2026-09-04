# Nanocodex for macOS

A SwiftUI and AppKit application for Apple Silicon Macs running macOS 14 or later.
The interface uses native windows, menus, text editing, folder pickers, keyboard
shortcuts, and Keychain. It does not embed an Electron window or webview.

The managed-agent and Hand implementation comes from the explicit shared package
`js/desktop-runtime`. A private bundled Node executable hosts that package over a
small JSONL protocol. A built app needs no separate Node installation.

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

## Working with tabs and compute

- Tabs can be in the sidebar or across the top. They retain drafts and their
  chosen compute, can be dragged to reorder, renamed, closed, and reopened.
  History search opens existing durable threads.
- `⌘T` / `⌘N` opens a tab; `⌘W` closes it; `⌘⇧T` reopens the last closed tab;
  `⌘K` searches; `⌘,` opens Settings; `⌘⇧[` / `⌘⇧]` switches tabs.
- Return sends; Shift Return inserts a newline. While an agent is running,
  send queues a follow-up, **Steer** updates the current turn, and Stop / `⌘.`
  cancels it. Model, effort, Pro reasoning, and fast mode are available in the
  composer. Model and Pro are fixed after the first accepted turn; effort and
  Fast mode can still change.
- **Enable this Mac** creates the default workspace automatically. Choosing a
  folder for a tab and then sending automatically prepares a Hand scoped to that
  thread. Merely choosing a folder does not start compute.
- Native Hands run commands with the macOS user's permissions. Their processes
  use a filtered environment. Closing a window keeps Nanocodex and its Hands
  running; quitting disconnects the Hands and stops owned processes.
- VM Hands use the existing nanocodex2 VM lifecycle, with discovered defaults
  when available. Advanced controls select an existing VM image/runtime and
  CPU/memory/network settings. A Cloud Hand is created through the agent's real
  `mount` tool.
- Another Mac connects by opening Nanocodex with the same account and enabling
  its Hand. Advanced server instructions use the documented nanocodex2 VM
  command. Account connections, provider access, MCP, and SSH are managed through
  the existing account page.

## Verification

Current evidence (2026-09-04):

- **Thirteen native protocol and rendering tests pass:** durable event replay, tool-result projection,
  separate subagent streams, the shared state/tab contract, and Astra settings
  compatibility, including supported reasoning efforts and unchanged defaults;
  SMS challenge timing, sign-in commit/retry, account-switch cancellation, model
  locking, typed JSONL frame decoding, native screen rendering, and preserving
  manual scroll position during streaming. SMS tests use isolated
  protocol responses; no production SMS was sent.
- The hosted native test verified the actual `NSTextView` editor, Return and
  Shift Return, draft switching, top tabs, an automatically scoped folder Hand,
  and a real first-turn file write/read. Restarting the helper restored the tab
  layout and durable conversation history.
- **The complete two-turn Hand journey is not yet passing.** The second turn
  after reconnect reaches the managed API, but its tool namespace retains the
  retired execution attachment. The API attachment-cache fix has not yet been
  deployed to the tested endpoint. This is a remaining service integration
  blocker, not a successful end-to-end result.

The hosted service test includes the second file read and explicit Hand stop so
that it can verify the complete journey after that API fix. It requires the
development `.env`, uses isolated preferences, and removes its own managed
thread. Native view PNGs are under `macos/build/evidence`; full-journey timing
metrics are written only when the whole journey succeeds.

The isolated native rendering benchmark captures before/after chat, Hands,
Settings, and narrow-window screenshots, plus editor and tab timings in
`native-performance-before.json` and `native-performance-after.json`. It measures
actual AppKit editing and SwiftUI layout in a Debug test host, not process launch
or network latency. Typed event decoding and unchanged-event replay suppression
cut the 800-event JSONL snapshot benchmark from 111 ms to about 47 ms. Draft
serialization is deferred until the save debounce expires. Streaming follows
growing messages at a bounded cadence, pauses during manual scrolling, and offers
a **Latest** button to resume.

```sh
xcodebuild -project macos/Nanocodex.xcodeproj -scheme Nanocodex -configuration Debug -destination 'platform=macOS,arch=arm64' -derivedDataPath macos/build -only-testing:NanocodexTests -parallel-testing-enabled NO test
```

`ProtocolTests` separately cover durable event replay, tool-result projection,
Astra settings, and compatibility with the shared state/tab contract. To run only those tests,
use `-only-testing:NanocodexTests/ProtocolTests`.

`NanocodexUITests` additionally uses macOS UI automation for keyboard/menu and
navigation checks. It requires a Mac with Xcode UI automation enabled; the
current development machine rejected that runner with **“Timed out while
enabling automation mode.”** Hosted native tests do not require changing that
machine-wide setting.

Use `NANOCODEX_DESKTOP_DATA` for isolated development sessions. Such sessions
never read, write, or delete the normal account's Keychain entry. Normal
preferences live in `~/Library/Application Support/Nanocodex/Native`, and are
scoped to the connected account.
