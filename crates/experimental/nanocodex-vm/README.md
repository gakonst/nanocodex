# Nanocodex VM

Retained libkrun workspaces and canonical workspace tools for Nanocodex.

`nanocodex-vm` is an experimental, unpublished, library-first crate. An
application owns one [`VmWorkspace`] for each isolation boundary, gives its
[`tools::VmTools`] to one or more agents, and keeps the VM alive across
sequential turns. The crate does not own agent scheduling, evaluation policy,
payment providers, or secrets.

The normal API is:

- [`VmWorkspaceBuilder`] materializes and launches a retained private
  workspace;
- [`image`] prepares immutable root images;
- [`tools`] stages the companion guest and exposes VM-backed Nanocodex tools;
  and
- [`host`] contains the lower-level libkrun, launch-record, networking, and
  egress types used by specialized applications.

The crate root intentionally re-exports only [`VmWorkspace`],
[`VmWorkspaceBuilder`], and [`VmWorkspaceError`].

## Use VM-backed workspace tools

Build the static Linux companion with `just build-vm-guest`, prepare one
read-only runtime disk, and launch a private copy of an immutable root:

```no_run
use nanocodex_vm::{
    image::{CachePolicy, VmImageBuilder},
    tools::GuestRuntimeDisk,
};

# async fn prepare() -> Result<(), Box<dyn std::error::Error>> {
let runtime = GuestRuntimeDisk::prepare(
    "target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest",
    ".cache/nanocodex/vm",
)?;
let image = VmImageBuilder::new("nanocodex", runtime.path())
    .vmm_args(["vm-run-config", "--config"])
    .firmware_directory(".cache/libkrunfw/libkrunfw")
    .prepare(
        "tasks/project/environment",
        10 * 1024 * 1024 * 1024,
        ".cache/nanocodex/vm",
        CachePolicy::Reuse,
    )
    .await?;
let workspace = image.private_workspace(
    ".nanocodex/sessions/018f/root.ext4",
    "nanocodex",
)?
.vmm_argument("vm-run-config")
.vmm_argument("--config")
.guest_runtime_disk(runtime.path())
.firmware_directory(".cache/libkrunfw/libkrunfw")
.launch()
.await?;

let tools = workspace.tools_builder().build()?;
// Pass `tools` to `Nanocodex::builder(...).tools(tools)`.

drop(tools);
workspace.shutdown().await?;
# Ok(())
# }
```

High-fanout ephemeral attempts use guest OverlayFS instead of copying that
retained workspace shape. [`host::VmConfig::overlay_ext4`] boots the runtime
disk read-only, mounts the prepared task disk read-only as the lower layer,
and sends all mutations to a fresh sparse ext4 upper created by
[`host::create_sparse_overlay_disk`]. Reset is deletion of that upper disk;
the host filesystem needs ordinary sparse-file support, not reflinks, XFS, or
a host OverlayFS mount. Attempts configured for rootfs retention continue to
use standalone private ext4 copies so retained artifacts remain self-contained.

```no_run
use nanocodex_vm::{
    host::{
        EgressLease, Network, VmConfig, create_sparse_overlay_disk,
        overlay_guest_command,
    },
    tools::VmToolSession,
};
use tokio::process::Command;

# async fn launch() -> Result<(), Box<dyn std::error::Error>> {
let upper = ".nanocodex/attempts/018f/upper.ext4";
create_sparse_overlay_disk(upper, 10 * 1024 * 1024 * 1024)?;
let config = VmConfig::overlay_ext4(
    ".cache/nanocodex/vm/runtime.ext4",
    ".cache/nanocodex/vm/prepared-task.ext4",
    upper,
)
.cpus(2)
.memory_mib(1024)
.network(Network::Disabled);
let session = VmToolSession::spawn_configured(
    Command::new("dedicated-vmm-process"),
    config,
    overlay_guest_command("/workspace", ""),
    EgressLease::disabled(),
)
.await?;
session.shutdown().await?;
# std::fs::remove_file(upper)?;
# Ok(())
# }
```

