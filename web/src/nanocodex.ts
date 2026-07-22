import { createNanocodexConfig } from "nanocodex-react";

/** Website-owned wiring for the publishable React package. */
export const nanocodexConfig = createNanocodexConfig({
  createWorker: () => new Worker(new URL("./agent.worker.ts", import.meta.url), { type: "module" }),
  checkHealth: async () => {
    const response = await fetch("/api/health");
    return response.json() as Promise<{
      agent_configured?: boolean;
      credential_source?: "user" | "deployment" | null;
    }>;
  },
});
