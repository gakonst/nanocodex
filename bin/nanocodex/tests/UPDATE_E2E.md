# Public CLI updater journeys

Run from the repository root with the CLI and Hand built from the same checkout:

```sh
# Plain debug development pair:
cargo build --locked
# The full release-identity journey below also needs explicit provenance.
export VERGEN_GIT_SHA="$(git rev-parse HEAD)"
export NANOCODEX_HAND_IDENTITY="$(python3 scripts/release/hand-source-identity.py compute \
  --target "$(rustc -vV | sed -n 's/^host: //p')" --profile dev \
  --report target/debug/hand-identity-inputs.json)"
cargo build --locked
python3 scripts/release/hand-source-identity.py verify \
  --report target/debug/hand-identity-inputs.json --dep-info target/debug/nanocodex-hand.d
# --source adds the minimal source-selector journeys on macOS, with a clean env.
# Linux always checks historical/unsupported-source preflight rejection instead.
# --old-updater PATH also installs the pair with a previously shipped two-binary
# updater (copied read-only, e.g. ~/.nanocodex/updater/nanocodex).
node bin/nanocodex/tests/update_local_e2e.mjs target/debug/nanocodex target/debug/nanocodex-hand --source
# A plain `cargo build --locked` pair (no VERGEN_GIT_SHA/NANOCODEX_HAND_IDENTITY):
node bin/nanocodex/tests/update_local_e2e.mjs target/debug/nanocodex target/debug/nanocodex-hand \
  output/update-local-dev --development
```

On a Linux host whose own system Hand is installed (`nanocodex-hand.service`,
root-owned `/opt/nanocodex`), run the runner in a private user + mount namespace
with an empty `/run`. The CLI then observes no systemd and no root-owned Hand
root, so `hand status` reports no owner and nothing reaches the host service or
real HOME; no host mount changes:

```sh
unshare --user --map-root-user --mount sh -c 'mount --make-rprivate / &&
  mount -t tmpfs tmpfs /run &&
  node bin/nanocodex/tests/update_local_e2e.mjs CLI HAND OUTPUT_DIR --development'
```

A user namespace alone cannot query the system bus, so `hand status` fails
there; do not run the runner directly against a host with a live system Hand.

Install layout: `versions/<key>/nanocodex` is the CLI and `versions/<key>/nanocodex2`
is the Hand (release asset `nanocodex2-<triple>`, or a locally built
`nanocodex-hand`); service records keep the file name `nanocodex2`. A Hand that reports a
Hand Identity is stored once under `hand-versions/<identity>/` and linked from every
version with that identity, so a CLI-only update keeps the Hand's path and bytes.
On macOS the Hand also runs from a signed bundle,
`hand-versions/<identity>/Nanocodex.app` (linked as `versions/<key>/Nanocodex.app`):
releases ship it as `nanocodex-app-aarch64-apple-darwin.tar.gz`, local and source
pairs are wrapped and signed with identifier `com.nanocodex.hand` using the identity
named by `NANOCODEX_CODESIGN_IDENTITY`, else the single installed Developer ID
Application identity, else ad hoc. The first verified bundle stored for an
identity is kept, so re-signed copies of an unchanged Hand never
replace it. An installed standalone Hand with the same identity stays in place
until an explicit `--restart-hand` moves the service into the bundle. For a CLI
containing both command trees, `bin/{nanocodex,nanocodex2,nc,ncl}` all link
`../current/nanocodex` and argv[0] selects the tree (`ncl` is local);
`bin/{nanocodex-hand,nc-hand}` link the Hand (`../current/nanocodex2`, or the signed
`../current/Nanocodex.app/Contents/MacOS/nanocodex2` on macOS) when it serves the
`hand` command under those names: `nc-hand --help` is `nanocodex hand --help` and
`nc-hand status` is `nanocodex hand status`. Older
pairs keep `bin/nanocodex2` and `nc` on their managed `nanocodex2`. Windows
writes `nc.cmd`/`ncl.cmd` shims (`ncl.cmd` passes `--local`). An activation whose
Hand is absent or byte-identical to the active/running Hand switches only the
CLI and never stages, switches or restarts the Hand service.

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

## Published nightly journey (Linux)

