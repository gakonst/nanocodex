import type { DurabilityStore } from '../types.mjs';
export type Family = 'claude' | 'codex';
export type NativeRequest = Readonly<Record<string, unknown>>;
export type PhysicalModel = Readonly<{
  model: string; family: Family; contextTokens: number; maxOutputTokens: number;
  /** Explicit compatibility group; switching also requires switchSafe and transparent history. */
  switchGroup?: string;
}>;
export type Section = Readonly<{ name: string; text: string }>;
export type Tool = Readonly<{ name: string; definition: NativeRequest }>;
export type Patch = Readonly<
  | { kind: 'set_section'; section: Section }
  | { kind: 'remove_section'; name: string }
  | { kind: 'set_tool'; tool: Tool }
  | { kind: 'remove_tool'; name: string }
>;
export type Context = Readonly<{
  family: Family; requestId: string; continuationOf?: string;
  inputTokens?: number; outputTokens?: number; switchSafe?: boolean;
}>;
export type Receipt = Readonly<{
  requestId: string; family: Family; selected: string; dispatched: string;
  original: NativeRequest; context: Context; request: NativeRequest;
  inputTokens: number; outputTokens: number;
  configuration: Readonly<{ sections: readonly Section[]; tools: readonly Tool[] }>;
  routerState: unknown; status: 'prepared' | 'dispatched' | 'completed' | 'failed';
  usage: NativeRequest | null;
}>;
export type WarmReceipt = Readonly<{
  requestId: string; status: 'dispatched' | 'completed'; ttlSeconds: 300 | 3600;
  estimatedWriteUsd: number; usage: NativeRequest | null; actualUsd: number | null;
}>;
export type Snapshot = Readonly<{
  format: 'nanocodex-request-policy-v1'; selection: string;
  configuration: Receipt['configuration']; history: readonly Readonly<{ requestId: string; patches: readonly Patch[] }>[];
  pending: readonly Patch[]; routerState: unknown; requests: readonly Receipt[];
  warms: readonly WarmReceipt[]; reservedWarmUsd: number; actualWarmUsd: number;
}>;
export type CacheWarm = Readonly<{
  enabled: true; ttlSeconds: 300 | 3600;
  maxSpendUsd: number; estimatedWriteUsd: number; estimatedReadUsd: number; estimatedUncachedUsd: number;
  reuseProbability: number; expectedReuseCount: number; expectedReuseWithinSeconds: number;
  inputUsdPerMillion: number; outputUsdPerMillion: number;
  cacheWriteUsdPerMillion: number; cacheReadUsdPerMillion: number;
}> | Readonly<{ enabled: false }>;
export type Options = Readonly<{
  durability: DurabilityStore; durabilityId: string;
  selection: string; models: readonly PhysicalModel[]; initialState?: unknown;
  route(input: Readonly<{
    original: NativeRequest; context: Context; selection: string; state: unknown;
    previous: string | null; models: readonly PhysicalModel[];
  }>): Readonly<{ model: string; state: unknown }> | Promise<Readonly<{ model: string; state: unknown }>>;
  estimateInputTokens?(request: NativeRequest, family: Family): number | Promise<number>;
  switchSafe?(request: NativeRequest, family: Family): boolean | Promise<boolean>;
  /** Rechecks current authority before each model or warm dispatch. Never persist credentials. */
  authorize?(receipt: Receipt): void | Promise<void>;
  requestContext?(request: NativeRequest, family: Family): Omit<Partial<Context>, 'family'> | Promise<Omit<Partial<Context>, 'family'>>;
  cacheWarm?: CacheWarm;
}>;
declare const brand: unique symbol;
export type RequestPolicy = Readonly<{
  readonly [brand]: true;
  configure(patches: readonly Patch[]): Promise<void>;
  snapshot(): Promise<Snapshot>;
  prepare(original: NativeRequest, context: Context): Promise<Receipt>;
  dispatch<T>(requestId: string, send: (request: NativeRequest, receipt: Receipt) => T | Promise<T>): Promise<T>;
  observe(requestId: string, usage: NativeRequest | null, status?: 'completed' | 'failed'): Promise<void>;
  fork(destination: Readonly<{ durability: DurabilityStore; durabilityId: string }>): Promise<RequestPolicy>;
  warm(requestId: string, send: (request: NativeRequest, receipt: WarmReceipt) => Response | Promise<Response>): Promise<NativeRequest>;
  /** Full native request/response boundary; successful streaming bodies remain pull-based. */
  fetch(fetchImpl: typeof globalThis.fetch, family: Family): typeof globalThis.fetch;
}>;
export function create(options: Options): Promise<RequestPolicy>;
/** @internal Rejects caller-fabricated policy handles. */
export function assertRequestPolicy(handle: unknown): RequestPolicy;
