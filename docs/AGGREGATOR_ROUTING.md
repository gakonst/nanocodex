# Provider-aware aggregation and measurement

A candidate is a provider + canonical model + thinking level. The same model on Cloudflare Workers AI, OpenRouter and Vercel is a different candidate because its price, queueing, caching, reliability and network path can differ. ChatGPT subscription inference remains a separate backend; API-equivalent pricing is not subscription cash billing.

## Gateway configuration

Deployment-owned Worker secrets are `OPENROUTER_API_KEY` and `AI_GATEWAY_API_KEY` (Vercel). They are never part of thread configuration, Jev input or persisted route metadata. A provider without a configured secret is removed before classification. Removing a secret from an already pinned route produces an error, not a silent provider switch.

Gateway candidates currently cover GLM-5.3, Astra, Kimi K3, and MiMo v2.6 Pro. Sol and Luna use native ChatGPT or explicitly enabled Cloudflare Responses routes; their Chat Completions tool support requires `none` effort, outside the managed routing policy. Candidate availability and effort follow the [routing catalog](../js/managed/src/thread-model-routing.ts), not a fixed catalog count. GLM uses `z-ai/glm-5.3` on OpenRouter and `zai/glm-5.3` on Vercel. GPT IDs use `openai/` at both gateways. Existing native candidate IDs remain accepted; gateway candidate IDs are provider-qualified, for example `openrouter:z-ai/glm-5.3:low` and `vercel:zai/glm-5.3:low`.

This is an explicitly supported model catalog, not permission to invoke arbitrary catalog entries or URLs. Broader model families require runtime capability profiles and compatibility validation. Tool-use support and reasoning controls must be supported by the selected endpoint. The gateway transports use fixed HTTPS endpoints and bearer authorization, refuse redirects, and do not expose raw provider error bodies. No cross-model fallback list is sent to the gateways. A gateway may select its own upstream host for that same model; measurements describe the gateway route unless the actual upstream is known.

OpenRouter single-call mode omits `parallel_tool_calls: false` from provider matching because some tool-capable endpoints do not advertise that parameter. The adapter enforces the single-call contract on the buffered response before dispatching any tools. Explicit parallel mode continues to require provider support. Both gateways use manual redirect handling and reject redirect responses, including in Cloudflare's runtime.

Both gateway routes reuse the existing Rust/WASM loop through a full-history HTTP adapter with streaming and buffered response support. Model and effort remain canonical in agent state; only the gateway wire identifier differs. Custom tools and namespace aliases use the same translation as the Cloudflare route. Stateless HTTP is explicitly configured for gateway GPT models rather than relying on the GLM-specific default. Gateway costs must be interpreted using gateway metadata or invoice records; core canonical-model estimates are not verified gateway charges.

## Cost evidence

On 2026-09-20 the public model catalogs listed GLM input/output USD per million tokens as $0.91/$2.86 at OpenRouter and $1.40/$4.40 at Vercel. These are dated base-rate hints. Context tiers, cache hits, selected upstreams, discounts, currency, and time-dependent pricing can alter effective costs. Catalog token prices are not predicted total task spend. Provider-specific local measurements remain the stronger evidence for completion/cost/duration selection.

## Interpreting latency evidence

Record ingress location separately from inference execution location. Durable Object placement or Smart Placement may move work away from the user's ingress colo. Never label an ingress `request.cf.colo` as the inference executor without supporting execution evidence. Preserve unknown values.

Measure request start, response headers, body completion, failure and cancellation. Streaming adapters measure generated output; buffered responses leave generation time-to-first-token unknown. Body/header latency must not be mislabeled as TTFT. User-perceived first token and completion latency require client-side receipt timing, not just the time when a Worker writes a response. Track output tokens and prompt/cache size alongside timings so a two-token probe does not stand in for an agent run.

Use recent provider/model/effort/location observations, minimum sample counts, sample age and errors. Probe samples and production samples must remain distinguishable. Successful HTTP response is endpoint availability, not verified task correctness. Timeout/cancellation observations must not be counted as fast successful completions. A scheduled trigger alone does not establish coverage of every Cloudflare region.

Telemetry supports operational inspection. The current router does not wait for geographic/probe snapshots or feed their aggregates to Jev. Caller-supplied routing `estimates` remain supported; existing route pins do not change.

## Scheduled TTFT probes

The Worker supports a half-hourly streaming probe schedule, disabled by default and independent of each agent's routing policy. One deployment-wide Durable Object deduplicates schedule slots and limits requests. Probe results are operational telemetry; enabling probes does not enable latency-based model selection. Existing roots and children remain pinned.

To enable probes, add `*/30 * * * *` UTC to the Worker's `triggers.crons`, retain
the `NANOCODEX_PROVIDER_PROBE_COORDINATOR` binding, and set
`NANOCODEX_PROVIDER_PROBES=true` with a positive
`NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT` (1–4,096). The checked-in configuration
has no cron triggers, disables probes, and sets the limit to zero. With no limit override,
the runtime default is 1,600 requests per day. Each slot rotates a bounded slice
of available candidates; failures consume reserved budget and are not retried.
Request limits do not impose a dollar cap.

Probes request at most 128 output tokens by default and use a ten-second timeout.
TTFT means the first nonempty generated text or plaintext reasoning event, not
headers or buffered-response delivery. Three successful samples within two hours
qualify a TTFT aggregate; sparse, stale, or failed measurements remain unknown. ChatGPT
subscription routes have no deployment-owned probe credential and do not inherit
gateway timings. See the [telemetry contract](../js/managed/docs/provider-telemetry.md)
for measurement, storage, and verification commands.
