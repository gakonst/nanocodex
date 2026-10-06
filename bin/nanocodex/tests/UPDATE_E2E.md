# Public CLI updater journeys

Run from the repository root with native executables built from the same checkout:

```sh
cargo build --locked -p nanocodex-bin --bin nanocodex --features tempo
cargo build --locked -p nanocodex2-bin --bin nanocodex2
# --source adds the minimal source-selector journeys on macOS, with a clean env.
# Linux always checks historical/unsupported-source preflight rejection instead.
node bin/nanocodex/tests/update_local_e2e.mjs target/debug/nanocodex target/debug/nanocodex2 --source
```

`CARGO_TARGET_DIR` may be used for the build; pass the resulting absolute binary
paths to each runner. Linux distributable source builds also need the pinned
screen-helper build prerequisites described by `scripts/build-linux-screen-helpers.sh`.
Do not substitute mock Cargo or a mock updater for these commands.

## Installer download journey

```sh
python3 bin/nanocodex/tests/install_network_e2e.py target/debug/nanocodex target/debug/nanocodex2 output/install-network
```

This invokes the real CLI with `install --no-setup --no-modify-path` against a
loopback HTTPS GitHub transport fixture. It uses an isolated HOME, credential
file and installation store, with automatic scheduling disabled. A test CA is
trusted only by the child process; no system trust settings change. The Hand
payload is a real executable; the voice archive is structural fixture data.

The journey verifies checksum-gated reuse of the running bootstrap, overlapping
Hand/voice transfers, CLI download fallback, missing checksums, and corrupted
payload rejection without changing the selected or pending release. Any
existing native Hand remains running and unchanged. Transcript, request trace,
and structured results are written to the output directory. This does not
exercise voice execution or first installation of an OS service.

## Local-pair runner

The runner copies the **real CLI and Hand binaries** into a disposable directory,
uses an isolated HOME/store and a generated invalid synthetic account file, and
opts out of automatic scheduling. It never reads saved account credentials,
passes `--restart-hand`, installs a live service, or invokes a privileged command.
It writes commands, expected/observed results, versions and its explicit coverage
scope to `output/update-local-e2e/transcript.log` (or the third argument). Add
`--source` to run the existing macOS source-selector fixture under the same
clean environment, isolated Cargo state and synthetic account. Its separate
transcript is under `OUTPUT_DIR/mac-source/output/update-source-e2e/`.

Covered public boundaries:

- `update --path CLI --hand-binary HAND`: probes the real binaries, validates
  matching full revisions and caches their actual bytes.
- macOS: native launchd status/plist inspection stages the pair; `update --apply`
  without explicit restart leaves active CLI and the synthetic login-owner plist
  unchanged after each updater process exits. The fixture is **not bootstrapped**.
- Linux: reads public `hand status`, then asserts installed/loaded-owner staging
  (including `--apply` deferral) or no-owner CLI-only activation as appropriate.
  The native owner status must remain unchanged. No start/stop is requested.
- Linux: real Git fetch of a minimal historical branch must fail before Cargo
  because the fetched revision lacks the self-contained screen-helper contract;
  the prior active/pending pair and version set are preserved.
- Nonzero candidate version-probe and mismatched revision rejection, missing
  companion and invalid/conflicting selectors. The two rejection candidates are
  deliberately executable input fixtures, not a Hand-service implementation.
- Corrupted cached companion is rejected by real `update --apply`; active CLI
  stays unchanged and pending evidence is preserved.
- macOS `hand recover`: synthetic interrupted CLI state restores the previous
  active version; committed CLI-only state finalizes; inconsistent committed
  state is refused with the journal retained. **These are injected on-disk crash
  records, not an actual killed update or OS-service rollback.**

The account fixture and synthetic Mac owner definition must remain byte-identical.
Windows is explicitly refused: running `--path` there can self-replace the running
executable and overwrite sibling entrypoints, and task identity is not isolated by
HOME alone. Use a disposable native interactive Windows user/VM for acceptance.

## Source-selector runner

`update_source_e2e.mjs` runs the actual updater, Git and Cargo **on macOS**. A
local bare Git repository substitutes only for GitHub's source URL; an executable `gh` fixture
supplies only external PR metadata. The minimal Rust CLI/Hand packages are source
build inputs, **not acceptance of production Hand runtime/service behavior**.
The journeys select a branch and an open PR, then reject closed/changing PR heads,
a missing branch and an actual Rust compile failure while preserving the prior
bundle. Its transcript is `output/update-source-e2e/transcript.log`.

On Apple Silicon, the source fixture also uses the shipped cross-tool wrappers
in a nested Cargo build, compiling and archiving C and linking a static AArch64
musl guest init. Install the Rust target with
`rustup target add aarch64-unknown-linux-musl` and provide `ld.lld` and `llvm-ar`
on PATH or in their standard Homebrew locations. A failing `brew` fixture checks
that installed tools work without Homebrew. The runner retains the ELF artifact
and verifies it with `scripts/check-vm-init.py`.

