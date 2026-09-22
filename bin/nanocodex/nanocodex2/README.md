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

## Hand screens

Type `/screen`, filter by Hand name, then press Enter to watch its live screen
beside the conversation. It stays inside the current terminal window. Tab and
Shift+Tab cycle the chat, screen, and any existing `/btw` pane; `/zoom` toggles
the focused pane between the split layout and full width. In the screen pane,
`z` also toggles zoom, `r` reconnects, and Esc closes the viewer. `/screen` there
returns to Hand selection. Watching does not acquire mouse or keyboard control.
Desktop Opus audio plays automatically through the local output device. Press
`m` to mute or unmute. Closing the viewer or switching Hands stops its audio;
watching never opens the microphone. Audio failure is shown separately and
does not interrupt video.

The viewer receives the Hand's live H.264/WebRTC stream and targets 60 video
frames per second. `ffmpeg` decodes video and `ffplay` handles stereo Opus RTP,
packet reordering and audio output. Both come from the local FFmpeg package.
Executable discovery checks PATH and standard Homebrew/system directories.

Local Kitty-compatible terminals, including Ghostty, read temporary RGB pixel
buffers directly, avoiding per-frame base64 encoding and Rust-side resizing.
The decoder scales to the pane’s pixel dimensions and refreshes on zoom/resize.
Frames replace one named image placement, keeping the image grid stable between
updates. Pending transfers are bounded and reclaimed on close. Over SSH, or
when local file transfers are unavailable, the viewer falls back to inline
terminal graphics. Other image protocols and text previews remain supported.
The displayed FPS counts distinct frames presented by the TUI. Source cadence,
terminal rendering and transport still determine actual throughput; a 60 fps
target does not upgrade a slower publisher. Slow presentation drops stale frames
instead of queuing old video. `frames-v1` Hands retain their existing lower-rate
image transport and do not publish audio.

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
`gdocs`, `gsheets`, `gslides`, `gcontacts`, `slack`, `x`, `spotify`, `soundcloud`,
and public `mcp.*` hosts.
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

## Voice

In the terminal, `/voice` toggles voice in the current conversation. During agent
startup it starts automatically once connected; mute and stop work while
waiting. The empty conversation shows a pixel spinner around `nanocodex2`.
Spoken user and voice-agent messages stream inline in the chat and remain in
scrollback after the call ends. A compact strip above the composer shows audio
levels and call status. Ctrl+X or `/voice mute` toggles the microphone; `/voice unmute`
explicitly unmutes it. `/voice on` and `/voice off` (also `start`/`stop`) explicitly
start or stop; `/voice status` reports state. `/voice voices` lists choices and
`/voice cove` starts with a named voice. Voice also appears in the Actions menu.
Typed input suppresses stale spoken replies; stopping voice stops audio
immediately and leaves agent work running. Switching conversations closes the previous conversation's audio.

For a voice-only terminal session:

```bash
nanocodex2 voice                         # Creates a conversation
nanocodex2 voice --agent AGENT_ID         # Resumes an existing conversation
nanocodex2 voice --voice cove --muted     # Connects with the microphone muted
nanocodex2 voice --muted --duration 10 --log-format json
```

Ctrl+C stops the call. The command prints the conversation ID and JSON status
updates; timing logs separately report media startup, control-channel readiness,
agent handoff, and speech delivery. A connection check does not prove speech
recognition or audible playback. Voice needs a connected ChatGPT account and
the matching native voice package beside the executable. Source builds can set
`NANOCODEX_VOICE_PACKAGE` to a package directory and must use the helper's
`STABLE_GIT_COMMIT` build identity. Availability is checked before opening audio
hardware; an unavailable runtime produces a visible error.

The managed service owns authentication and agent execution. The CLI uses the
shared voice protocol and native audio helper, starts media and agent admission
concurrently, and reads agent events independently of realtime audio events.
The latter prevents frequent audio traffic from cancelling a pending event-stream
connection and delaying spoken tool results.

## Working in a running session

