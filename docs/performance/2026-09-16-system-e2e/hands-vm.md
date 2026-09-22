# Hands and VM E2E — 2026-09-16

Fresh measurements, 23:20:57–23:27 UTC. All six journeys and 18 shell calls succeeded. Three prepared VM mounts took **1,823 / 1,914 / 1,782 ms**. The VM itself was claimed in **0.68–0.90 ms**; connecting its tool and screen publishers remains the largest measured service cost.

[Sanitized correlated measurements](hands-vm-measurements.json) include exact agent/mount/allocation/call IDs, Worker stage events, host spans, read counters and before/after installation state. This was measurement only: no source change, deployment, restart or installation.

## Runtime and state

- Mac installed release CLI: `nightly-92528e17b4feb63fed238abc2f2b766777fc24ff`, reports `nanocodex2 0.6.1`; SHA256 `3e3ccc3e3093f9b7049b31e03d6fa1b6099bda356674b60d1b94993bdde35ad0`.
- Managed Worker: `e1877ce4-0170-4de6-9cf9-ccfc003b7ece`, 100%, deployment `52011c4c-236b-439f-9cea-64ccad4c90cf`. Read-only deployment queries before and after agree; deployment timestamp 23:12:21Z.
- Linux installed release is the same nightly, installation revision `5bd37aeb25c6a3d2021642ff`. Native machine `0dbfda66-12e4-4bcf-b1a9-994a052b3181`; user factory `linux-paradigm`; Ubuntu 22.04/KVM, 2 guest vCPUs and 4096 MiB RAM.
- Native/factory PIDs remained `1007910` / `1009041`, active with zero restarts. Installed executable hashes, workspace, identity and release remained unchanged. No temporary performance overrides or binaries.
- Every sample starts a fresh installed CLI process and fresh managed agent. The existing Linux Hand, factory, account and immutable base cache were already warm. Each VM is a new allocation claimed from the existing prepared spare; **none of these mount measurements is a cold VM boot**.
- `NANOCODEX_DISABLE_HAND=1` keeps the benchmark CLI from altering the existing local Mac Hand. It otherwise uses its ordinary managed tool path. Model: `gpt-6-astra`, low thinking, fast mode. A preceding discovery/mount call and a separate model cell with three sequential shell calls exercise the real agent journey.
- Root and screen/voice agents waited during these cohorts. VM3 was retained afterward for three read-only screen samples and then deleted.

## Service timings

All values below are milliseconds from emitted tool duration fields, excluding model generation between tool calls. Repeated shell calls execute consecutively within one code cell.

| Journey | Discovery / mount | First shell | Second shell | Third shell |
|---|---:|---:|---:|---:|
| Native 1 | 609 | 462 | 382 | 384 |
| Native 2 | 661 | 448 | 434 | 414 |
| Native 3 | 868 | 432 | 408 | 401 |
| VM 1 | 1,823 | 282 | 130 | 130 |
| VM 2 | 1,914 | 257 | 123 | 119 |
| VM 3 | 1,782 | 274 | 144 | 141 |

Native discovery is `accountInfo`; VM discovery is an explicit `mount` of `linux-paradigm`. Native commands print a fixed marker. VM commands additionally verify the installed guest executable exists. Every call returned exit zero and the expected marker.

Native host `attachment.call.completed` spans were **4.49–5.45 ms** across all nine calls. Correlated `hand.call.provider` spans exactly reproduce the native service durations above, with fetch consuming the measured duration and decode recording zero. Thus local command execution is a small part of the 382–462 ms service path. These traces do not fully partition account broker transport versus routing; that remainder must not be labeled network latency without further evidence.

For VM first calls, an already-ready retained mount is checked again: readiness RPCs cost **141 / 128 / 127 ms**, then the command runs. The next two calls in that code cell avoid this preparation, accounting for much of the first-versus-repeat difference. Whether that validation can safely be reused across cells needs a separate correctness review.

## Actual CLI journey

These are same-process monotonic client-arrival measurements. They include the model and event delivery; they are not the same clock as server tool timers.

| Journey | CLI start→first shell result | Discovery/mount call→first shell result | Discovery/mount result→next shell call | CLI start→run complete |
|---|---:|---:|---:|---:|
| Native 1 | 11,102 | 4,649 | 3,718 | 14,043 |
| Native 2 | 13,662 | 4,608 | 3,638 | 18,721 |
| Native 3 | 13,253 | 5,561 | 4,341 | 17,088 |
| VM 1 | 16,820 | 7,582 | 5,530 | 24,479 |
| VM 2 | 13,815 | 5,976 | 3,905 | 16,467 |
| VM 3 | 14,786 | 7,490 | 5,577 | 17,443 |

The 3.6–5.6 second gap between the preceding result and next tool call is larger than mounting or command service time. It is the observed agent continuation interval for this explicitly two-cell prompt; these traces do not split provider inference, scheduling and event delivery inside it. It should not be reported as VM boot time or as pure inference time. Parent agent/API cohorts measure model TTFT separately.

## VM component timings

Mount spans correlate by mount ID; factory spans correlate by allocation ID. Each value comes from a timer within its own process/handler. No cross-isolate or Linux↔Worker epoch subtraction is used.

