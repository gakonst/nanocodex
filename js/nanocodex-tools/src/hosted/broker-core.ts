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
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  MACHINE_PREVIEW_PARAMETERS,
  PREVIEW_OUTPUT_SCHEMA,
  WRITE_STDIN_PARAMETERS,
} from "../../tools/execution-contract.mjs";

const SOCKET_TAG = "hosted-tools";
const INVALID_CONNECT_GRANT_ID = "invalid-connect-grant";
const DEFAULT_MAX_IN_FLIGHT = 64;
const OPEN = 1;
const MAX_RETAINED_RECEIPTS = 512;
const MAX_CALLS_PER_GENERATION = 512;
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const LEGACY_ROUTE_ID = "$legacy";
export const HOSTED_MACHINE_TOOL_NAMES = Object.freeze([
  "exec_command",
  "write_stdin",
  "preview",
] as const);
const MACHINE_TOOL_NAMES: ReadonlySet<string> = new Set(HOSTED_MACHINE_TOOL_NAMES);
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
  route_id: string;
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
  routeId?: string;
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
  routeToken: string;
  connectGrantId?: string;
  appToolCatalogDigest?: string;
  canonicalName: string;
  machine?: HostedMachine;
  entry: HostedToolCatalogEntry;
  invoke(request: HostedToolsInvokeRequest): Promise<HostedToolsInvocationOutcome>;
}>;

type HostedToolsCatalogBinding = Readonly<{
  routeId: string;
  connectGrantId?: string;
  appToolCatalogDigest?: string;
  hostId: string;
  leaseId: string;
  generation: number;
  wireName: string;
  machine?: HostedMachine;
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
  /** Opaque immutable route identity for trusted broker-to-broker relays. */
  routeToken?: string;
  handler(
    input: unknown,
    context: { sessionId: string; callId: string; model?: string; signal?: AbortSignal },
  ): Promise<unknown>;
}>;

export type HostedMachineToolName = (typeof HOSTED_MACHINE_TOOL_NAMES)[number];

export interface HostedToolsDynamicProvider {
  definitions(): readonly HostedToolsCodeDefinition[];
  resolve(name: string): HostedToolsCodeTool | undefined;
  /** Installed by the owning ToolRouter to reject non-parity catalogs before ACK. */
  setCatalogValidator(validator: HostedToolsCatalogValidator | undefined): void;
}

