# Parallel web search

This standalone Node example gives a Nanocodex agent web search and page fetching
through [Parallel Search MCP](https://docs.parallel.ai/integrations/mcp/search-mcp).
The public Streamable HTTP endpoint requires no Parallel API key. Anonymous
usage has rate limits; this example does not configure paid access or retries.

Use Node 22.13 or newer. From this directory:

```sh
npm ci
export OPENAI_API_KEY=...
npm start -- "What does Nanocodex do? Find its repository and cite a source."
```

`OPENAI_API_KEY` authenticates the model, which has its own inference costs.
The example discovers the remote tools with `tool_search`, searches the web,
and fetches a relevant page before answering. Tool selection is model-driven.
It uses the published `nanocodex` 0.6.7 Node runtime, so no local Rust or WASM
build is needed.

To reuse the opt-in configuration in another application, pass `parallelMcp`
from `index.mjs` as the `mcp` option to `Agent.create`. When combining servers,
merge it with your application's existing `mcp` entries. The example closes its
turn, result, and agent session after returning the final answer.
