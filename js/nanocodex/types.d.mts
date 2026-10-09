import type { Options as ClaudeOptions } from './runtime/claude.mjs';
export type Thinking = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type ReasoningMode = "standard" | "pro";
export type Model = "gpt-6.1-sol" | "gpt-6-luna" | "gpt-6-astra" | "@cf/zai-org/glm-5.3" | "kimi-k3" | "mimo-v2.6-pro";
/** Native agent-loop family that owns a session's conversation. */
export type HarnessFamily = "codex" | "claude";
/** Cataloged models of the native Claude Messages harness. */
export type ClaudeModel =
  | "claude-opus-5-5"
  | "claude-sonnet-5-5"
  | "claude-haiku-5-5"
  | "claude-fable-5-1"
  | "claude-opus-4-6"
  | "claude-sonnet-4-6"
  | "claude-haiku-4-5";
/** A cataloged model id of either harness family. */
export type HarnessModel = Model | ClaudeModel;

/** How a session was created. */
export type SessionOrigin = "root" | "fork" | "side_conversation" | "subagent" | "branch";

/** How a session relates to the conversation tree it belongs to. */
export type SessionLineage = Readonly<{
  /** Root session of this conversation tree; equal to the session for a root. */
  rootSessionId: string;
  /** Directly preceding session, or null for a root. */
  parentSessionId: string | null;
  origin: SessionOrigin;
  /** Distance from the root session. */
  depth: number;
}>;

/** Stable identity and provenance of one session, shared by every harness. */
export type SessionInfo = Readonly<{
  sessionId: string;
  harness: HarnessFamily;
  lineage: SessionLineage;
}>;

/** Processing tier for model requests. */
export type ServiceTier = "standard" | "priority" | "fast" | "ultrafast";

/** When a session setting may change. */
export type Mutability = "fixed" | "before_first_prompt" | "anytime";

/**
 * Lifecycle operations a session's backend supports. Unsupported operations
 * reject with an Error whose `code` is `unsupported_capability` and whose
 * `capability` names the operation.
 */
export type SessionCapabilities = Readonly<{
  checkpoint: boolean;
  /** Checkpoints can be resumed in a new runtime, keeping the session identity. */
  resume: boolean;
  fork: boolean;
  /** Forking from a completed TurnResult or a SessionCheckpoint. */
  forkAt: boolean;
  sideConversation: boolean;
  spawn: boolean;
  steering: boolean;
  identifiedSteering: boolean;
  compaction: boolean;
  developerMessages: boolean;
  context: boolean;
  model: Mutability;
  thinking: Mutability;
  serviceTier: Mutability;
  /** Whether the "ultrafast" service tier is accepted, subject to serviceTier. */
  ultrafastServiceTier: boolean;
}>;

/** Where a session is persisted. */
export type SessionPersistence = Readonly<{
  /** Durable store state that is the session's source of truth, when any. */
  durableStateId: string | null;
  /** Server-side session that durably owns the conversation (managed hosts). */
  serverSessionId: string | null;
  /** Whether another process can resume this session. */
  resumable: boolean;
}>;

declare const sessionCheckpointBrand: unique symbol;
/**
 * Portable, versioned, family-tagged conversation boundary for any harness.
 * It is JSON-safe: `JSON.stringify` it to store it, and `JSON.parse` the
 * stored text to pass it back as `resume` or `fork({ at })`. It contains the
 * complete unredacted model-visible conversation; protect it accordingly.
 * Its contents are opaque and decoded only by the harness family that produced it.
 */
export type SessionCheckpoint = Readonly<{
  readonly [sessionCheckpointBrand]: "NanocodexSessionCheckpoint";
}>;

export type PromptItem =
  | { type: "text"; text: string }
  | { type: "image"; image_url: string; file_id?: never; detail?: "auto" | "low" | "high" | "original" | undefined }
  | { type: "image"; file_id: string; image_url?: never; detail?: "auto" | "low" | "high" | "original" | undefined }
  | { type: "audio"; audio_url: string }
  /** Inline document (`data:application/pdf;base64,…` or `data:text/plain;base64,…`); native Claude only. */
  | { type: "file"; file_data: string; filename?: string | undefined };

