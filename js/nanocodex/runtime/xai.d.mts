import type { Agent as BaseAgent, EventWatcher, TurnUsage, WatchEventsOptions, DurabilityStore, ToolContext } from '../types.mjs';

/** Explicit, caller-approved credentials. The callback is resolved independently for each request. */
export type Auth = Readonly<
  | { apiKey: string; headers?: never }
  | { headers(): HeadersInit | Promise<HeadersInit>; apiKey?: never }
>;
export type ToolContent = Readonly<
  | { type: 'input_text'; text: string }
  | { type: 'input_image'; image_url: string; detail?: 'auto' | 'low' | 'high' }
  | { type: 'input_file'; file_data?: string; file_id?: string; filename?: string }
>;
export type ToolResult = Readonly<{
  output: string | readonly ToolContent[];
  success: boolean;
  structuredResult?: unknown;
  metadata?: unknown;
}>;
/** No default catalog: each named tool must be supplied with an actual host handler.
 * JS prompts carry no instruction revision; handlers receive the shared ToolContext. */
export type Tool = Readonly<{
  name: string;
  description: string;
  inputSchema?: Record<string, unknown>;
  /** Alias for existing named-tool object schemas. */
  parameters?: Record<string, unknown>;
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
}>;
export type CodexHarnessOptions = Readonly<{
  transport: import('../browser/Transport.mjs').ResponsesTransport | import('../node/Transport.mjs').ResponsesTransport;
  model?: import('../types.mjs').Model;
  thinking?: import('../types.mjs').Thinking;
  instructions?: string;
  workspace?: string;
  toolMode?: 'code' | 'direct';
  /** Overrides Node's native evaluator; required for Code Mode in non-Worker Web API hosts. */
  codeEvaluator?: import('../types.mjs').CodeEvaluator;
  tools?: import('../types.mjs').ToolConfiguration;
}>;
export type Options = Readonly<{
  harness?: 'xai';
  /** Opt in to the canonical shared subagent task tree. */
  subagents?: Readonly<{ maxConcurrency?: number }>;
  /** Explicit alternate-family capability; no credentials are inferred. */
  harnesses?: Readonly<{ codex?: CodexHarnessOptions; claude?: import('./claude.mjs').Options }>;
  auth: Auth;
  model: string;
  endpoint?: string;
  /** Explicit host Responses fetch; never serialized into model/session state. */
  fetch?: typeof globalThis.fetch;
  instructions?: string;
  /** Defaults to durabilityId for durable sessions; an explicit ID must match it. */
  sessionId?: string;
  workspace?: string;
  tools?: readonly Tool[];
  /** Explicit provider-owned tool definitions, not host capabilities. */
  serverTools?: readonly Record<string, unknown>[];
  thinking?: 'low' | 'medium' | 'high' | 'xhigh';
  contextWindowTokens?: number;
  /** Automatic compaction trigger as percent of context capacity, 1..100. */
  autoCompactThresholdPercent?: number;
  maxSteps?: number;
  /** Retries of explicit transient HTTP rejections; 0 disables retries. Default 3.
   * Interrupted streams and uncertain tool effects are never automatically replayed. */
  maxRetries?: number;
  /** Maximum executions of identical tool name/arguments in one turn. Default 3. */
  repetitionLimit?: number;
  /** Desired recent native item count to retain during compaction. Default 8.
   * Complete user/tool boundaries can retain more; even 0 retains the latest user turn. */
  compactionKeepTail?: number;
  requestTimeoutMs?: number;
  terminalReceiptRetention?: number;
  /** Compiled browser WASM module for this exact package. */
  module?: unknown;
}> & (
  | { durability?: never; durabilityId?: never }
  | { durability: DurabilityStore; durabilityId: string }
);
/** Shared output/event contract, with canonical subagents available through Subagents when enabled. */
export type Agent = BaseAgent<{
  events: { watch(options?: WatchEventsOptions): EventWatcher };
  session: { context(): Promise<import('../types.mjs').AgentSessionContext>; compact(): Promise<void>; cancel(): Promise<void>; shutdown(): Promise<void> };
  turn: { prompt(options: { input: string; id?: string }): Turn };
}>;
export type Turn = Readonly<{
  readonly agent: Agent;
  accepted(): Promise<string | undefined>;
  result(): Promise<Result>;
  steer(options: { input: string; messageId?: string }): Promise<void>;
  withdrawSteer(options: { messageId: string }): Promise<boolean>;
  cancel(): Promise<void>;
  dispose(): void;
}>;
export type Result = Readonly<{
  finalMessage: string;
  /** Unsupported for Xai: native checkpoints are owned by durability. Always rejects. */
  snapshot(): Promise<never>;
  usage(): Promise<TurnUsage>;
  dispose(): void;
}>;
