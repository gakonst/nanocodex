# VM-backed tools and egress

Nanocodex keeps the agent lifecycle independent from where workspace tools
execute. The default `Tools` selection runs `exec_command`, `write_stdin`,
`apply_patch`, and `view_image` in the embedding process. Applications can
replace those handlers with one persistent libkrun VM without changing their
model-visible names or schemas.

The unpublished `nanocodex-vm` package owns this boundary through its
high-level retained workspace API and focused `host`, `image`, and `tools`
modules: typed libkrun configuration and its small audited FFI surface, private
VMM process configuration, gvproxy and neutral egress leases, OCI/Dockerfile
image preparation and reflinks, and the retained host/guest tool protocol.

The package does not own payment-provider policy, agent identity, secret
resolution, or the caller's choice to enable VM tools.

The crate's canonical public documentation is
[`crates/experimental/nanocodex-vm/README.md`](../crates/experimental/nanocodex-vm/README.md).
Its
[host/guest RPC section](../crates/experimental/nanocodex-vm/README.md#hostguest-rpc-protocol)
specifies every frame, limit, cancellation rule, and terminal failure in the
private lockstep protocol.

The Cargo split stays internal to this boundary. The default `host` feature
retains image preparation, libkrun lifecycle, and VM-backed `Tools`;
`guest-runtime` links only the canonical apply-patch, shell, write-stdin, and
view-image handlers. The guest deliberately excludes the OpenAI transport,
Code Mode/QuickJS, MCP, HTTP clients, and OCI preparation. Normal
`nanocodex-tools` and `nanocodex-oai-api` builds retain their complete native
behavior through default features. CI checks the guest-only all-target matrix
and separately builds the actual x86_64 musl companion.

The checked-in x86_64 musl target configuration sets the supported ABI floor
to musl 1.2.3 because the pinned libkrun KVM path requires `statx`. Building
the companion on Linux therefore needs `musl-tools` and produces a static PIE.
The host application remains a normal native build; VM execution still
requires `/dev/kvm` and `libkrunfw.so.5` at runtime.

## Preparing immutable images

`VmImageBuilder` turns a directory containing a concrete `Dockerfile` into one
validated immutable disk. The cache key covers the Dockerfile, deterministic
context archive, target architecture, base manifest digests, disk size, VMM
arguments and exact VMM/guest-runtime/configured-firmware files, VM resources
and address-family policy, resolver configuration, network mode, and a
non-secret egress-policy identity. Configure the firmware directory explicitly
when firmware upgrades must invalidate cache entries; omitted system firmware
is caller-managed stable runtime state. The caller-selected cache directory is
trusted application state, not a security boundary against the same OS user.
`PreparedRootDisk` retains the final container working directory, environment,
and shell, then applies them while making each mutable VM's no-clobber reflink
or sparse copy:

```rust,no_run
use nanocodex_vm::{
    image::{CachePolicy, VmImageBuilder},
    tools::GuestRuntimeDisk,
};

# async fn prepare() -> Result<(), Box<dyn std::error::Error>> {
let runtime = GuestRuntimeDisk::prepare(
    "target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest",
    ".cache/vm",
)?;
let images = VmImageBuilder::new(
    "target/debug/vm-tools",
    runtime.path(),
)
.firmware_directory(".cache/libkrunfw/libkrunfw")
.vmm_arg("--vmm");
let image = images
    .prepare(
        "tasks/project/environment",
        10 * 1024 * 1024 * 1024,
        ".cache/vm",
        CachePolicy::Reuse,
    )
    .await?;
let workspace = image
    .private_workspace(
        ".nanocodex/attempts/018f/rootfs.ext4",
        "target/debug/vm-tools",
    )?
    .vmm_argument("--vmm")
    .guest_runtime_disk(runtime.path())
    .firmware_directory(".cache/libkrunfw/libkrunfw")
    .launch()
    .await?;
# workspace.shutdown().await?;
# Ok(())
# }
```

`GuestRuntimeDisk::prepare` hashes the exact guest ELF, validates an existing
cache entry when present, and otherwise formats and atomically publishes one
read-only 128 MiB ext4 runtime disk. Same-key processes
single-flight on a filesystem lock. The returned path remains in the
caller-selected cache after the value is dropped.

Dockerfile build VMs default to 2 vCPUs, 4096 MiB, ordinary internet egress, a
30-minute `RUN` timeout, and a 10-minute mount/`COPY` timeout.
`VmImageBuilder::cpus`, `memory_mib`, `egress`, `run_timeout`, and
`copy_timeout` make each policy explicit when those defaults are unsuitable.

OCI references and layers resolve concurrently, with at most eight operations
at either boundary. Same-key work single-flights across tasks and processes,
while unrelated images remain parallel. Cache records and disks publish
atomically and are made read-only. OCI blobs are SHA-256 checked before their
metadata fast path is established; changes to cached file identity or
permissions force validation or rebuilding. A valid warm disk hit never
launches a VM or decodes layer contents.

Build guests temporarily install the current usable host resolver and restore
the image's original `/etc/resolv.conf` before publishing a stage. A retained
private ext4 workspace installs resolver configuration only at boot, so an
immutable image never captures host-specific DNS. Offline and gvproxy
workspaces skip that injection; host-backed directory roots are left alone.

## Selecting host or VM tools

Host execution remains the default:

```rust,no_run
# use nanocodex::{Nanocodex, OpenAiAuth};
# fn build(auth: OpenAiAuth) -> nanocodex::Result<()> {
let (agent, events) = Nanocodex::builder(auth).build()?;
# drop((agent, events));
# Ok(())
# }
```

The shipped CLI exposes the same opt-in boundary in the normal TUI, one-shot
runner, and resumed TUI:

```sh
nanocodex \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
nanocodex run "make the requested change" \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
nanocodex resume <thread-id> \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
```

`--vm` accepts either a raw ext4 root or a directory rootfs, retains one VM
across sequential turns and the root agent tree, and modifies that root in
place. Raw ext4 roots require the guest ELF through `--vm-guest-runtime` or
`NANOCODEX_VM_GUEST_RUNTIME`; the CLI packs it into a content-addressed
read-only block device. Directory roots must already contain
`/usr/local/bin/nanocodex-vm-guest`. Use `--vm-no-network` for an offline guest.
The provided root must be session-private; the CLI takes an exclusive advisory
lock on raw disks to reject accidental concurrent attachment.

A caller normally materializes one private raw-ext4 root and starts a retained
workspace through the high-level API. Bootstrap shell, runtime block-device
mounts, private process configuration, and guest readiness stay inside
`nanocodex-vm`:

```rust,no_run
# use nanocodex::{Nanocodex, OpenAiAuth};
# use nanocodex_vm::image::PreparedRootDisk;
# async fn build(
#     auth: OpenAiAuth,
#     image: &PreparedRootDisk,
# ) -> Result<(), Box<dyn std::error::Error>> {
let workspace = image.private_workspace(
    ".nanocodex/sessions/018f/root.ext4",
    "nanocodex-vmm",
)?
.guest_runtime_disk(".cache/nanocodex/runtime.ext4")
.firmware_directory(".cache/libkrunfw/libkrunfw")
.launch()
.await?;
let tools = workspace.tools_builder().build()?;
let (agent, events) = Nanocodex::builder(auth)
    .workspace(workspace.guest_workspace())
    .tools(tools)
    .build()?;
# drop((agent, events));
workspace.shutdown().await?;
# Ok(())
# }
```

`VmTools::tools_builder` replaces only workspace-effecting tools. Web search,
image generation, and `update_plan` retain their existing host-side behavior.
Callers can disable or replace those independently.

Use `NanocodexBuilder::tools_factory` when an agent can spawn or fork. Start one
`VmWorkspace` for the root agent tree and capture its clone-cheap `VmTools` in
the factory. Nanocodex invokes the factory once per driver, so agent-relative
tools are freshly bound to that driver while every driver deliberately shares
the same VM, filesystem, and retained guest shell sessions:

```rust,no_run
# use nanocodex::{Nanocodex, OpenAiAuth};
# use nanocodex_vm::tools::VmToolSession;
# fn build(auth: OpenAiAuth, session: VmToolSession) -> nanocodex::Result<()> {
let vm = session.tools();
let (agent, events) = Nanocodex::builder(auth)
    .workspace("/workspace")
    .tools_factory(move |_agent| {
        vm.tools_builder()
            .working_directory("/workspace")
            .default_shell("sh")
            .build()
    })
    .build()?;
# drop((agent, events));
# Ok(())
# }
```

The `VmToolSession` is the non-cloneable graceful-shutdown capability.
`VmTools` and `VmToolSessionHandle` are cloneable capabilities. Every one of
them keeps the VMM, private launch configuration, and egress guards alive, so
capturing `VmTools` in a driver factory is sufficient for the complete agent
tree. Graceful shutdown fails while sibling capabilities remain; drop the
agents, tool registries, and cloned handles before calling it.

## Configurable egress

`EgressLease` is the VM-facing output of application policy. A layer may
contribute:

- a network mode;
- guest environment such as an authenticated `HTTP_PROXY`/`HTTPS_PROXY`;
- read-only provider directories and public guest configuration files; and
- lifecycle guards that keep revocable host services alive for the VM.

Independent provider layers compose transactionally:

```rust,no_run
use nanocodex_vm::host::{EgressFile, EgressLease, EgressMount};
use std::sync::Arc;

# fn configure() -> Result<EgressLease, nanocodex_vm::host::EgressError> {
let mut payment_proxy = EgressLease::internet();
payment_proxy.insert_environment(
    "HTTPS_PROXY",
    "http://mpp-lease:credential@host.internal:8080",
)?;
payment_proxy.insert_file(EgressFile::new(
    "/tmp/nanocodex/egress/mpp/ca.pem",
    b"public CA bytes".to_vec(),
    0o444,
))?;

let mut secrets = EgressLease::internet();
secrets.insert_environment(
    "NANOCENTAUR_SECRET_BASE_URL",
    "https://secret-gateway.internal/v1",
)?;
secrets.insert_mount(EgressMount::read_only(
    "secret-ca",
    "/host/secret-ca",
    "/tmp/nanocodex/egress/secrets/ca",
))?;
secrets.retain(Arc::new(())); // the real layer retains its proxy lease

let mut egress = EgressLease::internet()
    .with_layer(payment_proxy)?
    .with_layer(secrets)?;
egress.set_build_cache_scope("mpp-and-secret-route-v1")?;
Ok(egress)
# }
```

The built-in internet and disabled leases have stable cache scopes. Adding
provider environment, mounts, or files clears the scope. Set a non-secret
identity only after complete composition when the lease will be used by
`VmImageBuilder`: Dockerfile build preparation rejects an unscoped provider
lease rather than reusing output from a different route or credential policy.
Runtime-only `VmWorkspaceBuilder::egress` does not require a cache scope.

`VmToolSession::spawn_configured` consumes the complete lease and applies it to
both launch configuration and retained session state. This selects the network,
attaches provider directories read-only, mounts them before the guest runtime
starts, injects only the resolved guest environment, provisions public files,
and retains every provider guard:

```rust,no_run
# use nanocodex_vm::{
#     host::{EgressLease, GuestCommand, VmConfig},
#     tools::VmToolSession,
# };
# use tokio::process::Command;
# async fn launch(egress: EgressLease) -> Result<VmToolSession, Box<dyn std::error::Error>> {
let guest = GuestCommand::new("/usr/local/bin/nanocodex-vm-guest").arg("/workspace");
let mut vmm = Command::new("dedicated-vmm-process");
vmm.arg("--run-config");
let session = VmToolSession::spawn_configured(
    vmm,
    VmConfig::ext4("private-session-rootfs.ext4"),
    guest,
    egress,
).await?;
# Ok(session)
# }
```

The method serializes complete launch configuration to a mode-`0600` temporary
file and retains it until the last VM capability is dropped. This keeps bearer
proxy URLs and secret-route placeholders out of the VMM command line and avoids
a process-start race. Lower-level `configure`, `write_private`, `spawn`, and
`provision_egress` operations remain available for specialized launchers, but
the application must then preserve the same ownership ordering itself.

### Application-owned payment and secret proxy layers

Payment providers remain host-owned HTTP(S) proxies. An application-owned
adapter can point the guest at a proxy, provision its public interception CA,
and retain the wallet/proxy guard in a provider-neutral `EgressLease`.
`nanocodex_vm` deliberately has no payment-provider integration;
Tempo-specific payment policy stays under `bin/`.

NanoCentaur's Iron/secret egress follows the same contract: its layer carries
the scoped proxy or gateway route, public CA/configuration files, placeholders,
and revocable lease guard. Resolved secrets remain host-side and must never be
placed in an `EgressFile`.

A guest process can have only one value for `HTTPS_PROXY`. If two independently
started providers both claim the front-proxy variables, lease composition
fails closed. An application that needs both proxies on one request path must
chain or route them host-side and expose one front proxy to the guest. The VM
package deliberately does not guess proxy order or silently overwrite one
provider's credentials.

## Lifecycle and security

- One VM tool session is shared by the complete root-agent tree and retains
  interactive guest processes across sequential turns and subagent calls.
- Concurrent drivers are multiplexed by request ID; one slow guest command
  does not block unrelated subagent calls. Dropping an individual host request
  sends a targeted cancellation frame through a bounded queue while retaining
  its admission permit until the request and cancellation are physically
  written in order. The guest aborts that request's process group without
  disturbing sibling work.
- The last session/tool capability kills its VMM child and releases egress.
  Startup defaults to a 30-second deadline for readiness plus provisioning.
  Explicit shutdown atomically rejects live sibling capabilities and
  owner-borrowed requests, gives guest sync and VMM exit a 10-second default
  deadline, and forcibly terminates the child if the shutdown future itself is
  cancelled. Both deadlines are configurable on `VmWorkspaceBuilder`.
- Protocol frames are limited to 64 MiB and carry binary fields as base64
  strings rather than allocation-heavy JSON byte arrays. Host-control file reads
  accept only regular files and are limited to 32 MiB; trusted command output
  defaults to 8 MiB. Command timeouts, request cancellation, output-limit
  cancellation, guest shutdown, and capability drop terminate process groups,
  including descendants.
- Egress files are limited to 4 MiB. Mounts and files must be non-overlapping
  descendants of `/tmp/nanocodex/egress`; mount tags and modes are validated
  before launch.
- A writable ext4 root is session-private. Reflink or sparse-copy an immutable
  base image for each VM rather than attaching a shared benchmark image
  directly.
- On macOS, the dedicated VMM executable must carry the
  `com.apple.security.hypervisor` entitlement. `just build-vm-example` builds
  and ad-hoc signs the public proof binary with `nanocodex-vm.entitlements`.
- Failed partial protocol responses are not converted into successful tool
  results.
- Egress values are omitted from `Debug`; only environment names, mount
  metadata, and guard counts are shown.
- Read-only provider mounts and environment conflicts are explicit.
- The libkrun unsafe surface stays inside two audited
  `nanocodex_vm` modules; the rest of Nanocodex remains safe Rust.
- The companion guest reuses the canonical `nanocodex-tools` request/result
  contracts and workspace-tool implementations. Cross-compiling the companion
  guest target does not create an alternate tool runtime or change MCP
  availability in normal native builds.

See
`cargo run -p nanocodex-examples --bin vm-tools -- ROOTFS GUEST_RUNTIME_BINARY`
for the end-to-end tool protocol example. Build the lean guest artifact with
`just build-vm-guest`; the example stages that ELF through
`GuestRuntimeDisk::prepare` and mounts the resulting disk read-only.
If the runtime argument is omitted, the rootfs must already contain
`/usr/local/bin/nanocodex-vm-guest`.

Keep VM regression budgets in executable benchmark thresholds, not dated
baseline narratives.
