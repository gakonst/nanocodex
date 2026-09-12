#!/usr/bin/env bash
set -euo pipefail

# Build for the selected Docker daemon, including remote Docker contexts.
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
image=${1:-nanocodex-hand:local}
platform=$(docker version --format '{{.Server.Os}}/{{.Server.Arch}}')
case "$platform" in
  linux/arm64) target=aarch64-unknown-linux-musl ;;
  linux/amd64) target=x86_64-unknown-linux-musl ;;
  *) printf 'Unsupported Docker platform: %s\n' "$platform" >&2; exit 1 ;;
esac

cargo build --locked --release -p nanocodex-vm --bin nanocodex-vm-guest \
  --no-default-features --features guest-runtime --target "$target"
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
cp "${CARGO_TARGET_DIR:-target}/$target/release/nanocodex-vm-guest" "$context/"
cp crates/experimental/nanocodex-vm/image/Dockerfile.hand "$context/Dockerfile"
cp -R crates/experimental/nanocodex-vm/image/toolkit "$context/toolkit"
docker build --platform "$platform" --tag "$image" \
  --build-context computer-source=crates/experimental/nanocodex-computer/runtime "$context"