export type PromptInput = string | readonly PromptItem[];

export type AgentEvent = {
  protocol_version: number;
  request_id: string;
  seq: number;
  type: string;
  payload: Record<string, unknown>;
};

/** An awaited preservation barrier before manual or automatic compaction. */
export type BeforeCompactionRequest = Readonly<{
  /** Stable across durable retries. Deduplicate preservation by this identity. */
  boundaryId: string;
  sessionId: string;
  rootSessionId: string;
  /** Source order, at most 64 messages / 32 KiB UTF-8 text. No tools or harness context. */
  messages: readonly Readonly<{ role: "user" | "assistant"; text: string }>[];
  truncated: boolean;
  /** Aborted on interruption, host disposal, or the 30-second deadline. */
  signal: AbortSignal;
}>;

export type CompactionReceipt = Readonly<{
  /** Return only after preservation or an intentional no-op is durable (1–256 UTF-8 bytes). */
  receiptId: string;
}>;

export type AgentOptions = {
  /** Native agent-loop family. Pass `harness: "claude"` with Claude options (auth, model) for the Messages harness; both return the same Agent. */
  harness?: "codex" | undefined;
  /** Explicit alternate-family credentials and native tools; children remain in the shared task tree. */
  harnesses?: Readonly<{ claude?: ClaudeOptions }> | undefined;
  /** Optional host barrier. Rejection/timeout stops compaction and retains context.
   * Durable execution replays completed receipts; hosts must deduplicate by boundaryId
   * for interruption between host commit and receipt persistence. Disabled by default.
   * Available in Node, nanocodex/host, and Cloudflare; never inherited by subagents.
   */
  beforeCompaction?: ((request: BeforeCompactionRequest) => Promise<CompactionReceipt>) | undefined;

  /** Replaces the selected model's built-in instructions. */
  instructions?: string | undefined;
  /** Appends host instructions while retaining the selected model's prompt. */
  additionalInstructions?: string | undefined;
  model?: Model | undefined;
  reasoningMode?: ReasoningMode | undefined;
  fastMode?: boolean | undefined;
  /** Yield exec/wait observations on accepted steering; cells continue. Default false. */
  instantToolSteering?: boolean | undefined;
  /** Emit full raw API request/response events. Defaults to true. */
  rawApiEvents?: boolean | undefined;
  sessionId?: string | undefined;
  thinking?: Thinking | undefined;
  workspace?: string | undefined;
  /**
   * Reopens this checkpointed session in a new runtime, keeping its session
   * identity, lineage, model, thinking level and committed conversation.
   * Passing a different `sessionId` instead starts a new root session that
   * continues the checkpoint's conversation. Use `session.fork` to branch a
   * live session.
   */
  resume?: SessionCheckpoint | undefined;
};

/** Model-visible facts for tools executing outside the embedding process. */
export type ExecutionEnvironment = Readonly<{
  currentDate: string;
  timezone: string;
  projectInstructions?: string | undefined;
}>;

/** Unsigned decimal revision. Strings preserve the complete Rust `u64` range. */
declare const durabilityRevisionBrand: unique symbol;
export type DurabilityRevision = string & {
  readonly [durabilityRevisionBrand]: "NanocodexDurabilityRevision";
};

/** Unsigned decimal owner generation. Strings preserve the complete Rust `u64` range. */
declare const durabilityFenceBrand: unique symbol;
export type DurabilityFence = string & {
  readonly [durabilityFenceBrand]: "NanocodexDurabilityFence";
};

export type DurabilityStoredState = Readonly<{
  revision: DurabilityRevision;
  payload: string | null;
}>;

/** JSON-safe exact state archive used for an offline provider cutover. */
export type DurabilityPortableStateArchive = DurabilityStoredState & Readonly<{
  format: "nanocodex-durability-state-v2";
  stateId: string;
  records: readonly DurabilityRecord[];
}>;

export type DurabilityExportCursor = string;

