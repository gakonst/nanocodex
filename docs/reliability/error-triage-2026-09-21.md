# Error recovery and transcript isolation

This change addresses child output appearing as parent answers, ambiguous completion
and Hand failures, temporary WebSocket send pressure, unsupported shell flags, and
reproducible native CI contract/fixture failures. It preserves canonical child
history, schema validation, turn-token checks, and durable-operation ownership.

## Comparison with clabby/tact

Reviewed Tact's source and commit history alongside Nanocodex `1728ccee7`.

- [Tact #172](https://github.com/clabby/tact/commit/3ede7184219676d66b7d2175838efa1e6a68f354)
  keeps child transcripts in their own inspectable nodes. Completion wakeups carry
  the child ID rather than injecting raw child output into the parent transcript.
  This change uses the same provenance-based presentation principle, adapted to
  Nanocodex's web, Swift, and managed terminal surfaces. Root JSON answers remain
  ordinary answers; child content remains inspectable.
- Tact still requires an accepted `submit_result` and rejects stale tokens. It
  does not provide a restricted formatting-only recovery turn. Its native tool
  registration does not prove a Code Mode binding exists in a hosted runtime.
- [Tact #193](https://github.com/clabby/tact/commit/344ffebca8c28a86b5daf8840bd5edc090f4adb8)
  updates transcript state after code-cell termination. The equivalent handling
  was already present here; it does not fix failed OS process-group termination.
- [Tact #189](https://github.com/clabby/tact/commit/2c72bf59071fcb1bb9773d539caa1f49e242b528)
  retries SQLite transcript locking, and
  [#197](https://github.com/clabby/tact/commit/7debd327dec6d9fe7f8976d569d926bae7c81d45)
  persists terminal provider-stop state. Neither is a fix for a standalone
  checkpoint blocked by an unfinished durable operation.
- No equivalent browser send-buffer, virtual-workspace archive-link, or `gh --jq`
  fix was identified. These have different ownership and runtime boundaries.

## Deliberate recovery boundaries

A rejected result exposes typed validation/recovery information to native callers,
while ordinary error text remains readable. Missing submissions do not trigger an
unrestricted model turn: that could repeat task side effects. Accepted output is
retained as evidence if cancellation wins settlement, without relabeling the
interrupted execution as successful.

Transport overflow distinguishes a permanently oversized frame from temporary
aggregate pressure. The latter waits for a bounded drain before reporting a safe
reconnectable failure. A timed-out or cancelled wait never sends later. Managed
MPP sockets retain their existing transport policy.

The virtual workspace still cannot faithfully represent symbolic links across
its JavaScript adapters, R2 persistence, and native s3fs mount. Archive extraction
rejects them explicitly and cleans up partial output. The change does not claim
to make symlink-containing repositories cloneable, move a requested destination,
or replace links with copies. Full support needs a shared filesystem contract and
cross-reader confinement tests, including forward links, cycles and mount grants.

Historical unsupported-provider-field and checkpoint-conflict receipts did not
reproduce against this source. No speculative field stripping or removal of the
pending-operation guard is included. Existing #458 interruption/operation-identity
recovery must be evaluated on new, correlated failure receipts.

Docker desktop tests register an explicit fixture MCP provider and exercise its
real guest screenshot/input path. They validate registration and desktop lifetime,
not installation or behavior of the production Sky JavaScript provider.

## Native process cleanup

Darwin may transiently refuse a process-group signal before Node has delivered
its close event. Cleanup retries `EPERM` within a fixed five-attempt budget on
Darwin, then propagates a persistent refusal. A failed cleanup retains ownership
and permits a later cleanup attempt instead of caching the rejected promise.
Tests cover transient and persistent refusal, bounded retries, and later cleanup.
Process-capacity fixtures now poll yielded results to completion and synchronize
on readiness rather than assuming a short yield guarantees process startup/exit.
They retain their process-count, output, exit-status, ownership and reaping checks.

## Validation

Executed on macOS unless noted. These are scoped checks, not production rollout.

| Check | Result |
|---|---|
| `cargo test --locked -p nanocodex-subagents --lib` | 56 passed |
| `cargo test --locked -p nanocodex-tools --lib -- --test-threads=2` | 246 passed, one existing ignored test |
| `cargo test --locked -p nanocodex2-bin --bin nanocodex2 child_` | 12 passed, including collapsed child JSON and provenance replay |
| `cargo test --locked -p nanocodex2-bin --bin nanocodex2 device_hand::account` | Seven passed |
| `cargo test -p nanocodex-vm --doc` | Seven passed |
| `cargo test --locked -p nanocodex-vm --test docker_live --no-run` | Compiled; Docker daemon unavailable for execution |
| `pnpm --filter nanocodex-tools test` | Build and all 57 tests passed |
| `node --test js/nanocodex/test/browser-host.test.mjs` | 41 passed |
| `pnpm --filter nanocodex-react test` | 58 passed, TypeScript/package/pack checks passed; subsequent focused controller run passed 22 tests |
| `pnpm --filter nanocodex-terminal test` | 23 passed, TypeScript/build/pack checks passed |
| `swift test --package-path apple/InboxCore` | 258 passed, five opt-in skips |
| Native voice host suite | 49 passed, one ignored fixture; jitter case also passed six focused runs |
| Formatting and whitespace | `cargo fmt --all -- --check` and `git diff --check` passed |

The JS presentation suites used a real, existing generated WASM package with
verified artifact hashes, not a rebuild of this branch's Rust. SwiftUI and UI-test
files passed syntax parsing, but complete iPhone simulator journeys and the
isolated SwiftPM CI workflow have not been executed locally. The voice CI failure
was not reproduced; its original continuity/gap thresholds are unchanged.