The caller owns upper-disk retention and deletion. Drop all session/tool
capabilities and complete [`tools::VmToolSession::shutdown`] before removing
the disk. Overlay startup creates only the requested workspace; harness- or
application-specific directories remain the caller's responsibility.

[`VmWorkspace::tools`] returns a clone-cheap capability suitable for
`NanocodexBuilder::tools_factory`. Every clone routes to the same retained
guest runtime, filesystem, and interactive shell sessions. The non-cloneable
workspace owner is the graceful-shutdown capability; drop agents, registries,
and cloned tool handles before calling [`VmWorkspace::shutdown`].

The default tool selection keeps web search, image generation, and
`update_plan` on the host. It replaces only `exec_command`, `write_stdin`,
`apply_patch`, and `view_image`, preserving their standard model-visible names
and schemas.

### Session control and cleanup

Specialized applications that construct a lower-level
[`tools::VmToolSession`] can run trusted setup and harness commands with
[`tools::VmCommand`]. Commands have explicit time and combined-output bounds.
Dropping an in-flight command request queues cancellation; the guest terminates
the command's process group on cancellation, timeout, output overflow, or
session shutdown.

[`tools::VmCommand::mirror_output`] additionally truncates two harness-owned
guest files before launch and updates them as stdout and stderr arrive. This is
intended for observing a long-running command from another request. It does not
relax the command's retained-output bound or change its terminal result.

At an agent-lifecycle boundary,
[`tools::VmToolSession::terminate_tool_processes`] cancels processes and
interactive shells owned by the workspace-tool runtime while leaving the VM,
filesystem, and host-control channel alive. It does not claim to kill a process
that deliberately detached from the runtime's managed process group. Call
[`tools::VmToolSession::memory_observation`] for best-effort peak host RSS,
guest memory use, and guest OOM evidence. Missing telemetry is represented by
absent fields so it cannot replace the command or agent failure being
diagnosed.

## Host, VMM, and guest ownership

The retained path has three processes:

```text
embedding application
  ├─ owns VmWorkspace, agent state, tools, policy, and egress leases
  └─ spawns a dedicated VMM process from a mode-0600 launch record
       └─ libkrun starts one Linux guest
            └─ nanocodex-vm-guest serves workspace tools over the console
```

The application process does not call libkrun after starting an async runtime.
Instead, [`host::VmProcessConfig::write_private`] writes a complete private
launch record and the dedicated VMM entry point calls
[`host::VmProcessConfig::run`] synchronously. This process boundary also keeps
the macOS hypervisor entitlement on the smallest executable and prevents guest
environment values from appearing in command-line arguments.

The shipped `nanocodex` binary provides that entry point as the hidden
`vm-run-config` command. A library consumer may provide the same small entry
point in its own executable:

```no_run
use nanocodex_vm::host::VmProcessConfig;

# fn vmm(config_path: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
VmProcessConfig::read(config_path)?.run()?;
# Ok(())
# }
```

On macOS, the VMM executable must be signed with the
`com.apple.security.hypervisor` entitlement. Ad-hoc signing is sufficient for
local development; distribution uses the application's normal signing
identity. `just build-eval-host` uses `nanocodex-vm.entitlements` to produce the
prepared repository build; runtime execution only validates the entitlement.

Linux uses the same Rust API and needs no code signing. Running VMs requires
`/dev/kvm` and `libkrunfw.so.5`. The supported x86_64 static guest build uses a
musl 1.2.3 ABI floor because the pinned libkrun KVM path requires `statx`.

## Root and runtime disks

[`image::VmImageBuilder`] resolves a constrained Dockerfile/OCI build context
into a content-addressed immutable ext4 root. The final OCI/Dockerfile working
directory, process environment, and detected shell are retained with
[`image::PreparedRootDisk`]. Its
[`image::PreparedRootDisk::private_workspace`] method is the normal bridge to
the retained workspace API: it makes a no-clobber reflink or sparse copy and
applies that runtime metadata. Writable roots are session-private.