/** One deterministic page of the total-state replacement from `from` (exclusive) to `to` (inclusive). */
export type DurabilityPortableStatePage = Readonly<{
  format: "nanocodex-durability-state-page-v2";
  stateId: string;
  from: DurabilityRevision;
  /** SHA-256 over the UTF-8 JSON tuple `[from, fromPayload]`. */
  fromDigest: string;
  to: DurabilityRevision;
  cursor: DurabilityExportCursor;
  nextCursor: DurabilityExportCursor | null;
  /** Total UTF-16 code units in the opaque state payload. */
  payloadLength: number;
  payload: string;
  records: readonly DurabilityRecord[];
}>;

export type DurabilityExportPageRequest = Readonly<{
  from: DurabilityRevision;
  /** Digest of the exact state at `from`; omit only when `from` is revision zero. */
  fromDigest?: string | undefined;
  /** Omit on the first request to select the current source revision; repeat the returned `to`. */
  to?: DurabilityRevision | undefined;
  cursor?: DurabilityExportCursor | undefined;
  /** UTF-16 code units per page. Defaults to 256 KiB and is capped at 1 MiB. */
  limit?: number | undefined;
}>;

export type DurabilityAcquireRequest = Readonly<{
  ownerId: string;
}>;

export type DurabilityAcquiredState = DurabilityStoredState & Readonly<{
  ownerId: string;
  fence: DurabilityFence;
}>;

export type DurabilityRecord = Readonly<{ key: string; value: string }>;

export type DurabilityReplaceRequest = Readonly<{
  records: readonly DurabilityRecord[];
  ownerId: string;
  fence: DurabilityFence;
  expectedRevision: DurabilityRevision;
  payload: string;
}>;

export type DurabilityReplaceResult =
  | Readonly<{ status: "replaced"; revision: DurabilityRevision }>
  | Readonly<{ status: "fenced" }>
  | Readonly<{ status: "conflict"; actualRevision: DurabilityRevision }>
  | Readonly<{ status: "not_committed"; message: string }>;

/** Host capability consumed by the Rust/WASM durability driver. */
export type DurabilityStore = Readonly<{
  readRecord(stateId: string, key: string): string | null | Promise<string | null>;
  /** Optional single-query implementation; Rust requests at most 16 records. */
  readRecords?(stateId: string, keys: readonly string[]): readonly (string | null)[] | Promise<readonly (string | null)[]>;
  load(stateId: string): DurabilityStoredState | Promise<DurabilityStoredState>;
  acquire(
    stateId: string,
    request: DurabilityAcquireRequest,
  ): DurabilityAcquiredState | Promise<DurabilityAcquiredState>;
  replace(
    stateId: string,
    request: DurabilityReplaceRequest,
  ): DurabilityReplaceResult | Promise<DurabilityReplaceResult>;
}>;

/** Store that can atomically restore an exact revision into an empty destination. */
export type DurabilityPortableStore = DurabilityStore & Readonly<{
  scanRecords(stateId: string, after?: string, limit?: number): readonly DurabilityRecord[] | Promise<readonly DurabilityRecord[]>;
  importRecords(stateId: string, records: readonly DurabilityRecord[]): void | Promise<void>;
  importState(
    stateId: string,
    state: DurabilityStoredState,
    options?: Readonly<{
      records?: readonly DurabilityRecord[];
      expectedRevision?: DurabilityRevision | undefined;
      /** When supplied, compare the complete expected state atomically before importing. */
      expectedPayload?: string | null | undefined;
    }> | undefined,
  ): DurabilityStoredState | Promise<DurabilityStoredState>;
}>;

/** In-process store for hosts that carry its snapshot across durable steps. */
export type MemoryDurabilityStore = DurabilityPortableStore & Readonly<{
  stateId: string;
  snapshot(): DurabilityStoredState;
}>;

/**
 * SQLite scalar accepted by the generic adapter. Revision and fence numbers
 * must be nonnegative safe integers; return exact decimal TEXT for larger values.
 */
export type DurabilitySqliteValue = string | number | null;
export type DurabilitySqliteRow = Record<string, DurabilitySqliteValue>;

