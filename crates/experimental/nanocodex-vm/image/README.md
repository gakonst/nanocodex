# Rust VM desktop image

This Alpine 3.21 template supplies Xvfb, Openbox, xterm, DejaVu fonts, and
Mesa's Gallium software renderer. The renderer gives the private X11 desktop
a modern OpenGL core profile even when the VM host does not expose a GPU
device, so applications such as Blender can open directly in the published
desktop. Rendering is CPU-backed unless a future VM transport exposes an
accelerated DRM device.
The separately built `nanocodex-vm-guest` starts the X11 session and implements
capture and input directly through Rust `x11rb`. The image contains no Go
`nanocodex-remote`, Waymote, grim, Go compiler, or Zig compiler. It does not
use the legacy desktop image in `hands/remote/image`.

Build on a Docker host (Apple Silicon VMs require `linux/arm64`):

```sh
docker build --platform linux/arm64 -t nanocodex-vm-x11:alpine3.21 \
  crates/experimental/nanocodex-vm/image
crates/experimental/nanocodex-vm/image/build-root.sh \
  nanocodex-vm-x11:alpine3.21 /absolute/path/desktop-x11.ext4 2048
```

The build uses an isolated Linux filesystem packager, with no privileged
mounts. It preserves numeric ownership, validates required and forbidden
files, runs read-only `e2fsck`, and compares executable bytes extracted from
the ext4 image. The immutable output has SHA-256, source image identity,
package inventory, executable checksums, and filesystem-check sidecars.
The base Alpine digest is pinned; APK repository updates mean rebuilds can
have different packages and checksums. The Rust runtime remains on its
separate runtime disk and must match the host build.

Point the host's template configuration at the new image only after a fresh
VM validates capture and input. Existing VMs retain their private root disk;
changing the template does not upgrade their installed packages.

To add desktop and software OpenGL infrastructure to an existing Alpine VM, run
`upgrade-alpine.sh` as root through its connected Hand. It requires 400 MiB
free and adds only the display packages and their dependencies. It never
replaces a disk, edits workspace files, launches a display, or restarts the
VM. Retain existing files and let the host owner stage the matching Rust
guest runtime and restart the VM after the package upgrade so the new Xvfb
process can load Gallium's software driver.
