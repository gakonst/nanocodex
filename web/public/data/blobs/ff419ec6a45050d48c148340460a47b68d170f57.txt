# Browser and Computer Use Parity Plan

Status: proposed. No browser or computer-use production code exists in Harness yet.

This plan is based on the local Codex checkout at `3ac476b` and the locally
installed `browser@26.715.21425` and `computer-use@1.0.1000451` plugins. The
plugins are proprietary; the goal is to reproduce their model-facing contract
and operational invariants, not their packaging or internal abstractions.

## Decision

Browser and computer use stay behind Harness's persistent JavaScript execution
surface. They must not become top-level Responses tools.

Stock Codex works the same way:

- Browser use installs a persistent `agent` global, then the model retains
  `browser`, `tab`, and locator bindings across JavaScript cells.
- Computer use installs a frozen persistent `sky` global.
- The Responses request still advertises the code execution tool, rather than a
  large flat set of browser or computer functions.

This is important for tool-shape parity and prompt caching. When UI capabilities
are disabled, the existing Harness request profile, tool descriptions, prompt,
and cache prefix must remain byte-for-byte unchanged. Browser and computer
documentation is included only in their dedicated eval configurations.

Browser is the first implementation target. It gives us deterministic,
portable fixtures and supplies most of the state, screenshot, cancellation, and
side-effect lifecycle needed by computer use.

## What Codex Actually Ships

### Browser

The implementation is in the installed bundled plugin, not in `codex-rs`:

- `~/.codex/plugins/cache/openai-bundled/browser/26.715.21425/scripts/browser-client.mjs`
- `~/.codex/plugins/cache/openai-bundled/browser/26.715.21425/docs/api.json`
- `~/.codex/plugins/cache/openai-bundled/browser/26.715.21425/docs/api-use-behavior.md`
- `~/.codex/plugins/cache/openai-bundled/browser/26.715.21425/docs/tab-cleanup-iab.md`

The model-facing hierarchy is:

```text
agent.browsers
  -> browser
       -> tabs
            -> tab
                 -> playwright   semantic locators and DOM state
                 -> dom_cua      visible DOM IDs
                 -> cua          screenshot and coordinates
```

The complete plugin supports browser discovery, multiple backends, user tab
claiming, navigation, semantic locators, DOM and coordinate interaction,
screenshots, downloads, file choosers, clipboard, dialogs, history, developer
tools, full CDP, tab handoff, and finalization. We should retain the object
hierarchy but expose only members that are implemented and exercised by an
eval.

Important behavior:

- Prefer semantic Playwright-style state and locators.
- After a mutation, collect the cheapest fresh state needed for the next
  decision. Do not fetch both DOM and a screenshot by default.
- Fall back from locators to visible DOM IDs, then to visual coordinates.
- Reuse browser and tab bindings across cells and turns.
- Treat document-scoped locators and node IDs as stale after navigation or
  document replacement.
- Serialize stateful browser operations.
- Do not blindly replay a mutating operation after an ambiguous failure.
- Finalize tabs as the last browser action of a turn, keeping only explicit
  deliverables or handoffs.
- Reject pending requests when the backend transport closes.
- End and clean up browser state on completion, abort, cancellation, timeout,
  API failure, and code-host exit.

The plugin uses a privileged, length-prefixed JSON-RPC transport to Codex app,
Chrome-extension, or CDP backends. It carries session and turn identity, tracks
turn completion, and cleans up backend resources at turn end. That exact native
transport is not portable or available to copy.

After successful mutations, the plugin captures sanitized tab metadata and a
viewport screenshot in response metadata. It does not print screenshot base64
into JavaScript output. Harness should preserve the separation: operation
metadata is not textual model context, and a screenshot becomes model-visible
only through an explicit image result. Whether automatic response metadata is
useful to our CLI can be decided by an eval; it is not required for the first
slice.

The unmerged Codex Carbonyl prototype is not stock behavior, but supplies useful
process invariants: an ephemeral profile, a secret-free child environment,
bounded snapshots and screenshots, document-scoped handles, serialized CDP,
timeouts, and teardown guards.

### Computer use

The implementation is a signed macOS plugin and service, not `codex-rs`:

- `~/.codex/plugins/cache/openai-bundled/computer-use/1.0.1000451/scripts/computer-use-client.mjs`
- `~/.codex/plugins/cache/openai-bundled/computer-use/1.0.1000451/skills/computer-use/SKILL.md`

