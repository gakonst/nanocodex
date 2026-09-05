# Nanocodex2 VM hand

`nanocodex2 hand` registers one retained libkrun VM as an account-scoped
execution hand. Any hosted agent in the account can use the VM through the
standard `exec_command` and `write_stdin` process contracts over the existing
outbound Hosted Tools WebSocket. The logical cwd selects the hand; inside the
selected VM it is translated to that hand's native workspace.

```bash
just build-vm-guest

NANOCODEX_API_KEY=ncx_live_... \
nanocodex2 hand \
  --vm /srv/nanocodex/build-root.ext4 \
  --vm-guest-runtime target/x86_64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /workspace \
  --vm-cpus 8 \
  --vm-memory-mib 16384 \
  --machine-id build-vm \
  --machine-name "Build VM"
```

Set `NANOCODEX_MANAGED_URL` to connect the same binary to another Nanocodex
cluster. The API key determines account attachment authority and is not passed
into the guest. The hand initiates the only network connection, so it works
behind NAT without an inbound listener.

The raw ext4 root is modified in place and exclusively locked while attached.
It survives turns and reconnects, but its files are independent from the
brain's Cloudflare Computer workspace and its lazy Cloudflare Sandbox. Use
`--vm-no-network` for an offline guest. A directory root is supported as a
development escape hatch and must already contain
`/usr/local/bin/nanocodex-vm-guest`.

The immutable attachment snapshot publishes the guest workspace plus `vm`,
`linux`, shell/filesystem/process/PTY, network state, CPU count, and memory to
`accountInfo().machines`. Reconnecting the hand replaces its current account
attachment generation under the existing lease/fencing rules. Ctrl-C drains
admitted calls, syncs the guest filesystem, and stops the VM.

## On-demand VM hosts

`nanocodex2 host` advertises bounded capacity instead of attaching one VM. The
command registers a named VM factory. The managed control plane asks that exact
factory to create a private VM when an agent uses its name as the `/mount`
provider, and releases that VM when the durable agent is deleted.
Every allocation gets its own cloned root image, Hosted Tools attachment, and
machine identity. One host process can run up to `--max-vms` allocations.

```bash
NANOCODEX_API_KEY=ncx_live_... \
nanocodex2 host \
  --scope user \
  --factory-name garage-mac \
  --vm-template /srv/nanocodex/template.ext4 \
  --state-dir /srv/nanocodex/host-state \
  --vm-guest-runtime target/x86_64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --max-vms 10 \
  --vm-cpus 8 \
  --vm-memory-mib 16384
```

The scope chooses who may consume the advertised capacity:

- `--scope user` is the default. Any agent owned by the API-key account may
  request a VM.
- `--scope agent --agent AGENT_ID` reserves the host for one durable agent.
- `--scope system` contributes capacity to the whole managed system. It uses
  `NANOCODEX_SYSTEM_HOST_TOKEN`, not an account API key.

Several factories may be connected at once. `--factory-name` is the exact
lowercase portable selector agents pass to `/mount`; it is unique within its
scope and remains bound to the persisted host identity. `cf_sandbox` names the
built-in Cloudflare factory and cannot be registered by a device. If the same
factory name is visible in several scopes, agent scope shadows user scope,
which shadows system scope; an unavailable higher-priority factory is never
silently replaced by a different lower-priority machine.

Regardless of pool scope, an allocated VM and its tool connection are leased
only to the durable agent that requested them. When multiple scopes have free
capacity for the requested name, lookup prefers the exact-agent pool, then the
user's pool, then the system pool. The host identity is generated once under `--state-dir`; that
directory is process-locked and also retains allocation roots across host
restarts. Graceful host shutdown stops VMs without deleting those roots so the
next control lease can reconcile them.

The command emits structured lifecycle and call traces to stderr by default.
They include the machine ID, configured CPU/memory and root-image size,
connection and catalog state, and each call's ID, tool name, outcome, and
duration. VM launch, VM shutdown, and each attachment call are bounded spans,
so long-running hands export them continuously. Command arguments, output,
credentials, remote failure reasons, machine names, workspaces, and host paths
are omitted. Use `--log-format json`, `--log-file PATH`, or
`--otel-endpoint URL` for standard JSON, retained-file, or OTLP output;
`--log-filter` and
`RUST_LOG` accept normal tracing filter directives.
