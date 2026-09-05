import type { Model, PromptInput, ReasoningMode, Thinking, TurnUsage } from "../types.mjs";
import type { AgentId } from "../runtime/subagents.mjs";

export type HistorySource = Readonly<{ turn_id: string; cursor: string }>;
export type HistoryCitation = Readonly<{
  thread_id: string;
  title: string;
  sources: readonly HistorySource[];
}>;
export type SessionSearchHit = Readonly<{
  session_id: string;
  title: string;
  turn_id: string;
  cursor: string;
  score: number;
  snippet: string;
}>;
export type FindSessionsRequest = Readonly<{
  query: string;
  limit?: number | undefined;
}>;
export type FindSessionsResponse = Readonly<{
  query: string;
  results: readonly SessionSearchHit[];
  citations: readonly HistoryCitation[];
}>;
export type ReadSessionRequest = Readonly<{
  session_id: string;
  turn_ids?: readonly string[] | undefined;
}>;
export type SessionTurn = Readonly<{
  session_id: string;
  title: string;
  turn_id: string;
  cursor: string;
  user: string;
  assistant: string;
}>;
export type ReadSessionResponse = Readonly<{
  turns: readonly SessionTurn[];
  citations: readonly HistoryCitation[];
}>;
export type MemoryKey = Readonly<{ id: number; version: number }>;
export type MemoryRecord = Readonly<{
  key: MemoryKey;
  content: string;
  created_at_ms: number;
  updated_at_ms: number;
  last_scanned_at_ms: number | null;
  scan_count: number;
  last_used_at_ms: number | null;
  use_count: number;
  probation_until_ms: number | null;
}>;
export type MemoryCandidate = Readonly<{
  key: MemoryKey;
  preview: string;
  score: number;
}>;
export type MemoryScanOperation = Readonly<{
  operation: "scan";
  query: string;
  limit?: number | undefined;
}>;
export type MemoryReadOperation = Readonly<{ operation: "read"; keys: readonly MemoryKey[] }>;
export type MemoryPutOperation = Readonly<{
  operation: "put";
  content: string;
  replace?: MemoryKey | undefined;
}>;
export type MemoryDeleteOperation = Readonly<{ operation: "delete"; key: MemoryKey }>;
export type MemoryOperation =
  | MemoryScanOperation
  | MemoryReadOperation
  | MemoryPutOperation
  | MemoryDeleteOperation;
export type MemoryScanResult = Readonly<{
  operation: "scan";
  abstained: boolean;
  candidates: readonly MemoryCandidate[];
}>;
export type MemoryReadResult = Readonly<{
  operation: "read";
  memories: readonly MemoryRecord[];
}>;
export type MemoryPutResult = Readonly<{
  operation: "put";
  memory: MemoryRecord;
  replaced: boolean;
}>;
export type MemoryDeleteResult = Readonly<{ operation: "delete"; key: MemoryKey }>;
export type MemoryResult =
  | MemoryScanResult
  | MemoryReadResult
  | MemoryPutResult
  | MemoryDeleteResult;

export type Organization = Readonly<{
  id: string;
  name: string | null;
  rootTeam: Readonly<{ id: string; name: string | null }>;
  authorizationEpoch: number;
  createdAt: number;
  updatedAt: number;
}>;
export type OrganizationUpdate = Readonly<{ name: string | null }>;

export type Options = Readonly<{
  /** Managed service origin. Defaults to the current browser origin. */
  baseUrl?: string | URL | undefined;
  /** Server credential. Browsers omit this and authenticate with the account cookie. */
  apiKey?: string | undefined;
  /** Platform-compatible fetch implementation, primarily for non-browser hosts and tests. */
  fetch?: typeof globalThis.fetch | undefined;
  /** WebSocket factory for Node/RN. Authorization is supplied only to this private handshake callback. */
  toolsTransport?: ((target: URL, options: Readonly<{
    headers?: Readonly<Record<string, string>>;
    credentials?: "include";
  }>) => import("../tools/Tools.mjs").AttachmentSocket | Promise<import("../tools/Tools.mjs").AttachmentSocket>) | Readonly<{
    connect(target: URL, options: Readonly<{
      headers?: Readonly<Record<string, string>>;
      credentials?: "include";
    }>): import("../tools/Tools.mjs").AttachmentSocket | Promise<import("../tools/Tools.mjs").AttachmentSocket>;
  }> | undefined;
}>;

export type CreateSettings = Readonly<{
  model: Model;
  thinking: Thinking;
  reasoningMode: ReasoningMode;
  fastMode: boolean;
}>;

