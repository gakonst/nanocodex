# Nanocodex Browser

Deterministic browser automation exposed as a normal Nanocodex Code Mode tool,
with an optional isolated headed-browser VM lifecycle.

`nanocodex-browser` is an experimental, unpublished, library-first crate. It
owns the browser action protocol, Chromium DevTools controller, diagnostics,
artifacts, and ordinary `BrowserTool`. Its named `vm` module composes those
browser concerns with `nanocodex-vm`; it does not own a second VM runtime.

The primary consumer is a Nanocodex agent. `BrowserTool` is caller-owned and is
not installed by default:

```rust,ignore
use nanocodex::{Nanocodex, OpenAi, Tools};
use nanocodex_browser::BrowserTool;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let browser = BrowserTool::new()?;
let tools = Tools::builder().provider(browser).build()?;
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(openai).tools(tools).build()?;

let result = agent
    .prompt("Open https://example.com, inspect it, and report the main heading.")
    .await?
    .result()
    .await?;
println!("{}", result.final_message());
# Ok(())
# }
```

The model reaches the tool as `await tools.browser(...)` inside Code Mode.
`ALL_TOOLS` discovers the deferred tool without expanding its schema; callers
can inspect the exact input and output contracts on demand with
`toolSchema("browser")`. Provider registration therefore keeps the full contract
out of the model-facing tool prefix. A runnable version lives at
`examples/browser_agent.rs`. Callers that deliberately want the eager schema
can register the same value with `.tool(browser)`.

`BrowserTool::new()`, `BrowserTool::with_executable(...)`, and the Nanocodex CLI
install an isolated virtual platform authenticator by default, so passkey
registration and sign-in work unattended inside the private browser session.
Low-level `Browser::builder()` consumers retain explicit control through
`.virtual_authenticator(...)`. On macOS, callers may instead configure
`.host_passkey_authenticator(...)` for explicit user-approved access to the
system passkey UI.

For isolation, one non-cloneable VM owner keeps the disposable browser alive
and gives the agent a tool handle:

```no_run
use nanocodex_browser::{BrowserAction, vm::BrowserVm};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let browser = BrowserVm::builder(
    ".cache/browser/rootfs.ext4",
    "target/debug/nanocodex",
    ".cache/bin/gvproxy",
)
.vmm_args(["vm-run-config", "--config"])
.spawn()
.await?;

browser
    .browser()
    .execute(BrowserAction::Open {
        url: "https://example.com/".to_owned(),
    })
    .await?;

let tool = browser.tool();
// Pass `tool` to `Tools::builder().provider(tool)`.
drop(tool);
browser.shutdown().await?;
# Ok(())
# }
```

Every VM spawn reflinks an immutable ext4 template, runs Chromium as an
unprivileged guest user under Xvfb, and exposes CDP only through a random host
loopback port. The owner shuts down the DevTools controller, Chromium, gvproxy,
the VMM, and the disposable disk together. The image definition and guest init
script live in `image/`.

For trusted local development, `Browser` provides the same typed actions
without a VM:

```no_run
use nanocodex_browser::{Browser, BrowserAction};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let browser = Browser::new()?;
browser
    .execute(BrowserAction::Open {
        url: "https://example.com/".to_owned(),
    })
    .await?;
browser.close().await?;
# Ok(())
# }
```

On macOS, the private in-process browser selects chrome-headless-shell when it
is available, avoiding macOS application/profile services entirely. It also
recognizes Chrome for Testing installations created by `agent-browser install`
or Playwright and always uses a fresh temporary profile with the noninteractive
mock Keychain/basic password store. If no dedicated automation browser is
installed, construction fails with an actionable configuration error instead
of silently launching personal Chrome, an ambient `CHROME` executable, or the
login Keychain. Explicit profile import, an explicit executable, and remote CDP
remain caller-owned policies.

The browser starts lazily on its first local action. Use `Browser::builder()`
for deterministic browser context, egress policy, storage state, diagnostics,
or an explicitly managed CDP endpoint.

Pixel-calibrated captures set both CSS viewport dimensions and device pixel
ratio, then reuse one semantic or CSS target across screenshots and visual
comparison:

