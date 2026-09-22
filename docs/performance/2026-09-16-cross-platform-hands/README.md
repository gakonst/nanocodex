# Computer Hands and VM startup — 2026-09-16

## Latest component measurements

The subsequent [VM mount path investigation](vm-mount-path.md) removes a measured
1.3–1.7-second empty agent-pool lookup; complete after mounts were 2.94/1.83/1.60 s.
The [local viewer authority report](screen-local-authority.md) records cached
viewer readiness falling from 1,058 ms to 366 ms median by verifying existing bounded
authority locally before the broker upgrade. Two earlier routing variants were
measured and reverted because they did not improve startup.

The [Hand call and attachment report](hand-call-latency.md) records the deployed
SQL/read reduction, router reuse, and the remaining transport/durability waits.
The [screen admission report](screen-admission.md) records short-lived authority
reuse, decoded WAN frames, and request-ID-correlated account proxy spans.

| Component | Before | After | What the evidence supports |
| --- | ---: | ---: | --- |
| Linux spare replenishment | 10.3–10.9 s | 0.87–1.04 s | Cached immutable base plus private overlay; first base copy is still 7.2 s |
| Native Hand call median | 407 ms | 446 ms | Two fewer SELECT queries; no demonstrated end-to-end speedup |
| Warm VM WebSocket connection | 1,305–1,368 ms | 979–983 ms | Repeated catalog discovery removed from attachment |
| Fresh-agent mount of a prepared VM, median | 3,178 ms | 1,832 ms | Three samples each; removes a traced 1.3–1.7 s empty-pool lookup; first after sample remained 2,937 ms |
| Screen authentication | 189–196 ms live | No observed I/O wait with snapshot | Reuses credential-bound, 120-second authority; live renewal remains |
| Cached screen viewer ready, median | 1,058 ms | 366 ms | Four interleaved samples each; local verification removes the managed call before the existing broker upgrade |
| Linux native first decoded frame, median | 1,423 ms | 1,253 ms | Three-sample WAN cohorts, affected by host/network variation |
| Linux VM first decoded frame, median | 1,530 ms | 1,182 ms | Three-sample WAN cohorts, affected by host/network variation |

These are separate, nested measurements, not additive intervals or percentile
estimates. Same-Mac WebRTC did not improve under heavy concurrent build load.
See each report for exact deployment versions, trace IDs, timer limitations,
validation, and unresolved intervals. [Installed Apple app verification](installed-apps.md)
records the real Mac host/VM journey and signed iPhone build.
[Nightly publication and updater checks](nightly-publication.md) record the
released artifact hashes and installed CLI bundle. No real Windows
latency or physical phone first-frame result is claimed.

See the [native screen startup follow-up](screen-startup.md) for ICE prefetch,
initial frame delivery and bitrate controls. The subsequent
[Linux recovery](linux-recovery.md) resolves the stale publisher call rejection.
The [installer and release repair](linux-installer-fix.md) fixes fresh-image
packaging and VM teardown, with live installation and deletion evidence.
[Linux overlay disks](vm-overlays.md) reduce repeated spare preparation from
10.3–10.9 seconds to 0.87–1.04 seconds and verify retained writes across restart.

See [further Hand component measurements](hands-components.md) for the inventory
and Mac capture improvements, native WebRTC startup breakdown, and remaining
connection/tool overhead.

See the subsequent [remote-screen measurements and rollout](remote-screen.md)
for the bounded frame pipeline, shared TLS configuration, and live native
viewer comparison (1.91 → 7.62 fps). Screen connection/catalog latency remains
separate from frame throughput and local VM handoff.

## Changes

- Windows now runs the same shared computer publisher as macOS/Linux, using a
  local named pipe. The CLI and desktop hold separate leases on one authenticated
  account/OS-user identity. Closing one client preserves the others; closing the
  last drains the publisher and its VM factory.
- Linux/Windows use their platform data directories. Windows native execution
  stays on Windows; a configured WSL2 distribution with accessible KVM runs the
  VM factory. Credentials enter through environment variables, not arguments.
- Linux systemd installation advertises the existing factory on the native
  computer with `hand --vm-provider`. Explicitly retained server identities and
  workspaces remain intact.
- VM tools registration overlaps desktop startup, with allocation readiness still
  requiring both. Desktop failure detaches any tools registration before teardown.
- Desktop startup waits for Openbox's actual startup callback. Previously it
  launched xterm as soon as `_NET_SUPPORTING_WM_CHECK` appeared, during Openbox
  initialization. xterm could wait five seconds for a geometry response or fail
  to acquire focus. We retain normal X11 timeouts and verify terminal focus.
