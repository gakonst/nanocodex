# nanocodex-computer (experimental)

Persistent computer-use tools for Nanocodex. `cua_repl.js` evaluates JavaScript using
`cua` and `nodeRepl`; `cua_repl.js_reset` clears that conversation's bindings. Native
apps, browser tabs, screenshots, keyboard input and pointer input are provided
by the companion executable, `nanocodex-computer`.

This imports the independent Rust implementation from the Sky research rebuild.
It replaces the earlier macOS-only proposal in PR #102. The companion is a
separate process and Cargo workspace and uses the published `rquickjs` crate.
See [runtime provenance](runtime/README.md).

The Rust client is included in the 0.6 registry release with an experimental
label. Its API and companion protocol may change; distribute a matching
companion when embedding it. The native runtime remains a separate build.

## Install and run

From the repository root:

```sh
pnpm install:computer
nanocodex-computer permissions
nanocodex2
```

Release installers place the companion beside both CLIs. The macOS app bundles
it with its Node runtime. VM, Docker and Linux server Hand builds include it.
Older installed Hands need rebuilding or updating before they advertise CUA.

The CLIs and native desktop attachments discover a sibling executable or one
on `PATH`. `NANOCODEX_COMPUTER=/absolute/path` chooses a specific binary;
`NANOCODEX_COMPUTER=off` disables local registration. The VM transport advertises
the same tools and executes them inside the selected guest.
The first guest CUA call starts the existing private X11 desktop when no screen
publisher has started it. CLI-only sessions own and stop that desktop with the
guest; an existing published desktop is reused.

| Mode | Execution |
| --- | --- |
| Local `nanocodex` / `nanocodex2` | Companion on the local computer |
| Native Hand / desktop app | Companion on the attached computer |
| VM / Docker Hand | Typed guest tool request; companion inside that guest |
| Hosted agent | Same CUA contract, bound to a selected attached Hand |

Hosted calls select a sole available computer automatically. With multiple
computers, call `select_computer({workdir:"/mounted-hand"})` first. Selection
retains the exact attachment for the conversation; switching requires another
explicit selection. The CUA argument schemas never acquire routing fields.
`/brain` has no desktop. A disconnected attachment cannot silently
redirect an admitted call to a replacement machine.

Hosted agents also expose `computer` for live screen control across macOS,
Windows, Wayland, phones, and VM desktops. Use
`computer({workdir:"/omarchy-desktop", action:"observe"})`, or select the Hand
first and omit `workdir`. Selection returns the available tool names. A screen
publisher can provide `computer` without the native CUA companion; use its
screenshots and normalized coordinates for click, type, key, scroll, and drag.
The native `cua_repl` contract still requires the companion. Screen routes retain
their publication generation, respect human control, and never replay input on
reconnect. Reselect after reconnecting a screen.

Native CUA full-display capture requires host authorization for native control
and is unavailable under configured app or browser-origin restrictions. Use a
permitted app or browser target in restricted sessions.

macOS uses AppKit, Accessibility and ScreenCaptureKit, with the normal OS grants
and an unlocked graphical session. Linux uses X11/XTEST; native Hands bind the
companion to their private Xvfb display and Xauthority. A plain Wayland session
does not provide this X11 backend. Windows provider code is included in the
experimental runtime, but this integration's release targets are macOS and Linux.
Linux desktop hosts require `libpulse` and `libxkbcommon` (Debian/Ubuntu packages
`libpulse0` and `libxkbcommon0`); Hand installers and images include them.
Browser-only Linux sessions do not require `DISPLAY`; the X11 connection opens
on the first native desktop operation.
The separate Wayland screen service provides the hosted `computer` interface;
it does not provide the native `cua_repl` runtime.

