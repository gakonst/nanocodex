import {
  HOSTED_TOOL_CALL_TIMEOUT_MS,
  HOSTED_TOOLS_LEASE_MS,
  MAX_HOSTED_TOOL_OUTPUT_BYTES,
  HostedToolsProtocolError,
  parseHostedToolsHostFrame,
  parseHostedToolsManagedFrame,
  type HostedToolCallOutcome,
  type HostedToolCatalogEntry,
  type HostedMachine,
  type HostedToolsHostFrame,
  type HostedToolsManagedFrame,
} from "./protocol.js";
import { hostedToolCatalogDigest } from "../../tools/hostedCatalog.mjs";

const SOCKET_TAG = "hosted-tools";
const INVALID_CONNECT_GRANT_ID = "invalid-connect-grant";
const DEFAULT_MAX_IN_FLIGHT = 64;
const OPEN = 1;
const MAX_RETAINED_RECEIPTS = 512;
const MAX_CALLS_PER_GENERATION = 512;
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const encoder = new TextEncoder();

/**
 * Marks the one case where an attached source is known to be absent before a
 * durable admission. The unified ToolRouter may then select the exact
 * same-name cloud contract. It must never infer this from an outcome message:
 * every other unavailable, cancellation, timeout, and ambiguous outcome is
 * pinned to the attached source and is final.
 */
export const HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE = Symbol.for(
  "nanocodex.tool.preDispatchUnavailable",
);

export type HostedToolsCallState =
  | "admitted"
  | "dispatched"
  | "completed"
  | "unavailable"
  | "ambiguous"
  | "cancelled";

export type HostedToolsStateRow = {
  generation: number;
  host_id: string | null;
  lease_id: string | null;
  lease_expires_at: number;
  catalog_json: string | null;
};

export type HostedToolsCallRow = {
  call_id: string;
  session_id: string;
  source_call_id: string;
  host_id: string;
  lease_id: string;
  generation: number;
  model: string;
  name: string;
  input_json: string;
  output_token_budget: number;
  output_byte_budget: number;
  deadline_at: number;
  cancel_requested: number;
  state: HostedToolsCallState;
  result_json: string | null;
  receipt_json: string | null;
};

type HostedToolsSocketAttachment = {
  kind: typeof SOCKET_TAG;
  sessionId: string;
  allowedMcpIds?: readonly string[];
  appToolCatalogDigest?: `0x${string}`;
  connectGrantId?: string;
  leaseId?: string;
  generation?: number;
  active?: true;
  draining?: true;
  machines?: readonly HostedMachine[];
};

type PendingCall = {
  leaseId: string;
  generation: number;
  deadlineAt: number;
  promise: Promise<HostedToolCallOutcome>;
  resolve(outcome: HostedToolCallOutcome): void;
  timeout?: ReturnType<typeof setTimeout>;
  removeAbort?: () => void;
};

export type HostedToolsSocket = {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
};

export type HostedToolsBrokerCoreContext = Readonly<{
  accept(socket: HostedToolsSocket): void;
  sockets(): HostedToolsSocket[];
  readAttachment(socket: HostedToolsSocket): unknown;
  writeAttachment(socket: HostedToolsSocket, value: unknown): void;
}>;

export type HostedToolsProviderDefinition = Readonly<HostedToolCatalogEntry>;

export type HostedToolsInvokeRequest = Readonly<{
  sessionId: string;
  callId: string;
  model: string;
  input: Record<string, unknown> | string;
  outputTokenBudget: number;
  outputByteBudget?: number;
  deadlineAt?: number;
  signal?: AbortSignal;
}>;

export type HostedToolsPreparedTool = Readonly<{
  connectGrantId?: string;
  appToolCatalogDigest?: string;
  entry: HostedToolCatalogEntry;
  invoke(request: HostedToolsInvokeRequest): Promise<HostedToolsInvocationOutcome>;
}>;

type HostedToolsCatalogBinding = Readonly<{
  connectGrantId?: string;
  appToolCatalogDigest?: string;
  hostId: string;
  leaseId: string;
  generation: number;
  entry: HostedToolCatalogEntry;
}>;

export type HostedToolsCodeDefinition = HostedToolCatalogEntry["definition"] & {
  defer_loading: true;
};

export type HostedToolsCatalogCandidate = Readonly<
  Omit<HostedToolCatalogEntry, "definition"> & { definition: HostedToolsCodeDefinition }
>;

export type HostedToolsCatalogValidator = (
  definitions: readonly HostedToolsCatalogCandidate[],
) => true;

export type HostedToolsCodeTool = Readonly<{
  name: string;
  parallelSafe: boolean;
  handler(
    input: unknown,
    context: { sessionId: string; callId: string; model?: string; signal?: AbortSignal },
  ): Promise<unknown>;
}>;

export interface HostedToolsDynamicProvider {
  definitions(): readonly HostedToolsCodeDefinition[];
  resolve(name: string): HostedToolsCodeTool | undefined;
  /** Installed by the owning ToolRouter to reject non-parity catalogs before ACK. */
  setCatalogValidator(validator: HostedToolsCatalogValidator | undefined): void;
}

