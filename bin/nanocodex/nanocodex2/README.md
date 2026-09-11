# Nanocodex2

The managed terminal client uses the same durable agents, model settings, and
scheduled prompts as the web and native apps. Run `nanocodex2 login` to sign in
with an SMS code. `NANOCODEX_MANAGED_URL` selects another cluster.
Running `nanocodex2` opens a new interactive session;
`nanocodex2 attach AGENT_URL_OR_ID` resumes an existing one with local workspace
tools.

Image rendering uses terminal geometry and known terminal hints without reading
keyboard input for capability probes. Recognized Kitty, Ghostty, iTerm2, and
WezTerm environments use native images where supported; other terminals use
half-block images.

## Account sign-in

```bash
nanocodex2 login                    # Prompts for your phone number and SMS code
nanocodex2 status                   # Verifies the selected key; prints account JSON
nanocodex2 logout                   # Removes this server's saved login locally

# Import an existing account-issued key through stdin, never a command argument.
cat /path/to/private-api-key | nanocodex2 login --with-api-key

# Select a different server for both authentication and managed commands.
export NANOCODEX_MANAGED_URL=https://your-cluster.example
nanocodex2 login --phone '+1 415 555 0123' --label 'Work laptop CLI'
```

`nanocodex account login/status/logout` uses the same implementation and saved
account credentials. `nanocodex2 account` (also `auth`) groups those commands.
The native CLI's existing `nanocodex login/connect/status/logout` commands still
manage Connect installation grants; `nanocodex auth` manages ChatGPT provider
credentials. Those credentials are independent of the managed account key.

Connect accepts `chatgpt`, `github`, `gmail`, `gdrive`, `gcalendar`, `gtasks`,
`gdocs`, `gsheets`, `gslides`, `gcontacts`, `slack`, `x`, and public `mcp.*` hosts.
For example, `nanocodex connect slack gcalendar` authorizes those services for
the local installation through the existing browser approval flow.

SMS login exchanges the verified session for an API key and ends the temporary
session. The phone number, SMS code, and session cookie are never saved. If login
is cancelled or saving fails after minting, the CLI attempts to revoke the unused
key before ending that session. Incorrect codes can be retried up to three times;
run login again to request a fresh code. Rate-limit responses show the retry delay.

The shared credential file is `$CODEX_HOME/nanocodex-account.json`, defaulting to
`~/.codex/nanocodex-account.json`. It is written atomically with private Unix
permissions and stores a separate key for each exact server origin. Set
`NANOCODEX_ACCOUNT_FILE` to use another file (auth commands also accept
`--account-file`). Only HTTPS and loopback HTTP are accepted; redirects
are never followed.

For automation, `NANOCODEX_API_KEY` takes precedence over `NC_API_KEY`, which
takes precedence over the saved login. An explicitly empty or invalid key fails
instead of falling through to another account. `status` reports the selected
source and public key ID without printing the secret. Logout removes only the
selected server's saved key; environment credentials remain active until unset.
To revoke a key remotely, remove it in the web account's API Keys menu. Logging
in again replaces the saved key without revoking previous account keys.

## Working in a running session

Press Enter to send steering input during a response, or Tab to queue a
follow-up for when the current turn finishes. Esc twice interrupts the turn.
Press Alt+U to undo the latest queued or steered message before the model receives it.
Confirmed withdrawal restores the message and its images to the composer. If you
are already writing another draft, clear the composer and press Ctrl+Z to restore
the withdrawn message. Failed or unconfirmed withdrawals preserve the original.

Rapid steering instructions are sent in order. The terminal records its own
successful acknowledgements as **steering accepted**. This confirms admission,
not application at a model boundary. Shared steering telemetry can originate
from another client and is never used to confirm a local instruction.
Steering takes effect at the next model step, so a running tool can finish its
current call first. Accepted steering is never automatically retried. If an
acknowledgement is lost, the instruction stays visible as **delivery unknown**,
including after the turn finishes. Select it to explicitly edit/retry or dismiss
it; cancelling the editor preserves its unknown status. Further steering waits
until that turn ends, then known-unsent follow-ups continue in order. This avoids
duplicating potentially delivered instructions across clients.
Queued follow-ups also run when an agent resumed with `attach` finishes work
that started in another client.

Scrolling back through older history keeps typing and live updates responsive.
Local `!` commands can also be stopped with Esc twice; captured output remains
in the transcript and is included with the next prompt. On macOS and Linux,
cancellation stops the shell's process group, including its child processes.

## Headless controls

```bash
# Create with explicit initial settings; defaults are Astra, low, standard.
nanocodex2 new --model astra --thinking high
nanocodex2 run "Inspect this repository" --model sol --thinking high
nanocodex2 run "Continue the review" --agent AGENT_ID

# Read settings or update one field for subsequent turns.
nanocodex2 settings AGENT_ID
nanocodex2 settings AGENT_ID model astra
nanocodex2 settings AGENT_ID thinking high
nanocodex2 settings AGENT_ID reasoning-mode standard
nanocodex2 settings AGENT_ID fast-mode true

# Create or replace a durable schedule, then inspect or delete it.
nanocodex2 cron put AGENT_ID daily --cron "0 9 * * *" \
  --timezone Europe/Athens --prompt "Summarize overnight progress"
nanocodex2 cron list AGENT_ID
nanocodex2 cron get AGENT_ID daily
nanocodex2 cron delete AGENT_ID daily
```

