# Thread model routing PoC

The default now uses direct Jev model-and-thinking selection. See [Preference-aware thread routing](THREAD_ROUTING_PREFERENCES.md) for the current API. The family policy described below is retained as `strategy: "legacy"`; historical validation reports remain unchanged.

This opt-in hosted API path classifies the first admitted task with Jev, persists a model and thinking level in the thread's Durable Object, and runs the existing Rust `nanocodex-agent` loop through either Workers AI or the existing ChatGPT transport. Later turns reuse the persisted choice. There is no automatic mid-thread escalation.

The implementation is on `poc/thread-model-routing`. It is not deployed or enabled by default.

## Enable and configure

The managed Worker has an `AI` binding. Set `NANOCODEX_THREAD_ROUTING="true"` in the desired deployment environment and ensure that account can invoke both models. Create a new managed thread through the existing create/run endpoint, supplying:

```json
{
  "configuration": {
    "model_routing": {
      "strategy": "legacy",
      "objective": "balanced",
      "frontier_model": "gpt-6-astra",
      "oss_thinking": "medium",
      "frontier_thinking": "high",
      "min_confidence": 0.75
    }
  }
}
```

The normal input field and authorization requirements of that endpoint still apply. Routed threads require full account authority and the managed runtime profile. Do not combine `model_routing` with explicit settings or enabled subagents. Model and thinking settings are locked after creation. Existing threads are not converted. The PoC supports `low`, `medium`, and `high` thinking, standard reasoning mode, and no fast mode. Frontier options are Astra, Sol, Terra, and Luna using their existing wire IDs.

`GET /state` exposes `model_route` (null before initial admission) and recent `routing_observations`. The retained record includes model, thinking, task family, classifier confidence, selection reason, evidence links, optional local estimate, classifier usage and duration. A same-isolate singleflight plus atomic SQLite commit prevents competing admissions from choosing different routes; after restart the retained record is authoritative. A restart before commit can repeat the classifier call.

## Model selection

