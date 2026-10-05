# nanocodex-durability

`nanocodex-durability` is the portable durable-execution boundary used by
Nanocodex agents. Rust owns a small execution head, immutable payload records, its optimistic
revision protocol, deduplication, checkpoint selection, and every recovery
decision. Hosts publish records and their head in one transaction. There is no event-log
replay during recovery.

See [the end-to-end durability model and correctness review](../../docs/DURABILITY.md)
for the Rust state machine, Agent/WASM/application consumption, and crash
matrix.

This crate is an optional layer over `nanocodex-agent`: durability depends on
the agent, never the reverse. It implements the agent's neutral execution
policy seam at prompt admission, model calls, tool calls, and committed
session boundaries.

The pieces compose progressively; none of the lower layers imports this crate:

```text
nanocodex-oai-api <- nanocodex-tools <- nanocodex-agent
                                             ^
                                             |
                                  nanocodex-durability
```

Construct only the layer an application needs, or attach durable state after the
OpenAI client and tool registry have been composed into an agent. The examples
below import this crate's core `DurableAgentExt`, which configures execution
recovery for that agent; it does not install a child registry or durable factory:

```rust,ignore
use nanocodex_agent::{Nanocodex, OpenAi, PromptRequest};
use nanocodex_durability::{DurableAgentExt, DurableSession, MemoryStore};
use nanocodex_oai_tools::Tools;

let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let tools = Tools::builder().without_defaults().build()?;

let store = MemoryStore::new()?;
let state = DurableSession::open(store, "agent-123").await?;
let (agent, events) = Nanocodex::builder(openai)
    .tools(tools)
    .durability(state)
    .await?
    .build()?;

// Omit request_id() to let the durable agent generate one during admission.
let turn = agent
    .prompt(PromptRequest::new("hello").request_id("request-7"))
    .await?;
assert_eq!(turn.request_id(), Some("request-7"));
```


Enable the `claude` feature on `nanocodex-durability` to use the same stores,
admission rules, and receipts with the Claude builder. Claude checkpoints keep
native Messages content, including signed thinking and tool results. The
application supplies its authenticated client again when reopening a session;
credentials are never part of a checkpoint.

```rust,ignore
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_durability::{DurableAgentExt, DurableSession, MemoryStore};

let client = ClaudeClient::official(
    reqwest::Client::new(),
    std::env::var("ANTHROPIC_API_KEY")?,
);
let state = DurableSession::open(MemoryStore::new()?, "claude-session").await?;
let (agent, events) = Nanocodex::builder(Claude::latest(client))
    .durability(state).await?
    .build()?;
let result = agent.prompt(
    PromptRequest::new("hello").request_id("request-7"),
).await?.result().await?;
```

Choose `SqliteStore`, `PostgresStore`, or a persistent host store to retain work
across process restarts. Reopen the same state ID and replay the same request ID
and input to recover pending work or return its committed receipt. Unfinished
Claude effects follow the same explicit replay-safety contract as OpenAI effects.
Claude snapshots use the store's chunked immutable payloads; they do not yet
use the OpenAI adapter's per-message context pages. Snapshot serialization and
restoration therefore process the full retained Claude context.

Durability belongs to each explicitly configured agent. Core spawning does not
inherit a durable owner or journal. A lower-level embedding must configure
`Registry::enable_durability`, a factory that opens each child's own
`DurableSession`, registry tools, and reconstruction before replaying child
capabilities. `RegistryOwnership` supplies startup recovery and the foreground
settlement barrier when attached to the native builder's `turn_ownership` hook.