- Guest readiness records cumulative Xvfb, Openbox, xterm and capture timings;
  host traces record root cloning, guest boot, local desktop, screen and tools
  registration. These make future cold-start regressions attributable.

## Measurements

[All six measurements and correlated stage events](measurements.json),
[artifact hashes](binaries.json), [five-second syscall timeout](xterm-timeout.txt).

Each run started a dedicated factory, allocated one fresh private GPU VM with
2 vCPUs and 2 GiB RAM through the deployed managed `mount` tool, ran `uname -s`
and printed a marker in a subsequent Code Mode cell, deleted the agent, and
stopped the factory. All six executions returned Linux and the expected marker.
No previous VM was intentionally retained between samples.

The host and guest binaries were release builds. Baseline and guest-fix-only
runs used the same host binary. Baseline guest includes timing instrumentation;
optimized guest changes the Openbox readiness barrier. The combined host adds
concurrent tools registration. Exact binaries are retained under the local
`output/cross-platform-hands/` directory; subsequent source changes also add
normal-level logging of the guest timing JSON and improve an error message.

| Variant | Run 1 mount | Run 2 mount |
| --- | ---: | ---: |
| Baseline, serial registration | 25.290 s | 13.967 s |
| Desktop fix only, serial registration | 12.421 s | 8.432 s |
| Desktop fix + concurrent registration | 6.436 s | 8.989 s |

These are two samples per variant, not percentile estimates. This Mac was also
running other builds/apps. In baseline run 1, Xvfb alone took 6.903 s versus
1.267 s in baseline run 2. Do not attribute that variance to this patch, or
interpret the total before/after difference as a controlled effect size.
The factory's initial registration (4.7–7.3 s) is recorded separately and is
excluded from the already-online factory's mount duration.

### Individual optimization, independently of overlap

The xterm stage took **5.219–5.592 s** in the two real-VM baseline traces and
**0.068–0.160 s** across all four optimized-guest traces. `strace` reproduced a
**5.006094 s X11 `ppoll` timeout**. Changing `waitForWm` did not solve it;
shortening `wmTimeout` lost readiness/focus. Neither workaround is in the patch.
Waiting for Openbox's startup callback removed the stall with default xterm
settings, and screenshots plus Unicode keyboard input passed.

This race is intermittent: separate warm container probes included a baseline
that started in 0.389 s and another that took 5.223 s. Optimized container probes
completed in 0.192–1.639 s, with screenshot and input checks passing in all three.
The Openbox checkpoint now denotes complete initialization; the previous one
only denoted early identity publication, so its stage alone is not directly
comparable before/after.

