# Nanocodex on exe.dev

This example runs one native, retained Nanocodex session beside a project in an
exe.dev VM. It is a concrete application consumer, not a generic app-server
protocol in the stable Nanocodex crates.

It also includes the inverse experiment: `exe-dev-tool` leaves Nanocodex and
its model credentials on the host while exposing one exact exe.dev VM through
narrow caller-defined tools.

The service:

- owns one ordered prompt queue and one native Nanocodex driver;
- streams the existing typed agent events over SSE without reshaping them;
- atomically persists the complete unredacted session snapshot after every
  completed turn;
- resumes that snapshot when systemd restarts the process; and
- exposes a small no-build web UI on port 9998.

The snapshot contains the full model-visible conversation and tool activity.
The image stores it mode `0600` under the `exedev` user's state directory. Do
not publish, copy, or back it up without applying the same controls as the
source repository and agent transcript.

## Build and create a VM

Build and publish the image from the repository root:

```sh
docker build -f examples/exe-dev/Dockerfile \
  -t ghcr.io/OWNER/nanocodex-exe-dev:latest .
docker push ghcr.io/OWNER/nanocodex-exe-dev:latest
```

Create a VM with the exe.dev LLM integration attached directly or through a
tag:

```sh
ssh exe.dev integrations setup chatgpt --name nanocodex
ssh exe.dev \
  'integrations add llm --name=nanocodex-subscription --openai=chatgpt --openai-account=nanocodex --anthropic=disabled --fireworks=disabled'
ssh exe.dev \
  'integrations attach nanocodex-subscription tag:nanocodex'
ssh exe.dev new \
  --name=nanocodex-spike \
  --image=ghcr.io/OWNER/nanocodex-exe-dev:latest \
  --tag=nanocodex
```

Open `https://nanocodex-spike.exe.xyz:9998`. Alternate ports are private to VM
users on exe.dev. Keep port 9998 private: this example deliberately relies on
exe.dev's authentication boundary and does not implement a second login layer.

Port 8000 remains available for the application Nanocodex builds. To make that
the VM's ordinary preview URL, install the included preview unit and then map
the port:

```sh
scp examples/exe-dev/nanocodex-preview.service nanocodex-spike.exe.xyz:/tmp/
ssh nanocodex-spike.exe.xyz \
  'sudo install -m 0644 /tmp/nanocodex-preview.service /etc/systemd/system/ && sudo systemctl daemon-reload && sudo systemctl enable --now nanocodex-preview'
ssh exe.dev share port nanocodex-spike 8000
```

Only make the application preview public. Never select port 9998 with
`share set-public`.

## Model integration spike

The image points Nanocodex's HTTPS transport at the subscription-backed
exe.dev LLM integration created above:

```text
https://nanocodex-subscription.int.exe.xyz/v1
```

The `OPENAI_API_KEY` value in the unit is a non-secret placeholder because the
integration authenticates the source VM and keeps provider credentials outside
the guest. The integration currently rejects the Responses WebSocket upgrade
with HTTP 400, so this application selects Nanocodex's typed HTTPS/SSE path
with `NANOCODEX_EXE_TRANSPORT=https`. The SDK still owns request encoding,
stream completion, retries, typed history, tools, and events. Inspect a live
turn through the journal:

```sh
ssh nanocodex-spike.exe.xyz journalctl -u nanocodex-exe-dev -f
```

For a direct OpenAI fallback, add a systemd override with the real API key and
the standard endpoints, then restart the service:

```ini
[Service]
Environment=OPENAI_API_KEY=sk-...
Environment=NANOCODEX_EXE_API_BASE=
Environment=NANOCODEX_EXE_TRANSPORT=websocket
```

## exe.dev as an external sandbox tool

`exe-dev-tool` demonstrates the inverse ownership boundary:

```text
host Nanocodex session
  -> sandbox_create
  -> sandbox_exec
  -> sandbox_info
       -> one caller-named private exe.dev VM
```

The model cannot choose a VM name or delete a VM. By default the embedding
application parses `CODEX_THREAD_ID` and derives a stable
`nanocodex-<uuid-without-hyphens>` VM name. `NANOCODEX_EXE_SANDBOX` is an
explicit override for hosts without that Codex identity. The application
applies `retain` or `delete` cleanup only after the turn finishes. Commands
travel to the guest over SSH stdin and receive both a local deadline and a
combined output bound. On Unix, successive operations reuse a private
per-sandbox OpenSSH control master; structured results expose whether the
connection was reused and its master PID. Caller cleanup explicitly closes
every master, with a 60-second idle fallback after abrupt host termination. The
tool registry disables host workspace tools, so shell work cannot silently fall
back to the machine running Nanocodex.

Run it with a Codex-compatible ChatGPT `auth.json` or an OpenAI API key:

```sh
NANOCODEX_AUTH_FILE="$HOME/.codex/auth.json" \
NANOCODEX_EXE_CLEANUP=delete \
cargo run -p nanocodex-exe-dev --bin exe-dev-tool -- \
  'Create the sandbox, run uname -srm, write and verify ~/proof.txt, then report the result.'
```

The host must already be authenticated for non-interactive `ssh exe.dev` and
guest SSH. Set `NANOCODEX_EXE_SANDBOX` when running outside a Codex environment;
`NANOCODEX_EXE_SSH` may select a different SSH executable. Cleanup defaults to
`retain`; use `delete` only when the named VM belongs exclusively to this run.

## Local validation

```sh
cargo test -p nanocodex-exe-dev
cargo clippy -p nanocodex-exe-dev --all-targets -- -D warnings
OPENAI_API_KEY=sk-... cargo run -p nanocodex-exe-dev
NANOCODEX_EXE_SANDBOX=nanocodex-tool-local cargo run \
  -p nanocodex-exe-dev --bin exe-dev-tool
```

Configuration is intentionally narrow:

- `NANOCODEX_EXE_LISTEN` defaults to `0.0.0.0:9998`;
- `NANOCODEX_EXE_WORKSPACE` defaults to the current directory;
- `NANOCODEX_EXE_STATE_FILE` defaults inside that workspace;
- `NANOCODEX_EXE_INSTRUCTIONS` replaces the stable project-agent instructions;
- `NANOCODEX_EXE_TRANSPORT` selects `websocket` (default) or `https`;
- `NANOCODEX_EXE_API_BASE` replaces the OpenAI HTTPS API root; and
- `NANOCODEX_EXE_WEBSOCKET_URL` replaces the Responses WebSocket endpoint.
