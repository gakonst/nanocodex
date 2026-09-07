import type {
  CodeEvaluator,
  McpServers,
  MppSession,
  SubagentToolContext,
  ToolMap,
} from "../types.mjs";
import type { Workspace } from "./workspace.mjs";
export type BrowserTool = {
  description: string;
  parameters: Record<string, unknown>;
  handler: (
    input: unknown,
    context: { sessionId: string; signal: AbortSignal },
  ) => unknown | Promise<unknown>;
};

export type BrowserToolMap = Record<string, BrowserTool>;

type BrowserWebSocketMetadata = {
  accountId?: string | undefined;
  fedramp?: boolean | undefined;
  threadId?: string | undefined;
  turnState?: string | undefined;
};

export type BrowserWebSocketRequest = BrowserWebSocketMetadata & (
  | {
    authorization: "bearer";
    /** Resolved credential for this handshake. Do not retain or log it. */
    bearerToken: string;
  }
  | {
      authorization: "host_managed";
      bearerToken?: never;
    }
  | {
      /** Credential-free eager connection; the later model connect consumes this exact socket. */
      authorization: "preconnect";
      bearerToken?: never;
    }
);

export type BrowserWebSocketConnection = {
  socket: WebSocket;
  status?: number | undefined;
  requestId?: string | undefined;
  serverModel?: string | undefined;
  reasoningIncluded?: boolean | undefined;
  turnState?: string | undefined;
};

export function createBrowserHost(options?: {
  WebSocketImpl?: typeof WebSocket;
  hostAuth?: boolean;
  hostManagedProtocol?: boolean;
  createWebSocket?: (
    endpoint: string,
    sessionId: string,
    request: BrowserWebSocketRequest,
  ) => WebSocket | BrowserWebSocketConnection | Promise<WebSocket | BrowserWebSocketConnection>;
  filesystem?: Workspace;
  filesystemTools?: boolean;
  onEvent?: (eventJson: string) => void;
  tools?: ToolMap;
  mpp?: MppSession;
  /** Remote MCP servers exposed through native and Code Mode tool_search plus deferred tools. */
  mcp?: McpServers;
  codeEvaluator?: CodeEvaluator;
  toolMode?: "code" | "direct";
  /** @internal Durable host lifecycle for Rust-owned subagent descriptors. */
  subagentSessions?: {
    restore(): readonly SubagentToolContext[];
    bind(sessionId: string, descriptor: SubagentToolContext): void;
    release(sessionId: string): void;
  };
  maxQueuedMessages?: number;
  maxQueuedBytes?: number;
  maxBufferedSendBytes?: number;
}): unknown;
