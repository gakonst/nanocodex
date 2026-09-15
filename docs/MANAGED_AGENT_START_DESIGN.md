# Design exercise: create a managed session and admit its first turn

11 September 2026. This is a second design pass for PR #307, grounded in the
deployed [600-trial comparison](../output/cloudflare-agents-2026-09-11/README.md).
The selected operation is now exported as `Agent.createAndPrompt` and served by
`POST /v1/agent-runs`. The branch also implements caller-owned `Agent.create`
idempotency keys and fixes the existing JavaScript examples to supply all four
required settings fields.

## Decision

Keep the explicit session and `Turn` handles. The separate `Agent.createAndPrompt`
operation composes the existing durable creation and admission owners. Its response
should acknowledge admission and return both handles; observing or cancelling the
turn remains a separate operation. Do not overload `Agent.create` to sometimes
return a different type or silently run work.

OpenAI accepts initial input with session creation. Later session input starts a
turn when idle and steers it when busy. We should adopt the convenient first
request while preserving Nanocodex's explicit distinction between admission and
steering. This does not establish equivalent provider recovery guarantees.
[OpenAI create reference](https://developers.openai.com/api/reference/resources/beta/subresources/agents/subresources/sessions/methods/create),
[session guide](https://developers.openai.com/api/docs/guides/agents-api/sessions)

## Compare the alternatives

| Shape | Requests before admission | Return value | Tradeoff |
| --- | --- | --- | --- |
| Existing `create`, then `turn.prompt` | Two | Session, then `Turn` | Explicit and already recoverable with two persisted keys; caller owns the gap between operations. |
| Client-only `run` helper | Two | Usually final output | Easier example but no network saving; hides admission, cancellation and intermediate recovery unless it returns both handles. |
| `create({ input })` overload | One with server support | Session or session-plus-turn | Couples creation to execution and complicates types, overloads and retries for existing callers. |
| Separate `createAndPrompt` | One with server support | `{ agent, turn }` after admission | Clear lifecycle and compatible types; requires a durable server workflow across existing owners. Recommended. |

"One request" means one client mutation before admission, not one total request:
event subscriptions, state reads and cancellation still use their own endpoints.
Session setup and credential provisioning remain real work.

## Existing JavaScript recovery path

This is available after this PR update. The application must persist the job ID,
prompt and creation options before the first request. The ID below is deliberately
compatible with both creation-key and turn-ID validation.

```js
import { Agent } from "nanocodex/managed";

const client = {
  baseUrl: process.env.NANOCODEX_MANAGED_URL,
  apiKey: process.env.NANOCODEX_API_KEY,
};
const job = { id: "invoice-42", prompt: "Compute 17 * 19 and return JSON." };
const agent = await Agent.create({
  ...client,
  idempotencyKey: `create:${job.id}`,
  settings: {
    model: "gpt-5.6-luna", thinking: "high",
    reasoningMode: "standard", fastMode: false,
  },
  configuration: {
    instructions: "Use only the supplied input.",
    tools: [],
    multi_agent: { enabled: false },
    environment: { network: { access: "disabled" } },
  },
});
// Persist agent.id; future jobs should use Agent.open rather than create replay.
const turn = agent.turn.prompt({
  input: job.prompt,
  id: `${job.id}:first`,
  idempotencyKey: `first:${job.id}`,
});
console.log(await turn.accepted());
console.log((await turn.result()).finalMessage);
// Delete only after the application has durably stored the result.
```

The SDK previously generated a new creation key for each invocation, although
retries inside that invocation shared a key. A process restart lost that recovery
identity. The optional public key now reaches the existing HTTP header without
entering the JSON body or leaking into subsequent handle operations. Invalid keys
fail locally; 409 conflicts propagate rather than creating a second session.

This is not a permanent lookup API. The current server resolves templates before
checking session initialization, and compares retained configuration and current
settings. Deleting/replacing a template or changing session settings can prevent
creation replay. Persist the session ID once known and use `Agent.open`. The
combined endpoint inherits this template-replay limitation. A separate account
operation journal would be required to pin resolved snapshots permanently.
See [SDK creation](../js/nanocodex/managed/Agent.mjs),
[public types](../js/nanocodex/managed/Agent.d.mts), and
[server creation/initialization](../js/managed/src/index.ts).

## TypeScript and JavaScript contract

The exported names reuse existing types without adding another settings vocabulary
or a provider-universal `Agent` type.

```ts
type CreateAndPromptOptions = Omit<CreateOptions, "idempotencyKey"> & Readonly<{
  idempotencyKey: string; // Required, persisted operation identity.
  input: PromptOptions["input"];
  signal?: AbortSignal; // Stop observing admission; not a durable cancellation.
}>;

declare function createAndPrompt(options: CreateAndPromptOptions): Promise<
  Readonly<{ agent: Agent; turn: Turn }>
>;
```

```js
const { agent, turn } = await Agent.createAndPrompt({
  ...client,
  idempotencyKey: "invoice-42:first-run",
  settings: {
    model: "gpt-5.6-luna", thinking: "high",
    reasoningMode: "standard", fastMode: false,
  },
  configuration: { tools: [], multi_agent: { enabled: false } },
  input: "Compute 17 * 19 and return JSON.",
});
const turnId = await turn.accepted(); // Already acknowledged by the response.
const result = await turn.result();
// Another turn remains explicit: agent.turn.prompt({ input: "Now multiply by 2." }).
// A correction remains explicit: turn.steer({ input: "Use 23 instead of 19." }).
```

Require the key for the combined mutation: a returned Promise cannot expose the
server's session ID if the caller loses the admission response. Derive stable,
separate session and first-turn identities from the account and operation key on
the server; do not hash the prompt as an identity or trust client-supplied ownership.
The returned `Turn.idempotencyKey` must be the canonical key used by admission.
No implicit deletion when observation aborts. Retry the same operation to recover
its handles, then explicitly cancel or delete if that is the application's intent.

## Durable workflow, not a cross-object transaction

`POST /v1/agent-runs` accepts `Idempotency-Key`, the existing creation body, and
`input`. A successful admission returns 201 with `agent_id`, `turn_id`,
`turn_idempotency_key` and `accepted_cursor`; exact replay returns 200 with the
same receipt.

1. Authenticate and authorize through the existing route policy. Validate the
   complete creation body and prompt before mutation.
2. Derive stable, account-scoped session, turn and turn-key identities from the
   caller's operation key. Prompt contents and credentials are never identities.
3. Drive the existing idempotent session creation workflow. Its persisted
   preparation and cleanup watchdog recover interrupted credential provisioning;
   retained settings and configuration reject changed same-key requests.
4. Submit the first input through normal durable turn admission with the derived
   ID and key. Its retained request hash rejects a changed prompt and its durable
   turn view reconstructs an admission receipt after a lost response.
5. Return the existing Agent and Turn handles over that receipt. An admission does
   not promise successful setup, inference or tool execution; those settle through
   normal terminal events.

There is no distributed transaction across the session, memory and credential
owners. Keyed replay resumes at the first unfinished idempotent stage. Session
deletion retains the existing creation tombstone and returns 409 on reuse, so it
cannot resurrect work. Every retry reauthenticates and rechecks current authority.

The remaining hardening opportunity is an account-owned operation journal with
an immutable resolved template snapshot and lookup-by-key endpoint. The current
implementation resolves named templates again during creation replay, just like
`Agent.create`; callers should persist the returned session ID and use `Agent.open`
after admission. There is no claim of unlimited idempotency retention.

| Failure point | Required observable result |
| --- | --- |
| Before session preparation | No retained operation; retry normally. |
| After preparation, before committed creation | Retry or the cleanup watchdog resolves the same session. |
| After creation, before first admission | Same retained session, exactly one eventual durable admission. No second session. |
| After admission, before the client receives its receipt | Recover the same turn ID/cursor; do not send the prompt as a new turn or steer. |
| During setup/model execution | Normal terminal failure tied to the accepted turn, with partial usage if available. |
| Duplicate key with different input/configuration | 409, with no replacement session or extra admission. |
| After deletion | 409 from the creation tombstone; no resurrection. |
| Client aborts or its event stream disconnects | Work continues; explicit cancellation remains necessary. |

Exactly one durable admission is the target. Exactly-once model requests or
external tool effects are not implied by either operation deduplication or SSE
replay; ambiguous upstream outcomes still need their own handling.

## Performance hypothesis and release criteria

The measured post-change creation median was about 1.424 s; warm Nanocodex TTFT
was 1.286 s versus 7.596 s for its fresh-session cohort. These are different
measurements/cohorts, not additive components or a prediction of savings. Combining
the request can remove one client round trip and repeated routing/authentication.
It does not remove all creation time, make cold starts warm, or reduce model
reasoning time. More journal writes may offset the saving.
[Measurement methodology and results](../output/cloudflare-agents-2026-09-11/README.md)

Benchmark combined and existing paths on the same deployed runtime, client region,
model, thinking level, service tier, tools, delegation policy and workload. Alternate
order, distinguish cold and warm sessions, and report failures alongside correct
results. Use enough repetitions per cell for uncertainty estimates; the earlier
two repetitions per fine cell are insufficient for tail-latency claims.

Measure client start to admission, first nonempty root text, last root text and
terminal result; server provisioning/admission phases; root and child token usage;
DO duration, SQL writes/reads, Worker CPU and request counts. Keep clocks local to
their origin: server phase durations can be correlated but must not be subtracted
from client timestamps. A faster receipt with unchanged TTFT is an admission
improvement, not faster generation. Compare correctness before pooling latency.

Before release, inject dropped receipts and crashes at every workflow stage in
Workers tests. Verify parallel same-key calls, changed input, template deletion,
settings changes, setup failure, cross-account access, cancellation and tombstones.
Exercise delayed first events and stale stream owners in the actual runtime.
Require no duplicate durable admissions or orphaned resources and demonstrate a
measurable end-to-end benefit without a material error, correctness or cost
regression. This document contains no new live measurements or claimed speedup.

## Scope of this PR update

- Implement optional creation keys and combined `createAndPrompt`; preserve
  default random keys and explicit later-turn submission.
- Verify retries, lost receipts, exact replay, changed-input conflicts, invalid
  requests, authority checks, body isolation, public types and accepted-handle reuse.
- Correct incomplete settings examples and ignore only opaque provider/ray IDs
  in the JSON spelling check, preserving measured data byte-for-byte.
- Leave the separate operation journal, immutable template snapshots and lookup by
  key for future hardening; the shipped endpoint composes existing durable owners.
