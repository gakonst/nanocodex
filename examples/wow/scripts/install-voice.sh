#!/usr/bin/env bash
set -euo pipefail
root="${NANOCODEX_WOW_VOICE_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/nanocodex-wow-voice}"
mkdir -p "$root"
cd "$root"
command -v ffmpeg >/dev/null || { echo 'Install ffmpeg first.' >&2; exit 1; }
cmake_bin="$(command -v cmake || true)"
if [[ -z "$cmake_bin" ]]; then
  [[ "$(uname -m)" == x86_64 ]] || { echo 'Install cmake for this architecture.' >&2; exit 1; }
  base=https://github.com/Kitware/CMake/releases/download/v3.31.6
  curl -fL --retry 2 "$base/cmake-3.31.6-linux-x86_64.tar.gz" -o cmake.tar.gz
  curl -fL --retry 2 "$base/cmake-3.31.6-SHA-256.txt" -o cmake-checksums.txt
  expected=$(awk '$2=="cmake-3.31.6-linux-x86_64.tar.gz" {print $1}' cmake-checksums.txt)
  [[ "$expected" =~ ^[a-f0-9]{64}$ ]]
  printf '%s  cmake.tar.gz\n' "$expected" | sha256sum -c -
  tar xzf cmake.tar.gz
  cmake_bin="$root/cmake-3.31.6-linux-x86_64/bin/cmake"
fi
revision=4979e04f5dcaccb36057e059bbaed8a2f5288315
if [[ ! -d whisper.cpp/.git ]]; then git clone https://github.com/ggml-org/whisper.cpp.git whisper.cpp; fi
git -C whisper.cpp checkout --detach "$revision"
"$cmake_bin" -S whisper.cpp -B whisper.cpp/build -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DGGML_VULKAN=OFF -DGGML_NATIVE=OFF -DWHISPER_BUILD_TESTS=OFF
"$cmake_bin" --build whisper.cpp/build --config Release -j "${VOICE_BUILD_JOBS:-4}"
model=ggml-tiny.en.bin
expected=921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f
if ! printf '%s  %s\n' "$expected" "$model" | sha256sum -c - 2>/dev/null; then
  curl -fL --retry 2 https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin -o "$model.part"
  printf '%s  %s.part\n' "$expected" "$model" | sha256sum -c -
  mv "$model.part" "$model"
fi
chmod -R a+rX "$root"
printf 'Local speech ready: %s\n' "$root"
