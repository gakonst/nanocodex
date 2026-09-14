# Apple Silicon GPU Hands

The Mac shares its GPU with Linux Hands through **Mesa Venus → virtio-gpu →
virglrenderer → MoltenVK → Metal**. This exposes Vulkan compute and an opt-in
OpenGL 2.1 driver with GPU rendering and X11 presentation. It is not
PCI passthrough or a Linux Metal/CUDA device. PR #310 remains the portable
software OpenGL desktop path.

## Build and use

On an Apple Silicon Mac with the normal Rust/Node/Docker build prerequisites:

```sh
brew install slp/krun/virglrenderer
rustup target add aarch64-unknown-linux-musl
corepack pnpm build:macos:gpu-hand /absolute/path/to/new-assets /path/to/libkrunfw
```

The firmware directory must contain `libkrunfw.5.dylib`. The command builds
matching host and guest binaries, bundles the three native GPU libraries
and their licenses, signs the host for Hypervisor, and prepares a clean
16 GiB sparse ext4 template plus `vm.json`. It refuses to overwrite an
existing output directory. Put the generated recipe at the desktop runtime's
`Native/vm.json` to use these assets for new Hands. Keep the asset directory
at the path used when generating the recipe.

The desktop's GPU option follows the host recipe for newly created Hands.
Existing Hands retain their own setting and private disks. The CLI accepts
`--vm-gpu` on both `hand` and `host`; pass the generated template, runtime,
and firmware paths. Rust callers select `Gpu::Vulkan` on `VmConfig` or
`VmWorkspaceBuilder`. Software-only builds reject GPU requests explicitly.

Inside a GPU Hand:

```sh
nanocodex-gpu-check       # real compute dispatch and checked GPU readback
vulkaninfo --summary     # supported Vulkan API and physical renderer
nanocodex-gpu-gl nanocodex-gpu-gl-check  # rendered pixels, X11 pixels, resizes
nanocodex-gpu-gl glxgears # when mesa-demos is installed
```

Vulkan applications use the installed Venus driver directly. Use
`nanocodex-gpu-gl PROGRAM [ARGUMENTS...]` for compatible OpenGL applications.
The launcher reads the Hand desktop's display/authentication paths and selects
the separate hardware driver. Ordinary OpenGL applications retain the software
driver, which supports newer GL features. No capabilities or version numbers
are overridden to make unsupported applications pass their checks.

## Readiness and lifecycle

The Rust VM session checks a requested GPU before returning a usable session.
The check rejects CPU devices, creates a compute pipeline, dispatches 65,536
integer calculations, waits for completion, and validates every returned
value. A missing driver, failed device, incorrect result, or timeout fails
startup. CLI/JS/Swift do not perform their own GPU detection or retry policy.

The CLI factory also boots a disposable private clone to validate its GPU
recipe before connecting to the managed allocation service. A broken image
therefore fails once at startup instead of repeatedly accepting allocations
that can never become ready. Every subsequent allocation and resumed VM
passes the same Rust session check. The existing VM owner handles teardown
and retained disks; GPU execution state itself does not survive a VM restart.

## Driver versions and limits

- Host: libkrun at the Cargo lockfile revision, optional `nanocodex-vm/gpu`
  feature; virglrenderer from `slp/krun`, MoltenVK, and libepoxy. Bundled dylib
  references are relative to the executable, not an installed Homebrew path.
