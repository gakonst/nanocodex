# Managed agent configuration and operations

Managed configuration is stored beside the session's durable state. Named
agent definitions and environment templates belong to the authenticated account.
They are immutable while retained: repeat the same PUT to retry, or use another
ID for a revision. Deleting a template does not change existing sessions, which
hold a resolved copy. Template IDs are 1–64 ASCII letters, digits, `_` or `-`.

```js
import { Agent } from "nanocodex/managed";

const client = { baseUrl: "https://managed.example", apiKey: process.env.NANOCODEX_API_KEY };
await Agent.definitions.put("reviewer-v1", {
  settings: { model: "gpt-5.6-luna", thinking: "low", reasoning_mode: "standard", fast_mode: false },
  instructions: "Review the supplied code. Return concrete findings.",
  prompt_cache: "explicit",
}, client);
await Agent.environments.put("review-workspace-v1", {
  network: { access: "disabled" },
  files: [{ path: "/brain/README.md", content: "Review fixtures live here." }],
  skills: [{ name: "review", instructions: "Check correctness, then tests. Cite filenames." }],
  setup_commands: ["mkdir -p /brain/outputs"],
}, client);
const agent = await Agent.create({
  ...client, definitionId: "reviewer-v1", environmentTemplateId: "review-workspace-v1",
  configuration: { instructions: "Review only the supplied diff and save findings in /brain/outputs." },
});
console.log(await agent.configuration(), await agent.environment());
```

Inline fields replace the corresponding definition field, including entire arrays
and objects. Existing `settings` in create options take precedence over a
definition's settings. An explicit `environmentTemplateId` cannot be combined
with an inline/definition environment. Model settings remain editable through the
existing settings API; other session configuration is fixed at creation.

`tools` is an optional exact allowlist of built-in managed tools. Omission retains
the default catalog; `[]` disables those tools. An explicit allowlist excludes
dynamic account tool providers and MCP catalogs. Unknown/unavailable names fail
runtime construction. Existing authorization checks still apply. Startup history and
memory retrieval obey the same allowlist and network restrictions; excluded account
catalogs are not refreshed. Previously retained bootstrap receipts remain part of
durable recovery; this does not rewrite existing conversation history. An empty application-tool list does not disable the
SDK's default subagent orchestration. Set `multi_agent: { enabled: false }` to
remove delegation, including explicit subagent extensions. Setting
`multi_agent: { enabled: true, max_concurrent_subagents: 2 }` enables bounded
delegation through the existing runtime. Explicit enablement without a limit uses
six concurrent children; omitting `multi_agent` retains legacy defaults. A disabled
configuration cannot also specify a concurrency limit. Limits must be positive
32-bit integers, matching the WASM runtime. These settings are immutable
for a session and can be stored in a named agent definition.

```js
const single = await Agent.create({
  ...client,
  settings: { model: "gpt-5.6-luna", thinking: "low", reasoningMode: "standard", fastMode: false },
  configuration: {
    instructions: "Use only the supplied data and return JSON.",
    tools: [],
    multi_agent: { enabled: false },
    environment: { network: { access: "disabled" } },
  },
});
try {
  console.log((await single.turn.prompt({ input: "Compute 17 * 19." }).result()).finalMessage);
} finally {
  await single.delete();
}
```

`output_schema` supplies a strict Responses JSON schema. `prompt_cache` selects
`implicit` or `explicit`; explicit mode marks the last developer text in the
stable prefix as a cache boundary. Incremental requests without that prefix make
no explicit cache write. Provider validation, minimum eligible prefix length,
cache pricing and expiry still apply. Provider controls are applied to every
managed Responses socket, including reopened connections and child sessions.

## Recovering session creation

`Agent.create({ idempotencyKey, ...options })` accepts an account-scoped key of
1–256 printable ASCII characters, excluding spaces. Persist the key and the
creation options before sending the request. The SDK retains the key through its
internal retries; another invocation with the same key can recover the same
retained session after a lost response or application restart. Omission still
creates a random key per invocation. This key is independent of the first turn's
idempotency key.

