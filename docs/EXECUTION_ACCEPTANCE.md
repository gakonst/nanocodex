# Execution and action acceptance

Proposed acceptance cases for host/native execution, graphics and simulation,
browser/device actions, and commerce. The clone → Cargo failures are the first
regression to close. These are required outcomes, not claims that the current
build passes or that every named application or integration is installed.
Existing unit tests or a successful Worker upload do not close a case.

Track work and verification in the [execution checklist](EXECUTION_CHECKLIST.md).
It gives each case a checkbox and names the concrete execution environment.

## Environment selection

- **Host:** the agent's Cloudflare Worker running bounded Just Bash against
  durable `/brain`. This is not the user's Mac, phone, browser, or a Linux
  container. Supported file, text, HTTP, and GitHub source operations stay here.
- **Cloudflare sandbox:** a native Linux hand. Reuse a suitable hand attached to
  this agent; otherwise call `mount({ provider: "cf_sandbox", name: "build" })`
  once and use the exact returned mount root as `exec_command.workdir`.
  Native commands may access the same source tree under `/brain`, such as
  `/brain/itoa` for the selected small-repository baseline.
- **Connected device:** select its exact advertised mount when the user requests
  that device, or the task requires a capability available there. A connected
  Mac is not a suitable substitute for a Cloudflare hand's `/brain` access.
- **Managed browser:** use the retained browser session through
  `browser_execute` for public pages or workflows whose authenticated session
  lives there. It is independent of a native build hand. A screenshot of it is
  not a screenshot of the user's laptop or phone.
- **Specialized hand:** select an advertised GPU, desktop application, CAD
  solver, operating system, or other required capability. Mount a provider only
  through its actual connected factory; do not invent GPU support or a provider
  name. Missing capabilities produce an actionable limitation.

Select placement from the required runtime, input-file access, user-selected
device, and authenticated session. The word “CLI” does not by itself require a
sandbox: a supported Just Bash/API command stays on the host; a program requiring
native Node, Python, or another executable runs on a suitable native hand.
Device-bound application sessions remain on that device. Never extract browser
cookies or copy credential stores into a sandbox to manufacture a connection.

Placement belongs to `workdir`, not to text inside the shell command. For
example, `cd /brain/nanocodex && cargo test` with a Cloudflare hand `workdir`
runs in that hand. Omitting `workdir` selects the host. `write_stdin` stays bound
to the hand and process that originally returned its session ID.

Known native commands go directly to a suitable hand. An unsupported command
discovered during host execution can trigger escalation, after checking any
partial effects. An ordinary nonzero exit is not a reason to switch machines.

## Cases

