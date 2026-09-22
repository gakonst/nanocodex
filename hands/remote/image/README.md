# Releasing the Linux server Hand image

## Screen performance

The companion publishes WebRTC H.264 by default at 60 Hz. It probes NVIDIA
NVENC using the actual capture dimensions and encoder settings, and falls back
to software H.264 when a GPU, compatible driver, or encoder is unavailable.
`NANOCODEX_VIDEO_ENCODER=software` forces the portable path; `nvenc` requires
hardware and reports an error if the probe fails. Both paths use a half-second
keyframe interval, a one-frame bitrate buffer, immediate packet flushing, and
compositor pacing rather than FFmpeg's additional `-re` clock. RTP timestamps
follow elapsed time so skipped captures do not accumulate playback delay.
The encoder helper lives in the same binary and is included automatically in
AMD64 and ARM64 builds. The image smoke test exercises the software fallback.

Hosts with multiple interfaces can explicitly choose `--interface NAME`,
`--ipv4-only`, and a bounded `--udp-port-min PORT --udp-port-max PORT` range for
their firewall. Defaults preserve all interfaces, both IP families, and OS
ephemeral ports. Interface names, LAN routes, and firewall rules are deployment
configuration; no machine-specific network settings are embedded in the image.

`--frames` remains an explicit compatibility transport. Shared web and Apple
viewers target 30 JPEG requests per second for legacy single-frame publishers,
counting capture/network/decode time toward that budget. Publishers advertising
a frame window retain the bounded pipelined transport; Rust publishers allow
six outstanding frames and pace capture at up to 30 Hz. Rust native and
VM/Docker Hand publishers now also default to continuous 60 Hz H.264/WebRTC
when FFmpeg is available. Linux captures X11 independently of input; macOS uses
AVFoundation screen capture and VideoToolbox. VM video uses bounded streaming
stdout over the existing private guest channel, including offline guests.
Agent observations still return bounded JPEGs. Older guest images and hosts
without a working encoder retain `frames-v1`; that fallback is not 60 fps.
Swift macOS capture already feeds ScreenCaptureKit buffers directly to WebRTC
at up to 60 Hz; paired-device capture retains its existing 30 Hz WDA limit.

On September 16, 2026, the shared companion was verified on an RTX 3080 Ti host
at 1600×900: 59.9 decoded fps, zero dropped frames or packet loss over a 12-second
sample, about 7.3 ms receiver buffering, and 1 ms network RTT. A separate
GPU-free container encoded and decoded all 60 test frames using the automatic
software fallback. These are measured host/viewer results, not a guarantee for
every network or VM. Account authentication and control leases are unchanged.

The shared Rust publisher was also measured that day: native Linux 59.9 fps,
native macOS 60.1 fps, libkrun VM 59.9 fps, and offline Docker with an animated
screen 60.0 fps, each with zero dropped frames over 12 seconds. Linux/guest
samples used 1280×800; the Mac main display was scaled to 1280×534. Receiver
buffering was approximately 6.5–9 ms on Linux/guests and 35.6 ms on the Mac.
These browser measurements do not establish phone or off-LAN performance.


## Image release

The manual `Linux Hand image` workflow builds `hands/remote/image/Dockerfile`
on native AMD64 and ARM64 GitHub runners. Zig runs on the target architecture;
the build does not depend on Rosetta or QEMU. Each image must start a non-root
headless desktop, capture and decode a JPEG, and encode and decode H.264 before
publication. The smoke container has no network and is removed on exit.

Waymote is compiled for the baseline CPU of each architecture. Before registry
login, a bounded user-mode QEMU check also starts it with `qemu64` or `cortex-a53`
CPU features. Native encoder tests alone can pass on a CI runner while producing
a binary that crashes with an illegal instruction on another deployment CPU.

After the workflow is on master, validate both architectures without publishing:

```sh
gh workflow run hand-image.yml --ref master -f publish=false
gh run list --workflow hand-image.yml --limit 1
gh run watch RUN_ID --exit-status
```

To publish a verified image:

