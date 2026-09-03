# Distributed execution namespace

Status: contract plus cwd-routing milestone implemented locally; not deployed.
Native cross-mount filesystem access (C2/C5) remains gated. This supersedes the
proposed `environment` argument on execution tools.

## Core contract

Nanocodex presents every authorized hand as a mount in one private filesystem
namespace:

```text
/
├── brain/        durable brain scratch
├── mnt-test-a1b2c3d4/  explicitly mounted sandbox hand
├── laptop/       connected user hand
├── buildbox/     connected user hand
└── .nanocodex/   synthetic namespace metadata
```

- **C0 — Provisioning:** an agent begins with no sandbox hand. The generic
  `mount` capability selects a provider, provisions a named hand, and returns
  its logical root. A model should infer this need from work such as building or
  testing code; the user does not have to request a mount explicitly.
- **C1 — Placement:** the mount root containing a command's effective cwd
  selects the hand that executes it.
- **C2 — Access:** every process sees the same authorized mounts. Discovery,
  read, and write rights are independent of placement.
- **C3 — Stable tools:** `exec_command` and `write_stdin` keep their canonical
  shapes; no environment, machine, host, or sandbox selector is added.
- **C4 — Stable context:** a Code Mode cell has an immutable namespace and
  default-cwd snapshot. Each command captures its cwd when scheduled.
- **C5 — Native namespace:** arbitrary native children access mounts through
  normal filesystem syscalls. Rewriting `workdir`, parsing shell text, or
  copying/syncing trees is not conforming.
- **C6 — Subagent inheritance:** each child gets a stable, capability-bounded
  namespace and independently uses C1–C5, including concurrent use of hands.

Thus both forms below execute on `laptop`, while Cargo and its children may use
other authorized mounts:

```js
await tools.exec_command({ cmd: "cargo test", workdir: "/laptop/repo" });
await tools.exec_command({ cmd: "cd /laptop/repo && cargo test" });
```

## Design basis and naming

The design takes Plan 9's central idea: file servers expose trees, a protocol
carries local or remote operations, and each process gets a private namespace
composed from those trees. A 9P2000.L-like operation model and
capability-secured mounts are useful foundations, but Plan 9's exact wire,
authentication, caching, and Unix-compatibility choices are not the contract.
FUSE, v9fs, virtio-fs, or an in-process VFS may be platform adapters.

A **hand** exports files, executes processes, or both. An **export** is an
explicitly configured physical directory, never an implicit OS root. A stable
logical **mount root** such as `/laptop` names it; the mount root containing the
cwd is the **execution root**.

The broker creates an immutable cell manifest mapping each mount root to opaque
hand and export identities, attachment generation, lease, and rights. Mount
names are portable, unique, non-overlapping, and distinct from reserved roots
such as `brain`, `sandbox`, `.nanocodex`, `dev`, `proc`, and `tmp`. Names and
paths locate resources but never authorize them. `brain` is a broker-owned
system root; provider mounts receive broker-issued roots and user-assigned
roots cannot reuse any reserved name.
`/laptop/a` means `a` beneath
the configured export, not OS path `/a` unless the OS root was deliberately
exported.

A cell's name-to-identity mapping never changes. New connections appear only in
a later cell; revocation may invalidate existing authority immediately.

## Execution semantics

The canonical tool schemas remain authoritative.

- `workdir` sets the initial cwd; relative paths resolve against the cell's
  captured default cwd. The managed default is non-executable `/brain`, so the
  model must use a returned or discovered hand root. Shell `cd` is applied
  before external execution.
- The longest component-boundary mount match of the effective cwd selects the
  hand. Executable lookup, environment, architecture, and children belong to
  that hand.
- An external command started in a synthetic directory without an executor
  fails; there is no default-hand fallback. `/brain` is storage unless it is
  explicitly given a process executor.
- Redirections use the namespace, and a pipeline may span hands.
- Each parallel call captures cwd independently. Commands on different hands
  run concurrently without a namespace-wide lock.
- `write_stdin` uses a public session ID durably bound to the exact hand,
  process, attachment generation, manifest, agent/turn, and output cursor.
  Reconnect cannot retarget it.
- Other placement-sensitive operations either resolve a canonical workdir or
  bind to a process/session. There is no ambient "last selected hand."