| ID | User instruction or action | Expected calls / environment | Required result |
| --- | --- | --- | --- |
| E01 | Fresh thread: “Clone github.com/gakonst/nanocodex.” | One host `exec_command`, such as `gh repo clone gakonst/nanocodex /brain/nanocodex`. | Source files without `.git`, including the lockfile and expected file modes. No sandbox, device call, or prerequisite account discovery. Record the resolved revision. |
| E02 | “List the files, read Cargo.toml, and find the default workspace members.” | Host commands with `workdir: "/brain/nanocodex"`. Repeat after a sandbox has been attached. | Correct file results; attaching a hand does not move ordinary file inspection off the host. |
| E03 | Same thread: “Run cargo test.” | Reuse or mount one Cloudflare hand; `exec_command({ cmd: "cd /brain/nanocodex && cargo test", workdir: "<returned hand root>" })`. | No preliminary Cargo attempt on the host or Mac. No re-clone or model-directed copy. Observe the real final exit and report default-member test counts accurately. |
| E04 | Fresh thread: “Clone nanocodex and run cargo test.” | Host clone, then the same native path as E03, within one user turn. | Complete without asking the user to request a sandbox or provide another prompt. |
| E05 | Follow-up: “Run all workspace tests.” | `cargo test --workspace` on the same Cloudflare hand. | Run the full requested scope. Persist the exit code and test output. The earlier default-member result cannot stand in for this run. |
| E06 | “Run cargo test again.” | Same hand, source tree, and build target. | Reuse available dependency/build caches; do not provision another hand or re-download/rebuild everything on a healthy warm sandbox. Distinguish this measurement from a cold start. |
| E07 | “Make a full Git checkout with history, then show git status and git log.” | Native `git clone` or `gh repo clone` in a Cloudflare hand's workspace. | Real `.git` and functional Git operations. Do not represent the host's source-only download as a full Git checkout. |
| E08 | Repeat source-only and full-Git clone with an authorized private test repository. | Source-only on host; full Git on native hand; both through existing account credential routing. | Authorized access works without login prompts or tokens in model output, command arguments, app state, or logs. Revoked access fails clearly. |
| E09 | A host command discovers an unsupported native capability. | Host attempt, then one suitable native hand. | Escalate based on missing capability; check partial effects before retry. A compiler error or failing test remains on its original hand for diagnosis. |
| E10 | “Run this on my Mac,” with an existing repo on that Mac. | Exact connected Mac mount from its advertised capabilities. | Execute on that Mac. If unavailable or unable to access the path, report that limitation; never silently run on a different machine. |
| E11 | Edit a source file on host, read/use it natively, then edit it natively and read it on host. | Host and the existing Cloudflare hand against the same `/brain` tree. | Each next operation sees acknowledged writes. Preserve contents, executable modes, and relevant timestamps; no manual copying or stale filesystem cache workaround. |
| E12 | Known passing and failing native commands, including large stdout/stderr and a quiet interval. | One `exec_command`, followed by `write_stdin` on its original session. | Remain Running after yielding. Show subsequent output in the original command card. Preserve the final output and actual exit code; nonzero exit displays failure. Elapsed time spans command call → observed completion, including time between polls; never sum poll durations. Web and iPhone agree after reload. |
| E13 | A native job runs for 60 minutes; close/reopen the app, disconnect/reconnect its event stream, and reconnect the Worker to the live sandbox. | Existing native process and session; no duplicate execution. | Keep the process reachable and restore output/status. Returning after completion still exposes its final receipt. Sixty minutes is a deliberate test duration exceeding the observed 39-minute failure, not a platform limit. |
| E14 | Force process termination, resource exhaustion, or container restart in an isolated test hand. | The hand already running the affected test command. | Persist a truthful interrupted/failed outcome and the available diagnostics. Never fabricate exit 0, lose the last delivered output, or silently restart the command. A destroyed container cannot preserve its live OS process. |
| E15 | While a job is running, send a follow-up or press Stop with an empty composer. | Steering stays in the active turn; Stop targets that turn and its running native work. | No confirmation prompt. No duplicate turn, retargeted session, or orphaned process. Unrelated work remains running; a subsequent command works. |

Run E01–E06 with an unrelated Mac already connected as well as without one.
This catches the exact accidental device placement seen in the original run.
Run retained-session cases across multiple turns and with two native commands
active so output, exit status, and cancellation cannot cross sessions.

## Graphics, CAD, and simulation

| ID | User instruction or action | Expected calls / environment | Required result |
| --- | --- | --- | --- |
| E16 | “Render this attached Blender scene to a PNG.” | Native hand with Blender and access to the scene/assets; CPU rendering is a baseline fixture. | Actual render completes, preserves required textures and camera settings, and returns a viewable image artifact. Verify image contents and dimensions, not just process exit or filename. |
| E17 | “Render the scene currently open in Blender on my Mac.” | Connected Mac's advertised application/automation capability, or its native Blender entrypoint when it can access the intended scene state. | Use the correct open document, including relevant unsaved edits. Do not render an older disk file or change device without resolving that difference. |
| E18 | “Render this animation on a GPU.” | Advertised GPU hand with the necessary renderer support. | Verify actual GPU use, emit frame progress, and retain completed frames. If no suitable hand exists, report it; do not claim a CPU render used a GPU. |
| E19 | “Change this bracket dimension and export STEP and STL.” | Native CAD tool or the connected device that owns the required CAD application/document. | Apply the requested dimension and units; export valid geometry that can be reopened. Retain both the edited source and exported artifacts. |
| E20 | “Run the thermal/stress simulation for these parameters.” | Native solver hand with the required software and adequate resources. | Record fixture units, boundary conditions, solver version, convergence, and numerical result. Compare a known fixture against its expected tolerance; a produced image alone is insufficient. |
| E21 | “Run these 20 simulation variants and compare them.” | Native jobs on suitable hands, with separate output locations and bounded concurrency matched to observed capacity. | Results remain associated with their inputs; show completed/failed counts, retain partial results, and never substitute a different variant's output. |
| E22 | “How is the render going?” then “Stop it; lower the samples and try again.” | Status and cancellation address the original render hand/process; the revised run uses the same suitable hand. | Status is current, cancellation stops the intended job, and new artifacts clearly belong to the revised inputs. Closing the app alone does not cancel the render. |

