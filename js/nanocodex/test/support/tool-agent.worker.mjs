import { create } from "../../browser/InlineAgent.mjs";
import { installWorkerAgentRuntime } from "../../browser/WorkerAgent.mjs";

installWorkerAgentRuntime(globalThis, {
  createAgent({ blockedUrl, ...options }) {
    return create({
      ...options,
      tools: {
        blocked: {
          description: "Run one cancellation-aware fixture operation.",
          parameters: {
            type: "object",
            properties: { mode: { type: "string" } },
            required: ["mode"],
            additionalProperties: false,
          },
          async handler({ mode }, context) {
            if (mode === "fast") return "RECOVERED_TOOL";
            const response = await fetch(blockedUrl, { signal: context.signal });
            return response.text();
          },
        },
      },
    });
  },
});
