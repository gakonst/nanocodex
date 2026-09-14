#!/usr/bin/env bash
set -euo pipefail
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"
target_dir="${CARGO_TARGET_DIR:-$repository_root/target}"
if [[ "$target_dir" != /* ]]; then target_dir="$repository_root/$target_dir"; fi
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
  cargo build --locked -p nanocodex-voice-ffi --release --target "$target"
done
mkdir -p "$target_dir/voice-core/macos" "$target_dir/voice-core/simulator"
xcrun lipo -create "$target_dir/aarch64-apple-darwin/release/libnanocodex_voice_ffi.a" "$target_dir/x86_64-apple-darwin/release/libnanocodex_voice_ffi.a" -output "$target_dir/voice-core/macos/libnanocodex_voice_ffi.a"
xcrun lipo -create "$target_dir/aarch64-apple-ios-sim/release/libnanocodex_voice_ffi.a" "$target_dir/x86_64-apple-ios/release/libnanocodex_voice_ffi.a" -output "$target_dir/voice-core/simulator/libnanocodex_voice_ffi.a"
headers="$repository_root/crates/experimental/nanocodex-voice-ffi/include"
artifact="$repository_root/apple/NanocodexVoice/Artifacts/NanocodexVoiceCore.xcframework"
rm -rf "$artifact"
xcodebuild -create-xcframework \
  -library "$target_dir/voice-core/macos/libnanocodex_voice_ffi.a" -headers "$headers" \
  -library "$target_dir/voice-core/simulator/libnanocodex_voice_ffi.a" -headers "$headers" \
  -library "$target_dir/aarch64-apple-ios/release/libnanocodex_voice_ffi.a" -headers "$headers" \
  -output "$artifact"
