# Rust API changelog: 0.5 → 0.6

Nanocodex 0.6 expands the embeddable agent runtime with optional durable
execution, reusable subagent orchestration, and a managed backend. Existing
applications can keep their own UI, storage, tools, and deployment.

This guide compares the released `v0.5.0` tag with the 0.6 release branch. It
covers public Rust contracts and behavior that affects embedders; the
[full changelog](../CHANGELOG.md) contains the commit inventory. JavaScript,
Python, HTTP API, and application migrations are outside this guide.

## Upgrade at a glance

| Area | 0.5 | 0.6 / migration |
| --- | --- | --- |
| `TurnResult::usage()` | `&TurnUsage` | `Option<&TurnUsage>`; handle missing accounting |
| `TurnResult::snapshot()` | `SessionSnapshot` | `Option<SessionSnapshot>`; handle backends without local checkpoints |
| `Nanocodex::session_id()` | `SessionId` | `&str`; use `local_session_id()` for an optional typed local ID |
| `Nanocodex::prompt()` | `impl Into<Prompt>` | `impl Into<PromptRequest>`; strings and `Prompt` still work |
| `Nanocodex::builder()` | Generic over a Responses service factory | Generic over `BuilderBackend`; ordinary `builder(openai)` still works |
| Default model / reasoning | `Model::Sol` / `Thinking::High` | `Model::Astra` / `Thinking::Low`; pin both for old defaults |
| Embedded tool host | `nanocodex_tools::hosted` | `nanocodex_tools::embedded`; shared `Tools` recipe |
| Code Mode execution / wait | `CodeModeExecution` | `Result<CodeModeExecution, CodeModeHostError>` |
| Code Mode result literal | No cell metadata | Add `cell: None`, or describe the observed cell |
| Billing uncertainty | Counter and service-error accessor | Removed; retain actual reported usage and cost availability |
| Tool / response enum literals | No asynchronous flag | Add `asynchronous: false`, or use constructors |
| MCP discovery | Tool search plus generic resource helpers | Tool search is the discovery surface; generic resource helpers removed |
| Browser / egress / VM / voice paths | `crates/experimental/...` | `crates/...`; update Git/path dependencies |

Rust **1.97** and edition **2024** are unchanged from 0.5. Update directly used
Nanocodex crates together to `0.6`; mixing their 0.5 and 0.6 types is not a
supported migration strategy.

## 1. Cargo features and imports

The facade now defaults to `durability`, `openai`, and `tools`. Enabling the
durability feature makes the API available; it does **not** persist an ordinary
agent until you attach a store.

```toml
# Ordinary embedded agent, including the optional durability API.
nanocodex = "0.6"

# Or explicitly select the local agent without the durability dependency.
# nanocodex = { version = "0.6", default-features = false, features = ["openai", "tools"] }
```

For applications that used `default-features = false`, select features explicitly:

| Crate / feature | Contract |
| --- | --- |
| `nanocodex/openai` | Local builder, `OpenAi`, and provider-specific APIs |
| `nanocodex/tools` | Tool recipes and native attachment support |
| `nanocodex/workspace-tools` | Adds the workspace runtime |
| `nanocodex/durability` | Durable execution extension; also enables `openai` |
| `nanocodex/managed` | Native managed backend; optional and not in defaults |
| `nanocodex/realtime` | Realtime client; also enables `openai` |
| `nanocodex-agent/openai` | Enabled by default; disabling it leaves the common lifecycle contracts |
| `nanocodex-oai-api/events` | Event contracts without the complete client; `client` includes it |
| `nanocodex-tools/attachment` | Native attached tool execution without the complete native runtime |

Code importing local builders, `AgentHandle`, rollout APIs, or provider-specific
errors from a no-default-features build must enable `openai`. The lower-level
OAI crate keeps `client` enabled by default; its `pricing` module is also usable
without that feature.

Sources: [facade features](../crates/nanocodex/Cargo.toml),
[agent features](../crates/nanocodex-agent/Cargo.toml),
[OAI features](../crates/nanocodex-oai-api/Cargo.toml),
[tool features](../crates/nanocodex-tools/Cargo.toml).

## 2. Agent handles, prompts, and completed turns

### Optional usage and snapshots — source breaking

The common handle can represent a local or managed agent. A completed turn no
longer promises that every backend reports usage or exports a local snapshot.

```rust,ignore
// 0.5
let result = agent.prompt("Review this change").await?.await?;
let tokens = result.usage().total_tokens();
let snapshot = result.snapshot();
```