```no_run
use nanocodex_browser::{Browser, BrowserAction, BrowserActionResult, BrowserTarget};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let browser = Browser::new()?;
browser
    .execute(BrowserAction::SetViewport {
        width: 1440,
        height: 1200,
        device_scale_factor: Some(2.0),
    })
    .await?;
let capture = browser
    .execute(BrowserAction::Screenshot {
        full_page: false,
        annotate: false,
        target: Some(BrowserTarget::css("#mixer")),
    })
    .await?;
let BrowserActionResult::Screenshot { image: Some(image), .. } = capture else {
    return Err("browser did not return an image".into());
};
println!("{}x{} device pixels", image.width, image.height);
browser.close().await?;
# Ok(())
# }
```

Mobile audits use pinned Chromium profiles and verify page-visible state before
reporting layout and input findings:

```no_run
use nanocodex_browser::{
    Browser, BrowserAction, BrowserDevicePreset, BrowserOrientation, BrowserTarget,
};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let browser = Browser::new()?;
browser.execute(BrowserAction::SetDevice {
    device: BrowserDevicePreset::Iphone15Pro,
    orientation: BrowserOrientation::Portrait,
}).await?;
browser.execute(BrowserAction::Open {
    url: "https://example.com/".to_owned(),
}).await?;
let report = browser.execute(BrowserAction::MobileAudit {
    devices: vec![],
    orientations: vec![],
    ready: Some(BrowserTarget::css("#app > *")),
}).await?;
println!("{report:?}");
browser.close().await?;
# Ok(())
# }
```

Real Mobile Safari is an explicit Appium/XCUITest backend. The harness chooses
an exact device name or UDID and owns the external Appium server lifecycle:

```no_run
use nanocodex_browser::{
    BrowserIosConfig, BrowserIosDeviceSelector, BrowserTool, IosBrowser,
};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let config = BrowserIosConfig::new(
    "http://127.0.0.1:4723/".parse()?,
    BrowserIosDeviceSelector::ExactName("iPhone 16 Pro".to_owned()),
)?;
let ios = IosBrowser::new(config)?;
let tool = BrowserTool::from_ios(ios.clone());
// Pass `tool` to `Tools::builder().provider(tool)`.
drop(tool);
ios.close().await?;
# Ok(())
# }
```

`BrowserIosDeviceInventory::discover().await` reports Xcode simulators and USB
devices while preserving discovery failures separately from a successful empty
inventory. iOS supports mobile state/audit, raw tap/swipe, active-element text,
evaluation, URL/title, navigation/reload, and plain screenshots. CDP-only
actions return a typed unsupported error; there is no Chromium fallback.

## Capabilities

- Chromium navigation and interaction through semantic references, CSS, roles,
  text, labels, frames, tabs, and open shadow roots.
- Bounded snapshots, DOM/layout/style inspection, console and source-mapped
  errors, network bodies, WebSocket messages, HAR, and React diagnostics.
- Viewport- and element-targeted screenshots, pixel-level visual diffs,
  PDF, visual/session/performance traces, constant-rate WebM video at up to 60
  fps, CPU profiles, coverage, heap inspection, accessibility/axe, Lighthouse,
  and CrUX actions.
- Harness-owned cookies/storage, virtual passkeys, explicit host-passkey handoff,
  upload roots, browser egress policy, remote CDP, and libkrun VM composition.
- Pinned Chromium mobile profiles, verified audit matrices, and an explicit
  real-Safari Appium/XCUITest backend with Xcode device discovery.

The Nanocodex CLI default and bare `--browser` use private automation-browser
discovery with a dedicated profile under the Nanocodex state directory. Its
cookies, origin databases, extension state, and permissions survive across
Nanocodex sessions. The profile is owner-only, remains separate from ordinary
desktop-browser profiles, and is exclusively locked while Chromium is running;
a concurrent browser action returns an in-use error instead of risking profile
corruption. `--browser-profile=temporary` restores the fresh-profile behavior:
that mode imports cookies from the first usable Brave, Chrome, Chromium, Edge,
Firefox, or Safari profile and deletes browser changes at shutdown.

On macOS the automation profile uses a dedicated chrome-headless-shell or
Chrome for Testing installation, not the user's Brave or Chrome application.
`--browser=brave` remains an explicit executable choice while still using the
dedicated Nanocodex profile. Temporary-mode Chromium-family cookie decryption
may briefly use the source browser's Keychain identity, but that bounded broker
exits before the long-lived automation session starts. macOS first attempts
background decryption and automatically opens the isolated interactive broker
only when the populated cookie store exports no usable cookies.

