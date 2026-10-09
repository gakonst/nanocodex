# Build-speed tips for agents working right now (env-only, no code changes)

Measured on this 10-core/32 GB Mac at load average 55-120 (the swarm itself), toolchain 1.97.1,
target nanocodex-bin. Raw data: output/build-speed/times.tsv (in the build-speed worktree:
.claude/worktrees/build-speed/output/build-speed/).

## 1. Stop the nanocodex-bin build script from recompiling the 150k-line bin crate on EVERY cargo call  (biggest win)

bin/nanocodex/build_version.rs emits `rerun-if-changed=nanocodex-version-always-rerun` (a file that never exists)
unless VERGEN_GIT_SHA is set, and it embeds a fresh build timestamp. Result: every `cargo test/check/build -p nanocodex-bin`
re-runs the build script and recompiles nanocodex-bin even when nothing changed.

Prefix your cargo commands with the current HEAD SHA (re-evaluated per command, so it stays correct after commits):

    VERGEN_GIT_SHA=$(git rev-parse HEAD) cargo test -p nanocodex-bin --bin nanocodex <filter>

Measured (no source change, warm target dir):
- `cargo test --no-run -p nanocodex-bin --bin nanocodex`: 55.9 s -> 4.0 s
- `cargo check -p nanocodex-bin --bin nanocodex --tests`: 28.3 s -> 10.6 s
(First call after setting it pays one rebuild.) A permanent fix lands on master from perf/build-speed.

## 2. Don't alternate `cargo check` and `cargo test`/`build` in the same CARGO_TARGET_DIR

check produces metadata-only artifacts, test/build produce full ones; they are separate caches.
Measured: `cargo check --tests` right after a full `cargo test --no-run` in the same target dir = 509 s (cold check of the whole graph).
Pick one loop: either `cargo test --no-run`/`cargo test <filter>` (also catches type errors), or `cargo check`/`cargo clippy`
(check and clippy share dependency artifacts).

## 3. Keep the same feature/package selection per target dir

Different `-p` sets / `--all-features` change feature unification and rebuild shared deps under new hashes
(e.g. output/targets/cli has 3 serde_derive builds). Reuse one command shape per target dir.
