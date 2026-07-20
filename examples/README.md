# Nanocodex examples

All language consumers live at this repository boundary:

- Rust: `minimal.rs`, `follow_on.rs`, `lifecycle.rs`, `custom_tool.rs`, `subagents.rs`,
  `fork_conversations.rs`, `fork_checkpoint_bench.rs`, and `mcp.rs` are binaries
  in the `nanocodex-examples` package.
- Python: `python/` uses the native PyO3 binding.
- Node.js: `node/` uses the shared Rust/WASM package with a Node WebSocket host.
- Browser: `react-vite/` runs that WASM agent in a module Worker and renders its
  ordered events in React.

From the repository root:

```sh
cargo run -p nanocodex-examples --bin minimal
cargo run -p nanocodex-examples --bin lifecycle
cargo run -p nanocodex-examples --bin fork-conversations
cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin subagents -- \
  "Review the retry policy using whatever clean or context-bearing workers you need"
NANOCODEX_SUBAGENT_JSONL=1 cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin mcp
just smoke-python
just smoke-wasm-node
just build-react-example
```

The live programs require `OPENAI_API_KEY`. The browser example instead asks
the embedding application for an already-authorized Responses WebSocket URL;
standard browser WebSockets cannot attach the upgrade authorization header.

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