```js
// job.id and creation options must come from your durable job record.
const session = await Agent.create({
  ...client,
  idempotencyKey: `create:${job.id}`,
  settings: { model: "gpt-5.6-luna", thinking: "low", reasoningMode: "standard", fastMode: false },
  configuration: { tools: [], multi_agent: { enabled: false } },
});
const turn = session.turn.prompt({
  input: job.prompt,
  id: `job-${job.id}:first`, // Use a job ID compatible with the turn ID grammar.
  idempotencyKey: `first:${job.id}`,
});
console.log(await turn.result());
```

Use distinct keys for distinct jobs. Replaying creation checks retained settings
and configuration; it is not a general session lookup. Changed settings can cause
409, and templates are resolved again, so deleted or replaced templates can fail
replay. Once you have persisted the session ID, resume with `Agent.open(id, client)`
instead of recreating it. A deleted session is not resurrected by reusing its key.
There is no claim of exactly-once model execution or external tool side effects.

The [second API design exercise](MANAGED_AGENT_START_DESIGN.md) proposes a future
combined create-and-prompt operation. That operation is not implemented here.

## Environment execution and limits

The initial implementation targets the durable `/brain` workspace and its
embedded Bash interpreter. It installs text files and named `SKILL.md` packages,
then runs setup commands serially before constructing the agent. Setup status is
`uninitialized`, `running`, `ready`, or `failed`, with a completed-step count.
A failed or interrupted setup requires a new session; commands with an uncertain
outcome are never automatically rerun. Prepared workspaces survive runtime eviction.

Configuration is bounded to 1 MB of JSON, 50 files (262,144 characters each),
32 skills (65,536 characters each), and 32 setup commands (8,192 characters each).
Each setup command has a 30-second deadline. Paths must be canonical descendants
of `/brain`. This is configuration, not a VM image snapshot. Native apt/npm/pip
installation and arbitrary ZIP/plugin executables are not supplied by this
embedded-shell template API; existing native hands remain separately provisioned.

Network access is `enabled`, `disabled`, or `restricted` with 1–100 exact DNS
hostnames. Restricted requests pass through the existing destination/credential
gateway; redirects are not automatically followed by that gateway. Each shell
request is checked. Restricted sessions expose only the embedded shell, image
viewing and planning tools (optionally narrowed by `tools`). They do not expose
remote hands, browser/search, MCP, SSH or dynamic account tools that could bypass
the policy. Model inference and account control-plane operations are not covered
by the tool network policy. Inline environment initialization requires `tools:use`.

## Webhooks

```js
const { secret } = await agent.webhook.create("https://backend.example/hooks/nanocodex");
// Store the secret now: GET never returns it.
const event = await Agent.verifyWebhook(incomingRequest, secret);
// Transactionally deduplicate event.id before scheduling downstream work.
```

One HTTPS endpoint can be registered per session. Delivery starts with subsequent
lifecycle events: `turn_accepted`, `turn_completed`, `turn_failed`,
`turn_cancelled`, and `stream_failed` (and `agent_created` if an endpoint was
already retained). The payload contains IDs, cursor and timestamp, never prompts,
outputs or credentials. Read current session/turn state after receipt.

The durable outbox is written in the lifecycle event transaction. Delivery is
at least once, can arrive out of order, and uses a stable `webhook-id`. The
signature is `v1,<hex HMAC-SHA256>` over
`<webhook-id>.<webhook-timestamp>.<raw-body>`, with the secret's UTF-8 bytes.
`Agent.verifyWebhook` checks that signature and a five-minute timestamp window;
it does not persist deduplication state. Retries refresh the signature timestamp.

Delivery uses public-only Worker fetch, manual redirects and a 10-second timeout.
Failed attempts back off exponentially, stop after 12 attempts, and remain
inspectable through `agent.webhook.get()`. The 1,000 most recent successful
receipts are retained. Delete the endpoint before replacing it; deletion discards
pending deliveries. An already-issued HTTP request cannot be recalled. Session
deletion discards the endpoint/outbox and does not emit a deletion notification.

