# Provider-aware aggregation and measurement

A candidate is a provider + canonical model + thinking level. The same model on Cloudflare Workers AI, OpenRouter and Vercel is a different candidate because its price, queueing, caching, reliability and network path can differ. ChatGPT subscription inference remains a separate backend; API-equivalent pricing is not subscription cash billing.

## Gateway configuration

Deployment-owned Worker secrets are `OPENROUTER_API_KEY` and `AI_GATEWAY_API_KEY` (Vercel). They are never part of thread configuration, Jev input or persisted route metadata. A provider without a configured secret is removed before classification. Removing a secret from an already pinned route produces an error, not a silent provider switch.

With both gateways configured, the supported catalog contains 45 provider/model/effort choices: 15 existing choices plus the same five supported canonical models through each gateway. GLM uses `z-ai/glm-5.3` on OpenRouter and `zai/glm-5.3` on Vercel. GPT IDs use `openai/` at both gateways. Existing native candidate IDs remain accepted; gateway candidate IDs are provider-qualified, for example `openrouter:z-ai/glm-5.3:low` and `vercel:zai/glm-5.3:low`.

This is an explicitly supported model catalog, not permission to invoke arbitrary catalog entries or URLs. Broader model families require runtime capability profiles and compatibility validation. Tool-use support and reasoning controls must be supported by the selected endpoint. The gateway transports use fixed HTTPS endpoints and bearer authorization, refuse redirects, and do not expose raw provider error bodies. No cross-model fallback list is sent to the gateways. A gateway may select its own upstream host for that same model; measurements describe the gateway route unless the actual upstream is known.

OpenRouter single-call mode omits `parallel_tool_calls: false` from provider matching because some tool-capable endpoints do not advertise that parameter. The adapter enforces the single-call contract on the buffered response before dispatching any tools. Explicit parallel mode continues to require provider support. Both gateways use manual redirect handling and reject redirect responses, including in Cloudflare's runtime.

Both gateway routes reuse the existing Rust/WASM loop through a buffered, full-history HTTP adapter. Model and effort remain canonical in agent state; only the gateway wire identifier differs. Custom tools and namespace aliases use the same translation as the Cloudflare route. Stateless HTTP is explicitly configured for gateway GPT models rather than relying on the GLM-specific default. Gateway costs must be interpreted using gateway metadata or invoice records; core canonical-model estimates are not verified gateway charges.

## Cost evidence

On 2026-09-20 the public model catalogs listed GLM input/output USD per million tokens as $0.91/$2.86 at OpenRouter and $1.40/$4.40 at Vercel. These are dated base-rate hints. Context tiers, cache hits, selected upstreams, discounts, currency, and time-dependent pricing can alter effective costs. Catalog token prices are not predicted total task spend. Provider-specific local measurements remain the stronger evidence for completion/cost/duration selection.

## Regional performance design

Record ingress location separately from inference execution location. Durable Object placement or Smart Placement may move work away from the user's ingress colo. Never label an ingress `request.cf.colo` as the inference executor without supporting execution evidence. Preserve unknown values.

Measure request start, response headers, body completion, failure and cancellation. The current buffered adapters cannot measure generation time-to-first-token; body/header latency must not be mislabeled as TTFT. User-perceived first token and completion latency require client-side receipt timing, not just the time when a Worker writes a response. Track output tokens and prompt/cache size alongside timings so a two-token probe does not stand in for an agent run.

Use recent provider/model/effort/location observations, minimum sample counts, sample age and errors. Probe samples and production samples must remain distinguishable. Successful HTTP response is endpoint availability, not verified task correctness. Timeout/cancellation observations must not be counted as fast successful completions. A scheduled trigger alone does not establish coverage of every Cloudflare region.

Changes in telemetry inform new threads and new children. They do not silently alter an already pinned route.

## Research

- [OpenRouter model catalog](https://openrouter.ai/api/v1/models) and [Vercel model catalog](https://ai-gateway.vercel.sh/v1/models) expose provider-specific pricing/capabilities.
- [OpenRouter performance documentation](https://openrouter.ai/docs/features/latency-and-performance) describes its own Cloudflare Workers deployment, cache warming and failover overhead. Cloudflare placement alone is therefore not a unique advantage; the hypothesis to validate is better cross-gateway decisions using our workload and location measurements.
- [OpenRouter provider metrics](https://openrouter.ai/providers/apply) distinguish TTFT, throughput, uptime and tool-use support.
- [Cloudflare placement](https://developers.cloudflare.com/workers/configuration/placement/) explains that moving execution toward an upstream can improve overall latency, and that scheduled/RPC entrypoints do not share fetch Smart Placement behavior.

Public listings establish availability in a catalog, not access granted to our deployment. Authenticated live OpenRouter/Vercel inference requires configured keys and credits. Do not describe mocked transport checks or Mac-side timing as live global Worker measurements.

Authenticated GLM-low CLI execution and gateway compatibility fixes are recorded in [the 2026-09-21 live verification report](THREAD_ROUTING_GATEWAYS_2026_09_21.md).

## Scheduled TTFT selection

The Worker exposes opt-in routing and includes a half-hourly streaming probe schedule, disabled by default. Explicitly enable probes independently of each new agent’s routing policy. One deployment-wide Durable Object deduplicates schedule slots and limits requests. Three successful fresh samples qualify a candidate's p50/EWMA TTFT for Jev; failure counts remain separate evidence. The complete audit is retained, while compact candidate fields avoid overloading Jev's input. Existing roots and children remain pinned.

See [schedule, controls, measurement definition and live verification](THREAD_ROUTING_TTFT_2026_09_21.md). Shared probes cover Workers AI, OpenRouter and Vercel; ChatGPT subscriptions do not inherit gateway TTFT. The API is opt-in through `configuration.model_routing`; deployment alone never enrolls clients. Background probes ship disabled and require an explicit `NANOCODEX_PROVIDER_PROBES=true` deployment setting.
