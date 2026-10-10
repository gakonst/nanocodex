import type {
  Agent as BaseAgent,
  AgentActions,
  AgentOptions,
  AgentEvent,
  DefaultAgent,
  ToolConfiguration,
  SessionCheckpoint,
} from "../types.mjs";
import type { CloudflareDurableObjectStorage } from "../runtime/cloudflare-durability-store.mjs";
import type { Tool as SubagentTool } from "../runtime/subagents.mjs";
import type {
  DurabilityExportPageRequest,
  DurabilityPortableStateArchive,
  DurabilityPortableStatePage,
  DurabilityStoredState,
} from "../types.mjs";

export type DurableObjectContext = Readonly<{
  storage: CloudflareDurableObjectStorage;
  acceptWebSocket(socket: WebSocket, tags?: string[]): void;
  getWebSockets(tag?: string): WebSocket[];
}>;

/** The owning Cloudflare Durable Object instance. Runtime fields remain adapter-private. */
export type DurableObjectOwner = object;

export type EventFrame = Readonly<{
  cursor: string;
  event: AgentEvent;
}> | Readonly<{
  type: "replay_paused";
  cursor: string;
  latest_cursor: string;
}>;

type CloudflareAgentActions = Omit<AgentActions, "events" | "turn"> & Readonly<{
  events: AgentActions["events"] & Readonly<{
    /** Accepts a read-only hibernatable event socket; reconnect from the last event or replay pause cursor. */
    connect(request: Request): Response;
  }>;
  turn: AgentActions["turn"];
}>;

/** A durable Agent whose Cloudflare event socket survives typed extensions. */
export type Agent<extended extends object = {}> =
  Omit<BaseAgent<CloudflareAgentActions & extended>, "extend"> & Readonly<{
    extend<const extension extends object>(
      decorator: (agent: Agent<extended>) => extension,
    ): Agent<extended & extension>;
  }>;

/** Copies the exact latest committed model boundary; rejects before the first safe boundary. */
export function checkpoint(agent: Agent): Promise<SessionCheckpoint>;

/** Removes the package-owned durable history for one Cloudflare Agent. */
export function destroy(owner: DurableObjectOwner): void;

/**
 * One Cloudflare Agent's portable durable session: its root state and, when
 * the root has a durable task tree, the complete `<stateId>:subagents`
 * task-tree journal state. Importing restores both atomically.
 */
export type DurabilityPortableSessionArchive = DurabilityPortableStateArchive & Readonly<{
  subagents?: DurabilityPortableStateArchive | undefined;
}>;

/** Selects the root state, or with `subagents: true` its task-tree journal state. */
export type CloudflareDurabilityExportPageRequest = DurabilityExportPageRequest & Readonly<{
  subagents?: true | undefined;
}>;

/** Execution head for a host that transfers its immutable root records separately; the task-tree journal travels complete. */
export function exportDurabilityHead(owner: DurableObjectOwner): Promise<DurabilityPortableSessionArchive>;

/** Fences and exports this inactive Cloudflare Agent's provider-neutral session, including its task tree. */
export function exportDurabilityState(
  owner: DurableObjectOwner,
): Promise<DurabilityPortableSessionArchive>;

/** Fences once and exports one resumable page of an exact revision range of the root or its task-tree journal. */
export function exportDurabilityState(
  owner: DurableObjectOwner,
  request: CloudflareDurabilityExportPageRequest,
): Promise<DurabilityPortableStatePage>;

/** Imports a provider-neutral session, including any task-tree journal, into a pristine Cloudflare Agent owner. */
export function importDurabilityState(
  owner: DurableObjectOwner,
  archive: DurabilityPortableSessionArchive,
): Promise<DurabilityStoredState>;

/** Prunes old terminal receipts before constructing the full Agent runtime. */
export function pruneDurableReceipts(
  owner: DurableObjectOwner,
  options?: Readonly<{ terminalReceiptRetention?: number | undefined }>,
): Promise<void>;

/**
 * @internal Reopens an idle root Responses WebSocket before the next turn.
 * Returns false when one is already open or the Agent uses HTTP inference.
 */
export function prepareTransport(agent: Agent): boolean;

/**
 * @internal Live-input seam for realtime voice hosts: atomically steers the
 * active turn or starts a new one, for either harness. Applications use the
 * shared agent.turn.prompt() and turn.steer() actions.
 */
export function route(
  agent: Agent,
  options: { input: string },
): Promise<import("../types.mjs").Turn | undefined>;

/** Creates one durable Agent from its owning Durable Object instance. */
export function create(owner: create.Owner, options?: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type Owner = DurableObjectOwner;
  type Options = Readonly<{
    /** Worker-compatible isolated evaluator; required unless the owning runtime supplies one. */
    codeEvaluator?: import("../types.mjs").CodeEvaluator;
    /** Observer-only instant steering; disabled unless explicitly enabled. */
    instantToolSteering?: boolean | undefined;
    /** Awaited host preservation barrier; scoped to this root, never inherited by children. */
    beforeCompaction?: AgentOptions["beforeCompaction"];
    /** Stable portable state identity. It cannot change after first construction or import. */
    durabilityId?: string | undefined;
    /**
     * `durable` retains the adapter's resumable event socket. `caller` leaves
     * event retention to the embedding Durable Object and disables connect().
     */
    eventPersistence?: "durable" | "caller" | undefined;
    instructions?: string | undefined;
    /** Appends host instructions while retaining the selected model's prompt. */
    additionalInstructions?: string | undefined;
    /**
     * Bounds terminal receipts retained in the hot Rust state checkpoint.
     * The caller must preserve older exact-ID results before selecting this.
     */
    terminalReceiptRetention?: number | undefined;
    /** Child topology and history use the root's separate durable subagent journal. */
    tools?: ToolConfiguration<SubagentTool> | undefined;
  }>;
  type ReturnType = Agent;
}

/** Creates one non-durable Rust/WASM Agent in the current Cloudflare isolate. */
export function createEphemeral(
  owner: createEphemeral.Owner,
  options: createEphemeral.Options,
): Promise<createEphemeral.ReturnType>;
export declare namespace createEphemeral {
  type Owner = DurableObjectOwner;
  type Options = Readonly<AgentOptions & {
    /** Worker-compatible isolated Code Mode evaluator. */
    codeEvaluator: import("../types.mjs").CodeEvaluator;
    /** Caller-owned tools available inside exec. */
    tools?: ToolConfiguration<SubagentTool> | undefined;
  }>;
  type ReturnType = DefaultAgent;
}

/** Reads a correlated receipt without acquiring or restoring the agent. */
export function steerReceipt(owner: DurableObjectOwner, operationId: string, messageId: string): Readonly<{
  input_key: string; index: number; withdrawn: boolean;
}> | null;
/** Fingerprint of the exact browser Prompt serialization retained by Rust. */
export function steerInputKey(input: import("../types.mjs").PromptInput): Promise<string>;