It installs this frozen API:

```ts
sky.target = "mac"
sky.list_apps()
sky.get_app_state({ app, disableDiff? })
sky.click({ app, element_index?, x?, y?, mouse_button?, click_count? })
sky.drag({ app, from_x, from_y, to_x, to_y })
sky.perform_secondary_action({ app, element_index, action })
sky.press_key({ app, key })
sky.scroll({ app, element_index, direction, pages? })
sky.select_text({ app, element_index, text, prefix?, suffix?, selection_type? })
sky.set_value({ app, element_index, value })
sky.type_text({ app, text })
```

`get_app_state` returns an accessibility tree or tree diff and a `file://`
screenshot reference. Accessibility text is the default state channel. The
model emits the screenshot as an image only when accessibility state is
insufficient.

Important behavior:

- A turn starts with state inspection, not a mutation.
- Accessibility indices are ephemeral; inspect again after actions.
- Prefer indexed semantic actions over coordinates.
- Return tree diffs by default and a full tree for `disableDiff: true`.
- Automatically settle after actions, normally for about one second and longer
  while loading is detected.
- Serialize requests.
- Never automatically replay semantic mutations.
- If a cached accessibility element is invalidated, refetch only when the match
  is unambiguous; otherwise require a fresh inspection.
- End the computer-use turn on every terminal path.

The native backend depends on macOS Accessibility, ScreenCaptureKit, process
authentication, and OS policy. Harness needs a separate Linux/Harbor backend;
the model-facing `sky` contract and the invariants above are the parity target.

## Harness Architecture

Harness already has the correct outer shape:

```text
Responses exec call
  -> persistent JavaScript host
       -> frozen agent / sky facade
            -> one hidden typed nested handler
                 -> Rust-owned browser or desktop runtime
```

The JavaScript facade is responsible only for the Codex-shaped object model and
argument/result adaptation. Rust owns subprocesses, bounded I/O, state,
cancellation, timeouts, turn identity, and cleanup. There is no per-operation
Python bridge and no generic plugin framework.

Each accepted Harness request owns at most one browser runtime and one computer
runtime. A runtime survives JavaScript cells, model turns, and Responses
WebSocket reconnects within that request. It never survives the request's
terminal event.

UI runtimes receive a scrubbed child environment. In particular, they must not
inherit `OPENAI_API_KEY`, web-search credentials, or arbitrary Harness secrets.

Harness's existing image output and image-aware context accounting are reused.
DOM and accessibility text remain bounded text. Screenshots enter Responses
history only when explicitly emitted. Compaction may discard old UI outputs;
the runtime remains live, but the model must obtain fresh state before acting.

## Eval-First Development Order

Every production slice begins with a failing deterministic fixture or a
recorded stock-Codex parity case. Each slice is one focused commit and must pass
its focused UI eval plus the existing 41-task core-agent gate before the next
surface is exposed.

### Phase 0: fixtures, traces, and measurement

Add a dedicated UI eval configuration without changing existing benchmark
tasks.

Build a deterministic local web fixture containing:

- labelled forms, duplicated labels, checkboxes, selects, and keyboard submit;
- asynchronous loading and SPA rerenders;
- navigation that invalidates old DOM handles;
- iframe and shadow-DOM controls;
- a popup and dialog;
- a canvas or deliberately inaccessible visual-only control;
- a side-effect ledger that makes duplicate clicks/submissions observable.

Build a deterministic desktop contract fixture containing:

- a full accessibility tree and incremental diffs;
- rerenders that invalidate element indices;
- editable text, selection, scrolling, keyboard input, and secondary actions;
- delayed loading;
- incomplete accessibility and a visual-only target;
- the same immutable side-effect ledger.

Capture stock-Codex trajectories against the logical fixtures before adding a
Harness runtime. Preserve the raw rollout/tool stream and derive measurements
from it; do not add another journal.

Record:

- verifier success;
- model turns, JavaScript cells, and nested UI actions;
- DOM/AX bytes and screenshot count;
- input, cached-input, output, and image tokens;
- model, transport, and action latency;
- stale-handle attempts;
- duplicate side effects;
- tabs/processes remaining after termination.

Phase 0 is the gate for all production code.

### Phase 1: capability-gated facade contract

Install a frozen persistent `agent` global only when browser use is enabled.
Back it with the deterministic driver, not Chromium yet. Prove:

