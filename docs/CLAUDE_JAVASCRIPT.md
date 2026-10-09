# Explicit Claude JavaScript runtime

`Agent.create({ harness: "claude", ...options })` runs the Rust Messages backend with the
**same** `nanocodex-durability` store, fencing, admission, effect receipts and
terminal replay machinery. It does not route through OpenAI Responses, launch
Claude Code, or install the Codex native catalog. Both harnesses expose only
`exec` and `wait`; supplied capabilities are called inside shared Code Mode.

It returns the same Agent as a Codex session (Node, host, and browser SDKs);
`Claude.create(options)` is the equivalent constructor on the Claude-only
`nanocodex/worker` entry point. Browser mixed-family sessions run
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
Children retain their native transcripts in the shared task tree. Durable roots
restore journaled child identities, status and committed snapshots after reopen;
hosts must reattach their capabilities and authorization. Interrupted work still
requires reconciliation, and process-local JavaScript cells cannot resume.

Hosted managed threads use the same canonical task tree and Code Mode contract;
the explicit SDK recipes above do not configure hosted account routing.

```js
import { Agent } from "nanocodex/node";
import { createMemoryDurabilityStore } from "nanocodex/durability";

const durabilityId = "claude-example";
const options = {
  harness: "claude",
  model: "claude-sonnet-5-5",
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
const agent = await Agent.create(options);
const turn = agent.turn.prompt({ input: "Add 19 and 23.", id: "sum-request" });
await turn.accepted();
const result = await turn.result();
console.log(result.finalMessage);
result.dispose();
turn.dispose();
await agent.session.compact();
await agent.session.shutdown();

// Reattach auth and handlers, which are not serialized in checkpoints.
const reopened = await Agent.create(options);
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

The explicit Claude tool array forms the nested Code Mode catalog. The model
sees only `exec` and `wait`, and calls capabilities with `tools.<name>(input)`
inside JavaScript. Explicit `strict` and deferred-definition flags must be
preserved or rejected, never silently ignored. Workspace, shell, web and MCP
capabilities require supplied handlers; subagents require an enabled task tree.
The injected handler owns
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