Do not run that historical minimal fixture as Linux success acceptance: current
Linux source updates correctly reject it before Cargo. A successful Linux source
journey must fetch a source revision containing the helper builder, pinned
inputs, build script, runtime and verifier. The actual updater must call that
checkout's `scripts/build-linux-screen-helpers.sh --auto`, clear inherited
`NANOCODEX_LINUX_SCREEN_BUNDLE`, build both binaries, and run that checkout's
`linux-screen-helpers-bundle.py --binary HAND BUNDLE` before installing. Do not
replace the builder/verifier or accept an arbitrary inherited payload to make a
minimal fixture pass. Missing prerequisites, missing/invalid payload and absent
historical contract must preserve the existing bundle. Release, nightly and
helper-aware release-recovery jobs run the same shipped-binary payload check;
historical release recovery explicitly retains its older source contract.

Use the local-pair runner's `--source` option to launch the macOS fixture safely.
When running a source fixture directly, provide a clean environment and an
explicit generated invalid `NANOCODEX_ACCOUNT_FILE`. Keep `CARGO_HOME` isolated;
`RUSTUP_HOME` may point at the existing toolchain, not saved account state. The
fixture packages need no external Rust dependencies. Never inherit a real
Nanocodex account-file or GitHub/provider token override.

## Separate native-service acceptance

For an already connected macOS service, run
`node bin/nanocodex/tests/hand_install_e2e.mjs CLI [OUTPUT_DIR]` to check repeated
`hand install` and invalid executable errors against the real launchd owner.
It uses an invalid synthetic account override, checks that the exact PID and
plist survive installer exit, and records screen warnings when capture is
unavailable. Use a CLI containing this idempotency behavior; an older installer
may restart a connected service whose screen is unavailable. This journey does
not cover first installation or version handover.

The runners above do **not** establish these contracts. Run each on a disposable
native desktop user/VM with a real installed service and a synthetic account API
fixture reachable over the actual Hand transport. Do not run these destructive
journeys on a shared user's Hand:

1. Install the shipped pair with `nanocodex hand install`. Observe the persistent
   owner with `hand status` plus native service-manager PID/executable/definition,
   and Hand **and screen** in the account API. Stop the installer/updater process;
   the exact owner PID and account surface must remain live.
2. Start a real CLI/managed session attached to that same account, close it, and
   poll both service PID and account surfaces. The Hand must not be its child and
   must survive closing all CLI sessions. Separately log out/log back in or reboot
   and verify the configured login/boot owner starts the same selected version.
   Passing `--version` or preserving plist text does not cover this lifetime test.
3. Repeat selectors `update`, `update --nightly`, `update --branch TOPIC`, and
   `update --pr NUMBER`, `update --version VERSION`, and a local `--path` pair:
   default commands cache a verified coherent pair and stage without disrupting the installed service. Check the old CLI/service remain
   selected and pending state names the new pair after updater exit.
4. `update --apply` must keep the staged pair deferred while an owner is installed;
   `update --apply --restart-hand` explicitly hands over to the exact new worker,
   proves a fresh Hand connection, commits the CLI, clears pending and removes
   rollback evidence. Also verify explicit `hand restart`, and `hand start` when
   the owner is stopped, use the same staged-pair transaction. Close the updater
   and CLI sessions, then recheck persistence. On Linux capture the separate
   factory PID/executable hash, unit definition and guest inventory before/after:
   they must remain unchanged. On macOS, repeat with screen capture unavailable:
   the exact Hand owner must still connect and commit, with an explicit screen
   availability warning. Screen capture readiness must not roll back a connected
   service. Check committed `hand recover` finalization under the same condition.
5. Supply an actual new Hand that starts but cannot reconnect to the synthetic
   account. Explicit restart must fail, restoring the old CLI, old worker bytes,
   original configuration and prior loaded/stopped state. Assert account readiness
   for the restored old worker, not merely successful service-manager exit.
6. Kill the updater at prepare, Hand handover, CLI activation and commit boundaries.
   Invoke `hand recover`; verify old state restoration or verified committed-state
   finalization, with evidence retained on ambiguous recovery. An existing idle
   barrier must fail closed for remote work/retained processes/VMs, not just CLI
   lease absence.

Normal/nightly network downloads additionally need real HTTPS release-metadata,
immutable-nightly resolution, manifest verification and payload extraction tests.
A cached release or source selector does not cover these HTTP paths. If GitHub,
sudo or service-manager transport fixtures are necessary, list exactly those
fixtures in the transcript; never report their fabricated status as native
platform acceptance. Capture synthetic API requests, native service receipts and
selected binary hashes alongside CLI transcripts in ignored `output/` artifacts.