```rust,ignore
// 0.6
let result = agent.prompt("Review this change").await?.await?;
if let Some(usage) = result.usage() {
    let tokens = usage.total_tokens();
    // Record reported accounting here.
}
if let Some(snapshot) = result.snapshot() {
    // Persist this snapshot in your application's resume envelope.
}
```

For a local-only application whose persistence requires a snapshot, treat
`None` as an application error instead of silently skipping a save. Do not
replace absent usage with zero: missing accounting and a zero-token turn mean
different things. `TurnUsage::cost_status()` still distinguishes whether an
estimate is available.

### Session identity — source breaking

`session_id()` now borrows a string. Use `.to_owned()` when retaining it after
the handle is dropped. Code requiring the local typed ID can call
`local_session_id() -> Option<SessionId>` with the `openai` feature.
`agent_id()` provides the stable agent identity used to reopen durable
backends; do not assume that a backend's agent and session IDs are identical.

```rust,ignore
// 0.5: let id: SessionId = agent.session_id();
// 0.6: retain an opaque identity suitable for every backend.
let id: String = agent.session_id().to_owned();
```

### Prompt conversion and builder generics — source breaking for wrappers

Plain strings and `Prompt` values still work, as does the two-stage await:
`agent.prompt(input).await?.await?`. A wrapper accepting `impl Into<Prompt>`
should convert explicitly before submitting:

```rust,ignore
async fn submit(agent: &Nanocodex, input: impl Into<Prompt>) -> Result<Turn> {
    let prompt: Prompt = input.into();
    agent.prompt(prompt).await
}
```

Alternatively expose `impl Into<PromptRequest>` to allow caller-owned request
IDs. `Nanocodex::builder` now takes a `BuilderBackend` and returns that
backend's associated builder. Remove old service-factory turbofish arguments
and let `Nanocodex::builder(openai)` infer the concrete builder.

### Additive lifecycle APIs

- `context()` reads model-visible history at a safe boundary without appending
  a developer message.
- `set_model()` selects the local model before conversation activity begins;
  it does not switch models in an already active local conversation.
- `spawn_with(SpawnOptions::new().model(...).thinking(...))` overrides a clean
  child's settings. `AgentHandle` also adds `spawn_with` and `spawn_many`.
- `steer_with_id` and `withdraw_steer` are available on `Turn` and `TurnControl`.
  Withdrawal returns `false` once the identified steer is no longer the latest
  or has reached a model boundary.
- `disconnect()` releases local resources without asking a durable backend to
  cancel its work. Backends without detached execution fall back to shutdown.
- `Turn::request_id()` and `TurnResult::request_id()` expose admitted operation
  identity when one exists.
- `TurnUsage::from_reported(ReportedTurnUsage { ... })` lets backend adapters
  construct explicit accounting without a serialization round trip.

Sources: [handle](../crates/nanocodex-agent/src/agent/handle.rs),
[turn/result/request](../crates/nanocodex-agent/src/agent/turn.rs).

## 3. Defaults and runtime behavior

### Model and reasoning defaults changed

`MODEL` and `Model::default()` now select `gpt-6-astra`.
`Thinking::default()` changed from `High` to `Low`. When reasoning has not been
explicitly selected, the builder uses the chosen model's default: low for
Sol/Astra and medium for Terra/Luna. To retain the 0.5 default selection:

```rust,ignore
let (agent, events) = Nanocodex::builder(openai)
    .model(Model::Sol)
    .thinking(Thinking::High)
    .build()?;
```

This pins model and effort, not the old release's entire runtime behavior.
Astra rejects `Thinking::None` and `ReasoningMode::Pro`; invalid combinations
fail validation. Update model pickers and configuration validation accordingly.

`context_window_tokens(...)` is new on `OpenAi` and the local agent builder.
The default remains 272,000; the supported maximum is 872,000 and larger
values are clamped. Increasing this setting affects compaction and can cross
the SDK's long-context pricing threshold.

`instructions(...)` replaces the selected model's built-in instructions.
Use new `additional_instructions(...)` on the agent builder to append host
instructions while retaining that base.

### Resume uses the current runtime policy

In 0.5, resume required matching instructions and tool definitions. In 0.6,
`resume(snapshot)` retains committed conversation history and cache lineage,
but installs the current builder's instructions, tool definitions, and handlers
for subsequent work. This lets applications upgrade their tool catalog across
restarts. If your product requires an exact policy match, validate your own
policy version in the application-owned resume envelope.

The local `SessionSnapshot` format version remains **1**. There is no mandatory
whole-database rewrite solely because of this release. Structural validation
still applies; this is not a promise that arbitrary modified snapshots work.
`into_context_parts()` and `SessionSnapshotHead::with_context(...)` let stores
separate metadata from conversation records and reassemble the snapshot.