[Jev](https://developers.cloudflare.com/ai/models/typesafe/jev/) is a structured evaluator, used here as a task classifier through `env.AI.run("typesafe/jev", ...)`. It is not the executing agent model. The OSS route uses [`@cf/zai-org/glm-5.3`](https://developers.cloudflare.com/workers-ai/models/glm-5.3/), a text model supporting tools and reasoning. Its listed input/cache/output rates are $1.40/$0.26/$4.40 per million tokens. Cloudflare lists Jev pricing in its dashboard; this PoC does not invent a price for it.

GLM is the initial candidate, not a claim that it wins every task. Other OSS families can be added through explicit model capability profiles and adapters after evaluation. AI Gateway is optional proxy/observability infrastructure; the OSS path here calls the Worker AI binding directly. Frontier inference preserves the existing ChatGPT authorization and transport.

## Task-to-eval mapping

Published evals supply candidate-selection evidence, not calibrated success probabilities for arbitrary user threads.

| Task family | Relevant eval | Initial policy without local measurements |
| --- | --- | --- |
| Repository issue repair | SWE-bench Pro / Verified, kept separate | Frontier |
| Long engineering | DeepSWE v1.1 | GLM for balanced/cost |
| Shell, build, configuration | Versioned Terminal-Bench | GLM for balanced/cost |
| Research | BrowseComp | Frontier |
| Science | GPQA Diamond | Frontier |
| Mathematics | Year-pinned AIME | Frontier |
| Desktop | Versioned OSWorld | Frontier; GLM profile is text-only |
| Business tool workflows | Toolathlon Verified | GLM for balanced/cost |
| Mixed/unknown | No applicable eval | Frontier |

[GLM's model card](https://huggingface.co/zai-org/GLM-5.3) reports DeepSWE v1.1 66.9, Terminal-Bench 2.1 88.2, and Toolathlon Verified 73.0. [Astra's release](https://openai.com/index/gpt-6-astra/) reports DeepSWE 74.1, BrowseComp 91.5 and GPQA 96.0. These are vendor-reported results under their published harnesses, efforts and limits. GLM's terminal result uses Claude Code; neither it nor Astra's maximum-effort results measure this Nanocodex harness at the configured effort. Terminal-Bench 2.1, 3.0 and 4.0 are distinct evaluations. [Sol/Terra/Luna results](https://openai.com/index/gpt-5-6/) are also directional evidence, not interchangeable measurements. Do not use simulated vendor latency as observed Cloudflare completion time.

For `effectiveness` or `time` without matching local measurements, the policy conservatively retains frontier. Low-confidence, malformed or unavailable Jev output also pins frontier. Oversized opening input and unsupported modalities skip classification and pin frontier. A positive minimum success threshold instead fails admission when no eligible measured route can be established.

## Measured cost/effectiveness/time policy

Pass `estimates` in `model_routing` to replace the provisional prior. Each record has:

```json
{
  "family": "terminal",
  "backend": "workers_ai",
  "model": "@cf/zai-org/glm-5.3",
  "thinking": "medium",
  "success_rate": 0.8,
  "expected_cost_usd": 0.12,
  "expected_duration_ms": 12000,
  "sample_size": 100,
  "source": "example-only-heldout-cohort-v1"
}
```

These numbers are illustrative, not observed results. Supply both backend records from the same held-out cohort, exact model and thinking level. `source` must identify that shared dataset/harness; equality is checked, provenance is not independently authenticated. Duplicate family/backend/model/thinking entries are rejected. `min_success_rate` excludes ineligible candidates after comparability is established; the prior cannot override it.

`success_rate` is independently verified successes divided by all attempts. Cost and duration are mean totals across all attempts, including failures. Objectives minimize cost/success, duration/success, or failure rate. Balanced combines normalized cost/success, normalized duration/success and failure rate using configurable nonnegative `weights` (`cost`, `time`, `effectiveness`). Ratios are amortized cohort metrics, not a promise that repeating one task will succeed. No retry/escalation policy is implemented.

For calibration, fix representative task sets, tools, harness revision, effort and graders; run both models, recording accepted completions, all failed attempts, billed tokens/cache, tools, and wall-clock p50/p95. Include classifier cost when comparing total service cost. ChatGPT subscription usage must not be confused with API-equivalent token pricing. This PoC accepts those aggregates but does not run an eval campaign or learn a policy automatically.

## Transport and limits

The Workers AI adapter converts existing Responses requests and full conversation replay to GLM chat/tool calls, then converts replies into Responses events consumed by the same Rust loop. Tool aliases, custom tools and deferred tool discovery retain their identities. No second JavaScript agent loop is introduced. GLM uses HTTP, disables WebSocket warmup, and retains its real model identity and a neutral prompt.

This is a text-only PoC. Provider replies are buffered before emitting canonical events, so it does not yet deliver true token streaming. Grammar-constrained custom tools use JSON plus descriptions rather than provider-enforced grammars. Opaque compaction and `previous_response_id` are rejected; long threads eventually need a compatible compaction implementation. Abort stops waiting but cannot guarantee cancellation of already dispatched provider computation. Subagents and routed durability portability are unavailable in this PoC.

Observations record terminal outcomes after a route is pinned, including admission failures at the common terminal commit. Pre-pin failures have no route observation; failed/cancelled agent usage is currently unavailable. Observations are telemetry, not an eval dataset: `completed` does not imply verified success, and verification fields start null. Missing usage is unknown, never zero. Separate classifier usage from agent usage and account for failures when constructing estimates. No live Cloudflare billing, model-access smoke test, or production deployment is implied by mocked transport tests.

## Other candidates checked (2026-09-19)

The [Cloudflare catalog](https://developers.cloudflare.com/ai/models/) lists Kimi K3 as third-party and Kimi K2.7 Code as Cloudflare-hosted. [Moonshot](https://www.moonshot.ai/) identifies K3 as its flagship. Availability through the unified AI surface and Cloudflare-hosted execution are distinct; do not infer native hosting from a catalog entry.

[DeepSeek V4 Pro 0813 and Flash 0731](https://developers.cloudflare.com/changelog/post/2026-08-14-deepseek-v4-workers-ai/) are available on Workers AI. DeepSeek also publishes the newer [V4.1 Flash](https://huggingface.co/deepseek-ai/DeepSeek-V4.1-Flash); this research did not establish its availability as a Cloudflare-hosted binding model. These are useful follow-up candidates, especially if local evaluations show a better cost/completion tradeoff. The initial adapter intentionally accepts only the tested GLM identity; adding another model requires a capability/effort profile, provider schema verification and the same transport/agent-loop tests.

## Validation performed

- Routing policy/persistence/migration tests: 19 passed.
- Hosted settings/configuration workerd tests: 24 passed; five additional workerd admission/immutability/portability regression tests passed.
- Adapter and GLM identity tests plus real Rust/WASM tool-loop integration: 16 passed. The loop test mocks only the provider binding, executes a real tool handler, replays its result, and verifies no WebSocket probe.
- Cloudflare agent tests plus WASM loop: 28 passed.
- Rust API suite: 200 passed; targeted GLM, managed identity, subagent tool and Responses-event parser checks also passed.
- Managed TypeScript and public JS declarations passed; WASM build and package artifact checks passed.
- Worker deployment dry-run passed with `--containers-rollout=none`. The ordinary build bundled the Worker but could not build unrelated container images because this Hand lacks the Docker CLI.

No production settings were changed. Run the targeted test scripts again after changing model schemas or routing policy.

## Local follow-up (2026-09-20)

`pnpm --dir js/managed test:routing` also runs the combined SQLite/Rust-WASM regression test. The two-turn fixture calls Jev once, dispatches four GLM requests through the adapter, executes two real tool handlers, and checks conversation replay. Closing and reopening SQLite preserves the selected model and thinking despite a changed proposed policy. A second case checks persisted ChatGPT fallback after classifier failure. Only model responses are mocked; this combined fixture mirrors the Durable Object transaction rather than driving HTTP admission. Separate workerd admission tests cover that boundary.

Local verification: 19 policy tests, two combined runtime tests, 30 workerd settings/admission/configuration/schema tests, and 16 adapter/identity/WASM tests passed (overlapping coverage, not independent model evaluations). A local managed Worker returned HTTP 200 from `/health` and HTTP 401 for unauthenticated agent creation. The temporary server was stopped afterward. `wrangler whoami` reports no authentication on the test Hand, so real Jev/GLM inference, model quality and actual billing remain untested.