/** Injectable durable call ledger boundary; the production default is Durable Object SQLite. */
export interface HostedToolsBrokerPersistence {
  initialize(now: number): HostedToolsStateRow | undefined;
  transaction<T>(callback: () => T): T;
  state(): HostedToolsStateRow;
  replaceHost(row: HostedToolsStateRow): void;
  clearHost(leaseId: string, generation: number): void;
  clearCatalog(leaseId: string, generation: number): void;
  call(callId: string): HostedToolsCallRow | undefined;
  callBySource(sessionId: string, sourceCallId: string): HostedToolsCallRow | undefined;
  insertCall(row: HostedToolsCallRow, now: number): void;
  markCancelRequested(callId: string, now: number): HostedToolsCallRow | undefined;
  transitionCall(
    callId: string,
    from: readonly HostedToolsCallState[],
    state: HostedToolsCallState,
    resultJson: string,
    now: number,
  ): HostedToolsCallRow | undefined;
  recordLateReceipt(callId: string, receiptJson: string, now: number): HostedToolsCallRow | undefined;
  markGenerationAmbiguous(leaseId: string, generation: number, resultJson: string, now: number): void;
  activeCallCount(leaseId: string, generation: number): number;
  generationCallCount(leaseId: string, generation: number): number;
  pruneReceipts(activeLeaseId: string | null, activeGeneration: number, limit: number): void;
}

export type HostedToolsBrokerCoreOptions = Readonly<{
  now?: () => number;
  randomUUID?: () => string;
  maxInFlight?: number;
  maxCallsPerGeneration?: number;
  persistence: HostedToolsBrokerPersistence;
  onCatalogChanged?: (definitions: readonly HostedToolsProviderDefinition[]) => void;
  entryAllowed?: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
  ) => boolean;
}>;