### Accounting and transport changes

- Removed `RunMetrics::billing_uncertain_response_attempts`,
  `TransportStatsDelta::billing_uncertain_response_attempts`, and
  `ResponsesServiceError::billing_uncertain()`. Remove dashboard bindings and
  struct fields that depended on them; there is no replacement certainty flag.
- `ServiceTier::Fast` is new. `ServiceTier::for_model(model, fast_mode)` selects
  `Fast` for Astra fast mode and `Priority` for older models. Update exhaustive
  matches and serialized tier consumers.
- Built-in cost rates changed, including Sol and long-context rates. Turn cost
  now accumulates per-call estimates. Do not expect recalculation with 0.6 to
  reproduce an old saved estimate; retain historical estimates if needed.
- `ResponsesError::IdleTimeout` was removed along with Responses WebSocket
  event-idle timeouts. Remove matches on that variant. Applications needing
  their own deadline must own it and cancel the turn explicitly; dropping a
  result waiter is not a cancellation policy.
- `ResponsesError::InvalidToolSchema` retains the offending discovered
  definition. Policy-violation errors have explicit classification, and missing
  checkpoint detection covers additional provider responses.

Sources: [model/defaults](../crates/nanocodex-oai-api/src/lib.rs),
[builder](../crates/nanocodex-agent/src/agent/builder.rs),
[snapshot](../crates/nanocodex-agent/src/session.rs),
[pricing](../crates/nanocodex-oai-api/src/pricing/estimate.rs),
[transport errors](../crates/nanocodex-oai-api/src/transport/error.rs).

## 4. Tools, Code Mode, and MCP

### Embedded host rename and shared recipe — source breaking

| 0.5 | 0.6 |
| --- | --- |
| `hosted::CodeModeHost` and related contracts | `embedded::CodeModeHost` and related contracts |
| `HostedToolMode` | `EmbeddedToolMode` |
| `HostedToolRuntime` | `EmbeddedToolRuntime` |
| `HostedToolRuntimeControl` | `EmbeddedToolRuntimeControl` |
| `HostedTools::new(host)` | `embedded::bind_host(tools, host)` using ordinary `Tools` |

```rust,ignore
// 0.6: host bridges use the same tool recipe as native callers.
use nanocodex_tools::{Tools, embedded::bind_host};
let tools = Tools::builder().without_defaults().build()?;
let tools = bind_host(tools, application_host);
```

`bind_host` is a documented-here but doc-hidden language-binding seam; ordinary
Rust applications should configure `ToolsBuilder` directly. On WASM,
`runtime::Tools` now refers to the shared recipe rather than `HostedTools`.

`ToolRuntime::with_tools` is now private. Replace
`ToolRuntime::new(workspace, web, images).with_tools(&tools)` with
`ToolRuntime::new_with_tools(workspace, web, images, &tools)`.

`execute_code`, `wait_for_code`, and their owned/observer variants now return
`Result<CodeModeExecution, CodeModeHostError>` on both native and embedded
runtimes. Add `?` or handle bridge failures. A successful `Result` can still
contain `execution.success == false`, which represents a model-visible script
failure rather than a broken host bridge.

`CodeModeExecution` literals need `cell: None` for complete-cell hosts, or
`Some(CodeModeCell { origin_call_id, running })` for resumable cells. Hosts can
opt into `supports_cells()` and implement `wait_with_updates`; existing
complete-cell implementations default to no resumable cells. Implement host
cancellation when retaining work across calls.

### Tool recipes and exposure

`ToolsBuilder::add` composes fixed tools, `WorkspaceTools`, and native MCP
families through `ToolSource`. Native attachments can execute a caller-owned
recipe remotely without moving model execution into the tool process.

The existing `ToolExposure::DirectOnly` and `Hidden` policies are now enforced
at nested dispatch too. Direct-only and hidden tools are not callable through
Code Mode; do not depend on nested dispatch bypassing exposure restrictions.

### MCP discovery and credentials — behavior changes

The generic model tools `list_mcp_resources`, `list_mcp_resource_templates`,
and `read_mcp_resource` were removed. MCP tool search is now the discovery
surface. If your product relies on resource browsing, expose application-owned
tools for that behavior and update prompts that mention the old helpers. These
were runtime tools, not public Rust resource classes.

`McpOAuthStore::acquire_refresh_lock` is a new method with a default
implementation, so existing implementations still compile. Its default lock
coordinates refreshes only within one process. Stores shared across processes
must override it with a bounded cross-process lock covering load, refresh, and
save. Persist the authorization issuer via `McpOAuthCredentials::issuer` and
`authorization_issuer()`; refresh tokens must remain bound to their issuer.

