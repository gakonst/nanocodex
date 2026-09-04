export type Thinking = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type ReasoningMode = "standard" | "pro";
export type Model = "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-6-astra";

export type PromptItem =
  | { type: "text"; text: string }
  | { type: "image"; image_url: string; detail?: "auto" | "low" | "high" | "original" | undefined }
  | { type: "audio"; audio_url: string };

export type PromptInput = string | readonly PromptItem[];

export type AgentEvent = {
  protocol_version: number;
  request_id: string;
  seq: number;
  type: string;
  payload: Record<string, unknown>;
};

export type AgentOptions = {
  /** Replaces the selected model's built-in instructions. */
  instructions?: string | undefined;
  /** Appends host instructions while retaining the selected model's prompt. */
  additionalInstructions?: string | undefined;
  model?: Model | undefined;
  reasoningMode?: ReasoningMode | undefined;
  fastMode?: boolean | undefined;
  sessionId?: string | undefined;
  thinking?: Thinking | undefined;
  workspace?: string | undefined;
  resume?: SessionSnapshot | undefined;
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
  format: "nanocodex-durability-state-v1";
  stateId: string;
}>;

export type DurabilityExportCursor = string;

/** One deterministic page of the total-state replacement from `from` (exclusive) to `to` (inclusive). */
export type DurabilityPortableStatePage = Readonly<{
  format: "nanocodex-durability-state-page-v1";
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

export type DurabilityReplaceRequest = Readonly<{
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
  importState(
    stateId: string,
    state: DurabilityStoredState,
    options?: Readonly<{
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
  service_tier: "standard" | "priority" | "fast";
}>;

export type CostStatus =
  | "estimated_from_usage"
  | "usage_not_reported"
  | "other";

export type SessionSnapshot = Readonly<{
  version: number;
  model: string;
  lineage_id: string;
  prompt_cache_key: string;
  workspace: string;
  request_prefix?: readonly Record<string, unknown>[] | undefined;
  canonical_context: Record<string, unknown>;
  history: readonly Record<string, unknown>[];
}>;

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

export type ForkOptions = Readonly<{ at?: TurnResult | undefined }>;
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
    appendDeveloperMessage(text: string): Promise<AgentSessionContext>;
    compact(): Promise<void>;
    context(): Promise<AgentSessionContext>;
    fork(options?: ForkOptions): Promise<DefaultAgent>;
    setModel(model: Model): Promise<void>;
    setFastMode(enabled: boolean): Promise<void>;
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
  steer(options: { input: PromptInput }): Promise<void>;
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
  steer(options: { input: PromptInput }): Promise<void>;
  cancel(): Promise<void>;
  /** Releases this handle without cancelling its accepted turn. */
  dispose(): void;
}>;

declare const turnResultBrand: unique symbol;
/** Opaque completed-turn identity. Materialize large values explicitly and release it when done. */
export type TurnResult = Readonly<{
  readonly [turnResultBrand]: "NanocodexTurnResult";
  finalMessage: string;
  snapshot(): Promise<SessionSnapshot>;
  usage(): Promise<TurnUsage>;
  dispose(): void;
}>;

import type { NamedTool, ToolMap } from "nanocodex-tools";
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
  tools: Readonly<Record<string, (input: unknown) => Promise<unknown>>>;
  toolDefinitions: readonly Record<string, unknown>[];
  text(value: unknown): void;
  image(value: unknown, detail?: string): void;
  generatedImage(value: unknown): void;
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

export type McpPayment = {
  /** MPPx client methods, such as `tempo.session({ account, getClient, channelStore })`. */
  methods: readonly unknown[];
  /** Optional MPP method context forwarded for each paid MCP tool call. */
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

export type McpServer = {
  /** Public Streamable HTTP MCP endpoint. Omit when supplying an initialized client. */
  url?: string | URL | undefined;
  /** Existing MCP SDK-compatible client; Nanocodex does not close caller-owned clients. */
  client?: McpClient | undefined;
  description?: string | undefined;
  headers?: HeadersInit | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  payment?: McpPayment | undefined;
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
