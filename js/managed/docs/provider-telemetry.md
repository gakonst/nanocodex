# Provider telemetry

Live provider observations measure actual managed generation attempts without issuing extra requests. Scheduled synthetic probes use a separate durable request budget. These measurements support diagnostics and the router dashboard; they are not inputs to Jev selection. Neither is a task completion probability, quality score, task-duration estimate, or client-delivery measurement.

## Live observer contract

`beginLiveProviderObservation(metadata, store)` in `src/provider-telemetry.ts` starts a wall-clock timestamp and monotonic duration clock immediately before provider dispatch. The public adapter's `GatewayRequestObserver` exposes:

- `headers(status)` once HTTP headers arrive. Bindings may expose neither status nor headers.
- Optional `firstToken()` when the adapter emits its first nonempty public text or validated tool event. Headers, roles, reasoning, hidden tool fragments and encrypted metadata do not count. Tools remain private until terminal validation, so tool-only TTFT includes that validation delay.
- `finish(outcome)` after successful protocol completion or a terminal failure/cancellation. It persists at most one observation. Failed attempts retain elapsed time and outcome but censor both generation TTFT and successful full-response duration, even if partial text was emitted.

HTTP OpenRouter/Vercel Chat SSE and Cloudflare Responses SSE use incremental observation. Workers AI and Cloudflare bindings may return a stream or a validated buffered object. Buffered object fallback leaves generation TTFT unknown. `clientDeliveryMs` remains null: server output does not establish receipt by a user. Telemetry exceptions must not break generation, and no callback receives prompt, URL, credential, provider body, raw exception, or user identity.

`gatewayRuntime(env, route, assertActive, fetch, telemetry)` supplies the observer through the public adapter's `onRequest` option. Its optional fifth argument is `GatewayTelemetry`, containing the append store plus `workerColo` and `clientIngressColo`. Pass `undefined` for the fourth argument to use global fetch. Keep the returned adapter options intact.

## Storage, origin and shared evidence

`SqliteProviderTelemetryStore` projects only measurement fields and retains the latest 512 observations per store. Managed threads append locally and publish asynchronously to the private `ProviderProbeCoordinator.observe()` service-binding RPC. Shared live collection and reads work even when scheduled probes are disabled. The RPC accepts only fresh, bounded, internally consistent live measurements for allowed catalog backend/model/effort combinations; provider aliases normalize to the canonical model. It has no public observation route.

Managed ingress is captured from trusted `request.cf.colo` at the public Worker boundary. Caller-supplied internal origin headers are stripped. The first thread creation stores the normalized ingress cohort transactionally in `managed_routing_origin`; reconnects and repeated creation assertions cannot replace it. Root runtime reconstruction reads that persisted value; new ephemeral children inherit it from the live root. Ingress is not execution placement: managed `workerColo` remains null unless separately established by trusted execution evidence.

`summarizeProviderObservationGroups(samples, now, origin)` keeps live and probe sources separate and emits deployment-global aggregates plus matching live ingress/execution cohorts. A coordinator snapshot without an origin returns global cohorts only; its dashboard snapshot includes all observed cohorts. Managed routing performs no telemetry reads or coordinator snapshot RPCs before generation.

Summaries use a two-hour freshness window, exclude future/stale samples, and report successful-duration p50, nearest-rank p95 and EWMA alongside censored outcome counts. Summary `usable` requires at least three full-response or TTFT samples. Full-response timings cannot substitute for missing TTFT.

The `jev-direct-v4` selector ignores historical provider telemetry and geography. Its compact input includes the opening task, eligible model and effort profiles, catalog price hints, published evaluation evidence, explicit policy `estimates`, and preferences. Telemetry collection remains independent of candidate selection. Existing root and retained-child route pins remain immutable.

## Scheduled probes and bounds

Scheduled probes are disabled in the checked-in `wrangler.jsonc`: `triggers.crons` is empty, `NANOCODEX_PROVIDER_PROBES` is `"false"`, and `NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT` is `"0"`. Live observations continue independently. To enable synthetic probes for a deployment, add the half-hour cron (`*/30 * * * *`), set `NANOCODEX_PROVIDER_PROBES` to `"true"`, choose a daily request limit from 1 to 4,096, and retain the coordinator binding. Deploy the configuration for these settings to take effect.

The coordinator uses the single deployment budget owner `deployment-provider-probes-v1`, persisted at-most-once slot claims, and atomic request reservations before provider dispatch. Failures consume budget; probes do not retry. A zero or invalid daily limit prevents probe dispatch.

For example, an enabled 1,600/day limit with a fully available 45-target catalog makes `probeSlotAllocation()` rotate 33 or 34 targets through each of 48 daily slots, totaling 1,600 allocated attempts with 35 or 36 per target. Actual attempts can be lower when targets are unavailable or execution fails. Guaranteeing three observations for every target in every four-slot/two-hour window requires at least 1,620/day. A 1,600/day budget leaves occasional sparse cohorts unknown under the three-sample gate; failures can further reduce coverage.

Targets come from configured catalog availability: Workers AI, Cloudflare native Responses (binding or authenticated REST), OpenRouter and Vercel. ChatGPT subscription routes have no deployment-owned probe credential and retain unknown synthetic TTFT. The runner uses fixed allowlisted HTTPS endpoints, rejects redirects, adds fresh entropy before a fixed synthetic prompt, and requests streaming. The default output limit is 128 tokens (configurable within 16–2,048), response consumption is capped at 64 KiB, and timeout is 10 seconds by default with a 30-second hard cap. The runner permits at most 45 sequential targets per invocation and durable daily limits of 1–4,096; request/token bounds are not a dollar cap.

A cron or deployment-global coordinator does not establish geographic coverage. Probe execution colo remains unknown unless independently verified. Regional benchmarks need separately verified execution placement, per-source budgets and explicit source labels. Ingress colo and local Mac timings cannot establish regional Worker execution.

## Focused verification

Run from the repository root:

```sh
pnpm --filter nanocodex-managed-service exec vitest run --config vitest.routing.config.ts
pnpm --filter nanocodex-managed-service exec node --test test/provider-telemetry.test.mjs test/thread-routing-wasm.test.mjs
pnpm --filter nanocodex-managed-service exec vitest run test/provider-probe-coordinator.test.ts test/managed-ingress-origin.test.ts test/managed-routing-admission.test.ts test/managed-subagent-routing-runtime.test.ts
pnpm --filter nanocodex-managed-service exec tsc --noEmit
```

These tests use synthetic identities and provider responses. They cover privacy projection, monotonic timing and censoring, origin spoofing and reconstruction, private shared live observations, candidate aliases, cohort summaries, routing independence from telemetry, budgets and slot rotation, and real Rust/WASM root/child tool loops with pinned independent transports. They issue no paid provider requests.