`McpServer::parallel_tools` declares individual tools safe for concurrency.
`payment_provider`, `McpPaymentProvider`, and `McpPendingPayment` add optional
paid-call preparation and commit/rollback handling. The underlying RMCP
dependency moves from 1.8 to 3.0; consumers that also use RMCP directly must
align their integration types.

Sources: [embedded host](../crates/nanocodex-tools/src/embedded/mod.rs),
[Code Mode results](../crates/nanocodex-tools/src/embedded/types.rs),
[runtime](../crates/nanocodex-tools/src/runtime/execution.rs),
[tool selection](../crates/nanocodex-tools/src/runtime/selection.rs),
[MCP](../crates/nanocodex-tools/src/mcp/mod.rs),
[OAuth store](../crates/nanocodex-tools/src/mcp/oauth.rs).

## 5. Low-level Responses and events

For applications constructing protocol values rather than using the agent:

- `ToolDefinition::Function` and `Custom`, and `ResponseItem::FunctionCall`
  and `CustomToolCall`, add `asynchronous: bool`. Add `false` to old literals;
  use `..` in patterns that do not inspect every field. Tool constructors
  default to synchronous execution; `with_async_execution()` opts in.
- `ResponseItem::ConfigurationUpdate` and `ConfigurationUpdateReasoning` carry
  Astra reasoning updates. Preserve them when storing typed history.
- `ToolOutputContent::EncryptedContent` carries opaque provider output. Update
  exhaustive matches and preserve its payload rather than rendering it as text.
- `ToolDefinition` now accepts `output_schema` when deserializing host metadata,
  while still omitting it when serializing provider requests.
- `ResponseItem::strip_unbound_id()` preserves IDs bound into encrypted
  reasoning, compaction, and function arguments. Use it instead of blindly
  stripping every ID when copying replay history.
- `AgentEventPublisher` and `AGENT_EVENT_PROTOCOL_VERSION` are public. External
  publishers validate version, request identity, advancing sequence, and
  terminal ordering; `EventError` gains the corresponding variants. The event
  protocol version remains **1**.
- In events-only builds, provider API frames project as transport diagnostics;
  `AgentEventData::OpenAi` and `OpenAiEvent` require `client`.
- `SessionBuildError::SerializePromptPrefix` is new. Applications matching
  errors or models exhaustively must also handle the new variants.

Consumers of doc-hidden `__private` integration APIs also need to update
`ModelConfig`: `system_prompt` is now `Option<Arc<str>>`, its accessor returns
`Cow<'_, str>`, and configuration adds `additional_instructions`,
`thinking_explicit`, and `context_window_tokens`. These are implementation
integration surfaces; prefer the public builders where possible.

Sources: [tool definitions](../crates/nanocodex-oai-api/src/responses/tool.rs),
[response items](../crates/nanocodex-oai-api/src/responses/item.rs),
[tool outputs](../crates/nanocodex-oai-api/src/tools/mod.rs),
[events](../crates/nanocodex-oai-api/src/events/stream.rs).

### ChatGPT authentication additions

`chatgpt_access_token(...)` accepts a ChatGPT personal access token, and
`resolve_chatgpt_auth_status(...)` resolves credential metadata asynchronously.
For hosts that own credential storage and HTTP transport,
`ChatGptSubscription` and `ChatGptSubscriptionHost` expose the Rust-owned
device-login, credential rotation, and unauthorized-recovery lifecycle.
These are additive APIs; existing file-based login is not replaced. See the
[authentication exports](../crates/nanocodex-oai-api/src/auth/mod.rs) and
[subscription host contract](../crates/nanocodex-oai-api/src/auth/subscription.rs).

## 6. Optional SDK layers introduced in 0.6

### Durable execution in your own process

`nanocodex-durability` works with the local Rust agent and caller-owned storage;
using the managed API is not required.

```rust,ignore
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_durability::{DurableAgentExt, DurableSession, MemoryStore};

let state = DurableSession::open(MemoryStore::new()?, "agent-123").await?;
let (agent, events) = Nanocodex::builder(openai)
    .durability(state)
    .await?
    .build()?;
let result = agent
    .prompt(PromptRequest::new("Review this change").request_id("job-123"))
    .await?
    .await?;
```

`MemoryStore` demonstrates the contract; use a persistent store for recovery
after process loss. SQLite and Postgres stores are optional native features.

Migration boundaries:

- A request ID identifies one operation. Retrying the same ID with the same
  prompt resumes or replays it; reusing the ID with a different prompt conflicts.
  Supplying an identified prompt to a local agent without an execution policy
  fails with `ExecutionPolicyNotConfigured`.
