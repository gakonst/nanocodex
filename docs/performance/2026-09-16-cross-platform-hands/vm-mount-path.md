# VM mount reservation and readiness — 2026-09-16

New agents were spending **1,316 / 1,680 ms** attempting to acquire a factory
from their empty agent-specific pool before trying the account pool. The new
code skips that attempt only when durable local state proves the pool is empty.
After deployment, reservation took **295 / 195 / 141 ms** and the skipped phase
was absent from every trace. Complete mount times were **2,937 / 1,832 / 1,603 ms**.

## Real before/after cohorts

All six samples created a fresh agent, mounted one VM from `linux-paradigm`,
ran a guest command that checked the runtime and wrote private filesystem
markers, and deleted the agent. The same installed Linux nightly and service
PIDs remained active throughout. These are individual samples, not percentiles
or a controlled estimate of platform variance.

| Sample | Full mount | Empty agent-pool attempt | Account reservation | Reservation → settled | Factory tools ready | Factory desktop ready |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Before 1 | 3,383 ms | Not captured | Not captured | Not captured | 1,218 ms | 954 ms |
| Before 2 | 2,706 ms | 1,316 ms | 151 ms | 1,239 ms | 1,170 ms | 1,046 ms |
| Before 3 | 3,178 ms | 1,680 ms | 153 ms | 1,345 ms | 1,139 ms | 1,014 ms |
| After 1 | 2,937 ms | Skipped | 295 ms | 2,642 ms | 1,212 ms | 2,202 ms |
| After 2 | 1,832 ms | Skipped | 195 ms | 1,637 ms | 1,435 ms | 1,082 ms |
| After 3 | 1,603 ms | Skipped | 141 ms | 1,462 ms | 1,233 ms | 1,001 ms |

The first post-deployment sample is included: its desktop publisher's WebSocket
connection took **2,015 ms**, while tools attachment took 1,212 ms. The publisher
trace measured DNS at 0.34 ms and TCP at 20.62 ms, leaving the longer wait in the
TLS/upgrade/backend interval. The trace does not establish its precise cause.
Both tools and desktop must be ready before allocation acknowledgement, so this
sample could not settle when its tools alone became ready.

The observed three-sample mount medians are 3,178 ms before and 1,832 ms after.
The stronger conclusion is the directly verified removal of the unnecessary
reservation phase; the full-mount observations also include connection and
publication variation. The first baseline has tool/factory evidence but lacks
VM-stage tail events; restarting the tail after deployment captured the remaining
five samples. No baseline stage timings were reconstructed for that first sample.

## What each trace measures

`vm.mount.stage` records, on the calling Session's clock:

- Durable mount intent, locator computation and each scope's acquire round trip.
- The retained reservation and every readiness response, including poll number.
- Local machine-route visibility and final mount settlement.

`vm.pool.stage` records the matching mount/allocation IDs at reservation response,
provision dispatch, factory acknowledgement and readiness reply. Factory logs
record local claim, guest, tools attachment and desktop publication durations.
The caller's 1,316/1,680 ms empty-pool phase includes its selection-intent write,
request dispatch and remote lookup. It is **not** an isolated measurement of
network time or constructor CPU.

Absolute timestamps from different Worker isolates or Linux are retained for
correlation, but are not subtracted to invent one-way command delivery or return
latencies. Such subtraction produced inconsistent ordering in the concurrent
screen investigation. Factory columns in the table overlap one another and are
nested within the mount journey; they must not be added to the caller columns.
Pool acknowledgements and polling explain the completion protocol, but this
cohort does not individually resolve their transport/durability barriers.

## Policy and recovery

A small durable state has three meanings: no row means legacy/unknown, zero
means a newly initialized agent with no factory registration, and one means a
factory may exist. Fresh initialization writes zero in the existing creation
transaction. Authenticated agent-factory registration writes one before allowing
the WebSocket upgrade. Failed upgrades, offline hosts and initialization replay
never clear that fence.

Only an unselected, proven-empty agent scope is skipped. A retained allocation
uses its original pool. A retained selection intent continues to probe its
selected scope, and all candidate locators remain available for validation.
Existing agent/account/system precedence and release-intent recovery are
preserved. The marker is read after asynchronous locator computation, immediately
before choosing the first scope, so a concurrent registration is not hidden by
an earlier cached empty value. Legacy agents retain the conservative lookup.

This removes one cross-object acquire and its selection-intent update from the
new-agent mount path. It adds a local marker read to that decision and one marker
insert to initial creation; registration changes the marker conservatively.
Readiness polling is unchanged and still waits 100 ms between replies. Its waits
overlap factory work, so summing all polling sleeps would exaggerate removable
latency.

## Remaining ranking and verification

The measured remaining waits are:

1. Factory attachment/publication: tools 1.21–1.44 s after deployment; desktop
   1.00–2.20 s in parallel. The longer branch gates acknowledgement.
2. Account reservation: 141–295 ms in the new cohort.
3. Readiness/control return and settlement around those operations: still
   included in the caller's 1.46–2.64 s post-reservation interval, without a
   defensible independent one-way breakdown.
4. Local prepared-VM claim: 0.66–0.81 ms after deployment.

Twelve focused tests pass: three scope/lifecycle tests and nine VM-pool policy
and protocol tests. TypeScript checks pass. Tests cover unknown state, fresh
state, retained selections, registration fencing, denied registration, failed
upgrades, replay and persistence across eviction. The eviction fixture disables
unrelated personalization warmup; it retains the actual eviction assertions.

Before Worker: `30220bd9-b9f8-414e-90c9-625843a452cb` (source `1dada0ac3`).
After Worker: `a0afbf49-c783-4803-9320-8498def61e38` (source `8f41f2cfd`).
Linux remained on published `nightly-92528e17b4feb63fed238abc2f2b766777fc24ff`,
installed revision `5bd37aeb25c6a3d2021642ff`; native PID 1007910 and factory PID
1009041 both stayed active with zero automatic restarts. No temporary executable
or service override was introduced.

All six scratch agents were deleted and the allocation directory was empty.
The second baseline VM also served the screen agent's read-only viewer tests
before deletion. [Curated traces, tool results, final state and artifact hashes](vm-mount-path-measurements.json)
retain every sample; raw local evidence is under `output/vm-mount-path/`.
