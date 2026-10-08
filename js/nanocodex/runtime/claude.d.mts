import type { Agent as BaseAgent, EventWatcher, TurnUsage, WatchEventsOptions, DurabilityStore, ToolContext } from '../types.mjs';

/** Explicit, caller-approved credentials. The callback is resolved independently for each request. */
export type Auth = Readonly<
  | { apiKey: string; headers?: never }
  | { headers(): HeadersInit | Promise<HeadersInit>; apiKey?: never }
>;
export type ToolContent = Readonly<
  | { type: 'input_text'; text: string }
  | { type: 'input_image'; image_url: string; detail?: 'auto' | 'low' | 'high' }
>;
export type NativeToolResult = Readonly<{
  content: string | readonly Record<string, unknown>[];
  isError?: boolean;
  structuredResult?: unknown;
  metadata?: unknown;
}>;
export type ToolResult = Readonly<{
  output: string | readonly ToolContent[];
  success: boolean;
  structuredResult?: unknown;
  metadata?: unknown;
}>;
/** No default catalog: each named tool must be supplied with an actual host handler. */
export type Tool = Readonly<{
  name: string;
  description: string;
  inputSchema?: Record<string, unknown>;
  /** Alias for existing named-tool object schemas. */
  parameters?: Record<string, unknown>;
  strict?: boolean;
  deferLoading?: boolean;
  /** Native wire-name alias for deferLoading; do not supply both. */
  defer_loading?: boolean;
  /** Independent calls may overlap with adjacent parallel-safe calls in one response. */
  supportsParallelToolCalls?: boolean;
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
}>;
export type CodexHarnessOptions = Readonly<{
  transport: import('../browser/Transport.mjs').ResponsesTransport | import('../node/Transport.mjs').ResponsesTransport;
  model?: import('../types.mjs').Model;
  thinking?: import('../types.mjs').Thinking;
  instructions?: string;
  workspace?: string;
  toolMode?: 'code' | 'code-only' | 'direct';
  /** Overrides Node's native evaluator; required for Code Mode in non-Worker Web API hosts. */
  codeEvaluator?: import('../types.mjs').CodeEvaluator;
  tools?: import('../types.mjs').ToolConfiguration;
}>;
export type Options = Readonly<{
  harness?: 'claude';
  /** Direct native tools by default; code-only exposes only exec and wait. */
  toolMode?: 'direct' | 'code-only';
  /** Required for code-only; evaluation is never inferred from ambient JavaScript. */
  codeEvaluator?: import('../types.mjs').CodeEvaluator;
  /** Opt in to the canonical shared subagent task tree. */
  subagents?: Readonly<{ maxConcurrency?: number }>;
  /** Explicit alternate-family capability; no credentials are inferred. */
  harnesses?: Readonly<{ codex?: CodexHarnessOptions }>;
  auth: Auth;
  model: string;
  endpoint?: string;
  /** Explicit host Messages fetch; never serialized into model/session state. */
  fetch?: typeof globalThis.fetch;
  /** Protocol compatibility only; supplies neither authentication nor product parity. Requires endpoint. */
  compatibilityProfile?: 'subscription';
  /** Public stable OMP wire affinity. Reuse installId across sessions/restarts; never supply credentials. */
  subscriptionIdentity?: Readonly<{ installId?: string; accountUuid?: string; userId?: string; platform?: string; arch?: string; version?: string }>;
  instructions?: string;
  systemBlocks?: readonly Record<string, unknown>[];
  /** Defaults to durabilityId for durable sessions; an explicit ID must match it. */
  sessionId?: string;
  workspace?: string;
  tools?: readonly Tool[];
  /** Explicit provider-owned tool definitions, not host capabilities. */
  serverTools?: readonly Record<string, unknown>[];
  maxTokens?: number;
  thinking?: 'none' | 'low' | 'medium' | 'high' | 'xhigh' | 'max';
  adaptiveThinking?: boolean;
  keepThinking?: boolean;
  cache?: 'off' | '5m' | '1h';
  parallelTools?: boolean;
  clientToolSearch?: boolean;
  contextWindowTokens?: number;
  autoCompactWindowTokens?: number;
  /** Disabling automatic compaction is not supported. */
  autoCompact?: true;
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
  session: { compact(): Promise<void>; cancel(): Promise<void>; shutdown(): Promise<void> };
  turn: { prompt(options: { input: string; id?: string }): Turn };
}>;
export type Turn = Readonly<{
  readonly agent: Agent;
  accepted(): Promise<string | undefined>;
  result(): Promise<Result>;
  cancel(): Promise<void>;
  dispose(): void;
}>;
export type Result = Readonly<{
  finalMessage: string;
  /** Unsupported for Claude: native checkpoints are owned by durability. Always rejects. */
  snapshot(): Promise<never>;
  usage(): Promise<TurnUsage>;
  dispose(): void;
}>;
