# nanocodex-terminal

Reusable React presentation for Nanocodex conversations. The package renders the
same semantic transcript and native composer used by the Nanocodex website. It
does not create an Agent, own credentials, choose a transport, or retain history.

```tsx
import {
  AgentTerminalView,
  TerminalComposer,
  TerminalTranscriptSurface,
} from "nanocodex-terminal";
import "nanocodex-terminal/styles.css";
```

`AgentTerminalView` is the complete controller-backed component. It accepts a
structural `Agent` from `nanocodex-react/agent` but never creates one or chooses
its runtime, transport, credentials, or persistence policy. It forwards
`maxEntries` to the canonical controller and supports `showToolCalls` without
changing retained state. Set `voice` to render and own the standard microphone
control; normalized managed and Connect sources retain their canonical voice
handle, while a normal browser Agent works directly. `voiceOptions` is available
for application policy such as a pre-turn authorization fence. Pass `composer`
to replace the input surface without detaching the controller or clearing the
visible transcript.

Full and hidden modes share the same mounted transcript and accessory tree, so
route owners can hide a retained terminal without resetting its reading position
or an interactive artifact. Preview mode omits the accessory. The caller owns
the visibility of a hidden terminal's surrounding route.

```tsx
<AgentTerminalView agent={agent} voice {...terminalProps} />
```

`TerminalTranscriptSurface` and `TerminalComposer` are lower-level controlled
pieces for consumers that already own their controller composition. The
transcript accepts `followTailRequest` for explicit submit-to-tail behavior and
`showToolCalls` for surfaces that intentionally hide tool activity.
Scrolling toward the top loads older history automatically, including upward
wheel or touch gestures on short pages. Requests are single-flight, and retained
message geometry preserves the reading position during delayed prepends and live
output. Failed loads rearm after leaving and returning to the loading boundary.

Generated output remains visible when tool activity is collapsed or hidden.
Code-mode text renders as Markdown; image, audio, video, and file content uses
native media and download controls. Nested tool copies are deduplicated, and
media elements retain their identity when an outer exec result arrives.
Consumers needing only this view can import `GeneratedOutputView` from
`nanocodex-terminal/generated-output` and its styles from
`nanocodex-terminal/generated-output.css`, without loading agent or voice code.
The stylesheet consumes optional `--terminal-background`,
`--terminal-foreground`, `--terminal-muted`, `--terminal-border`,
`--terminal-hover`, `--negative`, and `--font-mono` variables and includes
standalone fallbacks.
