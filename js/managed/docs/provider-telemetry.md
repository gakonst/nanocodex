# Provider telemetry and routing

Live provider observations measure actual managed generation attempts without issuing extra requests. Scheduled synthetic probes use a separate durable request budget. Both supply bounded context to Jev; neither is a task completion probability, quality score, task-duration estimate, or client-delivery measurement.

## Live observer contract

`beginLiveProviderObservation(metadata, store)` in `src/provider-telemetry.ts` starts a wall-clock timestamp and monotonic duration clock immediately before provider dispatch. The public adapter's `GatewayRequestObserver` exposes:

- `headers(status)` once HTTP headers arrive. Bindings may expose neither status nor headers.
- Optional `firstToken()` when the adapter emits its first nonempty public text or validated tool event. Headers, roles, reasoning, hidden tool fragments and encrypted metadata do not count. Tools remain private until terminal validation, so tool-only TTFT includes that validation delay.
- `finish(outcome)` after successful protocol completion or a terminal failure/cancellation. It persists at most one observation. Failed attempts retain elapsed time and outcome but censor both generation TTFT and successful full-response duration, even if partial text was emitted.

HTTP OpenRouter/Vercel Chat SSE and Cloudflare Responses SSE use incremental observation. Workers AI and Cloudflare bindings may return a stream or a validated buffered object. Buffered object fallback leaves generation TTFT unknown. `clientDeliveryMs` remains null: server output does not establish receipt by a user. Telemetry exceptions must not break generation, and no callback receives prompt, URL, credential, provider body, raw exception, or user identity.

`gatewayRuntime(env, route, assertActive, fetch, telemetry)` supplies the observer through the public adapter's `onRequest` option. Its optional fifth argument is `GatewayTelemetry`, containing the append store plus `workerColo` and `clientIngressColo`. Pass `undefined` for the fourth argument to use global fetch. Keep the returned adapter options intact.

## Storage, origin and shared evidence

`SqliteProviderTelemetryStore` projects only measurement fields and retains the latest 512 observations per store. Managed threads append locally and publish asynchronously to the private `ProviderProbeCoordinator.observe()` service-binding RPC. Shared live collection and reads work even when scheduled probes are disabled. The RPC accepts only fresh, bounded, internally consistent live measurements for allowed catalog backend/model/effort combinations; provider aliases normalize to the canonical model. It has no public observation route.

Managed ingress is captured from trusted `request.cf.colo` at the public Worker boundary. Caller-supplied internal origin headers are stripped. The first thread creation stores the normalized ingress cohort transactionally in `managed_routing_origin`; reconnects and repeated creation assertions cannot replace it. Runtime reconstruction reads that persisted value for the root and children. Ingress is not execution placement: managed `workerColo` remains null unless separately established by trusted execution evidence.

`summarizeProviderObservationGroups(samples, now, origin)` keeps live and probe sources separate and emits deployment-global aggregates plus matching live ingress/execution cohorts. A coordinator snapshot without an origin returns global cohorts only. Managed snapshot reads have a 250 ms deadline; unavailable evidence stays unknown. Local cohorts fill gaps in shared live snapshots, but overlapping local/shared counts are never added together.

Summaries use a two-hour freshness window, exclude future/stale samples, and report successful-duration p50, nearest-rank p95 and EWMA alongside censored outcome counts. Summary `usable` requires at least three full-response or TTFT samples; routing independently requires at least three fresh successful TTFT samples before exposing TTFT as usable. Full-response timings cannot substitute for missing TTFT.

Jev receives at most one cohort per candidate/source. Sufficient fresh ingress TTFT is preferred, then known execution-cohort TTFT, then global live fallback; a sparse regional cohort cannot hide sufficient global evidence. Synthetic probes remain global and separate from live observations. Selection still respects candidate availability, explicit model/effort restrictions and task capability. Missing evidence is unknown, never zero. Existing root and retained-child route pins remain immutable; updated evidence applies only to new decisions.

## Scheduled probes and bounds

The managed Worker dispatches the half-hour schedule (`*/30 * * * *`) only when `NANOCODEX_PROVIDER_PROBES === "true"` and the coordinator binding exists. The checked-in `wrangler.jsonc` explicitly enables this setting and specifies `NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT: "1600"`; changing source files alone does not deploy or start a schedule. The coordinator uses the single deployment budget owner `deployment-provider-probes-v1`, persisted at-most-once slot claims, and atomic request reservations before provider dispatch. Failures consume budget; probes do not retry.

At the configured 1,600/day limit and a fully available 45-target catalog, `probeSlotAllocation()` rotates 33 or 34 targets through each of 48 daily slots, totaling 1,600 allocated attempts with 35 or 36 per target. Actual attempts can be lower when targets are unavailable or execution fails. The previous all-45-per-slot cadence would exhaust this budget before the day ended. Guaranteeing three observations for every target in every four-slot/two-hour window requires at least 1,620/day. The current budget deliberately leaves occasional sparse cohorts unknown under the unchanged three-sample gate; failures can further reduce coverage.

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

These tests use synthetic identities and provider responses. They cover privacy projection, monotonic timing and censoring, origin spoofing and reconstruction, private shared live observations, candidate aliases, cohort fallback, budgets and slot rotation, and real Rust/WASM root/child tool loops with pinned independent transports. They issue no paid provider requests.