/** Injectable durable call ledger boundary; the production default is Durable Object SQLite. */
export interface HostedToolsBrokerPersistence {
  initialize(now: number): readonly HostedToolsStateRow[];
  transaction<T>(callback: () => T): T;
  states(): readonly HostedToolsStateRow[];
  state(routeId: string): HostedToolsStateRow | undefined;
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
  pruneReceipts(limit: number): void;
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
    this.#nextCandidateGeneration = Math.max(0, ...this.#persistence.states().map((state) => state.generation));
    for (const socket of this.context.sockets()) {
      const generation = this.#attachment(socket)?.generation;
      if (generation !== undefined) this.#nextCandidateGeneration = Math.max(this.#nextCandidateGeneration, generation);
    }
    for (const state of retired) {
      if (!state.lease_id) continue;
      for (const socket of this.context.sockets()) {
        const attachment = this.#attachment(socket);
        if (attachment?.leaseId !== state.lease_id
          || attachment.generation !== state.generation) continue;
        closeSocket(socket, 1012, "Hosted Tools owner restarted");
      }
    }
    this.#provider = Object.freeze({
      // ToolRouter owns the one aggregate tool_search. This provider exposes
      // only the current attached definitions, which stay deferred and can be
      // overlaid onto exact cloud contracts by that router.
      definitions: () => {
        return this.#publicCatalogBindings()
          .filter((binding) => this.#entryAllowed(
            binding.entry,
            binding.connectGrantId,
            binding.appToolCatalogDigest,
          ))
          .map((binding) => Object.freeze({
            ...binding.entry.definition,
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
        return this.#codeTool(name, prepared);
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
    for (const state of this.#persistence.states()) this.#retireState(state, reason);
  }

  isReady(): boolean { return this.#definitions().length > 0; }

  hasPendingCalls(): boolean { return this.#pending.size > 0; }

  provider(): HostedToolsDynamicProvider { return this.#provider; }

  /** Resolves one canonical machine primitive against its exact admitted attachment generation. */
  machineTool(machineId: string, name: HostedMachineToolName): HostedToolsCodeTool | undefined {
    if (!MACHINE_TOOL_NAMES.has(name)) return undefined;
    const binding = this.#catalogBindings().find((candidate) => (
      candidate.machine?.id === machineId && candidate.wireName === name
    ));
    if (!binding) return undefined;
    const prepared = this.#preparedTool(binding);
    if (!this.#entryAllowed(
      prepared.entry,
      prepared.connectGrantId,
      prepared.appToolCatalogDigest,
    )) return undefined;
    return this.#codeTool(name, prepared);
  }

  /** Returns the live, non-secret user-machine snapshot for the account-owned host. */
  machines(): readonly HostedMachine[] {
    const machines: Array<{ routeId: string; machine: HostedMachine }> = [];
    const ids = new Set<string>();
    for (const state of this.#sortedStates()) {
      const socket = this.#liveRoutingSocketForState(state);
      if (socket === undefined) continue;
      const attachment = this.#attachment(socket);
      if (attachment?.connectGrantId !== undefined) continue;
      for (const machine of attachment?.machines ?? []) {
        if (ids.has(machine.id)) return [];
        ids.add(machine.id);
        machines.push({ routeId: state.route_id, machine });
      }
    }
    return machines
      .sort((left, right) => left.machine.id.localeCompare(right.machine.id)
        || left.routeId.localeCompare(right.routeId))
      .map(({ machine }) => machine);
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
    for (const state of this.#persistence.states()) {
      if (!state.lease_id || state.lease_expires_at > this.#now()) continue;
      const socket = this.#socketForState(state);
      if (socket) this.#fence(socket, "Hosted Tools lease expired");
      else this.#retireState(state, "Hosted Tools lease expired");
    }
  }

  cancel(callId: string): boolean {
    const pending = this.#pending.get(callId);
    if (!pending) return false;
    const row = this.#persistence.call(callId);
    if (!row || row.state !== "dispatched") return false;
    const state = this.#stateForLease(row.lease_id, row.generation);
    const socket = this.#socketForState(state);
    if (!socket
      || !state
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
    const state = attachment?.routeId === undefined
      ? undefined
      : this.#persistence.state(attachment.routeId);
    if (!attachment?.active || !attachment.leaseId || attachment.generation === undefined || !state
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
    const state = this.#persistence.state(attachment.routeId!)!;
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
    const routeId = scopedRouteId(initial.connectGrantId, frame.attachment_id);
    let state = this.#persistence.state(routeId) ?? emptyState(routeId);
    if (state.lease_id && state.lease_expires_at <= this.#now()) {
      const expiredSocket = this.#socketForState(state);
      if (expiredSocket) this.#fence(expiredSocket, "Hosted Tools lease expired");
      else this.#retireState(state, "Hosted Tools lease expired");
      state = this.#persistence.state(routeId) ?? emptyState(routeId);
    }
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
      routeId,
      leaseId,
      generation,
    } satisfies HostedToolsSocketAttachment;
    this.context.writeAttachment(socket, candidate);
    const catalogJson = JSON.stringify(frame.tools);
    const machine = frame.machines?.[0];
    const candidateEntries = frame.tools
      .filter((entry) => !reservedMachineEntry(entry, machine))
      .map((entry) => exposedEntry(entry, machine));
    const candidateDefinitions = candidateEntries.map((entry) => Object.freeze({
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
      if ((frame.machines?.length ?? 0) > 0
        && (frame.machines?.length !== 1 || frame.attachment_id !== frame.machines[0]?.id)) {
        throw new Error("an account machine route requires one machine whose id equals attachment_id");
      }
      if (machine !== undefined) validateMachineToolContracts(frame.tools);
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
      const otherBindings = this.#publicCatalogBindings(routeId, true);
      const exposedNames = new Set(otherBindings.map((binding) => binding.entry.definition.name));
      const duplicateTool = candidateEntries.find((entry) => exposedNames.has(entry.definition.name));
      if (duplicateTool) {
        throw new Error(`tool name ${duplicateTool.definition.name} is already exposed by another attachment`);
      }
      if (initial.connectGrantId === undefined) {
        const machineIds = new Set(this.#machineIds(routeId));
        const duplicateMachine = frame.machines?.find((machine) => machineIds.has(machine.id));
        if (duplicateMachine) {
          throw new Error(`machine ID ${duplicateMachine.id} is already published by another attachment`);
        }
      }
      const validator = this.#catalogValidator;
      const aggregateDefinitions = [
        ...otherBindings.map((binding) => Object.freeze({
          ...binding.entry,
          definition: Object.freeze({
            ...binding.entry.definition,
            defer_loading: true as const,
          }),
        })),
        ...candidateDefinitions,
      ].sort((left, right) => left.definition.name.localeCompare(right.definition.name));
      if (validator !== undefined && validator(aggregateDefinitions) !== true) {
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
        route_id: routeId,
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
    const state = this.#persistence.state(attachment.routeId!)!;
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
    return this.#publicCatalogBindings().map((binding) => binding.entry);
  }

  #resolve(name: string): HostedToolsPreparedTool | undefined {
    const binding = this.#publicCatalogBindings()
      .find((candidate) => candidate.entry.definition.name === name);
    if (!binding) return undefined;
    return this.#preparedTool(binding);
  }

  #preparedTool(binding: HostedToolsCatalogBinding): HostedToolsPreparedTool {
    return Object.freeze({
      routeToken: JSON.stringify([
        binding.routeId,
        binding.generation,
        binding.leaseId,
        binding.wireName,
      ]),
      ...(binding.connectGrantId === undefined ? {} : { connectGrantId: binding.connectGrantId }),
      ...(binding.appToolCatalogDigest === undefined
        ? {}
        : { appToolCatalogDigest: binding.appToolCatalogDigest }),
      canonicalName: binding.wireName,
      ...(binding.machine === undefined ? {} : { machine: binding.machine }),
      entry: binding.entry,
      invoke: (request: HostedToolsInvokeRequest) => this.#invoke(binding, request),
    });
  }

  #codeTool(name: string, prepared: HostedToolsPreparedTool): HostedToolsCodeTool {
    return Object.freeze({
      name,
      parallelSafe: prepared.entry.parallel_safe,
      routeToken: prepared.routeToken,
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
        if (outcome.status === "completed") {
          return wireToolResult(
            outcome.output,
            prepared.canonicalName,
            prepared.machine,
          );
        }
        const result = toolResult(outcome.message, outcome, false, null);
        return outcome[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE] === true
          ? Object.freeze({
              ...(result as Record<PropertyKey, unknown>),
              [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true,
            })
          : result;
      },
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
        name: binding.wireName,
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
      const state = this.#persistence.state(binding.routeId);
      const socket = state?.lease_id === leaseId && state.generation === binding.generation
        ? this.#socketForState(state)
        : undefined;
      if (socket) this.#fence(socket, "Hosted Tools generation exhausted its durable call ledger");
      else if (state?.lease_id === leaseId && state.generation === binding.generation) {
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
    const current = this.#persistence.state(binding.routeId);
    const dispatchNow = this.#now();
    const socket = this.#routingSocketForState(current);
    if (!socket
      || !current
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
    const current = this.#persistence.state(binding.routeId);
    return current !== undefined
      && current.host_id === binding.hostId
      && current.lease_id === binding.leaseId
      && current.generation === binding.generation
      && current.lease_expires_at > now
      && this.#routingSocketForState(current) !== undefined;
  }

  #repeatedCall(existing: HostedToolsCallRow, proposed: HostedToolsCallRow): Promise<HostedToolsInvocationOutcome> {
    if (!sameImmutableCall(existing, proposed)) {
      const state = this.#stateForLease(existing.lease_id, existing.generation);
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
      const state = this.#stateForLease(pending.leaseId, pending.generation);
      const now = this.#now();
      if (state?.lease_id === pending.leaseId
        && state.generation === pending.generation
        && state.lease_expires_at > now
        && pending.deadlineAt > now) {
        this.#armExpiry(callId, pending, Math.min(state.lease_expires_at, pending.deadlineAt));
        return;
      }
      if (state?.lease_id === pending.leaseId
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
    const state = attachment.routeId === undefined
      ? undefined
      : this.#persistence.state(attachment.routeId);
    if (state) this.#retireState(state, reason, attachment.leaseId, attachment.generation);
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
    this.#persistence.pruneReceipts(MAX_RETAINED_RECEIPTS);
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

  #socketForState(state: HostedToolsStateRow | undefined): HostedToolsSocket | undefined {
    if (!state) return undefined;
    if (!state.host_id || !state.lease_id) return undefined;
    return this.context.sockets().find((socket) => {
      const attachment = this.#attachment(socket);
      return socket.readyState === OPEN
        && attachment?.leaseId === state.lease_id
        && attachment.generation === state.generation
        && attachment.routeId === state.route_id
        && attachment.active === true;
    });
  }

  #routingSocketForState(state: HostedToolsStateRow | undefined): HostedToolsSocket | undefined {
    if (!state) return undefined;
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

  #activeConnectGrantId(state: HostedToolsStateRow): string | undefined {
    const socket = this.#socketForState(state);
    if (socket === undefined) return undefined;
    const attachment = this.#attachment(socket);
    if (attachment?.connectGrantId === undefined) return undefined;
    return isConnectGrantId(attachment.connectGrantId)
      ? attachment.connectGrantId
      : INVALID_CONNECT_GRANT_ID;
  }

  #activeAppToolCatalogDigest(state: HostedToolsStateRow): string | undefined {
    const socket = this.#socketForState(state);
    return socket === undefined ? undefined : this.#attachment(socket)?.appToolCatalogDigest;
  }

  #sortedStates(): HostedToolsStateRow[] {
    return [...this.#persistence.states()].sort((left, right) => left.route_id.localeCompare(right.route_id));
  }

  #stateForLease(leaseId: string, generation: number): HostedToolsStateRow | undefined {
    return this.#persistence.states().find((state) => state.lease_id === leaseId
      && state.generation === generation);
  }

  #catalogBindings(
    excludeRouteId?: string,
    includeAmbiguous = false,
  ): HostedToolsCatalogBinding[] {
    const bindings: HostedToolsCatalogBinding[] = [];
    for (const state of this.#sortedStates()) {
      if (state.route_id === excludeRouteId || !state.host_id || !state.lease_id || !state.catalog_json) continue;
      const socket = this.#liveRoutingSocketForState(state);
      if (!socket) continue;
      const attachment = this.#attachment(socket);
      const connectGrantId = this.#activeConnectGrantId(state);
      const appToolCatalogDigest = this.#activeAppToolCatalogDigest(state);
      let entries: HostedToolCatalogEntry[];
      try {
        entries = JSON.parse(state.catalog_json) as HostedToolCatalogEntry[];
      } catch {
        continue;
      }
      for (const entry of entries) {
        const machine = attachment?.machines?.[0];
        bindings.push(Object.freeze({
          routeId: state.route_id,
          hostId: state.host_id,
          leaseId: state.lease_id,
          generation: state.generation,
          wireName: entry.definition.name,
          ...(machine === undefined ? {} : { machine }),
          entry: exposedEntry(entry, machine),
          ...(connectGrantId === undefined ? {} : { connectGrantId }),
          ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
        }));
      }
    }
    bindings.sort((left, right) => left.routeId.localeCompare(right.routeId)
      || left.entry.definition.name.localeCompare(right.entry.definition.name));
    if (includeAmbiguous) return bindings;
    const counts = new Map<string, number>();
    for (const binding of bindings) {
      const name = binding.entry.definition.name;
      counts.set(name, (counts.get(name) ?? 0) + 1);
    }
    return bindings.filter((binding) => counts.get(binding.entry.definition.name) === 1);
  }

  #publicCatalogBindings(
    excludeRouteId?: string,
    includeAmbiguous = false,
  ): HostedToolsCatalogBinding[] {
    return this.#catalogBindings(excludeRouteId, includeAmbiguous)
      .filter((binding) => !reservedMachineBinding(binding));
  }

  #machineIds(excludeRouteId?: string): string[] {
    const ids: string[] = [];
    for (const state of this.#sortedStates()) {
      if (state.route_id === excludeRouteId) continue;
      const socket = this.#liveRoutingSocketForState(state);
      if (!socket) continue;
      const attachment = this.#attachment(socket);
      if (attachment?.connectGrantId !== undefined) continue;
      for (const machine of attachment?.machines ?? []) ids.push(machine.id);
    }
    return ids;
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

function emptyState(routeId: string): HostedToolsStateRow {
  return {
    route_id: routeId,
    generation: 0,
    host_id: null,
    lease_id: null,
    lease_expires_at: 0,
    catalog_json: null,
  };
}

function scopedRouteId(connectGrantId: string | undefined, attachmentId: string | undefined): string {
  return `${connectGrantId === undefined ? "user" : "connect"}:${attachmentId ?? LEGACY_ROUTE_ID}`;
}

function reservedMachineEntry(
  entry: HostedToolCatalogEntry,
  machine: HostedMachine | undefined,
): boolean {
  return machine !== undefined && MACHINE_TOOL_NAMES.has(entry.definition.name);
}

function reservedMachineBinding(binding: HostedToolsCatalogBinding): boolean {
  return binding.machine !== undefined && MACHINE_TOOL_NAMES.has(binding.wireName);
}

function validateMachineToolContracts(entries: readonly HostedToolCatalogEntry[]): void {
  for (const entry of entries) {
    const name = entry.definition.name;
    if (!MACHINE_TOOL_NAMES.has(name)) continue;
    if (entry.definition.type !== "function") {
      throw new Error(`machine tool ${name} must use its canonical function schema`);
    }
    switch (name) {
      case "exec_command":
        validateCanonicalObjectSchema(name, entry.definition.parameters, EXEC_COMMAND_PARAMETERS);
        validateCanonicalObjectSchema(
          `${name} output`,
          entry.definition.output_schema,
          EXECUTION_OUTPUT_SCHEMA,
        );
        break;
      case "write_stdin":
        validateCanonicalObjectSchema(name, entry.definition.parameters, WRITE_STDIN_PARAMETERS);
        validateCanonicalObjectSchema(
          `${name} output`,
          entry.definition.output_schema,
          EXECUTION_OUTPUT_SCHEMA,
        );
        break;
      case "preview":
        validateCanonicalObjectSchema(name, entry.definition.parameters, MACHINE_PREVIEW_PARAMETERS);
        validateCanonicalObjectSchema(
          `${name} output`,
          entry.definition.output_schema,
          PREVIEW_OUTPUT_SCHEMA,
        );
        break;
    }
  }
}

function validateCanonicalObjectSchema(
  label: string,
  schema: Record<string, unknown> | undefined,
  canonical: Readonly<Record<string, unknown>>,
): void {
  const properties = objectRecord(canonical.properties);
  const required = canonical.required;
  if (properties === undefined || !Array.isArray(required)) {
    throw new Error(`invalid canonical machine tool schema for ${label}`);
  }
  const propertyTypes = Object.fromEntries(Object.entries(properties).map(([name, property]) => {
    const type = objectRecord(property)?.type;
    if (typeof type !== "string") throw new Error(`invalid canonical type for ${label}.${name}`);
    return [name, type];
  }));
  validateObjectSchema(label, schema, required as string[], propertyTypes);
}

function validateObjectSchema(
  label: string,
  schema: Record<string, unknown> | undefined,
  required: readonly string[],
  propertyTypes: Readonly<Record<string, string>>,
): void {
  const properties = objectRecord(schema?.properties);
  const actualRequired = schema?.required;
  if (schema?.type !== "object"
    || schema?.additionalProperties !== false
    || properties === undefined
    || !Array.isArray(actualRequired)
    || actualRequired.some((value) => typeof value !== "string")
    || !sameStrings(actualRequired as string[], required)
    || !sameStrings(Object.keys(properties), Object.keys(propertyTypes))) {
    throw new Error(`machine tool ${label} must use its canonical object schema`);
  }
  for (const [property, type] of Object.entries(propertyTypes)) {
    if (objectRecord(properties[property])?.type !== type) {
      throw new Error(`machine tool ${label}.${property} must use canonical type ${type}`);
    }
  }
}

function objectRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value) => right.includes(value));
}

function exposedEntry(entry: HostedToolCatalogEntry, machine: HostedMachine | undefined): HostedToolCatalogEntry {
  if (machine === undefined) return entry;
  const routeName = `user:${machine.id}:${entry.definition.name}`;
  const candidate = `user_${machine.id}_${entry.definition.name}`;
  const safeCandidate = candidate.replace(/[^A-Za-z0-9_-]/g, "_");
  const exposedName = candidate === safeCandidate && candidate.length <= 128
    ? candidate
    : `${safeCandidate.slice(0, 111)}_${stableHash(routeName)}`;
  const definition = {
    ...entry.definition,
    name: exposedName,
    description: `Routes to ${routeName}. ${entry.definition.description}`,
  };
  return Object.freeze({
    ...entry,
    definition: Object.freeze(definition),
  });
}

function stableHash(value: string): string {
  let hash = 0xcbf29ce484222325n;
  for (const byte of encoder.encode(value)) {
    hash ^= BigInt(byte);
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return hash.toString(16).padStart(16, "0");
}

function isConnectGrantId(value: unknown): value is string {
  return typeof value === "string" && /^0x[0-9a-f]{64}$/.test(value);
}

function wireToolResult(
  output: Extract<HostedToolCallOutcome, { status: "completed" }>["output"],
  canonicalName: string,
  machine: HostedMachine | undefined,
): unknown {
  return toolResult(
    output.output,
    output.structured_result,
    output.success,
    toolExecutionMetadata(output.metadata, canonicalName, machine),
  );
}

function toolExecutionMetadata(
  metadata: unknown,
  canonicalName: string,
  machine: HostedMachine | undefined,
): unknown {
  if (machine === undefined) return metadata;
  const execution = {
    machine_id: machine.id,
    machine_name: machine.name,
    tool_name: canonicalName,
  };
  if (metadata === null || metadata === undefined) return Object.freeze(execution);
  if (typeof metadata === "object" && !Array.isArray(metadata)) {
    return Object.freeze({ ...(metadata as Record<string, unknown>), ...execution });
  }
  return Object.freeze({ ...execution, provider_metadata: metadata });
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