- MoltenVK 1.4.2 is built from pinned source with SPIRV-Cross revision
  `cd3fcb2603ede297edb90ab5a679e4ac814055e2`. Its
  [upstream fix](https://github.com/KhronosGroup/SPIRV-Cross/commit/cd3fcb2603ede297edb90ab5a679e4ac814055e2)
  prevents resource names such as `sampler` from shadowing Metal types.
  Without it, a generated fragment shader fails compilation and guest pixel
  readback aborts while waiting for the failed work. The root build command
  builds and caches this dependency, checking its artifact hashes on reuse.
- Guest: Alpine 3.24 and Mesa Venus 26.1.6. The source archive is checked
  against its published hash. Compilation stays in Docker build stages;
  no compiler or source tree enters the template.
- The supplied libkrunfw 6.12 kernel uses 4 KiB guest pages, while Apple's
  Hypervisor requires 16 KiB mappings. The image applies the libkrun
  maintainer's [alignment patch](https://gitlab.freedesktop.org/slp/mesa/-/commit/761ef1ec5ff2aae1cc3dc8bbc22b3d06ef04b549),
  amended to copy the allocation description instead of modifying a const
  caller-owned structure. Track [krunkit #114](https://github.com/libkrun/krunkit/issues/114)
  for negotiated alignment support that can remove this downstream patch.
- On the tested **M1 Max**, Venus exposes Vulkan 1.2. A normal Vulkan
  swapchain is unavailable. Applications must check their required features.
- Hardware OpenGL uses separate Mesa Zink 25.1.9 libraries. Newer Zink
  requires `nullDescriptor`, which MoltenVK does not expose. Zink's normal
  presentation function also skips windows without a Vulkan swapchain.
  The small `gpu/zink-xvfb.patch` routes those windows through the supplied
  software window-system loader: map the GPU-rendered image, present its
  existing bytes, then unmap. It adds no software rendering and no retained
  copy of the framebuffer. GPU swapchain presentation is unchanged.
- The tested hardware GL profile is **2.1**. MoltenVK still lacks Zink
  features including logic operations and custom border colors. Applications
  depending on them can render incorrectly; this is not a conformant general
  purpose OpenGL replacement.
- Modern Blender's GPU viewport/Cycles rendering is not enabled in these
  Linux VMs. Working Vulkan compute does not create a Metal backend there.
  Blender on a native Mac Hand can use the Mac's Metal backend.
  An actual Blender 5.1.2 launch rejected the Venus device for missing
  geometry shaders, logical operations, `VK_KHR_swapchain`, and
  `VK_EXT_provoking_vertex`; the graphics launcher does not override those
  checks or advertise unsupported capabilities.

## Hardware evidence (2026-09-11)

Tests use real `nanocodex2` binaries and the managed service:

- The clean graphics bundle (pinned MoltenVK/SPIRV-Cross and patched Zink)
  passed five compute and five graphics checks in each of two separate
  factory VMs. Each graphics run checked GPU readback and actual X11
  presentation at 256×256, 127×91, 381×219, and 32×32. The launcher found
  each VM's authenticated desktop without manually supplied display settings.
  Both retained VMs then passed compute and all four presentation sizes again
  after a complete factory stop/restart, with their original marker files intact.
- A visible `glxgears` window displayed colored gears through Zink/Venus on
  the M1 Max; before the presentation fix the same window remained black.
- `vulkaninfo`: `Virtio-GPU Venus (Apple M1 Max)`, integrated GPU, vendor
  `0x106b`, Vulkan 1.2, Mesa Venus 26.1.6.
- Compute shader `out[i] = i * 3 + 7`: 65,536 values, zero mismatches.
- Two clean factory allocations each completed five checked compute runs,
  then 15 overlapping runs each (16:18:14–16:18:30 UTC), all successful.
- After stopping and restarting the factory with the final Vulkan-only
  bundle, both existing mounts preserved their markers and passed compute
  again; no replacement mounts were created.
- A broken image missing its Vulkan loader was rejected during factory
  preflight, before the host connected or advertised allocation capacity.
- Without a GPU, the packaged probe exits unsuccessfully rather than
  accepting a software renderer.
- The rebuilt, developer-signed Mac app created `gpu-smoke-final` through
  its native Hand's `start_vm` tool using the installed GPU recipe. Startup
  passed the Rust compute/readback gate; `stop_vm` returned stopped and
  retained its private disk. A separate command invocation in that test
  session was blocked by VM tool discovery, so repeated command execution
  evidence comes from the factory tests above.

The existing Ubuntu box was not used as accelerated capacity: its inspected
AMD device had no bound graphics driver or render node. No host driver
rebind, kernel change, or GPU rental was needed for the Mac path.