```js
await Promise.all([
  tools.exec_command({ cmd: "cargo test", workdir: "/laptop/repo" }),
  tools.exec_command({ cmd: "cargo test", workdir: "/buildbox/repo" }),
]);
```

## Subagents

The canonical task tree and its tools, including `spawn_agent`, keep their
schemas and authority rules. Spawning creates a model session, not a process on
a hand, and adds no hand selector.

At admission the runtime atomically binds the child to the invoking cell's
manifest and generations, the parent's default logical cwd, necessary leases,
task-tree/session/cancellation identities, and an authority ceiling no broader
than the parent's effective rights. Exact inheritance is the compatibility
default. Trusted host policy may attenuate mounts, subtrees, or write rights;
grandchildren may only attenuate further. If the resulting scope cannot access
the inherited cwd, spawn fails atomically. Roles, tasks, messages, paths, and
mount names cannot grant authority.

A child is not pinned to a hand. Every command is placed from that command's
effective cwd, so one child may use several hands and siblings may fan out
concurrently. Task text can suggest a cwd; ordinary `exec_command({workdir})` or
shell `cd` performs placement under the server-bound authority.

Parents and children share exported files subject to individual rights and
filesystem consistency. Model sessions, conversation state, Code Mode stores,
output cursors, and cancellation controllers remain private. Concurrent
writers need disjoint worktrees/subtrees, a trusted write-scope policy, or
ordinary filesystem locking; descriptive task text is not a write lease.

Model capacity and hand capacity are independent. `max-subagents` bounds model
turns; each hand separately bounds processes, sessions, filesystem requests,
and bytes in flight. Model/thinking selection affects inference only, never
execution placement.

## Filesystem and capabilities

Every full-capability execution hand receives an actual private root or mount
namespace. Its own export should be a local mount; peer exports are remote
mounts; immutable metadata is a synthetic overlay. Keeping the local root local
prevents ordinary builds from becoming filesystem RPC workloads.

The host-generic interface must cover ordinary development tools: lookup,
open/close, stat and attributes, directory enumeration, offset I/O, create,
truncate, mkdir, remove, rename, links, fsync, advisory locks, stable identity,
version tokens, and cancellation. It must stream with bounded memory,
backpressure, pagination, concurrent tagged requests, and per-mount isolation.

- Owner-acknowledged writes become visible.
- Same-export rename is atomic when supported; cross-mount rename and hard-link
  return `EXDEV`.
- Ambiguous mutations use operation IDs and durable receipts, never blind
  replay.
- Disconnect or stale generation returns bounded `ESTALE`, `EIO`, or
  unavailable failure, never fallback.
- Optional caches validate stable identity/version and do not silently weaken
  consistency.

An export server resolves beneath an already-open directory handle and prevents
traversal or symlink escape. Absolute symlinks resolve in the client's logical
namespace. A hand exports only its physical tree, never peer mounts, preventing
recursive mount cycles.

Initial server-issued capability classes cover namespace discovery, filesystem
read/write, process execution/stdin, and network preview. They are projected as
immutable discovery hints such as:

```text
/laptop/.nanocodex/status
/laptop/.nanocodex/capabilities/<capability>
```

Every operation still rechecks an unforgeable grant, lease, and attachment
generation at the authoritative server. Effective file access is the
intersection of namespace rights and exporter filesystem policy. Cross-hand
access is an explicit disclosure grant to the accessing hand, which receives
filesystem operations, never the exporter's credentials.

The binary, streaming filesystem data plane is separate from bounded JSON
hosted-tool calls, though it may reuse attachment authentication and fencing.
Full native conformance requires mounts visible to arbitrary native children:
Linux may use mount/user namespaces plus FUSE or v9fs; sandboxes and VMs may use
v9fs or virtio-fs; macOS/Windows may need a helper or VM. A browser VFS proves
only interpreted behavior. Platforms without native mount visibility must not
advertise full namespace `process.exec`.

## Lifecycle and security

One lifecycle mechanism governs mounts, handles, processes, previews, and
sessions:

