# Nanocodex2 project navigation

F2 toggles the project sidebar. Each project appears once: its row opens the
master conversation, and Left/Right or the disclosure marker folds its child
threads. The current project starts expanded. Up/Down, the mouse wheel, and
clicks navigate; Enter opens a thread. Escape/Tab returns to chat. Press `/` to
filter project and thread names locally, including children of folded projects.
The current-thread marker is separate from the keyboard selection.

The compact rail uses the current theme, muted child rows, and a subtle selected
background. Running counts roll up to the project. Up to three running helpers
appear beneath the current thread, with an overflow count. Unknown activity is
not presented as confirmed idle. Missing names have readable fallbacks instead
of raw UUIDs. The grouping and quiet visual treatment are inspired by
[Herdr's workspace navigation](https://herdr.dev/docs/concepts/).

The catalog is prefetched and cached. Opening paints retained rows immediately
and independently refreshes metadata; slow activity requests never gate the
list. Activity requests cover at most 12 visible/relevant threads with four
concurrent requests and a 750 ms background budget. Cached activity expires in
15 seconds. The sidebar refreshes while visible; `r` requests fresh metadata.
Generation checks discard stale catalog and status responses. The sidebar hides
below 60 columns, keeping composer input usable.

Switching detaches the local observer without cancelling service-owned work.
Draft text, image attachments, and cursor position survive per conversation for
the TUI process. Unresolved local work, message delivery, or queued followups
must settle before switching. A newer sidebar selection replaces an attachment
still in flight. Failed/cancelled switches preserve the original conversation.
Actual transcript attachment remains network-bound: fresh state is required,
then history and attachment run concurrently. No stale state is used to enable
input on a different thread.

Validation uses real terminal input against a controlled managed service:
73 terminal lifecycle tests pass, including active switching, independent
drafts, detached progress, nested hosted helpers, local filtering over 501
threads, and superseding a slow attachment with a later keyboard selection.
The old build fails the 500 ms catalog-paint test with a blocked status request;
the redesigned build paints in about 12 ms, reopens from cache in about 13 ms,
and filters the 501-thread fixture in about 13 ms on the development machine.
These are fixture measurements, not production network-latency guarantees.
A synthetic themed rendering is covered by `synthetic_sidebar_preview`.
