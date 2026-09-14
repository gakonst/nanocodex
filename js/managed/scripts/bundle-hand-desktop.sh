#!/bin/sh
set -eu
# Preserve the SDK's system libc. Only these desktop executables use the
# bundled loader and dependency directory; no global LD_LIBRARY_PATH is set.
bundle=/opt/nanocodex-desktop/lib
mkdir -p "$bundle" /out/desktop-bin
for binary in /usr/bin/labwc /usr/bin/foot /usr/bin/ffmpeg /usr/bin/grim /usr/bin/wlr-randr /usr/bin/Xwayland /src/waymote/zig-out/bin/waymote-streamd; do
  ldd "$binary" | awk '/=> \// { print $3 } /^\s*\// { print $1 }' | while read -r library; do
    cp -L "$library" "$bundle/"
  done
  cp "$binary" /out/desktop-bin/
done
loader=/lib64/ld-linux-x86-64.so.2
cp -L "$loader" "$bundle/"
for binary in /out/desktop-bin/*; do
  patchelf --set-interpreter "$bundle/ld-linux-x86-64.so.2" --force-rpath --set-rpath "$bundle" "$binary"
done
for library in "$bundle"/*; do
  case "$library" in */ld-linux-*) continue ;; esac
  patchelf --force-rpath --set-rpath "$bundle" "$library"
done
