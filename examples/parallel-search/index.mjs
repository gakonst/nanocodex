import { pathToFileURL } from "node:url";
import { Agent, Transport } from "nanocodex/node";

// No bearer token, OAuth store, payment adapter, or Parallel API key is needed.
export const parallelMcp = {
  parallel: {
    url: "https://search.parallel.ai/mcp",
    description: "Search the web and fetch pages with Parallel.",
    headers: { "User-Agent": "nanocodex-parallel-search-example/0.1.0" },
    supportsParallelToolCalls: true,
    startupTimeoutMs: 30_000,
    timeoutMs: 60_000,
  },
};

export async function searchWeb({ transport, input }) {
  const agent = await Agent.create({
    transport,
    thinking: "low",
    tools: {},
    mcp: parallelMcp,
    instructions: "Use tool_search to discover Parallel web search and fetch tools. Search for evidence, fetch a relevant page, then answer with source URLs.",
  });
  let turn;
  let result;
  try {
    turn = agent.turn.prompt({ input });
    result = await turn.result();
    return result.finalMessage;
  } finally {
    result?.dispose();
    turn?.dispose();
    await agent.session.shutdown();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const apiKey = process.env.OPENAI_API_KEY?.trim();
  if (!apiKey) throw new Error("Set OPENAI_API_KEY for the model. Parallel needs no API key.");
  const input = process.argv.slice(2).join(" ").trim()
    || "Find the Nanocodex repository and summarize its purpose, citing the source.";
  console.log(await searchWeb({ transport: Transport.openAi({ apiKey }), input }));
}
