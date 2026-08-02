<div align="center">

<h1>Nanocodex</h1>

<p><strong>Building blocks for frontier OpenAI agents.</strong></p>

[![CI](https://img.shields.io/github/actions/workflow/status/gakonst/nanocodex/ci.yml?branch=master)][ci]
[![Crates.io](https://img.shields.io/crates/v/nanocodex.svg)][crates]
[![Docs.rs](https://img.shields.io/docsrs/nanocodex)][docs]
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)][license]

**[Install](#install)** · **[Agent API](#minimal-api-example)** ·
**[Thesis](#thesis)** · **[Components](#components)** ·
**[VM-backed tools](#vm-backed-tools)** · **[Documentation](#documentation)**

[ci]: https://github.com/gakonst/nanocodex/actions/workflows/ci.yml
[crates]: https://crates.io/crates/nanocodex
[docs]: https://docs.rs/nanocodex
[license]: LICENSE-MIT

</div>

## Install

Install the Nanocodex CLI on macOS or Linux:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash
```

Or add the Rust SDK to an application:

```sh
cargo add nanocodex
```

Switch the installed CLI between builds:

```sh
nanocodex update                 # latest stable release
nanocodex update 0.2.0           # exact release, including downgrades
nanocodex update --nightly       # latest nightly
nanocodex update --pr 50         # verified on-demand PR artifact
nanocodex update --path ./nanocodex  # trusted local binary
```

Downloaded builds are retained under `~/.nanocodex/versions`. Running
`nanocodex update 0.2.0` again switches to the cached binary without another
download. A stable launcher keeps `nanocodex update` available even while an
older binary is active, and `~/.nanocodex/current` points to the selected
version.

PR artifacts require an authenticated `gh` CLI and an already completed
on-demand artifact workflow for that PR.

## Minimal API Example

```rust,ignore
use nanocodex::{Nanocodex, OpenAi};

let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, mut events) = Nanocodex::builder(openai)
    .instructions(
        "You are a Rust coding agent. Make focused changes, preserve unrelated work, \
         and run relevant tests before finishing.",
    )
    .workspace(std::env::current_dir()?)
    .build()?;

let event_task = tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        eprintln!("event {}: {:?}", event.seq, event.kind);
        if event.kind.is_terminal() {
            break;
        }
    }
});

// Alternative: stream this turn's response as it arrives:
// use futures_util::StreamExt;
// use nanocodex::agent::events::{AgentEventData, AssistantEvent};
// let mut turn = agent.prompt("Find and fix the failing parser test.").await?;
// while let Some(event) = turn.next().await {
//     if let AgentEventData::Assistant(AssistantEvent::Delta(delta)) = event.data()? {
//         print!("{}", delta.text);
//     }
// }
let result = agent
    .prompt("Find and fix the failing parser test.")
    .await?
    .await?;

event_task.await?;
println!("{}", result.final_message());
```

The first `await` accepts and orders the prompt. The second waits for its typed
`TurnResult`. Follow-on prompts automatically reuse the agent's retained
history, WebSocket, tools, shell sessions, and prompt-cache identity.
`agent.clone()` is a cheap handle to that same session; the independently
returned `AgentEvents` stream is the session-wide event firehose.

Nanocodex supports `gpt-5.6-sol` (the default) and `gpt-5.6-luna`. Select the
model with `.model(Model::Luna)` when creating an agent. The model is fixed for
that thread: switching later would invalidate the provider checkpoint and
require an inefficient replay of the complete retained context.

## Voice: devices or Unix pipes

The non-TUI desktop example owns the default microphone and speaker directly
in Rust, using the same `VoiceSessionBuilder` as the production TUI:

```sh
nanocodex auth login # once; shares ~/.codex/auth.json with Codex
cargo run -p nanocodex-examples --bin voice
```

The lower adapter leaves device and media ownership outside Nanocodex. It reads
24 kHz mono PCM16 little-endian audio from stdin, writes the same format to
stdout, and keeps transcripts and agent events on stderr:

```sh
cargo run -p nanocodex-examples --bin realtime-pipe < microphone.pcm > speaker.pcm

# Equivalently, compose any live capture/decoder and playback/encoder:
capture-s16le | cargo run --quiet -p nanocodex-examples --bin realtime-pipe | play-s16le
```

Both retain one coding-agent session. A spoken request starts work while idle;
a follow-up received during that work atomically steers the active turn at its
next safe model boundary. Both use shared Codex/ChatGPT subscription auth, not
an API key. Set `NANOCODEX_AUTH_FILE` to override the normal Codex credential
path.

## Streaming dictation (experimental)

Run the interactive TUI with dictation enabled using either the selected
ChatGPT subscription or Platform API-key authorization:

```sh
nanocodex auth login # once; shares ~/.codex/auth.json with Codex
cargo run -p nanocodex-bin -- --dictation

# Or use Platform billing through an API key:
cargo run -p nanocodex-bin -- --api-key "$OPENAI_API_KEY" --dictation
```

API keys use OpenAI's dedicated Realtime transcription WebSocket. Subscription
credentials use the managed ChatGPT Realtime WebRTC path.

Press Space normally to insert a space immediately. While it is held, the
footer briefly shows `keep holding…`, then starts listening and streams speech
into the composer. A one-cell microphone level follows the streamed text at its
insertion point while the footer keeps listening visible beside any concurrent
agent activity. Release Space to finalize.

For longer dictation, start with a clear composer, type `/dictate`, and press
Enter; press Enter again to finalize and send (or steer an active turn), Tab to
finalize and queue the next prompt, or Escape to cancel. Submission occurs after
the transcript reaches its final form. Hold-Space preserves the existing draft
and inserts completed speech at the captured cursor. The equivalent environment
setting is `NANOCODEX_DICTATION=true`.

Hold-Space follows the terminal's ordinary key-repeat stream. Terminals that
report modifier release events also provide hold-right-Option as an alias.

## Thesis

### Small, excellent building blocks

Agent infrastructure is easier to understand and reuse when each piece has a
sharp owner and a useful API of its own. An OpenAI client should work without
an agent loop. Tools should work without a CLI. The high-level agent should
compose those pieces rather than hide another implementation of them.

Nanocodex makes a small number of deliberate choices—Rust, Tower, typed
protocols, owned lifecycle state, and builder APIs—then keeps the boundaries
boring.

### The model and harness are co-designed

We do not try to outsmart behavior that frontier models and Codex already make
explicit. Context management, `AGENTS.md`, compaction, cache identity, tool
shapes, continuation, reconnect replay, cancellation, and process cleanup are
parts of the model-facing contract.

Nanocodex carries those invariants into a smaller, library-first API while
leaving application policy with the caller.

### Evidence over intuition

Representative `cargo bench` workloads, OpenTelemetry traces, differential
tests, and end-to-end evals keep the harness honest. The goal is simple: normal
agent turns should be model- and network-latency bound, with token usage and
estimated USD cost visible at the same typed boundary as the result.

## Components

```text
nanocodex                         Alloy-style facade and prelude
├── agent                         nanocodex-agent
│   ├── oai                       nanocodex-oai-api
│   └── tools                     nanocodex-tools
│       └── macros                nanocodex-tools-macros
├── oai                           nanocodex-oai-api
├── tools                         nanocodex-tools
└── observability                 nanocodex-observability (optional)
```

The facade provides the canonical common imports. Each lower crate is also
designed to be useful directly, without importing the higher orchestration
layer.

### `nanocodex`

The thin facade reexports the golden agent path at the crate root and keeps
detailed APIs under `nanocodex::agent`, `nanocodex::oai`, and
`nanocodex::tools`. Its prelude contains only the common types needed to build
an agent.

[Facade guide](crates/nanocodex/README.md) ·
[API documentation](https://docs.rs/nanocodex)

### `nanocodex-agent`

The batteries-included lifecycle: an owned private driver, a cheap cloneable
`Nanocodex` handle, typed `Turn` and `TurnResult` values, and an optional event
stream. It owns prompt ordering, the tool loop, `AGENTS.md` discovery,
compaction timing, cancellation, snapshots, and branching through `spawn`,
`fork`, and `fork_from`.

Callers never pass previous messages, response IDs, or tool results back into
the agent.

[Agent guide](crates/nanocodex-agent/README.md) ·
[API documentation](https://docs.rs/nanocodex-agent)

### `nanocodex-oai-api`

The complete OpenAI boundary: API-key and ChatGPT authentication, typed
Responses protocol values, a persistent WebSocket transport, client-owned
context, continuation and replay, automatic pricing, and a generic Tower
client.

Its standalone `OpenAi -> Session -> ResponseTurn -> Response` path provides a
managed conversation without taking on agent policy. Custom Tower layers and
services remain concrete and nameable—no boxing or global client is required.

[OpenAI API guide](crates/nanocodex-oai-api/README.md) ·
[API documentation](https://docs.rs/nanocodex-oai-api)

### `nanocodex-tools`

The model-facing tool runtime: the `Tool` contract, heterogeneous `Tools`
registry, standard workspace tools, shell and process lifecycle, Code Mode,
deferred `tool_search`, remote dispatch, and MCP. MCP is always available on
native targets.

Applications can implement `Tool` directly or use the reexported `#[tool]`
macro. The separate `nanocodex-tools-macros` package exists only for Rust's
procedural-macro boundary.

[Tools guide](crates/nanocodex-tools/README.md) ·
[API documentation](https://docs.rs/nanocodex-tools)

### `nanocodex-observability`

Application-owned tracing and OpenTelemetry setup for the data already flowing
through the agent. It provides structured lifecycle, model, tool, usage, cost,
cache, and latency telemetry without changing the core runtime path.

Enable the facade's `observability` feature or depend on the component
directly.

[Observability guide](crates/nanocodex-observability/README.md) ·
[API documentation](https://docs.rs/nanocodex-observability)

### Experimental components

Components whose public contracts are still maturing live under
[`crates/experimental/`](crates/experimental/README.md):

| Package | Responsibility |
| --- | --- |
| [`nanocodex-voice`](crates/experimental/nanocodex-voice/README.md) | Desktop GPT Realtime audio and reusable voice-to-agent lifecycle |
| [`nanocodex-vm`](crates/experimental/nanocodex-vm/README.md) | VM lifecycle and images plus retained guest-backed workspace tools |

The CLI is a consumer of these crates. Voice and VM-backed tools remain thin,
opt-in adapters over the stable library contracts.

### CLI and language bindings

The CLI/TUI, Python package, Node/browser package, React bindings, and examples
are thin consumers of the same owned session API. They do not define a second
agent protocol.

[Examples](examples/README.md) · [JavaScript](js/README.md) ·
[Python](py/README.md) · [Web](web/README.md)

## VM-backed tools

Normal TUI and one-shot sessions keep host workspace tools by default. They can
instead route `exec_command`, `write_stdin`, `apply_patch`, and `view_image`
through one retained VM:

```sh
just build-vm-guest
nanocodex \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
nanocodex run "inspect the repository" \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /app
```

Directory roots may instead contain
`/usr/local/bin/nanocodex-vm-guest`. See the
[VM guide](docs/VM.md) for image preparation, lifecycle, egress, Linux
requirements, and macOS signing.

## Documentation

- [Facade API](https://docs.rs/nanocodex)
- [Migration from 0.2.x](docs/MIGRATING.md)
- [Examples](examples/README.md)
- [Benchmarks and retained measurements](benchmarks/)
- [VM-backed tools and egress](docs/VM.md)

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
