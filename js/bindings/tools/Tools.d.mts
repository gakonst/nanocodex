import type { McpServers, NamedTool, ToolMap } from "../types.mjs";
import type { Workspace } from "../runtime/workspace.mjs";

declare const toolsBrand: unique symbol;

export type AttachmentSocket = {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener?(type: string, listener: (event: any) => void, options?: unknown): void;
  on?(type: string, listener: (...args: any[]) => void): void;
};

export type AttachmentTransport = Readonly<{
  connect(target: string | URL): AttachmentSocket | Promise<AttachmentSocket>;
}>;

export type AttachmentTarget = string | URL | Readonly<{
  endpoint: string | URL;
  transport: AttachmentTransport;
}>;

export type AttachmentClient = Readonly<{
  readonly connected: boolean;
  closed(): Promise<void>;
  close(): Promise<void>;
}>;

export type Tools = Readonly<{
  readonly [toolsBrand]: true;
  attach(target: AttachmentTarget): Readonly<{
    connect(): Promise<AttachmentClient>;
    closed(): Promise<void>;
    close(): Promise<void>;
  }>;
  close(): Promise<void>;
}>;

export type ToolProvider = Readonly<{
  sourceId?: string | undefined;
  kind?: string | undefined;
  mode?: "union" | "attached-over-cloud" | undefined;
  deferred?: boolean | undefined;
  definitions(): readonly Record<string, unknown>[];
  resolve(name: string): Readonly<{
    name: string;
    parallelSafe?: boolean | undefined;
    handler(input: unknown, context: import("../types.mjs").ToolContext): unknown | Promise<unknown>;
  }> | undefined;
  settled?(): void | Promise<void>;
  close?(): void | Promise<void>;
}>;

/**
 * Creates one owned tool runtime. Caller-supplied tool ownership transfers only
 * after this promise resolves; a rejected construction leaves those tools with
 * the caller. The returned runtime joins all owned cleanup through close().
 */
export function createTools(options?: {
  tools?: ToolMap | readonly NamedTool[];
  /** Portable workspace handle; React Native supplies one via createWorkspace({ backend }). */
  workspace?: Workspace;
  workspaceOptions?: { maxEntries?: number; maxReadBytes?: number; maxWriteBytes?: number };
  mcp?: McpServers | false;
  mcpOptions?: Readonly<Record<string, unknown>>;
  /** Dynamic caller-owned capability sources such as the embedding page's WebMCP registry. */
  providers?: readonly ToolProvider[] | undefined;
}): Promise<Tools>;