export type SettingsPatch = Readonly<Partial<CreateSettings>>;

export type CreateOptions = Options & Readonly<{
  /** Complete immutable starting policy. GPT-6 Astra requires at least low reasoning. */
  settings?: CreateSettings | undefined;
}>;

export type Capabilities = Readonly<{
  durable_turns: true;
  resumable_events: true;
  live_steer: true;
  live_cancel: true;
  workspace: "cloudflare-computer";
  /** Tools can target explicit sandbox and connected-user environments. */
  execution_environments: true;
  /** Canonical commands select an execution hand from the root of their logical cwd. */
  execution_namespace: "cwd-root-v1";
  /** Native processes cannot yet access peer mounts through filesystem syscalls. */
  native_cross_mounts: false;
}>;

export type State = Readonly<{
  agent_id: string;
  session_id: string;
  has_snapshot: boolean;
  completed_turns: number;
  accepted_turns: number;
  last_active: number;
  active_turns: readonly string[];
  active_turn_details: readonly Readonly<{ id: string; input: PromptInput }>[];
  agent_loaded: boolean;
  connected_clients: number;
  capabilities: Capabilities;
  latest_event_cursor: string;
  stream_error: string | null;
  settings: Readonly<{
    model: Model;
    thinking: Thinking;
    reasoning_mode: ReasoningMode;
    fast_mode: boolean;
  }>;
}>;

export type Summary = Readonly<{
  title: string;
  createdAt: number;
  updatedAt: number;
  turnCount: number;
}>;

export type TurnState =
  | "accepted"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed";

export type TurnView = Readonly<{
  turn_id: string;
  state: TurnState;
  input: PromptInput;
  accepted_cursor: string;
  terminal_cursor: string | null;
  created_at: number;
  accepted_at: number;
  updated_at: number;
  attempt_count: number;
  retry_at: number | null;
  error?: string | undefined;
  terminal?: TerminalEventData | undefined;
}>;

export type EventData = Readonly<{
  cursor: string;
  created_at: number;
  turn_id: string | null;
}> & (
  | { type: "agent_created"; agent_id: string; capabilities: Capabilities }
  | { type: "turn_accepted"; id: string; input: PromptInput; replayed: boolean }
  | { type: "turn_cancelling"; id: string; error?: string; retry_at?: number }
  | CompletedEventData
  | { type: "turn_cancelled"; id: string }
  | { type: "turn_retryable"; id: string; error: string }
  | { type: "turn_failed"; id: string; error: string }
  | { type: "event"; event: unknown; agent_id?: AgentId | undefined }
  | { type: "stream_failed"; error: string }
);

export type CompletedEventData = Readonly<{
  type: "turn_completed";
  id: string;
  final_message: string;
  usage: TurnUsage | null;
  citations: readonly HistoryCitation[];
  usage_error?: string | undefined;
}>;

export type TerminalEventData =
  | CompletedEventData
  | Readonly<{ type: "turn_cancelled"; id: string }>
  | Readonly<{ type: "turn_failed"; id: string; error: string }>;

export type Event = Readonly<{
  cursor: string;
  createdAt: number | undefined;
  turnId: string | null;
  type: EventData["type"] | string;
  data: EventData;
}>;

export type WatchEventsOptions = Readonly<{
  /** Resume after a durable decimal cursor, or tail atomically from `"latest"`. Defaults to `"0"`. */
  cursor?: string | "latest" | undefined;
  signal?: AbortSignal | undefined;
}>;

export type EventHistoryOptions = Readonly<{
  /** Fetch events strictly before this durable cursor. Omit for the newest page. */
  before?: string | undefined;
  /** Page size from 1 through 256. Defaults to 128. */
  limit?: number | undefined;
  signal?: AbortSignal | undefined;
}>;

export type EventHistoryPage = Readonly<{
  data: readonly Event[];
  hasMore: boolean;
  /** Cursor captured with the page; attach the live watcher strictly after it. */
  latestCursor: string;
}>;

export type PromptOptions = Readonly<{
  input: PromptInput;
  /** Stable request key. A random key is generated when omitted. */
  idempotencyKey?: string | undefined;
  /** Optional stable turn identifier. */
  id?: string | undefined;
  signal?: AbortSignal | undefined;
}>;

export type TurnResult = Readonly<{
  turnId: string;
  finalMessage: string;
  usage: TurnUsage | null;
  citations: readonly HistoryCitation[];
  usageError?: string | undefined;
  cursor?: string | undefined;
}>;

