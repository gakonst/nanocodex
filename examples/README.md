# Nanocodex examples

All language consumers live at this repository boundary:

- Rust: `minimal.rs`, `durable.rs`, `voice.rs`, `realtime_pipe.rs`,
  `realtime_external.rs`, `follow_on.rs`, `lifecycle.rs`, `custom_tool.rs`,
  `subagents.rs`, `resume.rs`, `fork_conversations.rs`,
  `fork_checkpoint_bench.rs`, `secret_egress.rs`, and `mcp.rs` are binaries in the
  `nanocodex-examples` package.
- Python: `python/` uses the native PyO3 binding (`follow_on.py`, `events.py`,
  `lifecycle.py`).
- Node.js: `node/` uses the shared Rust/WASM package with a Node WebSocket host
  and demonstrates the Rust-owned task-tree extension compiled into WASM.
- Browser: `react-vite/` runs that WASM agent in a module Worker and renders its
  ordered events in React.
- Existing Privy auth: `privy/` is an independent Cloudflare/Vite application
  that exchanges a server-verified Privy session for a hosted Connect principal.
- Existing Better Auth: `better-auth/` is an independent Cloudflare/Vite
  application with GitHub login, D1-backed sessions, and a hosted Connect
  principal. Both host-auth examples prove two durable turns across reload.
- Browser CDN: `browser-cdn/` is one static HTML file that imports the published
  package directly, with no install or build step.
- Rivet Actors: `rivet-actors/` runs the same harness as a durable,
  SQLite-backed Rivet Actor with an actor-owned AgentOS sandbox.
- Cloudflare fetch + MCP: `cloudflare-fetch-mcp/` exposes one authenticated HTTP
  fetch endpoint backed by Rust/WASM, Tempo MPP, deferred Mercator MCP tools,
  and QuickJS Code Mode inside a serialized Durable Object wallet owner.
- Vercel Workflows: `vercel-workflows/` runs Nanocodex as a durable Workflow
  actor with a persistent Vercel Sandbox, replayable state, and synchronized
  native WebSocket clients.
- exe.dev: `exe-dev/` proves both a private retained session inside a persistent
  VM and an external Nanocodex session using an exe.dev VM as a caller-owned tool.

From the repository root:

```sh
cargo run -p nanocodex-examples --bin minimal
# Compose OpenAI, tools, the agent lifecycle, and the optional durability layer:
cargo run -p nanocodex-examples --bin durable
# Own the default microphone and speaker directly in Rust:
cargo run -p nanocodex-examples --bin voice
# Or keep devices outside the process and compose raw PCM with Unix pipes:
cargo run -p nanocodex-examples --bin realtime-pipe < microphone.pcm > speaker.pcm
# Apply the printed answer SDP to a caller-owned peer:
cargo run -p nanocodex-examples --bin realtime-external -- offer offer.sdp > answer.sdp
# Or attach only the authenticated sideband to a call negotiated elsewhere:
cargo run -p nanocodex-examples --bin realtime-external -- attach rtc_...
cargo run -p nanocodex-examples --bin lifecycle
cargo run -p nanocodex-examples --bin fork-conversations
cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin subagents -- \
  "Review the retry policy using whatever clean or context-bearing workers you need"
NANOCODEX_SUBAGENT_JSONL=1 cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin mcp
cargo run -p nanocodex-examples --bin secret-egress -- host
just build-vm-example
target/debug/vm-tools ROOTFS [GUEST_RUNTIME_BINARY_OR_EXT4]
just smoke-python
just smoke-wasm-node
npm run subagents --prefix examples/node -- "Review the JS API"
just build-react-example
npm ci --prefix examples/privy
npm test --prefix examples/privy
npm run build --prefix examples/privy
npm ci --prefix examples/better-auth
npm test --prefix examples/better-auth
npm run build --prefix examples/better-auth
just build-rivet-example
just build-cloudflare-example
npm run check --prefix js/egress
just build-vercel-example
```

