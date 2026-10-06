import type { HandResourceObservation } from "./hostedMachine.mjs";
import type { NamedTool } from "./types.mjs";
export function createNodeProcessTools(options: {
  workspace: string;
  onActivity?: (event: {
    type: "started" | "completed";
    sessionId: string;
    processId: number;
    exitCode?: number;
  }) => void;
}): Promise<{ tools: readonly NamedTool[]; resources: HandResourceObservation; close(): Promise<void> }>;
