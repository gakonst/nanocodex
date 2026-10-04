# nanocodex-connect-embed

Composable React agent UI for managed and Connect agents. The package uses
`nanocodex-react/agent` for transcript state, streaming, steering, queues,
cancellation, history, and cleanup. Authorization, source selection, and agent
creation stay with the host application.

## Install

```sh
npm install nanocodex-connect-embed nanocodex-react nanocodex react react-dom @tanstack/react-query
```

React 18+ is supported. Connect hooks additionally need a `QueryClientProvider`,
as described in `nanocodex-react/connect`. No component imports CSS automatically.

## Nanocodex conversation UI

```tsx
import { AgentConversation } from "nanocodex-connect-embed";
import "nanocodex-connect-embed/conversation.css";
import "nanocodex-connect-embed/generated-output.css";

return <AgentConversation {...existingAgentTerminalViewProps} />;
```

`AgentConversation` is the existing `AgentTerminalView` component with identical
props and runtime identity. It preserves voice, rich Markdown, queued prompts,
conversation timing events, custom tools/accessories, transcript scrolling,
hidden/preview modes, and generated media. It owns its own shared controller, so
render it directly instead of nesting it inside `AgentProvider` for the same
source. The full view uses its existing CSS classes; import `conversation.css`
for its layout. Existing `TerminalComposer`, `TerminalTranscriptSurface`,
`GeneratedOutputView`, `ConversationHistoryRail`, and `ElevenLabsSettings` are
also public exports. Dedicated `/composer`, `/transcript`, `/generated-output`,
and `/conversation` entry points preserve their existing prop contracts.

The `/primitives` entry point avoids importing the rich terminal/Markdown/voice
presentation when only the minimal embed is needed. `/connect` re-exports existing
connection hooks and adapter; `/managed` owns the reusable managed source adapter.
Neither presentation layer implements another transport or transcript reducer.

To match the Nanocodex account app, retain its host styles and wrapper classes:
`js/account/src/index.css`, `AgentTerminal.css`, and `Home.css` provide the
`chat-workspace` palette, message bubbles, typography, and rounded composer.
`conversation.css` supplies the shared component layout; it does not import an
application theme. The minimal primitives below intentionally have a different,
optional presentation and remain styleless unless CSS is explicitly imported.

Run `pnpm --filter nanocodex-connect-embed demo:account` for the representative
app-styled browser demo. It imports those exact app styles with the shipped
`AgentConversation`, submits over real SDK HTTP/SSE, and records dark/light and
mobile screenshots, a video, a trace, and request evidence in
`output/connect-embed-account-demo/`. Set `EMBED_BROWSER_OUTPUT` to choose another
output directory and `PLAYWRIGHT_CHROMIUM_EXECUTABLE` for an installed browser.
This is an app-styled fixture with a synthetic agent service, not a signed-in
account session or live-model recording. Authorization, voice, and host account
controls are outside this demo.

## Ready-made minimal embed

```tsx
import { useMemo } from "react";
import { AgentEmbed } from "nanocodex-connect-embed/primitives";
import { createConnectAgentSource } from "nanocodex-connect-embed/connect";
import "nanocodex-connect-embed/styles.css";
import "nanocodex-connect-embed/themes.css";

function ConnectedChat({ connectAgent, historyAllowed }) {
  const agent = useMemo(() => connectAgent
    ? createConnectAgentSource(connectAgent, { history: historyAllowed })
    : undefined, [connectAgent, historyAllowed]);
  return <AgentEmbed agent={agent} theme="light" />;
}
```

Get `connectAgent` from the existing `useConnectAgent` hook or your authorized
Connect client. The explicit `history` flag must reflect the approved grant;
`false` excludes other turns and never requests retained history. The embed does
not request broader capabilities or hold credentials. Keep the normalized source
identity stable between renders; replace it when changing accounts or sessions.

For a managed agent, use the same UI with the managed source adapter:

```tsx
import { createManagedAgentSource } from "nanocodex-connect-embed/managed";

const source = useMemo(() => managedAgent
  ? createManagedAgentSource(managedAgent, { history: true })
  : undefined, [managedAgent]);
return <AgentEmbed agent={source} error={connectionError} retry={reconnect} />;
```

Managed history may use a host-owned `ManagedHistoryCache` and `onActivity`
callback. Clear account-owned caches on sign-out; cache attachments should ignore
late `retain` calls after eviction. Both adapters release their watcher on
unmount. Unmounting detaches the observer; the Stop action requests cancellation
of backend work.

## Compose your own UI

