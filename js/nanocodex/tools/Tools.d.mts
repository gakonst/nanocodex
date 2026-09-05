import type { McpServers, NamedTool, ToolMap } from "../types.mjs";
import type { Workspace } from "../runtime/workspace.mjs";
import type { HostedMachine } from "./hostedCatalog.mjs";

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

/**
 * Creates one owned tool runtime. Caller-supplied tool ownership transfers only
 * after this promise resolves; a rejected construction leaves those tools with
 * the caller. The returned runtime joins all owned cleanup through close().
 */
export function createTools(options?: {
  tools?: ToolMap | readonly NamedTool[];
  /** Stable safe ASCII identifier (at most 123 bytes) for this independent attachment. */
  attachmentId?: string;
  /** Sole non-secret machine published by this host; its id must equal attachmentId. */
  machines?: readonly HostedMachine[];
  /** Portable workspace handle; React Native supplies one via createWorkspace({ backend }). */
  workspace?: Workspace;
  workspaceOptions?: { maxEntries?: number; maxReadBytes?: number; maxWriteBytes?: number };
  mcp?: McpServers | false;
  mcpOptions?: Readonly<Record<string, unknown>>;
}): Promise<Tools>;