## Usage and trace inspection

The account app's **Inspect** button opens turn accounting, model request details,
child IDs, immutable outputs, and the existing cursor-addressed event timeline.
No new tracing system is introduced. The same account ownership checks protect
these routes and conversation history.

- `agent.usage({ after })` returns up to 256 lifecycle rows, with existing
  `TurnUsage` at completion. `null` means unknown, not zero.
- `agent.requests({ after, agentId })` returns up to 256 model/warmup/compaction
  records, deduplicated by response ID. It retains provider usage and first-event,
  first-output and completion timings. `agentId` is the recorded numeric child
  ID as a string, or `root` for an untagged root event.
- Use the last returned cursor when `has_more` is true. These new projections
  cover events emitted after rollout; the existing event/history APIs remain
  authoritative for older sessions. The inspector labels bounded pages.
- Per-request usage is detail within turn accounting. **Do not add request usage
  to turn totals.** Retries, warmup and compaction may incur usage. Missing values
  stay unknown; no provider billing reconciliation is implied.

## Published artifacts

At successful root-turn completion, files beneath `/brain/outputs/` are copied
into an immutable per-turn catalog. Paths, SHA-256 digests, sizes and bytes are
retained in the session's existing SQLite storage, independently of mutable
workspace files and native-hand lifetime. Later turns receive separate versions.

```js
const page = await agent.artifacts.list({ turnId: "turn-id" });
const bytes = await agent.artifacts.download(page.data[0].id);
```

A publication allows 50 files, 1 MB per file and 10 MB total (at most 100 listed
entries including directories). Its catalog commit is atomic. Publication failure
is reported separately and does not convert a successful model turn into failure.
The bytes are captured while the completed managed turn is still owned, before
its terminal receipt is committed. A previously published turn is not overwritten.
A failed publication is not retried from a later, potentially changed workspace.
The unfiltered catalog shows the latest 256 files; exact-turn listing is complete.
Downloads force attachment/octet-stream and private no-store caching. Deleting
the session removes its artifacts. New configured sessions, endpoints and published
artifacts currently block portability export rather than silently dropping them.

## HTTP application-tool results

`agent.requiredActions.list()` returns pending calls from the existing session
Hosted Tools broker. `agent.requiredActions.submit(callId, outcome)` uses the same
protocol validator, pinned attachment, deadline, output budget, terminal receipt
and conflict checks as WebSocket results. Catalog registration/lease ownership
still uses the existing Hosted Tools attachment protocol; this is an HTTP result
facade, not a second tool execution system. The pinned attachment must remain
active. These operations require account authority and `tools:use`.

## Native Responses lanes

`multiplex` from `nanocodex/browser/transport` adapts a caller-owned, authenticated
Responses WebSocket into named lanes. Pass a lane from a `createWebSocket`
callback to the browser/host runtime. Pool only within a single credential and
policy boundary. Pooling is opt-in; managed relay deployments are not silently
switched to a shared provider connection.

The adapter stamps `stream_id`, routes interleaved events, isolates lane errors,
and permits at most 32 distinct names over a connection's lifetime. OpenAI queues
beyond 16 active responses. The caller owns lineage and recovery: preserve
`previous_response_id`, wait for a cross-lane fork's `response.in_progress` before
advancing a `store=false` parent, and replay full context after cache loss.
Lanes accept `response.create`; native steering is not yet integrated with pooled
lanes. Closing one lane releases its local subscriber and does not cancel server
work or sibling lanes; `pool.close()` closes the physical connection. Connections
must be replaced at the provider's 60-minute limit.

Run `OPENAI_API_KEY=... node js/nanocodex/scripts/response-lanes.bench.mjs` for six
bounded Luna smoke generations: independent lanes, fork/continuation, and explicit
cache write/read. It records setup separately, correctness, latency and provider
usage, with no tools or stored responses. It is not a tail-latency benchmark.

Official protocol references: [WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode),
[prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching),
[Agents configuration](https://developers.openai.com/api/docs/guides/agents-api/configuration).