export type DurabilitySqliteQuery = <Row extends DurabilitySqliteRow>(
  sql: string,
  args: readonly DurabilitySqliteValue[],
) => readonly Row[] | Promise<readonly Row[]>;

export type DurabilitySqliteTransaction = <Result>(
  callback: (query: DurabilitySqliteQuery) => Result | Promise<Result>,
) => Result | Promise<Result>;

export type SqliteDurabilityStoreOptions = Readonly<{
  transaction: DurabilitySqliteTransaction;
}>;

/** Unsigned decimal revision for opaque ChatGPT subscription state. */
declare const subscriptionRevisionBrand: unique symbol;
export type SubscriptionRevision = string & {
  readonly [subscriptionRevisionBrand]: "NanocodexSubscriptionRevision";
};

export type SubscriptionStoredValue = Readonly<{
  revision: SubscriptionRevision;
  payload?: string | undefined;
}>;

export type SubscriptionCommitRequest = Readonly<{
  expectedRevision: SubscriptionRevision;
  /** Opaque Rust-owned credential state. Hosts must store it as a secret. */
  payload: string;
}>;

export type SubscriptionCommitResult =
  | Readonly<{ status: "committed"; revision: SubscriptionRevision }>
  | Readonly<{ status: "conflict"; actualRevision: SubscriptionRevision }>;

/** Generic secret persistence consumed by the Rust ChatGPT lifecycle. */
export type ChatGptSubscriptionStore = Readonly<{
  load(id: string): SubscriptionStoredValue | Promise<SubscriptionStoredValue>;
  compareAndSwap(
    id: string,
    request: SubscriptionCommitRequest,
  ): SubscriptionCommitResult | Promise<SubscriptionCommitResult>;
}>;

export type MemoryChatGptSubscriptionStore = ChatGptSubscriptionStore & Readonly<{
  id: string;
  snapshot(): SubscriptionStoredValue;
}>;

export type ChatGptCredentialSeed = Readonly<{
  accessToken: string;
  refreshToken?: string | undefined;
  accountId: string;
  fedramp?: boolean | undefined;
}>;

export type ChatGptLoginStatus =
  | Readonly<{ state: "signed_out" | "expired" }>
  | Readonly<{
      state: "pending";
      verificationUrl: string;
      userCode: string;
      expiresAt: number;
      pollAfterMs: number;
    }>
  | Readonly<{
      state: "authenticated";
      accountId: string;
      expiresAt: number | null;
    }>;

export type ChatGptCredential = Readonly<{
  kind: "chatgpt";
  /** Resolved bearer credential. Do not retain or log it. */
  accessToken: string;
  accountId: string;
  fedramp: boolean;
  revision: SubscriptionRevision;
}>;

export type ChatGptSubscriptionHandle = Readonly<{
  id: string;
  startLogin(): Promise<ChatGptLoginStatus>;
  status(): Promise<ChatGptLoginStatus>;
  credential(): Promise<ChatGptCredential>;
  recover(rejectedRevision: SubscriptionRevision): Promise<ChatGptCredential>;
  logout(): Promise<void>;
  dispose(): void;
}>;

export type ChatGptSubscriptionOptions = Readonly<{
  id: string;
  store: ChatGptSubscriptionStore;
  /** Generic bounded HTTP capability; defaults to global fetch. */
  fetch?: typeof globalThis.fetch | undefined;
  /**
   * Trusted initial credentials, typically imported from Codex auth.json.
   * A same-account seed repairs stored access-only credentials; refreshable durable state wins.
   */
  seed?: ChatGptCredentialSeed | undefined;
  /** Test-only local issuer override. */
  issuer?: string | undefined;
  /** Browser WASM module compiled from the same nanocodex package. */
  module?: unknown;
}>;

export type EstimatedUsdCost = Readonly<{
  usd: string;
  input_usd: string;
  cached_input_usd: string;
  cache_write_input_usd: string;
  output_usd: string;
  service_tier: ServiceTier;
}>;

export type CostStatus =
  | "estimated_from_usage"
  | "usage_not_reported"
  | "other";

