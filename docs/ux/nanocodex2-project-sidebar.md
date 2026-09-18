# Nanocodex2 project navigation

Press **F2** to show or hide Projects > Threads. Opening focuses the sidebar:
use Up/Down and Enter to switch, or click a thread. Escape or Tab returns to
the composer. Press `r` in the sidebar to refresh. The sidebar automatically
refreshes every five seconds while visible and leaves the composer usable.
It hides below 60 terminal columns to preserve chat space.

Project names and parent relationships come from managed service metadata.
Conversations without project metadata appear as standalone projects; deleted
parents and missing roots do not hide their remaining children. The `›` marker
identifies the open thread independently of keyboard selection. Thread markers
mean running (`●`), idle (`○`), or unknown (`?`). Status enrichment prioritizes
the current project, queries at most 64 conversations with eight concurrent
requests, and stops after 750 ms. Failed or unqueried states remain unknown.

The running section shows up to five in-process subagents of the open thread,
with an overflow count. It derives activity from retained/live managed run
lifecycle and structured subagent tool receipts, including nested parent IDs.
Persistent project child conversations remain separately selectable threads.

Switching detaches the local observer without cancelling service-owned work.
Draft text, image attachments and cursor position are retained per conversation
for this TUI process. Switching waits for local shell work and unresolved
message delivery or queued followups; it does not silently discard that input.
Failed or cancelled switches keep the current draft and conversation.

Validation: 537 TUI unit tests pass (two existing ignored), and all 70 terminal
lifecycle tests pass. The new PTY journeys exercise project/child switching
while the master stays active, independent drafts, progress while detached,
no navigation-triggered cancellation or submission, and nested hosted subagent
lifecycle. These use a controlled managed service and real terminal input.
