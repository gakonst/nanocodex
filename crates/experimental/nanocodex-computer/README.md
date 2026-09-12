# nanocodex-computer (experimental)

Persistent computer-use tools for Nanocodex. `cua_repl.js` evaluates JavaScript using
`cua` and `nodeRepl`; `cua_repl.js_reset` clears that conversation's bindings. Native
apps, browser tabs, screenshots, keyboard input and pointer input are provided
by the companion executable, `nanocodex-computer`.

This imports the independent Rust implementation from the Sky research rebuild.
It replaces the earlier macOS-only proposal in PR #102. The companion is a
separate process and Cargo workspace so its patched QuickJS engine does not
change Nanocodex's agent engine. See [runtime provenance](runtime/README.md).

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

macOS uses AppKit, Accessibility and ScreenCaptureKit, with the normal OS grants
and an unlocked graphical session. Linux uses X11/XTEST; native Hands bind the
companion to their private Xvfb display and Xauthority. A plain Wayland session
does not provide this X11 backend. Windows provider code is included in the
experimental runtime, but this integration's release targets are macOS and Linux.
Linux desktop hosts require `libpulse` and `libxkbcommon` (Debian/Ubuntu packages
`libpulse0` and `libxkbcommon0`); Hand installers and images include them.
The separate legacy Wayland screen-only service is not a CUA tool host.

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

Each conversation owns a process and persistent scope. Calls are serialized
within an attachment. Cancellation, timeouts or malformed protocol output stop
the affected process and require an explicit reset. Detaching drops the owned
processes. Attachments permit up to 32 retained conversations; Node hosts also
release scopes through the existing tool lifecycle hooks. Account credentials
are omitted from the companion's environment. The runtime's browser, app and
OS permission checks still apply.
Nanocodex adapters bound individual evaluations to 120 seconds; hosted transport
deadlines may end a call sooner. The standalone MCP server accepts a caller's
positive timeout. Both use a 30-second default.

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