| Component | VM 1 | VM 2 | VM 3 |
|---|---:|---:|---:|
| Account reservation request | 181 | 149 | 172 |
| Local prepared spare claim | 0.680 | 0.896 | 0.816 |
| Tool publisher attached, since factory provision start | 1,331 | 1,529 | 1,230 |
| Screen publisher ready, since factory provision start | 1,020 | 1,119 | 1,372 |
| Caller mount settled | 1,823 | 1,914 | 1,782 |
| Readiness poll count through settlement | 9 | 10 | 9 |
| First-command retained readiness check | 141 | 128 | 127 |

Tool and screen attachment overlap; do not sum their times. All new-agent traces contain `agent_scope_known_empty` and only acquire the account pool, confirming the removed absent-agent-pool probe remains absent. Caller `intent`, scope lookup and final settlement bookkeeping record zero at Workers clock granularity, which is not proof of zero CPU cost.

### Tool attachment breakdown on the Linux factory

| Component | VM 1 | VM 2 | VM 3 |
|---|---:|---:|---:|
| Catalog preparation | 0.067 | 0.097 | 0.087 |
| TLS trust preparation | 5.668 | 0.003 | 0.003 |
| Address resolved, cumulative | 76.245 | 0.269 | 0.546 |
| TCP connected, cumulative | 96.439 | 20.198 | 20.834 |
| Full WebSocket connect | 1,028 | 1,280 | 994 |
| Catalog send | 0.081 | 0.069 | 0.072 |
| Catalog acknowledgment | 302 | 248 | 235 |

Most connection time remains after TCP connect and before WebSocket upgrade completes. These logs do not isolate TLS from backend authorization, routing or upgrade. Catalog acknowledgment is another measured 235–302 ms serialized after connect. Local catalog construction is negligible in this cohort. Screen attachment runs concurrently and wins the critical path in VM3.

Readiness polling overlaps attachment; its waits cannot be added as independent removable latency. Pool `provision_dispatch`, `acquire_response` and readiness events are preserved. A complete factory-reply/Worker-receipt span was not emitted in the available tail, so dispatch/return transit is not independently measurable here.

## Fresh guest refill, outside the mount hot path

Automatic refill after each claim boots an isolated unassigned guest. This supplies actual new guest evidence without stopping a shared service or user VM. The root image and host page cache are warm; it is not a fully cold-container/storage test.

| Cumulative factory stage | Refill 1 | Refill 2 | Refill 3 |
|---|---:|---:|---:|
| Private root prepared | 8.24 | 7.66 | 7.85 |
| Guest ready | 262.71 | 282.71 | 268.92 |
| Guest + local desktop/capture ready | 873.67 | 994.95 | 880.40 |

These are cumulative within each refill, not additive. Claims use previously prepared guests, so these 0.87–0.99 second refills are background work. Uncached image preparation, exhausted spare pool, fresh factory boot, Windows and Mac VM startup are not measured by this cohort.

## Reads, SQL and deletion

Production did not emit `managed.sql_batch` events. Existing named `managed.performance.reads` counters are preserved per exact related trace ID; they are partial instrumentation, not a full SQL inventory. Examples:

- `/credential-owner` scope: one session-state read plus one initialization-ownership read.
- `/create-live`: four session-state reads and one ownership read.
- `/tool-host`: two session-state reads; `attachment.router_ready` recorded zero ms on the ready-router path.
- Native turn admission: 17 session-state reads and two ownership reads in its encompassing scope.
- Native DELETE: two session-state and two ownership reads; VM DELETE: three session-state and two ownership reads.

Nested scopes overlap and must not be summed. Their zero read durations are limited by the Workers timer; no claim is made that these SQL operations are free or that other SELECT statements do not occur.

| Delete cohort | CLI wall | Managed DELETE scope | `delete.containers` scope |
|---|---:|---:|---:|
| Native 1 | 2,728 | 2,091 | 1,026 |
| Native 2 | 2,940 | 2,191 | 1,054 |
| Native 3 | 2,971 | 2,197 | 961 |
| VM 1 | 3,253 | 2,616 | 1,169 |
| VM 2 | 3,353 | 2,478 | 1,238 |
| VM 3 | 2,973 | 2,267 | 994 |

Deletion spends about a second in the containers cleanup stage even for the native-only cohort. That stage label is measured; this trace does not prove a container was created or identify each subordinate cleanup request.

All six CLI deletes returned success. VM1/VM2 allocation files were gone before VM3 deletion; after screen viewers closed and VM3 was deleted, the factory allocations directory was empty. Before/after service state proves no restart and no change of identity/runtime. The shared tail also observed archive/deletion-recovery failures during the broader window, but none matched these six agents’ exact Durable Object IDs in the captured records; see [scoped warning checks](lifecycle-warnings.json). Physical VM cleanup is verified, but successful CLI deletion alone is not evidence that every archive cleanup step completed.

## Where the evidence points next

1. Agent continuation after discovery/mount: 3.6–5.6 s in this journey; diagnose alongside model TTFT traces.
2. VM attachment: WebSocket connect 0.99–1.28 s plus catalog acknowledgment 0.24–0.30 s; screen publication can also determine readiness. Split the remaining upgrade path before proposing another optimization.
3. Native command service path: 382–462 ms versus 4.5–5.5 ms on the host. Broker/routing spans are incomplete here.
4. First VM command: an additional 127–141 ms readiness check on an already-mounted VM.
5. Deletion: 2.1–2.6 s managed scope, with ~1 s labeled containers cleanup, plus separately observed archive recovery errors.

Three samples establish the shape of this warm steady-state path, not tail percentiles or fully cold behavior.