`voice` is the dead-simple non-TUI desktop consumer. It uses the same
`VoiceSessionBuilder` as the production TUI, owns the default microphone and
speaker directly in Rust, prints completed transcripts, and logs the retained
coding agent's ordered events. Spoken coding follow-ups atomically steer work
that is still running; speech while idle starts a new turn. It supports the
default devices on macOS and Windows.

`realtime-pipe` demonstrates the lower, device-neutral boundary. Stdin and
stdout are raw 24 kHz mono signed-16-bit little-endian PCM, so capture,
playback, files, sockets, `ffmpeg`, or another media stack can be composed
without Nanocodex owning a device. The desktop and pipe examples are two thin
adapters over the same typed Realtime events and retained agent lifecycle.
`realtime-external` demonstrates Codex's caller-owned WebRTC modes. `offer`
returns answer SDP before the authenticated sideband finishes joining;
`attach` joins an existing V1 call without creating or reconfiguring it. Set
`NANOCODEX_REALTIME_V3=1` for Frameless V3. In both modes the caller owns media
and closing Nanocodex detaches only the sideband. Lines on stdin append user
text; prefix a line with `/say ` to append speakable context.
Both use the shared Codex/ChatGPT subscription credentials at
`$CODEX_HOME/auth.json` or `~/.codex/auth.json`; `NANOCODEX_AUTH_FILE` overrides
that path. Run `nanocodex auth login` once if the shared credential does not
exist. Business and Enterprise hosts can instead set a persistent
`CODEX_ACCESS_TOKEN=at-...` without a browser login.

The other command-line examples use `OPENAI_API_KEY` by default. The browser
example instead asks the
embedding application for an already-authorized Responses WebSocket URL;
standard browser WebSockets cannot attach the upgrade authorization header.

`vm-tools` does not call the model. It proves all VM-backed standard workspace
tools against one retained guest and accepts either a directory root containing
`/usr/local/bin/nanocodex-vm-guest` or an ext4 root plus a guest-runtime ELF or
read-only runtime image. A runtime ELF is packed into a temporary ext4 image,
and a supplied ext4 root is reflinked or sparse-copied into a private per-run
disk before boot. On macOS, `just build-vm-example` also applies the required
Hypervisor entitlement.

`subagents` exposes generic `spawn_agent`, `fork_agent`, and `prompt_agent` Code
Mode tools; its Rust host contains no worker graph. The parent model decides the
orchestration topology and follow-ups from the goal. Initial workers return an
`agent_id` with their attributed report; `prompt_agent` sends later turns
through that child's retained session. `tools_factory` reinstantiates
agent-relative handlers with a weak `AgentHandle` for every driver. Its
`spawn()` method reuses private builder configuration without inheriting
conversation history, while `fork()` targets the agent that actually invoked
the tool.
The example prints only the final root answer by default. Set
`NANOCODEX_SUBAGENT_JSONL=1` to emit each child's lifecycle JSONL to stderr;
the records retain their native request IDs and sequence numbers without a
custom merged-event protocol.

The MCP example defaults to the public OpenAI documentation MCP. Override
`NANOCODEX_MCP_URL` for another Streamable HTTP server and set
`NANOCODEX_MCP_BEARER_TOKEN` when it requires bearer authentication.

`secret-egress` runs a real Nanocodex turn whose Code Mode cell fans out curl
commands through an authenticated host-owned proxy. Set `OPENAI_API_KEY`,
`NANOCODEX_SECRET_UPSTREAM`, and `NANOCODEX_SECRET_VALUE`; the model and its
commands receive only `DEMO_SERVICE_BASE_URL` and the public
`DEMO_SERVICE_TOKEN` placeholder. Use `NANOCODEX_SECRET_STRESS_REQUESTS` to
change the default eight-way `Promise.all` fanout. The same example runs tools
inside the retained VM while model traffic remains on the host:

```sh
cargo run -p nanocodex-examples --bin secret-egress -- \
  vm ROOTFS [GUEST_RUNTIME_BINARY_OR_EXT4]
```

VM mode provisions the proxy's public CA and child environment through a
provider-neutral `EgressLease`. It uses the default libkrun TSI network, under
which the host loopback proxy is reachable from guest workspace commands.