export type TurnUsage = Readonly<{
  input_tokens: number;
  cached_input_tokens: number;
  cache_write_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
  estimated_cost: EstimatedUsdCost | null;
  cost_status: CostStatus;
}>;

export type ForkOptions = Readonly<{
  /** Boundary to fork from; defaults to the latest committed safe boundary. */
  at?: TurnResult | SessionCheckpoint | undefined;
  /** Provenance recorded on the child's lineage. Defaults to `"fork"`. */
  origin?: "fork" | "side_conversation" | undefined;
}>;
export type WatchEventsOptions = { includeAllSessions?: boolean | undefined };

/** Read-only model context captured at the latest safe agent boundary. */
export type AgentSessionContext = Readonly<{
  workspace: string;
  history: readonly Record<string, unknown>[];
}>;

export type RealtimeTranscriptEntry = Readonly<{
  role: "user" | "assistant";
  text: string;
}>;

export type EventWatcher = Readonly<{
  onEvent(listener: (event: AgentEvent) => void): () => void;
  off(): void;
  [Symbol.asyncIterator](): AsyncIterableIterator<AgentEvent>;
}>;

export type AgentActions = {
  events: {
    watch(options?: WatchEventsOptions): EventWatcher;
  };
  session: {
    /** This session's identity, harness family, and lineage. */
    info(): SessionInfo;
    /** Lifecycle operations this session's backend supports. */
    capabilities(): SessionCapabilities;
    /** Where this session is persisted, or null when it lives only in memory. */
    persistence(): SessionPersistence | null;
    /** Exports the latest committed safe boundary without mutating this session. */
    checkpoint(): Promise<SessionCheckpoint>;
    appendDeveloperMessage(text: string): Promise<AgentSessionContext>;
    /** Cancels every nonterminal turn issued through this Agent. */
    cancel(): Promise<void>;
    compact(): Promise<void>;
    context(): Promise<AgentSessionContext>;
    fork(options?: ForkOptions): Promise<DefaultAgent>;
    /** Changes the model to another model id of this session's harness family. */
    setModel(model: HarnessModel): Promise<void>;
    /** Shorthand for `setServiceTier(enabled ? "priority" : "standard")`. */
    setFastMode(enabled: boolean): Promise<void>;
    /** Selects the processing tier; governed by `capabilities().serviceTier`. */
    setServiceTier(serviceTier: ServiceTier): Promise<void>;
    setThinking(thinking: Thinking): Promise<void>;
    shutdown(): Promise<void>;
    spawn(): Promise<DefaultAgent>;
    realtime: {
      start(): Promise<AgentSessionContext>;
      end(): Promise<AgentSessionContext>;
      delegation(
        input: string,
        transcript?: readonly RealtimeTranscriptEntry[],
      ): Promise<string>;
      tailDelegation(
        transcript: readonly RealtimeTranscriptEntry[],
      ): Promise<string | undefined>;
    };
  };
  turn: {
    prompt(options: {
      input: PromptInput;
      id?: string | undefined;
    }): Turn;
  };
};

export type Agent<extended extends object = {}> = {
  readonly agentId: string;
  readonly key: string;
  readonly name: string;
  readonly sessionId: string;
  readonly type: string;
  readonly uid: string;
  extend<const extension extends object>(
    decorator: (agent: Agent<extended>) => extension,
  ): Agent<extended & extension>;
  /** Releases this JavaScript/WASM handle without joining unfinished turns. */
  dispose(): void;
} & extended;

export type DefaultAgent = Agent<AgentActions>;

/** Transport-independent Agent lifecycle shared by local and managed durable Agents. */
export type AgentLifecycle = {
  readonly agentId: string;
  readonly key: string;
  readonly name: string;
  readonly sessionId: string;
  readonly type: string;
  readonly uid: string;
  dispose(): void;
  events: {
    watch(options?: WatchEventsOptions): EventWatcher;
  };
  session: {
    /** Stops this client lifecycle. A managed shutdown never deletes the durable Agent. */
    shutdown(): Promise<void>;
  };
  turn: {
    prompt(options: {
      input: PromptInput;
      id?: string | undefined;
    }): LifecycleTurn;
  };
};

