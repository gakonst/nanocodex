import { Agent, Transport } from "nanocodex/node";
import { runOwnedSession } from "./session.mjs";

const apiKey = process.env.OPENAI_API_KEY?.trim();
if (!apiKey) {
  throw new Error("Set OPENAI_API_KEY or put it in the repository's ignored .env file.");
}

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey }),
  thinking: "low",
  tools: {
    multiply: {
      description: "Multiply two numbers.",
      parameters: {
        type: "object",
        properties: { left: { type: "number" }, right: { type: "number" } },
        required: ["left", "right"],
        additionalProperties: false,
      },
      handler: async ({ left, right }) => left * right,
    },
  },
});
await runOwnedSession(agent);