/** Owns one reverse-tool attachment over an injected socket and durable ledger. */
export class HostedToolsBrokerCore {
  readonly #provider: HostedToolsDynamicProvider;
  readonly #pending = new Map<string, PendingCall>();
  readonly #now: () => number;
  readonly #randomUUID: () => string;
  readonly #maxInFlight: number;
  readonly #maxCallsPerGeneration: number;
  readonly #persistence: HostedToolsBrokerPersistence;
  readonly #onCatalogChanged: ((definitions: readonly HostedToolsProviderDefinition[]) => void) | undefined;
  readonly #entryAllowed: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
  ) => boolean;
  #catalogValidator: HostedToolsCatalogValidator | undefined;
  #nextCandidateGeneration: number;

  constructor(
    readonly context: HostedToolsBrokerCoreContext,
    options: HostedToolsBrokerCoreOptions,
  ) {
    this.#now = options.now ?? Date.now;
    this.#randomUUID = options.randomUUID ?? (() => crypto.randomUUID());
    this.#maxInFlight = options.maxInFlight ?? DEFAULT_MAX_IN_FLIGHT;
    if (!Number.isSafeInteger(this.#maxInFlight) || this.#maxInFlight < 1
      || this.#maxInFlight > DEFAULT_MAX_IN_FLIGHT) {
      throw new TypeError(`maxInFlight must be an integer from 1 through ${DEFAULT_MAX_IN_FLIGHT}`);
    }
    this.#maxCallsPerGeneration = options.maxCallsPerGeneration ?? MAX_CALLS_PER_GENERATION;
    if (!Number.isSafeInteger(this.#maxCallsPerGeneration) || this.#maxCallsPerGeneration < 1
      || this.#maxCallsPerGeneration > MAX_CALLS_PER_GENERATION) {
      throw new TypeError(`maxCallsPerGeneration must be an integer from 1 through ${MAX_CALLS_PER_GENERATION}`);
    }
    this.#persistence = options.persistence;
    this.#onCatalogChanged = options.onCatalogChanged;
    this.#entryAllowed = options.entryAllowed ?? (() => true);
    const retired = this.#persistence.initialize(this.#now());
    this.#nextCandidateGeneration = this.#persistence.state().generation;
    for (const socket of this.context.sockets()) {
      const generation = this.#attachment(socket)?.generation;
      if (generation !== undefined) this.#nextCandidateGeneration = Math.max(this.#nextCandidateGeneration, generation);
    }
    if (retired?.lease_id) {
      for (const socket of this.context.sockets()) {
        const attachment = this.#attachment(socket);
        if (attachment?.leaseId !== retired.lease_id
          || attachment.generation !== retired.generation) continue;
        closeSocket(socket, 1012, "Hosted Tools owner restarted");
      }
    }
    this.#provider = Object.freeze({
      // ToolRouter owns the one aggregate tool_search. This provider exposes
      // only the current attached definitions, which stay deferred and can be
      // overlaid onto exact cloud contracts by that router.
      definitions: () => {
        const connectGrantId = this.#activeConnectGrantId();
        const appToolCatalogDigest = this.#activeAppToolCatalogDigest();
        return this.#definitions()
          .filter((binding) => this.#entryAllowed(binding, connectGrantId, appToolCatalogDigest))
          .map((binding) => Object.freeze({
            ...binding.definition,
            defer_loading: true as const,
          }));
      },
      resolve: (name: string) => {
        const prepared = this.#resolve(name);
        if (!prepared || !this.#entryAllowed(
          prepared.entry,
          prepared.connectGrantId,
          prepared.appToolCatalogDigest,
        )) return undefined;
        return Object.freeze({
          name,
          parallelSafe: prepared.entry.parallel_safe,
          provider: prepared.entry.provider,
          remoteName: prepared.entry.remote_name,
          summary: prepared.entry.summary,
          timeoutMs: prepared.entry.timeout_ms,
          handler: async (
            input: unknown,
            context: { sessionId: string; callId: string; model?: string; signal?: AbortSignal },
          ) => {
            if (!this.#entryAllowed(
              prepared.entry,
              prepared.connectGrantId,
              prepared.appToolCatalogDigest,
            )) {
              return toolResult("Hosted tool is outside the active grant", {
                status: "unavailable",
                message: "Hosted tool is outside the active grant",
              }, false, null);
            }
            const outcome = await prepared.invoke({
              sessionId: context.sessionId,
              callId: context.callId,
              model: context.model ?? "unknown",
              input: input as Record<string, unknown> | string,
              outputTokenBudget: 10_000,
              ...(context.signal === undefined ? {} : { signal: context.signal }),
            });
            if (outcome.status === "completed") return wireToolResult(outcome.output);
            const result = toolResult(outcome.message, outcome, false, null);
            return outcome[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE] === true
              ? Object.freeze({
                  ...(result as Record<PropertyKey, unknown>),
                  [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true,
                })
              : result;
          },
        });
      },
      setCatalogValidator: (validator: HostedToolsCatalogValidator | undefined) => {
        this.#catalogValidator = validator;
      },
    });
  }

  owns(socket: HostedToolsSocket): boolean { return this.handles(socket); }

  async message(socket: HostedToolsSocket, message: string): Promise<void> {
    await this.webSocketMessage(socket, message);
  }

  close(socket: HostedToolsSocket, reason: string): void {
    if (this.handles(socket)) this.#retire(socket, reason);
  }

  shutdown(reason: string): void {
    const sockets = this.context.sockets();
    for (const socket of sockets) this.#fence(socket, reason);
    if (sockets.length === 0) this.#retireState(this.#persistence.state(), reason);
  }

  isReady(): boolean { return this.#definitions().length > 0; }

  hasPendingCalls(): boolean { return this.#pending.size > 0; }

  provider(): HostedToolsDynamicProvider { return this.#provider; }

  /** Returns the live, non-secret user-machine snapshot for the account-owned host. */
  machines(): readonly HostedMachine[] {
    const state = this.#persistence.state();
    const socket = this.#liveRoutingSocketForState(state);
    if (socket === undefined) return [];
    const attachment = this.#attachment(socket);
    if (attachment?.connectGrantId !== undefined) return [];
    return attachment?.machines ?? [];
  }

  accept(
    socket: HostedToolsSocket,
    sessionId: string,
    allowedMcpIds?: readonly string[],
    appToolCatalogDigest?: `0x${string}`,
    connectGrantId?: string,
  ): void {
    if (allowedMcpIds !== undefined && !isConnectGrantId(connectGrantId)) {
      throw new TypeError("Connect Hosted Tools requires an exact grant ID");
    }
    this.context.writeAttachment(socket, {
      kind: SOCKET_TAG,
      sessionId,
      ...(allowedMcpIds === undefined ? {} : { allowedMcpIds: [...allowedMcpIds] }),
      ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
      ...(connectGrantId === undefined ? {} : { connectGrantId }),
    } satisfies HostedToolsSocketAttachment);
    this.context.accept(socket);
  }

  handles(socket: HostedToolsSocket): boolean {
    return this.#attachment(socket)?.kind === SOCKET_TAG;
  }

  async webSocketMessage(socket: HostedToolsSocket, message: string | ArrayBuffer): Promise<void> {
    if (!this.handles(socket)) return;
    if (typeof message !== "string") {
      this.#fence(socket, "Hosted Tools requires bounded text frames", 1003);
      return;
    }
    let frame: HostedToolsHostFrame;
    try {
      frame = parseHostedToolsHostFrame(message);
      await this.#dispatchHostFrame(socket, frame);
    } catch (error) {
      const protocol = error instanceof HostedToolsProtocolError
        ? error
        : new HostedToolsProtocolError("broker_failure", errorMessage(error));
      this.#fence(socket, `${protocol.code}: ${protocol.message}`);
    }
  }

  webSocketClose(socket: HostedToolsSocket, code: number, reason: string): void {
    if (!this.handles(socket)) return;
    this.#retire(socket, reason || `peer closed with code ${code}`);
    closeSocket(socket, code, reason || "Hosted Tools peer closed");
  }

  webSocketError(socket: HostedToolsSocket): void {
    if (!this.handles(socket)) return;
    this.#retire(socket, "WebSocket failed");
    closeSocket(socket, 1011, "Hosted Tools WebSocket failed");
  }

  /** May be called by an owning alarm; normal reads and call timers also expire leases lazily. */
  expire(): void {
    const state = this.#persistence.state();
    if (!state.lease_id || state.lease_expires_at > this.#now()) return;
    const socket = this.#socketForState(state);
    if (socket) this.#fence(socket, "Hosted Tools lease expired");
    else this.#retireState(state, "Hosted Tools lease expired");
  }

  cancel(callId: string): boolean {
    const pending = this.#pending.get(callId);
    if (!pending) return false;
    const row = this.#persistence.call(callId);
    if (!row || row.state !== "dispatched") return false;
    const state = this.#persistence.state();
    const socket = this.#socketForState(state);
    if (!socket
      || row.lease_id !== state.lease_id
      || row.generation !== state.generation
      || state.lease_expires_at <= this.#now()) {
      this.#finishAmbiguous(row, "Hosted Tools cancellation lost its pinned attachment");
      return false;
    }
    const cancelRequested = this.#persistence.markCancelRequested(callId, this.#now());
    if (!cancelRequested || cancelRequested.state !== "dispatched"
      || cancelRequested.cancel_requested !== 1) return false;
    try {
      this.#send(socket, {
        type: "cancel",
        call_id: row.call_id,
      });
      return true;
    } catch {
      this.#retire(socket, "cancellation delivery failed");
      closeSocket(socket, 1011, "Hosted Tools cancellation delivery failed");
      return false;
    }
  }

  async #dispatchHostFrame(socket: HostedToolsSocket, frame: HostedToolsHostFrame): Promise<void> {
    if (frame.type === "catalog") await this.#publishCatalog(socket, frame);
    else if (frame.type === "ping") this.#heartbeat(socket, frame);
    else if (frame.type === "drain") this.#drain(socket);
    else this.#completeResult(socket, frame);
  }

  #activeAttachment(socket: HostedToolsSocket): HostedToolsSocketAttachment {
    const attachment = this.#attachment(socket);
    const state = this.#persistence.state();
    if (!attachment?.active || !attachment.leaseId || attachment.generation === undefined
      || state.lease_id !== attachment.leaseId
      || state.generation !== attachment.generation
      || state.lease_expires_at <= this.#now()) {
      throw new HostedToolsProtocolError("stale_socket", "socket no longer owns the tool attachment");
    }
    return attachment;
  }

  #heartbeat(
    socket: HostedToolsSocket,
    frame: Extract<HostedToolsHostFrame, { type: "ping" }>,
  ): void {
    const attachment = this.#activeAttachment(socket);
    const expiresAt = this.#now() + HOSTED_TOOLS_LEASE_MS;
    const state = this.#persistence.state();
    this.#persistence.replaceHost({ ...state, lease_expires_at: expiresAt });
    this.#send(socket, {
      type: "pong",
      nonce: frame.nonce,
    });
  }

  async #publishCatalog(
    socket: HostedToolsSocket,
    frame: Extract<HostedToolsHostFrame, { type: "catalog" }>,
  ): Promise<void> {
    const initial = this.#attachment(socket);
    if (!initial || initial.leaseId || initial.generation !== undefined || initial.active) {
      throw new HostedToolsProtocolError("catalog_immutable", "one immutable catalog is allowed per socket");
    }
    if (this.#nextCandidateGeneration >= Number.MAX_SAFE_INTEGER) {
      throw new HostedToolsProtocolError("generation_exhausted", "Hosted Tools generation is exhausted");
    }
    const state = this.#persistence.state();
    const activeSocket = this.#socketForState(state);
    const activeGrantId = activeSocket === undefined
      ? undefined
      : this.#activeConnectGrantId(state);
    if (activeSocket !== undefined && activeGrantId !== initial.connectGrantId) {
      throw new HostedToolsProtocolError(
        "grant_conflict",
        "another Connect grant already owns this agent's tool host",
      );
    }
    const generation = ++this.#nextCandidateGeneration;
    const leaseId = this.#randomUUID();
    const expiresAt = this.#now() + HOSTED_TOOLS_LEASE_MS;
    const candidate = {
      ...initial,
      leaseId,
      generation,
    } satisfies HostedToolsSocketAttachment;
    this.context.writeAttachment(socket, candidate);
    const catalogJson = JSON.stringify(frame.tools);
    const candidateDefinitions = frame.tools.map((entry) => Object.freeze({
      ...entry,
      definition: Object.freeze({
        ...entry.definition,
        defer_loading: true as const,
      }),
    }));
    try {
      if (initial.connectGrantId !== undefined && (frame.machines?.length ?? 0) > 0) {
        throw new Error("Connect tool hosts cannot publish account machine metadata");
      }
      if (initial.allowedMcpIds !== undefined) {
        if (!isConnectGrantId(initial.connectGrantId)) {
          throw new Error("Connect tool host is missing its exact grant binding");
        }
        const allowed = new Set(initial.allowedMcpIds);
        const forbiddenMcp = frame.tools.find((entry) => {
          const match = /^mcp:([A-Za-z0-9_-]{43})$/.exec(entry.provider);
          return entry.provider.startsWith("mcp:")
            && (match === null || !allowed.has(match[1]!));
        });
        if (forbiddenMcp) {
          throw new Error(
            `tool ${forbiddenMcp.provider}:${forbiddenMcp.remote_name} is not authorized by the Connect grant`,
          );
        }
        const appTools = frame.tools.filter((entry) => !entry.provider.startsWith("mcp:"));
        const candidateDigest = appTools.length === 0
          ? undefined
          : await hostedToolCatalogDigest(appTools);
        if (candidateDigest !== initial.appToolCatalogDigest) {
          throw new Error("the app-local tool catalog does not match the signed Connect grant");
        }
      }
      const validator = this.#catalogValidator;
      if (validator !== undefined && validator(candidateDefinitions) !== true) {
        throw new Error("ToolRouter rejected the candidate catalog");
      }
    } catch (error) {
      throw new HostedToolsProtocolError(
        "catalog_contract_mismatch",
        `candidate catalog is incompatible with the managed tool route: ${errorMessage(error)}`,
      );
    }
    const now = this.#now();
    const replaced = state.lease_id ? state : undefined;
    // A failed ready send must leave the previous attachment routable.
    this.#send(socket, { type: "ready" });
    this.#persistence.transaction(() => {
      if (state.lease_id) {
        this.#persistence.markGenerationAmbiguous(
          state.lease_id,
          state.generation,
          JSON.stringify(hostedToolsAmbiguous("Hosted Tools call became ambiguous when its host was replaced")),
          now,
        );
      }
      this.#persistence.replaceHost({
        generation,
        host_id: candidate.sessionId,
        lease_id: leaseId,
        lease_expires_at: expiresAt,
        catalog_json: catalogJson,
      });
    });
    if (replaced?.lease_id) {
      const outcome = hostedToolsAmbiguous("Hosted Tools call became ambiguous when its host was replaced");
      this.#resolveGeneration(replaced.lease_id, replaced.generation, outcome);
      for (const existing of this.context.sockets()) {
        if (existing === socket) continue;
        const old = this.#attachment(existing);
        if (!old?.active || old.leaseId !== replaced.lease_id || old.generation !== replaced.generation) continue;
        closeSocket(existing, 1008, "Hosted Tools attachment replaced");
      }
    }
    this.context.writeAttachment(
      socket,
      {
        ...candidate,
        active: true,
        ...(frame.machines === undefined ? {} : { machines: frame.machines }),
      } satisfies HostedToolsSocketAttachment,
    );
    this.#notifyCatalogChanged();
  }

  #drain(socket: HostedToolsSocket): void {
    const attachment = this.#activeAttachment(socket);
    if (attachment.draining) {
      throw new HostedToolsProtocolError("already_draining", "socket is already draining");
    }
    const state = this.#persistence.state();
    // Visibility is removed before the peer is told that draining began.
    this.#persistence.clearCatalog(state.lease_id!, state.generation);
    this.context.writeAttachment(
      socket,
      { ...attachment, draining: true } satisfies HostedToolsSocketAttachment,
    );
    this.#notifyCatalogChanged();
    this.#send(socket, { type: "draining" });
  }

  #completeResult(
    socket: HostedToolsSocket,
    frame: Extract<HostedToolsHostFrame, { type: "result" }>,
  ): void {
    const attachment = this.#activeAttachment(socket);
    const row = this.#persistence.call(frame.call_id);
    const stored = JSON.stringify(frame.outcome);
    if (!row
      || row.lease_id !== attachment.leaseId
      || row.generation !== attachment.generation) {
      throw new HostedToolsProtocolError("unknown_call", "result does not match an admitted pinned call");
    }
    if (row.state === "ambiguous") {
      const receiptJson = JSON.stringify({ type: "result", outcome: frame.outcome });
      const recorded = this.#persistence.recordLateReceipt(row.call_id, receiptJson, this.#now());
      if (!recorded || recorded.receipt_json !== receiptJson) {
        throw new HostedToolsProtocolError("result_conflict", "late terminal receipt conflicts with retained proof");
      }
      this.#ackResult(socket, frame);
      return;
    }
    if (row.state !== "dispatched") {
      if (row.result_json === stored && row.state === outcomeState(frame.outcome)) {
        this.#ackResult(socket, frame);
        return;
      }
      throw new HostedToolsProtocolError("result_conflict", "terminal call result cannot be changed");
    }
    if (this.#now() >= row.deadline_at) {
      this.#finishAmbiguous(row, "Hosted Tools call result arrived after its durable deadline");
      const receiptJson = JSON.stringify({ type: "result", outcome: frame.outcome });
      const recorded = this.#persistence.recordLateReceipt(row.call_id, receiptJson, this.#now());
      if (!recorded || recorded.receipt_json !== receiptJson) {
        throw new HostedToolsProtocolError("result_conflict", "late terminal receipt conflicts with retained proof");
      }
      this.#ackResult(socket, frame);
      return;
    }
    if (frame.outcome.status === "completed"
      && encoder.encode(JSON.stringify(frame.outcome.output)).byteLength > row.output_byte_budget) {
      throw new HostedToolsProtocolError(
        "output_budget_exceeded",
        "completed output exceeds the byte budget pinned to the call",
      );
    }
    const completed = this.#persistence.transitionCall(
      row.call_id,
      ["dispatched"],
      outcomeState(frame.outcome),
      stored,
      this.#now(),
    );
    if (!completed || completed.result_json !== stored) {
      throw new HostedToolsProtocolError("result_conflict", "call result lost durable ownership");
    }
    const pending = this.#takePending(row.call_id);
    this.#pruneReceipts();
    this.#ackResult(socket, frame);
    pending?.resolve(frame.outcome);
  }

  #ackResult(socket: HostedToolsSocket, frame: Extract<HostedToolsHostFrame, { type: "result" }>): void {
    try {
      this.#send(socket, {
        type: "ack",
        call_id: frame.call_id,
      });
    } catch {
      this.#retire(socket, "result acknowledgement delivery failed");
      closeSocket(socket, 1011, "Hosted Tools result acknowledgement failed");
    }
  }

  #definitions(): readonly HostedToolsProviderDefinition[] {
    const state = this.#persistence.state();
    if (!state.host_id || !state.lease_id || !state.catalog_json) return [];
    if (!this.#liveRoutingSocketForState(state)) return [];
    return JSON.parse(state.catalog_json) as HostedToolCatalogEntry[];
  }

  #resolve(name: string): HostedToolsPreparedTool | undefined {
    const state = this.#persistence.state();
    const connectGrantId = this.#activeConnectGrantId(state);
    const appToolCatalogDigest = this.#activeAppToolCatalogDigest(state);
    const definition = this.#definitions().find((candidate) => candidate.definition.name === name);
    if (!definition) return undefined;
    const binding: HostedToolsCatalogBinding = Object.freeze({
      hostId: state.host_id!,
      leaseId: state.lease_id!,
      generation: state.generation,
      entry: definition,
      ...(connectGrantId === undefined ? {} : { connectGrantId }),
      ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
    });
    return Object.freeze({
      ...(connectGrantId === undefined ? {} : { connectGrantId }),
      ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
      entry: definition,
      invoke: (request: HostedToolsInvokeRequest) => this.#invoke(binding, request),
    });
  }

  #invoke(
    binding: HostedToolsCatalogBinding,
    request: HostedToolsInvokeRequest,
  ): Promise<HostedToolsInvocationOutcome> {
    const retained = this.#persistence.callBySource(request.sessionId, request.callId);
    const leaseId = binding.leaseId;
    const now = this.#now();
    const deadlineAt = request.deadlineAt === undefined && retained
      ? retained.deadline_at
      : Math.min(
        request.deadlineAt ?? Number.MAX_SAFE_INTEGER,
        now + Math.min(binding.entry.timeout_ms, HOSTED_TOOL_CALL_TIMEOUT_MS),
      );
    const outputByteBudget = request.outputByteBudget ?? MAX_HOSTED_TOOL_OUTPUT_BYTES;
    const transportCallId = retained?.call_id ?? this.#randomUUID();
    const hostId = retained?.host_id ?? binding.hostId;
    const pinnedLeaseId = retained?.lease_id ?? binding.leaseId;
    const generation = retained?.generation ?? binding.generation;
    let call: Extract<HostedToolsManagedFrame, { type: "call" }>;
    try {
      call = parseHostedToolsManagedFrame(JSON.stringify({
        type: "call",
        session_id: request.sessionId,
        call_id: transportCallId,
        model: request.model,
        name: binding.entry.definition.name,
        input: request.input,
        output_token_budget: request.outputTokenBudget,
        output_byte_budget: outputByteBudget,
        deadline_at: deadlineAt,
      })) as Extract<HostedToolsManagedFrame, { type: "call" }>;
    } catch (error) {
      return Promise.resolve(hostedToolsUnavailable(`Hosted Tools call was invalid before dispatch: ${errorMessage(error)}`));
    }
    const inputJson = JSON.stringify(call.input);
    const proposed: HostedToolsCallRow = {
      call_id: call.call_id,
      session_id: call.session_id,
      source_call_id: request.callId,
      host_id: hostId,
      lease_id: pinnedLeaseId,
      generation,
      model: call.model,
      name: call.name,
      input_json: inputJson,
      output_token_budget: call.output_token_budget,
      output_byte_budget: call.output_byte_budget,
      deadline_at: call.deadline_at,
      cancel_requested: 0,
      state: "admitted",
      result_json: null,
      receipt_json: null,
    };
    if (retained) return this.#repeatedCall(retained, proposed);
    const existing = this.#persistence.call(call.call_id);
    if (existing) return this.#repeatedCall(existing, proposed);
    if (!this.#attachmentIsPresent(binding, now)) {
      return Promise.resolve(preAdmissionUnavailable(
        "Hosted Tools attachment was absent before durable admission",
      ));
    }
    if (this.#persistence.generationCallCount(leaseId, binding.generation)
      >= this.#maxCallsPerGeneration) {
      const state = this.#persistence.state();
      const socket = state.lease_id === leaseId && state.generation === binding.generation
        ? this.#socketForState(state)
        : undefined;
      if (socket) this.#fence(socket, "Hosted Tools generation exhausted its durable call ledger");
      else if (state.lease_id === leaseId && state.generation === binding.generation) {
        this.#retireState(state, "Hosted Tools generation exhausted its durable call ledger");
      }
      return Promise.resolve(hostedToolsUnavailable("Hosted Tools generation reached its durable call limit"));
    }
    try {
      this.#persistence.insertCall(proposed, now);
    } catch {
      const recovered = this.#persistence.callBySource(request.sessionId, request.callId)
        ?? this.#persistence.call(call.call_id);
      if (recovered) return this.#repeatedCall(recovered, proposed);
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools admission may have persisted; replay is unsafe"));
    }
    if (request.signal?.aborted) {
      return Promise.resolve(this.#finishBeforeDispatch(proposed, "cancelled", {
        status: "cancelled",
        message: "Hosted Tools call was cancelled before dispatch",
      }));
    }
    const current = this.#persistence.state();
    const dispatchNow = this.#now();
    const socket = this.#routingSocketForState(current);
    if (!socket
      || current.host_id !== binding.hostId
      || current.lease_id !== leaseId
      || current.generation !== binding.generation
      || current.lease_expires_at <= dispatchNow
      || deadlineAt <= dispatchNow) {
      return Promise.resolve(this.#finishBeforeDispatch(
        proposed,
        "unavailable",
        hostedToolsUnavailable("Hosted Tools binding became unavailable before dispatch"),
      ));
    }
    if (this.#persistence.activeCallCount(leaseId, binding.generation) > this.#maxInFlight) {
      return Promise.resolve(this.#finishBeforeDispatch(
        proposed,
        "unavailable",
        hostedToolsUnavailable("Hosted Tools host is at its bounded in-flight limit"),
      ));
    }
    const dispatched = this.#persistence.transitionCall(
      call.call_id,
      ["admitted"],
      "dispatched",
      "",
      dispatchNow,
    );
    if (!dispatched || dispatched.state !== "dispatched") {
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools call lost durable dispatch ownership"));
    }
    let resolve!: (outcome: HostedToolCallOutcome) => void;
    const promise = new Promise<HostedToolCallOutcome>((completed) => { resolve = completed; });
    const pending: PendingCall = {
      leaseId,
      generation: binding.generation,
      deadlineAt,
      promise,
      resolve,
    };
    this.#pending.set(call.call_id, pending);
    if (request.signal) {
      const cancel = () => { this.cancel(call.call_id); };
      request.signal.addEventListener("abort", cancel, { once: true });
      pending.removeAbort = () => request.signal?.removeEventListener("abort", cancel);
    }
    this.#armExpiry(call.call_id, pending, Math.min(current.lease_expires_at, deadlineAt));
    try {
      this.#send(socket, call);
    } catch {
      this.#retire(socket, "call delivery failed");
      closeSocket(socket, 1011, "Hosted Tools call delivery failed");
    }
    return promise;
  }

  #attachmentIsPresent(
    binding: HostedToolsCatalogBinding,
    now: number,
  ): boolean {
    const current = this.#persistence.state();
    return current.host_id === binding.hostId
      && current.lease_id === binding.leaseId
      && current.generation === binding.generation
      && current.lease_expires_at > now
      && this.#routingSocketForState(current) !== undefined;
  }

  #repeatedCall(existing: HostedToolsCallRow, proposed: HostedToolsCallRow): Promise<HostedToolsInvocationOutcome> {
    if (!sameImmutableCall(existing, proposed)) {
      const state = this.#persistence.state();
      const socket = this.#socketForState(state);
      if (socket) this.#fence(socket, "call ID was reused with different immutable fields");
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools call ID conflicts with retained durable state"));
    }
    if (existing.result_json) return Promise.resolve(JSON.parse(existing.result_json) as HostedToolCallOutcome);
    const pending = this.#pending.get(existing.call_id);
    if (existing.state === "dispatched" && pending) return pending.promise;
    return Promise.resolve(existing.state === "admitted"
      ? hostedToolsUnavailable("Hosted Tools call was admitted but never dispatched")
      : hostedToolsAmbiguous("Hosted Tools call has no retained terminal receipt"));
  }

  #finishBeforeDispatch(
    row: HostedToolsCallRow,
    state: "unavailable" | "cancelled",
    outcome: HostedToolCallOutcome,
  ): HostedToolCallOutcome {
    this.#persistence.transitionCall(row.call_id, ["admitted"], state, JSON.stringify(outcome), this.#now());
    this.#pruneReceipts();
    return outcome;
  }

  #armExpiry(callId: string, pending: PendingCall, at: number): void {
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    pending.timeout = setTimeout(() => {
      const current = this.#pending.get(callId);
      if (current !== pending) return;
      const state = this.#persistence.state();
      const now = this.#now();
      if (state.lease_id === pending.leaseId
        && state.generation === pending.generation
        && state.lease_expires_at > now
        && pending.deadlineAt > now) {
        this.#armExpiry(callId, pending, Math.min(state.lease_expires_at, pending.deadlineAt));
        return;
      }
      if (state.lease_id === pending.leaseId
        && state.generation === pending.generation
        && state.lease_expires_at <= now) {
        const socket = this.#socketForState(state);
        if (socket) this.#fence(socket, "Hosted Tools lease expired during a call");
        else this.#retireState(state, "Hosted Tools lease expired during a call");
        return;
      }
      const row = this.#persistence.call(callId);
      if (row) {
        this.cancel(callId);
        this.#finishAmbiguous(row, "Hosted Tools call deadline expired after dispatch");
      }
    }, Math.max(1, at - this.#now()));
  }

  #finishAmbiguous(row: HostedToolsCallRow, message: string): void {
    const outcome = hostedToolsAmbiguous(message);
    this.#persistence.transitionCall(
      row.call_id,
      ["dispatched"],
      "ambiguous",
      JSON.stringify(outcome),
      this.#now(),
    );
    this.#pruneReceipts();
    this.#takePending(row.call_id)?.resolve(outcome);
  }

  #retire(socket: HostedToolsSocket, reason: string): void {
    const attachment = this.#attachment(socket);
    if (!attachment?.leaseId || attachment.generation === undefined) return;
    this.#retireState(this.#persistence.state(), reason, attachment.leaseId, attachment.generation);
  }

  #retireState(
    state: HostedToolsStateRow,
    reason: string,
    leaseId = state.lease_id ?? undefined,
    generation = state.generation,
  ): void {
    if (!leaseId) return;
    const outcome = hostedToolsAmbiguous(`Hosted Tools outcome is ambiguous after transport loss: ${reason}`);
    this.#persistence.transaction(() => {
      this.#persistence.markGenerationAmbiguous(leaseId, generation, JSON.stringify(outcome), this.#now());
      this.#persistence.clearHost(leaseId, generation);
    });
    this.#pruneReceipts();
    this.#resolveGeneration(leaseId, generation, outcome);
    this.#notifyCatalogChanged();
  }

  #resolveGeneration(leaseId: string, generation: number, outcome: HostedToolCallOutcome): void {
    for (const [callId, pending] of this.#pending) {
      if (pending.leaseId !== leaseId || pending.generation !== generation) continue;
      this.#takePending(callId)?.resolve(outcome);
    }
  }

  #pruneReceipts(): void {
    const state = this.#persistence.state();
    this.#persistence.pruneReceipts(state.lease_id, state.generation, MAX_RETAINED_RECEIPTS);
  }

  #takePending(callId: string): PendingCall | undefined {
    const pending = this.#pending.get(callId);
    if (!pending) return undefined;
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    pending.removeAbort?.();
    this.#pending.delete(callId);
    return pending;
  }

  #fence(socket: HostedToolsSocket, reason: string, code = 1008): void {
    const attachment = this.#attachment(socket);
    if (attachment?.leaseId && attachment.generation !== undefined) {
      this.#retire(socket, reason);
    }
    closeSocket(socket, code, boundedReason(reason));
  }

  #socketForState(state: HostedToolsStateRow): HostedToolsSocket | undefined {
    if (!state.host_id || !state.lease_id) return undefined;
    return this.context.sockets().find((socket) => {
      const attachment = this.#attachment(socket);
      return socket.readyState === OPEN
        && attachment?.leaseId === state.lease_id
        && attachment.generation === state.generation
        && attachment.active === true;
    });
  }

  #routingSocketForState(state: HostedToolsStateRow): HostedToolsSocket | undefined {
    if (!state.catalog_json) return undefined;
    const socket = this.#socketForState(state);
    return socket && this.#attachment(socket)?.draining !== true ? socket : undefined;
  }

  #liveRoutingSocketForState(state: HostedToolsStateRow): HostedToolsSocket | undefined {
    if (state.lease_id && state.lease_expires_at <= this.#now()) {
      const socket = this.#socketForState(state);
      if (socket) this.#fence(socket, "Hosted Tools lease expired");
      else this.#retireState(state, "Hosted Tools lease expired");
      return undefined;
    }
    return this.#routingSocketForState(state);
  }

  #activeConnectGrantId(state = this.#persistence.state()): string | undefined {
    const socket = this.#socketForState(state);
    if (socket === undefined) return undefined;
    const attachment = this.#attachment(socket);
    if (attachment?.connectGrantId === undefined) return undefined;
    return isConnectGrantId(attachment.connectGrantId)
      ? attachment.connectGrantId
      : INVALID_CONNECT_GRANT_ID;
  }

  #activeAppToolCatalogDigest(state = this.#persistence.state()): string | undefined {
    const socket = this.#socketForState(state);
    return socket === undefined ? undefined : this.#attachment(socket)?.appToolCatalogDigest;
  }

  #attachment(socket: HostedToolsSocket): HostedToolsSocketAttachment | undefined {
    const value = this.context.readAttachment(socket) as HostedToolsSocketAttachment | null;
    return value?.kind === SOCKET_TAG ? value : undefined;
  }

  #send(socket: HostedToolsSocket, frame: HostedToolsManagedFrame): void {
    socket.send(JSON.stringify(frame));
  }

  #notifyCatalogChanged(): void {
    this.#onCatalogChanged?.(this.#definitions());
  }
}