For automatic native composition, import `nanocodex::DurableAgentExt` instead
(with the facade's `durability` feature, plus `claude` for Claude builders).
It installs the registry, tools, same-family factory, and independent child
journals while retaining caller tools and tool-factory recipes. It rejects a
preconfigured spawn factory before mutating child identity or topology. Mixed
OpenAI/Claude routing requires explicit factories with the core adapter. On
WASM, the facade reexports the core adapter; JavaScript hosts compose the tree.

Native facade construction starts owner-bound child recovery without submitting
a root turn, including background work whose root turn already completed.
Await `agent.ready()` to observe completion or the retained startup error. New
prompts also await readiness; shutdown or dropping the last handle cancels
unfinished startup recovery. A successful foreground turn waits for its
foreground children, while failed or cancelled turns stop them before settlement.

Without `.durability(...)`, the same builder is an ordinary non-durable agent.
An OpenAI-only consumer can stop at `OpenAi::instructions(...).build()`, and a
tools-only consumer can stop at `Tools::builder().build()`. A caller that owns
either lower-level lifecycle can use `DurableSession` directly, choose its own
operation and step IDs, and persist its own typed checkpoints and outputs. The
automatic model/tool/checkpoint integration is specifically the
`DurableAgentExt` adapter.

The crate includes an in-memory store on every target and optional native
SQLite and Postgres stores. JavaScript runtimes implement the same small store
contract through the Nanocodex WASM host bridge.

Operations are durable accepted units of work. Steps cover every external
effect inside an operation: model calls, warmup, automatic compaction, and
tools. A new step returns `Execute`; a completed step returns its exact
`Replay(output)`. An unfinished effect requires both its retained and current
replay permissions to be safe. Otherwise it returns `OutcomeUnknown`. Model
calls and compaction explicitly permit replay; ordinary tools do not. See the
replay contract below before opting a tool into repeatable execution.

An active turn retains one current conversation, its execution phase and counters,
and only the effects in the current model/tool batch. Advancing to the next batch
atomically replaces that conversation and removes the settled effects. Recovery
starts at this position; it does not rerun earlier model/tool batches or retain
copies of their requests. Warmup and pre-turn compaction have explicit phases so
an interruption cannot repeat prompt preparation or lose its original context.
The Rust adapter owns these boundaries; hosts do not manage pruning or recovery.
Format 5 retains replay permission alongside immutable payload records. Format 4
heads remain readable, with legacy tool intents treated as unsafe.

Automatic compaction runs as one owned background effect alongside foreground
model/tool batches. Its journaled input contains an immutable conversation cutoff;
foreground checkpoints retain both the pending intent and any completed summary
receipt. The preservation hook settles before the summary provider request starts.
Responses summaries use a separate full-replay transport connection, so a held
summary cannot occupy the foreground connection or its request lock.

When a summary settles, the adapter rechecks durable ownership and verifies that
the current conversation still starts with the exact cutoff. It then installs the
summary followed by every item appended since that cutoff. Rewritten prefixes,
image repairs and competing summaries invalidate that result. Claude retains the
complete signed thinking, opaque blocks and tool-result content in this tail.
Responses installation clears the old response continuation ID; the next request
uses the installed summary and complete tail as its full-replay baseline.

Foreground work continues while the summary is pending below the hard context
boundary. At the hard boundary or context-exhaustion recovery it waits for the
owned summary before admitting another model request. Cancellation stops owned
work; a superseding store owner prevents the old response from publishing. Cold
recovery replays a completed summary receipt without another summary request and
retains foreground receipts until their exact outputs have been incorporated.
An unfinished safe summary may be requested again under the new owner.

The real HTTP/SSE and SQLite journeys in `tests/oai_background_http.rs` and
`tests/claude_background_http.rs` cover held-summary overlap, immutable inputs,
complete tails, hard-limit admission, lost foreground acknowledgements and owner
takeover. Run them with:

```sh
CARGO_INCREMENTAL=0 cargo +1.97 test -p nanocodex-durability \
  --features sqlite,claude --test oai_background_http --test claude_background_http \
  -- --nocapture
```

Completed tool outputs replay exactly without consulting the recovered runtime's
current tool catalog. A receipt for a capability, such as a child agent, requires
the capability's own durable identity and reconstruction path. The subagent
registry uses `ChildJournal` on the same fenced store protocol for topology,
mailboxes, assignments, and results. Each reconstructed child has a separate
`DurableSession`; it never borrows the parent's execution owner. Hosted durable
agents install this factory and registry automatically. Lower-level embeddings
must configure their child factory and durable registry together.

Session documents select `Initial`, `Current`, `AsOf`, or `Block` fork behavior.
The fork checkpoint and selected document values are initialized atomically in
a fresh destination. Successful operation boundaries remain addressable after
terminal receipt pruning; reusing one of those operation IDs is rejected.

The execution head contains references, active phase, counters, and a bounded
receipt tail. SHA-256 addressed records hold exact payloads in chunks of at most
256,000 UTF-8 bytes. Persistent context pages reference 64 messages each; new
boundaries write new messages and changed pages. Cold recovery loads only the
current context and active effects, in batches of at most 16 records. Opening a
session for admission or status does not hydrate conversation bodies.

Resident execution memory scales with current model context, active tool working
memory, and bounded storage buffers. Total historical storage grows with work.
There is no step count or duration cap. A tool that itself allocates an arbitrary
amount of memory still needs a host able to run it. Completed code cells release
their origin-call mappings; suspended cells retain only their live mappings.

The runtime follows the same ownership model as the agent SDK. A
`DurableSession` is a cheap channel handle; one spawned task owns its reducer,
live claims, revision, and owner token. One separate task serializes access to
the caller-owned store so independent agent state drivers can address distinct
state IDs even when the backend itself is not cloneable. There is no shared
mutable reducer or `Arc<Mutex<Connection>>` contract.

```rust
use nanocodex_durability::{Admission, DurableSession, MemoryStore};

# async fn example() -> nanocodex_durability::Result<()> {
let store = MemoryStore::new()?;
let state = DurableSession::open(store.clone(), "agent-123").await?;

match state.admit_typed::<_, String, String>("request-7", &"hello").await? {
    Admission::Accepted | Admission::Pending => {
        state.begin_attempt("request-7").await?;
        state.complete("request-7", &"checkpoint", &"answer").await?;
    }
    Admission::Completed { checkpoint, output } => {
        assert_eq!((checkpoint, output), ("checkpoint".to_owned(), "answer".to_owned()));
    }
    Admission::Failed { checkpoint, error } => {
        assert_eq!((checkpoint, error), ("checkpoint".to_owned(), "provider rejected input".to_owned()));
    }
    Admission::Cancelled => {}
}
# Ok(())
# }
```

Enable `sqlite` and open `SqliteStore` for a directly owned native connection.
Enable `postgres` and pass a driven `tokio_postgres::Client` to
`PostgresStore::new`. Both implement the exact same `StateStore` contract.

The logical host contract has three operations:

- `acquire(state_id, owner_id)` atomically advances the persisted owner
  fence and returns that token with one coherent current-state value.
- `read_record(state_id, key)` returns one immutable body; `read_records` batches
  up to 16 reads when the backend supports it.
- `replace(state_id, owner_token, expected_revision, payload, records)` checks
  authority before revision and publishes immutable records with their new head
  in one transaction. A head can never reference a partially committed batch.

Hosts do not deserialize state, snapshots, model outputs, or tool results.
Rust owns those types and all recovery decisions.

Only a definite `NotCommitted` replacement may be retried on the same owner.
`Fenced`, revision `Conflict`, and unconfirmed `Backend` failures require a fresh
owner acquisition and loading the complete current state before deciding what
ran.

Each external effect follows an intent/effect/settlement boundary. A start
commits `effect_pending` with `ReplaySafety`; settlement atomically replaces it
with `completed` and the exact output. A completed receipt always replays without
invoking the handler. An interrupted effect executes again only when both its
original persisted permission and its current permission are `Safe`. Otherwise
`BeginStep::OutcomeUnknown` requires an explicit unknown-outcome receipt.
The agent adapters turn this into a failed tool result visible to the model;
they never silently redispatch the handler.

Custom execution policies must implement `begin_step_with_replay`; the default
fails closed, including for currently safe effects whose original intent policy
cannot be established. The crate-provided adapters implement the full contract.

Tools default to `Unsafe`. Parallel safety does not imply replay safety. Native
tools opt in with `Tool::is_replay_safe`, dynamic providers with the corresponding
named method, and Claude callbacks with `.tool_replay_safety(name, ReplaySafety::Safe)`.
Use the opt-in only for genuinely repeatable actions or a host journal that
reuses completed receipts and reconciles unfinished effects without duplicating
them. Direct session users choose `begin_step_with_replay`; `begin_step` and
`begin_step_typed` default to unsafe.

Model calls, warmups, compaction, and the explicitly idempotent preservation
barrier are repeatable. Provider requests can incur additional usage after a
crash. Frozen request settings, live authorization, and owner fencing still
apply. Operation terminals atomically carry their checkpoint and replay receipt.

Unlike a store that stages an output separately from source-ordered transcript
placement, this store owns one opaque total state. A second materialization
write would add latency without adding a recovery boundary.

Session documents are committed through `complete_with_documents` or
`complete_step_with_documents`, together with the corresponding receipt. Keys
are limited to 256 bytes; a session retains at most 64 documents and 65,536 bytes
of encoded current document metadata and values, including creation values.
Oversized transactions and version conflicts change neither receipts nor documents.
Immutable historical snapshots use operation-derived lookup records in the
`StateStore`; the execution head has no growing boundary index. Legacy format-5
heads with a boundary index are migrated into these records on their next commit.
`document_fork(operation_id)` returns a loaded `EncodedPayload` checkpoint and a
policy-selected seed. For a self-contained checkpoint (including Claude), pass
it to `initialize_document_fork(seed, &checkpoint)` in an empty destination.
For OpenAI agent checkpoints, use `agent_document_fork(operation_id)` followed
by `initialize_agent_document_fork(seed, &snapshot)` instead: these load the
referenced context pages and publish them under the destination's own records.
Then attach the destination session to a freshly authorized builder.

These document forks copy the selected checkpoint and documents, not pending
effects, operation receipts, or a child tree. They differ from native
`Nanocodex::fork`/`fork_from`: OpenAI rejects those history operations when an
execution policy is installed, and Claude does not implement them. Historical
Claude document forks are supported independently of that native API limit.
