# Design exercise: create a managed session and admit its first turn

11 September 2026. This is a second design pass for PR #307, grounded in the
deployed [600-trial comparison](../output/cloudflare-agents-2026-09-11/README.md).
The combined operation below is a proposal, not a callable SDK method. This pass
implements only caller-owned `Agent.create` idempotency keys and fixes the existing
JavaScript examples to supply all four required settings fields.

## Decision

Keep the explicit session and `Turn` handles. Add a separate `Agent.createAndPrompt`
operation in a follow-up once durable recovery is implemented. Its first response
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
combined proposal needs a stronger immutable operation receipt described below.
See [SDK creation](../js/nanocodex/managed/Agent.mjs),
[public types](../js/nanocodex/managed/Agent.d.mts), and
[server creation/initialization](../js/managed/src/index.ts).

## Proposed TypeScript and JavaScript contract

Names in this section are proposed, not exported today. Reuse existing types
without adding another settings vocabulary or a provider-universal `Agent` type.

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
// PROPOSAL: this method is not implemented by this PR.
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

Propose `POST /v1/agent-runs` with `Idempotency-Key`, the existing creation body,
and `input`. A successful admission returns 201 with `agent_id`, `turn_id`,
`turn_idempotency_key` and `accepted_cursor`; replay returns 200 with the same
immutable receipt. Add an authenticated operation lookup by key for recovery
without resending a large prompt. It returns pending state or the retained receipt,
never a different session. No prompt text or credential belongs in a lookup URL.

1. Authenticate and authorize through the existing route policy. Validate the
   whole request before mutation, including model policy and input bounds.
2. On the account owner, atomically reserve the key with a canonical request hash
   and a snapshot of resolved definition/environment configuration. Same key and
   changed request is 409. Persist defaults explicitly. Concurrent reservations
   with the same request converge on one snapshot; a retry must not reread a
   template that was replaced or deleted after reservation.
3. Drive existing session ownership, credential provisioning, memory and
   initialization stages using that snapshot and stable identities. Store progress
   durably and arrange an alarm/reconciler so a disconnected client is not required
   to finish or clean up provisioning. There is no distributed SQL transaction
   spanning the account, session, memory and credential owners.
4. Once creation is committed, submit the first input through normal durable turn
   admission with its fixed ID/key. Store the accepted receipt before replying.
   Retry admission after a crash; existing conflict/deduplication rules apply.
5. Recover observation from the stored cursor and turn ID. An admission receipt
   does not promise successful environment setup, inference or tool execution.
   Those failures settle the accepted turn through normal terminal events.

Hash the validated caller request separately from the resolved snapshot. Hashing
only the resolved configuration would make retry behavior depend on later template
changes. Omitted defaults must have a pinned interpretation for that operation.
Do not hash ephemeral credentials, base URL, fetch implementations or observers.
Once admitted, return the original receipt even if session settings subsequently
change. Execution must capture first-turn settings at admission; current mutable
session settings are insufficient for that guarantee.

Retain an operation tombstone after session deletion so a retry cannot resurrect
work. A replay of a deleted operation returns 410. Scope keys to the authenticated
account and recheck current permissions on every request. Bound the journal per
account with an explicit quota and reject new reservations when full; never evict
an active operation silently. Define retention before release rather than claiming
unlimited deduplication. Expiry must leave enough identity information to reject
late reuse rather than unexpectedly launch another job.

| Failure point | Required observable result |
| --- | --- |
| Before reservation | No retained operation; retry normally. |
| After reservation, before committed creation | Pending operation; retry or reconciler resumes the same session. |
| After creation, before first admission | Same retained session, exactly one eventual durable admission. No second session. |
| After admission, before the client receives its receipt | Recover the same turn ID/cursor; do not send the prompt as a new turn or steer. |
| During setup/model execution | Normal terminal failure tied to the accepted turn, with partial usage if available. |
| Duplicate key with different input/configuration | 409, with no replacement session or extra admission. |
| After deletion | 410 from the operation tombstone; no resurrection. |
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

- Implement optional creation keys using the existing server contract; preserve
  default random keys and explicit two-step turn submission.
- Verify retries, separate SDK calls, invalid keys, body isolation, conflict
  propagation and public types. Existing server ownership tests remain relevant.
- Correct incomplete settings examples and ignore only opaque provider/ray IDs
  in the JSON spelling check, preserving measured data byte-for-byte.
- Leave the combined endpoint, operation journal, immutable admission snapshots
  and stronger replay guarantees for the follow-up described above.
