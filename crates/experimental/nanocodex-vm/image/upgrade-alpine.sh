#!/bin/sh
# Run through an existing VM Hand. Never mount, replace, or resize its root disk.
set -eu
test "$(id -u)" = 0 || { echo 'Run as root inside the existing Alpine VM' >&2; exit 1; }
test -f /etc/alpine-release || { echo 'Expected an Alpine VM' >&2; exit 1; }
cat /etc/alpine-release
df -h /
free_kib=$(df -Pk / | awk 'NR == 2 { print $4 }')
# Leave a conservative margin for package unpacking and retained user files.
test "$free_kib" -ge 409600 || { echo 'Need at least 400 MiB free; no files changed' >&2; exit 1; }
apk add --no-cache xvfb openbox xterm font-dejavu
apk info -v | awk '/^(xvfb|openbox|xterm|font-dejavu)-[0-9]/'
for binary in Xvfb openbox xterm; do command -v "$binary"; done
df -h /
echo 'Display packages installed. VM and guest runtime have not been restarted.'
