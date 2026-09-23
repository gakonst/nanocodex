# Text inference after memory caching and regional relay deployment

Measured 2026-09-23 02:45:45–02:48:07 UTC on source `a2f56cc296762a0ed395f536bd927b76963cd2c4`, which includes inference changes `74a2c2a3e`, `0ea29f5b1`, and `a692e7d44`. Fixed Astra, low thinking, standard reasoning, fast=false; routing and automatic routing disabled and verified.

Warm admission fell from 190–307ms in the preceding cohort to a recorded 0ms on all eight non-cold turns. Six non-cold turns have retained fine-grained logs showing personal and team Markdown cache hits and 0ms scope reads. The HTTP warm-1/warm-2 fine spans are missing; their event-based admission remains observable. The result establishes removal of repeated context RPCs where hits are observed. Cloudflare application clocks can remain frozen during synchronous execution: 0ms does not establish zero CPU or zero total API overhead.

## Client and server measurements

Two scratch agents, ten turns, simultaneous WS and SSE observation of the same events. Creation and initial client-stream setup are excluded from TTFT. All values below are milliseconds. Connection is remaining foreground wait on preconnection and is nested within the model phase; do not add overlapping columns.

| Turn | Client WS TTFT | Client SSE TTFT | Server admission | Connection wait | Response retries |
|---|---:|---:|---:|---:|---:|
| HTTP cold | 8056.7 | 8015.0 | 1942 | 2198 | 1 |
| HTTP warm 1 | 1805.5 | 1756.2 | 0 | 0 | 0 |
| HTTP warm 2 | 2355.2 | 2314.9 | 0 | 0 | 0 |
| HTTP after 36s idle | 9108.7 | 9067.2 | 0 | 1031 | 1 |
| HTTP cancel attempt | 1686.4 | 1653.9 | 0 | 0 | 0 |
| WS cold | 4871.0 | 4859.8 | 833 | 1245 | 0 |
| WS warm 1 | 2213.0 | 2204.0 | 0 | 0 | 0 |
| WS warm 2 | 2761.0 | 2749.3 | 0 | 0 | 0 |
| WS after 36s idle | 4364.7 | 4344.8 | 0 | 0 | 0 |
| WS cancel | 2830.9 | 2804.1 | 0 | unavailable | unavailable |

Nine turns completed; the WS cancellation ended cancelled. The HTTP cancellation returned 200 but completion won the race. WS cancellation reached the SSE terminal in 72.7ms and the WS terminal in 100.8ms. Both observation channels agreed on text, event identities and cursors for all ten turns; two replay probes were exact in both channels. There was one accepted event per turn, no duplicated output and no harness failure. Both scratch agents were deleted and verified absent; tails stopped.

The four ordinary warm turns used retained provider sockets with no new connection or retry. Their model telemetry recorded a first upstream event after 206–280ms and first output after 1677–2764ms. These include transport and upstream processing; they do not isolate provider compute. Client TTFT was 1805–2761ms. Separate clocks/resolutions mean subtracting these values from client intervals is not a valid overhead estimate.

## Cold admission and retries

The WS cold session recorded runtime readiness 609ms, account-context readiness 616ms and Markdown injection 217ms, yielding 833ms admission. Catalog and vault overlapped; personal/team Markdown reads also overlapped (101/217ms). Startup append recorded 0ms and was still awaited. Its provider connection overlapped preparation. HTTP cold fine admission spans were absent from the retained tail.

Two transport failures remain visible:
- HTTP cold: first attempt closed during receive after 2274ms; full-history retry opened a new socket after 192ms local backoff. Account-wide Cloudflare Observability recovered this exact-turn event after the live tail missed it.
- HTTP idle: generation 2 failed during send (`send_transport`); a full-history retry opened a new socket, with 192ms backoff and 1031ms connection wait. The specific underlying close/buffer condition is not established.

The HTTP idle 9109ms outlier includes retry and later model waiting. It cannot all be assigned to provider compute, container cold start, or the retention policy. The WS idle socket remained open. The prior ten-turn cohort had no retries; this small sequential comparison does not prove the new deployment caused or fixed either transport failure.

## Physical placement

The new WNAM-constrained text application allocated its observed controller instance in **dfw14**. The old unconstrained instance was **dfw07**. Region configuration has therefore not demonstrated SJC proximity. The existing credential broker previously traced in MXP remains the same durable identity; hints cannot relocate it.

Two exact egress-request → controller-relay → container-log joins give the following Node upgrade measurements:

| Sample | Container colo | Process age | Node total | DNS | TCP | TLS | First upstream byte |
|---|---|---:|---:|---:|---:|---:|---:|
| HTTP idle reconnect | DFW | 501.3s | 400.56ms | 4.78ms | 2.71ms | 93.73ms | 298.89ms |
| WS cold session | DFW | 520.5s | 338.40ms | 5.19ms | 3.22ms | 5.90ms | 323.66ms |

Both containers were already running, and both outbound sockets were new. “Cold session” is not “cold container.” Controller timings were obtained separately; fields contain overlapping intervals and must not be summed. The HTTP sample is the idle reconnect, not its missing initial upgrade. Session-to-egress correlation is by exact derived subject; turn/connection-generation IDs are not yet propagated into egress, limiting per-attempt attribution.

## Provenance and comparison limits

Account, egress and managed Workers each had one 100% deployment throughout the cohort. Their full source metadata matched a2f56cc; a fresh provider receipt at 03:03:42 UTC confirmed the same versions and health 200. Voice relay RPC=true and runtime idle retention=300000ms were verified from allowlisted deployed variables. The unchanged Connect API was also recorded in cohort receipts.

The preceding cohort was 01:51:40–01:54:05 UTC on ff442e28b. Its end receipt failed to read; a fresh 01:55:05 provider receipt matched all starting deployment IDs and versions. Preserve that failure and reconciliation. These are sequential cohorts with changing provider/client conditions, two ordinary warm samples per transport, and incomplete fine-tail coverage (7/10 current turns). Do not report causal total-TTFT percentages or p95 from them.

Private artifacts contain exact joins, deployment receipts, hashes, 224 cohort-filtered log records and the separate recovered HTTP retry event. Raw account-wide tails and operational identities remain outside tracked source. The harness is `harness/measure.mjs`; voice connection setup is reported separately.
