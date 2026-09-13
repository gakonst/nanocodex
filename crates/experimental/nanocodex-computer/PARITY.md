# Computer-use compatibility

This package targets the recovered September 6, 2026 CUA 0.2.4 / embedded Sky
0.6.26 interface. The implementation and retained behavioral oracles came from
the independent `sky-re-1000502/rebuild` research checkout. It does not implement
every original service or the complete Node/DOM environment. Passing the tests
below establishes the covered behavior, not complete product parity.

## September 13 integration audit

| Area | Implemented and checked | Remaining boundary |
| --- | --- | --- |
| Codex tools | Exact `js`/`js_reset` schemas and descriptions, canonical MCP names, persistent bindings, reset, ordered text/image content, original image detail and result metadata | Hosted transport deadlines still apply |
| Call options | Optional nulls, positive millisecond timeouts beyond 120 seconds, Node timers beyond 2³¹ ms | Adapter integers must be representable safely in JavaScript |
| Call identity | Current thread/call/model metadata reaches an existing kernel; omission restores launch metadata; authenticated routes and confirmation policies cannot be replaced by tool metadata | Host lifecycle events need integration in each embedding |
| Conversation cleanup | Separate processes, serialized input, cancellation, reset, active/queued release and a fresh scope after release | Rust attachments retain scopes until their owning executor is dropped |
| Native macOS | AppKit/AX input, UTF-16 text references, pointer hit testing, clipboard and capture providers; prior integration run verified Unicode input and JPEG output | Expanded live conformance was blocked by the locked Mac; full AX attributed-style/layout fidelity, keyboard layouts and multiple displays need more coverage |
| Native Linux | X11/XTEST typing, click, screenshot and reset through an owned live widget | X11 desktop control; no native Wayland backend |
| Headless Linux | Browser-only startup, navigation, form input, AX verification and screenshot without `DISPLAY` | Native input still requires a graphical session |
| Browser selection | Configured default, exact-origin preferences, native macOS/Linux URI association lookup and host visibility limits | No automatic personal-profile attachment; Windows OS association lookup is not implemented |
| Browser extension | Bundled MV3 assets, explicit export, independent native host, tab claims/groups, chunking, downloads, debugger control and service-worker reconnect | One active controller per bridge; automatic profile installation and concurrent conversation orchestration are not integrated |
| Browser APIs | Recovered tab/DOM/AX facades, locator/input/navigation, dialogs, downloads, raw waits, screencasts and WebMCP have contract/provider tests | Complete browser DOM object/prototype and selector/actionability behavior is not established |
| Embedded browser | Explicit CDP route authority, durable recovery and isolated host-turn kernels | Nanocodex has no attached native IAB shell/window-routing implementation yet |
| JavaScript environment | Persistent evaluation; covered timers, events, URL/URLSearchParams, Buffer/TextDecoder, StringDecoder, querystring, filesystem subset and CUA setup module | Full Node builtins, package/local module resolution, filesystem option matrix and formatting/prototype/stack fidelity remain incomplete |
| Host services | Configurable approval, download, origin, guardian, messaging and platform providers; authenticated lifecycle/recovery protocols | Original private brokers/account-persistent approvals are not reproduced; Nanocodex adapters do not yet forward MCP elicitation into an interactive host review UI |
| Windows/audio | Existing UIA, capture and audio providers plus synthetic protocol tests; optional audio switch forwarded | No live Windows validation or Windows release packaging; audio needs a live device run |
| Execution modes | CLI, native Hand/desktop and guest transports share the same tool contract; Docker exercises the actual guest protocol | No live hypervisor VM, hosted deployment or complete Apple application journey was run in this audit |

The audit fixed missing per-call metadata, dropped result metadata, divergent
descriptions/null handling, the 120-second adapter cap, Node timer overflow,
released queues reviving a scope, Linux opening X11 before any native operation,
missing distributable extension assets, and a native messaging reconnect bug.
The reconnect bug was reproduced in real Chrome: disconnecting the local port
does not dispatch that port's own `onDisconnect` event. Explicit local retirement
now rejects pending work and schedules reconnection exactly once.

## Evidence and repeatable checks

Run from the repository root:

```sh
pnpm test:computer
pnpm --filter nanocodex-computer typecheck
cargo clippy --locked --manifest-path crates/experimental/nanocodex-computer/runtime/Cargo.toml --all-targets -- -D warnings
```

The audit passed 176 runtime library tests, 13 server tests, 404 imported parity
tests, five Rust adapter tests and eight Node adapter/MCP tests. The parity target
also runs 27 Node extension tests. The two native/browser Node cases are opt-in;
they are not counted as passing when skipped. Tests cover actual Rust workers and
protocol boundaries with synthetic providers where an OS session is unavailable.
The imported corpus is self-contained; production and tests do not require the
research checkout or the original executable.

The owned live Chrome run passed navigation, form input, browser preferences, AX
verification and screenshot through the public Node adapter on macOS and on
headless Linux. The Linux run used a read-only, network-disabled container with
temporary browser state and no display server. The separate native Linux fixture
passed Unicode typing, click, screenshot and reset.

```sh
NANOCODEX_TEST_BROWSER=/absolute/path/to/chromium \
  node --test js/nanocodex-computer/test/browser.test.mjs

NANOCODEX_TEST_NATIVE_APP=/absolute/path/to/owned/Fixture.app \
  node --test js/nanocodex-computer/test/native.test.mjs
```

`NANOCODEX_TEST_COMPUTER` overrides the companion binary for these tests.
`NANOCODEX_TEST_BROWSER_NO_SANDBOX=1` is only for a separately isolated test
container that cannot launch Chromium's own sandbox. It does not change the
production launch configuration. A read-only container also needs a writable
temporary home for Chromium's crash reporter.

The live extension fixture requires Python `websocket-client` and an installed
Chromium supporting unpacked-extension debugging. It exports the extension from
the actual companion binary and compares every asset with its source. It creates
its own profile, native host registration and loopback pages, and writes an
evidence report into a new directory:

```sh
python3 crates/experimental/nanocodex-computer/runtime/tests/browser_extension_live.py \
  --chrome /absolute/path/to/chromium \
  --evidence /absolute/new/evidence-directory
```

Chrome 152 passed all ten live groups: loading, native messaging, native asset
operations, tab claims/debugger control, Unicode messages over 1 MiB, disconnect
cleanup, public download paths/bytes, worker restart, tab group/handoff behavior,
and authenticated conversation/subagent lifecycle rules. This checks the
independent extension, not the original vendor's private protocol.
