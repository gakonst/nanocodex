# JavaScript backends and the Claude runtime

## Node agents with automatic local tools

```js
import { Agent, Backend } from "nanocodex/node";

const backend = Backend.claude({ apiKey: process.env.ANTHROPIC_API_KEY });
// Or: Backend.codex({ apiKey: process.env.OPENAI_API_KEY })
const agent = await Agent.create({ backend });
// Optional: { backend, workspace: "/path/to/project", model: "..." }
try {
  const turn = agent.turn.prompt({ input: "Read README.md and describe this project." });
  const result = await turn.result();
  console.log(result.finalMessage);
  result.dispose();
  turn.dispose();
} finally {
  await agent.session.shutdown();
}
```

The backend selects the provider and installs the corresponding local tool
catalog without a caller tool array. Node defaults to `process.cwd()`, Codex
`gpt-6-astra`, and Claude `claude-opus-5-5`. `workspace` selects an existing
native directory; `model` and `thinking` remain optional agent settings.
A factory returning either backend can pass its result to the same `Agent.create`
call. Backend descriptors expose a readonly `kind`; credentials stay in private
SDK state and are not serialized with the descriptor.

Codex installs native process execution (`exec_command`, `write_stdin`), the
canonical Rust `apply_patch` planner, local `view_image`, `update_plan`, workspace
file operations, Code Mode, tool discovery, and the shared subagent lifecycle.
Claude installs `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Bash`, `NotebookEdit`,
`TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, and `TodoWrite`, plus the shared
subagent lifecycle. These preserve Claude's native input and result contracts.
The Node file adapters support bounded UTF-8 text; PDF/image reading is not
installed. `Grep` uses the embedded Rust regex matcher. Bash runs foreground
commands on POSIX hosts; background and sandbox-bypass flags are rejected. Task
boards and todos use the shared Rust implementation and durable checkpoints.
Claude also installs its native nested `WebSearch` Messages workflow using the
selected provider authentication.
The existing WASM agent loops execute both families. Codex web search, image
generation, remote MCP, and account integrations need their own service
capabilities; a model API key does not configure those services.

Local commands run with the Node process's host permissions. The workspace is
the initial working directory, not an OS sandbox. Use an isolated process or
container when the task needs isolation. Shutdown cancels work and joins owned
process cleanup. The browser and generic Web API host do not have Node's native
filesystem or process capabilities: this convenience form requires
`nanocodex/node`; their explicit constructors remain available.

`Backend.codex` accepts `apiKey`, optional `apiBaseUrl`, `websocketUrl`, and
`websocketWarmup`. `Backend.claude` accepts `apiKey` and optional Messages
`endpoint`. Unknown or conflicting options and models naming the other provider family are rejected. `backend` cannot be
combined with `harness`, `auth`, `transport`, `tools`, or `filesystem`; use the
explicit constructors below for a fine-grained host configuration.

## Explicit host configuration

The additive `Claude.create` constructor runs the Rust Messages backend with the
**same** `nanocodex-durability` store, fencing, admission, effect receipts and
terminal replay machinery. It does not route through OpenAI Responses, launch
Claude Code, or install the Codex catalog.

`Agent.create({ harness: "claude", ...options })` also selects this native
backend in the Node, host, and browser SDKs. Browser mixed-family sessions run
in the calling isolate so their explicit tool handlers remain callable.

Enable the canonical task tree with `subagents: { maxConcurrency: 6 }` on a
Claude root. Supply `harnesses.codex` to grant that tree an explicit Responses
transport and tools. On a Codex root, supply `harnesses.claude` with explicit
Claude authentication and native tools. These capability recipes are ephemeral;
they cannot contain session IDs, durability, nested harnesses, or resume state.
Alternate Codex capabilities accept API and host-managed transports.

```js
import { Agent, Subagents, Transport } from "nanocodex/host";

const agent = await Agent.create({
  harness: "claude",
  model: "claude-sonnet-4-6",
  auth: { apiKey: anthropicKey },
  subagents: { maxConcurrency: 6 },
  harnesses: {
    codex: {
      transport: Transport.openAi({ apiKey: openAiKey, stateless: true }),
      model: "gpt-6.1-sol",
    },
  },
});
const child = await Subagents.spawn(agent, {
  harness: "codex", model: "sol", thinking: "low",
  role: "Reviewer", task: "Return a concise assessment.",
  outputSchema: { type: "string" },
});
const report = await Subagents.wait(agent, { agentIds: [child.agent_id] });
await agent.session.shutdown();
```

Both families use the same `spawn_agent`, `wait_agent`, messaging, result
submission, and subtree lifecycle. Model names belong to their selected family:
for example, `sol` selects Codex and `sonnet` selects Claude. Omitted family
inherits the parent; selecting another family uses that family's defaults.
Within a Claude tree, `{ model: "sonnet", ... }` selects Sonnet without repeating
`harness: "claude"`. `Subagents.spawnMany` inherits the parent's family, model
and thinking for the entire batch and rejects any overrides before admitting
children. Use `Subagents.spawn` to select a child's family or model explicitly.
Children retain their native transcripts and are reusable only while the root
runtime lives. A durable root's reopen does not recreate a child task tree.

Hosted managed threads retain their existing provider selection and Claude
`Task` capabilities; the explicit SDK recipes above do not configure hosted
account routing.

```js
import { Claude } from "nanocodex/node";
import { createMemoryDurabilityStore } from "nanocodex/durability";