export type LifecycleTurn = Readonly<{
  readonly agent: Readonly<{
    agentId: string;
    key: string;
    name: string;
    sessionId: string;
    type: string;
    uid: string;
    dispose(): void;
  }>;
  accepted(): Promise<string | undefined>;
  result(): Promise<LifecycleTurnResult>;
  steer(options: { input: PromptInput; messageId?: string }): Promise<void>;
  /** Removes this identified steer only while it is still pending. */
  withdrawSteer(options: { messageId: string }): Promise<boolean>;
  cancel(): Promise<void>;
  dispose(): void;
}>;

export type LifecycleTurnResult = Readonly<{
  finalMessage: string;
  /** Managed Agents may return null when the service did not report usage. */
  usage(): Promise<TurnUsage | null>;
  dispose(): void;
}>;

export type Turn<agent extends Agent<object> = Agent<object>> = Readonly<{
  readonly agent: agent;
  /**
   * Waits for execution-policy admission and returns its durable request ID.
   * Rejections are Errors whose `code` is `cancelled`, `blocked`, `conflict`,
   * `retryable`, `reopen_required`, `invalid_request`, or `failed`.
   */
  accepted(): Promise<string | undefined>;
  /**
   * Waits for the terminal result. A `reopen_required` rejection means this
   * Agent is stale and the same durable turn may be resumed only on a new Agent.
   */
  result(): Promise<TurnResult>;
  steer(options: { input: PromptInput; messageId?: string }): Promise<void>;
  /** Removes this identified steer only while it is still pending. */
  withdrawSteer(options: { messageId: string }): Promise<boolean>;
  cancel(): Promise<void>;
  /** Releases this handle without cancelling its accepted turn. */
  dispose(): void;
}>;

declare const turnResultBrand: unique symbol;
/** Opaque completed-turn identity. Materialize large values explicitly and release it when done. */
export type TurnResult = Readonly<{
  readonly [turnResultBrand]: "NanocodexTurnResult";
  finalMessage: string;
  /** Materializes the portable checkpoint this turn committed. */
  checkpoint(): Promise<SessionCheckpoint>;
  usage(): Promise<TurnUsage>;
  dispose(): void;
}>;

import type { NamedTool, ToolContext, ToolMap } from "nanocodex-tools";
export type {
  NamedTool,
  SubagentToolContext,
  Tool,
  ToolContext,
  ToolMap,
} from "nanocodex-tools";

/** Static JavaScript tools, optionally composed with Rust-backed extensions. */
export type ToolConfiguration<Extension = never> =
  | ToolMap
  | readonly (NamedTool | Extension)[]
  | import("./tools/Tools.mjs").Tools;

export type CodeEvaluatorEnvironment = {
  /** Trusted session identity; lets shared evaluators schedule child agents independently. */
  sessionId?: string;
  tools: Readonly<Record<string, (input: unknown) => Promise<unknown>>>;
  toolDefinitions: readonly Record<string, unknown>[];
  text(value: unknown): void;
  image(value: unknown, detail?: string): void;
  generatedImage(value: unknown): void;
  audio(value: unknown): void;
  notify(value: unknown): void;
  yield_control(): void;
  setTimeout(callback: () => void, delayMs?: number): number;
  clearTimeout(timerId?: number): void;
  store(key: string, value: unknown): void;
  load(key: string): unknown;
  exit(): never;
  require?: unknown;
  console?: Console;
  /** Aborts the exact active Code Mode cell and all nested tool calls. */
  signal: AbortSignal;
};

export type CodeEvaluator = (
  source: string,
  environment: CodeEvaluatorEnvironment,
) => void | Promise<void>;

