# Markdown memory on Cloudflare Workers

Managed agents can maintain `USER.md` for stable preferences, `MEMORY.md` for
curated durable facts and decisions, and `memory/YYYY-MM-DD.md` (optionally with
a lowercase slug) for working notes. The Markdown bodies are authoritative;
search is a derived index. This adapts the core file model from
[OpenClaw's memory design](https://github.com/openclaw/openclaw/blob/main/docs/concepts/memory.md)
to the existing authenticated MemoryScope Durable Object.

No sandbox, mounted filesystem, filesystem watcher, native SQLite extension,
embedding service, or new Cloudflare binding is needed. The implementation uses
Durable Object SQLite transactions and FTS5, already used for session history.
Bodies, revisions, deletion tombstones, and index updates commit together. A
Worker restart does not lose files or an acknowledged append receipt.

## Tools and API

`memory_search` returns bounded lexical excerpts with paths, line ranges and
revisions. `memory_get` reads a bounded range; continue using the returned cursor
and revision rather than assuming an excerpt is the whole file. `memory_write`
accepts `put`, `append`, and `delete`. Every write requires `expected_revision`
(use zero only for a never-created file). Read the current file after a conflict
and reconcile the intended edit. Deletion retains a monotonically increasing
revision, so a stale create cannot resurrect a removed file.

Daily-note appends require a stable `operation_id`. Retry the exact same request
with the same ID after an uncertain response. Reusing an ID with changed input
fails; replaying an acknowledged append does not duplicate it. Explicit puts and
deletes use revision checks; do not retry them with a freshly guessed revision.

The same handlers are available over authenticated POST endpoints:

- `/v1/markdown-memory/get`
- `/v1/markdown-memory/search`
- `/v1/markdown-memory/write`

For example, read `{ "path": "MEMORY.md" }`, then write
`{ "operation": "put", "path": "MEMORY.md", "expected_revision": 0,
"content": "# Decisions\n\nUse UTC for scheduled exports.\n" }` if the read
reported a missing, never-created file. Responses carry the resulting revision.
The configuration alias `memory` enables these tools alongside the existing
`memories__*` compatibility tools. An empty tool configuration stays empty.

## Ownership and context

Direct account calls default to the authenticated user's private partition,
which follows that user across teams in the same organization. Connect calls
default to their authorized team and cannot select private memory. Explicit
`scope: "team"` selects shared knowledge. Shared writes require
`user_requested: true` and a user request to share that information; this flag is
an intent declaration, not a new source of authority. Every call still checks
live read/write capabilities. Subagents cannot mutate memory. Internal calls
carry the existing organization, team, subject, and private-owner assertions.

At managed prompt startup the host fetches bounded curated and recent daily
excerpts from authorized scopes (UTC today and yesterday). Each scope contributes
at most 12 KiB of content, with at most 4 KiB per file. Files are capped at 64 KiB
and lines at 8 KiB; a ranged read returns at most 16 KiB and 200 lines.
Unchanged snapshots are not appended again to the same live agent session.
The two scoped requests run concurrently with a five-second timeout; a retrieval
failure withdraws the previous snapshot instead of blocking the prompt. Saved prose is wrapped as untrusted data,
never instructions or permission. Current user corrections take precedence.
Fresh reads prevent an old local snapshot from being reused after a correction
or deletion. Already delivered conversation content cannot be erased.

Existing versioned records, prepared personalization, and append-only ad-hoc
notes remain intact and available through their existing APIs. Canonical Markdown
files are also visible through the existing memories list/read/search adapter. There is no
silent migration or reclassification of personal facts as team knowledge.

## Deliberate boundaries

This implements the Markdown source-of-truth, retrieval, revision-safe editing,
daily journal, and bootstrap parts of OpenClaw's design. Retrieval is lexical
FTS5; semantic embeddings, hybrid ranking, background dreaming/consolidation,
and an automatic model turn before compaction are not implemented here. Agents
are instructed to save useful context during work; that is not a guarantee of a
pre-compaction flush. These features require separate lifecycle and inference
work rather than a local daemon transplanted into a Worker.

Muse motivated the requested behavior. Meta's [personal Muse design](https://introducing.muse.ai/)
describes a persistent main conversation and side chats; its memory internals
are not public. Separately, [Muse Code configuration](https://dev.meta.ai/docs/muse-code/configuration)
documents compact MEMORY.md bootstrap and on-demand topic retrieval. These are
product references, not evidence that both products share an implementation.
The linked OpenClaw source and Nanocodex's authorization/storage contracts are
the implementation basis.
