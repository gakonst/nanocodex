import { Agent, Subagents, Transport } from "nanocodex/node";

const apiKey = process.env.OPENAI_API_KEY?.trim();
if (!apiKey) {
  throw new Error("Set OPENAI_API_KEY or put it in the repository's ignored .env file.");
}

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey }),
  tools: [
    // This selects Rust code already linked into nanocodex.wasm. There are no
    // JavaScript implementations of the seven subagent tools in this example.
    ...Subagents.create({ maxConcurrency: 8 }),
  ],
  instructions: [
    "You are the lead orchestrator.",
    "Delegate independent work with spawn_agent, use directed messages for coordination,",
    "wait for useful results, and synthesize an attributed final answer.",
  ].join(" "),
  thinking: "low",
});

try {
  const goal = process.argv.slice(2).join(" ").trim()
    || "Use two specialists to review this repository's JS API, let them coordinate if their findings overlap, then synthesize the highest-value next improvement.";
  const turn = agent.turn.prompt({ input: goal });
  let result;
  try {
    result = await turn.result();
    console.log(result.finalMessage);
  } finally {
    try {
      result?.dispose();
    } finally {
      turn.dispose();
    }
  }
} finally {
  // Rust closes the complete subagent subtree before the root driver exits.
  await agent.session.shutdown();
}
