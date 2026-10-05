# xAI backend

`nanocodex-xai` adapts the open-source Grok Build Responses conversation path
into the common `nanocodex-agent` lifecycle. The public Rust facade exposes it
with the default-off `xai` feature, `nanocodex::Xai`, and
`nanocodex::xai`. Construction follows the same provider-specific builder pattern
as Nanoclaude. The implementation runs in process and does not require an
installed `grok` executable.

## Native CLI prompt and workspace context

The shipped CLI selects this backend with `--harness xai`. Its original modular
default instructions cover coding workflow, native `read_file`, `write`,
`search_replace`, `list_dir`, `glob`, `grep`, foreground `run_terminal_cmd`, and
the retained `exec`/`wait`/`tool_search` bridge. Hosted web-search guidance follows
the selected model and `--web-search`; delegation guidance follows `--subagents`.
Additional host capabilities must be discovered from the current catalog.

For default instructions, native xAI builders load only `AGENTS.md` at the
canonical selected workspace root, capped at 8 KiB of source bytes and marked
when truncated. The excerpt is lower-authority JSON reference data. Claude's
`CLAUDE.md` files are not loaded into xAI. A lazy workspace-only
`.agents/skills/*/SKILL.md` index includes paths, not skill bodies: at most 128
immediate entries are scanned, and at most 32 paths / 8 KiB are included. Relevant
bodies can subsequently be read with `read_file`. Nonregular/unreadable files and
symlinks (including directory components) are skipped. Unix automatic content
reads use directory handles and no-follow flags. There is no home/ancestor
search, recursive discovery, import expansion or automatic skill execution.
These checks are not a substitute for OS isolation against hostile concurrent
filesystem changes.

`--instructions TEXT` replaces default modules and automatic project/skill
context, including delegation guidance. An explicit replacement is inherited by
children; without it, each child receives its selected family's defaults. A
Claude child receives Claude-native guidance and Claude project files, and a
Codex child retains the Codex builder's standard instructions. Prompt text does
not grant tools or permissions. Native restored checkpoints retain their saved
instructions. Library embeddings remain explicitly configured and do not inherit
CLI filesystem discovery.

