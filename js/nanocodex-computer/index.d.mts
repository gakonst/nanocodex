import type { NamedTool } from "nanocodex-tools";
export type * from "./api.d.mts";
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
/** Complete MCP form parameters, including provider-owned approval metadata. */
export type ComputerElicitationParams = Readonly<{
  mode?: "form";
  message: string;
  requestedSchema: Record<string, unknown>;
  _meta?: Record<string, unknown>;
  [key: string]: unknown;
}>;
export type ComputerElicitationResult = Readonly<{
  action: "accept" | "decline" | "cancel";
  content?: Record<string, unknown>;
  _meta?: Record<string, unknown>;
  [key: string]: unknown;
}>;
export type ComputerElicitationContext = Readonly<{
  /** Unavailable during initial provider discovery. */
  sessionId?: string;
  callId?: string;
  model?: string;
  requestId: string | number;
  /** Aborted on timeout, provider cancellation, caller abort, requesting call completion, or attachment close. */
  signal: AbortSignal;
}>;
export type ComputerElicitationHandler = (
  params: ComputerElicitationParams, context: ComputerElicitationContext,
) => ComputerElicitationResult | Promise<ComputerElicitationResult>;
export type ComputerOptions = {
  executable: string;
  /** Genuine host form UI callback. Only its presence advertises elicitation.form. */
  elicitationHandler?: ComputerElicitationHandler;
  /** Positive safe integer; defaults to 300000 ms. Expiry cancels the form. */
  elicitationTimeoutMs?: number;
  args?: readonly string[];
  environment?: Record<string, string>;
  desktopRuntime?: string;
  /** Exact MCP command/args; do not append companion-specific flags. */
  transport?: "mcp";
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
