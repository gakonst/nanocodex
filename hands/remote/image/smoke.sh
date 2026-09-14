#!/usr/bin/env bash
set -euo pipefail

image=${1:?Usage: smoke.sh IMAGE ARCH}
arch=${2:?Usage: smoke.sh IMAGE ARCH}
case "$arch" in amd64|arm64) ;; *) echo 'Expected amd64 or arm64' >&2; exit 2 ;; esac
test "$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")" = "linux/$arch"
docker run --rm --network none --entrypoint /usr/local/bin/nanocodex-remote "$image" server-host --help
docker run --rm --network none --entrypoint /usr/local/bin/waymote-streamd "$image" --help

container="nanocodex-hand-smoke-$(date +%s)-$$"
trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
docker run --detach --name "$container" --network none --cap-drop=ALL \
  --security-opt=no-new-privileges --shm-size=512m "$image" >/dev/null
if ! docker exec "$container" sh -ec '
  test "$(id -u)" = 1000
  attempt=0
  until test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"; do
    attempt=$((attempt + 1))
    test "$attempt" -le 40
    sleep 0.25
  done
  # Debian grim disables direct JPEG output; the daemon also starts from PNG.
  grim -t png -l 1 -s 0.8 /tmp/hand-smoke.png
  ffmpeg -hide_banner -loglevel error -i /tmp/hand-smoke.png -frames:v 1 /tmp/hand-smoke.jpg
  test -s /tmp/hand-smoke.jpg
  ffmpeg -hide_banner -loglevel error -i /tmp/hand-smoke.jpg -frames:v 1 -f null -
  status=0
  # Waymote keeps its control pipe open for the owning daemon. An immediate
  # stdin EOF would stop capture before the first frame in a noninteractive exec.
  timeout 4s sh -c "sleep 30 | waymote-streamd --frame-rate 5 --bitrate 1000" > /tmp/hand-smoke.h264 || status=$?
  test "$status" = 0 || test "$status" = 124
  test -s /tmp/hand-smoke.h264
  ffmpeg -hide_banner -loglevel error -i /tmp/hand-smoke.h264 -frames:v 1 -f null -
'; then
  docker logs "$container" >&2
  exit 1
fi
echo "Verified linux/$arch: non-root desktop, JPEG capture, H.264 encoding, daemon startup."