function isConnectGrantId(value: unknown): value is string {
  return typeof value === "string" && /^0x[0-9a-f]{64}$/.test(value);
}

function wireToolResult(output: Extract<HostedToolCallOutcome, { status: "completed" }>["output"]): unknown {
  return Object.freeze({
    [TOOL_RESULT]: true,
    metadata: output.metadata,
    output: output.output,
    structuredResult: output.structured_result,
    success: output.success,
    value: output.structured_result ?? output.output,
  });
}

function toolResult(
  output: unknown,
  structuredResult: unknown,
  success: boolean,
  metadata: unknown,
): unknown {
  return Object.freeze({
    [TOOL_RESULT]: true,
    metadata,
    output,
    structuredResult,
    success,
    value: structuredResult ?? output,
  });
}

function sameImmutableCall(left: HostedToolsCallRow, right: HostedToolsCallRow): boolean {
  return left.call_id === right.call_id
    && left.session_id === right.session_id
    && left.source_call_id === right.source_call_id
    && left.host_id === right.host_id
    && left.lease_id === right.lease_id
    && left.generation === right.generation
    && left.model === right.model
    && left.name === right.name
    && left.input_json === right.input_json
    && left.output_token_budget === right.output_token_budget
    && left.output_byte_budget === right.output_byte_budget
    && left.deadline_at === right.deadline_at;
}

