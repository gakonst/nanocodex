#!/usr/bin/env bash
# Native: requires Python 3, git, curl, xz, cc, pkg-config, readelf, objcopy, Meson, Ninja,
# libwayland-dev/libwayland-bin, libxkbcommon-dev, libpixman-1-dev, libpng-dev.
# Container mode is for release runners, not an implicit host package installer.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
if [[ "${1:-}" == --auto ]]; then
  output=${2:?usage: build-linux-screen-helpers.sh --auto OUTPUT.tar.gz WORK_DIR}
  work=${3:?usage: build-linux-screen-helpers.sh --auto OUTPUT.tar.gz WORK_DIR}
  missing=()
  for tool in python3 git curl xz cc pkg-config readelf objcopy meson ninja dpkg-query; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  if command -v pkg-config >/dev/null 2>&1; then
    for library in wayland-client xkbcommon pixman-1 libpng; do
      pkg-config --exists "$library" || missing+=("$library development files")
    done
  fi
  [[ -x /usr/bin/wayland-scanner ]] || missing+=("/usr/bin/wayland-scanner")
  if (( ${#missing[@]} == 0 )); then
    exec python3 "$root/scripts/build-linux-screen-helpers.py" --work-dir "$work" --output "$output"
  fi
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    exec bash "$root/scripts/build-linux-screen-helpers.sh" --container "$output"
  fi
  printf 'Cannot build self-contained Linux screen helpers: missing native prerequisites: %s. Install the documented build tools/development libraries yourself or provide Docker with a running accessible daemon. No host package installation was attempted.\n' "${missing[*]}" >&2
  exit 1
fi
if [[ "${1:-}" == --container ]]; then
  shift
  output=${1:?usage: build-linux-screen-helpers.sh --container OUTPUT.tar.gz}
  mkdir -p "$(dirname "$output")"
  output=$(cd "$(dirname "$output")" && pwd)/$(basename "$output")
  command -v docker >/dev/null 2>&1 || { echo 'Docker is required for container helper builds' >&2; exit 1; }
  # Source updates may compile different revisions concurrently. Never run a
  # shared mutable tag which another update could replace between build/run.
  image=$(mktemp)
  trap 'rm -f "$image"' EXIT
  docker build -f "$root/scripts/build-linux-screen-helpers.Dockerfile" --iidfile "$image" "$root"
  docker run --rm --user "$(id -u):$(id -g)" -v "$(dirname "$output"):/out" "$(cat "$image")" \
    --work-dir /tmp/screen-build --output "/out/$(basename "$output")"
else
  exec python3 "$root/scripts/build-linux-screen-helpers.py" "$@"
fi
