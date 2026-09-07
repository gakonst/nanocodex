# Execution and action checklist

Work through these in the order below, one case at a time. Case IDs match the
[full acceptance criteria](EXECUTION_ACCEPTANCE.md). Each item names the expected
environment and the result to verify.

**Latest completed baseline: `dtolnay/itoa` clone → Cargo test.**

- [x] **B01 — Small-repository clone and test, selected by the user.** Production Worker host clone: 0.642 s; one Cloudflare sandbox mount: 6.660 s; cold Cargo run: 112.089 s, exit 0, 11 integration tests and 2 doc tests passed. Full prompt-to-completion turns: 9.579 s for clone and 132.429 s for Cargo. All 16 upstream files matched by SHA-256; no `.git`. Both final responses survived browser reload. [Evidence](EXECUTION_ACCEPTANCE.md#itoa-baseline--2026-09-07).
- [x] **B02 — itoa reruns in the existing sandbox mount.** Immediate warm Cargo: 4.634 s, 13.923 s for the full turn, no downloads/recompilation, all 13 tests passed. The preceding rerun after approximately 45 minutes idle rebuilt dependencies and took 121.510 s. [Evidence](EXECUTION_ACCEPTANCE.md#itoa-reruns--2026-09-07).

B01 demonstrates the basic E01 → E03 path. The full case variants below remain
pending, including the original larger-repository regression. Next pending
reliability case: E12; the deployed command card still reports the initial yield
as success and omits subsequent output.

Check an item only after its acceptance criteria have been demonstrated on the
relevant deployed service or installed app, with evidence recorded below.
Local patches and passing unit tests count as progress. All 37 cases remain
unchecked until that verification is recorded. Missing software, a device, or
an integration leaves its case pending with the specific dependency noted.

## 1. Reliable execution and shared files

- [ ] **E12 — Correct progress and completion.** Cloudflare Linux sandbox → web and iPhone. Keep yielded commands Running, accumulate output in the original card, and retain the actual successful or failing exit code after reload. Measure elapsed time from command call to observed completion, including time between polls.
- [ ] **E13 — Long jobs and reconnect.** Same Cloudflare sandbox. Run a 60-minute job through app closure, event reconnect, and Worker reconnection; recover its progress and final result without duplicate execution.
- [ ] **E14 — Process/container failure.** Isolated Cloudflare sandbox. Exercise termination, resource exhaustion, and restart; retain output and a truthful interrupted/failed outcome.
- [ ] **E15 — Steering and Stop.** Original Cloudflare sandbox or explicitly selected device. Apply steering to the active turn; stop its work without confirmation or orphaned processes, preserving unrelated work.
- [ ] **E11 — Shared files and filesystem performance.** Worker host ↔ Cloudflare sandbox using `/brain`. Verify acknowledged edits, modes, and timestamps in both directions; record cold/warm measurements and the equivalent local-disk baseline.

## 2. Clone, inspect, build, and select the right computer

- [ ] **E01 — Source-only clone.** Worker host / Just Bash. One clone command creates the expected source tree without `.git` or a sandbox startup.
- [ ] **E02 — File inspection stays on the host.** Worker host / Just Bash. List, read, and search the repo correctly before and after a sandbox is attached.
- [ ] **E03 — Follow-up Cargo test.** Cloudflare Linux sandbox. Reuse or mount once and test the existing `/brain` source; no preliminary host/Mac attempt, re-clone, or manual copy.
- [ ] **E04 — Clone and test in one user turn.** Worker host → Cloudflare sandbox. Complete both steps without another user prompt or mount confirmation.
- [ ] **E05 — Full workspace tests.** Same Cloudflare sandbox. Run `cargo test --workspace` and retain its real final exit, output, and test counts.
- [ ] **E06 — Warm test rerun.** Same Cloudflare sandbox and build directory. Reuse available caches and measure the warm run separately from cold startup.
- [ ] **E07 — Full Git checkout.** Cloudflare sandbox. Native `git clone` / `gh repo clone` produces real `.git`, working `git status`, and history.
- [ ] **E08 — Private repository credentials.** Worker host for source-only clone; Cloudflare sandbox for full Git. Authorized access works; revoked access fails; credentials stay out of outputs and app state.
- [ ] **E09 — Unsupported host capability.** Worker host → suitable Cloudflare sandbox. Escalate once after checking partial effects; ordinary command/test errors stay in their original environment.
- [ ] **E10 — Explicit Mac selection.** Connected Mac. Execute against its actual files and programs, or identify its unavailability without switching computers.

Repeat E01–E06 with an unrelated Mac connected and disconnected. Exercise
retained process sessions across turns and with two commands running so output
and cancellation cannot cross sessions.

## 3. Screenshots, previews, and file outputs

- [ ] **E23 — Public URL screenshot.** Managed browser. Capture the requested page/viewport and return an actual image attachment.
- [ ] **E24 — Current Mac browser tab.** Browser on the connected Mac. Capture that tab's current state and session.
- [ ] **E25 — Phone app screenshot.** Connected phone's supported capture capability. Return the correct app image or identify the supported capture step required.
- [ ] **E26 — Web app preview and responsive screenshots.** Cloudflare sandbox runs the server → managed browser visits its exact preview. Return desktop and mobile viewport images of that build.
- [ ] **E27 — Authenticated page screenshot.** Managed browser or connected-device browser that owns the account session. Capture the correct account/page and handle expired login truthfully.
- [ ] **E28 — Video compression and thumbnail.** Cloudflare sandbox with media tools, or the explicitly selected connected computer. Return playable media and durable downloadable outputs.
- [ ] **E29 — CSV analysis and chart.** Worker host for supported small calculations; Cloudflare sandbox when Python or another native runtime is needed. Verify totals and a usable chart/download.

## 4. Blender, CAD, and simulations

- [ ] **E16 — Render an attached Blender scene.** Cloudflare Linux sandbox with Blender installed. Verify scene/assets, rendered image contents and dimensions, and the downloadable output.
- [ ] **E17 — Render the open Blender document.** Connected Mac running Blender. Use the intended open scene, including relevant unsaved changes.
- [ ] **E18 — GPU animation render.** Connected GPU computer with compatible rendering software. Verify GPU use, frame progress, and retained completed frames; identify a missing GPU capability explicitly.
- [ ] **E19 — CAD edit and export.** Cloudflare sandbox with a suitable headless CAD tool, or the named connected computer with the required CAD application. Verify dimensions/units and reopen the edited source, STEP, and STL outputs.
- [ ] **E20 — Thermal/stress simulation.** Cloudflare sandbox with the required solver, or a connected computer providing it. Verify fixture inputs, units, boundary conditions, convergence, and numerical tolerance.
- [ ] **E21 — Compare simulation variants.** Suitable Cloudflare sandboxes or connected compute servers. Keep each variant's inputs/results distinct, report completion/failure counts, and retain partial outputs.
- [ ] **E22 — Inspect, stop, and revise a render.** Same computer and original render job. Show current progress, stop that job, and associate the new run's artifacts with its revised inputs.

## 5. Shopping, CLI integrations, and account actions

Identify an actual supported API adapter, installed CLI, or authenticated browser
for each service. A supported host API command runs on the Worker; a CLI needing
Node/Python or another native runtime runs in a Cloudflare sandbox or the
connected computer that owns its session. These cases do not assume an official
Amazon or DoorDash CLI exists. Use controlled transaction fixtures for checkout
tests; real orders require the user's authorization for that order.

- [ ] **E30 — Amazon item and delivered-price lookup.** Available commerce API/CLI or authorized browser. Verify exact item, variant, seller, quantity, availability, and total.
- [ ] **E31 — Purchase within a stated budget.** Same commerce account/session. Resolve the cart, honor the authorized all-in amount and delivery details, and retain the provider receipt without redundant confirmation.
- [ ] **E32 — DoorDash reorder.** Authorized DoorDash interface/session. Resolve the intended meal, restaurant, address, and timing within the budget including fees, tax, and tip.
- [ ] **E33 — Changed price, availability, or substitution.** Existing checkout session. Preserve the user's constraints and resolve only changes outside their authorization before submission.
- [ ] **E34 — Uncertain checkout submission.** Same commerce account. Reconcile provider state before retrying; produce exactly one intended order or retain an explicit uncertain outcome.
- [ ] **E35 — Stop around checkout.** Active automation and its existing checkout session. Exercise Stop before, during, and after submission; report actual order state without inventing cancellation or refund.
- [ ] **E36 — Order tracking and cancellation.** Same account and retained order identity. Retrieve actual status or perform an explicitly requested cancellation and report its real outcome.
- [ ] **E37 — App context into an order.** Connected phone's authorized WhatsApp context → commerce interface. Resolve the correct contact/address and use the necessary delivery details for the authorized purchase.

Repeat session-dependent cases with expired login, an offline device, and a
second account present. Confirm the selected session remains correct and that
historical output is clearly identified when live status is unavailable.

## Evidence and handoff

For the case being worked, record the exact prompt/action, build or deployment,
actual environment/device/session, run or thread link, timings, artifacts or
terminal result, and any remaining blocker. Verify web/iPhone reload behavior
where required. Then check the item and move to the next pending case.

For performance cases, record clone, sandbox readiness, first output, dependency
retrieval, compilation, and test execution separately where measurable. Record
cold/warm cache state and baseline conditions; set budgets from measurements.

| Case | Progress / result | Evidence | Remaining work |
| --- | --- | --- | --- |
| E12 | Local web/iPhone progress and elapsed-time patches; 76 tests passed (42 React, 12 terminal, 20 Swift, 2 account runtime), plus package/type checks. | Branch `fix/terminal-command-progress`; saved production events replay as one Cargo card with Running → Completed, final output, exit 0, and 112.089 s. Live and history reduction agree. | Verify the deployed web and installed iPhone behavior, including completion/failure and reload. |
| E13 / E14 | A production rollout interrupted the two-minute E12 fixture: retained process session `770702757` became unknown/stale during a poll. The original command has no recovered exit receipt. | [Interrupted fixture](https://nanocodex.gakonst.workers.dev/agent/01a07b1d-6675-76c7-85e8-ecf2d7a1a283), cursors 13–47; initial and middle output were retained. | Preserve process-session ownership across Worker replacement and recover the original command’s terminal receipt. |
| B01 / E01 → E03 | `dtolnay/itoa` production baseline passed, with one host clone and one Cloudflare Cargo invocation. | [Production thread](https://nanocodex.gakonst.workers.dev/agent/01a07acf-461d-70be-8f61-ec5cbe319540); [timings and scope](EXECUTION_ACCEPTANCE.md#itoa-baseline--2026-09-07). | Device-availability variants, iPhone verification, and the full E01/E03 matrix remain separate cases. |
| B02 / E06 | itoa warm rerun passed in 4.634 s with caches reused; after-idle rerun rebuilt dependencies in 121.510 s. Both used the existing mount and passed 13 tests. | [Rerun timings and receipts](EXECUTION_ACCEPTANCE.md#itoa-reruns--2026-09-07). | Build-cache persistence across idle restarts, larger-repository warm runs, and device-availability variants remain open. |
| E05 | Prior workspace run lost its process after approximately 38m 42s; no Cargo exit code was recovered. | [Production thread](https://nanocodex.gakonst.workers.dev/agent/01a0788c-a640-734e-bb31-99cb0dafbe23), “Run all” turn. | Diagnose process loss, fix it, and retain a real final result from a fresh run. |

Initial progress notes recorded 2026-09-07. Update this table as each case is
worked; keep detailed acceptance requirements in the linked matrix.