const durabilityId = "claude-example";
const options = {
  model: "claude-sonnet-5",
  auth: { apiKey: process.env.ANTHROPIC_API_KEY },
  instructions: "Use the explicitly supplied tools. Preserve the result.",
  durability: createMemoryDurabilityStore(durabilityId),
  durabilityId,
  tools: [{
    name: "fixture_sum",
    description: "Add two integers.",
    inputSchema: {
      type: "object",
      properties: { a: { type: "integer" }, b: { type: "integer" } },
      required: ["a", "b"],
      additionalProperties: false,
    },
    handler: ({ a, b }, { sessionId, turnId, callId }) => {
      // Use these stable identities to reconcile external side effects.
      return String(a + b);
    },
  }],
};
const agent = await Claude.create(options);
const turn = agent.turn.prompt({ input: "Add 19 and 23.", id: "sum-request" });
await turn.accepted();
const result = await turn.result();
console.log(result.finalMessage);
result.dispose();
turn.dispose();
await agent.session.compact();
await agent.session.shutdown();

// Reattach auth and handlers, which are not serialized in checkpoints.
const reopened = await Claude.create(options);
const replay = reopened.turn.prompt({ input: "Add 19 and 23.", id: "sum-request" });
const replayResult = await replay.result();
console.log(replayResult.finalMessage);
replayResult.dispose();
replay.dispose();
await reopened.session.shutdown();
```

The example requires an explicitly authorized provider credential and model;
real provider execution and compaction may incur charges. The memory store
demonstrates handle reopen, not process-crash persistence. Use
a persistent existing `DurabilityStore` adapter for crash recovery. Terminal
replay requires the identical request ID **and input**. Changed input under an
existing ID is a conflict. An unfinished effect without a committed receipt
remains **at least once**; external hosts must deduplicate or reconcile
consequential actions. Dispatch is not proof of a committed result.

## Placement and authentication

Node executes WASM in the current process. Browser Claude executes in the
**current Web API isolate**, not the implicit module Worker created by Codex's
browser Agent. A caller-owned Worker can supply the WASM module explicitly.
Browser credentials remain accessible to that browser host: do not distribute
account-wide secrets to an untrusted frontend. The SDK does not install account sign-in UI. The managed platform adds private
subscription connection, authoritative model selection and egress around this same
WASM runtime; see [managed Claude](CLAUDE_MANAGED.md).

Supply exactly one `auth: { apiKey }` or `auth: { headers: async () => ({
authorization: "Bearer ..." }) }`. The callback owns authorized credential
acquisition, rotation and private storage. It resolves for outbound requests,
not terminal replay. This callback is **not** a JS PKCE login implementation and has no automatic
401-recovery callback; the host must supply a usable credential before dispatch.
Authentication failures are detail-free to avoid leaking credentials. Secrets
and handlers stay in host closures outside serialized config and checkpoints.

An explicit Messages `endpoint` may be supplied.
`compatibilityProfile: "subscription"` applies the measured public subscription
request profile to that endpoint; it neither grants subscription authority nor
borrows local Claude Code credentials. Fresh native OAuth login, live native
subscription admission and synthetic JS tests remain distinct. See
[authentication provenance](claude-authentication.md).

## Tools and lifecycle boundaries

On the explicit low-level path, only the supplied Claude tool array is advertised. Explicit `strict` and
deferred-definition flags must be preserved or rejected, never silently ignored. That low-level path installs no ambient workspace, shell,
web, MCP, Code Mode or subagent tools. The injected handler owns
permission, isolation, resource bounds and external idempotency; stable call
identities and schema validation are not an OS sandbox. Thrown handler errors
become detail-free error results. Deliberate error output is tool-result data.

Prompt/Turn results, event watching, compaction and shutdown use the common JS
lifecycle. `agent.dispose()` detaches this client; already accepted work retains
its host/auth/store routes until terminal settlement, even without a caller
waiting for the result. `session.shutdown()` explicitly cancels and joins work.
Turn cancellation uses the actual Rust lifecycle identity, including anonymous
non-durable turns, rather than guessing from whichever turn is currently active.
A host abort signal is cooperative and is never proof an external effect was
rolled back; durable unknown outcomes still require reconciliation. Provider-native checkpoints are managed by the shared store, not
OpenAI `SessionSnapshot` objects. Unsupported snapshot export, fork, runtime
model switches and other OpenAI-specific operations fail explicitly. Whole-JSON
checkpoint storage and complete host-tool/product parity remain open limits.
See [runtime coverage](CLAUDE_RUNTIME.md) and the [tool matrix](CLAUDE_TOOL_MATRIX.md).
