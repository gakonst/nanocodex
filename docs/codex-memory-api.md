# Codex memories API adapter

The public tools use pinned Codex `36430b36881cf5c289cb48e671cfc9e8b542ae7b`
namespace `memories` with `list`, `read`, `search`, and `add_ad_hoc_note`.
Native and JavaScript declarations carry identical input and output schemas.
The native CLI calls the managed `/v1/memories/{method}` API. The managed agent
uses the same file adapter. The old `memory` model tool is no longer registered;
old configuration entries select the four replacements, while an empty tool
list remains empty.

No existing memories are migrated or deleted. Versioned records appear as
`legacy/<id>-v<version>.md`, and reads use the existing ownership and lifecycle
checks. Direct account sessions have a private root and a read-only `team/`
view of shared memories. Connect sessions can access only their authorized team
root. Model arguments cannot select another user or organization. Every call
checks current capabilities, and subagents cannot append notes.

New notes use atomic, append-only creation in the selected partition. Notes are
stored verbatim under `extensions/ad_hoc/notes/`. Duplicate filenames fail.
Existing record replacement/deletion APIs and their personalization invalidation
remain available as management operations. The upstream model API does not
provide update or delete operations, and new notes do not automatically enter
the existing versioned-record personalization snapshot.

Validation includes existing-record reads, shared-team visibility, private-note
persistence, duplicate rejection, denied writes, cross-user isolation, input
validation, search/paging, and native/JS schema equality. The managed HTTP tests
load the already-built removal-version WASM module only to satisfy the worker
module dependency; they exercise the new JavaScript adapter and actual SQLite
Durable Objects, not the WASM execution loop. Full semantic equivalence of every
filesystem/search edge case is not established by this focused suite.