The [native CLI context journeys](CLAUDE_RUNTIME.md#native-cli-instructions-and-project-context)
exercise actual tool effects, transmitted context, bounded reads, symlink
exclusion and cross-family override boundaries using the shipped binary and
loopback providers. This is not full Grok Build or Claude Code application parity.

## Construction

From this checkout, an application can depend on the facade with
`default-features = false, features = ["xai"]`. It also needs `reqwest = "0.13"`
when constructing the HTTP client explicitly.

```rust,no_run
use nanocodex::{Nanocodex, Xai, XaiModel};
use nanocodex::xai::XaiClient;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = XaiClient::new(
    reqwest::Client::new(),
    "https://api.x.ai/v1/responses",
    std::env::var("XAI_API_KEY")?,
);
let (agent, _events) = Nanocodex::builder(Xai::new(
    client,
    XaiModel::Grok46.as_str(),
))
.build()?;
let first = agent.prompt("Remember the word violet.").await?.await?;
let next = agent.prompt("Which word did I give you?").await?.await?;
println!("{}\n{}", first.final_message(), next.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

The embedding supplies its API key and HTTP client. The backend does not read
Grok CLI credential files or perform browser login. The endpoint is explicit,
which also permits host-owned proxies and local protocol fixtures. Model
availability and account access remain provider decisions; selecting an entry
in `XaiModel` does not establish either.

`HarnessFamily::Xai` and `HarnessModel::Xai(XaiModel::Grok46)` identify this
backend in the shared model catalog. The facade reexports `XaiModel` even when
the provider feature is disabled, just as it does `ClaudeModel`. Enabling `xai`
does not enable the existing OpenAI runtime, workspace tools, or durability
extension. The facade's default features are unchanged.

## Pinned model catalog

The inspected Grok Build catalog contains these Responses models. These are
source defaults, not a guarantee of live account entitlement or service limits.

| Model | Context tokens | Reasoning efforts | Default effort | Hosted search |
| --- | ---: | --- | --- | --- |
| `grok-4.6` | 500,000 | `low`, `medium`, `high`, `xhigh` | `high` | Supported upstream |
| `grok-4.5` | 500,000 | `low`, `medium`, `high` | `high` | Disabled upstream |

Both upstream entries set the automatic compaction threshold to 80%, enable
`compaction_at_tokens`, and set `compactions_remaining` to one. The generic
upstream compaction policy defaults to 85%; the model entries override it.
These upstream settings are distinct from the portable backend's controls below.
See the pinned
[`default_models.json`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-models/default_models.json).

## Conversation and lifecycle

The backend owns its Responses HTTP/SSE transport and native conversation.
Follow-on turns replay reasoning and function-call/result associations. Requests
use `store: false`. Only a completed response can dispatch host callbacks;
truncated, incomplete, or failed responses are errors. This deliberately differs
from upstream's incomplete-response tool-call salvage.

The builder exposes `.system(...)`, `.thinking(...)`, `.max_steps(...)`, and
`.request_timeout(...)`. Defaults are 32 model calls per turn, high effort, and a
300-second timeout per sampling call. Rust prompts support text and URL/data-URL
images; local media requires a host capability. The JavaScript prompt surface
accepts text.

The shared lifecycle supports events, cancellation, shutdown, native history
export through `.context()`, developer messages, steering, forks, same-family
children, and idle native snapshots. Steering enters at a model boundary.
`.tools_factory(...)` constructs per-agent callbacks; `.spawn_factory(...)`
installs an explicit alternate-family factory. A child requires the embedding's
credentials and host capabilities for its selected family. Native snapshot
restoration rebinds these through `.restore_runtime(...)`. Snapshots preserve an
unrestricted provider model ID even when its shared family selector uses the
xAI default. Explicit child model selection uses the shared model catalog.

Cancellation interrupts sampling and skips unstarted callbacks. A callback
already running must settle and have its receipt recorded before cancellation
is acknowledged or shutdown releases the session. Hosts must bound external
work they start.

## Host capabilities

Tools are explicit capabilities. `.tool(ToolDefinition { name, description,
parameters }, callback)` retains the simple JSON-input, string-result interface.
`XaiTools::tool_with_context(...)` adds model, session, turn, call, instruction revision, and
host context through `XaiToolInvocation`, with structured `XaiToolReply` results.
`.host(...)` installs an `XaiHost` provider's definitions and dispatcher.

The facade's `xai-tools` feature exposes `nanocodex::xai_tools`; the provider
also accepts these adapters directly. `XaiWorkspaceFiles` supplies `read_file`,
`write`, `search_replace`, `list_dir`, `glob`, and `grep` against an explicitly
selected native directory. It bounds file reads and output, excludes symlinks,
and rejects traversal outside that root. These pathname checks do not replace
OS isolation against concurrent hostile filesystem changes. Files are UTF-8
text; this adapter does not extract PDF, image, or binary contents.

Additional adapters use the authorized host interfaces in
`nanocodex-xai-tools`:

| Adapter | Tools and boundary |
| --- | --- |
| `XaiBash` | `run_terminal_cmd` through `SandboxBashExecutor`; foreground only. Native Unix `AuthorizedShell` requires a per-command authorization callback, clears ambient environment, and bounds time/output. It is not an OS sandbox. |
| `XaiWeb` | Explicitly supported `web_fetch`/`web_search` through `ApprovedWebProvider`; URL, redirect, DNS, network and response-size policy belongs to the host. |
| `XaiMcp` | `search_tool` and inline `use_tool` against the provider's current authorized catalog. No automatic server launch, credentials, or remote transport. |
| `XaiTasks` | Advertised task, result, wait, cancellation and subagent-message capabilities; the host owns execution, task ownership, checkpoints and recovery. |
| `XaiHostTools` | An explicit catalog and dispatcher, including a real browser host's exact capabilities. |

Register providers with `.bash(...)`, `.approved_web(...)`, `.mcp(...)`,
`.tasks(...)`, or `.host(...)`. A browser capability requires a real host
implementation. Registering the adapter does not create a browser, authenticate
a service, or discover credentials. Shared subagent integration is provided by
`nanocodex-subagents` with its `xai` feature.

`.web_search()` and `.x_search()` request xAI-hosted web and X search; they do
not install host callbacks. Provider-hosted tools and embedding-owned tools
remain separate. The pinned upstream catalog enables hosted search only for
Grok 4.6. Colliding host/hosted names are rejected before a provider request.

## Compaction and recovery

Manual `.compact()` and automatic compaction use the same summary path.
`.context_window_tokens(...)` overrides the 500,000-token default;
`.auto_compact_threshold_percent(...)` overrides the model threshold (80% for
the pinned models, 85% for other IDs). A zero Rust override selects that default.
The trigger combines an approximate JSON-size estimate with reported input
usage. `.compaction_keep_tail(...)` controls the desired recent item tail;
the actual split preserves a complete user/tool boundary.

Compaction pins system and developer instructions, summarizes an earlier
prefix, and retains the recent native conversation. Summary generation has no
host tools. Empty, incomplete, or nonshrinking summaries do not replace history.
A recognized context-overflow response can trigger a compact-and-resample path.
Compaction events make success and failure observable.

`.max_retries(...)` enables bounded retries for explicit transient HTTP
rejections; its default is three retries. It does not grant permission to replay an
interrupted stream or an uncertain tool effect. Recovery classifies the actual
HTTP status and exact recognized context error code, never a matching substring
inside provider text. A truncated HTTP rejection is uncertain. An empty
`max_prompt_tokens` terminal permits context recovery only when the stream has
not already emitted output, tool activity or unknown events. `.repetition_limit(...)` limits
identical name/argument calls within a turn (default three executions).

These are portable policies, not every Grok Build recovery mechanism. The
upstream sampler's 15-attempt retry policy, server doom-loop headers/events,
authentication refresh, incomplete-output salvage, speculative two-pass
compaction, memory flush, and complete context-overflow input-fitting ladder
are not imported. The local repeated-tool guard is distinct from upstream's
server-driven generation-loop detection. Compaction is lossy; no upstream
segment database or transcript-retrieval service is implicitly installed.

## Durable sessions

Enable `nanocodex-durability`'s `xai` feature and a store feature as needed, then
apply `DurableAgentExt::durability` before building:

```rust,ignore
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};