| Surface | Platforms | Operations |
| --- | --- | --- |
| Native apps | macOS, Linux/X11 | discover apps, accessibility state, screenshots, click, drag, scroll, key input, text entry/paste, selection and secondary AX actions |
| Browser tabs | Any host with a configured Chromium CDP or bundled extension bridge | tab discovery/creation/navigation, DOM and accessibility state, screenshots, input, downloads, history, CDP and advertised WebMCP capabilities |
| Low-level desktop | Linux/X11 today; Windows provider implementation is experimental | screen capture, pointer movement/click/drag, scrolling and keyboard/text input |
| Optional audio | Platform provider dependent | start, stop and return captured audio through the same multimodal result path |

## Tool contract

The companion is an MCP stdio server exposing `js` and `js_reset`. Registering
it as `cua_repl` in Codex produces the canonical function names
`mcp__cua_repl__js` and `mcp__cua_repl__js_reset`, which the Rust and Node adapters
also expose directly. `js` accepts `code`, optional `title` and `timeout_ms`;
`js_reset` accepts an empty object. The persistent `cua` and `nodeRepl` bindings,
MCP image blocks and `codex/imageDetail` metadata follow the recovered contract.

```toml
[mcp_servers.cua_repl]
command = "/absolute/path/to/nanocodex-computer"
args = ["--allow-native-control", "serve"]
```

This configuration authorizes native control subject to OS grants and any
configured app/origin policy. Optional browser endpoints belong in host args.

```js
// First call; inspect the returned API documentation and state.
let app = await cua.getApp("TextEdit");
// A subsequent call can reuse app.
await app.getAXState();
await nodeRepl.emitImage(await app.getScreenshot({ emit: false }));

// macOS: capture the main display without opening or activating an app.
await cua.getScreenshot();
```

On Linux use `cua.computer`, whose returned documentation describes desktop
coordinates, `click`, `type_text`, `press_key`, `scroll` and `get_screenshot`.
For browser DOM/AX APIs, supply an explicitly owned Chromium CDP endpoint:

```sh
export NANOCODEX_COMPUTER_CDP='chrome=ws://127.0.0.1:9222/devtools/browser/…'
```

Then `await cua.getBrowser({id:"chrome"})` selects it. Personal browser profiles
are not attached automatically. Trusted embeddings can pass multiple `--cdp`
arguments in `ComputerConfig.args`. `NANOCODEX_COMPUTER_SECURITY_CONFIG` supplies
the runtime's app/origin policy file. These are host settings, never tool inputs.

`NANOCODEX_COMPUTER_BROWSER_PREFERENCES` selects a JSON file with an optional
default and exact HTTP(S) origin rules. Every value must name a configured browser:

```json
{
  "defaultBrowser": "chrome",
  "origins": { "https://work.example": "edge" }
}
```

`cua.getBrowser({url})` first uses an exact origin rule, then the configured
default. Without an explicit preference it consults the OS URI handler on macOS
and Linux (when GIO is available), and falls back to a configured browser.
This lookup never opens a browser or expands a conversation's visible providers.
An explicitly preferred browser outside the active host route returns an error.

Trusted embeddings can also set `NANOCODEX_COMPUTER_IAB_CONFIG`,
`NANOCODEX_COMPUTER_RUNTIME_CONFIG` and `NANOCODEX_COMPUTER_PLATFORM_CONFIG` to
forward the corresponding runtime JSON files. They configure existing providers;
they do not create an embedded browser shell. The optional audio surface retains
the `SKY_ENABLE_AUDIO` environment switch. See the
[compatibility record](PARITY.md) for implemented and remaining behavior.

## Browser extension

The companion embeds an independent Chrome MV3 extension. Export it into a new
directory, load that directory explicitly in your chosen Chromium profile, and
use its extension ID to configure native messaging:

```sh
nanocodex-computer extension-export --destination /absolute/new/extension
nanocodex-computer extension-bridge --socket /absolute/private/bridge.sock
# In another terminal, with the bridge still running:
nanocodex-computer extension-manifest \
  --destination /absolute/private/NativeMessagingHosts \
  --socket /absolute/private/bridge.sock \
  --extension-id YOUR_32_CHARACTER_EXTENSION_ID
```

