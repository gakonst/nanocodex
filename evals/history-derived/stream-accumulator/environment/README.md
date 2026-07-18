# Streaming file accumulator

`createState()` and `applyEvent(state, event)` maintain the visible files for a
streaming code view.

Events are:

- `{type: "file_chunk", path, sequence, text}`
- `{type: "snapshot_start", generation}`
- `{type: "snapshot_commit", generation}`

File chunks append text in strictly increasing sequence order. Duplicate or
older sequences are ignored. Normal chunks update visible files. While a
snapshot is pending, chunks build its replacement off-screen; visible files
must not temporarily become empty. A matching commit swaps the snapshot in one
step. A stale commit is ignored.

Inputs are immutable. When one visible file changes, preserve object identity
for every unchanged visible file. Preserve first-seen file ordering.
