# Conversation preparation: implementation and live measurements

## Result

On the same deployed patch, three alternating pairs of fresh agents measured
**5.78 s median send-to-first-text without preparation versus 2.43 s with a
five-second preparation lead**. Both arms waited five seconds after their event
socket became ready; only the prepared arm called the new endpoint. This is a
small experiment, not a percentile estimate or a guaranteed 3.35 s improvement.

The stronger component result: all three prepared turns reused their model
connection and account metadata. Their admission and connection durations
reported 0 ms at timer resolution. Preparation shifts initial work before Send;
bounded snapshots also prevent admission from repeating those account reads.

| Metric, median (range), milliseconds | Event socket only, n=3 | Prepared, n=3 |
| --- | ---: | ---: |
| Send → first text | 5,782 (4,449–8,357) | 2,427 (2,416–3,842) |
| Durable turn admission | 610 (528–697) | 0 (0–0) |
| Model connection during turn | 1,435 (1,375–1,759) | 0 (0–0) |
| Model call → first output, including connection | 4,888 (3,541–7,090) | 2,204 (1,999–3,472) |
| Client TTFT minus model first-output duration | 908 (894–1,267) | 370 (223–417) |

Connection is nested inside the model call. Do not add those rows. The last row
is a diagnostic residual covering admission, request routing and output delivery;
it is not an independently instrumented relay span. Medians are not additive.

## What changed

- `POST /v1/agents/:id/prepare` acknowledges with 202 and starts session-owned
  runtime/socket preparation, personalization, and first-turn account metadata.
  It does not create a turn or issue a `generate:false` model request.
- Active conversation hooks are implemented in the JS managed transport,
  desktop runtime, web conversation adapter, Rust managed transport, and iOS
  focused conversation. Prompting and voice media do not await this HTTP call.
  Passive low-level event/history subscriptions remain lazy.
- Connector/MCP catalog, hosted-tool discovery and startup account metadata
  reuse bounded in-memory snapshots, at most the existing 120-second access
  interval. Owner/authority changes, shutdown and settings replacement invalidate
  relevant snapshots. Failed reads can retry. Explicit account-info requests
  still force live discovery; tool execution retains its authorization checks.
- Preparation uses the configured 30-second runtime idle lifetime. It does not
  keep inactive conversations alive indefinitely. Reopening renews preparation.
- Transport logs now retain turn/request correlation, failure phase/class,
  connection generation, retry backoff and socket-reopening flags, without raw
  provider errors, credentials or prompt frames.

## Reads and where time went

Unprepared admission performed three remote discovery reads in parallel:

| Stage | Unprepared duration range | Prepared admission |
| --- | ---: | --- |
| Account catalog, also consumed by MCP discovery | 515–697 ms | Snapshot reused; no catalog-read span |
| Account vault metadata | 528–614 ms | Snapshot reused; no vault-read span |
| Account hosted tools | 349–438 ms | Reused; wrapper reports 0 ms |

The prepared arm paid 573–615 ms for this work before submission. The preparation
HTTP acknowledgement took 69–83 ms; acknowledgement does not mean that the
provider socket is ready. There were no model warmup requests, tools, or response
retries in any of the nine valid trials.

Named admission read counters fell from 17 `session_state` reads plus two
`session_initialization_ownership` reads to 14 `session_state` reads. Their
synchronous durations all reported 0 ms. Full SQL auditing was not enabled;
these counters are not a complete statement/row audit, and 0 ms does not mean
zero CPU cost. This experiment does not establish that further local SELECT
deduplication would materially improve TTFT.

After successful preparation, the largest remaining measured interval is the
model call to first output, 2.00–3.47 s. The remaining client/model residual is
223–417 ms. These traces do not separate provider queueing from inference and
network delivery; an additional provider-side attribution is needed before
calling one of those the next bottleneck. No retry was reproduced, so this patch
improves retry diagnostics but does not claim to fix a specific reconnect cause.

## Boundaries and regression found live

- **Immediate Send:** 5.97 s TTFT, 188 ms admission and 1.77 s connection. The
  request was submitted without waiting for preparation acceptance. Unfinished
  shared setup still costs time; preparation cannot make immediate cold Send warm.
- **35-second idle:** the exact session trace records `idle_shutdown` about
  30 seconds after preparation, then fresh runtime construction on Send. TTFT was
  5.10 s, admission 314 ms and connection 1.83 s. Idle cleanup also invalidated
  the discovery snapshots. This confirms the bounded lifetime and its tradeoff:
  the initial preparation benefit expires if the user waits longer.
- **Relay regression:** the first canary received 400 because an empty HTTP POST
  arrived as a non-null, zero-byte stream. The endpoint now distinguishes empty
  streams from actual content. A regression test covers both cases. That canary
  still completed its turn, but is excluded from preparation results. A corrected
  canary acknowledged in 67 ms, with 0 ms admission/connection and 3.35 s TTFT.

## Provenance, validation and release status

Base and freshly fetched `origin/master`:
`68a7f4edc39dfdd35388270160c0ecf935848658`. At measurement time, the patch was **uncommitted and unpushed**.
Final production managed Worker: `4f6c836f-921d-475c-a4a5-64906c1a7bb9`.
Only the managed Worker was deployed; existing container images were retained
using the documented code-only deployment flow. One upload failed with EPIPE;
retrying succeeded. Account, egress and connect were not redeployed by this task.

All measured final-version cohorts have unchanged start/end deployment receipts.
Exact turn IDs match the final Worker version in the server traces. Trials used
`gpt-6-astra`, low effort, standard reasoning, fast mode off, with the identical
prompt `Return exactly 42. Do not call any tools.` Fresh agent identities are
not proof of cold Cloudflare processes or cold provider infrastructure. Server
durations and client monotonic TTFT are retained separately; clocks across
isolates are not used for latency subtraction.

Focused Worker, SDK, desktop, web, Rust managed and Swift core checks passed, as
did SDK/package/type checks and the iOS simulator app build. The final empty-POST
regression and Worker typecheck were rerun after that fix. The initially
misfiltered full Worker suite hit an unrelated cross-DurableObject runner error;
this is not a full-suite green claim. Validation details are in the manifest.

**At measurement time, client hooks had build/test coverage but had not been
released or installed on the Mac or phone.** These live measurements explicitly
invoked the endpoint through the production public API. No real-audio, physical
device, direct Codex comparison, sandbox, or tool-using turn benchmark was rerun
in this window. In particular, this is not a new voice media-readiness claim.

All ten scratch agents, including the rejected-preparation canary, returned 404
after deletion. Tail collectors are stopped. API invisibility does not prove
completion of every deferred storage cleanup alarm.

## Evidence and reproduction

- [Measurements and exact turn IDs](hosted-measurements.json)
- [Correlated server traces](hosted-traces.json)
- [Immediate/idle-expiry measurements](boundaries-measurements.json) and [traces](boundaries-traces.json)
- [Corrected canary](canary-fixed-measurements.json)
- [Source hash, deployment receipts, validation and cleanup](manifest.json)

Raw sanitized tails, source patch, build/deploy logs and reusable harnesses are
under `output/conversation-preparation-20260916/`. The harness reads local
credentials without printing them and deletes its scratch agents. To reproduce
from this checkout, start `tail.py`, run the paired cohort below, stop the owned
collector, then run `analyze.py`. The five-second interval models time spent in
an open conversation; it is not a mandatory delay added to Send.

```sh
REPS=3 ARMS=events_only_5s,prepare_5s macos/Resources/runtime/node output/conversation-preparation-20260916/hosted.mjs hosted
```