Build cache identity includes the Dockerfile and deterministic context, base
manifest digests, architecture and disk size, VMM arguments and exact
VMM/guest-runtime/configured-firmware file identities,
CPU/memory/address-family policy, host resolver configuration, network mode,
and the egress lease's non-secret cache scope. Set the firmware directory
explicitly when firmware upgrades must invalidate cache entries; omitted
system firmware is caller-managed stable runtime state. Cached
OCI blobs are SHA-256 checked before their metadata fast path is established.
Cached blobs and ext4 disks are published atomically and made read-only;
changes to their inode, size, modification/change time, or permissions force
validation or rebuilding. The caller-selected cache directory remains trusted
application state rather than a security boundary against the same OS user.

By default, the complete VMM executable is part of build-cache identity. An
application whose small VMM entry point is embedded in a frequently changing
binary may set [`image::VmImageBuilder::vmm_build_cache_identity`] to a stable,
non-secret semantic version. This is an explicit correctness promise: the
caller must change it whenever the VMM's Dockerfile-build behavior changes.
The remaining runtime, firmware, resource, network, resolver, and egress inputs
are still hashed independently. Empty and excessively large identities are
rejected.

Prepared roots retain the configured UID-zero account's supported `bash` or
`sh` shell when that executable exists, then fall back to probing conventional
shell paths. A cache hit revalidates the shell from the immutable disk instead
of trusting metadata written by an older release.

Dockerfile build VMs temporarily install the current usable host resolver and
restore the image's original `/etc/resolv.conf` before a stage disk can be
published. Retained private ext4 workspaces install resolver configuration at
boot instead, so immutable images never retain host-specific DNS. Offline and
gvproxy workspaces do not receive host resolver injection. Directory roots are
host-backed development escape hatches and are not rewritten.

[`tools::GuestRuntimeDisk::prepare`] hashes the exact companion ELF and
atomically publishes a reusable 128 MiB ext4 disk. The runtime disk is mounted
read-only, independently from the writable project root. That keeps the guest
implementation identical across a sweep without mutating every root image.

Directory roots are a lower-level development escape hatch. They must already
contain `/usr/local/bin/nanocodex-vm-guest`, and direct virtiofs access does
not provide the same host mount-namespace isolation as a private ext4 root.

## Host/guest RPC protocol

This section is the complete current wire contract implemented by the host
session and `nanocodex-vm-guest`. The protocol is private implementation
detail: host and guest artifacts are built from the same Nanocodex revision,
and there is currently no version negotiation or cross-version compatibility
promise. Applications use the typed Rust API rather than constructing frames.

### Transport and envelope

The dedicated VMM's standard streams carry the guest's default virtio console:

- host to guest: VMM stdin;
- guest to host: VMM stdout; and
- diagnostics only: VMM stderr.

stdin and stdout are newline-delimited JSON. Each frame is one UTF-8 JSON
object followed by `\n`; readers also accept `\r\n`. The newline is not part of
the 64 MiB frame limit. Binary fields use standard padded base64. There is no
authentication, checksum, compression, streaming sub-frame, or handshake
beyond `ready`, because the transport is a private pipe to the owned VMM
process.

Every frame has the externally tagged envelope:

```json
{"kind":"ready","payload":{"id":0}}
```

`kind` is snake case. Every request carries a host-assigned `u64` `id`, and
exactly one response carries the same ID unless the request is cancelled or
the session fails. Responses may arrive in any order. The host allows at most
63 ordinary requests to await responses; the guest executes at most 64
requests concurrently, leaving capacity for control traffic.

Each ordinary request emits a `vm.tool.rpc` span. Its
`rpc.admission.duration_ns` field measures time waiting for one of those 63
host slots, while `rpc.queue.duration_ns` measures the later wait to enter the
bounded writer channel. `duration_ns` covers the complete RPC.