Creation flags on `run` apply only to new agents. Use `settings` to change an
existing agent. Astra accepts low through max effort and standard reasoning
mode; incompatible settings fail before creation. Cron defaults to a new agent
per occurrence; use `--session-mode continue` to append to the owning agent,
or `--disabled` to retain an inactive schedule. `cron put` replaces the full
configuration. The service validates cron expressions and IANA timezones.

Control commands return JSON; `run` and `watch` stream JSONL. The terminal
retains command output through polling and recovery replay, reports actual
process exits, and preserves recent diagnostics when a process disappears.
Expanded tool results retain text and resource URLs alongside media metadata;
embedded binary payloads are hidden.

Build and test both CLI consumers from the repository root:

```bash
cargo build -p nanocodex-bin -p nanocodex2-bin
cargo test -p nanocodex-bin -p nanocodex2-bin -p nanocodex-managed -p nanocodex-cli-auth
```

## VM hand

`nanocodex2 hand` registers one retained libkrun VM as an account-scoped
execution hand. Any hosted agent in the account can use the VM through the
standard `exec_command` and `write_stdin` process contracts over the existing
outbound Hosted Tools WebSocket. The logical cwd selects the hand; inside the
selected VM it is translated to that hand's native workspace.

```bash
cargo build -p nanocodex-vm --no-default-features --features guest-runtime \
  --bin nanocodex-vm-guest --target x86_64-unknown-linux-musl

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

## Docker hand (no KVM)

Use Docker on glibc Linux or Apple Silicon macOS with a Linux Docker daemon.
Linux containers do not require `/dev/kvm`; Docker Desktop still needs its own
Linux VM on macOS. This is an explicit container backend with a shared host
kernel, not an automatic fallback from failed VM startup.

Build the image for the selected Docker daemon's architecture from the repo root
(the matching Rust musl target and linker must be installed):

```bash
pnpm build:hand-docker
nanocodex2 hand \
  --docker nanocodex-hand:local \
  --docker-volume personal-hand-workspace \
  --machine-id personal-hand \
  --machine-name "Personal Docker Hand"
```

The existing account login supplies attachment authority. No account credential,
Docker socket, or host directory is passed into the container. The bundled image
contains the Rust guest runtime, shell, Git, Python, Node, and an X11 desktop.
Images must already exist on the selected Docker daemon; launch never pulls.
Use a pinned image digest when deploying an image from a registry.

Docker Hands default to **offline**. `--docker-internet` explicitly enables
ordinary Docker bridge networking, including any destinations that network can
reach. This mode does not enforce broker-only egress. Account signaling and
screen publication remain in the host process and work with an offline guest.
A future broker-only mode must enforce its network boundary, not rely on proxy
environment variables. `--docker-runtime runsc` selects an installed Docker
runtime without fallback; gVisor/desktop compatibility must be checked on that
host. The launcher does not install or configure gVisor.

`--vm-workspace` (default `/app`), `--vm-cpus`, `--vm-memory-mib`, and
`--vm-shell` apply to either backend. Docker Hands publish `container` instead
of `vm` in the machine capability list. `--vm` and `--docker` are mutually
exclusive; Docker images bundle their runtime and do not accept ext4 guest or
firmware options.

The named volume holds the workspace, including `$HOME` at `/app/.home` by
default. Reuse the same volume and machine ID to resume files after a restart or
image replacement. The root image is read-only; customize system packages in
the Dockerfile and install project dependencies inside the workspace. Custom
images must provide `/usr/local/bin/nanocodex-vm-guest`, `/bin/sh`, `/bin/sync`,
and a workspace directory writable by UID/GID 1000. Runtime scratch data under
`/run` and `/tmp` is temporary. Files on the volume are independent of the
managed brain's durable application state.

A deterministic container name reserves each workspace volume for one Hand.
A duplicate launch fails without stopping the current owner. Ctrl-C/SIGTERM
while attached drains work, syncs the guest, and removes the container; the
volume is retained. Dropping the last tool capability also schedules bounded
container cleanup. A killed host process or unreachable Docker daemon can leave
a stale container. Inspect `docker ps -a --filter volume=personal-hand-workspace`
and remove the exact stale container after confirming its Hand is stopped;
then relaunch. The launcher never removes an existing owner's container.
Back up volumes separately; container cleanup never deletes the named workspace.

The design takes inspiration from [Meta's separation of the agent runtime from
credential and permission services](https://research.meta.ai/blog/security-and-safety-for-ai-agents-our-approach-with-muse).
It reuses Nanocodex's existing host-owned attachment and screen authority; it
is not an implementation of Meta's Sentinel or a claim of VM-equivalent isolation.

Run the Docker contract tests against the image:

```bash
NANOCODEX_DOCKER_TEST_IMAGE=nanocodex-hand:local \
  cargo test --locked -p nanocodex-vm --test docker_live -- --ignored
```

The on-demand `host` pool remains libkrun-only; Docker is available through the
single `hand` command and the `nanocodex_vm::docker` library API.

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