The socket parent and manifest directory must be private to your user. Register
the generated `org.nanocodex.computer.json` in that browser's native-messaging
host location. The bridge prints its capability-bearing `endpoint`; pass that
whole value as `NANOCODEX_COMPUTER_CDP=chrome=<endpoint>` in the host environment.
Exports and manifests refuse to overwrite existing files. No profile or extension
installation is modified automatically.

The bridge supports tab claims, native tab groups, screenshots, input, downloads,
chunked messages and reconnecting service workers. A bridge has one active
controller; sharing it between concurrent conversation processes still requires
host lifecycle integration. See [PARITY.md](PARITY.md).

The tools use ordinary function calls and multimodal function outputs, following
the [OpenAI code-execution computer-use integration](https://developers.openai.com/api/docs/guides/tools-computer-use).
Text stays text; screenshot bytes become `input_image` with `detail: "original"`.
The built-in Responses `computer_call` wire type is not required. Existing
Nanocodex Codex/OAuth and Responses transports consume the same tool contract.

## Rust API

```rust,no_run
use nanocodex_computer::{ComputerConfig, ComputerTools};
let computer = ComputerTools::local(ComputerConfig::new("/usr/local/bin/nanocodex-computer"));
// Register computer.js() and computer.reset() in your existing Tools builder.
```

`ComputerExecutor` supports attachment-specific transports; `VmTools` implements
that path without exposing a guest executable or command string to the model.
The Node attachment adapter is [js/nanocodex-computer](../../../js/nanocodex-computer).

Each conversation owns a process and persistent scope. Calls remain ordered
inside that scope because a single QuickJS realm is stateful; independent
conversations are actors with separate processes and execute concurrently. No
attachment-wide mutex or promise chain serializes them. Cancellation, timeouts
or malformed protocol output stop only the affected process and require an
explicit reset. Detaching drops the owned processes. Node hosts release scopes
through the existing tool lifecycle hooks, cancelling both active and queued
calls without reviving the released scope. The adapters do not impose arbitrary
session, source, result or hosted-frame caps; caller-provided deadlines and
output budgets remain effective. Account credentials are omitted from the
companion's environment. The runtime's browser, app and OS permission checks
still apply.
The adapters accept positive safe-integer millisecond timeouts, including calls
longer than 120 seconds. Omission or `null` uses a 30-second default; hosted
transport deadlines can end a call sooner. Per-call Codex metadata reaches the
running REPL, and MCP result metadata survives both adapters. Tool metadata cannot
replace trusted confirmation policies or an authenticated host's route and turn.

## TypeScript API

The package publishes the runtime globals and the generated browser manifest as
types without introducing a second JavaScript implementation:

```ts
import type { Cua, CuaGlobals, Target, Tab } from "nanocodex-computer/api";
import type { Browsers } from "nanocodex-computer/browser-api";
```

`Cua` describes discovery and selection, `Target` covers the shared app/tab
observation and input surface, and the generated browser declarations cover
provider-specific tab capabilities. `Screenshot`/`Uint8Array` results remain
binary until the adapter converts them to an MCP image block.

## Validation

```sh
pnpm test:computer
cargo check -p nanocodex-vm --no-default-features --features guest-runtime --target aarch64-unknown-linux-musl
docker build -t nanocodex-computer:integration crates/experimental/nanocodex-computer/runtime
docker build -t nanocodex-computer:live crates/experimental/nanocodex-computer/tests/linux
docker run --rm --network none nanocodex-computer:live
```

The root build/test commands use the Turbo graph. Native tasks use Cargo's own
build cache instead of sharing executable artifacts across OS/architecture
through Turbo; JS tests depend on the runtime build and Rust tests.

The live Linux test creates its own X server/window, verifies native Unicode
typing and a button click through independent widget state, captures a screenshot,
and verifies reset. The optional Node native test uses the owned AppKit fixture
selected by `NANOCODEX_TEST_NATIVE_APP` and requires an unlocked Mac.
The default suite includes the imported compatibility corpus. For browser and
extension live checks, see [PARITY.md](PARITY.md).