The CLI's virtual platform authenticator persists testing passkeys across
browser and Nanocodex restarts in `$NANOCODEX_DIR/browser/passkeys.json`, or
`~/.nanocodex/browser/passkeys.json` by default. That owner-only file contains
private keys. Library consumers opt into persistence explicitly with
`VirtualAuthenticator::platform_passkey().credential_store(path)`.
With persistence configured, the model-facing browser tool can call `passkeys`
to inspect non-secret metadata, `passkey_use` to expose one saved credential,
`passkey_new` to create against an empty authenticator, and `passkey_auto` to
restore normal automatic credential selection. Selection never deletes the
other credentials in the persisted store.

Pass `--passkeys=host` on macOS to use passkeys from the host authenticator
without exporting them. The browser tool first navigates headlessly as usual.
At a passkey gate it calls `host_passkey_start`, which opens the same page in a
visible Brave, Chrome, Edge, or Chromium application backed by a new temporary
profile. Complete the ceremony with Touch ID or an iPhone and leave that window
open. A subsequent `host_passkey_resume` imports the resulting cookies, closes
the temporary window, and continues in the private headless session. The
ordinary desktop-browser profile is never opened, and passkey private keys and
user handles never enter Nanocodex or browser-tool output. Host and virtual
passkey modes are mutually exclusive.

```console
nanocodex --passkeys=host
```

Cookie-source selection is automatic and has no CLI flag. Nanocodex probes the
installed Brave, Chrome, Chromium, and Edge profiles for a usable cookie store,
then falls back to Firefox and Safari. It imports the first compatible source
for the private local browser. Disable the browser itself when a run must not
receive host cookies:

```console
nanocodex
nanocodex --browser=none
nanocodex --browser=chromium
nanocodex --browser=brave
```

Chromium-family sources use their own executable as a short-lived decryption
broker. Firefox is copied with SQLite's online backup API. Safari's bounded
binary-cookie decoder may require granting the Nanocodex process macOS access
to Safari's sandboxed profile. Source profiles are never mutated, and cookie
values remain outside the model-callable browser schema. The `exec` contract
advertises `tools.browser` with a compact summary; its complete action guidance
remains runtime-only in `ALL_TOOLS`. Automatic import deliberately gives the
agent authenticated access to every site represented in the selected profile.

On macOS, if a populated Chromium-family store cannot be decrypted in the
background, Nanocodex opens one visible window backed only by a fresh copied
temporary profile. Approve the Keychain prompt within two minutes; the broker
exports the cookies and closes before the dedicated automation browser starts.
The ordinary source profile and its tabs are never opened or controlled. Pass
`--cookie-auth=background` to suppress that interactive fallback.

```console
nanocodex --cookie-auth=background
```

Harnesses can export a lossless, exact-origin cookie snapshot without exposing
it to the browser tool. The returned values retain session, host-only,
expiration, SameSite, and supported partition metadata. Opaque partitions fail
closed instead of being flattened.

```rust,no_run
# use nanocodex_browser::BraveSession;
# use url::Url;
# async fn capture() -> Result<(), Box<dyn std::error::Error>> {
let source = BraveSession::standard()?;
let capture = source
    .capture_origin_cookies(Url::parse("https://example.com")?)
    .await?;
assert_eq!(capture.origin.as_str(), "https://example.com/");
# Ok(())
# }
```

## Current boundaries

- This package is unpublished and its API is not stable. Its complete backend
  targets native Unix hosts and Chromium's DevTools protocol. The focused iOS
  backend targets Mobile Safari through an operator-managed Appium/XCUITest
  server; it does not claim CDP feature parity. Firefox, WASM, Node, and Python
  are not browser backends.
- Local mode is a private browser profile, not an OS sandbox. VM mode requires a
  prepared ext4 image, VMM entry point, gvproxy, and libkrun firmware. Its
  default network lease permits internet access; callers must supply their
  egress lease and browser policy when stronger restrictions are required.
- All browser actions remain available together. Their roughly 67 KiB contract
  is runtime-only with provider registration, so enabling browser adds no model
  warmup schema bytes. The contract is not capability-filtered after discovery.
- Lighthouse, CrUX, and video require caller-supplied external tooling or
  credentials. Brave profile transfer remains harness-owned and intentionally
  absent from the model-callable schema. INFO tracing retains only safe
  configuration and count metadata, not cookie values, browser storage values,
  configured header values, raw network events, or action payloads/results.
- Cloned browser handles share one session and serialized action stream. Call
  `Browser::close` or `BrowserVm::shutdown` when deterministic cleanup matters.
- The crate consumes `nanocodex-vm` unconditionally today, so local-only builds
  still pay the VM dependency's compile/link cost.
