# GPT-6 Astra readiness

Nanocodex recognizes the provider model ID `gpt-6-astra`. Callers can explicitly
create Astra agents after their account is entitled. Brokered ChatGPT connections
query the authenticated Codex model catalog and default new conversations to Astra
only when the exact model is listed. Existing managed agents keep their retained
settings, while API-key and sponsored Luna traffic retain their existing defaults.

This support is based on OpenAI's current contracts:

- [GPT-6 Astra model reference](https://developers.openai.com/api/docs/models/gpt-6-astra)
- [GPT-6 Astra model guide](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra)
- [Async tool calling](https://developers.openai.com/api/docs/guides/async-tool-calling)
- [Mid-turn steering](https://developers.openai.com/api/docs/guides/steering)
- [Change reasoning mid-conversation](https://developers.openai.com/api/docs/guides/reasoning#change-reasoning-mid-conversation)
- [Misalignment monitoring](https://developers.openai.com/api/docs/guides/safety-checks/misalignment-monitoring)
- [Fast mode](https://openai.com/api-fast-mode/)
- [Codex bundled Astra catalog](https://github.com/openai/codex/commit/ed391d4dd21396715b66c278e6b451897672c93c)
- [Codex Astra Bedrock support](https://github.com/openai/codex/commit/1f7b99922a285f748ef323a53d421fd67ef8438d)
- [Codex durable reasoning updates](https://github.com/openai/codex/commit/0d502a423031396a8d11c096e5b9f1cb0d30b3d0)
- [Codex misalignment stop policy](https://github.com/openai/codex/commit/785ecd7452f87c7eb731fbb73892185cbdd9d5f9)

## Model contract

| Contract | Nanocodex behavior |
| --- | --- |
| Model ID | Accepts and serializes `gpt-6-astra` across Rust, WASM, JS, managed settings, and retained Durable Object state. |
| Reasoning | Accepts `low`, `medium`, `high`, `xhigh`, and `max`. Rejects Astra with `none` or Pro mode before dispatch, including dynamic settings and subagent overrides. Astra requests omit `reasoning.mode` and the default reasoning summary, matching the Codex Astra request policy. Nanocodex retains its existing `high` effort default; Codex's bundled Astra catalog defaults to `low`. |
| Context | OpenAI documents a 1,050,000-token API context window. Nanocodex follows the current Codex catalog: 272,000 by default, configurable to 872,000, with explicit provider compaction at 90% of the configured prompt budget. |
| Output | The provider documents a 128,000-token maximum. Nanocodex does not raise its own output limit beyond caller/provider limits. |
| Knowledge cutoff | April 30, 2026; this is documentation only and does not affect request encoding. |
| Base token rates | Estimates $10 input, $1 cached input, $12.50 cache write, and $50 output per million tokens. |
| Long-context rates | For more than 272,000 input tokens, estimates 2x input/cache rates and 1.5x output rates for the whole request. |
| Fast mode | Keeps the account-app Astra default off. Astra standard requests explicitly send `service_tier: "default"` so a project Fast default cannot change their accounting. Fast requests use Codex's accepted compatibility value `priority`; requested-tier cost estimates label Astra fast mode as `fast`, while GPT-5.6 retains `priority`. Deployments using EU data residency must not enable Astra fast mode. |

OpenAI currently describes Astra as rolling out first through its Trusted Access
Program, with API and Plus, Pro, Business, and Enterprise access following in the
coming days. That announcement is not an entitlement API and must not be used as
a client-side availability signal.

OpenAI's current short-context rates per million tokens are:

| Model | Tier | Input | Cached | Cache write | Output |
| --- | --- | ---: | ---: | ---: | ---: |
| GPT-5.6 Sol | Standard | $4 | $0.40 | $5 | $20 |
| GPT-5.6 Sol | Fast | $8 | $0.80 | $10 | $40 |
| GPT-5.6 Terra | Standard | $2 | $0.20 | $2.50 | $12 |
| GPT-5.6 Terra | Fast | $4 | $0.40 | $5 | $24 |
| GPT-5.6 Luna | Standard | $0.20 | $0.02 | $0.25 | $1.20 |
| GPT-5.6 Luna | Fast | $0.40 | $0.04 | $0.50 | $2.40 |
| GPT-6 Astra | Standard | $10 | $1 | $12.50 | $50 |
| GPT-6 Astra | Fast | $20 | $2 | $25 | $100 |

For every supported model, requests above 272,000 input tokens use 2x input,
cached-input, and cache-write rates and 1.5x output rates across the whole request.
Fast mode then uses the corresponding fast rates. Nanocodex's estimator applies
these thresholds to Sol, Terra, Luna, and Astra.

The provider prices Batch and Flex at 50% of Standard and Fast at 2x the
applicable short- or long-context rates. Nanocodex currently estimates only its
standard and fast-mode request paths. Fast mode has no latency SLA, and Astra
accepts neither `fast` nor `priority` with EU data residency. Tool-specific fees
remain separate from token estimates.

## Provider surface

Astra accepts text input and output plus image input. Audio and video are not
supported. Streaming, function calling, and Structured Outputs are supported;
fine-tuning is not. OpenAI exposes the model through Responses and Chat
Completions, but Astra tool calling requires Responses. The current model page
lists only the `gpt-6-astra` alias and no dated snapshot.

Responses tools listed for Astra are web search, file search, image generation,
code interpreter, hosted shell, apply patch, skills, computer use, MCP, and tool
search. Nanocodex continues to expose only tools owned by its existing runtime
and adapter contracts; model support does not implicitly enable a tool.

Published API limits currently start at 500 RPM, 500,000 TPM, and a 1,500,000
token batch queue for Tier 1, rising through 15,000 RPM, 40,000,000 TPM, and a
15,000,000,000 token batch queue for Tier 5. Free tier is not supported. These
are account-tier provider limits, not Nanocodex defaults.

## New Astra protocol capabilities

### Async tool calling

`ToolDefinition::with_async_execution()` emits `async: true` for application-run
function or custom tools. Async call items retain the provider's `async` marker in
history so full-history recovery does not silently change the call contract. The
application still owns the pending job and must return its output with the original
`call_id`.

The account app does not mark its tools async by default. Its current tool runtime
waits for tool completion and has no durable pending-job registry. Hosted tools,
programmatic tool calling, multi-agent parallel calls, and provider-hosted tools
therefore retain their existing synchronous policy.

### Mid-turn steering

Nanocodex's public steer operation remains durable across Worker restart and is
injected at the next model-call boundary. It does not yet send Astra's
`response.steer` frame while a provider response is streaming. Consequently, it
does not rely on connection-local pending steers or claim the provider's automatic
continuation semantics. Native provider steering requires a transport control path
that can write while the response stream is being consumed, plus handling for
`response.steer.accepted`, `response.steer.failed`, `response.incomplete` with
reason `steered`, required tool input, and disconnect recovery.

### Reasoning configuration updates

The Responses item model recognizes and serializes `configuration_update` with a
reasoning effort. Nanocodex does not automatically translate `setThinking` into
this item: the provider supports updates only for Astra in standard single-agent
mode and forbids combining them with automatic compaction, automatic truncation,
or standalone `/responses/compact` histories. Nanocodex uses explicit automatic
compaction, so changing this behavior requires a coordinated conversation-state
policy rather than a request-only rewrite.

### Misalignment monitoring

Provider errors with code `misalignment_policy_violation` are terminal and receive
a stable error class. Nanocodex does not automatically retry or resume the stopped
conversation. Earlier external actions are not rolled back.

OpenAI's automatic stop applies to Responses conversations whose context is
preserved with persisted reasoning, WebSockets, or OpenAI compaction. Stateless
Responses requests are monitored and can produce project alerts but are not
automatically stopped; Chat Completions is not covered by this monitoring system.
Nanocodex still handles the stop code on any transport where the provider returns
it, including a top-level streaming error after output has begun.

Project-level `safety.alert.created` webhooks and `GET /v1/safety/alerts/{id}` use
API-project credentials and belong in an operator-owned backend. They are not
implemented in the browser, account app, or ChatGPT credential relay. A production
operator that configures them must verify webhook signatures, handle duplicate
delivery, retain request/response and tool-call IDs under its data policy, and use
an API key with `api.safety.alerts.read` for the same project.

## Existing Responses capabilities

Astra inherits the GPT-5.6 contracts Nanocodex already uses: Responses streaming,
Structured Outputs, direct and programmatic tool calling, computer-use-compatible
tool transport, multi-agent orchestration, prompt caching, persisted encrypted
reasoning and explicit compaction. Astra rejects the legacy Pro execution-mode
field, so Nanocodex validates that combination locally and does not send it. Nanocodex does not send the
unsupported sampling parameters `temperature`, `top_p`, or `top_logprobs`, nor the
superseded `prompt_cache_retention` field.

For migrations, OpenAI recommends replacing `none` or `minimal` effort with
`low`, while preserving other effective efforts. Chat Completions integrations
must also remove `logprobs`; Responses integrations must not request
`message.output_text.logprobs` through `include`.

Prompt owners should account for Astra being more likely to ask a focused
question when missing information could change the result, more sensitive to
instructions in skills and files such as `AGENTS.md`, inclined toward detailed
formatted answers, potentially less eager to delegate, and thorough about broad
test coverage. Applications should explicitly state their desired autonomy,
writing style, delegation, and verification scope and audit all model-visible
instructions before enabling the model.

## Astra instructions

Astra uses [its own built-in prompt](../crates/nanocodex-oai-api/prompts/astra.md),
adapted from the `gpt-6-astra` `model_messages.instructions_template` in the
[Codex catalog at `8e6a44b428`](https://github.com/openai/codex/blob/8e6a44b428e31f91b21edc97904fcdf4f0931ade/codex-rs/models-manager/models.json).
The adaptation retains upstream's autonomy, permission, steering, writing,
verification, and skill guidance. It uses Nanocodex's identity and describes
async questions, skills, connectors, and plugins in terms of capabilities actually
supplied by the host, without claiming Codex's orchestrator or approval reviewer
is installed. Other models retain their existing prompt.

The prompt is resolved from the selected model at agent creation, before-first-turn
model changes, subagent creation, and resume. Rust `instructions(...)` and JS
`instructions` remain complete caller replacements. Rust `additional_instructions(...)`
and JS `additionalInstructions` append host rules to either the model prompt or
the caller's replacement. The native CLI uses this additive path for enabled
subagent and memory guidance; Astra's managed and built-in browser harnesses use
it for their runtime instructions. Retained sessions rebuild the prefix using the
retained model and the host's current instructions.

## Current Codex compatibility signals

The inspected upstream Codex revision `03467026f2` includes Astra in its Amazon
Bedrock catalog as `openai.gpt-6-astra`, including global and US cross-region routing.
Its bundled Astra metadata uses a 272,000-token default and 872,000-token maximum,
which Nanocodex mirrors. That internal catalog also advertises an `ultra` reasoning
choice tied to automatic delegation. The public Astra API contract currently lists
only `low`, `medium`, `high`, `xhigh`, and `max`, so Nanocodex deliberately does not
expose the internal-only `ultra` value.

Codex's Fast enum accepts either `fast` or `priority` when parsing but emits
`priority` as its request value. Nanocodex keeps that wire behavior while labeling
requested Astra fast-mode estimates as `fast`. The Responses API's actual returned
tier is not yet retained, so provider fallback to standard processing can make the
local requested-tier estimate differ from the final charge.

Codex now persists harness-authored `configuration_update` items with trusted
provenance and rejects client-injected updates. It also stops ordinary input after
a misalignment violation; a newer UI flow can continue only when the server
provides review details and a bounded continuation request and the user explicitly
acknowledges it. Nanocodex has no equivalent trusted update author or safety-review
UI, so configuration updates stay low-level and misalignment remains terminal.

No `response.steer` implementation exists in that upstream revision. Codex's
ordinary steer path still queues input for a model boundary, matching the durable
boundary-based behavior documented above rather than Astra's connection-local
provider steering protocol.

## Rollout and live evidence

The managed `nanocodex2` terminal selects Astra directly for new conversations;
an explicit provider rejection is its availability signal. The account app still
uses the authenticated Codex model catalog to choose its default and fails closed
when that projection is unavailable. The selector is available only before the
first accepted turn, while thinking and Fast remain live settings.

Both native terminal clients expose the complete Sol, Terra, Luna, and Astra
roster through `/model`; `/model astra` applies the same selection directly.
`/effort`, `/reasoning`, and `/thinking` are aliases for the reasoning picker and
accept a direct `low`, `medium`, `high`, `xhigh`, or `max` value. In the managed
client these commands update the hosted agent's retained settings and are never
submitted or recorded as model prompts. The hosted service continues to enforce
the first-accepted-turn model lock and validates incompatible Astra settings.

On September 4, 2026, a local subscription-authenticated smoke test reached
`gpt-6-astra` over the Responses WebSocket with max thinking and standard service
tier. It ran a workspace `pwd`, spawned and joined a low-thinking child agent,
completed three model calls and five tool calls, and required no response retry or
WebSocket reconnect. The provider rejected an earlier request carrying
`reasoning.mode: "pro"`; Nanocodex now rejects that combination locally and omits
the field from every Astra request.

A separate September 4 prompt check sent the exact built-in Astra prompt with
high thinking, `service_tier: "default"`, and no reasoning summary. It completed
a shell command, shut down, then restored the SQLite-backed conversation in a
fresh CLI process and recalled the prior turn's marker. Both turns completed
without retries or WebSocket reconnects. This verifies the native prompt and
portable resume path; it does not substitute for the managed browser journey below.

Production rollout should additionally verify the exact managed and Worker journey:

1. Create a new managed conversation and confirm the outgoing model is
   `gpt-6-astra` with high reasoning, standard mode, and fast mode off.
2. Complete two durable turns with a reload/reconnect between them; verify retained
   history, persisted settings, compaction behavior, and credential isolation.
3. Submit a steer while a turn is active and confirm the current boundary-based
   behavior; do not record this as native `response.steer` evidence.
4. Run the hosted-tool attach/call/revoke fencing journey on a newly created Astra
   agent.
5. Confirm a simulated `misalignment_policy_violation` is surfaced once, causes no
   retry, dispatches no later actions, and does not imply rollback.
6. Inspect browser console, network, storage, WebSocket frames, and CSP. Provider
   credentials must never reach browser or app surfaces.
