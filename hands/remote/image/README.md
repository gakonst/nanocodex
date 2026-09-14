# Releasing the Linux server Hand image

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