Openbox's startup sequence publishes its identity before initializing its
subsystems, and invokes `--startup` after entering its running state:
[Openbox startup implementation](https://github.com/danakj/openbox/blob/master/openbox/openbox.c),
[Openbox screen initialization](https://github.com/danakj/openbox/blob/master/openbox/screen.c).
The paired traces establish the cost and observed behavior on our image.

### Remaining time, ranked from the two combined runs

| Component | Observed duration | Interpretation |
| --- | ---: | --- |
| Guest boot/control readiness | 1.816–3.007 s | VMM/guest startup; separate from root cloning |
| Local desktop | 1.374–2.601 s | Xvfb 0.757–1.303 s; Openbox 0.449–1.111 s; terminal 0.068–0.153 s |
| API dispatch/readiness residual | 1.683–1.691 s | Mount duration minus host critical path; not broken down into SQL/network internals in this experiment |
| Screen registration | 1.548–1.658 s | Remote publication after local desktop readiness |
| Root clone | 0.015–0.032 s | Already small; not an optimization priority |

Tools registration takes 1.726–1.824 s but completes during desktop setup, so
it no longer adds that duration to the critical path. First JPEG capture is
16–20 ms in the combined runs. Both readiness conditions remain mandatory.

Next investigations should target guest boot and Xvfb/Openbox initialization,
and trace remote screen registration plus the ~1.69 s API residual. These are
measured remaining costs, not claims that all of their duration can be removed.
The existing trace does not justify a SQL, credential, or relay-specific fix.

## Fresh spare: millisecond local handoff

[Correlated warm measurements and artifact hashes](warm-measurements.json).

The factory now retains one never-assigned VM, including its running desktop,
within `--max-vms`. Previously GPU preflight booted and discarded a VM; it now
prepares the initial spare. Allocation claims that VM's private disk without
copying it, gives it a fresh machine identity, and only then attaches remote
screen/tools credentials. Used VMs never reenter the pool. Retained allocation
roots bypass the spare and restart from their own disk. `--warm-spare=false`
disables this behavior and its idle resource cost.

Three sequential allocations on one release-mode Mac factory (2 vCPUs / 2 GiB
per VM, capacity 4) measured:

| Component | First | Second | Third |
| --- | ---: | ---: | ---: |
| Local claim, including guest health check and durable disk assignment | 39.20 ms | 25.19 ms | 15.07 ms |
| Guest health check within that claim | 0.300 ms | 0.297 ms | 0.501 ms |
| Screen publication after claim | 1.503 s | 1.415 s | 1.647 s |
| Tools registration after claim, overlapping screen publication | 1.523 s | 1.694 s | 2.113 s |
| API dispatch/readiness residual | 1.685 s | 0.594 s | 0.570 s |
| **Agent-visible mount** | **3.248 s** | **2.313 s** | **2.698 s** |

Each mount was followed by a command asserting that a marker from prior VMs did
not exist, printing Linux and the expected marker, then writing that file.
All three succeeded. The traces show a new spare prepared before each claim;
there were three assigned VMs plus one spare at the end, within capacity 4.
The agent was deleted successfully and the benchmark factory shut down.

This is **warm handoff**, not millisecond cold boot or millisecond end-to-end
mounting. Initial spare preparation took 4.235 s; replacements took
3.766–4.169 s. Initial factory readiness took 6.364 s and is excluded from the
mount durations. A burst that outruns replenishment still waits for preparation.
These three samples establish observed behavior, not percentiles or a latency
SLA. They were measured on the same development Mac as the earlier samples.

The remaining critical path is remote tools registration (1.52–2.11 s),
followed by API dispatch/readiness residual (0.57–1.69 s). Screen publication
runs concurrently and finished earlier in these samples. The residual is not
attributed to SQL or authentication without further server-side traces.
Health checking costs under 1 ms; most local claim time is in the durable disk
assignment region between that check and the claim event. We retain the disk
and directory durability operations.

Failure handling preserves existing disks, aborts unfinished spare tasks, and
only falls back to cold startup after preparation cleanup is known to be safe.
The benchmark artifact precedes the final tightening of that error-only branch;
the successful preparation/claim/publication path is unchanged. The final binary
was then tested through two more real allocations with `--max-vms 1`:
[final artifact and capacity-one traces](warm-capacity-measurements.json).
Both returned clean filesystems and successful command output. Local claims
were **28.49 ms and 12.11 ms**; full mounts were **3.785 s and 3.890 s**.
The first agent was deleted before the second allocation, and a fresh spare
replenished in 2.236 s. Partial process sampling (53 samples, approximately
200 ms apart) observed no more than one VMM child. Both agents were deleted
and the factory stopped. Across all five measured claims, local handoff was
**12–39 ms**, while end-to-end mounting remained **2.31–3.89 s**.

## Validation and limits

- Real Mac and Linux CLI/desktop tests: one publisher across two account keys,
  shell execution after one client disconnects, stable identity, final shutdown.
- 25 focused Hand tests pass on both Mac and Linux; 31 VM host lifecycle tests
  pass on both Mac and Linux, including spare disk adoption, cancellation, and
  retained-allocation capacity accounting. Desktop JS: 53 passing, opt-in helper test run separately; focused
  platform-path tests pass. Installer: 5 passing. Managed mount contract: 4 passing.
- Five guest desktop tests pass in Linux, including the real capture, Unicode
  and raw-input, authentication, cleanup, and shutdown test.
- Windows MSVC compilation and test compilation pass. Actual Windows named-pipe
  execution and WSL2 VM startup have **not** been run on a Windows machine.
  A three-OS CI matrix now covers real helper sharing when this is pushed.
- The initial wider managed account run could not load the unbuilt WASM SDK;
  after building dependencies, the subsequent account/relay suites passed.
  See the linked component and remote-screen reports for those results.
- CLI-binary Clippy with warnings denied and formatting checks pass.
- All-target Clippy hits 59 pre-existing TUI benchmark `missing_const_for_fn`
  errors; these unrelated files were left alone.

Raw host JSONL, tool events, syscall logs, build/test output, benchmark harness,
and full guest capture/input probe live in `output/cross-platform-hands/` in the
worktree. No API credentials are included in the curated measurements.

The managed and browser changes have been deployed as recorded in the linked
reports. Native CLI/app changes still require an installed release; the release
probes used here do not replace the user's installed nightly.