On macOS and Linux, after installing a new binary at the same executable path,
use `/reload` in any
terminal to restart this user's reload-capable interactive nanocodex2 instances
on the current machine. Each returns to its current managed thread in the same
terminal and working directory. Accepted managed turns continue running; pending
local operations finish before the client disconnects. This does not restart
Hand services, VM helpers, or instances on other machines. Instances started with
an older binary without reload support need one manual restart first.
Reload restores the managed thread, not unsent drafts or the current pane layout.


Use `/id` to open the agent ID popup. Press Enter to copy the full ID to the
clipboard, or Esc to close it. Resume it later with `nanocodex2 attach AGENT_ID`.

Press Enter to send steering input during a response, or Tab to queue a
follow-up for when the current turn finishes. Esc twice interrupts the turn.
Press Alt+U to undo the latest queued message or steering instruction before
the model receives it. A successful undo restores the text and images to the
composer. If the composer already has a draft, it stays intact; clear it and
press Ctrl+Z to restore the withdrawn message. Steering is withdrawn only after
the server confirms it is still pending; if it has already been consumed, undo
leaves it in the conversation.
Ctrl+Z continues to restore the last cleared draft.
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
In the queue editor, Enter saves the revision and Esc cancels it. Tab leaves the
revision in the editor; it does not enqueue a separate message.
Queued messages and unknown-delivery retries retain their images in the editor.
You can change the text, remove attachments, or paste additional images before saving.
Esc can also cancel an edit while disconnected, restoring the original composer
draft without sending anything. Enter remains the reconnect action while offline.
While `attach` loads the session, sending is paused and the draft stays editable.
Text and image attachments survive loading. Press Enter after connecting to send
or steer; pressing it during loading does not start a parallel turn.

If the connection fails, the terminal reconnects to the same agent and catches
up on missed history. Your draft stays editable, queued input stays in order,
and remote work continues. A prompt whose delivery cannot be confirmed remains
visible as **delivery unknown** for explicit retry or dismissal. If reconnecting
fails or the replacement connection immediately fails again, press Enter to
retry; this keeps the draft without sending it. Once connected, Enter resumes
its usual send/steer behavior.
Ctrl+C clears an unfinished draft and Ctrl+Z restores it, including its images
and cursor position, even while disconnected or reconnecting.
An update the terminal cannot decode appears as a transcript error; other
history remains available and the draft is preserved.
Completed answers are recovered from the durable result when streamed final
text is missing. Partial text is completed in place and repeated final messages
remain a single answer. Successful, failed, and cancelled turns stop unfinished
tool spinners even when their streaming terminal events are missing. Commands
that returned a running process ID stay connected for polling in later turns.
Late lifecycle and usage events cannot restart a finished turn's activity or
replace the current turn's context count. Background output and child-agent
updates remain available.
Durable failure reasons replace provisional errors from the same run in place.
Earlier retry attempts keep their own errors; other turns and child agents keep
running.

Use `/bug [description]` to investigate a Nanocodex framework problem. It starts
an independent durable cloud agent with the source session ID, event cursor, and
a bounded snapshot of recent transcript records, then switches the TUI to that
agent's thread so you can watch the investigation and fix. The description is
optional, and the command works while the source agent is busy. Accepted source
turns continue in the cloud; local shell work is stopped when switching. If
launching or attaching fails, the source thread stays open and the error includes
the new agent ID when available. Use `/attach` to return to the source thread.

