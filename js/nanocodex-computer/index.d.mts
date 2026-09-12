import type { NamedTool } from "nanocodex-tools";
export function discoverComputer(options?: { binary?: string }): Promise<string | undefined>;
export function createComputerTools(options: {
  executable: string;
  args?: readonly string[];
  environment?: Record<string, string>;
  desktopRuntime?: string;
}): Readonly<{ tools: readonly NamedTool[]; close(): Promise<void> }>;
