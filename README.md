<div align="center">

<h1>Nanocodex</h1>

<p><strong>The coding agent is the library.</strong></p>

<p>
Embed the complete OpenAI Responses loop—retained sessions, typed history,
tools, Code Mode, branches, events, retries, and cleanup. Keep your interface,
data, memory, infrastructure, and policy.
</p>

[![CI](https://img.shields.io/github/actions/workflow/status/gakonst/nanocodex/ci.yml?branch=master)][ci]
[![Crates.io](https://img.shields.io/crates/v/nanocodex.svg)][crates]
[![Docs.rs](https://img.shields.io/docsrs/nanocodex)][docs]
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)][license]

**[Rust](#rust-start-here)** · **[JavaScript](#javascript-node-browser-and-wasm)** ·
**[Python](#python)** · **[Capabilities](#one-agent-owned-end-to-end)** ·
**[Desktop apps](#desktop-apps)** ·
**[Evaluation](#evaluation-is-a-product-boundary)** ·
**[Deployments](#deployment-proofs)** · **[Status](#what-is-stable)**

[ci]: https://github.com/gakonst/nanocodex/actions/workflows/ci.yml
[crates]: https://crates.io/crates/nanocodex
[docs]: https://docs.rs/nanocodex
[license]: LICENSE-MIT

</div>

Nanocodex is a headless, library-first SDK for building products around one
deliberately supported OpenAI coding-agent stack. It is not a provider
abstraction and it is not an app server. The public product is an embeddable
agent with an owned lifecycle; the native CLI/TUI, browser apps, Python and
JavaScript packages, durable actors, sandboxes, voice client, and evaluation
harness are consumers that prove the same contract.

The important difference from assembling a model client and a loop is what the
caller does **not** have to rebuild:

- no passing previous messages, response IDs, or tool results back on every
  turn;
- no separate state machine for prompt ordering, steering, compaction,
  reconnect replay, or partially completed responses;
- no coupling between receiving a typed result and consuming an event stream;
- no orphaned shell sessions or subprocess trees when a turn is cancelled; and
- no second orchestration runtime when an agent forks or delegates work.

The interface is deliberately not part of that list. Consume ordered typed
events in a native TUI, wterm, xterm.js, React, logs, or something that only
your product could have. The included renderers are complete consumers, not a
UI protocol every embedding must adopt.

## Desktop apps

Nanocodex has a [native SwiftUI/AppKit macOS app](macos/README.md) and an
[Electron app](js/desktop/README.md). Both use the managed-agent API, persistent
sidebar or top tabs, streamed conversations, and local, VM, and cloud Hands.
Choosing a folder and sending automatically connects compute for that thread.
Their shared transport and Hand lifecycle live in
[`@nanocodex/desktop-runtime`](js/desktop-runtime/README.md).

Use `pnpm dev:desktop` for Electron development. Build its app bundle with
`pnpm --filter @nanocodex/desktop package`; build Swift with `pnpm build:macos`
after preparing the documented bundled Node runtime. Each app's README describes
account setup, native controls, and real-service verification.

## Install

Choose the host language; each path runs the Rust-owned agent lifecycle. The
Rust crates and core JavaScript `nanocodex` binding are registry releases. The
Python binding and JavaScript companion packages under `js/` are currently
built from the repository checkout.

```sh
# Rust
cargo add nanocodex

# Node.js 22.13+
npm install nanocodex

# Python 3.11+ (from a checkout)
uv venv --python 3.11 py/bindings/.venv
uv pip install --python py/bindings/.venv/bin/python 'maturin>=1.9,<2'
VIRTUAL_ENV="$PWD/py/bindings/.venv" \
  py/bindings/.venv/bin/maturin develop --manifest-path py/bindings/Cargo.toml
```

The Node.js 22.13+ line is the published package's consumer floor. Developing
from this repository uses Rust 1.97, the `wasm32-unknown-unknown` target,
`wasm-bindgen-cli` 0.2.126, Node.js 24 from `.node-version`, and pnpm 11.25.0
from the root `packageManager` field:

```sh
rustup toolchain install 1.97
rustup target add wasm32-unknown-unknown --toolchain 1.97
cargo +1.97 install --locked wasm-bindgen-cli --version 0.2.126
corepack enable
pnpm install --frozen-lockfile
pnpm build
pnpm dev
```

`pnpm dev` starts the complete Turbo stack through Portless. Local HTTPS needs
one-time certificate trust, and binding port 443 may also need administrator
approval on macOS. `PORTLESS_PORT=1355 pnpm dev` avoids the privileged bind and
serves the same names with `:1355` appended. Portless's proxy is user-global,
while app routes, processes, and Wrangler state remain isolated per checkout
and worktree.

Or install the native CLI/TUI on Apple Silicon macOS or x86-64 glibc Linux:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash
nanocodex
```

The CLI is a production consumer and a useful way to try the agent, not a
process protocol that applications must adopt. See
[`bin/nanocodex`](bin/nanocodex), the [examples index](examples/README.md), and
the [release switcher documentation](bin/nanocodex/src/update.rs).

## Rust: start here

Build one agent, submit ordered prompts through its cheap handle, and await a
typed result:

```rust,no_run
use nanocodex::{Nanocodex, OpenAi};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
    let (agent, _events) = Nanocodex::builder(openai)
        .instructions(
            "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
        )
        .workspace(std::env::current_dir()?)
        .build()?;

    let turn = agent.prompt("Find and fix the failing parser test.").await?;
    let result = turn.await?;
    println!("{}", result.final_message());

    agent.shutdown().await?;
    Ok(())
}
```

The first `await` accepts and orders the prompt. The returned `Turn` is both an
independently awaitable future for `TurnResult` and an optional per-turn event
stream. The separate `AgentEvents` value is the session-wide stream; neither
stream has to be drained for the result to complete.

Follow-on prompts reuse the same retained typed history, persistent Responses
WebSocket, response chain, cache identity, tools, Code Mode worker, and shell
sessions. `agent.clone()` is a constant-time command capability to that same
session. `shutdown()` cancels unfinished work and joins model, tool, transport,
and process cleanup.

The runnable source is [`examples/minimal.rs`](examples/minimal.rs). For event
streaming, steering, cancellation, clean spawning, historical forks, and
snapshots, see [`examples/lifecycle.rs`](examples/lifecycle.rs),
[`examples/follow_on.rs`](examples/follow_on.rs), and
[`examples/resume.rs`](examples/resume.rs).

Nanocodex currently supports the OpenAI `gpt-5.6-sol` family (`sol` is the
default, with `terra` and `luna` selections). It owns the typed Responses
WebSocket behavior for that family. An API-key gateway may prefix the on-wire
model identifier with `NANOCODEX_MODEL_ID_PREFIX`, but that does not create an
alternate-provider or arbitrary-model API.

## One agent, owned end to end

```text
your application
  ├─ cheap Nanocodex command handle ── prompt / steer / cancel / fork
  ├─ optional typed events ─────────── UI / persistence / telemetry
  ├─ caller-defined tools ──────────── your data and capabilities
  ├─ optional durability layer ─────── total state / recovery / stores
  └─ private driver
       ├─ ordered turns and typed committed history
       ├─ persistent OpenAI Responses WebSocket + typed retries
       ├─ Code Mode, MCP, shell sessions, and process cleanup
       └─ snapshots, compaction, branches, and task-tree children
```

### Sessions and history are authoritative

The private spawned driver is the sole mutable owner. A healthy follow-on sends
only the new delta with its private continuation checkpoint. If the socket is
replaced or a stored checkpoint is unavailable, Nanocodex drops the checkpoint
and safely replays complete client-owned typed history. Only completed
responses enter history: a failed partial response cannot execute a tool or
become the base of a later turn.

That gives an embedding a simple contract:

- `prompt()` is bounded admission, not a hidden full-turn wait;
- accepted prompts retain FIFO ordering even when their `Turn` handles are
  awaited elsewhere;
- steering joins at the next safe model boundary;
- cancellation targets one active or queued turn and terminates managed
  subprocess groups;
- snapshots contain the complete committed conversation and can resume in a
  fresh process; and
- token usage, cache behavior, and estimated USD cost arrive on the same typed
  terminal result.

The implementation boundaries are documented in
[`nanocodex-agent`](crates/nanocodex-agent/README.md) and
[`nanocodex-oai-api`](crates/nanocodex-oai-api/README.md). Applications that
only need a managed OpenAI conversation can use the lower-level
`OpenAi -> Session -> ResponseTurn -> Response` API without adopting agent
policy. The Responses client remains generic over the caller's concrete Tower
service, so deadlines, concurrency limits, tracing, load shedding, and circuit
breaking stay composable without introducing a second retry owner.

### Durable execution is optional and Rust-owned

`nanocodex-durability` adds a replace-in-place total execution state and
recovery policy, operation deduplication, committed-output replay, and session
checkpoints. It includes memory, SQLite, and Postgres stores, plus a
host-provided store contract whose only requirement is atomic load and
fenced compare-and-replace. Rust owns the state format and every recovery decision.
Model calls, warmup, compaction, and tools share one effect admission result:
execute when no output is committed, or replay the exact committed output.
Unfinished effects are deliberately at-least-once and may be billed or applied
again after recovery. Live attempts are fenced in-memory capabilities, not a
second durable state machine.

The layer implements the agent's neutral execution-policy seam; the core agent
does not depend on it. Lower-level consumers can use `DurableSession` directly
with caller-owned operation, step, checkpoint, and output types. It currently
ships from repository source; read the
[durability guide](crates/nanocodex-durability/README.md) and pin a Git revision
when adopting it outside this workspace.

### Tools, Code Mode, and MCP

Tools are caller-owned capabilities, not callbacks hidden behind a global
runtime. Register the standard workspace set, implement the typed `Tool`
contract, or write a Rust function with `#[tool]`:

```rust,no_run
use nanocodex::{Nanocodex, OpenAi, Tools, tool};

#[tool(description = "Multiplies two signed integers.")]
async fn multiply(left: i64, right: i64) -> Result<i64, &'static str> {
    left.checked_mul(right).ok_or("integer overflow")
}

# fn build(openai: OpenAi) -> Result<(), Box<dyn std::error::Error>> {
let tools = Tools::builder().without_defaults().tool(multiply).build()?;
let (_agent, _events) = Nanocodex::builder(openai).tools(tools).build()?;
# Ok(())
# }
```

The default native workspace runtime supplies bounded `exec_command`, retained
`write_stdin` sessions, Rust-verified `apply_patch`, `view_image`, planning,
web search, and image generation. Code Mode presents one compositional
JavaScript execution tool to the model; inside a cell, ordinary code can loop,
branch, fan out with `Promise.all`, and call typed tools through
`await tools.<name>(...)`. The runtime bounds code, tool output, process output,
and cancellation while keeping the model-facing schema compact.

MCP is part of the native tools crate rather than a separate agent runtime.
Stdio and Streamable HTTP servers are discovered in the background; deferred
tools remain out of the initial model prefix, are found with BM25
`tool_search`, and become callable by their canonical
`mcp__<server>__<tool>` names from Code Mode. OAuth persistence, allow/deny
lists, bounded concurrent startup, hot reload, and caller-owned clients live at
that boundary.

Read [`crates/nanocodex-tools`](crates/nanocodex-tools/README.md), run
[`examples/custom_tool.rs`](examples/custom_tool.rs), or start the complete MCP
example in [`examples/mcp.rs`](examples/mcp.rs):

```sh
OPENAI_API_KEY=... cargo run -p nanocodex-examples --bin custom-tool
OPENAI_API_KEY=... cargo run -p nanocodex-examples --bin mcp
```

### Branches, snapshots, and subagents

Branching is a lifecycle primitive, not cloned mutable state:

- `spawn()` creates a clean agent with the same private builder configuration
  and no conversation history;
- `fork()` creates an independent session from the latest safe committed
  boundary;
- `fork_from(&completed_turn)` pins an exact historical checkpoint; and
- `SessionSnapshot` serializes authoritative committed history for later
  process or actor resumption without exposing provider response IDs.

Forked drivers get their own socket, prompt queue, tools, and cancellation
domain. Shared immutable history makes local fork-and-append constant-time, and
the retained provider checkpoint keeps healthy branch requests delta-sized.
See the runnable [`fork-conversations`](examples/fork_conversations.rs) example
and the [stored-checkpoint measurements](benchmarks/fork_results.md).

[`nanocodex-subagents`](crates/nanocodex-subagents/README.md) is an optional
extension above the core. It installs a shared task-tree registry and seven
agent-relative tools—spawn, structured result submission, directed messaging,
listing, waiting, interrupting, and closing—fresh for every root, child, and
fork. The root owns recursive cleanup. Native and WASM applications use the
same Rust implementation; the core agent crate does not depend on it and does
not become a general scheduler.

This lets the model synthesize a temporary orchestration program in Code Mode
without requiring the host to declare a DAG. The executable examples are
[`examples/subagents.rs`](examples/subagents.rs) and
[`examples/node/subagents.mjs`](examples/node/subagents.mjs).

## JavaScript: Node, browser, and WASM

The repository's `nanocodex` package exposes viem-style `Agent`, `Actions`, and
`Transport` namespaces for Node and browser hosts. The current registry release
contains the core root, Node, browser, and WASM entrypoints; newer browser-tool
and subagent exports shown below currently require a pinned checkout. The
Rust/WASM engine still
owns prompt ordering, history, tool calls, branching, snapshots, and cleanup;
JavaScript owns WebSocket creation, credentials, UI, persistence, and ordinary
application tools.

### Node.js

```js
import { Agent, Transport } from "nanocodex/node";

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  instructions: "You are a coding agent. Make focused changes and verify them.",
  workspace: process.cwd(),
  tools: [{
    name: "lookup_issue",
    description: "Return one issue by number.",
    parameters: {
      type: "object",
      properties: { number: { type: "integer" } },
      required: ["number"],
      additionalProperties: false,
    },
    handler: ({ number }) => issueTracker.get(number),
  }],
});

try {
  const turn = agent.turn.prompt({ input: "Fix issue 42." });
  try {
    const result = await turn.result();
    try {
      console.log(result.finalMessage, await result.usage());
    } finally {
      result.dispose();
    }
  } finally {
    turn.dispose();
  }
} finally {
  await agent.session.shutdown();
}
```

Use `Transport.chatGpt({ subscription })` for a caller-owned ChatGPT
subscription or `Transport.mpp({ session })` for a caller-owned MPP session.
Transport constructors are explicit immutable configurations; Nanocodex does
not infer provider portability from them. See the complete
[JavaScript guide](js/nanocodex/README.md) and runnable
[Node session](examples/node/session.mjs).

### A complete coding workspace in a browser

The browser entrypoint runs the same Rust agent in a Worker. It can open a
persistent origin-private filesystem (OPFS) workspace and compose a lazy
WASM-backed shell with Python through Pyodide, C/C++ through wasm-clang,
browser Git, bounded file commands, web and image tools, artifacts, and the
optional Rust subagent tree:

```js
import { Agent, Subagents, Transport } from "nanocodex/browser";
import { browser } from "nanocodex/tools/browser";

const runtime = await browser({
  threadId: "project-42",
  recentImages,
  rememberImage,
});

const agent = await Agent.create({
  transport: Transport.hostManaged({
    websocketUrl: "/api/responses",
    createWebSocket: (url) => new WebSocket(url),
  }),
  filesystem: runtime.filesystem,
  instructions: runtime.instructions,
  executionEnvironment: {
    currentDate: "2026-08-19",
    timezone: "America/Los_Angeles",
    projectInstructions: runtime.projectInstructions,
  },
  tools: [...runtime.tools, ...Subagents.create({ maxConcurrency: 8 })],
});
```

This is a browser-native workspace: files persist across page, Worker, and
agent restarts, and coding can happen without provisioning a server-side
sandbox. It is **not** a claim that OPFS or in-browser execution is an
untrusted-code security sandbox. Products that need stronger isolation should
provide remote caller-defined tools or use one of the VM/container consumers
below.

Browser WebSockets cannot attach OpenAI's authorization header, so
`Transport.hostManaged` expects an application-authorized same-origin relay;
the API key stays out of the page and WASM artifact. The one-file
[`browser-cdn`](examples/browser-cdn/README.md) consumer needs no bundler or
framework. The [`React + Vite`](examples/react-vite/README.md) example keeps
one persistent agent in a module Worker and forwards ordered events into React.

The package also exposes composable browser-safe web search, image generation,
public Parquet/JSONL/Hugging Face dataset queries, and live React artifact
tools. Read their exact contracts and bounds in
[`js/nanocodex/README.md`](js/nanocodex/README.md#standard-web-and-browser-tools).

### Bring any terminal or product interface

`agent.events.watch()` and the intentionally narrow `nanocodex-react` Context
and hooks expose ordered typed data independently from `Turn.result()`. The SDK
does not make a DOM transcript or terminal emulator authoritative. UI
frameworks and terminal renderers consume Agent events directly and remain
application code. Those events can feed wterm, xterm.js, Ink, a design-system
transcript, persistence, or telemetry without adding a UI protocol to the SDK.

The application owns event reduction, ANSI presentation, line editing, prompt
history, steering, cancellation, and terminal subscriptions alongside the
agent, tools, transport, persistence, authorization, and shutdown. The
[Vercel Workflow example](examples/vercel-workflows/README.md) demonstrates
that application-owned seam with a durable replay journal and `@wterm/react`;
its separate Sandbox PTY remains a distinct shell-byte lifecycle.

## Python

The Python wheel embeds the native Rust runtime through PyO3. Blocking result
waits release the GIL; all agents in a process share one async runtime, while
each agent retains its own driver, WebSocket, history, Code Mode worker, and
cleanup boundary.

```python
import os
from nanocodex import Nanocodex

agent, events = Nanocodex(
    os.environ["OPENAI_API_KEY"],
    instructions="You are a coding agent. Preserve unrelated work.",
)

first = agent.prompt("Remember the identifier PYO3_17.").result()
second = agent.prompt("Return the identifier I asked you to remember.").result()
print(second.final_message)

branch, branch_events = agent.fork_from(first)
print(branch.prompt("What was the identifier?").result().final_message)

branch.shutdown()
agent.shutdown()
```

Python exposes typed event envelopes, steering, per-turn cancellation,
compaction, thinking and fast-mode policy, `spawn`, `fork`, `fork_from`,
snapshots, and resume. Start with the [Python guide](py/bindings/README.md) and
the runnable [`examples/python`](examples/python) consumers.

## Web search and a real browser agent

Web search and browser automation are different capabilities. The stable tool
runtime includes the bounded OpenAI/Codex-compatible web-search boundary, and
JavaScript hosts can use the matching `web()` factory. Applications decide
which network tool to install and where credentials live.

For full deterministic Chromium control, the unpublished experimental
[`nanocodex-browser`](crates/experimental/nanocodex-browser/README.md) crate
provides an ordinary deferred `BrowserTool`. It supports semantic/CSS/role/text
targets, tabs and frames, bounded DOM/layout/style and network inspection,
screenshots and pixel diffs, PDFs, traces, video, accessibility, performance,
coverage, heap and React diagnostics, uploads, and virtual passkeys. The full
roughly 67 KiB action contract stays runtime-only until discovered, adding no
browser schema bytes to the initial model request.

```rust,ignore
use nanocodex::{Nanocodex, OpenAi, Tools};
use nanocodex_browser::BrowserTool;

# fn build(openai: OpenAi) -> Result<(), Box<dyn std::error::Error>> {
let tools = Tools::builder().provider(BrowserTool::new()?).build()?;
let (_agent, _events) = Nanocodex::builder(openai).tools(tools).build()?;
# Ok(())
# }
```

Local CLI mode uses a dedicated persistent browser profile by default so login
and site state survive Nanocodex restarts; `--browser-profile=temporary` opts
back into one disposable profile with host-cookie import. The profile is still
private browser state, not an OS sandbox. The optional
`BrowserVm` composition starts an unprivileged headed Chromium under Xvfb in a
disposable libkrun guest and closes CDP, Chromium, networking, VMM, and disk as
one owned lifecycle. Run the source in
[`examples/browser_agent.rs`](examples/browser_agent.rs).

## VMs, sandboxes, and voice

These are application-owned adapters over the same agent session, not alternate
agent runtimes.

### Retained VM workspaces

The experimental, unpublished
[`nanocodex-vm`](crates/experimental/nanocodex-vm/README.md) crate owns the
libkrun boundary. An application launches one private workspace, retains it
across sequential turns, and swaps only `exec_command`, `write_stdin`,
`apply_patch`, and `view_image` for guest-backed implementations with the same
model-visible names and schemas. Web search, image generation, and planning can
remain on the host.

Immutable OCI/Dockerfile roots are content-addressed; each retained session
gets a writable private ext4 copy, while high-fanout attempts can use a fresh
sparse OverlayFS upper. The non-cloneable workspace is the shutdown capability,
and clone-cheap tool handles share its filesystem and interactive shells.
Cancellation, output limits, process groups, egress leases, VMM process, guest
runtime, and disk cleanup all have explicit owners.

The CLI can exercise the same boundary:

```sh
just build-vm-guest
nanocodex run "inspect the repository" \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
```

See the [VM operations guide](docs/VM.md) for image preparation, libkrun,
Linux KVM, macOS signing, networking, and egress.

### Voice is another input to the retained agent

The experimental, unpublished
[`nanocodex-voice`](crates/experimental/nanocodex-voice/README.md) crate connects
GPT Realtime to an existing `Nanocodex` session. Speech while idle starts an
independently awaitable coding turn; speech while work is active atomically
steers it at the next safe model boundary. Typed work is mirrored back to the
voice session, while stopping audio does not silently cancel coding work.

The default-device adapter supports macOS and Windows. The lower device-neutral
Realtime boundary reads and writes raw 24 kHz mono PCM16, so other applications
can own capture, codecs, sockets, or playback:

```sh
nanocodex auth login
cargo run -p nanocodex-examples --bin voice
cargo run -p nanocodex-examples --bin realtime-pipe \
  < microphone.pcm > speaker.pcm
```

Runnable sources: [`examples/voice.rs`](examples/voice.rs) and
[`examples/realtime_pipe.rs`](examples/realtime_pipe.rs).

## Evaluation is a product boundary

Evals are not a score pasted onto the end of development. They are how the
session, tool, VM, event, and cleanup contracts are exercised together.

The experimental, unpublished
[`nanocodex-eval`](crates/experimental/nanocodex-eval/README.md) layer runs every
attempt and its canonical verifier in a microVM. `eval add` fingerprints tasks
and pre-materializes one immutable SQLite row for every
task/treatment/repetition. Workers atomically claim exactly one row; verifier
pass/fail is terminal, while infrastructure failure is retained in attempt
history and safely returns work for another claim. A claim ID fences late
writes.

The durable ledger—not a TOML recipe, controller memory, or inferred queue—is
the authority. Each arm receives a fresh writable overlay. The retained output
contains raw JSONL, typed trajectories, model API exchanges and summaries,
usage, verifier reward/stdout/stderr, and exact treatment coordinates. External
harnesses such as stock Codex are independent coordinates using the same task,
isolation, capture proxy, verifier, and evidence format; differential reports
are offline joins rather than special comparison behavior in the agent.

```sh
# Materialize an immutable generation from the repository recipe.
nanocodex eval add local-smoke --recipe local-smoke

# Inspect state without deriving or adding work.
nanocodex eval status local-smoke --json

# Atomically claim and run one row, or let the benchmark consumer size workers.
nanocodex eval run local-smoke
nanocodex eval benchmark local-smoke
```

Task packages are ordinary inputs with agent instructions, a starting
environment, hidden deterministic tests, and an oracle used to validate the
task itself. Explore [`tasks/`](tasks), the
[history-derived suite](evals/history-derived/README.md), and the
[comparison-plan contract](evals/harbor-comparisons/README.md). Benchmark tasks
and verifiers are never modified to make Nanocodex pass.

## Deployment proofs

Nanocodex does not impose a generic app-server protocol. These applications
show how different products can own authentication, idempotency, durable state,
client projection, and sandbox policy while reusing one agent lifecycle:

| Consumer | What it proves |
| --- | --- |
| [Native CLI and Ratatui TUI](bin/nanocodex) | Interactive sessions, JSONL one-shot adapter, branching UI, MCP, browser, VM, voice, and full lifecycle cleanup. |
| [Static browser CDN page](examples/browser-cdn/README.md) | One HTML file runs the Rust/WASM agent from the npm package with no framework, bundler, or install step. |
| [React + Vite Worker](examples/react-vite/README.md) | A browser Worker owns one persistent session and React consumes ordered events without reshaping the contract. |
| [Cloudflare managed agents + Multiplayer](js/managed/README.md) | Signed room objects add ordered N-human chat, bounded replay, a tool-free host-owned agent, and a global durable spend/allocation quota; provider credentials stay behind a private broker binding. |
| [Cloudflare credential broker](js/egress/README.md) | Two ordinary Workers use a private Service Binding for exact API-key or OAuth replacement and a singleton rotating Codex OAuth broker. |
| [Cloudflare fetch + MCP](examples/cloudflare-fetch-mcp/README.md) | CSP-safe QuickJS Code Mode, deferred remote MCP, and caller-owned paid transport inside a serialized Durable Object. |
| [Rivet Actor](examples/rivet-actors/README.md) | Durable SQLite snapshots and idempotent turns around the WASM driver, with an actor-owned AgentOS workspace and previews. |
| [Vercel Workflow actor](examples/vercel-workflows/README.md) | A Rust-owned journal between stateless steps, replayable multi-client streams rendered through a replaceable wterm agent UI, and a persistent caller-owned Vercel Sandbox with a separate ephemeral wterm operator shell. |
| [exe.dev](examples/exe-dev/README.md) | Both a retained native session inside a VM and the inverse: a host agent controlling one exact remote VM through narrow tools. |
| [Python](examples/python) and [Node](examples/node/README.md) | Thin language bindings over the same results, events, history, snapshots, branches, and shutdown semantics. |

These are reference consumers, not portability promises. Cloudflare, Rivet,
Vercel, exe.dev, and the native VM layer each keep their platform policy above
the stable crates.

## What is stable

“Experimental” below describes API stability and publication status, not a
second-class quality bar. Experimental crates remain workspace members and pass
the repository's formatting, Clippy, documentation, test, cancellation,
tracing, and benchmark gates; stable crates never depend on them.

| Surface | Status | Owner |
| --- | --- | --- |
| [`nanocodex`](crates/nanocodex/README.md) | Stable, published | Thin Alloy-style facade and canonical imports; no runtime implementation. |
| [`nanocodex-agent`](crates/nanocodex-agent/README.md) | Stable, published | Owned driver, turns/results/events, history policy, snapshots, compaction, branches, and cancellation. |
| [`nanocodex-durability`](crates/nanocodex-durability/README.md) | Stable, source/Git-only, optional | Total execution state, deduplication, recovery policy, staged outcomes, checkpoints, and memory/SQLite/Postgres/host stores. |
| [`nanocodex-oai-api`](crates/nanocodex-oai-api/README.md) | Stable, published | OpenAI auth, typed Responses and Realtime boundaries, persistent transports, managed context, retry, pricing, and Tower client. |
| [`nanocodex-tools`](crates/nanocodex-tools/README.md) | Stable, published | Tool contract, standard tools, shell/process lifecycle, Code Mode, deferred search, MCP, and remote dispatch. |
| [`nanocodex-subagents`](crates/nanocodex-subagents/README.md) | Source/Git-only optional workspace extension | Task-tree lifecycle and the seven canonical child-agent tools above the core. |
| [`nanocodex-observability`](crates/nanocodex-observability/README.md) | Stable, published, optional | Full-fidelity tracing and application-owned OpenTelemetry initialization. |
| [`nanocodex` for JavaScript](js/nanocodex/README.md) | Published headless core binding; narrow source companions | Node/browser hosts around the Rust/WASM agent, plus React hooks, Vite integration, and optional terminal presentation under [`js/`](js/README.md). Agent lifecycle remains headless and caller-owned. |
| [`nanocodex` for Python](py/bindings/README.md) | Source-distributed language binding | Native PyO3 consumer of the Rust-owned lifecycle, built and tested with Maturin. |
| [`nanocodex-browser`](crates/experimental/nanocodex-browser/README.md) | Experimental, unpublished | Deterministic Chromium control and optional headed browser VM. |
| [`nanocodex-vm`](crates/experimental/nanocodex-vm/README.md) | Experimental, unpublished | libkrun images, retained/ephemeral guests, and canonical VM-backed workspace tools. |
| [`nanocodex-voice`](crates/experimental/nanocodex-voice/README.md) | Experimental, unpublished | Opinionated desktop GPT Realtime voice-to-agent lifecycle. |
| [`nanocodex-egress`](crates/experimental/nanocodex-egress/README.md) | Experimental, unpublished | Authenticated loopback HTTP(S) proxy and application-owned outbound layers. |
| [`nanocodex-eval`](crates/experimental/nanocodex-eval/README.md) | Experimental, unpublished | VM-isolated attempts, durable SQLite work, verification, retained evidence, and differential coordinates. |

## Design boundaries

Nanocodex is intentionally narrow:

- one supported OpenAI coding-model family and the Responses WebSocket API;
- one owned agent lifecycle with client-owned typed history;
- caller-defined tools and application-owned policy;
- no provider/model portability layer;
- no generic JSON-RPC agent daemon or app-server protocol;
- no approval subsystem or compatibility framework; and
- no stable generic scheduler hidden inside the core agent.

The separation is what makes the SDK embeddable. A lower OpenAI client works
without the agent. Tools work without the CLI. Subagents compose above the
agent. VMs, browsers, voice, payment, durable actors, and evaluation remain
consumers with explicit owners.

## Repository map

```text
crates/
├── nanocodex/                  facade and prelude
├── nanocodex-oai-api/          OpenAI protocol, context, transport, Tower
├── nanocodex-tools/            tools, Code Mode, MCP, process runtime
├── nanocodex-agent/            owned agent lifecycle
├── nanocodex-subagents/        optional task-tree extension
├── nanocodex-observability/    optional tracing and OTLP setup
└── experimental/               browser, VM, voice, egress, eval
js/                             npm packages, account/Connect apps, Workers
py/                             native Python binding
bin/nanocodex/                  CLI and Ratatui product consumer
examples/                       native, language, browser, actor, sandbox proofs
evals/ and tasks/               deterministic evaluation inputs
benchmarks/                     retained measurements and regression gates
```

Further reading:

- [Facade API documentation](https://docs.rs/nanocodex)
- [Examples and runnable commands](examples/README.md)
- [Migration guide](docs/MIGRATING.md)
- [Responses + Tower design](docs/RESPONSES_TOWER.md)
- [Observability contract](docs/OBSERVABILITY.md)
- [Subagent design](docs/SUBAGENTS.md)
- [VM operations](docs/VM.md)
- [Benchmarks and executable regression gates](benchmarks/)

## License

Licensed under either the Apache License, Version 2.0 or the MIT License, at
your option.