## Screenshots, previews, and file workflows

| ID | User instruction or action | Expected calls / environment | Required result |
| --- | --- | --- | --- |
| E23 | “Screenshot this public URL.” | Retained managed browser via `browser_execute`; no native build hand needed. | Navigate to the requested page, wait for the relevant content, and return an actual image attachment with the correct viewport. A URL or textual description is not a screenshot. |
| E24 | “Screenshot the tab open on my Mac.” | Connected Mac/browser capability owning that tab. | Capture the selected tab's current state. Do not recreate the URL in the managed browser and call that the same tab. |
| E25 | “Screenshot this app on my phone.” | Phone Hand's advertised, user-granted capture capability. | Return that device/app's image when supported. If unavailable, explain the required supported capture step; do not fabricate background device access or return a cloud-browser screenshot. |
| E26 | “Start this web app and show desktop and mobile screenshots.” | Native hand starts the server; `preview` exposes its port; managed browser visits that exact preview at two viewport sizes. | Screenshots reflect the running build, not another deployment. The server and browser have distinct lifecycles; stop targets the intended process. |
| E27 | “Screenshot my logged-in order page.” | Browser session that already owns the authorized account, managed or on a connected device. | Correct account/page; login-required or expired-session states are truthful. Initial setup can require user login; later work reuses the authorized session without handling raw credentials. |
| E28 | “Compress this uploaded video and extract a thumbnail.” | Native media tools on a suitable hand; host can handle file metadata/routing. | Playable output, requested codec/size behavior, correct thumbnail, and durable downloadable artifacts after reconnect. |
| E29 | “Read this CSV, calculate totals, and make a chart.” | Host for supported bounded parsing/calculation; native hand when the requested library/runtime or workload requires it. | Correct numeric results and a viewable chart/download. Do not provision native compute just to inspect a small text file. |

## CLI, shopping, and account actions

Amazon and DoorDash below name desired journeys. They do not assert that a
particular official CLI exists or is installed. The fixture must identify the
actual authorized API adapter, installed CLI, or browser capability. Discover
that interface before issuing commands; do not guess a binary or its syntax.
If none is usable, report the missing integration. Use provider test facilities
or controlled transaction fixtures for automated checkout/failure tests; this
document does not authorize real purchases.

