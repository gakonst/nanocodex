# Rust dependency layers for the sandbox and phone images

Both Dockerfiles install `cargo-chef 0.1.78` with `cargo install --locked` before
copying application sources. This installation is itself a reusable image layer.
The sandbox keeps Ubuntu 22.04 and Rust 1.97.0; the phone keeps
`rust:1.97.0-bookworm`. The runtime images, default features, package/binary
selection, release profile, two-job limit, and `CI_TESTS_ENABLED` check remain
unchanged.

The planner sees the original complete Rust context and creates `recipe.json`.
The builder starts again from the toolchain stage, copies only that recipe, and
cooks dependencies in the same `/source` working directory used by the final
build. Only then does it copy the complete original context, restoring real
sources, external binary entrypoints, build scripts, and embedded assets.
**Never run `cargo chef cook` in a checkout:** it replaces source files with dummies.

There are no Cargo cache mounts on these build steps. Dependency objects under
`/source/target`, downloaded registry sources, and Git dependencies must all be
ordinary layer contents so a fresh hosted builder can import them together from
the existing registry cache (`mode=max`). An existing source or embedded-asset
edit changes the final application layer but leaves the recipe layer reusable.
Dependency, feature, profile, discovered target, and registry lockfile changes
invalidate the recipe. Cargo-chef deliberately normalizes formatting, lints, and
local package versions; those edits need not invalidate external dependencies.

We use unfiltered `prepare` on the workspace's existing lockfile. This avoids the
full dependency resolution/network access required by `prepare --bin`. Cooking
still uses exactly the package/binary flags of the final build. The current
Docker contexts omit `.cargo` and rust-toolchain files, as before.

The pin and behavior were checked against the upstream
[v0.1.78 source](https://github.com/LukeMathWalker/cargo-chef/tree/v0.1.78), including
its [changelog](https://github.com/LukeMathWalker/cargo-chef/blob/v0.1.78/CHANGELOG.md),
[recipe implementation](https://github.com/LukeMathWalker/cargo-chef/blob/v0.1.78/src/recipe.rs),
and [skeleton implementation](https://github.com/LukeMathWalker/cargo-chef/blob/v0.1.78/src/skeleton/mod.rs).

## Focused verification

Structural checks need only Node:

```sh
node --test scripts/ci/docker-rust-recipe.test.mjs
```

To also exercise actual recipes with Rust 1.97.0 and the real workspace, install
the pinned tool in a private directory and opt in explicitly:

```sh
cargo +1.97.0 install cargo-chef --version 0.1.78 --locked --jobs 2 --root "$PWD/.cache/docker-chef-tool"
CARGO_CHEF_TEST_BIN="$PWD/.cache/docker-chef-tool/bin/cargo-chef" node --test scripts/ci/docker-rust-recipe.test.mjs
```

This test regenerates the sandbox's ignored build context, then mutates only
throwaway copies under `.cache`. It checks recipe equality between both Docker
contexts; source and embedded-asset edits; dependency manifest, release profile,
registry checksum, and new build-script invalidation; and the `../` target paths
for `nanocodex2` and `phone-voice-cloud`. `cook --no-build` reconstructs the real
workspace skeleton in an empty directory and Cargo validates its target metadata.
Overlay checks confirm the original assets and entrypoints return. It does not
compile production dependencies and does not enable any paused CI test suite.

Hosted validation must still build both complete Linux images and run the
existing image checks. Record a cold run and a run with only an existing Rust
source edited; the second must import the registry cache on a fresh builder and
show the `cargo chef cook` step cached. Compare final Rust compilation and total
wall time, plus cache export size/time: storing dependency objects in layers can
increase cache export volume. A recipe test or Docker `--check` alone is not
proof of dependency compilation, runtime correctness, or a measured speedup.