let session = DurableSession::open(SqliteStore::open("agent.sqlite")?, "xai-session")
    .await?;
let (agent, events) = Xai::new(client, "grok-4.6")
    .durability(session).await?
    .build()?;
```

The adapter uses the shared owner fencing, compare-and-swap store, request IDs,
and effect receipts. Native checkpoints retain xAI conversation items, including
opaque reasoning and function associations. Reopening requires fresh explicit
credentials and tools; credentials are not embedded in checkpoints.

A completed identified request replays its retained result without provider or
tool calls and does not rewind later history. Reusing the ID with changed input
fails. An interrupted continuation reuses committed receipts. An effect recorded
as started without a receipt is uncertain and is not executed again
automatically. Unknown store-write outcomes require reopening; they are not
reported as success. This is receipt-based recovery, not a guarantee of
exactly-once external side effects.

Without a durable execution policy, identified prompt requests remain rejected;
in-memory session IDs and idle snapshots alone do not supply deduplication.
The facade's `durability` feature also enables its existing OpenAI dependency;
applications needing a provider-specific dependency graph can depend directly
on `nanocodex-xai` and `nanocodex-durability` with selected features.

## JavaScript and WASM

`Xai` is exported from `nanocodex/node`, `nanocodex/browser`, `nanocodex/worker`,
and `nanocodex/host`. It loads the native `Nanoxai` WASM class. Supply an explicit
model, API key or header callback, endpoint when using a proxy, and any named
host tools:

```js
import { Xai } from "nanocodex/node";