/** Trusted host-owned receipts for direct application tools and nested Code Mode effects. Never supplied by guest source. */
export type CodeEffectContext = Readonly<{
  sessionId: string;
  /** Host turn metadata only; not canonical identity for effect replay. */
  turnId?: string;
  /** Original durable Rust operation key, or unique accepted-input scope for a deliberately non-durable invocation. */
  operationId?: string;
  /** Original model-call ordinal; provider call IDs may repeat within an operation. */
  modelCallIndex?: number;
  /** Direct tools use parentCallId = callId; Code Mode uses its cell ID and nested ordinal. */
  parentCallId: string;
  callId: string;
  name: string;
  /** Exact admitted guest source, or `host-tool:<name>` for a direct application tool. Included in the host fingerprint, never a new instruction. */
  source: string;
  input: unknown;
}>;
/** JSON wire receipt, bounded to 8 MiB/32,768 entries before host output copies.
 * Optional references deduplicate identical payloads within this receipt only.
 * Replay expands outputJsonRef or structuredResultRef first, then valueRef. */
export type CodeEffectReceipt = Readonly<{
  output: unknown;
  /** Compact derived JSON text; output is null and restores JSON.stringify(structured_result). */
  outputJsonRef?: "structured_result";
  structured_result: unknown;
  /** Compact wire alias; structured_result is null and restores output on replay. */
  structuredResultRef?: "output";
  success: boolean;
  metadata: unknown;
  /** Direct receipts always use null: no guest value is exposed. */
  value: unknown;
  /** JSON has no undefined value; this restores a fulfilled/rejected undefined. */
  valueUndefined?: boolean;
  /** Compact wire alias; value is null and restores this receipt field on replay. */
  valueRef?: "output" | "structured_result";
  /** Direct receipts always use false, including failed handler results. */
  thrown: boolean;
  failure?: unknown;
}>;
/** Admission must durably retain intent; completion must durably retain the exact receipt.
 * A recovered intent without an outcome is unknown, never permission to execute again.
 * Keys must scope [sessionId, operationId ?? "", modelCallIndex ?? 0, parentCallId, callId]; validate identity/input and fence concurrent runtime generations. */
export type CodeEffectJournal = Readonly<{
  /** Optional host-owned public-cell ledger. register must persist the original
   * operation/model/parent mapping before execution or any yielded ID escapes.
   * record durably saves an observation before acknowledgement. recover is
   * read-only and session-scoped: return terminal evidence or
   * explicitly historical receipt evidence (nested_calls must be empty), never resume/reexecute guest source.
   * Returned JSON uses the existing CodeModeExecution wire schema. */
  observations?: Readonly<{
    register(context: CodeEffectContext, cellId: string): Promise<void>;
    record(sessionId: string, cellId: string, encodedObservation: string): Promise<void>;
    recover(sessionId: string, cellId: string): Promise<string | null>;
  }>;
  /** Optional durable cell store protocol; provide both methods together.
   * Context uses name="code-cell", callId=parentCallId, input=null. beginCell
   * pins and returns immutable starting entries before evaluation. commitStore
   * merges only writes, once per canonical cell identity (including failed scripts).
   * Replaying an older committed cell must never overwrite newer session writes.
   * Entries are JSON snapshots, bounded to 8 MiB/32,768 nodes. Missing legacy
   * state must fail closed if prior effects make the starting state unprovable.
   * Deterministic conflicts/corruption throw code="CODE_EFFECT_UNKNOWN";
   * unclassified transport/storage exceptions remain retryable interruptions. */
  beginCell?(context: CodeEffectContext): Promise<readonly (readonly [string, unknown])[]>;
  commitStore?(context: CodeEffectContext, writes: readonly (readonly [string, unknown])[]): Promise<void>;
  begin(context: CodeEffectContext): Promise<
    | { status: "execute" }
    | { status: "replay"; receipt: CodeEffectReceipt }
    | { status: "unknown" }
  >;
  complete(context: CodeEffectContext, receipt: CodeEffectReceipt): Promise<void>;
}>;

declare const mcpPaymentBrand: unique symbol;
declare const lazyMcpPaymentBrand: unique symbol;

/** MCP payment options returned by `mcpPayment()` from `nanocodex/tempo`. */
export type PaidMcpPayment = McpPayment & { readonly [mcpPaymentBrand]: true };

/** Deferred payment setup. Methods and context exist only after its factory resolves. */
export type LazyPaidMcpPayment = { readonly [lazyMcpPaymentBrand]: true };