### Requests and responses

`ready` establishes that the guest runtime is accepting work:

```json
{"kind":"ready","payload":{"id":0}}
{"kind":"ready","payload":{"id":0,"error":null}}
```

`tool` executes one canonical workspace tool:

```json
{"kind":"tool","payload":{"id":1,"tool":"exec_command","input":{"function":{"arguments":{"cmd":"pwd"}}},"context":{"model":"gpt-5.6","session_id":"session-1","call_id":"call-1","output_token_budget":10000}}}
{"kind":"tool","payload":{"id":1,"execution":{"output":"/app\n","success":true,"structured_result":null,"metadata":null,"process_trace":{"exit_code":0,"session_id":null,"original_token_count":null,"output_bytes":5,"wall_time_seconds":0.01}},"error":null}}
```

The normal adapter sends `exec_command`, `write_stdin`, `apply_patch`, or
`view_image`. `input` is exactly one of:

```json
{"function":{"arguments":{"cmd":"pwd"}}}
{"freeform":{"input":"*** Begin Patch\n...\n*** End Patch\n"}}
```

Function `arguments` remain opaque JSON. `context` contains `model`,
`session_id`, `call_id`, and `output_token_budget`; conversation history is
not copied into the guest context. `execution.output` is either a string or
the canonical ordered multimodal array of `input_text`, `input_image`, and
`input_audio` objects. `structured_result` and `metadata` are opaque JSON or
`null`. `process_trace` is `null` or contains `exit_code`, `session_id`,
`original_token_count`, `output_bytes`, and `wall_time_seconds`.

An execution with `"success":false` is a model-visible tool failure. A failure
of the RPC/tool runtime itself instead uses `"execution":null` and a non-null
`"error"`. Exactly one of `execution` and `error` is present.

The remaining control methods have these payloads:

| `kind` | Request payload after `id` | Response payload after `id` |
| --- | --- | --- |
| `write_file` | `path`, base64 `contents`, Unix `mode`, optional `modified_unix_seconds` | `error` |
| `create_directory` | `path`, Unix `mode`, optional `modified_unix_seconds` | `error` |
| `read_file` | `path` | base64 `contents` or `error` |
| `memory` | none | optional `total_kib`, optional `minimum_available_kib`, `oom_kills`, `error` |
| `execute` | `program`, `arguments`, `current_directory`, `environment`, `timeout_millis`, `max_output_bytes`, optional `stdout_mirror`, optional `stderr_mirror` | `exit_code`, base64 `stdout`, base64 `stderr`, `error`, `timed_out`, `output_limit_exceeded` |
| `cancel` | `target_id` | `error` |
| `terminate_tool_processes` | none | `error` |
| `shutdown` | none | `error` |

Concrete examples:

```json
{"kind":"write_file","payload":{"id":2,"path":"/tmp/input","contents":"aGVsbG8K","mode":420}}
{"kind":"write_file","payload":{"id":2,"error":null}}
{"kind":"create_directory","payload":{"id":3,"path":"/tmp/results","mode":493,"modified_unix_seconds":0}}
{"kind":"create_directory","payload":{"id":3,"error":null}}
{"kind":"read_file","payload":{"id":4,"path":"/tmp/results/out.txt"}}
{"kind":"read_file","payload":{"id":4,"contents":"b2sK","error":null}}
{"kind":"execute","payload":{"id":5,"program":"/bin/sh","arguments":["-lc","printf ok"],"current_directory":"/app","environment":[["PATH","/usr/bin:/bin"]],"timeout_millis":60000,"max_output_bytes":8388608}}
{"kind":"execute","payload":{"id":5,"exit_code":0,"stdout":"b2s=","stderr":"","error":null,"timed_out":false,"output_limit_exceeded":false}}
{"kind":"cancel","payload":{"id":6,"target_id":5}}
{"kind":"cancel","payload":{"id":6,"error":null}}
{"kind":"memory","payload":{"id":7}}
{"kind":"memory","payload":{"id":7,"total_kib":786432,"minimum_available_kib":524288,"oom_kills":0,"error":null}}
{"kind":"terminate_tool_processes","payload":{"id":8}}
{"kind":"terminate_tool_processes","payload":{"id":8,"error":null}}
{"kind":"shutdown","payload":{"id":9}}
{"kind":"shutdown","payload":{"id":9,"error":null}}
```

