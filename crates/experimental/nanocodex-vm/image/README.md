# Hand toolkit images

The portable VM, Docker Hand, and Apple Silicon GPU VM images include the same
Alpine 3.24 toolkit. Cloudflare and the standalone Linux Hand use their existing
glibc bases with equivalent development, document, science, and creative tools.
The specialized browser image and account relay remain separate services.

| Work | Installed tools |
| --- | --- |
| Repositories | Git, Git LFS, GitHub CLI, ripgrep, fd, jq, SSH, rsync, tmux |
| Development | Rust 1.97, rustfmt, Clippy, WASM target, Go, Node, npm, pnpm, uv, C/C++, CMake |
| Documents | LibreOffice, Pandoc, python-docx, python-pptx, openpyxl |
| PDF and OCR | Poppler, qpdf, Ghostscript, Tesseract English, pypdf, ReportLab |
| Data | NumPy, SciPy, pandas, Matplotlib, SymPy, Pillow, lxml |
| Creative | Blender, FFmpeg, ImageMagick, Inkscape, Graphviz |
| Browser and fonts | Chromium/Chrome, DejaVu, Noto, CJK, emoji |

Cloudflare also retains Swift 6.3.3 and wasm-bindgen CLI. Alpine uses musl; it does
not include a Swift host compiler or Apple SDKs. Use a native Mac Hand for Xcode.
The GPU image retains its patched Mesa Venus driver. Portable VM and Docker
images support CPU rendering; installing Blender does not grant GPU access.

Build a Docker Hand with `pnpm build:hand-docker`. Build a portable VM template:

```sh
docker build -t nanocodex-vm:toolkit crates/experimental/nanocodex-vm/image
bash crates/experimental/nanocodex-vm/image/build-root.sh \
  nanocodex-vm:toolkit /absolute/path/desktop.ext4
```

The default ext4 capacity is 16 GiB. Existing images and retained workspaces are
not replaced; create a new Hand from the rebuilt template. Allow additional host
disk space for image layers, build caches, and per-Hand writable files. For
creative tasks, start with `--memory 4096 --cpus 2` and increase for the workload.
Cloudflare uses `standard-3` (2 vCPU, 8 GiB RAM, 16 GB disk) because its full
filesystem occupies about 10 GiB. CI reserves at least 4 GB for workspace data.
This increases compute capacity and running cost compared with `standard-1`.
See [Cloudflare instance limits](https://developers.cloudflare.com/containers/platform/limits/).
Docker sets `TMPDIR` to the retained workspace's `.tmp` directory so compilers can
execute temporary programs while `/tmp` remains a restricted tmpfs. Temporary
files count against workspace storage and can be removed when their jobs finish.
Network policy is unchanged: Docker Hands default to `--network off`.

Run `nanocodex-check-hand-toolkit /workspace/toolkit-check` inside a Hand to retain
its output. It runs offline and compiles C, Rust, Go, and Node programs; creates
DOCX, XLSX, PPTX, and PDF files; converts DOCX to PDF; plots data; and renders a
short video and a Blender CPU image. With no argument it uses temporary output.
CI also runs it as UID 1000 with a read-only root and no network or capabilities.
Python libraries live in `/opt/hand-python`; `python3` resolves to that environment.
Toolchain launchers in `/usr/local/bin` preserve this setup after VM boot and in
login shells, where OCI environment variables are unavailable or reset.
Use a project venv for additional dependencies. Toolkit installation and smoke
inputs are bundled by SSH setup and included in the template cache identity.

The VM's separate `nanocodex-vm-guest` runtime starts Xvfb and implements capture
and input through Rust `x11rb`. These images do not contain the legacy Go desktop
runtime, Waymote, or grim. Go is available as a development tool.

The filesystem packager uses no privileged mounts. It preserves numeric ownership,
checks required executables and forbidden legacy runtimes, runs read-only `e2fsck`,
and compares executable bytes extracted from ext4. Output includes SHA-256, source
image identity, APK inventory, executable checksums, and filesystem-check sidecars.
The Alpine digest is pinned; repository package updates can change rebuild output.
The guest runtime remains a separate disk and must match the host build.

`upgrade-alpine.sh` remains a limited desktop/OpenGL repair for existing Alpine
VMs. It requires 400 MiB free and installs display packages only; it does not
install this full toolkit. It leaves workspace files intact and never restarts a
VM. Stage the matching guest runtime and restart separately to load a new driver.