| ID | User instruction or action | Expected calls / environment | Required result |
| --- | --- | --- | --- |
| E30 | “Find this exact item on Amazon and compare delivered prices.” | Host/API adapter if supported; otherwise installed native CLI or the authorized browser session. | Correct item, variant, quantity, seller, availability, and delivered total. Research alone does not submit an order. |
| E31 | “Buy this item, delivered to Home, for at most $40 total.” | Same authorized commerce interface/session; native execution only if that interface needs it. | Resolve a concrete cart and submit when the user's authorization covers its final contents and total, including tax/shipping. Retain the provider's order receipt. No redundant approval when the final order is already authorized. |
| E32 | “Reorder my usual DoorDash dinner, under $35 including fees and tip.” | Authorized DoorDash adapter/CLI or browser session owning order history and checkout. | Resolve the intended meal, saved delivery address, restaurant, and requested timing. Include fees/tax/tip in the bound. Clarify only material missing choices or a change outside authorization. |
| E33 | The price rises, an item is unavailable, or a substitution changes the order. | Existing checkout session. | Respect the user's amount, quantity, variant, substitution, and delivery constraints. Ask for the specific unresolved change before committing when it falls outside those constraints. |
| E34 | Checkout times out after the submission may have reached the provider. | Reconcile using the same account and retained attempt/receipt information. | Query the provider before retrying. Use supported idempotency controls; otherwise keep an uncertain outcome explicit. Exactly one intended order is the acceptance outcome, not repeated successful CLI invocations. |
| E35 | “Stop” arrives before submission, during an uncertain submission, or after the order is placed. | Stop the active automation and reconcile any in-flight purchase. | Before submission, place no order. After submission, report the actual or uncertain order state. Stopping the agent must not falsely claim an existing order was cancelled or refunded. |
| E36 | “Where is my order?” or “Cancel the order you just placed.” | Same account's order-status/cancellation interface, using the retained order identity. | Status corresponds to the actual order. An explicit cancellation request invokes the supported operation and reports its real outcome; avoid duplicate orders or invented refunds. |
| E37 | “Find Sam's delivery address in WhatsApp and order these flowers there.” | Authorized phone/app context capability → host reasoning → authorized commerce interface. | Use the correct contact and grounded address, resolving ambiguity before purchase. Send only the required shipping details to checkout; do not claim that a generic cloud browser can read the phone's chat session. |

For every session-dependent case, repeat with the session expired, the owning
device offline, and a second account available. Never silently choose another
account, cart, document, or device to turn an unavailable capability into an
apparent success. Preserve the last known result while clearly identifying it
as historical when live status is unavailable.

Artifacts must be usable from the conversation: image/video preview where
appropriate and downloadable source/output files. Retain input provenance,
producing command/job, and completion state so an old screenshot or render
cannot be presented as the result of a newer request. Device file handoff uses
an explicit supported artifact/export capability; it must not assume native
cross-mount access the device has not advertised.

## Deterministic evidence and timing

For reproducible infrastructure comparisons, fix the repository revision,
lockfile, Rust toolchain, sandbox image, and resource class, then establish a
passing baseline for that fixture. Select the command from that repository's
checked-in CI and the requested test scope. Retain the exact natural-language
journeys above as routing tests, and record any unpinned or unmeasured conditions.
The ordinary itoa clone/test below is a routing baseline; it does not establish
toolchain parity with nanocodex or replace either repository's full CI matrix.

Collect these separately, without estimating one from the assistant's prose:

1. Prompt accepted → first tool call.
2. Host clone command → complete source tree.
3. Native hand request → readiness, or confirmed reuse of an existing hand.
4. Native command start → first output.
5. Dependency retrieval, compilation, and test execution where instrumentation
   can distinguish them; label any intervals that cannot be separated.
6. Command start → persisted terminal result.

Measure cold and warm Cargo runs separately. Compare the shared source tree
against a local-disk control using the same image, resource class, source
revision, and equivalent cache state. The control is a diagnostic benchmark,
not a product workflow that asks the model to copy the repo before building.
Record CPU, memory, restart/OOM signals, and filesystem waits to explain the
difference. Choose latency budgets from this baseline; do not invent platform
limits or add another application headroom check.

Every execution case needs the tool trace, selected hand identity, output,
terminal receipt, and relevant timing. UI cases additionally need actual browser
and iPhone evidence, including reload/reconnect. Keep authorization material out
of retained evidence. An assistant summary, green tool-transport status, or
`ProcessNotFoundError` is not evidence that Cargo completed successfully.

## Order

1. Close deterministic process, failure, reconnect, and cancellation cases
   (E12–E15), including recovery of a real terminal receipt.
2. Establish the cold/warm native filesystem baseline and fix the observed
   bottleneck without weakening E11.
3. Run the exact host clone → default Cargo → workspace Cargo journeys
   (E01–E06), then Git credential and explicit-device cases.
4. Repeat the touched journeys against the deployed build and both clients.
5. Extend the same lifecycle checks to one CPU Blender render, one CAD/solver
   fixture, managed/device screenshots, and a native server → browser preview.