`write_file` creates parents and publishes through a sibling temporary file
plus rename. `read_file` accepts only regular files and caps contents at
32 MiB. `execute` clears the inherited environment, uses only the supplied
pairs, captures combined output up to the requested bound, and kills the
process group on timeout, output overflow, cancellation, or shutdown. Optional
mirror paths receive the same stdout and stderr incrementally but do not alter
that bound. `execute` is a bounded one-response operation rather than a
streaming terminal; retained interactive shells use the
`exec_command`/`write_stdin` tool protocol. `memory` reports the guest's
minimum observed `MemAvailable` and OOM-kill counter over the session.

Dropping a host request removes its pending response and queues a `cancel` with
a fresh ID. The cancellation queue is bounded by the same 63 admission
permits, and a permit is retained until the original request and its
cancellation have both been written in that order. Cancelling an unknown or
already completed target is successful. The host does not wait for this
automatically generated cancel acknowledgement. `shutdown` stops acceptance,
cancels active tool work and shell process groups, gives `/bin/sync` a
five-second deadline, replies, and exits.

### Protocol failure

The session fails closed on malformed JSON, an unknown request/response kind,
an unknown field in a strict request payload, a frame larger than 64 MiB, a
partial frame at EOF, or reuse of an ID that is still active. Clean host EOF
cancels active work and exits the guest. A tool response that is too large is
replaced with a scoped tool RPC error when that fallback fits; an oversized
non-tool response terminates the session. A failed partial response is never
turned into a successful tool result.

## Egress and lifecycle

[`host::EgressLease`] is the provider-neutral output of application policy. It
combines network mode, guest environment, read-only mounts, public guest files,
and host-side guards that must live as long as the VM. The VM crate never
resolves secrets or chooses a payment provider. Conflicting environment or
mount claims fail closed.

The built-in internet and disabled leases have stable build-cache scopes.
Adding provider environment, mounts, or files clears that scope. An application
using the resulting lease for Dockerfile builds must assign a non-secret
identity with [`host::EgressLease::set_build_cache_scope`] after composition;
otherwise image preparation fails rather than reusing output built through a
different route or credential policy.

The default [`host::Gvproxy`] topology exposes host loopback to the guest at
[`host::Gvproxy::HOST_IPV4`]. If an owned gvproxy exits before cleanup, its
status is appended to the caller-selected gvproxy log and emitted through
tracing; ordinary owner drop still terminates and reaps a live child.

The last workspace/tool capability kills the VMM child. Workspace startup has
a 30-second default deadline covering readiness and egress provisioning;
graceful shutdown has a 10-second default covering guest acknowledgement and
VMM exit. Both are configurable on [`VmWorkspaceBuilder`]. Explicit shutdown
atomically rejects live sibling capabilities and owner-borrowed requests.
Cancelling the shutdown future force-terminates and reaps the child instead of
leaving an unreachable VM. Timeouts and request cancellation terminate process
groups and descendants.

## Cargo features

The default `host` feature contains image preparation, libkrun lifecycle, and
VM-backed tool clients on Linux and macOS. `guest-runtime` contains only the
companion server and the canonical `nanocodex-tools` workspace runtime. The
split exists to produce a small static Linux guest ELF; it is not a second
public execution model. Normal native `nanocodex-tools` and
`nanocodex-oai-api` builds retain their complete default behavior. CI checks
the guest-only all-target matrix and builds the actual x86_64 musl guest
artifact independently from the host feature.

See `docs/VM.md` in the repository for CLI operation, egress composition, and
build commands.
