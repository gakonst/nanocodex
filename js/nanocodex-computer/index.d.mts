import type { NamedTool } from "nanocodex-tools";
export type * from "./api.d.mts";
export function discoverComputer(options?: { binary?: string }): Promise<string | undefined>;
export type ComputerOptions = {
  executable: string;
  args?: readonly string[];
  environment?: Record<string, string>;
  desktopRuntime?: string;
  /** Exact MCP command/args; do not append companion-specific flags. */
  transport?: "mcp";
};
export type ComputerAttachment = Readonly<{ tools: readonly NamedTool[]; close(): Promise<void> }>;
export function createComputerTools(options: ComputerOptions): ComputerAttachment;
export function connectComputerTools(options: ComputerOptions): Promise<ComputerAttachment>;
