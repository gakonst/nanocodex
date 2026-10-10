# Releasing Nanocodex

Nanocodex releases eight crates.io packages in lockstep with the CLI binaries and
the `nanocodex` and `nanocodex-vite` npm packages. The process combines Alloy's conventional-commit changelog with
Foundry's label-grouped, contributor-attributed GitHub release notes.

## Nightly releases

The `Nightly Release` workflow runs daily and may also be dispatched manually.
Only runs from `master` with no pull request input publish releases; branch and
pull request builds produce artifacts without changing the rolling nightly.
Each successful publication creates an immutable `nightly-<full SHA>` prerelease and
refreshes the rolling `nightly` prerelease with the same gzip-compressed
binaries and `SHA256SUMS`. The immutable release is assembled as a draft and
published only after every asset is attached, so updaters never observe a
partial one. A published immutable nightly is never modified: rerunning the
same commit reuses its exact, checksum-verified assets to refresh the rolling
pointer instead of uploading a different build under the same tag. The rolling release is a commit pointer; the updater
resolves and verifies assets from the corresponding immutable release. Raw
executables remain only on the rolling release so pre-compression updaters can
cross the format transition.

The immutable manifest includes both compressed and decompressed executable
checksums. The raw checksum lets the installer reuse its verified running
bootstrap without downloading it again; the raw executable need not be a
release attachment. Publication verifies compressed assets before unpacking
them, then verifies every raw checksum. For manual verification of downloaded
compressed assets on Linux, `sha256sum --check --strict --ignore-missing SHA256SUMS`
checks the files present; unpack an executable and repeat to check its raw bytes.

Each native nightly and stable release builds both role binaries per target in
one invocation (`cargo build --features nanocodex-bin/tempo`, with the release
profile selected by the workflow). The workspace defaults select the CLI and
Hand packages; plain `cargo build` produces both debug executables. `nanocodex-<triple>[.gz]` is the CLI and
`nanocodex2-<triple>[.gz]` is the `nanocodex-hand` daemon, keeping the companion
name older updaters fetch (`nanocodex-x86_64-pc-windows-msvc.exe` and
`nanocodex2-x86_64-pc-windows-msvc.exe` on Windows). `SHA256SUMS` lists both.
x86_64 Linux also contains the static VM guest.

Linux x86_64 and Apple Silicon bundles include a platform voice runtime archive.
The voice jobs verify initialization and relocation before upload; the updater
checks its checksum and installs it under the selected version's
`nanocodex-resources/voice`. Windows currently has no voice archive. Native voice
payloads are release components, not outputs of an ordinary Rust debug build.

Ordinary debug builds use Cargo's incremental cache without a provenance build
script. CLI, Hand, shared executable support, and terminal rendering are separate
packages. CLI-only edits leave the Hand package cached. A plain development build
reports its package version without inventing a Git revision or release identity.

Release staging sets `VERGEN_GIT_SHA`, `TAG_NAME`, and
`NANOCODEX_HAND_IDENTITY` at the executable boundary. The identity comes from
`scripts/release/hand-source-identity.py`: the Hand dependency closure, resolved
features and dependency edges, source, toolchain, build configuration, and native
payloads. Post-build verification checks the compiler's dependency file against
that closure before the artifact can be reused. This preserves an unchanged
Hand across CLI-only releases without invalidating ordinary debug builds.
Linux distribution builds additionally embed the prepared screen helpers using
`nanocodex-hand-daemon/embedded-screen-helpers`; plain debug builds report a
missing payload if that screen backend is requested.

On Apple Silicon, `scripts/release/macos-sign-hand.sh` signs the Hand with the
identifier `com.nanocodex.hand` and the hypervisor entitlement its libkrun VMM
children need, both as the standalone `nanocodex2-aarch64-apple-darwin[.gz]`
companion and inside `Nanocodex.app` (`CFBundleIdentifier` `com.nanocodex.hand`,
executable `Contents/MacOS/nanocodex2`), published as
`nanocodex-app-aarch64-apple-darwin.tar.gz` and listed in `SHA256SUMS`. The CLI
keeps the default linker signature: it never runs daemon code and needs no
privacy grants. When the `MACOS_DEVELOPER_ID_*` secrets are present the Hand is
signed with that Developer ID Application certificate (no hardened runtime or
notarization yet), so its designated requirement names the identifier and team
rather than one build's code hash. Without them the job signs ad hoc, emits an
"Unsigned macOS Hand" warning, and records `ad-hoc` in the job summary; macOS
treats every ad-hoc build as a new identity for Screen Recording and
Accessibility. The workflow verifies the signature, designated requirement and
entitlement on the runner; whether macOS keeps an existing grant across Hand
builds is verified only on a real Mac.

