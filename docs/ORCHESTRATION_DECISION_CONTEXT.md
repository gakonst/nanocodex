# Orchestration decision context

This brief records private product context for workers choosing Nanocodex's
next orchestration slice. Independent workers should read it explicitly rather
than assume it is present in their inherited prompt context.

## Decision context

- The primary user experience is one long-running root agent with 3–8
  short-lived, mostly read-only specialist branches.
- Fast live branching matters more than provider-side prompt privacy.
- Nanocodex must remain a headless, library-first SDK: no app server and no
  generic core scheduler.
- `/btw` currently provides one ephemeral fork of the latest completed root
  turn.
- Branches share the workspace but receive fresh drivers, WebSockets, and tool
  runtimes.
- The release should prefer correctness and explicit lifecycle behavior over
  adding more UI surface.

## Candidate next slices

1. Multiple named `/btw` panes.
2. Turn cancellation plus safe branch cleanup.
3. Durable serializable conversation snapshots with checkpoint acceleration.