Scrolling back through older history keeps typing and live updates responsive.
`nanocodex2 attach` and the in-TUI `/attach` command show recent threads first,
ordered by last activity, with titles above session IDs. Type to fuzzy search
titles and IDs; space-separated terms can appear in any order. Title matches rank
by relevance, with recent activity breaking ties. The same query also searches
retained user and assistant messages after a short typing pause. Content matches
appear once per attachable thread, below title matches. A preview pane on the
right shows the selected thread's matching passage, wraps the text, and highlights
literal query terms. Page Up/Down scrolls the preview. In narrower terminals the
preview moves below the list; very small terminals keep compact inline excerpts. This uses
the server's history search (up to 20 hits), while title/ID matching stays local
and fuzzy. Clearing the query restores recent threads. Use arrows or Ctrl+N/Ctrl+P to
move, Enter/Tab to select, Ctrl+U to clear, and Esc/Ctrl+C to close.
If the session picker takes too long to load, Esc or Ctrl+C cancels the lookup
and restores the draft. A cancelled lookup cannot reopen or replace a newer picker.
If the current turn finishes during a lookup, ready follow-ups resume when the
lookup completes, fails, or is cancelled; the unfinished draft stays in the composer.
Editing while offline dismisses open pickers and cancels pending lookups, so late
results cannot replace the edited draft after reconnecting.
Browsing earlier prompts with Up/Down or Ctrl+P/Ctrl+N preserves image attachments
when you return to the unfinished draft.
Messages sent or queued in the current terminal session also retain their images
when recalled, edited, and submitted again.
Answers and tool results remain available when a long turn spans history pages.
Streaming progress summaries stay with their own turn and child agent when
steering acknowledgements, other turns, or local output arrive between chunks.
Local `!` commands can also be stopped with Esc twice; captured output remains
in the transcript and is included with the next prompt. On macOS and Linux,
cancellation stops the shell's process group, including its child processes.
Local shell cancellation remains available while the managed connection is down.
Pending local shell output stays with its session: successfully resuming another
session clears it, while a failed resume keeps it available for the original session.
While a selected session loads, input remains paused even if the old connection
drops. The terminal waits for that switch; if it fails, it recovers the original
connection instead. Background activity from the old session does not carry over.
Esc or Ctrl+C cancels a slow session switch and keeps the original draft. Late
results from the cancelled switch cannot replace the current or newly selected session.
An open Actions menu updates as work starts or finishes, so session actions become
available without reopening the menu.

## Headless controls

```bash
# Create with explicit initial settings; defaults are Astra, low, standard.
nanocodex2 new --model astra --thinking high
nanocodex2 run "Inspect this repository" --model sol --thinking high
nanocodex2 run "Continue the review" --agent AGENT_ID

# Pin a new session to one connected ChatGPT account for testing.
nanocodex2 new --chatgpt-account ACCOUNT_ID
nanocodex2 run "Reply with hello" --chatgpt-account ACCOUNT_ID

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
  --volume personal-hand-workspace \
  --machine-id personal-hand \
  --machine-name "Personal Docker Hand"
```

Startup checks backend support before account login or attachment. Missing Docker, a stopped or inaccessible daemon, non-Linux containers, missing images, mismatched image architecture, and unavailable OCI runtimes produce actionable errors.

The existing account login supplies attachment authority. No account credential,
Docker socket, or host directory is passed into the container. The bundled image
contains the Rust guest runtime, shell, Git, Python, Node, and an X11 desktop.
Images must already exist on the selected Docker daemon; launch never pulls.
Use a pinned image digest when deploying an image from a registry.

Docker Hands default to **offline**. `--network internet` explicitly enables
ordinary Docker bridge networking, including any destinations that network can
reach. This mode does not enforce broker-only egress. Account signaling and
screen publication remain in the host process and work with an offline guest.

Rust native and VM/Docker screens default to continuous H.264/WebRTC at 60 Hz.
Install FFmpeg on native hosts; desktop guest images already include it. macOS
uses AVFoundation capture of the main display and VideoToolbox encoding; Linux
uses X11 capture and low-latency x264. Keyboard/control use a reliable WebRTC
channel, and pointer motion keeps only the latest event. Capture runs separately
from input and agent observations. Agent screenshots retain their bounded JPEG
contract. Actual decoded frame rate depends on capture, CPU/GPU, and network.

Upgrade the CLI and guest runtime together to enable video in VMs and Docker
Hands. The host recognizes older desktops and retains `frames-v1` compatibility;
missing or unavailable encoders also fall back to screenshots. Encoded guest
stdout travels over the existing private control channel, with bounded queues,
backpressure cancellation, and no need to enable guest networking. Account and
WebRTC credentials stay on the host.