function outcomeState(outcome: HostedToolCallOutcome): Exclude<HostedToolsCallState, "admitted" | "dispatched"> {
  return outcome.status;
}

export function hostedToolsUnavailable(message: string): HostedToolCallOutcome {
  return { status: "unavailable", message: boundedReason(message) };
}

type HostedToolsInvocationOutcome = HostedToolCallOutcome & {
  [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]?: true;
};

function preAdmissionUnavailable(message: string): HostedToolsInvocationOutcome {
  return Object.freeze({
    ...hostedToolsUnavailable(message),
    [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true as const,
  });
}

export function hostedToolsAmbiguous(message: string): HostedToolCallOutcome {
  return { status: "ambiguous", message: boundedReason(message) };
}

function boundedReason(message: string): string {
  if (encoder.encode(message).byteLength <= 2 * 1024) return message;
  return "Hosted Tools protocol failure exceeded the bounded reason limit";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function closeSocket(socket: HostedToolsSocket, code: number, reason: string): void {
  try { socket.close(code, websocketCloseReason(reason)); }
  catch { /* The socket is already closed or never reached an open state. */ }
}

function websocketCloseReason(reason: string): string {
  if (encoder.encode(reason).byteLength <= 123) return reason;
  let bounded = "";
  for (const scalar of reason) {
    if (encoder.encode(bounded + scalar).byteLength > 123) break;
    bounded += scalar;
  }
  return bounded;
}
