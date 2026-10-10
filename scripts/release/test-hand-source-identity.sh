#!/usr/bin/env bash
# Black-box check of hand-source-identity.py on a synthetic workspace laid out
# like the release tree: a CLI package with the Hand and shared packages nested
# inside it. Asserts which edits change the identity and that verify fails
# closed on Hand build inputs the identity did not hash.
set -euo pipefail
script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/hand-source-identity.py"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
ws="$work/ws"
mkdir -p "$ws"/{.cargo,scripts/tests,macos/HandMenuBar,app/src,app/hand/src,app/hand/tests,app/shared/src}
# Offline vendored registry: mid accepts leaf 1 or 2 and the Hand closure
# links both majors, so Cargo.lock can re-point mid's edge without changing
# the package set.
for crate in "leaf 1.0.0" "leaf 2.0.0" "mid 1.0.0"; do
  read -r name version <<<"$crate"
  dir="$ws/vendor/$name-$version"; mkdir -p "$dir/src"
  printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n' "$name" "$version" > "$dir/Cargo.toml"
  [[ "$name" == mid ]] && printf '[dependencies]\nleaf = ">=1, <3"\n' >> "$dir/Cargo.toml"
  echo 'pub fn f() {}' > "$dir/src/lib.rs"
  printf '{"files":{},"package":"%s"}' "$(printf '%s' "$crate" | sha256sum | cut -c1-64)" > "$dir/.cargo-checksum.json"
done
cd "$ws"
cat > Cargo.toml <<'EOF'
[workspace]
resolver = "2"
members = ["app", "app/hand", "app/shared"]
EOF
cat > app/Cargo.toml <<'EOF'
[package]
name = "cli"
version = "0.1.0"
edition = "2021"
[[bin]]
name = "nanocodex"
path = "src/main.rs"
[dependencies]
shared = { path = "shared" }
EOF
cat > app/hand/Cargo.toml <<'EOF'
[package]
name = "hand-daemon"
version = "0.1.0"
edition = "2021"
[[bin]]
name = "nanocodex-hand"
path = "src/main.rs"
[features]
tempo = []
[dependencies]
shared = { path = "../shared" }
mid = "1"
leaf = "1"
EOF
cat > app/shared/Cargo.toml <<'EOF'
[package]
name = "shared"
version = "0.1.0"
edition = "2021"
[dependencies]
leaf = "2"
EOF
echo 'fn main() {}' > app/src/main.rs
echo '// terminal UI' > app/src/tui.rs
echo 'fn main() {}' > app/hand/src/main.rs
echo '#[test] fn t() {}' > app/hand/tests/journey.rs
echo 'pub fn f() {}' > app/shared/src/lib.rs
printf '[source.crates-io]\nreplace-with = "vendored"\n[source.vendored]\ndirectory = "vendor"\n' > .cargo/config.toml
for input in nanocodex-vm.entitlements scripts/tests/linux-screen-helpers-bundle.py \
  macos/HandMenuBar/main.swift scripts/aarch64-unknown-linux-musl-linker scripts/aarch64-unknown-linux-musl-ar; do
  echo "# $input" > "$input"
done
echo helpers-v1 > "$work/screen-helpers.tar.gz"
git init -q . && cargo generate-lockfile --offline -q

identity() {
  python3 "$script" compute --target x86_64-unknown-linux-gnu --profile release --features "${features-tempo}" \
    --payload "screen-helpers=$work/screen-helpers.tar.gz" --report "$work/report.json"
}
expect() { # expect same|changed DESCRIPTION
  local next; next="$(identity)"
  [[ "$next" =~ ^[0-9a-f]{64}$ ]] || { echo "FAIL: no identity after $2" >&2; exit 1; }
  if [[ "$1" == same && "$next" != "$current" ]] || [[ "$1" == changed && "$next" == "$current" ]]; then
    echo "FAIL: expected identity $1 after $2" >&2; exit 1
  fi
  echo "ok: identity $1 after $2"; current="$next"
}
current="$(identity)"
expect same "recomputing an unchanged tree"
echo '// edit' >> app/src/tui.rs;              expect same "a CLI-only source edit"
echo '// edit' >> app/src/main.rs;             expect same "a CLI entry point edit"
echo '// edit' >> app/hand/tests/journey.rs;   expect same "a Hand integration-test edit"
echo '// edit' >> app/hand/src/main.rs;        expect changed "a Hand source edit"
echo '// edit' >> app/shared/src/lib.rs;       expect changed "a shared-package edit"
echo '# edit' >> .cargo/config.toml;           expect changed "a Cargo config edit"
echo helpers-v2 > "$work/screen-helpers.tar.gz"; expect changed "a native payload change"
features=""; expect changed "a feature change"; unset features; current="$(identity)"
export CARGO_TARGET_DIR="$work/elsewhere/target"
expect same "moving only the Cargo output directory"
unset CARGO_TARGET_DIR
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=x86-64-v3"
expect changed "target-specific rustflags"
unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS; current="$(identity)"

# Same package set and file bytes, different resolved edge: mid -> leaf 1.
grep -A6 '^name = "mid"$' Cargo.lock | grep -q '"leaf 2.0.0"' || { echo "FAIL: fixture did not resolve mid to leaf 2" >&2; exit 1; }
packages_before="$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print([n for k,n,_ in r["records"] if k=="lock"], r["files"])' "$work/report.json")"
sed -i '/^name = "mid"$/,/^$/ s/"leaf 2.0.0"/"leaf 1.0.0"/' Cargo.lock
grep -A6 '^name = "mid"$' Cargo.lock | grep -q '"leaf 1.0.0"' || { echo "FAIL: Cargo.lock edge edit did not apply" >&2; exit 1; }
expect changed "re-pointing a locked dependency edge with an unchanged package set"
packages_after="$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print([n for k,n,_ in r["records"] if k=="lock"], r["files"])' "$work/report.json")"
[[ "$packages_before" == "$packages_after" ]] || { echo "FAIL: the edge fixture changed the package or file set" >&2; exit 1; }
echo "ok: the edge fixture kept the locked package set and hashed files identical"

verify() { python3 "$script" verify --report "$work/report.json" --dep-info "$work/hand.d"; }
printf '%s: %s %s %s\n' "$ws/target/release/nanocodex-hand" "$ws/app/hand/src/main.rs" "$ws/app/shared/src/lib.rs" \
  "$ws/Cargo.toml $ws/rust-toolchain.toml $ws/target/release/build/out.rs $work/screen-helpers.tar.gz" > "$work/hand.d"
verify >/dev/null && echo "ok: verify accepts hashed sources, absent optional inputs, outputs and payloads"
printf '%s: %s\n' "$ws/target/release/nanocodex-hand" "$ws/app/src/tui.rs" > "$work/hand.d"
if verify 2>/dev/null; then echo "FAIL: verify accepted an unhashed workspace input" >&2; exit 1; fi
echo "ok: verify rejects a Hand input outside the hashed closure"
echo external > "$work/extra.bin"
printf '%s: %s\n' "$ws/target/release/nanocodex-hand" "$work/extra.bin" > "$work/hand.d"
if verify 2>/dev/null; then echo "FAIL: verify accepted an undeclared external input" >&2; exit 1; fi
echo "ok: verify rejects an undeclared external input"
printf '%s: %s\n' "$ws/target/release/nanocodex-hand" "$ws/app/hand" > "$work/hand.d"
if verify 2>/dev/null; then echo "FAIL: verify accepted a directory input with unhashed tests" >&2; exit 1; fi
echo "ok: verify rejects a directory input containing unhashed files"
echo "hand-source-identity: all checks passed"
