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

All memory tools use the `memories__*` namespace. The four pinned Codex tools
(`list`, `read`, `search`, `add_ad_hoc_note`) keep their input/output schemas.
The same `memories__read` and `memories__search` operations cover curated Markdown
and daily notes. Markdown adds only `memories__write` and `memories__status`.

`memories__read` accepts `path`, optional one-based `line_offset`, and `max_lines`.
`memories__search` accepts a `queries` array and Codex substring matching options,
and returns file/line matches. Semantic indexing is an internal retrieval facility;
it does not change the baseline Codex search contract. To save a note, provide
the operation, path, and content:

```json
{ "operation": "put", "path": "MEMORY.md", "content": "Use UTC for scheduled exports." }
```

Use `append` for daily notes and `delete` to remove a note. Read existing content
before replacing it. The model does not supply revisions or retry identifiers;
the host supplies delivery identity and storage commits each write atomically.
Internal consolidation still uses revision fences, and repeated delivery of the
same tool call reuses its stored result.

Authenticated POST endpoints are `/v1/memories/{list,read,search,add_ad_hoc_note,write,status}`.
Existing `/v1/markdown-memory/{get,search,write,status}` endpoints remain available
for older clients, including their optional explicit revision and delivery fields.

The configuration alias `memory` enables the complete namespace. Old `memory_*`
configuration entries resolve to the corresponding namespaced tools without
advertising duplicate tools. An empty tool configuration stays empty.

## Ownership and context

Direct account calls default to the authenticated user's private partition,
which follows that user across teams in the same organization. Connect calls
default to their authorized team and cannot select private memory. For direct
accounts, Codex file reads/searches access shared notes through the `team/` path
prefix. `write` and `status` accept `scope: "team"`. Shared writes require
`user_requested: true` and a user request to share that information; this flag is
an intent declaration, not a new source of authority. Every call still checks
live read/write capabilities. Subagents cannot mutate memory. Internal calls
carry the existing organization, team, subject, and private-owner assertions.

Normal and voice use the same already-prepared, scoped personalization snapshot.
The existing background refresh loads bounded curated/recent daily
Markdown excerpts (UTC today and yesterday). Admission does not await a
memory read, timeout, extraction, indexing pass, or consolidation job. A cache
miss starts the turn or voice session without memory; a background refresh can
make context available on later turns or through the active voice context channel.

Each Markdown scope contributes at most 12 KiB after serialization, with at most
4 KiB per file. Files are capped at 64 KiB and lines at 8 KiB. Codex reads retain
the upstream line-selection and truncation behavior. Prepared copies have bounded leases and are
invalidated in the background after Markdown edits. Recent changes may lag until
refresh or invalidation arrives. Expired copies are not injected; current user
corrections take precedence, and explicit tools can verify canonical memory.
Unchanged snapshots are not appended again to the same live agent session.

Saved prose is untrusted data, never instructions or permission. Voice lifecycle
replay projects only currently eligible prepared context, retaining scope and
authorization checks. Voice clients deliver prepared fields through the existing
background context channel, including after media connects. Large snapshots stay
out of the bounded SDP call request. Already delivered conversation content cannot
be erased.

Versioned legacy facts and their CRUD endpoints are retired. Activation removes
only legacy fact/scan tables and invalidates old fact-bearing prepared bodies.
Canonical Markdown, append-only Codex notes, history, and prepared Markdown
context remain. Private notes are never reclassified as team knowledge.

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
while remote index cleanup is asynchronous and visible in `memories__status`.
Index operations have durable 30-second leases and a 16-attempt budget; deletion
reconciliation stops after a 15-minute horizon. Exhausted/expired receipts remain
visible instead of keeping an alarm alive forever. A new canonical revision
queues new indexing work. Late upload completion reopens deletion work, but
physical removal from the remote index is not claimed until cleanup succeeds.
`DREAMS.md` is readable explicitly but excluded from search and bootstrap.

## Capture on message arrival

An accepted direct-account user input sends a detached event to the user's private
MemoryScope. The live session does not await delivery or a memory decision. The
recipient acknowledges the event and starts its decision immediately with
`waitUntil`; there is no capture debounce, scheduled batch, persistent inbox, or
retry loop. A slow decision does not hold up another message or a conversation.
This uses the existing internal Durable Object binding, without a long-lived
ReadableStream connection or a new queue/stream resource.

Jev (`typesafe/jev` through the existing Workers AI binding) classifies complete
host-selected statements as `retain`, `skip`, or `uncertain`. It receives the user
text and any surrounding text in that input, not a scan of previous chats.
Only `retain` decisions at confidence >= 0.9 are selected. That threshold is a
conservative policy, not a calibrated accuracy claim. The host validates exact
source quotations, speaker, bounds and secrets, then saves eligible evidence to
`memory/YYYY-MM-DD-flush-<hash>.md`. The model cannot choose paths or invent prose.
These notes are immediately readable/searchable; prepared chat/voice context
refreshes separately and may lag.