```sh
python3 bin/nanocodex/tests/nightly_install_e2e.py --old-sha PREVIOUS_NIGHTLY_SHA \
  --new-sha NEW_NIGHTLY_SHA [--final-sha LATER_NIGHTLY_SHA] --output output/nightly-install
```

Run it only after `nightly-NEW_NIGHTLY_SHA` is published and the `nightly` pointer
names it. It installs real immutable nightlies with the public installer
(https://nanocodex.paradigm.xyz with `NANOCODEX_RELEASE_TAG=nightly-SHA`, `--no-setup --no-modify-path`)
and the shipped `nanocodex update --nightly`. No binary is built, copied in or faked.
Each step fetches the public installer, records its SHA-256 and that of the tagged
`refs/tags/nightly-SHA/install` it re-executes, and runs exactly that file.

Every installer/CLI process runs in a private user + mount + PID namespace with an
empty tmpfs `/run` (no systemd, so no Hand owner is found or started), read-only
binds of `/opt/nanocodex` and the real `~/.nanocodex`, and a synthetic
HOME/TMPDIR/`NANOCODEX_DIR` with automatic updates opted out. The mounts are made as
mapped root; the journey then enters a nested user namespace mapping the real
uid/gid back and re-executes, so every CLI runs as the ordinary unprivileged user
(`id -u` and `CapEff: 0` are asserted). Host `nanocodex*` unit state, MainPID,
InvocationID, the selected Hand hash, the `/opt/nanocodex` listing, the real store,
real-HOME user units, `~/.local/bin`, shell profiles and the crontab are captured
outside the namespace before and after; any difference fails the run.

Prefix A: install OLD; OLD `update --nightly` downloads NEW; NEW `update --nightly`
is a cached no-op; roll back with the installer for `nightly-OLD` (cached bundle);
OLD `update --nightly` reactivates cached NEW. Prefix B: a fresh NEW install by the
NEW updater (Hand stored once under `hand-versions/<identity>` and linked), rollback
to OLD, roll forward, then a NEW no-op. Each step checks the active immutable key
(`nightly-<sha>-<cli>-<hand>-<guest>` asset IDs), every CLI entrypoint's `--version`
Commit SHA and link, and that cached activations rewrite no version or Hand file
(inode, nlink, size, mtime, ctime, SHA-256). Hand aliases must run `nanocodex-hand` and
report the active Hand Identity: nightly Hands record no commit, so unchanged Hand
bytes are reusable. The published `nanocodex2-TRIPLE.identity` reuse key must equal
`scripts/release/hand-identity.sh`'s digest of the stored Hand Identity. Each Linux
payload is streamed once independently: its digest must equal SHA256SUMS and its
decompressed bytes (and each voice archive member) must equal the installed files
and receipts.

Step `b4` copies the installed, published NEW CLI and Hand to a separate directory,
downloads the NEW voice archive (checked against SHA256SUMS), selects that exact
pair with `update --path CLI --hand-binary HAND --voice-archive ARCHIVE` (a distinct
`local-*` key), and requires the version to link the existing
`hand-versions/<identity>/nanocodex2` with the same inode, nlink, mtime, ctime and
SHA-256 and no second stored Hand; `update --nightly` then returns to the NEW key.
With `--final-sha`, steps `c1`/`c2` upgrade both prefixes from NEW to a later
published nightly; from the published reuse keys they report whether the Hand was
reused (same canonical file) or stored once under a new identity beside the
untouched previous Hand, and `final-modes` repeats the start checks.

The `modes` step runs `ncl run` and a local TUI turn against a loopback synthetic
Responses server, starts the managed CLI with an empty HOME and records which
boundary it reached (a session, a login prompt still shown by the live process 2 s
later, or the exact "No account login for this origin" error with no panic);
`status` must exit 0 with `authenticated: false`. The managed TUI itself is not reached
without a login. `hand status`/`nc-hand status` must report no owner. Steps may run
separately; state lives in OUTPUT/state.json and the result in OUTPUT/summary.json.

Rollback to a pre-unified nightly (2639aec and older) uses that release's own
installer, which knows only `bin/nanocodex` (local tree) and `bin/nanocodex2` (managed
CLI); the journey asserts those roles by running a local turn and managed `status`.
NEW-only aliases (`ncl`, `nc`, `nanocodex-hand`, `nc-hand`) are left behind and are
unsupported until a unified version is activated again; their targets are recorded
in `summary.json`. After a legacy updater activates a unified version, `bin/nanocodex2` must link
the CLI and every NEW alias must exist (managed commands through the old link must
keep forwarding); a no-op update must not rewrite the manager copies.

With `--candidate CLI HAND VOICE_ARCHIVE` (an unpublished pair built with
`VERGEN_GIT_SHA` and `NANOCODEX_HAND_IDENTITY`), step `p1` installs the published OLD
nightly in prefix P, lets OLD's own `update --path ... --hand-binary ... --voice-archive`
activate the candidate (the legacy activation that leaves `bin/nanocodex2` on the Hand),
runs `nanocodex2 status` through the stale link first (it must forward to the
CLI and is where a self-repairing candidate fixes its links), then requires every
entrypoint role to be correct; `p2` repeats
the selection and requires no version, Hand or manager-copy rewrite. Run
`--steps p1,p2` with the same `--old-sha`/`--new-sha` against a candidate fix before
publishing it. Once the `nightly` pointer names the fixed release, run the full matrix
in a fresh output directory with `--new-sha FIXED` (OLD -> FIXED directly); add
`--final-sha` and `c1,c2,final-modes` only to also upgrade from an intermediate
nightly. Keep earlier failing runs as separate evidence.

Not covered: OS-service handover, `--restart-hand`, the running-service Hand reuse
decision (no service exists in the namespace), managed work after sign-in, and
voice execution.

## Local-pair runner

The runner copies the **real CLI and nanocodex-hand binaries** into a disposable directory,
uses an isolated HOME/store and a generated invalid synthetic account file, and
opts out of automatic scheduling. It never reads saved account credentials,
passes `--restart-hand`, installs a live service, or invokes a privileged command.
It writes commands, expected/observed results, versions and its explicit coverage
scope to `output/update-local-e2e/transcript.log` (or the third argument). Add
`--source` to run the existing macOS source-selector fixture under the same
clean environment, isolated Cargo state and synthetic account. Its separate
transcript is under `OUTPUT_DIR/mac-source/output/update-source-e2e/`.

Covered public boundaries:

- `update --path CLI` finds the sibling `nanocodex-hand`; `--hand-binary HAND`
  selects the same pair. Both probe the real binaries, validate matching full
  revisions and cache CLI + Hand (as `nanocodex2`). With `--development` the
  pair reports no Commit SHA or Hand Identity; the updater instead verifies one
  package version plus Hand service protocol 1, and stores the exact Hand bytes
  as a checksummed regular file in the version (no `hand-identity`, no
  `hand-versions`).
- Hand decoupling: a CLI-only `update --path` (no Hand given or beside it)
  carries the active Hand bytes forward, and a pair whose Hand equals the active
  Hand, both activate immediately with an installed owner: nothing is staged,
  the synthetic plist is unchanged and the live Hand PID (read-only
  `launchctl print`) is identical before and after the whole run.
- Entrypoints: after a real activation `bin/{nanocodex,nanocodex2,nc,ncl}` link
  `../current/nanocodex`, each prints one `Commit SHA:` line (with
  `--development`: the same package version/profile and no provenance lines),
  `ncl --help` shows the local tree and the others the managed tree.
- `--old-updater`: the old two-binary updater installs the pair, its own
  `bin/nanocodex`/`bin/nanocodex2` run it, and the new CLI then takes over the
  same cached pair without a Hand switch.
- macOS: native launchd status/plist inspection stages the pair; `update --apply`
  without explicit restart leaves active CLI and the synthetic login-owner plist
  unchanged after each updater process exits. The fixture is **not bootstrapped**.
- Linux: reads public `hand status`, then asserts installed/loaded-owner staging
  (including `--apply` deferral) or no-owner CLI-only activation as appropriate.
  The native owner status must remain unchanged. No start/stop is requested.
- Linux: real Git fetch of a minimal historical branch must fail before Cargo
  because the fetched revision lacks the self-contained screen-helper contract;
  the prior active/pending pair and version set are preserved.
- Nonzero candidate version-probe and mismatched revision rejection (with
  `--development`: a Hand answering service protocol 2, a different package
  version, and a stamped Hand beside an unstamped CLI), missing
  companion and invalid/conflicting selectors. The rejection candidates are
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