`nanocodex update --nightly` verifies and installs that complete
platform bundle atomically and exposes the CLI as `nanocodex`, `nc`, and `ncl`
under `$NANOCODEX_DIR/bin`; the invoked name selects the managed tree
(`nanocodex`, `nc`) or the local agent tree (`ncl`, or `nanocodex --local`).
The verified Hand companion is exposed as `nanocodex-hand` and `nc-hand`.
These names are aliases for the two role executables, not extra Cargo binaries.
`nanocodex update --branch NAME` and `nanocodex update --pr NUMBER` fetch source into a temporary
checkout, compile the CLI and Hand locally, and install them together; when the
built Hand reports the running Hand's identity, only the CLI changes.
The PR must be open; the updater checks that the fetched head still matches the
PR metadata. The source build requires Git and a working Rust toolchain, plus
`gh` for PR selection. Locally compiled source bundles do not include the native
voice runtime archive. Updaters published before this bundle contract need one
nightly update to promote the bundle-aware manager and a second invocation to
fetch `nanocodex2`; subsequent nightly updates install the complete bundle in
one invocation.

To install the newest published nightly, including the CLI, Hand, and voice
bundle, use:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash -s -- --nightly
```

Use `--help` to inspect the installer options without downloading a binary. For
an unattended installation into a separate prefix, pass `--no-setup` and
`--no-modify-path`, and set `NANOCODEX_DIR` on the shell side of the pipe.
The native updater bounds metadata retries and reports rate-limit reset times;
a transient failure can be retried with the same command.

To bootstrap or roll back to an exact published nightly, pin its full commit
tag on the shell side of the public installer pipeline:

```sh
curl -fsSL https://raw.githubusercontent.com/gakonst/nanocodex/master/install | NANOCODEX_RELEASE_TAG='nightly-<full-40-hex-commit>' sh
```

Replace the placeholder with the published tag. `NANOCODEX_RELEASE_TAG=vX.Y.Z`
also pins a stable release. The tagged installer and native installation both
use that exact tag; nightly metadata must target its named commit, and the
existing manifest checks verify the complete bundle. Without the variable,
the public installer and standalone `nanocodex install` select latest stable.
The pinned release must contain an installer and CLI supporting this contract.

## JavaScript package previews

Every pull request and every commit merged to `master` builds and tests the
actual Node/browser WASM and Vite packages, then publishes immutable SHA-addressed
previews through pkg.pr.new. The workflow can also be dispatched manually for a
selected ref. Preview package versions are rewritten to
`0.0.0-preview-<sha>` so a lockfile cannot confuse them with an npm release.

Install the pkg.pr.new GitHub App on `gakonst/nanocodex`; the preview CLI is a
pinned development dependency in the `js/nanocodex` and `js/nanocodex-vite`
lockfiles. Pull-request
comments use commit URLs, so every tested artifact remains reproducible after
new commits are pushed.

## Changelogs

There are two complementary kinds of release record:

- The root `CHANGELOG.md` is generated by `git-cliff` from every commit, while
  each of the eight published crates carries its own path-filtered
  `crates/*/CHANGELOG.md`. Conventional prefixes group changes into Features,
  Bug Fixes, Documentation, Dependencies, Performance, Refactor, Styling,
  Testing, Miscellaneous Tasks, and Other. This also records direct commits
  that did not arrive through a pull request.
- The GitHub Release is generated from merged pull requests by the same
  changelog builder used by Foundry. Pull requests are grouped by label, every
  line ends in `by @author`, uncategorized pull requests remain visible under
  Other, and the notes end with the full comparison link.

Use conventional commit subjects (`feat:`, `fix:`, `docs:`, `perf:`,
`refactor:`, `test:`, `chore:`, `ci:`, or `build:`). Before merging a pull
request, apply one release-note label:

- `breaking-change`
- `feature` or `enhancement`
- `fix` or `bug`
- `performance`
- `documentation`
- `dependencies`
- `internal`
- `ignore-for-release` only when the pull request should not appear

The label is for the readable GitHub Release; the commit prefix is for the
complete repository changelog. Neither path silently drops uncategorized work.

## Prepare a release

Install the release tools once:

```sh
cargo install git-cliff --locked
cargo install cargo-semver-checks --locked
```

Then prepare a release pull request from the latest `master`:

1. Choose a new semantic version and update `workspace.package.version` plus every
   `nanocodex*` entry in `workspace.dependencies` in `Cargo.toml`, and keep
   `js/nanocodex/package.json` and `js/nanocodex-vite/package.json` on that same
   version. Never reuse a version
   already published to crates.io; the stable-API refactor after `0.2.0` is
   source-breaking and therefore requires a new version.
2. Run `cargo check --workspace` to refresh `Cargo.lock`.
3. Generate the committed changelog after all intended feature commits are in
   the branch:

   ```sh
   just changelog x.y.z
   ```

   This runs the same `WORKSPACE_ROOT`/`CRATE_ROOT` hook used by Alloy and by
   `cargo-release`, refreshing the root changelog and all eight crate-specific
   changelogs. Review the generated groups and wording. Fix misleading commit
   subjects in the release branch when necessary, then regenerate; do not
   hand-maintain a second grouping scheme. A short editorial Highlights section
   may be added to the root changelog above the generated groups, but it must
   not replace or reorder them.
4. Starting with the second published version, run
   `cargo +stable semver-checks` and review every reported API break. A breaking
   result must either be intentional and reflected in the chosen version, or
   fixed before release.
5. Run `just release-check x.y.z` and the normal `just check` gate. The release
   check packages every crate and builds its all-features documentation with
   rustdoc warnings denied from the normalized `.crate` archives—the dependency
   view docs.rs will actually receive.
6. Merge the release preparation through normal review. Verify the release
   commit is the exact commit to ship.

## Trigger a release

Create and push one annotated tag matching the workspace version:

```sh
git tag -a vx.y.z -m "Nanocodex x.y.z"
git push origin vx.y.z
```

The tag starts the release workflow. It:

1. rejects a tag/version mismatch or any root/crate changelog not generated by
   `git-cliff`;
2. validates all crate packages and archive documentation;
3. creates a **draft** GitHub Release with grouped PR notes and contributor
   attribution;
4. builds the optimized CLI (`nanocodex-*`) and Hand (`nanocodex2-*`) for x86_64
   Linux, Apple Silicon macOS, and the Windows installer payload, and signs the
   macOS Hand and `Nanocodex.app`;
5. publishes the eight crates to crates.io in dependency order;
6. builds, tests, and publishes the Node/browser WASM package to npm with
   provenance;
7. attaches gzip-compressed binaries, raw compatibility executables, and
   `SHA256SUMS` to the draft.

The release check also exercises the public installer contract against both
asset layouts. The installer prefers the compressed binary and uses the raw
compatibility executable only when that is the artifact named by
`SHA256SUMS`, as on `v0.5.0`.

Open the draft at <https://github.com/gakonst/nanocodex/releases>, inspect the
notes, verify `SHA256SUMS`, and smoke-test a downloaded platform binary. Then
click **Publish release** and immediately smoke the public curl installer and
`nanocodex update`. Draft assets are not exposed through GitHub's public
`releases/latest` URL, so that final installer smoke necessarily follows
publication. The manual publish is deliberate: the tag starts irreversible
crates.io publishing, but the public GitHub announcement still gets a final
human editorial check.

## GitHub configuration

Optional macOS Hand signing uses `MACOS_DEVELOPER_ID_P12_BASE64` (a base64
PKCS#12 export of the Developer ID Application certificate and private key),
`MACOS_DEVELOPER_ID_P12_PASSWORD`, and `MACOS_DEVELOPER_ID_TEAM_ID`. The
certificate is imported into a temporary keychain that is deleted after signing.

The Actions secret `CARGO_REGISTRY_TOKEN` must contain a crates.io token allowed
to publish all eight `nanocodex*` crates. Configure `nanocodex` on npm with the
GitHub trusted publisher `gakonst/nanocodex`, workflow `release.yml`, allowed to
run `npm publish`. The publishing job uses OIDC and emits npm provenance. The
first package publication must claim the unscoped name using an npm account
with two-factor authentication; until trusted publishing is configured, an
`NPM_TOKEN` repository secret may authenticate that first workflow run.

The workflow otherwise uses the short-lived repository `GITHUB_TOKEN`; write
permission is restricted to the draft-release and asset-upload jobs.

The repository labels and `.github/changelog.json` are part of the release
contract. When adding or renaming a category, update both together.

## Recovery

The crate publisher queries crates.io and skips package versions already
present, so a registry propagation failure can be retried by rerunning the tag
workflow. An existing draft is reused, its notes are refreshed, and assets are
uploaded with `--clobber`. A published release is never mutated by a rerun.

Published crate and npm versions are immutable. Fix a bad release with a new
patch version; only yank a version when continuing to resolve it would harm
users.