const agent = await Xai.create({
  model: "grok-4.6",
  auth: { apiKey: process.env.XAI_API_KEY },
});
const turn = agent.turn.prompt({ input: "Explain this project." });
const result = await turn.result();
console.log(result.finalMessage);
result.dispose();
turn.dispose();
await agent.session.shutdown();
agent.dispose();
```

Options include `instructions`, `thinking`, `contextWindowTokens`,
`autoCompactThresholdPercent`, `maxSteps`, `maxRetries`, `repetitionLimit`,
`compactionKeepTail`, `requestTimeoutMs`, host `tools`,
provider-owned `serverTools`, and paired `durability`/`durabilityId`.
`subagents` opts into shared task routing; `harnesses` explicitly supplies
alternate-family configurations. The host/browser entry points use the current
isolate. `maxRetries` defaults to three; zero disables retries of explicit rejections.
`repetitionLimit` defaults to three executions of an identical tool call per turn
and must be positive. `compactionKeepTail` defaults to eight native items; complete
user/tool boundaries can retain more, and even zero preserves the latest user
turn. These controls share the Rust backend's policies and never enable replay
of uncertain effects. Host authentication and tool callbacks remain outside serialized
configuration. The result's `.snapshot()` is unsupported; durable state belongs
to the selected store.

This library surface does not itself add an account connector, sign-in UI,
managed Worker provider, or product model-picker entry. It does not import
Grok Build's TUI, ACP server, configuration discovery, subscription credentials,
permission engine, sandbox, plugin system, or session database. Live provider
acceptance and complete Grok Build application parity are separate concerns.

## Upstream provenance and licenses

The source reference is the official
[`xai-org/grok-build`](https://github.com/xai-org/grok-build/tree/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8)
repository at commit `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`.
Its `SOURCE_REV` records monorepo revision
`559751fdcec02d413e4c57c8832ab275e4f44980`. These identify the inspected source;
they are not a floating Cargo dependency or a claim that the entire upstream
workspace was copied.

Relevant upstream boundaries are
[`xai-grok-sampling-types/src/conversation/responses.rs`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs)
for conversation conversion and
[`xai-grok-sampler/src/stream/responses.rs`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-sampler/src/stream/responses.rs)
for streamed Responses semantics. The upstream sampling/tool loop is in
`xai-grok-shell/src/session/acp_session_impl/turn.rs`
(`process_conversation_turn_inner`) and `tool_calls.rs` in the same directory.
`xai-grok-shell/src/agent/mvp_agent/` handles host setup; `xai-grok-agent` alone
is an agent-definition and prompt builder.

Upstream first-party source is Copyright 2023–2026 SpaceXAI, licensed under
Apache-2.0. The backend's [provenance notice](../crates/nanocodex-xai/UPSTREAM.md)
lists the exact adapted files and modifications; its
[retained license](../crates/nanocodex-xai/THIRD-PARTY-LICENSES) contains the
upstream copyright and complete Apache license. Upstream third-party and
vendored code retains its own licenses, as recorded in its
[`THIRD-PARTY-NOTICES`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/THIRD-PARTY-NOTICES).
The upstream Apache license does not replace those separate licenses.

## Validation

Run the public facade journey without default provider features:

```sh
cargo test -p nanocodex --no-default-features --features xai --test it xai:: -- --nocapture
```

It uses the public facade builder and an actual loopback HTTP/SSE server to
check two successive prompts, retained history, provider rejection and shutdown
fencing. Its synthetic request transcript is written to ignored
`output/xai/facade-requests.json`. Additional public-boundary journeys can be run with:

```sh
cargo test -p nanocodex-xai --features tools -- --nocapture
cargo test -p nanocodex-xai-tools -- --nocapture
cargo test -p nanocodex-durability --no-default-features --features xai,sqlite --test xai_recovery -- --nocapture
pnpm --dir js/nanocodex test:typecheck
node --test js/nanocodex/test/xai-wasm.test.mjs
```

The JavaScript runtime journeys require freshly generated Node/web WASM bindings
for this checkout. `js/nanocodex/scripts/test-xai-browser.mjs` additionally runs
an actual Chromium page and module Worker; it requires Playwright and a browser
installation (or the script's explicit module/executable environment overrides).

The SQLite recovery suite reopens the actual store after interrupted effects and
store-write failures, checks stale-owner fencing and terminal receipt replay,
and writes synthetic evidence under `output/xai-durability/`.
These fixtures replace only the external model provider; they do not establish
successful authentication or inference against the live xAI service.
