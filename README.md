# Nanocode

Nanocode gives you **frontier embedded agents** through a stable Rust SDK.
We implement the agent backends ourselves:

- **Codex** — OpenAI.
- **Claude Code** — Anthropic.
- **Grok Build** — xAI. Coming soon.

## Get started

Package names, executables, and install URLs still use `nanocodex` while the
rename is in progress.

### CLI

On Apple Silicon macOS or x86-64 glibc Linux:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash
```

On x86-64 Windows 10 or 11:

```powershell
irm https://nanocodex.paradigm.xyz/install.ps1 | iex
```

The interactive installer walks through account sign-in and connecting the
machine as a Hand. Connect your model provider in the account's Connections
screen, then run `nanocodex2` for a cloud agent. To sign in again, use
`nanocodex2 login`.

For a local agent, run `nanocodex auth login`, then `nanocodex`. For Claude,
run `nanocodex --claude auth login`, then `nanocodex --claude`. Local provider
sign-in and cloud account sign-in are separate.

See the [managed CLI guide](bin/nanocodex/nanocodex2/README.md),
[Mac app](macos/README.md), and [iPhone and iPad app](apple/README.md) for details.

### Embed an agent

Choose a backend; it sets up the agent and its built-in tools. These examples
use the current directory as the workspace. Set `OPENAI_API_KEY` for Codex, or
set `ANTHROPIC_API_KEY` and use the commented line for Claude Code.

<table>
<tr><th>Rust</th><th>JavaScript (Node.js)</th></tr>
<tr>
<td valign="top">

```sh
cargo add nanocodex
cargo add tokio --features macros,rt-multi-thread

# For Claude Code:
# cargo add nanocodex --features claude
```

```rust
use nanocodex::{Backend, Nanocodex};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = Backend::codex(env::var("OPENAI_API_KEY")?)?;
    // let backend = Backend::claude(env::var("ANTHROPIC_API_KEY")?)?;

    let (agent, _events) = Nanocodex::builder(backend)
        .build()?;

    let turn = agent
        .prompt("Read the README and summarize this project.")
        .await?;
    let result = turn.await?;
    println!("{}", result.final_message());

    agent.shutdown().await?;
    Ok(())
}
```

</td>
<td valign="top">

```sh
npm install nanocodex
```

```js
import { Agent, Backend } from "nanocodex/node";

const backend = Backend.codex({
  apiKey: process.env.OPENAI_API_KEY,
});
// const backend = Backend.claude({
//   apiKey: process.env.ANTHROPIC_API_KEY,
// });

const agent = await Agent.create({ backend });

const turn = agent.turn.prompt({
  input: "Read the README and summarize this project.",
});
const result = await turn.result();
console.log(result.finalMessage);

result.dispose();
turn.dispose();
await agent.session.shutdown();
```

</td>
</tr>
</table>

Backend selection can live in a `match`, a configuration loader, or a function.
The rest of your agent code stays the same. Each backend owns its native tool
catalog; you can customize it when your application needs to.

Node.js requires 22.13+. The examples above use local tools. For browser and
Worker hosts, see the [JavaScript guide](js/nanocodex/README.md),
[Node example](examples/node/README.md), and
[React and Vite example](examples/react-vite/README.md).

The SDK provides prompts, streaming events, conversation history, steering,
cancellation, and cleanup. Your app owns the interface, storage, and
permissions. You can also run Codex and Claude together and have them delegate
to each other as subagents.

See the [Rust guide](crates/nanocodex/README.md),
[JavaScript guide](js/nanocodex/README.md), or
[Python bindings](py/bindings/README.md). The [examples](examples/README.md) cover
custom tools, subagents, voice, browser apps, and cloud deployments.

## Model–harness co-design

We believe the model and its harness are one system. The tools, system prompt,
context management, compaction, and prompt caching all affect how well the model
works. Our goal is to give each model the exact tools and behavior it gets in
its original agent, embedded in software you can build yourself.

Each backend implements its original agent's tool interfaces: Codex tools for
Codex, Claude Code tools for Claude Code. We aim to match the names, argument
schemas, and behavior the model expects. Each also keeps its own model protocol
and conversation history behind the SDK's shared lifecycle API.

This is our difference from [pi-mono](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/README.md).
Pi provides its own extensible harness over a
[unified model API](https://github.com/badlogic/pi-mono/blob/main/packages/ai/README.md).
Nanocode implements the original agent harnesses and makes them embeddable.

Claude support is newer and still catching up with Claude Code; see the
[tool coverage](docs/CLAUDE_TOOL_MATRIX.md) and
[current capabilities and limits](docs/CLAUDE_MANAGED.md#tools-durability-and-limits).
The [eval tooling](crates/experimental/nanocodex-eval/README.md) runs tasks against
Nanocode and upstream harnesses, keeps their traces, and compares the results.

## Durability and WASM

Once we have a backend implemented, it can use the same durability and WASM
infrastructure. We own the agent's execution state, so we can checkpoint and
restore it through
[`nanocodex-durability`](crates/nanocodex-durability/README.md). With a persistent
store, the agent can recover its execution after a process restart. The WASM
build lets us run the Rust agent in a browser, Node.js, or a serverless runtime.

This makes the "harness outside the sandbox" architecture straightforward. The
agent loop can live in a durable cloud runtime and send tool calls to whichever
machine should execute them. The agent's lifetime is independent of a particular
sandbox or connected client.

Our hosted implementation runs on Cloudflare Workers and Durable Objects. It has
a persistent filesystem and [Just Bash](https://github.com/vercel-labs/just-bash)
for file work, HTTP requests, and shell commands that do not need a native
process. It attaches a runner when it needs a compiler, installed software, or a
desktop. Research and API work can run without keeping a VM alive.

## Hands

A **Hand** is a runner on a device. It gives the agent access to that device's
files, installed programs, and, on supported platforms, its screen. Hands can run
on your laptop, desktop, server, or a VM. Each connects outbound to the hosted
agent, so you do not need to expose a port on your computer.

Commands are routed by working directory. The agent can run `cargo test` on your
Linux box and use software installed on your Mac in the same conversation.
Hands provide the execution environments; the agent loop stays in the cloud.
A device needs to be online for work to run there.

Part of the inspiration was my dad's older Windows machines, each connected to
measurement hardware and running proprietary software. I wanted one agent to
coordinate the machines, run experiments, and collect logs.

We build our terminal, web, Mac, and iPhone and iPad apps on these pieces. You can
start a task on your phone and check the same conversation on your Mac. Closing
the app does not stop the cloud agent. The hosted platform adds account
connectors, persistent memory, scheduled tasks, and a credential vault.
[Connect](docs/connect-mcp.md) exposes approved capabilities to other agents
through MCP.

## Work on Nanocode

The main pieces are in `crates/` (Rust libraries), `js/` (bindings, web apps, and
Workers), `bin/nanocodex/` (terminal clients), `macos/`, and `apple/`.

See [the development guide](docs/development.md) to build and run the web stack,
and [AGENTS.md](AGENTS.md) for contribution and testing conventions. Native app
builds are covered in the [Mac](macos/README.md) and [iOS](apple/README.md) guides.

## License

MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