```sh
gh workflow run hand-image.yml --ref master -f publish=true
gh run list --workflow hand-image.yml --limit 1
gh run watch RUN_ID --exit-status
gh run download RUN_ID --name hand-image-receipt --dir /tmp/nanocodex-hand-release
```

Publication is restricted to master. Both architecture jobs must succeed before
the manifest is created. Tags include the full source commit, run ID, and attempt;
the receipt records the immutable multi-architecture manifest digest, each child
digest, and the manifest itself. No `latest` tag is used. The workflow uses the
repository's package-write `GITHUB_TOKEN`; it does not deploy Workers or update
their configuration.

The SSH installer pulls without registry credentials. Make the
`gakonst/nanocodex-hand` GitHub package public if its initial publication is
private, then verify anonymous access before configuring the service:

```sh
HAND_IMAGE=$(cat /tmp/nanocodex-hand-release/hand-image.txt)
ANONYMOUS_DOCKER_CONFIG=$(mktemp -d)
printf '%s\n' '{"auths":{"ghcr.io":{}}}' > "$ANONYMOUS_DOCKER_CONFIG/config.json"
docker --config "$ANONYMOUS_DOCKER_CONFIG" manifest inspect "$HAND_IMAGE"
for arch in amd64 arm64; do
  child=$(cat "/tmp/nanocodex-hand-release/digests/digest-$arch.txt")
  docker --config "$ANONYMOUS_DOCKER_CONFIG" pull --platform "linux/$arch" "$child"
done
rm -rf "$ANONYMOUS_DOCKER_CONFIG"
```

Use the architecture-specific child digests for this two-platform check: Docker's
classic image store cannot retain both architectures under one index digest.
The explicit empty registry entry prevents credential-helper fallback during
anonymous verification. Production still uses the combined manifest digest;
Docker selects the server's architecture when it pulls that reference.

On the ARM64 deployment host, also execute the pulled capture binary before
changing the production pin; a successful image download does not prove that
its executable is compatible with that host's CPU:

```sh
arm64_child=$(cat /tmp/nanocodex-hand-release/digests/digest-arm64.txt)
docker run --rm --network none --ulimit core=0:0 --platform linux/arm64 \
  --entrypoint /usr/local/bin/waymote-streamd "$arm64_child" --help
```

Set `NANOCODEX_HAND_IMAGE` in the production `vars` of
`js/managed/wrangler.jsonc` to the exact `ghcr.io/gakonst/nanocodex-hand@sha256:...`
receipt. The managed SSH installer rejects mutable tags. Deploy egress, managed,
then account using the root deployment scripts, or the existing Cloudflare
production workflow. Reverting this variable to a previously verified digest
selects that image for subsequent server setup; existing running hosts retain
their current image until explicitly reconnected.

Cloudflare Sandbox desktops use the separate AMD64 `js/managed/Dockerfile`,
which bundles the desktop with the Sandbox SDK. `NANOCODEX_SANDBOX_DESKTOPS=true`
enables publication from those containers. It is independent of the SSH image
variable. The Cloudflare workflow includes Hand source and image preparation
scripts when deciding whether a container rollout is necessary.

For a local native build, use the host architecture (`arm64` on Apple Silicon):

```sh
docker buildx build --platform linux/arm64 --load \
  --tag nanocodex-server-hand:local --file hands/remote/image/Dockerfile .
bash hands/remote/image/smoke.sh nanocodex-server-hand:local arm64
```

Use the native CI jobs for the combined release. The Cloudflare image's native
cross-compiler is specific to its AMD64 SDK target and is not the server image
release path.

## Current toolkit release

The production pin uses `ghcr.io/gakonst/nanocodex-hand@sha256:cf44994c9ca68dd69962cb52cf8e9af572550763780d57483bfcaa05e2ab337b`.
[Publication run 34641948392](https://github.com/gakonst/nanocodex/actions/runs/34641948392)
built master commit `b1e10a50f9f89fdae9d1ae1b1468fad4d69fd3f0`, passed the
AMD64 and ARM64 toolkit, desktop, codec, and baseline CPU checks, and published
the immutable two-architecture receipt. Anonymous manifest access was verified.
Existing server Hands pick up this image when explicitly reconnected.
