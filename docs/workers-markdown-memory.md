# Markdown memory on Cloudflare Workers

Managed agents can maintain `USER.md` for stable preferences, `MEMORY.md` for
curated durable facts and decisions, and `memory/YYYY-MM-DD.md` (optionally with
a lowercase slug) for working notes. The Markdown bodies are authoritative;
search is a derived index. This adapts the core file model from
[OpenClaw's memory design](https://github.com/openclaw/openclaw/blob/main/docs/concepts/memory.md)
to the existing authenticated MemoryScope Durable Object.

No sandbox, mounted filesystem, filesystem watcher, or native SQLite extension
is needed. The implementation uses Durable Object SQLite transactions and FTS5,
the existing `HISTORY_AI_SEARCH` binding for semantic retrieval, and the existing
`AI` binding for bounded extraction and consolidation.
Bodies, revisions, deletion tombstones, and index updates commit together. A
Worker restart does not lose files or an acknowledged append receipt.

## Tools and API

`memory_search` returns bounded hybrid lexical/semantic excerpts with paths, line
ranges and revisions, plus explicit retrieval availability. `memory_get` reads a bounded range; continue using the returned cursor
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
- `/v1/markdown-memory/status`

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

## Semantic retrieval

Every canonical edit atomically enqueues immutable revision/chunk projections.
Durable Object alarms drain bounded batches into AI Search. Provider responses
supply candidate identities; returned text is always rehydrated from live,
authorized SQLite rows after the request finishes. Stale revisions, deleted
notes and foreign owner metadata cannot become recall results. Lexical search
continues when the binding is missing, unavailable or times out; responses report
that fallback rather than claiming semantic retrieval worked.

Hybrid search uses reciprocal rank fusion, recency decay for dated notes and
MMR diversity. Evergreen curated files do not decay. Index retry state survives
eviction. Deletion is immediately effective in canonical reads and recall,
while remote index cleanup is asynchronous and visible in `memory_status`.
Index operations have durable 30-second leases and a 16-attempt budget; deletion
reconciliation stops after a 15-minute horizon. Exhausted/expired receipts remain
visible instead of keeping an alarm alive forever. A new canonical revision
queues new indexing work. Late upload completion reopens deletion work, but
physical removal from the remote index is not claimed until cleanup succeeds.
`DREAMS.md` is readable explicitly but excluded from search and bootstrap.

## Compaction is independent of memory

Managed sessions do not invoke memory extraction before compaction and do not
require a memory receipt to continue. Memory inference failures cannot block
compaction or fail a conversation. Agents save useful context explicitly with
`memory_write` during their work.

## Background consolidation

Successful daily writes queue durable work for the next UTC day. Alarms process
bounded source batches using a tool-free Workers AI completion. Every selected
candidate must match exact source lines and revisions; generated prose cannot
invent a new fact. The pass may add, merge or supersede its own attributed
entries in `MEMORY.md` and `USER.md`, preserving unrelated manual curation.

Revision checks and durable source/curation fences reject stale proposals after
concurrent edits or deletion. Provenance and preimages support audit; deleting a
source invalidates dependent generated entries and retained preimages. Explicit
recall markers and consolidation reports are excluded from automatic promotion,
and identical evidence is deduplicated.
`DREAMS.md` records bounded outcomes without being fed back into retrieval.
Model attempts and retry leases are bounded and persist across eviction.
Extraction permits 48 inference attempts per owner per UTC day. Consolidation
permits three attempts per owner per UTC day, selects at most eight sources and
12 KiB per batch, and retains 32 audit receipts with their preimages. Both passes
limit model output to 2,048 tokens. These are ceilings, not usage targets.

`memory_status` (and its authenticated HTTP endpoint) exposes semantic backlog,
consolidation work and receipts, and extraction receipts without invoking a
model. `NANOCODEX_MEMORY_AUTOMATION=false` disables automatic extraction and
consolidation while retaining authored Markdown and search. The configured
Workers AI model is `@cf/meta/llama-3.3-70b-instruct-fp8-fast`; automatic passes
consume Workers AI usage within the enforced per-owner budgets. Existing
Cloudflare bindings are reused; no local daemon or additional resource is
required.

## References and limits

This is a Workers adaptation of the requested Markdown, hybrid retrieval,
and consolidation behavior. It is not a claim of exact
OpenClaw scheduler or model parity. See OpenClaw's
[memory search](https://docs.openclaw.ai/concepts/memory-search) and
[dreaming](https://docs.openclaw.ai/concepts/dreaming) designs. Automatic extraction
and consolidation are conservative and bounded; explicit memory saves remain
useful for technical progress not present as firsthand user statements.

Muse motivated the requested behavior. Meta's [personal Muse design](https://introducing.muse.ai/)
describes a persistent main conversation and side chats; its memory internals
are not public. Separately, [Muse Code configuration](https://dev.meta.ai/docs/muse-code/configuration)
documents compact MEMORY.md bootstrap and on-demand topic retrieval. These are
product references, not evidence that both products share an implementation.
The linked OpenClaw source and Nanocodex's authorization/storage contracts are
the implementation basis.