- Completed model/tool outputs replay from committed records. An unfinished
  external call may run again, including duplicate tool effects or billing.
  This is not exactly-once external execution.
- Clean spawned descendants receive independent durable state. Forking a
  policy-owned checkpoint is unsupported, and replayed results cannot serve
  as in-process `fork_from` checkpoints. Ordinary non-durable forks remain.
- `execution_policy_disposition()` distinguishes retry, reopen, and fatal
  failures. A stopped policy owner must be rebuilt from authoritative state.
- The new durability record format is **4**, separate from local snapshot
  version 1. It is not a schema migration for an existing application's SQLite
  session index, transcript journal, or resume envelope. Task-tree topology,
  mailboxes, and application memory remain application-owned.

See [durability usage and store contract](../crates/nanocodex-durability/README.md)
and the [recovery model](DURABILITY.md).

### Reusable subagents

`nanocodex-subagents` extracts task-tree orchestration into an optional native
and WASM crate. Use `channel(...)`, `install_tools(...)`, and
`NanocodexBuilder::tools_factory(...)` to give every agent fresh tools backed
by one application-owned registry. Drain updates and close the tree through
its control handle. This builds on the spawn/fork capabilities already present
in 0.5; it does not make subagents an entirely new core capability.

See [the subagent integration example](../crates/nanocodex-subagents/README.md).

### Managed backend and attached tools

`nanocodex-managed` supplies a native managed lifecycle backend, resumable
events, and optional attachment of caller-owned tools. `nanocodex-hand`
provides the Hand protocol. These are optional integrations; a local Rust
embedding can adopt the rest of 0.6 independently.

See [managed client](../crates/nanocodex-managed/README.md) and
[tool attachments](../crates/nanocodex-tools/README.md).

## 7. Browser, egress, VM, and voice

Browser, egress, VM, and voice crates move out of `crates/experimental`.
Their Rust crate names retain their existing spelling. Update path dependencies
and build scripts. Evals and computer crates retain the experimental label.
Promotion does not imply crates.io availability: browser, egress, and VM still
have `publish = false` because of their dependency constraints.

Specific migration points:

- Browser removes `Browser::auth_handoff`, `BraveAuthHandoff`, and
  `OpenedBraveAuthHandoff`. Applications must own any interactive login flow;
  use the cookie-source/session APIs for supported cookie capture instead of
  the removed open/resume helper.
- `VirtualAuthenticator` is no longer `Copy`; clone it when reusing a value.
  `BrowserBuilder::virtual_authenticator` is no longer a `const fn`.
- `BrowserTool` now uses its automation-browser configuration. On macOS, a
  missing dedicated automation browser fails construction instead of falling
  back to personal Chrome. Recheck executable/profile setup.
- Browser adds persistent virtual credentials, host passkey support, and
  frame-aware WebMCP discovery/invocation. Match new action and result variants
  as needed in adapters.
- `CHATGPT_REALTIME_MODEL` changes from `gpt-live-1-boulder-alpha` to
  `gpt-live-1-codex`. Realtime gains `connect_with_sdp` for caller-owned media
  and `OpenAi::attach_realtime_call` for sideband attachment to an existing call.
  Existing-call attachment does not create or reconfigure the call; V2
  attachment is unsupported.
- Voice adds separate `nanocodex-voice-protocol`, `nanocodex-voice-native`, and
  `nanocodex-voice-ffi` layers. Choose the protocol, native client, or C boundary
  appropriate to the embedding rather than copying application voice state.

Sources: [browser](../crates/nanocodex-browser/src/lib.rs),
[cookie sessions](../crates/nanocodex-browser/src/session.rs),
[realtime](../crates/nanocodex-oai-api/src/realtime.rs),
[voice](../crates/nanocodex-voice/README.md),
[release crate inventory](../scripts/release-crates.sh).

## Migration checklist for an existing Rust product

1. Upgrade direct crate dependencies together and select required features.
2. Fix optional usage/snapshot handling, session ID storage, prompt wrappers,
   tool-host imports, and protocol literals.
3. Pin model and reasoning if the new defaults are not intended.
4. Review resume policy: current instructions and tools now apply after restart.
5. Update event/accounting consumers and prompts naming removed MCP helpers.
6. For shared OAuth stores, implement refresh locking across processes.
7. Adopt durability, subagent orchestration, or managed execution only where
   the application needs them; retain ownership of its UI and data model.

Examples above are migration fragments: `openai`, application host objects,
and surrounding error types are supplied by the embedding application.
