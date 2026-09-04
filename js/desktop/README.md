# Nanocodex — Electron

A desktop client for the managed-agent API, with a Codex-inspired interface and
Herdr-inspired tabs. The installed Codex app was a read-only visual reference;
no Codex application source or assets are redistributed.

## Run

Use Node 24 or newer from the repository root:

```sh
pnpm install
pnpm dev:desktop
```

The main process reads `NC_API_KEY` or `NANOCODEX_API_KEY` from the root `.env`
in development. `NANOCODEX_ENV_FILE` selects an explicit file, including when
launching a packaged app. `NANOCODEX_MANAGED_URL` overrides the default service,
`https://nanocodex.gakonst.workers.dev`.

First launch walks through phone number and SMS verification. The main process
creates a device API key, remembers it with OS encryption, and ends the temporary
sign-in session. Phone, code, session cookie, and API key never enter saved tabs
or conversation events. Settings offers the same flow when switching accounts;
an existing API key and a custom service origin remain under Advanced.
On a packaged app's first successful environment connection, it remembers that
account with OS encryption so Finder and Dock relaunches stay connected.
Provider credentials stay in the managed account. The renderer is sandboxed,
has context isolation and a restrictive CSP, and receives only specific actions
and public snapshots. No credential or general-purpose HTTP/process proxy is
exposed by the preload bridge.

## Tabs and conversations

Tabs can live in the sidebar or across the top. Their order, custom titles,
drafts, selected Hand, folders, appearance, and active selection survive restart.
Drag to reorder, double-click to rename, close with the close button, or reopen
with Shift+Cmd+T. Closing a tab keeps its managed agent running; recent threads
and search reopen it. Dots show working, completed-unseen, or attention states.

Cmd+T/N creates a tab, Cmd+W closes it, Cmd+1–9 switches tabs, Ctrl+Tab cycles,
Cmd+K searches conversations, Cmd+O chooses a folder, and Cmd+, opens Settings.
Enter sends; Shift+Enter adds a line. While running, Send queues a follow-up,
Steer updates the current task, and Stop cancels it.

Astra (`gpt-6-astra`) is available in the model picker with Low through Max
reasoning and Standard mode. Selecting it replaces unsupported None/Pro choices
with High/Standard. Model and reasoning mode lock after the first accepted turn;
effort and Fast remain editable. Sol remains the default. The tested production
account accepts Astra settings but currently rejects its turns with a provider
error saying Astra is unsupported for that ChatGPT account; no fallback model is
silently substituted.

History, streamed text, tool calls, results, and subagent activity come from
durable managed events. The shared runtime reconnects from event cursors and
batches updates; Markdown loads separately from the initial shell. Appearance
follows macOS by default, with explicit light/dark choices.

## Hands

Choosing a folder and sending with Automatic creates or reuses a local Hand for
that conversation. Selecting the folder alone does not start compute. Cloud
actions use cloud compute even when the current tab has a local folder.

**Use this computer** defaults to a named local Hand and `~/Nanocodex` workspace.
Start Hand creates that default folder if needed and connects immediately.
Choose another folder with the native picker. Machine IDs and thread scope are
under Advanced. A selected stopped local Hand starts when you send a message.

Local commands run with the OS user's permissions; the folder is a working
location, not an OS sandbox. Credentials from the main process are excluded from
child environment variables. Native Hands provide shell, filesystem, and pipe
sessions. Stop closes the attachment and terminates process groups, including
detached descendants. Account changes stop Hands and discard the old account's
grants. Hands never start merely because preferences were restored.

VM Hands use the existing `nanocodex2 hand` implementation and a prepared Linux
root image and guest runtime. Each Hand receives a private writable image copy;
on macOS its private helper copy receives the required hypervisor entitlement.
Original files remain unchanged. Advanced settings expose those paths,
CPU/memory, and networking. `NANOCODEX_HAND_BINARY`,
`NANOCODEX_VM_ROOTFS`, and `NANOCODEX_VM_GUEST_RUNTIME` prefill installed paths.
The desktop does not build VM images or replace the Rust runtime.

Run Nanocodex on another computer with the same account to provide remote
compute. **Discover in thread** asks the agent for `accountInfo`; results are
labeled as last-reported inventory. **Create cloud Hand** requests the existing
Cloudflare `mount` tool through a managed turn. Selected Hands are resolved to
actual mount paths by the agent, with instructions against silent substitution.

Connections opens the existing account app for provider sign-in, Connect
approval, connectors, MCP, and SSH identities. These retain their existing owners.

## Build and verify

```sh
pnpm build:desktop
pnpm --filter @nanocodex/desktop test
pnpm --filter @nanocodex/desktop-runtime test
node --test js/nanocodex-tools/test/node-process.test.mjs
pnpm test:desktop
pnpm --filter @nanocodex/desktop package
```

Electron Vite bundles the used workspace packages; this app doesn't load a
Rust/WASM agent engine. Packaging produces `release/mac-arm64/Nanocodex.app`
on Apple Silicon. The runtime app payload contains only the bundled code and
metadata, with no workspace `node_modules`, `.env`, or native build outputs.

Playwright runs the real Electron app in hidden, isolated test instances. Live
checks require the environment account key, use their own conversations and
workspace folders, and clean up those resources. Screenshots and traces are in
`test-results/`. The separate native SwiftUI app lives in `macos/`; both apps
use `@nanocodex/desktop-runtime` for agent and Hand behavior.

The onboarding check runs against an isolated HTTP SMS fixture and verifies
wrong-code recovery, successful sign-in, encrypted persistence, and relaunch.
Shared protocol tests also exercise resend/cancel/retry and private JSONL
credential boundaries. Real phone delivery requires a user-supplied number.