Optional host settings `NANOCODEX_VIDEO_INTERFACE`, `NANOCODEX_VIDEO_IPV4_ONLY=1`,
and `NANOCODEX_VIDEO_UDP_PORTS=50032-50127` constrain ICE candidates and firewall
ports. Defaults use all interfaces, both address families, and ephemeral ports.
These settings are host configuration; do not put host interface names in images.
A future broker-only mode must enforce its network boundary, not rely on proxy
environment variables. `--runtime runsc` selects an installed Docker
runtime without fallback; gVisor/desktop compatibility must be checked on that
host. The launcher does not install or configure gVisor.

`--workspace` (default `/app`), `--cpus`, `--memory`, and
`--shell` apply to either backend. Docker Hands publish `container` instead
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

The original `--docker-volume`, `--docker-runtime`, `--docker-internet`, and
`--vm-*` flag spellings remain accepted. New commands should use the shorter
names above. `--network off` explicitly disables networking for either backend;
VM networking remains enabled by default for compatibility. VM-only settings
such as `--gpu` and `--guest-runtime` cannot be combined with `--docker`.
VM-specific environment defaults are ignored when selecting Docker.

On Linux, `--vm` checks access to `/dev/kvm`, the KVM API, and VM creation
before account setup. If KVM is unavailable, the error explains how to enable
it and shows `hand --docker IMAGE --volume NAME` as the explicit alternative.
Run `hand` without a backend flag to connect the native computer.

## Browser interactions through a connected Hand

Agents interact with desktop browsers through the Hand's CUA tools. Native,
VM, and Docker Hands do not publish `browser_execute` or browser egress
capabilities. A connected Hand must provide CUA tools for browser interaction;
a screen publisher alone does not provide agent control.

The legacy `--browser`, `--browser-executable`, and
`NANOCODEX_BROWSER_EXECUTABLE` options are rejected with guidance to use CUA.

The on-demand `host` pool remains libkrun-only; Docker is available through the
single `hand` command and the `nanocodex_vm::docker` library API.

## On-demand VM hosts

`nanocodex2 host` advertises bounded capacity instead of attaching one VM. The
command registers a named VM factory. The managed control plane asks that exact
factory to create a private VM when an agent uses its name as the `/mount`
provider, and releases that VM when the durable agent is deleted.
Every allocation gets its own cloned root image, Hosted Tools attachment, and
machine identity. One host process can run up to `--max-vms` allocations.

By default the factory keeps one fresh, never-assigned VM and its desktop
running ahead of demand. Claiming a ready spare avoids booting in the mount
request; remote screen/tools registration still happens at allocation time.
The spare uses one VM's configured memory within `--max-vms`, and is replenished
when capacity permits. A used VM is never returned to the spare pool. Retained
allocations always restart from their own disks. Set `--warm-spare=false` to
avoid idle VM resource use. A first factory launch or exhausted spare still
pays cold-start time; this does not make cold boot instantaneous.

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

### This computer and its VMs

Opening `nanocodex2`, attaching a conversation, or using `run` automatically
connects this computer as an account-wide Hand in `~/Nanocodex`. The Mac app and
terminal share one computer identity and local publisher, including across
separate login keys for the same account. Closing one client leaves the host and
its VMs running while another client is connected. The last client disconnects
the host after a short grace period; retained VM disks remain on disk.

Use `nanocodex2 hand` to keep the computer connected without opening a
conversation. Use `hand --workspace /path/to/repo --state-dir /private/identity`
for an additional explicit workspace, or `hand --vm ...` / `hand --docker ...`
for an isolated Hand. Set `NANOCODEX_DISABLE_HAND=1` to disable automatic
account-wide sharing in the CLI; the selected conversation's folder tools remain
available.