6. Validate commerce discovery and checkout with controlled fixtures, including
   the ambiguous-submit and Stop cases, before an explicitly authorized live
   order is used as evidence.

Command progress is deployed; the web and iPhone observations are recorded
below. Process recovery remains open: the original workspace Cargo run lost
its process after approximately 38m 42s and produced no terminal Cargo result.

## itoa baseline — 2026-09-07

The user selected `dtolnay/itoa` as the first small-repository clone/test
baseline. This run passed against production revision
`d0634663122c323fd7da748066d02467e7e8d2e7` with the default GPT-5.6 Sol / High
settings. The upstream branch head was
`1577ed901354d0d7448ac162328f9dbf5183124c` both before and after the run; the clone
used the ordinary default-branch prompt rather than an explicitly pinned ref.

[Production thread](https://nanocodex.gakonst.workers.dev/agent/01a07acf-461d-70be-8f61-ec5cbe319540).

| Step | Observed execution | Time / result |
| --- | --- | --- |
| Prompt: “Clone github.com/dtolnay/itoa” | One `exec_command`: `gh repo clone dtolnay/itoa itoa`, `workdir: /brain`. | 0.642 s command; 9.579 s accepted prompt → completed turn. Source-only download, exit 0. |
| Follow-up: “Run cargo test” — provisioning | One `mount`, provider `cf_sandbox`, name `itoa-test`. | 6.660 s. Returned `/mnt-itoa-test-8fd08513`. |
| Test execution | One `exec_command`: `cargo test --manifest-path /brain/itoa/Cargo.toml`, `workdir: /mnt-itoa-test-8fd08513`; subsequent `write_stdin` calls retained the same process. | 112.089 s command call → observed exit 0. First output observed after 9.849 s. |
| Cargo result | Fresh dependency resolution/download and compilation, including Criterion's test dependency graph. Cargo reported 1m 47s to finish the build. | 11 integration tests + 2 doc tests passed; 0 failures. The library unit-test target ran 0 tests. |
| Follow-up turn | Accepted prompt → completed assistant turn. | 132.429 s, including environment selection, mounting, execution/polling, and the final response. |
| Browser reload | Reopened the retained production thread after completion. | Both prompts and final responses restored, including the 11 + 2 passing counts; no application console warnings/errors observed. |

The original two-turn journey used no operator steering, Mac execution, second
clone, or source-copy workaround. Timings come from persisted tool/turn events; command timing includes
the interval until the final poll observed completion. The follow-up reruns
below measure cache reuse separately. This does not resolve the larger
nanocodex process-loss case.

A separate read-only host audit after the benchmark confirmed `.git` is absent
and all 16 upstream files match the revision above by SHA-256. The only extra
file was the generated `Cargo.lock`, with SHA-256
`6cb2b817513dfed929e3b7859687e91f48e8e3ca1d33e3b5e05457badc457935`.
File modes and timestamps were not audited. The run did not record the exact
Rust toolchain or sandbox image digest, and dependency download time was not
separately instrumented. It ran ordinary debug-profile `cargo test`; release,
feature variants, Miri, fuzzing, and the rest of itoa's CI were not exercised.

Before the E12 fix, the deployed UI displayed the initial Cargo yield as “Succeeded 1.56 s”
with empty output, even while compilation continued. The final assistant
message and durable tool result correctly reported success. This confirmed
B01's execution path; E12's later verification is recorded below.

The progress patch now derives elapsed time from persisted command-call
and result timestamps. Replaying this run produces a single Cargo card that
remains Running during compilation and completes with 112.089 s, exit 0, and
the final test output. Summing poll RPC durations would incorrectly show
65.107 s because the process also runs between polls. Live reduction and full
history replay agree. The deployed verification is recorded below.

### itoa reruns — 2026-09-07

Both follow-ups used the same production thread and the prompt “Run cargo test
again in the same sandbox.” Both selected the existing mount
`/mnt-itoa-test-8fd08513` and ran
`cargo test --manifest-path /brain/itoa/Cargo.toml`, without another mount,
re-clone, source copy, Mac call, or operator steering.

| Run | Cache behavior | Command call → observed completion | Full accepted turn | Result |
| --- | --- | --- | --- | --- |
| Original cold run | Resolved/downloaded dependencies and compiled. | 112.089 s | 132.429 s | Exit 0; 11 integration + 2 doc tests. |
| Rerun after approximately 45 minutes without sandbox activity | Downloaded and compiled dependencies again. Cargo reported 1m 57s for the build. | 121.510 s | 142.705 s | Exit 0; 11 integration + 2 doc tests. |
| Immediate warm rerun, accepted 31.582 s after the preceding turn completed | No index update, dependency download, or compilation appeared. Cargo reported 2.18 s to finish the test profile, then 0.73 s for doc tests. | 4.634 s | 13.923 s | Exit 0; 11 integration + 2 doc tests. |
| User-requested second warm rerun | Same mount and cached build; no downloads or recompilation. Cargo reported 1.67 s to finish the test profile, then 0.36 s for doc tests. | 4.484 s | 14.038 s | Exit 0; 11 integration + 2 doc tests. |

The after-idle turn first attempted `cargocargo`, which exited 127, then
corrected itself without intervention. Its full turn time includes this typo;
the 121.510 s command interval starts at the corrected Cargo invocation. The
warm turn used one `exec_command` and one `write_stdin`.

Cache reuse works while the container remains active. Reusing the mount does
not establish that the original container disk survived idle time. Our
[sandbox configuration](../js/managed/src/sandbox-tools.ts) sets
`sleepAfter: "10m"`, and the [image](../js/managed/Dockerfile) puts Cargo's cache
at `/opt/cargo` and build output at `/tmp/nanocodex-cargo-target`. Cloudflare's
[lifecycle documentation](https://developers.cloudflare.com/sandbox/concepts/sandboxes/)
states that an idle stop resets the container's local disk. This is consistent
with the observed cache loss; the specific container-stop reason was not
captured. Durable `/brain/itoa` remained available. Preserving build caches
across idle restarts remains open.

Durable receipts: after-idle turn `ed52292a-7d1a-43c5-ab2e-98f5eb2080f4`;
warm turn `c35dac33-1fab-4228-997b-b6df59bb25d0`; second warm turn
`19fab06a-8a64-43c3-8037-d5c88e6b5eda`.

Implementation owners: [namespace routing](../js/managed/src/namespace-tools.ts),
[native process lifecycle](../js/managed/src/sandbox-tools.ts),
[managed runtime policy](../js/managed/README.md),
[React projection](../js/nanocodex-react/agent/transcript.mjs), and
[iPhone projection](../apple/InboxCore/Sources/InboxCore/Protocol.swift).

## Terminal command presentation — 2026-09-07

[PR #290](https://github.com/gakonst/nanocodex/pull/290) retains yielded commands
as Running, folds later `write_stdin` output into the original command, and
measures elapsed time from its persisted start through observed completion.
[PR #291](https://github.com/gakonst/nanocodex/pull/291) exposes the actual exit
code in expanded web results. Production revision
`f7f7b80a8826db185b7daf1d8adaa56017f1f9fd` completed its account deployment at
09:44:34 UTC. The deployment job succeeded; that does not imply all longer CI
or service tests finished.

On the deployed web app, reopening the original itoa thread now shows the cold
command as Succeeded 112 s with final test output. The two warm commands show
4.63 s and 4.48 s. The `cargocargo` typo correctly shows Failed with exit 127.

The fresh [E12 progress 09B5D4C9 thread](https://nanocodex.gakonst.workers.dev/agent/01a07b41-da6a-73ff-94aa-7df8dee07245)
used one Cloudflare sandbox, `01a07b42-147d-7de0-81e9-f76f009887b9`, mounted at
`/mnt-e12-009887b9`. Both commands used this environment.

| Fixture | Durable receipt | Result |
| --- | --- | --- |
| Print `E12_START`, sleep 60 s, print `E12_MID`, sleep 60 s, print `E12_DONE`; yield after 1 s and poll the original session. | Turn `59cfbaca-2f82-40f7-9227-c958016e77b0`, process session `492682587`. | First output after 1.283 s; completion observed after 121.316 s; exit 0; all three lines retained. |
| Print a start marker, sleep 30 s, emit 300 numbered stdout lines and 300 numbered stderr lines, print `E12_EXPECTED_FAILURE` to stderr, exit 7; poll without retrying the command. | Turn `2607ae86-0d21-48d7-bca2-3b4b3d897472`, process session `2067236829`. | First output after 1.262 s; completion observed after 30.868 s; exit 7. |

The web command stayed Running with initial output across reload, then became
Succeeded 121 s with exit 0 and all three lines in the original card. Reopening
the failure showed Failed 30.9 s, exit 7, and the output tail through
`E12_STDOUT_0300`, `E12_STDERR_0300`, and `E12_EXPECTED_FAILURE`. Empty polling
calls did not create separate command cards. Card output is bounded; this does
not claim all 600 lines remain visible at once. The durable receipt retains
the full fixture output.

The signed Centaur app (`xyz.paradigm.centaur`) was installed on a physical
iPhone 17 Pro. Its live fixture showed Running with initial output, restored
Running after relaunch, and displayed Completed with exit 0, elapsed 121.316 s,
and all three output lines. The native follow-up makes tool status visible in
the Activity list and deduplicates replayed calls/results by their original
turn and call identity, preserving the original elapsed-time start.

Validation includes the original 76 focused tests (42 React, 12 terminal,
20 Swift, 2 account runtime), React type/package checks, terminal package
checks, account typecheck, and a signed iPhone build. The exit-code follow-up
passed all 12 terminal tests. The native replay policy passed all 8 focused
`ToolPresentationTests`. Physical history verification is recorded separately
from the live observations so a reopened receipt is not described as a fresh
successful execution.

The final physical iPhone `testLiveTerminalReceiptHistory` passed in 212.080 s
at 10:07 UTC. It created no agents or turns: it reopened the two receipts above,
expanded each turn's Activity and command details, and verified the completed
output, elapsed field, failure output tail, and exit 7 after app relaunch.
Both final screenshots were visually inspected. Local evidence is retained in
`/tmp/nanocodex-progress-live-centaur-history-3.xcresult`; its device diagnostics
and account credentials must not be uploaded as a whole bundle. Earlier
history attempts failed in test navigation, before this corrected scoped
scrolling check. The full final InboxCore suite also passed: 64 tests passed,
3 opt-in live tests skipped, 0 failures.

### Remaining recovery failures

The full fresh live XCTest is not green. An earlier rollout-time fixture lost
process session `770702757` after retaining its initial and middle output:
[thread](https://nanocodex.gakonst.workers.dev/agent/01a07b1d-6675-76c7-85e8-ecf2d7a1a283),
cursors 13–47. A later [Centaur fixture](https://nanocodex.gakonst.workers.dev/agent/01a07b46-97da-7d3b-b35e-a70d509528b9)
lost session `2050295544` at 09:51:39 UTC, after the recorded account deployment
had completed. Its command began at cursor 13, recovered events were replayed
at cursors 29–45, and cursor 46 returned
`unknown or stale namespace process session`, 57.517 s after the original call.
Neither run recovered the command's terminal exit receipt. The later failure's
cause is not established. E13/E14 remain open; UI deduplication does not restore
the backend process.

The error originates in the [namespace session lookup](../js/managed/src/namespace-tools.ts),
which currently retains process bindings in an in-memory map and rejects a
missing binding or mismatched owner session. It does not by itself establish
that the native sandbox process exited. Recovering that binding and retrieving
the original provider receipt is the next diagnostic step.

The successful fixture also exposed a separate Code Mode receipt gap: a
yielding `exec` containing `tools.mount` returned the mounted result through
`wait`, but emitted no inner mount `tool.result`. Consequently the web Mount
card remained Running after the turn ended. This receipt gap remains open;
the command progress fix does not infer a successful mount receipt.
