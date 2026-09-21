import type { NamedTool } from "nanocodex-tools";
/** Installs a missing managed provider through the trusted native CLI on supported platforms. */
export function ensureComputer(options?: { binary?: string }): Promise<string | undefined>;
export function discoverComputer(options?: { binary?: string }): Promise<string | undefined>;
export type ComputerToolDefinition = Readonly<{
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
  annotations?: Readonly<Record<string, unknown>>;
  _meta?: Readonly<Record<string, unknown>>;
  [key: string]: unknown;
}>;
export type ComputerOptions = {
  executable: string;
  args?: readonly string[];
  environment?: Record<string, string>;
  /** Trusted discovered catalog; connectComputerTools obtains this automatically. */
  definitions?: readonly ComputerToolDefinition[];
};
export type ComputerProviderTool = NamedTool & Readonly<{ providerDefinition: ComputerToolDefinition }>;
export type ComputerAttachment = Readonly<{
  /** Full provider catalog, including hidden lifecycle hooks. */
  definitions: readonly ComputerToolDefinition[];
  /** Model-visible provider tools. */
  tools: readonly ComputerProviderTool[];
  /** Trusted access to any discovered tool, including hidden hooks. */
  tool(name: string): ComputerProviderTool | undefined;
  close(): Promise<void>;
}>;
export function createComputerTools(options: ComputerOptions): ComputerAttachment;
export function connectComputerTools(options: ComputerOptions): Promise<ComputerAttachment>;