A configured desktop VM recipe (`desktopRootfs`, `guestRuntime`, `binary`, and
optional `firmware` in the app's `vm.json`) starts an on-demand VM provider in the
background. Execution VMs are created on demand; GPU preflight may boot a
temporary guest. Native host readiness does not wait for VM registration or
screen permissions. The app reports VM readiness
separately. The GPU Hand build script writes the desktop recipe explicitly.

`accountInfo().machines` advertises `vm_provider` on the online Mac. The agent
uses the existing Mac mount for native work, or calls `mount` with that provider
to create a private VM. Each VM has its own workspace and screen. A configured
provider may still be connecting; mount verifies current readiness and capacity.

## Computer Hands on macOS, Linux, and Windows

The Hand publisher is owned by an OS service. The CLI and desktop app connect
as clients; closing the last client leaves the Hand and VM host running.
`NANOCODEX_DISABLE_HAND=1` opts out of CLI attachment. `nanocodex2 hand` runs the
publisher in the foreground; `hand --workspace PATH` publishes a separately
retained workspace.

On macOS, install and manage the login service from the standard Rust CLI:

```sh
nanocodex hand install
nanocodex hand status
nanocodex hand restart
nanocodex hand stop
nanocodex hand start
```

The per-user LaunchAgent runs the installed `nanocodex2` directly and starts at
login. It does not require Python or a shell service wrapper. It refuses to
compete with an existing system LaunchDaemon. Use `--executable PATH` with
`hand install` for an explicitly selected Hand binary. Starting before user
login is not supported by these user-service commands.

`nanocodex update --nightly --restart-hand` stages a complete verified release, switches the
installed Hand service, waits for that exact executable to publish a connected
catalog, and then activates the CLI. A failed Hand startup restores the previous
service and leaves the CLI unchanged. An explicit update restarts the Hand and
its VM host; finish active work first. For a matching local build, use
`nanocodex update --path PATH_TO_NANOCODEX --hand-binary PATH_TO_NANOCODEX2`.

```sh
nanocodex update --auto enable --nightly
nanocodex update --auto status
nanocodex update --apply --restart-hand
nanocodex update --auto disable
```

Automatic checks run hourly. When a Hand service is installed, they download and verify
a pending release without starting or restarting the Hand or switching the active
CLI. `hand start` applies a pending update while starting a stopped service, and
keeps the candidate running only after account reconnection succeeds. To update a running
Hand, explicitly use `nanocodex update --apply --restart-hand`; this restarts its
VM host as well. Ordinary updates defer activation when a Hand service is installed. Automatic
checks use the stable channel unless enabled with `--nightly`. Interrupted
activation retains a recovery record; `nanocodex hand recover` restores the
previous service and CLI, or finishes cleanup of an already committed update.

Optional VM assets live in a `vm.json` recipe. `NANOCODEX_DESKTOP_DATA` overrides
its containing directory:

| Host | Default recipe directory |
| --- | --- |
| macOS | `~/Library/Application Support/Nanocodex/Native` |
| Linux | `$XDG_DATA_HOME/nanocodex/native`, or `~/.local/share/nanocodex/native` |
| Windows | `%LOCALAPPDATA%\Nanocodex\Native` |

A recipe names `binary`, `desktopRootfs`, `guestRuntime`, optional `firmware`,
and optional `gpu`. Use absolute paths to a VM-capable host executable, immutable
desktop ext4 image, and Linux guest ELF. The native host connects independently
of VM initialization. Its `vm_provider` tells the agent exactly where to create
VMs on that computer; factory readiness and capacity are checked when mounting.

On Windows the host runs natively; VM provisioning uses the Linux host executable
inside a configured WSL2 distribution. Example Windows `vm.json`:

```json
{
  "wslDistribution": "Ubuntu",
  "binary": "/opt/nanocodex/current/nanocodex2",
  "desktopRootfs": "/opt/nanocodex/images/desktop.ext4",
  "guestRuntime": "/opt/nanocodex/current/nanocodex-vm-guest",
  "firmware": "/opt/nanocodex/firmware",
  "gpu": false
}
```

These are Linux paths inside that distribution. WSL2 must expose readable and
writable `/dev/kvm` to its default user; this requires working nested
virtualization. The helper checks KVM before registering. Missing VM assets or
KVM do not prevent native Windows shell/filesystem work. Credentials are passed
in the child environment, never in command arguments. Closing the last client
closes the factory's parent pipe so it drains and stops its VMs, including across
WSL. See [Microsoft's WSL configuration reference](https://learn.microsoft.com/en-us/windows/wsl/wsl-config)
for `nestedVirtualization` and supported Windows configurations.

Linux servers installed with `nanocodex hand setup` keep their existing systemd
services and retained identities. The installer links the native computer to its
factory using `hand --vm-provider NAME`; re-run setup to update existing units.