- the top-level Responses tool list remains unchanged;
- globals and browser/tab bindings persist across cells and turns;
- the disabled request profile and prompt remain byte-identical;
- only documented, implemented API members are visible;
- stale handles and unknown members fail deterministically;
- cancellation and all terminal outcomes close the driver.

Initial surface:

```text
agent.browsers.getDefault()
browser.tabs.new/list/get/selected/finalize
tab.goto/back/forward/reload/close/url/title/screenshot
tab.playwright.domSnapshot
tab.playwright.getByRole/getByText/getByLabel/getByPlaceholder/getByTestId/locator
locator.count/click/fill/press/check/uncheck/selectOption
tab.playwright.waitForURL/waitForLoadState/expectNavigation
```

Do not expose multi-backend discovery, user-profile claiming, history,
clipboard, downloads, file upload, raw CDP, or approvals in this phase.

### Phase 2: real browser transport smoke

Package a pinned Chromium for the Docker daemon's native architecture rather
than assuming benchmark images contain it. Python may install or upload the
artifact; Rust launches and controls it.

Use an ephemeral profile, isolated home and temp directories, disabled
extensions/background services, a scrubbed environment, process-group cleanup,
bounded output, and explicit startup/operation timeouts. A loopback DevTools
endpoint is sufficient; copying Codex's privileged app pipe is not a goal.

The first real-browser gate is one stdin CLI trajectory that navigates to the
fixture, reads title and URL, explicitly emits a screenshot, closes, and leaves
no browser descendants.

### Phase 3: semantic browser vertical slice

Implement the Phase 1 API over Chromium/CDP. Locator and node handles are opaque
and scoped to the current document. Navigation and document replacement
invalidate them. Mutations are serialized and never replayed after ambiguous
transport failure.

Admit semantic fixture tasks one at a time. Compare Harness to stock Codex on
success, action count, state-check discipline, tokens, latency, and duplicate
side effects.

### Phase 4: browser visual fallback and lifecycle failures

Add bounded viewport screenshots and coordinate click, drag, type, key, and
scroll only after the visual fixture proves semantic targeting is insufficient.
Do not automatically place a screenshot after every action into model context.

Exercise:

- navigation/action timeout;
- browser crash;
- cancellation during navigation and mutation;
- oversized DOM and screenshot output;
- popup and dialog handling;
- initially denied download/file-transfer operations;
- prompt injection requesting environment secrets;
- blocked `file:`, custom-scheme, and local-service probing.

### Phase 5: `sky` contract over a deterministic desktop

Install the exact frozen `sky` method shape behind a separate capability gate.
Use the desktop fixture first. Prove:

- initial inspection before mutation;
- default AX diff and `disableDiff` full state;
- stale-index rejection and mandatory reinspection;
- semantic-first and coordinate fallback behavior;
- on-demand screenshot emission;
- action settling;
- no mutation replay after ambiguous failure;
- turn-end cleanup on success, error, cancellation, timeout, and API failure.

The browser and computer capabilities remain separately enabled. Enabling
computer use must not implicitly add browser documentation or alter normal
coding requests.

### Phase 6: real Harbor computer backend

Implement the smallest real backend justified by Phase 5 failures. The likely
portable target is a Linux accessibility API such as AT-SPI plus an isolated
display, screenshot, and input backend. Keep the `sky` facade stable while the
driver changes.

Start with a controlled desktop image and one application. Expand application
coverage only through admitted evals. Do not attempt macOS compatibility,
signed-app authentication, or Codex's approval store.

## Gates and Stop Conditions

A slice does not advance when any of these regress:

- existing coding reward or verifier pass rate;
- disabled prompt/tool/cache identity;
- duplicate side effects above zero;
- leaked browser or desktop processes;
- stale handles capable of targeting a new document or element;
- memory bounds for DOM, AX, screenshots, or subprocess output;
- terminal-event uniqueness;
- cancellation latency and descendant cleanup.

Initial bounds should follow the useful Codex prototype defaults until evals
justify different values: roughly 8 KiB semantic snapshots, a 4 MiB screenshot
cap, 15-second ordinary browser operations, and 30-second explicit waits. These
are starting limits, not compatibility promises.

The first implementation batch is Phase 0 only: fixtures, verifier ledger,
stock-Codex trace capture, and metrics. It intentionally makes no production
browser or computer-use changes.
