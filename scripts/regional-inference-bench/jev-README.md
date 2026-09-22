# Isolated actual Jev comparison

These temporary Workers import the unchanged production `resolveThreadRoute` and call the real `env.AI.run('typesafe/jev', ...)`. They carry a dedicated AI binding, protected bearer token, expiry, fixed deployment-owned region and durable six-case reservation ledger. They do not write production observations or change production code. A reservation is never retried, even if a request outcome is unknown.

The frozen calibration is exactly streaming pairs 0–2: three measurements per provider/region, nine per provider globally. Pairs 3–4 are excluded before aggregation. Production `summarizeProviderObservationGroups` computes the actual input projections. Both arms receive identical full calibration bytes; the global arm sets `clientIngressColo:null`, and the regional arm sets the fixed deployment-owned expected API ingress. `workerColo` is always null. Original observation timestamps are preserved, and stale data expires rather than being refreshed. Calibration pools two short tasks and one long-prefix task per candidate/region; heldout prompts are all short. This distribution mismatch limits conclusions.

`generationTtft*` field names are required by the unchanged resolver. Their experiment values are API first public output delivery timings, explicitly labeled `experiment_api_output_delivery_ttft_proxy_not_internal_generation_ttft` in artifacts and the common policy preference text. They must not be read as production-internal generation TTFT. The resolver's own unchanged telemetry labels remain visible in its full audit for traceability.

Origin cannot come from request JSON or caller headers. The only accepted inputs are `{arm,prompt_index}`. Before reserving a case, a non-inference HEAD from the placed Worker must have the configured API ingress Ray suffix. After inference, both the server-owned `x-nanocodex-ingress-colo` and Ray suffix must match. This establishes the API ingress cohort; it does not establish provider compute placement. Runner `cf-placement` receipts are recorded separately; `remote-` without a named colo remains missing per-request execution evidence.

There are 3 fixed new short code/math/analysis tasks × 3 regions × 2 arms = at most 18 outer Jev binding calls plus 18 downstream API calls (36 attempts). Each downstream request explicitly uses the selected provider's same Luna low candidate, stream:true, store:false, max_output_tokens:256. Any API-internal routing is retained in the downstream `route.router_duration_ms`; this is not subtracted from API timings. A Jev timeout is conservatively a consumed classification attempt even if its binding promise later resolves. Candidate probabilities are actual classifier choice probabilities, not success probabilities. Null probabilities remain null. Low-confidence proposed-candidate fallbacks are recorded as fallbacks; they are not accepted learned decisions.

`downstream.first_meaningful_ms` begins at the downstream API POST. `experiment_first_meaningful_ms` includes outer Jev selection plus downstream API delivery. Both exclude controller-to-Worker transport, the protected-wrapper HEAD and the durable reservation. `preflight_ms` records HEAD overhead separately. Each region alternates arm order by prompt, with phase rotated across regions; three regional controllers run concurrently with at least 7.5s between case starts. No latency comparison can remove time/load/cache/order or classifier stochasticity confounding with only three tasks per arm/region. Retain failures and report matched pairs instead of a pooled headline.

Run from the repository root in the authorized native environment. Supply the private runner manifest and inference-key batch using paths outside the checkout; the manifest references tokens and configs without storing credentials in repository outputs. Its associated public frozen files are in `docs/performance/2026-09-21-regional-inference/jev-plan/`.

```sh
node --test scripts/regional-inference-bench/jev-core.test.mjs
node scripts/regional-inference-bench/jev-manage.mjs dry-run /path/to/private/jev/runners.json
# Deploy only after validating the frozen plan and available budget.
node scripts/regional-inference-bench/jev-manage.mjs deploy /path/to/private/jev/runners.json /path/to/private/keys.json --approved
node scripts/regional-inference-bench/jev-campaign.mjs /path/to/private/jev/runners.json docs/performance/2026-09-21-regional-inference/jev --approved
# Only after preserving receipts. Deletes isolated Jev Workers, not original baseline runners.
node scripts/regional-inference-bench/jev-manage.mjs delete /path/to/private/jev/runners.json --approved
```

Do not rerun deployment after an ambiguous error. Inspect the private Wrangler receipt and remote version before further action. Campaign refuses an existing output directory, but the authoritative duplicate-case limit is server-side durable storage and survives controller restarts. Keep the same Worker namespaces throughout this campaign. Recreating/resetting namespaces would invalidate its budget guarantee. Do not rerun expired calibration with rewritten timestamps.
