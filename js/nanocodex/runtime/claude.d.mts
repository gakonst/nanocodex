import type { DefaultAgent, DurabilityStore, SessionCheckpoint, ToolContext } from '../types.mjs';

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
  toolMode?: 'code-only';
  /** Overrides Node QuickJS; required in non-Worker Web API hosts. */
  codeEvaluator?: import('../types.mjs').CodeEvaluator;
  tools?: import('../types.mjs').ToolConfiguration;
}>;
export type Options = Readonly<{
  harness?: 'claude';
  /** Only exec and wait are exposed to the model; native tools run inside exec. */
  toolMode?: 'code-only';
  /** Overrides Node QuickJS or browser Worker evaluation; required in other Web API hosts. */
  codeEvaluator?: import('../types.mjs').CodeEvaluator;
  /** Opt in to the canonical shared subagent task tree. */
  subagents?: Readonly<{ maxConcurrency?: number }>;
  /** Explicit alternate-family capability; no credentials are inferred. */
  harnesses?: Readonly<{ codex?: CodexHarnessOptions }>;
  auth: Auth;
  /** A cataloged Claude model, or another id served by a compatible `endpoint`. */
  model: import("../types.mjs").ClaudeModel | (string & {});
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
  maxTokens?: number;
  thinking?: 'none' | 'low' | 'medium' | 'high' | 'xhigh' | 'max';
  adaptiveThinking?: boolean;
  keepThinking?: boolean;
  cache?: 'off' | '5m' | '1h';
  parallelTools?: boolean;
  contextWindowTokens?: number;
  autoCompactWindowTokens?: number;
  /** Disabling automatic compaction is not supported. */
  autoCompact?: true;
  terminalReceiptRetention?: number;
  /** Starts a new session continuing this Claude checkpoint's committed conversation. */
  resume?: SessionCheckpoint;
  /** Compiled browser WASM module for this exact package. */
  module?: unknown;
}> & (
  | { durability?: never; durabilityId?: never }
  | { durability: DurabilityStore; durabilityId: string }
);
/** The one harness-neutral Agent; canonical subagents are available through Subagents when enabled. */
export type Agent = DefaultAgent;