| Event | Required behavior |
| --- | --- |
| Cell starts | Capture authorized mount mapping and default cwd |
| `mount` completes | Retain provider identity and expose the new hand to later namespace snapshots |
| Child spawns | Atomically bind inherited or attenuated namespace ceiling |
| Message/steer/task replacement | Preserve namespace, cwd, and authority |
| Process starts | Pin hand, manifest, rights, lease, and generation |
| Disconnect/reconnect | Fail old work stale; reconnect creates a new generation |
| Revocation | Reject new work and invalidate affected resources |
| Cancel/interrupt/close | Stop native and descendant work; fence late results |
| Ambiguous mutation | Consult its receipt; never retry blindly |
| Cold recovery | Restore interrupted children unless exact live state is owner-fenced |

Security invariants:

1. Names, paths, mode bits, and reported capabilities never grant authority.
2. Normalization and mount selection are one fenced, component-aware operation.
3. Symlinks, links, mounts, junctions, devices, and special files cannot escape
   an export or create cross-mount authority.
4. Provider credentials and host secrets never enter namespaces or processes.
5. Limits cover mounts, handles, requests, bytes, directory entries, duration,
   processes, output, and retained receipts.
6. Audit records exclude contents and credentials.
7. Descendant rights never exceed parent ceiling intersected with trusted host
   policy.
8. Sessions are bound to task tree, agent, cancellation epoch, manifest, and
   generation; numeric IDs are not transferable.

Live recovery, if added, must durably restore and owner-fence topology,
namespace/cwd bindings, grants, leases, sessions, cursors, and receipts. It must
never reconstruct authority from current names or currently connected hands.

## Acceptance journeys

Unit tests and `workdir` routers are insufficient. Real native processes must
prove:

| Journey | Evidence |
| --- | --- |
| Inspect `/` and capability metadata | Safe discovery without identity/secret leakage |
| Run via `workdir=/host1/repo` and shell `cd` | C1, C3, shell placement |
| From host1, natively read/write `/host2` | C2, C5; no copy/sync |
| Run host1 and host2 calls with `Promise.all` | C4 and concurrency |
| Spawn siblings on host1 and host2 | C6, inheritance, independent capacity |
| Have one child successively use two roots | Per-command placement, no pinning |
| Share a permitted file but isolate model state | Export consistency and session privacy |
| Attenuate a child read-only, then spawn its child | Recursive ceiling; no escalation |
| Exercise read-only and denied host2 grants | Projection matches enforcement |
| Rename within and across exports | Atomic local rename; cross-mount `EXDEV` |
| Disconnect, revoke, and replace during I/O | Generation fencing and no fallback |
| Yield, reload, reconnect, and poll a process | Durable session binding, no retargeting |
| Cancel a cross-host pipeline/I/O | Bounded cleanup and isolated call state |
| Interrupt/close a parent during remote work | Descendant cancellation; no orphan work |
| Recover cold | Interrupted tombstones or exact owner-fenced restoration |

End-to-end evidence must inspect ancestry, mounts, broker traffic, storage,
sockets, and secret exposure.

## Phased delivery

1. Specify the manifest and host-generic filesystem interface. (Manifest and
   authority substrate complete; filesystem operation interface pending.)
2. Prove one rooted server and one native Linux namespace client.
3. Add the streaming data plane, receipts, and lifecycle fencing.
4. Mount two real hands and prove native cross-host read/write.
5. Bind root and child sessions to namespace/cwd contexts; prove sibling fan-out.
6. Replace environment-addressed execution with cwd-root placement. (Complete
   for canonical `workdir`; shell `cd` requires the native adapter.)
7. Add each platform adapter only after its conformance gate passes.
8. Verify reload, reconnect, revocation, cross-account isolation, and secret
   containment before deployment.

Cwd routing may be an internal milestone, but the distributed namespace cannot
ship until native processes can access peer mounts.

## References

- [Plan 9 from Bell Labs](https://9p.io/sys/doc/9.html)
- [The Use of Name Spaces in Plan 9](https://9p.io/sys/doc/names.html)
- [9P2000.L protocol](https://github.com/chaos/diod/blob/master/protocol.md)
- [virtio-fs design](https://virtio-fs.gitlab.io/design.html)
- [WASI capability design principles](https://github.com/WebAssembly/WASI/blob/main/docs/DesignPrinciples.md)
