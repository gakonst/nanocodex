#!/usr/bin/env bash
# Prepare a relocatable Apple Silicon Vulkan Hand and a clean desktop template.
set -euo pipefail
output=${1:?Usage: build-macos-gpu-hand.sh OUTPUT_DIR FIRMWARE_DIR}
firmware=${2:?Supply the libkrunfw directory}
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || { echo 'Requires Apple Silicon macOS' >&2; exit 1; }
[[ -f "$firmware/libkrunfw.5.dylib" ]] || { echo 'Missing firmware' >&2; exit 1; }
[[ ! -e "$output" && ! -L "$output" ]] || { echo "Output already exists: $output" >&2; exit 1; }
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
virgl=$(brew --prefix virglrenderer)
molten=$(bash scripts/build-macos-moltenvk.sh "${CARGO_TARGET_DIR:-$repo/target}/native-gpu")
epoxy=$(brew --prefix libepoxy)
[[ -f "$virgl/lib/libvirglrenderer.1.dylib" ]] || { echo 'Install slp/krun/virglrenderer first' >&2; exit 1; }
mkdir -p "$output/lib" "$output/licenses" "$output/firmware"
output=$(cd "$output" && pwd)
export LIBRARY_PATH="$virgl/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export PKG_CONFIG_PATH="$virgl/lib/pkgconfig:$epoxy/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
cargo build --locked --profile nightly -p nanocodex2-bin --bin nanocodex2 --features nanocodex-vm/gpu
cp "${CARGO_TARGET_DIR:-target}/nightly/nanocodex2" "$output/nanocodex2"
cargo build --locked --profile nightly -p nanocodex-vm --bin nanocodex-vm-guest --no-default-features --features guest-runtime --target aarch64-unknown-linux-musl
cp "${CARGO_TARGET_DIR:-target}/aarch64-unknown-linux-musl/nightly/nanocodex-vm-guest" "$output/nanocodex-vm-guest"
cp "$firmware/libkrunfw.5.dylib" "$output/firmware/"
cp "$virgl/lib/libvirglrenderer.1.dylib" "$output/lib/"
cp "$molten/libMoltenVK.dylib" "$output/lib/"
cp "$epoxy/lib/libepoxy.0.dylib" "$output/lib/"
cp "$virgl/COPYING" "$output/licenses/virglrenderer.txt"
cp "$molten/MoltenVK.LICENSE" "$output/licenses/MoltenVK.txt"
cp "$molten/SPIRV-Cross.LICENSE" "$output/licenses/SPIRV-Cross.txt"
cp "$epoxy/COPYING" "$output/licenses/libepoxy.txt"
# Rewrite only these explicit bundled dependencies; system frameworks remain system-owned.
for file in "$output/nanocodex2" "$output"/lib/*.dylib; do
  chmod u+w "$file"
  for library in libvirglrenderer.1.dylib libMoltenVK.dylib libepoxy.0.dylib; do
    dependency=$(otool -L "$file" | awk -v name="$library" '$1 ~ ("/" name "$") {print $1; exit}')
    if [[ -n "$dependency" ]]; then
      if [[ "$file" == "$output/nanocodex2" ]]; then
        install_name_tool -change "$dependency" "@executable_path/lib/$library" "$file"
      else
        install_name_tool -change "$dependency" "@loader_path/$library" "$file"
      fi
    fi
  done
  if [[ "$file" == *.dylib ]]; then
    install_name_tool -id "@loader_path/$(basename "$file")" "$file"
    codesign --force --sign - "$file"
  fi
done
codesign --force --sign - --entitlements nanocodex-vm.entitlements "$output/nanocodex2"
codesign --verify --strict "$output/nanocodex2"
image="nanocodex-desktop:mac-gpu-$(git rev-parse --short HEAD)"
docker build --platform linux/arm64 -f crates/experimental/nanocodex-vm/image/Dockerfile.gpu -t "$image" crates/experimental/nanocodex-vm/image
bash crates/experimental/nanocodex-vm/image/build-root.sh "$image" "$output/desktop.ext4" 16384
node --input-type=module - "$output" <<'JS'
import {writeFileSync} from 'node:fs';
import {join} from 'node:path';
const output=process.argv[2];
writeFileSync(join(output,'vm.json'), JSON.stringify({binary:join(output,'nanocodex2'),rootfs:join(output,'desktop.ext4'),guestRuntime:join(output,'nanocodex-vm-guest'),firmware:join(output,'firmware'),gpu:true},null,2)+'\n');
JS
printf 'Prepared GPU Hand assets and recipe at %s\n' "$output"
