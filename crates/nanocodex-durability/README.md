# nanocodex-durability

`nanocodex-durability` is the portable durable-execution boundary used by
Nanocodex agents. Rust owns one complete current-state value, its optimistic
revision protocol, deduplication, checkpoint selection, and every recovery
decision. Hosts atomically load and replace opaque state. There is no event-log
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
OpenAI client and tool registry have been composed into an agent:

```rust,ignore
use nanocodex_agent::{Nanocodex, OpenAi, PromptRequest};
use nanocodex_durability::{DurableAgentExt, DurableSession, MemoryStore};
use nanocodex_tools::Tools;

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
tools. Beginning a step returns `Execute` when no output is committed or
`Replay(output)` when one is. An unfinished step executes again after recovery.
This deliberately permits duplicate provider billing and duplicate external
tool effects. Standalone compaction follows the same rule: a committed
checkpoint replays, while an unfinished transform runs again.

Completed tool outputs replay exactly without consulting the recovered runtime's
current tool catalog. Tool availability matters only when an unfinished step
must execute. Capabilities represented by a tool result, such as spawned-agent
identity, own their persistence and reconnection semantics outside generic step
replay. A durability-attached agent passes the same lifecycle to every clean
descendant. Each child uses its own stable session ID as its state ID and owns
an independent fence, operation journal, and checkpoint. Forking a durable
checkpoint remains unsupported because a fork is not a clean state.

This persists agent execution, not a higher-level task-tree registry. An
orchestrator that assigns separate tree-local IDs, mailboxes, roles, or status
must persist that topology independently and map those IDs to agent session
IDs when it needs cold tree reconstruction.

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

The logical host contract has only two operations:

- `acquire(state_id, owner_id)` atomically advances the persisted owner
  fence and returns that token with one coherent current-state value.
- `replace(state_id, owner_token, expected_revision, payload)` checks authority
  before revision, installs the complete replacement, and advances the revision
  in one transaction.

Hosts do not deserialize state, snapshots, model outputs, or tool results.
Rust owns those types and all recovery decisions.

Only a definite `NotCommitted` replacement may be retried on the same owner.
`Fenced`, revision `Conflict`, and unconfirmed `Backend` failures require a fresh
owner acquisition and loading the complete current state before deciding what
ran.

Each external effect follows an intent/effect/settlement boundary. A start
commits `effect_pending`; settlement atomically replaces it with `completed`
and the exact output. A crash before settlement executes the effect again with
the same stable identity and input. A crash after settlement replays the output
without invoking the effect again. Agent model calls, warmups, and compactions
reconstruct their requests from that retained input, so changed instructions,
tool catalogs, or environment context cannot redefine an interrupted call.
New calls use the current runtime; live tool authorization and owner fencing
still apply. There is no per-effect retry policy or uncertainty state. Operation
terminals atomically carry their checkpoint and replay receipt.

Unlike a store that stages an output separately from source-ordered transcript
placement, this store owns one opaque total state. A second materialization
write would add latency without adding a recovery boundary.
