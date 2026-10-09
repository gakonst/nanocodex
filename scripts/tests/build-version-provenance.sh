#!/usr/bin/env bash
# Black-box check of bin/nanocodex/build_version.rs provenance caching:
# builds a tiny binary whose build script is that file inside a scratch Git
# repository, packs the branch ref (as git gc does), and asserts that
#   1. a repeated build with no change does not rerun provenance, and
#   2. the first commit after packing embeds the new HEAD SHA.
# usage: scripts/tests/build-version-provenance.sh [path/to/build_version.rs]
# Set CARGO_NET_OFFLINE=true to reuse the local registry cache only.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source_file="$(realpath "${1:-$root/bin/nanocodex/build_version.rs}")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
repo="$work/repo"
mkdir -p "$repo/src" "$repo/.cargo"
cd "$repo"
cat > Cargo.toml <<EOF
[package]
name = "provenance-fixture"
version = "0.0.0"
edition = "2024"
build = "build.rs"
[workspace]
[build-dependencies]
chrono = { version = "0.4.43", default-features = false, features = ["clock", "std"] }
vergen = { version = "8", default-features = false, features = ["build", "git", "gitcl"] }
EOF
printf '#[path = "%s"]\nmod build_version;\nfn main() { build_version::emit().unwrap(); }\n' "$source_file" > build.rs
echo 'fn main() { println!("{}", env!("VERGEN_GIT_SHA")); }' > src/main.rs
printf '[env]\nNANOCODEX_BUILD_CHECKOUT = { value = ".", relative = true, force = true }\n' > .cargo/config.toml
cp "$root/Cargo.lock" .
export CARGO_TARGET_DIR="$work/target"
commit() { git -c user.name=fixture -c user.email=fixture@example.invalid commit -qm "$1"; }
git init -q -b feature/packed
git add -A && commit one
git pack-refs --all
test ! -e .git/refs/heads/feature/packed
embedded="$(cargo run -q)"
[[ "$embedded" == "$(git rev-parse HEAD)" ]] || { echo "FAIL: initial build embedded $embedded"; exit 1; }
if cargo build -v 2>&1 | grep -q 'Running.*build-script-build'; then
  echo "FAIL: a repeated build reran provenance"; exit 1
fi
echo change > change.txt && git add -A && commit two
embedded="$(cargo run -q)"
head="$(git rev-parse HEAD)"
[[ "$embedded" == "$head" ]] || { echo "FAIL: embedded $embedded after commit, HEAD is $head"; exit 1; }
echo "PASS: provenance is cached and follows commits on a packed branch ($head)"
