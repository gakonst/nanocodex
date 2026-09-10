#!/usr/bin/env bash
set -euo pipefail
image=${1:?Usage: build-root.sh IMAGE OUTPUT.ext4 [SIZE_MIB]}
output=${2:?Usage: build-root.sh IMAGE OUTPUT.ext4 [SIZE_MIB]}
size_mib=${3:-2048}
case "$size_mib" in ''|*[!0-9]*) echo 'Expected disk size in MiB' >&2; exit 2 ;; esac
test "$size_mib" -ge 512
# Refuse replacement of existing images and evidence, including dangling symlinks.
for path in "$output" "$output".{fsck.txt,binaries.sha256,packages.txt,forbidden-files.txt,source.txt,sha256}; do
  if test -e "$path" || test -L "$path"; then echo "Output already exists: $path" >&2; exit 1; fi
done
script_dir=$(cd "$(dirname "$0")" && pwd)
parent=$(cd "$(dirname "$output")" && pwd)
output="$parent/$(basename "$output")"
work=$(mktemp -d "$parent/.vm-x11-build.XXXXXX")
container=""
cleanup() {
  if test -n "$container"; then docker rm "$container" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
packager=$(docker build --quiet --file "$script_dir/Dockerfile.ext4" "$script_dir")
container=$(docker create "$image")
docker export "$container" | docker run --rm --interactive --network none \
  --mount "type=bind,source=$work,target=/out" "$packager" "$size_mib"
printf '%s\n' "$(docker image inspect --format '{{.Id}} {{.Os}}/{{.Architecture}}' "$image")" > "$work/source.txt"
printf '%s  %s\n' "$(awk '{print $1}' "$work/desktop.sha256")" "$output" > "$work/image.sha256"
chmod 444 "$work/desktop.ext4"
# A hard link publishes without replacing any existing destination.
ln "$work/desktop.ext4" "$output"
for evidence in fsck.txt binaries.sha256 packages.txt forbidden-files.txt source.txt; do
  ln "$work/$evidence" "$output.$evidence"
done
ln "$work/image.sha256" "$output.sha256"
echo "Prepared $output; supply a matching Rust guest runtime separately."
