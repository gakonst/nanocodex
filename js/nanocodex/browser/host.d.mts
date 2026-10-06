import type {
  CodeEvaluator,
  CodeEffectJournal,
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

/** One streaming HTTPS Responses request. The host must preserve cancellation. */
export type BrowserHttpRequest = Exclude<BrowserWebSocketRequest, { authorization: "preconnect" }> & {
  body: string;
  signal: AbortSignal;
};

export type BrowserWebSocketConnection = {
  socket: WebSocket;
  status?: number | undefined;
  requestId?: string | undefined;
  /** @internal Correlation with the separately deployed credential egress. */
  egressRequestId?: string | undefined;
  serverModel?: string | undefined;
  reasoningIncluded?: boolean | undefined;
  turnState?: string | undefined;
};

/** @internal Passive lifecycle metadata; never provider frames or request content. */
export type BrowserSocketObservation = {
  event: "socket.connecting" | "socket.connect_waiting" | "socket.opened" | "socket.closed" | "socket.error"
    | "request.send_started" | "request.send_waiting" | "request.sent" | "request.waiting" | "request.first_message"
    | "request.first_output" | "request.finished" | "provider.timing";
  socket_id: string;
  request_id: string;
  egress_request_id?: string;
  provider_request_id?: string;
  response_id?: string;
  socket_request_index?: number;
  model_call_index?: number;
  phase?: "generation" | "compaction" | "warmup";
  outcome?: "completed" | "failed" | "send_failed" | "superseded";
  elapsed_ms?: number;
  send_wait_ms?: number;
  first_message_ms?: number;
  first_output_ms?: number;
  last_message_age_ms?: number;
  received_message_count?: number;
  queued_message_count?: number;
  socket_delivered_message_count?: number;
  socket_queue_residence_max_ms?: number;
  buffered_send_bytes?: number;
  close_code?: number;
  close_clean?: boolean;
  intentional?: boolean;
  pre_inference_ms?: number;
  engine_queue_max_ms?: number;
  engine_service_ttft_total_ms?: number;
};

export function createBrowserHost(options?: {
  /** @internal Trusted runtime configuration carried by nanocodex.browser.internalRuntime. */
  [key: symbol]: {
    traceTool?: <T>(
      name: string,
      context: { sessionId: string; callId: string; parentCallId?: string; turnId?: string },
      run: () => Promise<T>,
    ) => Promise<T>;
  } | undefined;
  WebSocketImpl?: typeof WebSocket;
  hostAuth?: boolean;
  hostManagedProtocol?: boolean;
  createWebSocket?: (
    endpoint: string,
    sessionId: string,
    request: BrowserWebSocketRequest,
  ) => WebSocket | BrowserWebSocketConnection | Promise<WebSocket | BrowserWebSocketConnection>;
  createResponse?: (endpoint: string, sessionId: string, request: BrowserHttpRequest) => Promise<Response>;
  filesystem?: Workspace;
  filesystemTools?: boolean;
  onEvent?: (eventJson: string) => void;
  tools?: ToolMap;
  mpp?: MppSession;
  /** Remote MCP servers exposed through native and Code Mode tool_search plus deferred tools. */
  mcp?: McpServers;
  codeEvaluator?: CodeEvaluator;
  /** @internal Trusted durable effect receipts, not available inside guest code. */
  codeEffectJournal?: CodeEffectJournal;
  toolMode?: "code" | "code-only" | "direct";
  /** @internal Live host lifecycle for ephemeral Rust-owned subagents. */
  subagentRouting?: Pick<import('../runtime/subagent-routing.mjs').SubagentRouting, 'resolve' | 'bind'>;
  subagentSessions?: {
    bindingDescriptor?(sessionId: string, descriptor: SubagentToolContext, hostContextRef?: string): SubagentToolContext;
    bind(sessionId: string, descriptor: SubagentToolContext, hostContextRef?: string): void;
    release(sessionId: string, hostContextRef?: string): void;
  };
  /** @internal Content-free summary, once when ownership closes; no public transport option. */
  onSocketTiming?: (timing: {
    message_count: number;
    delivered_message_count: number;
    buffered_message_count: number;
    discarded_message_count: number;
    queue_residence_total_ms: number;
    queue_residence_max_ms: number;
    provider_timings: Array<{
      response_id?: string;
      pre_inference_ms?: number;
      engine_queue_max_ms?: number;
      engine_service_ttft_total_ms?: number;
    }>;
  }) => void;
  /** @internal Live observations while an owned request waits; does not cancel it. */
  onSocketEvent?: (event: BrowserSocketObservation) => void;
  maxQueuedMessages?: number;
  maxQueuedBytes?: number;
  maxBufferedSendBytes?: number;
}): unknown;