```tsx
import {
  AgentProvider, AgentStatus, AgentMessages, AgentActivity,
  AgentComposer, AgentLoadOlder, AgentPendingPrompts, useAgentEmbed,
} from "nanocodex-connect-embed/primitives";

function Chat({ agent }) {
  return <AgentProvider agent={agent} maxEntries={300}>
    <header><AgentStatus /></header>
    <AgentLoadOlder />
    <AgentMessages showActivity={false} empty="Start a conversation." />
    <aside><AgentActivity /></aside>
    <AgentPendingPrompts />
    <AgentComposer placeholder="Ask about this project…" />
  </AgentProvider>;
}

function CustomStop() {
  const { controller } = useAgentEmbed();
  return <button disabled={!controller.running}
    onClick={() => controller.cancel()}>Stop</button>;
}
```

All primitives emit semantic HTML with `data-agent-part` attributes and accept
ordinary element props where applicable. There are no inline styles, injected
style sheets, global resets, or theme requirements. `AgentMessages` preserves
entry order and includes tools/plans by default. Set `showActivity={false}` when
showing them separately. `renderEntry(entry, controller)` replaces each default
entry renderer. `AgentMessage`, `AgentTool`, and `AgentOutput` also work without a
provider. Default message text is escaped plain text; supply a Markdown renderer
or use the rich conversation below. Generated media URLs are validated through
the shared SDK output policy.

`AgentComposer` supports controlled `draft`/`onDraftChange`, labels, placeholders,
`onSubmitted`, and `promptIntent="queue" | "steer"`. The default behavior starts
a turn when idle and steers while active. Enter inserts a newline; the Send
button submits. `disabled` disables typing/submission, while `submitDisabled`
blocks submission only, useful for draft validation. Stop remains usable while
the composer is disabled. Rejected root submissions retain the draft. Uncontrolled
drafts reset on source changes; hosts own controlled draft resets.

`useAgentEmbed()` exposes `{ agent, controller, error, retry }`. Controller
snapshots and actions are the existing `nanocodex-react/agent` public contract,
including `submit`, `steer`, `cancel`, `cancelPrompt`, `clear`, and `loadOlder`.
`AgentProvider` accepts the controller's `maxEntries`, `visible`, and `onEvent`
options. Hidden providers retain controller state and coalesce updates; they do
not automatically hide DOM. `AgentStatus` presents connection errors and retry;
turn errors are transcript entries. `AgentLoadOlder` offers retry after history
failures without losing the transcript.

`AgentEmbed` assembles status, history, messages, pending prompts, and composer.
Use `messages`, `composer`, and `containerProps` for customization. Optional
`styles.css` scopes its layout to `[data-agent-embed]`; add that attribute to your
own wrapper to opt in. Optional `themes.css` defines light/dark palettes selected
by `data-theme` (or `AgentEmbed theme`). Override `--agent-background`,
`--agent-foreground`, `--agent-border`, `--agent-muted`, `--agent-accent`,
`--agent-radius`, and `--agent-font` in your app.

## Development and verification

From the repository root:

```sh
pnpm --filter nanocodex-terminal build
pnpm --filter nanocodex-connect-embed test
```

The package test runs public component journeys through the real controller and
Connect adapter, declaration consumer checks, and an npm package contents check.
Only the external backend boundary is synthetic in the component journeys. They
cover streaming tools/media, steering, queued cancellation, failure recovery,
source changes, history-disabled isolation, and unmount cleanup. Browser and
managed HTTP/SSE journeys provide additional transport and interaction coverage.

Create release artifacts with `pnpm pack`, which rewrites workspace dependencies
to public semver ranges; do not publish a raw `npm pack` of workspace source.
`check:pack` installs that actual archive in an isolated offline consumer,
preprovisions the currently unpublished sibling dependency, verifies production
dependency ranges, imports every JavaScript entry point, and renders the
unconnected embed. The tarball and manifest evidence are retained under the
repository's ignored `output/connect-embed-pack/`. Publish the sibling packages
before releasing the embed package.

For low-level controller composition, `/headless` re-exports the existing
`nanocodex-react/agent` contract without another state layer. For an approved
Connect grant, `ConnectConversation` accepts the SDK `agent` and `connection`
and derives retained-history and tool-visibility defaults from that grant.

For rich conversation/voice bundles, use the existing `nanocodex-vite` plugin
(`nanocodex({ chatGpt: false })` for a hosted-only app) and deduplicate `react`,
`react-dom`, and `@tanstack/react-query`. Its browser tools resolver selects the
SDK's browser implementations. The `/primitives` entry point needs only normal
React bundling and does not pull in the rich SDK/voice tool graph.
