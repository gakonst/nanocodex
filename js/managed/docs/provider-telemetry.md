# Worker-origin provider telemetry PoC

These modules do not register a cron, deploy, or issue requests unless explicitly invoked with `enabled: true`. Scheduled probe integration must remain disabled by default. Live request observations do not issue additional provider requests. No live paid probe has been performed as part of this implementation.

## APIs and wiring

`ProviderObservation` and `ProviderTelemetryStore` are exported by `src/provider-telemetry.ts`. At the existing provider HTTP adapter, capture timestamp and request start, measure headers when fetch resolves, and fullResponseMs only after the complete successful body is consumed. Append a `source: "live"` observation using the selected catalog backend/model/effort. Failures retain elapsedMs and outcome, with fullResponseMs null, so timeouts do not appear as fast successes. Never pass prompt, URL, credential, provider body, raw exception text, or user identity. Projection in the SQLite writer prevents extra fields persisting; string fields still must come from trusted catalog/runtime metadata.

The current buffered adapter cannot observe generation TTFT; set generationTtftMs to null. HTTP headers are not TTFT. Set clientDeliveryMs to null unless measured by client acknowledgement over its own monotonic clock; server fetch completion does not establish user receipt. True user-delivery instrumentation remains an integration task in the client/adapter, outside this module.

Instantiate `new SqliteProviderTelemetryStore(existingDurableObjectStorage.sql)` in an existing tenant/thread DO for live samples. It retains the latest 512 records. `read()` supplies bounded samples. Group using `providerObservationKey`, then `summarizeProviderObservations(group, Date.now())`. The default five-minute window excludes future/stale records; at least five successful samples are needed before `usable` becomes true. Summaries expose censored counts and success fraction alongside successful-response p50 and EWMA. Do not silently rank only successes while ignoring failures. Feed these as contextual evidence to Jev, never as a calibrated completion probability or quality estimate.

For a separately budgeted scheduled shard:

```ts
scheduled(_event, env, ctx) {
  ctx.waitUntil(runProviderProbes({
    enabled: env.PROVIDER_PROBES_ENABLED === "true",
    dailyRequestLimit: 24,
    targets: configuredCatalogTargets, // backend, model, secret key; trusted configuration only
    store: regionalProbeStore, // durable atomic reserveProbe + append
    workerColo: verifiedExecutingWorkerColo ?? null,
  }));
}
```

The scheduling integration must execute provider fetch inside a Cloudflare Worker. One durable budget owner per scheduled shard must own `reserveProbe`; in-memory stores are test-only. Spread shards deliberately by region; do not place all live traffic behind one global telemetry object. A cron does not promise geographic coverage. Executing-Worker colo must be obtained from trusted execution metadata or independently verified deployment/runtime evidence. Incoming request.cf.colo represents ingress and must be stored separately as clientIngressColo, particularly with Smart Placement and Durable Objects. Unknown execution location stays null; local Mac timings cannot be labeled regional Worker evidence.

The runner permits only fixed OpenRouter/Vercel HTTPS completion endpoints, rejects redirects, sends a fixed short synthetic prompt with max_tokens 8, drains at most 64KiB, caps timeouts to 30 seconds, makes at most two sequential requests per tick, and atomically reserves at most 100 requests/day per store before fetching. No retries. Configure fewer than these hard bounds in practice and restrict catalog models/cost via provider-side budgets: request and output-token limits are not an exact dollar cap. Use enough bounded schedules to meet your own sampling threshold; sparse probes should remain unusable. Synthetic results are labeled probe and kept separate from real workloads and reasoning-effort choices.

## Primary-source comparison (reviewed 2026-09-20)

[OpenRouter provider selection](https://openrouter.ai/docs/guides/routing/provider-selection) supports price/throughput/latency sorting and provider orders; explicit sort/order disables default load balancing. Its performance preferences use rolling five-minute provider/model percentiles, with latency in seconds and throughput in tokens/sec. These upstream aggregates can supplement telemetry but do not establish this Worker's network or user-delivery performance.

[Vercel model metrics](https://vercel.com/changelog/live-model-performance-metrics-accessible-via-ai-gateway) exposes `/v1/models/{creator}/{model}/endpoints` with hourly provider P50/P95 TTFT in milliseconds and throughput in tokens/sec, derived from gateway traffic. [Vercel observability](https://vercel.com/docs/ai-gateway/observability-and-spend/observability) distinguishes TTFT, request duration, usage and spend, with project/API-key views. External aggregates need a separate source label and their own freshness semantics; these modules do not equate them to local full-response latency.

## Verification

`node --test js/managed/test/provider-telemetry.test.mjs` on Node with native TypeScript stripping verifies disabled operation, deterministic-clock latency, censoring, allowlisted endpoints, request budget ordering, privacy projection, and real SQLite persistence/retention. The tests inject fetch and issue no provider calls.

[OpenRouter's latency architecture](https://openrouter.ai/docs/guides/best-practices/latency-and-performance) already uses Cloudflare Workers and edge caching. The proposed benefit here is application-specific comparison across gateways, models, reasoning settings and observed origins, with real delivery evidence when available. Cloudflare deployment itself is not a unique advantage or proof of faster routes. Single-cron execution is not a global probe network: regional coverage requires multiple verified execution sources, explicit per-source budgets, and separate freshness/sample thresholds. Continuous probing is not enabled by these source files.

### Live server observer integration

`beginLiveProviderObservation(metadata, store)` is the server adapter hook. Instantiate immediately before each provider fetch, call `.headers(response.status)` when fetch resolves, and await `.finish("success")` only after complete body consumption. On HTTP failures finish with `http_error`; on thrown transport failure choose `network_error`, `timeout`, or `cancelled` from known local state, without recording raw exceptions. The hook uses monotonic durations and a separate wall timestamp, writes at most once, projects catalog/location metadata, and returns false on storage errors without breaking generation. Both generationTtftMs and clientDeliveryMs remain null. `gatewayRuntime(env, route, assertActive, fetch, telemetry)` now supplies the public adapter's `onRequest` hook. Its optional fifth argument is `GatewayTelemetry`: `{ store: Pick<ProviderTelemetryStore, "append">, workerColo: string | null, clientIngressColo: string | null }`. Pass `undefined` as the fourth argument to use global fetch. The index integration owns constructing the SQLite store on the existing thread DO and supplying trusted geography (or null). Pass the returned options unchanged to `createGatewayResponses`.

The public `GatewayRequestObserver` accepts only `headers(status)` and `finish(outcome)`; it receives no request, response, URL, error, session identity, body, or credentials. It starts after local request translation/validation, immediately before dispatch. Success requires consuming JSON and validating the complete provider result. Invalid JSON/completions are `protocol_error`; failed body transport is `network_error`; HTTP errors, cancellation, and local timeout remain distinct censored outcomes. Telemetry exceptions are swallowed. Late fetch completion after cancellation cannot finalize a second sample. The persisted thread route must remain pinned; only fresh new-thread/child decisions should consume updated snapshots.

Scheduled configuration is opt-in: map `env.PROVIDER_PROBES_ENABLED === "true"` into the runner, keep the deployed default false, and register a cron only in an explicitly selected probe deployment/config after budget ownership is wired. Merely enabling an unrelated existing scheduled handler must never imply probe authorization.
