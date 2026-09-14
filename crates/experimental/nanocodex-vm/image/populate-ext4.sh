#!/bin/sh
# Read a Docker filesystem export; no privileged mounts are required.
set -eu
size_mib=${1:?Expected disk size in MiB}
mkdir /rootfs
tar --extract --numeric-owner --same-owner --preserve-permissions -f - -C /rootfs
for binary in usr/bin/Xvfb usr/bin/openbox usr/bin/xterm; do
  test -x "/rootfs/$binary" || { echo "Missing executable: /$binary" >&2; exit 1; }
done
# Reject legacy desktop runtimes anywhere in the template.
find /rootfs \( -name nanocodex-remote -o -name 'waymote*' -o -name grim \) -print > /out/forbidden-files.txt
test ! -s /out/forbidden-files.txt || { cat /out/forbidden-files.txt >&2; exit 1; }
test -d /rootfs/usr/share/fonts/dejavu
rm -f /rootfs/.dockerenv /rootfs/etc/hostname /rootfs/etc/hosts /rootfs/etc/resolv.conf
printf '127.0.0.1 localhost\n::1 localhost\n' > /rootfs/etc/hosts
printf 'nanocodex-desktop\n' > /rootfs/etc/hostname
: > /rootfs/etc/resolv.conf
mkdir -p /rootfs/app /rootfs/workspace /rootfs/proc /rootfs/sys /rootfs/dev /rootfs/run /rootfs/tmp
chmod 1777 /rootfs/tmp
cp /rootfs/lib/apk/db/installed /out/packages.txt
truncate -s "${size_mib}M" /out/desktop.ext4
mkfs.ext4 -q -F -L nc-desktop -m 0 -O '^orphan_file' \
  -E lazy_itable_init=0,lazy_journal_init=0 -d /rootfs /out/desktop.ext4
e2fsck -f -n /out/desktop.ext4 > /out/fsck.txt 2>&1
cat /out/fsck.txt
for binary in /usr/bin/Xvfb /usr/bin/openbox /usr/bin/xterm; do
  debugfs -R "dump $binary /checked-binary" /out/desktop.ext4 >/dev/null 2>&1
  cmp "/rootfs$binary" /checked-binary
  sha256sum "/rootfs$binary" >> /out/binaries.sha256
  rm /checked-binary
done
sha256sum /out/desktop.ext4 > /out/desktop.sha256
# Docker preserves root ownership on Linux bind mounts. Publish artifacts to the
# invoking user while retaining the original numeric ownership inside ext4.
chown "${OUTPUT_UID:?}:${OUTPUT_GID:?}" /out/*