export type TurnResultOptions = Readonly<{
  /** Cancels only this result observer; it never cancels the durable server turn. */
  signal?: AbortSignal | undefined;
}>;

export type Turn = Readonly<{
  idempotencyKey: string;
  accepted(): Promise<string>;
  state(): Promise<TurnView>;
  steer(options: Readonly<{ input: PromptInput }>): Promise<Readonly<{ turn_id: string; state: "steering" }>>;
  /** With a caller-supplied prompt ID, cancellation does not wait for the prompt response. */
  cancel(): Promise<TurnView | Readonly<{ turn_id: string; state: "cancelling" }>>;
  result(options?: TurnResultOptions): Promise<TurnResult>;
}>;

export type CronTriggerConfig = Readonly<{
  /** Five-field cron expression, with minute precision. */
  cron: string;
  /** IANA time zone. Defaults to UTC. */
  timezone?: string | undefined;
  /** Text prompt submitted for each occurrence, at most 64 KiB. */
  input: string;
  /** Defaults to true; false pauses future occurrences. */
  enabled?: boolean | undefined;
}>;

export type CronTrigger = Readonly<{
  id: string;
  cron: string;
  timezone: string;
  input: string;
  enabled: boolean;
  /** Unix milliseconds; null while paused. */
  next_run_at: number | null;
  /** Scheduled time of the last accepted occurrence, in Unix milliseconds. */
  last_run_at: number | null;
  last_turn_id: string | null;
  last_skipped_at: number | null;
  created_at: number;
  updated_at: number;
}>;

export type Agent = Readonly<{
  type: "managed";
  id: string;
  /** Account-owned list metadata, present on handles returned by `list()`. */
  summary?: Summary | undefined;
  turn: Readonly<{ prompt(options: PromptOptions): Turn }>;
  settings: Readonly<{
    read(): Promise<CreateSettings>;
    update(patch: SettingsPatch): Promise<CreateSettings>;
  }>;
  triggers: Readonly<{
    list(): Promise<readonly CronTrigger[]>;
    get(id: string): Promise<CronTrigger>;
    /** Create or replace an account-owned schedule using a stable id. */
    put(id: string, config: CronTriggerConfig): Promise<CronTrigger>;
    delete(id: string): Promise<void>;
  }>;
  /** Reverse-tool endpoint with cookie/bearer transport retained in a private closure. */
  toolsTarget(): import("../tools/Tools.mjs").AttachmentTarget;
  events: Readonly<{
    page(options?: EventHistoryOptions): Promise<EventHistoryPage>;
    /**
     * Each iterator has a private 4,096-event/32-MiB buffer. A lagging iterator
     * fails with an actionable resume cursor instead of dropping durable events.
     */
    watch(options?: WatchEventsOptions): AsyncIterableIterator<Event>;
  }>;
  state(): Promise<State>;
  delete(): Promise<void>;
}>;

export function create(options?: CreateOptions): Promise<Agent>;
export function list(options?: Options): Promise<readonly Agent[]>;
export function get(id: string, options?: Options): Promise<Agent>;
/** Open a handle immediately; each subsequent operation verifies ownership server-side. */
export function open(id: string, options?: Options): Agent;
export function remove(id: string, options?: Options): Promise<void>;
export { remove as delete };
export function findSessions(request: FindSessionsRequest, options?: Options): Promise<FindSessionsResponse>;
export function readSession(request: ReadSessionRequest, options?: Options): Promise<ReadSessionResponse>;
/** List the authenticated account's hosted durable memory. */
export function listMemories(options?: Options): Promise<readonly MemoryRecord[]>;
/** Compare-and-swap delete one hosted durable memory; deleting an absent id is idempotent. */
export function deleteMemory(key: MemoryKey, options?: Options): Promise<void>;
export function memory(operation: MemoryScanOperation, options?: Options): Promise<MemoryScanResult>;
export function memory(operation: MemoryReadOperation, options?: Options): Promise<MemoryReadResult>;
export function memory(operation: MemoryPutOperation, options?: Options): Promise<MemoryPutResult>;
export function memory(operation: MemoryDeleteOperation, options?: Options): Promise<MemoryDeleteResult>;
export function memory(operation: MemoryOperation, options?: Options): Promise<MemoryResult>;
export function getOrganization(options?: Options): Promise<Organization>;
export function updateOrganization(request: OrganizationUpdate, options?: Options): Promise<Organization>;