export type McpPayment = {
  /** MPPx client methods, such as `tempo.session({ account, getClient, channelStore })`. */
  methods: readonly unknown[];
  /** Static MPP method context, or an async per-tool function called before MCP execution.
   * A function receives the remote tool, ToolContext, and its connected MCP client. */
  context?: unknown;
  /** Called before MPPx creates a payment credential. */
  onPaymentRequired?: ((challenge: unknown) => boolean | Promise<boolean>) | undefined;
  orderChallenges?: ((challenges: readonly unknown[]) => readonly unknown[] | Promise<readonly unknown[]>) | undefined;
  paymentPreferences?: unknown;
};

export type McpClient = {
  listTools(params?: { cursor?: string | undefined }, options?: Record<string, unknown>): Promise<{
    tools: readonly McpTool[];
    nextCursor?: string | undefined;
  }>;
  callTool(
    params: { name: string; arguments?: Record<string, unknown> | undefined },
    resultSchema?: unknown,
    options?: Record<string, unknown>,
  ): Promise<unknown>;
};

export type McpTool = {
  name: string;
  title?: string | undefined;
  description?: string | undefined;
  inputSchema?: Record<string, unknown> | undefined;
  annotations?: Readonly<Record<string, unknown>> & {
    readOnlyHint?: boolean | undefined;
  } | undefined;
};

/** Trusted host interception; callbacks must never log or throw private values. */
export type McpPrivateResultPolicy = {
  /** Runs after payment context validation and before remote execution; throw to reject a call.
   * privateContext is trusted state scoped to this invocation. An own result
   * property replays an already-safe receipt, bypassing dispatch and transform. */
  beforeCall?: ((call: { name: string; arguments: Record<string, unknown> }, context: ToolContext | undefined) => void | { privateContext?: unknown; result?: unknown } | Promise<void | { privateContext?: unknown; result?: unknown }>) | undefined;
  /** Receives the raw result and this invocation's preflight state (undefined when
   * beforeCall is absent). Returns ONLY model-safe MCP content. Private state is
   * never included in tool results or tracing unless this callback returns it. */
  transformResult: (call: { name: string; arguments: Record<string, unknown>; result: unknown; privateContext: unknown }, context: ToolContext | undefined) => unknown | Promise<unknown>;
};

export type McpServer = {
  /** Public Streamable HTTP MCP endpoint. Omit when supplying an initialized client. */
  url?: string | URL | undefined;
  /** Existing MCP SDK-compatible client; Nanocodex does not close caller-owned clients. */
  client?: McpClient | undefined;
  description?: string | undefined;
  headers?: HeadersInit | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  /** Created with `mcpPayment()` from `nanocodex/tempo` (requires the `mppx` peer). */
  payment?: PaidMcpPayment | LazyPaidMcpPayment | undefined;
  /** Host-only interception before all result projections. Errors are replaced with
   * fixed failures. Caller-owned clients/fetch functions remain trusted and must
   * not independently log results, notifications, progress, or exceptions. */
  privateResult?: McpPrivateResultPolicy | undefined;

  enabledTools?: readonly string[] | undefined;
  disabledTools?: readonly string[] | undefined;
  /** Declares every remote tool on this server safe for concurrent nested calls. */
  supportsParallelToolCalls?: boolean | undefined;
  /** Declares specific remote tool names safe for concurrent nested calls. */
  parallelTools?: readonly string[] | undefined;
  /** Synchronously reports whether this server may currently be discovered or called. */
  isAvailable?: (() => boolean) | undefined;
  startupTimeoutMs?: number | undefined;
  timeoutMs?: number | undefined;
};

export type McpServers = Record<string, string | URL | McpServer>;

/** A paid WebSocket session, such as an mppx Tempo session manager. */
export type MppSession = {
  ws(endpoint: string | URL): Promise<MppWebSocket>;
  close?(): unknown | Promise<unknown>;
};

export type MppWebSocket = {
  readonly readyState: number;
  readonly bufferedAmount?: number | undefined;
  addEventListener(type: string, listener: (event: any) => void, options?: unknown): void;
  send(message: string): void;
  close(code?: number, reason?: string): void;
};