Accepted text turns and steering inputs are eligible. Voice delegations and final
transcript tails use bounded `transcript_json` speaker entries; assistant speech
and the provider's handoff instruction are not user evidence. Legacy flattened
voice transcripts, truncated or oversized sources, and sources without clear
attribution are skipped. Capture occurs when the voice client delivers a handoff
or transcript tail, not on individual audio frames. Autonomous turns, Connect
sessions, multiplayer rooms, disabled memory/network settings and callers without
private memory read/write capabilities do not trigger automatic capture. There is
no historical backfill.

Capture is best effort: a lost delivery, unavailable provider, overload or failed
decision skips that event. It never creates recovery work for the live session.
Existing evidence receipts prevent repeated delivery from duplicating saved notes.
Withdrawal cancels an in-flight decision, retracts its generated note and derived
entries, and suppresses late delivery even after memory settings are disabled.
Explicit edits to the generated note take precedence and are preserved. Manual
note edits/deletion cancel active decisions. Normal memory tools remain available
for deliberate saves, corrections and deletions.

The service allows eight simultaneous decisions per scope and at most 1,000
inference attempts per private owner per UTC day. Decisions time out after 15
seconds. Full source events are bounded to 64 KiB; at most 32 candidates and 12
selected quotations of 1,024 characters each are accepted. Context-bearing long
messages may be skipped rather than clipping qualifications. These limits can
miss useful information; explicit saves remain available.

## Compaction is independent of memory

Managed sessions do not invoke memory extraction before compaction, wait for a
memory receipt, or include capture in their execution/recovery lifecycle. Memory
failures cannot reject live input, block compaction, or fail a conversation.
The old internal flush endpoint still has no pre-compaction caller. Existing
durable conversation records remain independent of optional memory capture.

## Background consolidation

Daily writes queue optional consolidation for the next UTC day. A queue or alarm
scheduling failure does not reject the saved note. Alarms process bounded source batches using a tool-free Workers AI completion. Every selected
candidate must match exact source lines and revisions; generated prose cannot
invent a new fact. The pass may add, merge or supersede its own attributed
entries in `MEMORY.md` and `USER.md`, preserving unrelated manual curation.

Revision checks reject stale proposals after concurrent edits or deletion. Appends
and edits outside cited lines preserve entries whose evidence is unchanged;
corrections and deletions retract affected generated entries. Manual curation
fences in-flight proposals while preserving unrelated pending sources and the
daily model budget. Removing an attributed entry excludes its cited lines from pending work while
preserving the other lines in the same daily file. Provenance supports audit, and source edits clear retained
preimages. Explicit recall markers and consolidation reports are excluded from
automatic promotion, and identical evidence is deduplicated.

Canonical reads and background consolidation proceed independently of remote
personalization-cache invalidation failures. Markdown changes invalidate prepared
copies in the background.
`DREAMS.md` records bounded outcomes without being fed back into retrieval.
Model attempts and retry leases are bounded and persist across eviction.
The older internal extraction endpoint retains its 48-attempt admission limit
on the same per-owner daily counter. Consolidation
permits three attempts per owner per UTC day, selects at most eight sources and
12 KiB per batch, and retains 32 audit receipts with their preimages. The generative extraction/consolidation model output is limited to 2,048 tokens.
Jev returns bounded classifications rather than generated notes. These are ceilings, not usage targets.

`memories__status` (and its authenticated HTTP endpoint) exposes semantic backlog,
consolidation work and receipts, and on-message capture attempts/receipts without
invoking a model. `NANOCODEX_MEMORY_AUTOMATION=false` disables automatic extraction and
consolidation while retaining authored Markdown and search. Consolidation uses
`@cf/meta/llama-3.3-70b-instruct-fp8-fast`; capture uses `typesafe/jev`. Automatic passes
consume Workers AI usage within the enforced per-owner budgets. Existing
Cloudflare bindings are reused; no local daemon or additional resource is
required.

## Evaluation

Run `pnpm --filter nanocodex-managed-service run test:memory` for runtime, scope,
Codex tool contract and personalization checks, and
`cargo test -p nanocodex-voice-protocol` for transcript provenance. The bounded
live classifier evaluation uses only synthetic data and the existing development
Wrangler AI binding:

```sh
node scripts/memory-capture-eval.mjs NEW_OUTPUT_DIRECTORY
```

It freezes cases and source hashes before calls, makes at most one request per
case, records abstentions/failures and never retries an uncertain request. It is
a small workload check, not a calibrated benchmark or production latency test.
The [2026-09-24 live results](evals/memory-capture-2026-09-24/REPORT.md) include
all outcomes, fixture labels and source hashes.

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
