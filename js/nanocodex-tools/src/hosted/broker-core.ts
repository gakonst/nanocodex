import {
  MAX_HOSTED_TOOLS_RECOVER_CALL_IDS,
  HostedToolsProtocolError,
  parseHostedToolsHostFrame,
  parseHostedToolsManagedFrame,
  type HostedToolCallOutcome,
  type HostedToolReceiptTiming,
  type HostedToolDiagnosticStage,
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
import type { ToolContext } from "../../tools/types.mjs";

const SOCKET_TAG = "hosted-tools";
const INVALID_CONNECT_GRANT_ID = "invalid-connect-grant";
const OPEN = 1;
const REVOKED_ROUTE_LEASE_EXPIRES_AT = -1;
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const LEGACY_ROUTE_ID = "$legacy";
export const HOSTED_MACHINE_TOOL_NAMES = Object.freeze([
  "exec_command",
  "write_stdin",
  "preview",
  "native_secure_input",
  "validate_app",
  "mcp__cua_repl__js",
  "mcp__cua_repl__js_reset",
] as const);
const MACHINE_TOOL_NAMES: ReadonlySet<string> = new Set(HOSTED_MACHINE_TOOL_NAMES);
// Older attachments and persisted catalogs must not restore hosted browser tools.
function disabledBrowserTool(name: string): boolean {
  return name === "browser_execute" || name.startsWith("browser_vault_");
}
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
  machines_json: string | null;
  runtime_id?: string | null;
  command_recovery?: number;
  connect_grant_id?: string | null;
};

export type HostedToolsCallRow = {
  call_id: string;
  session_id: string;
  source_call_id: string;
  /** Private managed thread correlation; never included in a Hand request. */
  thread_id?: string | null;
  turn_id?: string | null;
  host_id: string;
  lease_id: string;
  generation: number;
  model: string;
  connection_id?: string | null;
  host_connection_id?: string | null;
  host_runtime_id?: string | null;
  hand_id?: string | null;
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
  connectionId?: string;
  sessionId: string;
  allowedMcpIds?: readonly string[];
  appToolCatalogDigest?: `0x${string}`;
  connectGrantId?: string;
  expectedAttachmentId?: string;
  publisherIdentity?: Readonly<{ machineId: string; runtimeId: string }>;
  maximumLeaseExpiresAt?: number;
  fixedRouteId?: string;
  renewalToken?: string;
  routeId?: string;
  leaseId?: string;
  generation?: number;
  active?: boolean;
  draining?: true;
  machines?: readonly HostedMachine[];
  runtimeId?: string;
  commandRecovery?: true;
  turnLifecycle?: true;
  hostConnectionId?: string;
  diagnostics?: true;
  last_heartbeat_at?: number;
  heartbeat_count?: number;
  last_heartbeat_observation_at?: number;
  lease_expires_at?: number;
};

type HostFrameTiming = Readonly<{
  receivedAt: number;
  frame_decode_ms: number;
  lease_validation_ms: number;
  message_to_handler_ms: number;
}>;

type PendingCall = {
  threadId?: string;
  receivedAt: number;
  dispatchedAt: number;
  leaseId: string;
  generation: number;
  deadlineAt: number;
  promise: Promise<HostedToolCallOutcome>;
  resolve(outcome: HostedToolCallOutcome): void;
  timeout?: ReturnType<typeof setTimeout>;
  removeAbort?: () => void;
  restored?: true;
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
  threadId?: string;
  sessionId: string;
  callId: string;
  turnId?: string;
  model: string;
  input: Record<string, unknown> | string;
  outputTokenBudget: number;
  outputByteBudget?: number;
  deadlineAt?: number;
  signal?: AbortSignal;
}>;

export type HostedToolsReceiptRequest = Readonly<{
  sessionId: string;
  callId: string;
  /** Exposed definition name, or the canonical machine primitive when machineId is set. */
  name: string;
  machineId?: string;
  waitMs: number;
  /** The caller's pinned route token; must equal the route the row was admitted under. */
  routeToken: string;
  /** The caller's authorization context; receipts are never broader than invocation. */
  context?: HostedToolsAuthorizationContext;
}>;

export type HostedToolsReceipt =
  | Readonly<{ state: "missing" }>
  | Readonly<{ state: "unresolved" }>
  | Readonly<{ state: "running"; deadlineAt: number }>
  | Readonly<{ state: "settled"; result: unknown; processRouteToken?: string }>;

export type HostedToolsPreparedTool = Readonly<{
  routeToken: string;
  connectGrantId?: string;
  appToolCatalogDigest?: string;
  canonicalName: string;
  providerDefinition: HostedToolCatalogEntry["definition"];
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
  connectionId?: string;
  runtimeId?: string;
  hostConnectionId?: string;
  providerDefinition: HostedToolCatalogEntry["definition"];
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
  /** The admitted provider declaration, before namespace routing. */
  definition?: HostedToolsCodeDefinition;
  /** Opaque immutable route identity for trusted broker-to-broker relays. */
  routeToken?: string;
  processSessionKey?: string;
  endTurn?(sessionId: string, turnId: string, event: "Stop" | "Interrupt" | "SubagentStop"): Promise<void>;
  handler(
    input: unknown,
    context: HostedToolsInvocationContext,
  ): Promise<unknown>;
}>;

export type HostedToolsInvocationContext = Readonly<
  Pick<ToolContext, "sessionId" | "callId">
  & Partial<Pick<ToolContext, "parentCallId" | "turnId" | "model" | "signal" | "subagent">>
  & { threadId?: string }
>;

export type HostedToolsAuthorizationContext = Pick<ToolContext, "sessionId" | "subagent">;

export type HostedMachineToolName = (typeof HOSTED_MACHINE_TOOL_NAMES)[number] | `mcp__cua_repl__${string}`;

export interface HostedToolsDynamicProvider {
  endTurn?(sessionId: string, turnId: string, event: "Stop" | "Interrupt" | "SubagentStop"): Promise<void>;
  definitions(): readonly HostedToolsCodeDefinition[];
  resolve(name: string): HostedToolsCodeTool | undefined;
  /** Installed by the owning ToolRouter to reject non-parity catalogs before ACK. */
  setCatalogValidator(validator: HostedToolsCatalogValidator | undefined): void;
}

/** One synchronous discovery view; never retain it as an authority cache. */
export type HostedToolsCatalogSnapshot = Readonly<{
  machines(): readonly Readonly<{ machine: HostedMachine; online: boolean }>[];
  definitions(): readonly HostedToolsCodeDefinition[];
  resolve(name: string): HostedToolsCodeTool | undefined;
  machineTool(
    machineId: string,
    name: HostedMachineToolName,
    context?: HostedToolsAuthorizationContext,
  ): HostedToolsCodeTool | undefined;
}>;

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
  /**
   * Required for command recovery; enumerates this generation's unsettled
   * `dispatched` calls without resending commands. Settled history must stay
   * in storage: a resumed generation accumulates it for the Hand's lifetime.
   */
  generationCalls?(leaseId: string, generation: number): readonly HostedToolsCallRow[];
}

export type HostedToolsDiagnosticReason = "transport_closed" | "transport_error" | "protocol_error"
  | "owner_restarted" | "owner_shutdown" | "route_revoked" | "host_replaced" | "host_draining"
  | "lease_expired" | "lease_validation_failed" | "lease_validation_unavailable"
  | "generation_limit" | "call_conflict" | "call_deadline" | "attachment_unavailable"
  | "call_send_failed" | "cancel_send_failed" | "ack_send_failed"
  | "ready_send_failed" | "drain_send_failed"
  | "cancelled_before_dispatch" | "in_flight_limit" | "invalid_call" | "admission_uncertain"
  | "dispatch_ownership_lost";

export type HostedToolsCallObservation = Readonly<{
  stage: "received" | "admitted" | "dispatched" | "terminal" | "replay" | "receipt" | "late_receipt" | "receipt_replay" | "receipt_lookup" | "cancel_requested"
    | "host_progress" | "send_started" | "sent" | "send_failed" | "ack_attempt" | "ack_sent" | "ack_failed" | "transport_lost" | "admission_failed" | "connection_draining";
  tool: string;
  session_id?: string;
  thread_id?: string;
  source_call_id?: string;
  transport_call_id?: string;
  connection_id?: string;
  lease_id?: string;
  /** Compatibility field: retained runtime ownership epoch, not a socket counter. */
  connection_generation?: number;
  runtime_generation?: number;
  host_connection_id?: string;
  host_runtime_id?: string;
  hand_id?: string;
  host_stage?: HostedToolDiagnosticStage;
  host_elapsed_ms?: number;
  reason_code?: HostedToolsDiagnosticReason;
  /** Fixed schema field names only; never retained/proposed values. */
  conflict_fields?: readonly ImmutableCallField[];
  outcome?: HostedToolCallOutcome["status"] | "failed";
  success?: boolean;
  duration_ms?: number;
  admission_ms?: number;
  roundtrip_ms?: number;
  settlement_ms?: number;
  /** Broker handler entry is not a transport-level arrival timestamp. */
  dispatch_to_message_ms?: number;
  frame_decode_ms?: number;
  lease_validation_ms?: number;
  message_to_handler_ms?: number;
  host_timing?: HostedToolReceiptTiming;
  /** Combined transit, return, and unmeasured socket/serialization overhead. */
  transit_return_overhead_ms?: number;
}>;

export type HostedToolsConnectionObservation = Readonly<{
  stage: "accepted" | "ready" | "resumed" | "closed" | "error" | "replaced" | "draining" | "lease_expired" | "fenced" | "heartbeat" | "snapshot";
  reason_code?: HostedToolsDiagnosticReason;
  /** Broker-generated socket identity; host_connection_id identifies the client attempt. */
  connection_id?: string;
  lease_id?: string;
  /** Compatibility field: retained runtime ownership epoch, not a socket counter. */
  connection_generation?: number;
  runtime_generation?: number;
  host_connection_id?: string;
  host_runtime_id?: string;
  hand_id?: string;
  active?: boolean;
  connected?: boolean;
  close_code?: number;
  lease_expires_at?: number;
  last_heartbeat_at?: number;
  heartbeat_age_ms?: number;
  heartbeat_count?: number;
  pending_call_count?: number;
}>;

export type HostedToolsBrokerCoreOptions = Readonly<{
  now?: () => number;
  randomUUID?: () => string;
  /** Optional operator resource limit; ordinary attachments have no fixed call cap. */
  maxInFlight?: number;
  maxCallsPerGeneration?: number;
  /**
   * Bounded wait for a recoverable route's exact lease/generation to resume
   * after transient transport loss before reporting a new call unavailable.
   * Only never-admitted calls wait; nothing is dispatched until the same
   * runtime epoch is routable again. Defaults to 0 (report immediately).
   */
  reconnectAdmissionWaitMs?: number;
  persistence: HostedToolsBrokerPersistence;
  /** Resume exact live hibernated sockets instead of forcing every route to reconnect. */
  resumeRetainedSockets?: boolean;
  /** Correlated call boundaries only; inputs, outputs, and credentials are omitted. */
  onCallTiming?: (timing: Readonly<{ session_id: string; source_call_id: string; transport_call_id: string; admission_ms: number; roundtrip_ms: number; settlement_ms: number }>) => void;
  onCallObservation?: (observation: HostedToolsCallObservation) => void;
  onConnectionObservation?: (observation: HostedToolsConnectionObservation) => void;
  /** @deprecated Control frames are handled by the WebSocket platform. */
  heartbeatObservationIntervalMs?: number;
  onCatalogChanged?: (definitions: readonly HostedToolsProviderDefinition[]) => void;
  /**
   * Admit a fully validated candidate before it becomes visible. A returned
   * synchronous guard must still authorize publication immediately before ready;
   * owners can use it to fence an admission superseded during asynchronous I/O.
   */
  beforeCatalogPublish?: (candidate: Readonly<{
    routeId: string;
    attachmentId: string | undefined;
    runtimeId: string | undefined;
    publisherIdentity: Readonly<{ machineId: string; runtimeId: string }> | undefined;
    machine: HostedMachine | undefined;
    definitions: readonly HostedToolsCatalogCandidate[];
    generation: number;
    leaseId: string;
  }>) => Promise<(() => boolean) | void>;
  entryAllowed?: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
    context?: HostedToolsAuthorizationContext,
  ) => boolean;
  renewLeasedAttachment?: (
    renewal: HostedToolsLeasedAttachmentRenewal,
  ) => Promise<number | undefined>;
}>;

export type HostedToolsLeasedAttachmentPolicy = Readonly<{
  expectedAttachmentId: string;
  maximumLeaseExpiresAt: number;
  fixedRouteId: string;
  renewalToken: string;
}>;

export type HostedToolsLeasedAttachmentRenewal = Readonly<{
  expectedAttachmentId: string;
  fixedRouteId: string;
  renewalToken: string;
}>;

/** Owns one reverse-tool attachment over an injected socket and durable ledger. */
export class HostedToolsBrokerCore {
  readonly #provider: HostedToolsDynamicProvider;
  readonly #pending = new Map<string, PendingCall>();
  readonly #turnSockets = new Map<string, Set<HostedToolsSocket>>();
  readonly #now: () => number;
  readonly #onCallTiming: HostedToolsBrokerCoreOptions["onCallTiming"];
  readonly #onCallObservation: HostedToolsBrokerCoreOptions["onCallObservation"];
  readonly #onConnectionObservation: HostedToolsBrokerCoreOptions["onConnectionObservation"];
  readonly #randomUUID: () => string;
  readonly #maxInFlight: number | undefined;
  readonly #maxCallsPerGeneration: number;
  readonly #reconnectAdmissionWaitMs: number;
  readonly #persistence: HostedToolsBrokerPersistence;
  readonly #onCatalogChanged: ((definitions: readonly HostedToolsProviderDefinition[]) => void) | undefined;
  readonly #beforeCatalogPublish: HostedToolsBrokerCoreOptions["beforeCatalogPublish"];
  readonly #entryAllowed: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
    context?: HostedToolsAuthorizationContext,
  ) => boolean;
  readonly #renewLeasedAttachment:
    | ((renewal: HostedToolsLeasedAttachmentRenewal) => Promise<number | undefined>)
    | undefined;
  #catalogValidator: HostedToolsCatalogValidator | undefined;
  #nextCandidateGeneration: number;
  #catalogPublication: Promise<void> = Promise.resolve();
  readonly #leaseValidationTimers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(
    readonly context: HostedToolsBrokerCoreContext,
    options: HostedToolsBrokerCoreOptions,
  ) {
    this.#now = options.now ?? Date.now;
    this.#onCallTiming = options.onCallTiming;
    this.#onCallObservation = options.onCallObservation;
    this.#onConnectionObservation = options.onConnectionObservation;
    this.#randomUUID = options.randomUUID ?? (() => crypto.randomUUID());
    this.#maxInFlight = options.maxInFlight;
    if (this.#maxInFlight !== undefined
      && (!Number.isSafeInteger(this.#maxInFlight) || this.#maxInFlight < 1)) {
      throw new TypeError("maxInFlight must be a positive safe integer");
    }
    this.#maxCallsPerGeneration = options.maxCallsPerGeneration ?? Number.MAX_SAFE_INTEGER;
    this.#reconnectAdmissionWaitMs = options.reconnectAdmissionWaitMs ?? 0;
    if (!Number.isSafeInteger(this.#reconnectAdmissionWaitMs) || this.#reconnectAdmissionWaitMs < 0) {
      throw new TypeError("reconnectAdmissionWaitMs must be a non-negative safe integer");
    }
    if (!Number.isSafeInteger(this.#maxCallsPerGeneration) || this.#maxCallsPerGeneration < 1) {
      throw new TypeError("maxCallsPerGeneration must be a positive safe integer");
    }
    this.#persistence = options.persistence;
    this.#onCatalogChanged = options.onCatalogChanged;
    this.#beforeCatalogPublish = options.beforeCatalogPublish;
    this.#entryAllowed = options.entryAllowed ?? (() => true);
    this.#renewLeasedAttachment = options.renewLeasedAttachment;
    const now = this.#now();
    const sockets = this.context.sockets();
    const retainHosts = options.resumeRetainedSockets === true && sockets.length > 0;
    // Initialization settles unproven legacy calls. Capture their public correlation
    // before that transition; a fresh database can legitimately have no tables.
    const interruptedCalls: HostedToolsCallRow[] = [];
    if (this.#onCallObservation && this.#persistence.generationCalls) {
      try {
        for (const state of this.#persistence.states()) {
          if (!state.lease_id) continue;
          interruptedCalls.push(...this.#persistence.generationCalls(state.lease_id, state.generation)
            .filter(row => row.state === "dispatched"));
        }
      } catch { /* First initialization or an unavailable diagnostic read. */ }
    }
    const retained = this.#persistence.initialize(now);
    for (const row of interruptedCalls) {
      if (this.#persistence.call(row.call_id)?.state === "ambiguous") {
        this.#observe("transport_lost", row, { reason_code: "owner_restarted", outcome: "ambiguous" });
      }
    }
    this.#nextCandidateGeneration = Math.max(0, ...this.#persistence.states().map(state => state.generation));
    for (const socket of sockets) {
      const generation = this.#attachment(socket)?.generation;
      if (generation !== undefined) this.#nextCandidateGeneration = Math.max(this.#nextCandidateGeneration, generation);
    }
    for (const state of retained) {
      if (!state.lease_id) continue;
      const recoverable = this.#canRecover(state);
      const retainedSocket = this.#routingSocketForState(state);
      if (recoverable) {
        this.#restoreGeneration(state);
        if (retainedSocket) {
          this.#observeConnection("resumed", this.#attachment(retainedSocket), {}, state);
          this.#requestRecovery(retainedSocket, state);
        }
        continue;
      }
      if (retainHosts && state.lease_expires_at > now && retainedSocket) {
        this.#observeConnection("resumed", this.#attachment(retainedSocket), {}, state);
        continue;
      }
      this.#retireState(state, "Hosted Tools owner restarted", "owner_restarted");
      for (const socket of sockets) {
        const attachment = this.#attachment(socket);
        if (attachment?.leaseId !== state.lease_id || attachment.generation !== state.generation) continue;
        this.context.writeAttachment(socket, { ...attachment, active: false });
        closeSocket(socket, 1012, "Hosted Tools owner restarted");
      }
    }
    this.#provider = Object.freeze({
      endTurn: (session: string, turn: string, event: "Stop" | "Interrupt" | "SubagentStop") => this.endTurn(session, turn, event),
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

  /** Trusted owner HTTP facade. Retains the same lease, deadline, schema and result-conflict checks as WebSocket delivery. */
  completeHttpResult(callId: string, outcome: unknown): void {
    const frame = parseHostedToolsHostFrame(JSON.stringify({ type: "result", call_id: callId, outcome }));
    if (frame.type !== "result") throw new HostedToolsProtocolError("invalid_result", "expected a tool result");
    const row = this.#persistence.call(callId);
    if (!row) throw new HostedToolsProtocolError("unknown_call", "no retained tool call");
    const state = this.#persistence.states().find(state => state.lease_id === row.lease_id && state.generation === row.generation);
    const socket = this.#socketForState(state);
    if (!socket) throw new HostedToolsProtocolError("stale_socket", "tool result requires its active pinned attachment");
    this.#completeResult(socket, frame);
  }

  owns(socket: HostedToolsSocket): boolean { return this.handles(socket); }

  async message(socket: HostedToolsSocket, message: string): Promise<void> {
    await this.webSocketMessage(socket, message);
  }

  close(socket: HostedToolsSocket, reason: string): void {
    if (this.handles(socket)) {
      this.#observeConnection("closed", this.#attachment(socket), { reason_code: "transport_closed" });
      this.#retire(socket, reason, "transport_closed");
    }
  }

  shutdown(reason: string): void {
    const sockets = this.context.sockets();
    for (const socket of sockets) this.#fence(socket, reason, 1008, "owner_shutdown");
    for (const state of this.#persistence.states()) this.#retireState(state, reason, "owner_shutdown");
  }

  /**
   * Retire the current route without prohibiting a later ownership admission.
   * Dispatched calls remain ambiguous in the ledger; they are never reassigned
   * to a successor. Owners must separately fence pending asynchronous admissions
   * with the beforeCatalogPublish guard, including routes not yet published.
   */
  retireRoute(routeId: string, reason: string, closeCode: 1000 | 1008 | 1012 = 1008): boolean {
    const state = this.#persistence.state(routeId) ?? emptyState(routeId);
    const active = state.lease_id !== null;
    if (active) {
      const socket = this.#socketForState(state);
      if (socket) this.#fence(socket, reason, closeCode, "route_revoked");
      else this.#retireState(state, reason, "route_revoked");
    }
    // clearHost retains discovery metadata for transient reconnects. Explicit
    // retirement must remove it even when the retained route has no live lease.
    const retired = this.#persistence.state(routeId) ?? state;
    const catalogChanged = retired.catalog_json !== null || retired.machines_json !== null;
    this.#persistence.replaceHost({
      ...retired,
      host_id: null,
      lease_id: null,
      lease_expires_at: 0,
      catalog_json: null,
      machines_json: null,
    });
    if (catalogChanged) this.#notifyCatalogChanged();
    return active;
  }

  /** Permanently revoke this exact route, including pending admissions. */
  revokeRoute(routeId: string, reason: string): boolean {
    const active = this.retireRoute(routeId, reason);
    // Revocation can race ahead of catalog publication in another Durable
    // Object. Retain an exact route tombstone so a previously validated socket
    // cannot publish after its control-plane fence has completed.
    const retired = this.#persistence.state(routeId) ?? emptyState(routeId);
    this.#persistence.replaceHost({
      ...retired,
      host_id: null,
      lease_id: null,
      lease_expires_at: REVOKED_ROUTE_LEASE_EXPIRES_AT,
      catalog_json: null,
      machines_json: null,
    });
    return active;
  }

  isReady(): boolean { return this.#definitions().length > 0; }

  hasPendingCalls(): boolean { return this.#pending.size > 0; }

  /** Safe current liveness without renewing leases or producing journal events. */
  connectionDiagnostics(): readonly HostedToolsConnectionObservation[] {
    const observations: HostedToolsConnectionObservation[] = [];
    for (const socket of this.context.sockets()) {
      try {
        const attachment = this.#attachment(socket);
        if (!attachment) continue;
        const state = attachment.routeId === undefined ? undefined : this.#persistence.state(attachment.routeId);
        const connected = socket.readyState === OPEN;
        observations.push(this.#connectionObservation("snapshot", attachment, {
          connected,
          active: connected && attachment.active === true && state !== undefined
            && state.lease_id === attachment.leaseId && state.generation === attachment.generation
            && state.lease_expires_at > this.#now(),
        }, state));
      } catch { /* An unavailable diagnostic read must not change a connection. */ }
    }
    return Object.freeze(observations);
  }

  provider(): HostedToolsDynamicProvider { return this.#provider; }

  /**
   * Materialize and index the admitted catalog once for a synchronous request.
   * Admission still rechecks grants, durable ownership, renewal and generation
   * in the prepared handler; the view is discovery, not cached authority.
   */
  /** Names reserved by live attachments and retained native catalogs, without discovery filtering. */
  reservedToolNames(): readonly string[] {
    return this.#publicCatalogBindings(undefined, true).map(binding => binding.entry.definition.name);
  }

  catalogSnapshot(): HostedToolsCatalogSnapshot {
    const bindings = this.#catalogBindings();
    const publicBindings = new Map<string, HostedToolsCatalogBinding>();
    const machineBindings = new Map<string, Map<string, HostedToolsCatalogBinding>>();
    for (const binding of bindings) {
      if (!reservedMachineBinding(binding)) {
        publicBindings.set(binding.entry.definition.name, binding);
      }
      if (binding.machine !== undefined) {
        let tools = machineBindings.get(binding.machine.id);
        if (tools === undefined) {
          tools = new Map();
          machineBindings.set(binding.machine.id, tools);
        }
        // Match the existing sorted find-first machine lookup. Ambiguous
        // exposed names were already removed by #catalogBindings, not hidden
        // by a last-writer-wins index.
        if (!tools.has(binding.wireName)) tools.set(binding.wireName, binding);
      }
    }
    const resolveBinding = (
      name: string,
      binding: HostedToolsCatalogBinding | undefined,
      context?: HostedToolsAuthorizationContext,
    ): HostedToolsCodeTool | undefined => {
      if (binding === undefined || !this.#entryAllowed(
        binding.entry, binding.connectGrantId, binding.appToolCatalogDigest, context,
      )) return undefined;
      return this.#codeTool(name, this.#preparedTool(binding));
    };
    return Object.freeze({
      machines: () => {
        // Keep retained identity (including duplicate-ID fail-closed behavior)
        // separate from live dispatch ownership. Scan routes once, not once
        // per retained machine. Nothing escapes into a persistent cache.
        const machines = this.machines();
        if (machines.length === 0) return [];
        const onlineIds = new Set<string>();
        for (const state of this.#sortedStates()) {
          const socket = this.#liveRoutingSocketForState(state);
          const attachment = socket === undefined ? undefined : this.#attachment(socket);
          if (attachment === undefined || attachment.connectGrantId !== undefined) continue;
          for (const machine of attachment.machines ?? []) onlineIds.add(machine.id);
        }
        return machines.map(machine => ({ machine, online: onlineIds.has(machine.id) }));
      },
      definitions: () => [...publicBindings.values()]
        .filter(binding => this.#entryAllowed(
          binding.entry, binding.connectGrantId, binding.appToolCatalogDigest,
        ))
        .map(binding => Object.freeze({
          ...binding.entry.definition, defer_loading: true as const,
        })),
      resolve: (name: string) => resolveBinding(name, publicBindings.get(name)),
      machineTool: (machineId: string, name: HostedMachineToolName, context?: HostedToolsAuthorizationContext) => {
        if (!MACHINE_TOOL_NAMES.has(name) && !name.startsWith("mcp__cua_repl__")) return undefined;
        return resolveBinding(name, machineBindings.get(machineId)?.get(name), context);
      },
    });
  }

  /** Resolves one canonical machine primitive against its exact admitted attachment generation. */
  machineTool(
    machineId: string,
    name: HostedMachineToolName,
    context?: HostedToolsAuthorizationContext,
  ): HostedToolsCodeTool | undefined {
    if (!MACHINE_TOOL_NAMES.has(name) && !name.startsWith("mcp__cua_repl__")) return undefined;
    const binding = this.#catalogBindings().find((candidate) => (
      candidate.machine?.id === machineId && candidate.wireName === name
    ));
    if (!binding) return undefined;
    const prepared = this.#preparedTool(binding);
    if (!this.#entryAllowed(
      prepared.entry,
      prepared.connectGrantId,
      prepared.appToolCatalogDigest,
      context,
    )) return undefined;
    return this.#codeTool(name, prepared);
  }

  /** Resolves one canonical machine primitive only on the named durable route. */
  machineToolOnRoute(
    routeId: string,
    machineId: string,
    name: HostedMachineToolName,
    context?: HostedToolsAuthorizationContext,
  ): HostedToolsCodeTool | undefined {
    if (!MACHINE_TOOL_NAMES.has(name) && !name.startsWith("mcp__cua_repl__")) return undefined;
    const binding = this.#catalogBindings(undefined, true).find((candidate) => (
      candidate.routeId === routeId
      && candidate.machine?.id === machineId
      && candidate.wireName === name
    ));
    if (!binding) return undefined;
    const prepared = this.#preparedTool(binding);
    if (!this.#entryAllowed(
      prepared.entry,
      prepared.connectGrantId,
      prepared.appToolCatalogDigest,
      context,
    )) return undefined;
    return this.#codeTool(name, prepared);
  }

  /** Returns one live machine only when its exact durable route still owns it. */
  machineOnRoute(routeId: string, machineId: string): HostedMachine | undefined {
    const state = this.#persistence.state(routeId);
    if (state === undefined) return undefined;
    const socket = this.#liveRoutingSocketForState(state);
    if (socket === undefined) return undefined;
    const attachment = this.#attachment(socket);
    if (attachment?.connectGrantId !== undefined) return undefined;
    return attachment?.machines?.find(({ id }) => id === machineId);
  }

  /** Transport presence is separate from the retained machine identity. */
  machineOnline(machineId: string): boolean {
    return this.#sortedStates().some((state) => this.machineOnRoute(state.route_id, machineId) !== undefined);
  }

  /** Machine identity survives transport loss; dispatch still requires its live route. */
  machines(): readonly HostedMachine[] {
    const machines = new Map<string, HostedMachine>();
    for (const state of this.#sortedStates()) {
      if (!state.catalog_json || !state.machines_json) continue;
      for (const machine of JSON.parse(state.machines_json) as HostedMachine[]) {
        if (machines.has(machine.id)) return [];
        machines.set(machine.id, machine);
      }
    }
    return [...machines.values()].sort((left, right) => left.id.localeCompare(right.id));
  }

  accept(
    socket: HostedToolsSocket,
    sessionId: string,
    allowedMcpIds?: readonly string[],
    appToolCatalogDigest?: `0x${string}`,
    connectGrantId?: string,
    leasedAttachment?: HostedToolsLeasedAttachmentPolicy,
    publisherIdentity?: Readonly<{ machineId: string; runtimeId: string }>,
  ): void {
    if (allowedMcpIds !== undefined && !isConnectGrantId(connectGrantId)) {
      throw new TypeError("Connect Hosted Tools requires an exact grant ID");
    }
    if (leasedAttachment !== undefined && (connectGrantId !== undefined
      || leasedAttachment.expectedAttachmentId.length === 0
      || leasedAttachment.fixedRouteId.length === 0
      || leasedAttachment.renewalToken.length === 0
      || leasedAttachment.renewalToken.length > 2_048
      || !Number.isSafeInteger(leasedAttachment.maximumLeaseExpiresAt))) {
      throw new TypeError("leased Hosted Tools requires one complete ordinary-route policy");
    }
    if (publisherIdentity !== undefined && (connectGrantId !== undefined
      || typeof publisherIdentity.machineId !== "string" || publisherIdentity.machineId.length === 0
      || typeof publisherIdentity.runtimeId !== "string" || publisherIdentity.runtimeId.length === 0)) {
      throw new TypeError("Hosted Tools publisher identity requires an account machine and runtime ID");
    }
    this.context.writeAttachment(socket, {
      kind: SOCKET_TAG,
      connectionId: crypto.randomUUID(),
      sessionId,
      ...(allowedMcpIds === undefined ? {} : { allowedMcpIds: [...allowedMcpIds] }),
      ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
      ...(connectGrantId === undefined ? {} : { connectGrantId }),
      ...(publisherIdentity === undefined ? {} : { publisherIdentity: { ...publisherIdentity } }),
      ...(leasedAttachment === undefined ? {} : {
        expectedAttachmentId: leasedAttachment.expectedAttachmentId,
        maximumLeaseExpiresAt: leasedAttachment.maximumLeaseExpiresAt,
        fixedRouteId: leasedAttachment.fixedRouteId,
        renewalToken: leasedAttachment.renewalToken,
      }),
    } satisfies HostedToolsSocketAttachment);
    this.context.accept(socket);
    this.#observeConnection("accepted", this.#attachment(socket));
  }

  handles(socket: HostedToolsSocket): boolean {
    return this.#attachment(socket)?.kind === SOCKET_TAG;
  }

  async webSocketMessage(socket: HostedToolsSocket, message: string | ArrayBuffer): Promise<void> {
    const receivedAt = performance.now();
    if (!this.handles(socket)) return;
    if (typeof message !== "string") {
      this.#fence(socket, "Hosted Tools requires bounded text frames", 1003);
      return;
    }
    let frame: HostedToolsHostFrame;
    try {
      const decodeStarted = performance.now();
      frame = parseHostedToolsHostFrame(message);
      const frameDecodeMs = Math.max(0, performance.now() - decodeStarted);
      await this.#dispatchHostFrame(socket, frame, receivedAt, frameDecodeMs);
    } catch (error) {
      const protocol = error instanceof HostedToolsProtocolError
        ? error
        : new HostedToolsProtocolError("broker_failure", errorMessage(error));
      const attachment = this.#attachment(socket);
      const state = attachment?.routeId === undefined ? undefined : this.#persistence.state(attachment.routeId);
      const deliveryFailure = protocol.code === "ready_send_failed" || protocol.code === "drain_send_failed";
      const reasonCode: HostedToolsDiagnosticReason = deliveryFailure ? protocol.code as "ready_send_failed" | "drain_send_failed"
        : protocol.code === "lease_validation_unavailable" ? "lease_validation_unavailable"
        : protocol.code === "route_revoked" ? "route_revoked"
        : protocol.code === "stale_socket" && state && state.lease_id === attachment?.leaseId
          && state.generation === attachment?.generation && state.lease_expires_at <= this.#now() ? "lease_expired"
        : "protocol_error";
      this.#observeConnection("error", attachment, { reason_code: reasonCode });
      this.#fence(socket, `${protocol.code}: ${protocol.message}`,
        reasonCode === "lease_expired" ? 1012
          : protocol.code === "broker_failure" || protocol.code === "lease_validation_unavailable" || deliveryFailure ? 1011 : 1008,
        reasonCode);
    }
  }

  webSocketClose(socket: HostedToolsSocket, code: number, reason: string): void {
    if (!this.handles(socket)) return;
    this.#observeConnection("closed", this.#attachment(socket), { close_code: code, reason_code: "transport_closed" });
    this.#retire(socket, reason || `peer closed with code ${code}`, code === 1008 ? "protocol_error" : "transport_closed");
    closeSocket(socket, code, reason || "Hosted Tools peer closed");
  }

  webSocketError(socket: HostedToolsSocket): void {
    if (!this.handles(socket)) return;
    this.#observeConnection("error", this.#attachment(socket), { close_code: 1011, reason_code: "transport_error" });
    this.#retire(socket, "WebSocket failed", "transport_error");
    closeSocket(socket, 1011, "Hosted Tools WebSocket failed");
  }

  /** May be called by an owning alarm; normal reads and call timers also expire leases lazily. */
  expire(): void {
    for (const state of this.#persistence.states()) {
      if (!state.lease_id || state.lease_expires_at > this.#now()) continue;
      const socket = this.#socketForState(state);
      if (socket && this.#attachment(socket)?.renewalToken) void this.#checkCachedLease(socket);
      else if (!this.#canRecover(state)) {
        if (socket) this.#fence(socket, "Hosted Tools lease expired", 1012, "lease_expired");
        else this.#retireState(state, "Hosted Tools lease expired", "lease_expired");
      }
    }
  }

  async endTurn(sessionId: string, turnId: string, hookEventName: "Stop" | "Interrupt" | "SubagentStop"): Promise<void> {
    const key = JSON.stringify([sessionId, turnId]);
    const sockets = this.#turnSockets.get(key) ?? new Set<HostedToolsSocket>();
    this.#turnSockets.delete(key);
    // Only the original live socket may receive cleanup; a restored broker has
    // no such ownership and must not reconstruct it from a reconnectable ledger.
    for (const socket of sockets) {
      const attachment = this.#attachment(socket);
      // Never move cleanup to a replacement runtime, reconnect, or unadvertised host.
      if (!attachment?.active || !attachment.turnLifecycle || socket.readyState !== OPEN) continue;
      this.#send(socket, { type: "turn_ended", session_id: sessionId, turn_id: turnId, hook_event_name: hookEventName });
    }
  }

  /**
   * Receipt-only reconciliation for one exact source call identity. This is a
   * ledger lookup: it never admits, dispatches, cancels or repins a call, so a
   * missing row stays missing. A dispatched call is awaited only through its
   * original pinned lease/generation/runtime, bounded by the caller's wait and
   * the call's original admitted deadline.
   */
  async receipt(request: HostedToolsReceiptRequest): Promise<HostedToolsReceipt> {
    const row = this.#persistence.callBySource(request.sessionId, request.callId);
    if (!row || row.session_id !== request.sessionId || row.source_call_id !== request.callId) return { state: "missing" };
    const candidates = this.#catalogBindings(undefined, true).filter(candidate => (request.machineId === undefined
      ? candidate.machine === undefined && candidate.entry.definition.name === request.name
      : candidate.machine?.id === request.machineId && candidate.wireName === request.name)
      && candidate.hostId === row.host_id);
    // Authorize and attribute only from the row's own pinned route. A
    // reconnect of the same Hand runtime may carry it under a later
    // generation; a replacement runtime never resolves this receipt.
    const exact = candidates.find(candidate => candidate.leaseId === row.lease_id && candidate.generation === row.generation);
    const binding = exact ?? (row.host_runtime_id
      ? candidates.find(candidate => candidate.runtimeId === row.host_runtime_id) : undefined);
    if (!binding || row.name !== binding.wireName
      || (request.machineId !== undefined && row.hand_id !== request.machineId)
      || request.routeToken !== JSON.stringify(row.name === "write_stdin" && binding.machine && row.host_runtime_id
        // The caller must present the route this exact row was admitted under.
        ? [binding.routeId, "process-runtime", row.host_runtime_id, row.name]
        : [binding.routeId, row.generation, row.lease_id, row.name])
      || !this.#entryAllowed(binding.entry, binding.connectGrantId, binding.appToolCatalogDigest, request.context)) {
      return { state: "unresolved" };
    }
    this.#observe("receipt_lookup", row);
    let outcome: HostedToolCallOutcome | undefined;
    if (row.state === "ambiguous" && row.receipt_json) {
      // A terminal result that arrived after the deadline is real evidence.
      const late = JSON.parse(row.receipt_json) as { outcome?: HostedToolCallOutcome };
      outcome = late.outcome;
    }
    if (outcome) { /* late receipt */ }
    else if (row.result_json) outcome = JSON.parse(row.result_json) as HostedToolCallOutcome;
    else if (row.state === "dispatched") {
      const owner = this.#stateForLease(row.lease_id, row.generation);
      const pending = this.#pending.get(row.call_id)
        ?? (owner && this.#canRecover(owner) && owner.runtime_id === row.host_runtime_id ? this.#restorePending(row) : undefined);
      if (!pending) outcome = hostedToolsAmbiguous("Hosted Tools call has no retained terminal receipt");
      else {
        // Never attach a signal: a receipt read cannot cancel the call.
        const waitMs = Math.max(0, Math.min(request.waitMs, row.deadline_at - this.#now()));
        let timer: ReturnType<typeof setTimeout> | undefined;
        const settled = await Promise.race([
          pending.promise,
          new Promise<undefined>(resolve => { timer = setTimeout(() => resolve(undefined), waitMs); }),
        ]).finally(() => clearTimeout(timer));
        if (!settled) return { state: "running", deadlineAt: row.deadline_at };
        outcome = settled;
      }
    } else if (row.state === "admitted") outcome = hostedToolsUnavailable("Hosted Tools call was admitted but never dispatched");
    else outcome = hostedToolsAmbiguous("Hosted Tools call has no retained terminal receipt");
    if (outcome.status !== "completed") return { state: "settled", result: toolResult(outcome.message, outcome, false, null) };
    const final = outcome;
    const structured: unknown = final.output.structured_result;
    const sessionBound = structured !== null && typeof structured === "object" && "session_id" in structured;
    let processRouteToken: string | undefined;
    if (row.name === "exec_command" && request.machineId !== undefined && sessionBound) {
      // A process session belongs to the runtime that started it. Only bind
      // the write_stdin route of that exact lease/generation/runtime.
      const stdin = this.#catalogBindings(undefined, true).find(candidate => candidate.machine?.id === request.machineId
        && candidate.wireName === "write_stdin" && candidate.leaseId === row.lease_id
        && candidate.generation === row.generation && candidate.runtimeId === row.host_runtime_id);
      if (!stdin) {
        const message = "The retained command receipt belongs to an earlier Hand connection and cannot prove process ownership. Use its original saved process session if available. The command was not resent.";
        return { state: "settled", result: toolResult(message, hostedToolsAmbiguous(message), false, null) };
      }
      processRouteToken = this.#preparedTool(stdin).routeToken;
    }
    const prepared = this.#preparedTool(binding);
    return { state: "settled", result: wireToolResult(final.output, prepared.canonicalName, prepared.machine),
      ...(processRouteToken === undefined ? {} : { processRouteToken }) };
  }

  cancel(callId: string): boolean {
    return this.cancelDelivery(callId) !== false;
  }

  /**
   * Requests cancellation and reports how far it got: "sent" when the cancel
   * frame was written to the live Hand socket, "queued" when it is only
   * persisted for redelivery once the same runtime reconnects.
   */
  cancelDelivery(callId: string): "sent" | "queued" | false {
    const row = this.#persistence.call(callId);
    if (!row || row.state !== "dispatched") return false;
    const state = this.#stateForLease(row.lease_id, row.generation);
    const socket = this.#socketForState(state);
    if (!state || (state.lease_expires_at <= this.#now() && !this.#canRecover(state))) {
      this.#finishAmbiguous(row, "Hosted Tools cancellation lost its pinned attachment", "lease_expired");
      return false;
    }
    const cancelRequested = this.#persistence.markCancelRequested(callId, this.#now());
    if (!cancelRequested || cancelRequested.state !== "dispatched"
      || cancelRequested.cancel_requested !== 1) return false;
    this.#observe("cancel_requested", row);
    if (!socket) return this.#canRecover(state) ? "queued" : false;
    try {
      this.#send(socket, {
        type: "cancel",
        call_id: row.call_id,
      });
      return "sent";
    } catch {
      this.#retire(socket, "cancellation delivery failed", "cancel_send_failed");
      closeSocket(socket, 1011, "Hosted Tools cancellation delivery failed");
      return false;
    }
  }

  async #dispatchHostFrame(
    socket: HostedToolsSocket, frame: HostedToolsHostFrame, receivedAt: number, frameDecodeMs: number,
  ): Promise<void> {
    if (frame.type === "catalog") {
      const publication = this.#catalogPublication.then(() => this.#publishCatalog(socket, frame));
      this.#catalogPublication = publication.catch(() => {});
      await publication;
      return;
    }
    const validationStarted = performance.now();
    await this.#validateLeasedAttachment(socket);
    const handlerStarted = performance.now();
    const timing: HostFrameTiming = { receivedAt, frame_decode_ms: frameDecodeMs,
      lease_validation_ms: Math.max(0, handlerStarted - validationStarted),
      message_to_handler_ms: Math.max(0, handlerStarted - receivedAt) };
    if (frame.type === "ping") {
      // Older publishers do not advertise command recovery and still expect
      // JSON pongs. Preserve ownership/authority checks without a liveness TTL.
      this.#activeAttachment(socket);
      this.#send(socket, { type: "pong", nonce: frame.nonce });
    } else if (frame.type === "status") this.#recoverStatus(socket, frame);
    else if (frame.type === "drain") this.#drain(socket);
    else if (frame.type === "diagnostic") this.#hostProgress(socket, frame, timing);
    else this.#completeResult(socket, frame, timing);
  }

  #hostProgress(
    socket: HostedToolsSocket, frame: Extract<HostedToolsHostFrame, { type: "diagnostic" }>, timing: HostFrameTiming,
  ): void {
    const attachment = this.#activeAttachment(socket);
    if (attachment.diagnostics !== true) {
      throw new HostedToolsProtocolError("diagnostics_not_advertised", "host diagnostics require catalog opt-in");
    }
    const row = this.#persistence.call(frame.call_id);
    if (!row || row.lease_id !== attachment.leaseId || row.generation !== attachment.generation) {
      throw new HostedToolsProtocolError("unknown_call", "diagnostic does not match an admitted pinned call");
    }
    // A result can settle before queued progress arrives. Same-generation terminal
    // rows retain identity and remain observable without changing their outcome.
    this.#observe("host_progress", row, { host_stage: frame.stage, host_elapsed_ms: frame.elapsed_ms,
      ...this.#frameObservation(frame.call_id, timing) });
  }

  #frameObservation(callId: string, timing?: HostFrameTiming): Partial<HostedToolsCallObservation> {
    if (!timing) return {};
    const { receivedAt, ...durations } = timing;
    const pending = this.#pending.get(callId);
    return { ...durations, ...(pending && !pending.restored
      ? { dispatch_to_message_ms: Math.max(0, receivedAt - pending.dispatchedAt) } : {}) };
  }

  #activeAttachment(socket: HostedToolsSocket): HostedToolsSocketAttachment {
    const attachment = this.#attachment(socket);
    const state = attachment?.routeId === undefined
      ? undefined
      : this.#persistence.state(attachment.routeId);
    if (!attachment?.active || !attachment.leaseId || attachment.generation === undefined || !state
      || state.lease_id !== attachment.leaseId
      || state.generation !== attachment.generation
      || this.#socketForState(state) !== socket
      || state.lease_expires_at <= this.#now()) {
      throw new HostedToolsProtocolError("stale_socket", "socket no longer owns the tool attachment");
    }
    return attachment;
  }

  async #validateLeasedAttachment(socket: HostedToolsSocket): Promise<void> {
    const attachment = this.#attachment(socket);
    if (!attachment?.renewalToken) return;
    const state = attachment.routeId ? this.#persistence.state(attachment.routeId) : undefined;
    if (!attachment.active || !state || this.#socketForState(state) !== socket) {
      throw new HostedToolsProtocolError("stale_socket", "socket no longer owns the tool attachment");
    }
    let renewed: number | undefined;
    try {
      renewed = await this.#renewLeaseAuthority({
        expectedAttachmentId: attachment.expectedAttachmentId!,
        fixedRouteId: attachment.fixedRouteId!, renewalToken: attachment.renewalToken,
      });
    } catch {
      throw new HostedToolsProtocolError("lease_validation_unavailable", "leased Hosted Tools validation unavailable");
    }
    const current = this.#persistence.state(state.route_id);
    if (!current || this.#socketForState(current) !== socket) {
      throw new HostedToolsProtocolError("stale_socket", "socket changed while validating authorization");
    }
    if (!Number.isSafeInteger(renewed) || Number(renewed) <= this.#now()) {
      this.revokeRoute(attachment.fixedRouteId!, "leased Hosted Tools validation failed");
      throw new HostedToolsProtocolError("route_revoked", "leased Hosted Tools validation failed");
    }
    const refreshed = { ...current, lease_expires_at: Number(renewed) };
    this.#persistence.replaceHost(refreshed);
    this.context.writeAttachment(socket, { ...this.#attachment(socket), maximumLeaseExpiresAt: Number(renewed), lease_expires_at: Number(renewed) });
    this.#armLeaseValidation(refreshed);
  }

  #canRecover(state: HostedToolsStateRow): boolean {
    return state.command_recovery === 1 && !!state.runtime_id && !!state.catalog_json && !!this.#persistence.generationCalls;
  }

  #restorePending(row: HostedToolsCallRow): PendingCall {
    const existing = this.#pending.get(row.call_id);
    if (existing) return existing;
    let resolve!: PendingCall["resolve"];
    const promise = new Promise<HostedToolCallOutcome>(done => { resolve = done; });
    const pending: PendingCall = {
      ...(row.thread_id ? { threadId: row.thread_id } : {}),
      receivedAt: performance.now(), dispatchedAt: performance.now(), leaseId: row.lease_id,
      generation: row.generation, deadlineAt: row.deadline_at, promise, resolve, restored: true,
    };
    this.#pending.set(row.call_id, pending);
    this.#armExpiry(row.call_id, pending);
    return pending;
  }

  #restoreGeneration(state: HostedToolsStateRow): void {
    for (const row of this.#persistence.generationCalls?.(state.lease_id!, state.generation) ?? []) {
      if (row.state !== "dispatched") continue;
      if (row.host_runtime_id !== state.runtime_id || row.host_id !== state.host_id) {
        this.#finishAmbiguous(row, "Hosted Tools retained runtime identity does not match", "owner_restarted");
      } else if (row.deadline_at <= this.#now()) {
        this.#finishAmbiguous(row, "Hosted Tools call deadline expired after dispatch", "call_deadline");
      } else this.#restorePending(row);
    }
  }

  #requestRecovery(socket: HostedToolsSocket, state: HostedToolsStateRow): void {
    this.#armLeaseValidation(state);
    const ids = (this.#persistence.generationCalls?.(state.lease_id!, state.generation) ?? [])
      .filter(row => row.state === "dispatched").map(row => row.call_id);
    try {
      for (let offset = 0; offset < ids.length; offset += MAX_HOSTED_TOOLS_RECOVER_CALL_IDS) {
        this.#send(socket, { type: "recover", call_ids: ids.slice(offset, offset + MAX_HOSTED_TOOLS_RECOVER_CALL_IDS) });
      }
    } catch {
      this.#retire(socket, "recovery delivery failed", "call_send_failed");
      closeSocket(socket, 1011, "Hosted Tools recovery delivery failed");
    }
  }

  #recoverStatus(socket: HostedToolsSocket, frame: Extract<HostedToolsHostFrame, { type: "status" }>): void {
    const attachment = this.#activeAttachment(socket);
    const state = this.#persistence.state(attachment.routeId!)!;
    const row = this.#persistence.call(frame.call_id);
    if (!this.#canRecover(state) || !row || row.lease_id !== attachment.leaseId
      || row.generation !== attachment.generation || row.host_runtime_id !== attachment.runtimeId) {
      throw new HostedToolsProtocolError("unknown_call", "status does not match the retained runtime call");
    }
    if (row.state !== "dispatched") return;
    if (frame.state === "missing") {
      this.#finishAmbiguous(row, "The living Hand runtime has no retained proof of this dispatched call; it was not resent");
    } else if (row.deadline_at <= this.#now()) {
      this.#finishAmbiguous(row, "Hosted Tools call deadline expired after dispatch", "call_deadline");
    } else {
      this.#restorePending(row);
      if (row.cancel_requested === 1) {
        try { this.#send(socket, { type: "cancel", call_id: row.call_id }); }
        catch {
          this.#retire(socket, "cancellation delivery failed", "cancel_send_failed");
          closeSocket(socket, 1011, "Hosted Tools cancellation delivery failed");
        }
      }
    }
  }

  async #publishCatalog(
    socket: HostedToolsSocket,
    frame: Extract<HostedToolsHostFrame, { type: "catalog" }>,
  ): Promise<void> {
    // Keep exec and CUA available when an older Hand still advertises browser tools.
    frame = { ...frame, tools: frame.tools.filter((entry) => !disabledBrowserTool(entry.definition.name)) };
    const initial = this.#attachment(socket);
    if (!initial || initial.leaseId || initial.generation !== undefined || initial.active) {
      throw new HostedToolsProtocolError("catalog_immutable", "one immutable catalog is allowed per socket");
    }
    if (this.#nextCandidateGeneration >= Number.MAX_SAFE_INTEGER) {
      throw new HostedToolsProtocolError("generation_exhausted", "Hosted Tools generation is exhausted");
    }
    const routeId = initial.fixedRouteId ?? scopedRouteId(initial.connectGrantId, frame.attachment_id);
    let maximumLeaseExpiresAt = initial.maximumLeaseExpiresAt;
    if (initial.renewalToken !== undefined) {
      const renewal = {
        expectedAttachmentId: initial.expectedAttachmentId!,
        fixedRouteId: routeId,
        renewalToken: initial.renewalToken,
      };
      try {
        const renewed = await this.#renewLeaseAuthority(renewal);
        if (!Number.isSafeInteger(renewed) || Number(renewed) <= this.#now()) {
          this.revokeRoute(routeId, "leased Hosted Tools validation failed before admission");
          throw new HostedToolsProtocolError(
            "route_revoked",
            "leased Hosted Tools validation failed before admission",
          );
        }
        maximumLeaseExpiresAt = Number(renewed);
      } catch (error) {
        if (error instanceof HostedToolsProtocolError) throw error;
        throw new HostedToolsProtocolError(
          "lease_validation_unavailable",
          "leased Hosted Tools validation was unavailable before admission",
        );
      }
    }
    let state = { ...(this.#persistence.state(routeId) ?? emptyState(routeId)) };
    if (state.lease_id === null && state.lease_expires_at === REVOKED_ROUTE_LEASE_EXPIRES_AT) {
      throw new HostedToolsProtocolError(
        "route_revoked",
        "tool attachment route was revoked before admission",
      );
    }
    if (state.lease_id && state.lease_expires_at <= this.#now()
      && !(initial.renewalToken && this.#canRecover(state))) {
      const expiredSocket = this.#socketForState(state);
      if (expiredSocket) this.#fence(expiredSocket, "Hosted Tools lease expired", 1012, "lease_expired");
      else this.#retireState(state, "Hosted Tools lease expired", "lease_expired");
      state = { ...(this.#persistence.state(routeId) ?? emptyState(routeId)) };
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
    const catalogJson = canonicalJson(frame.tools);
    const machinesJson = frame.machines?.length ? canonicalJson(frame.machines) : null;
    const resumes = this.#canRecover(state) && frame.command_recovery === true
      && state.host_id === initial.sessionId && state.runtime_id === frame.runtime_id
      && (state.connect_grant_id ?? null) === (initial.connectGrantId ?? null)
      && state.catalog_json === catalogJson && state.machines_json === machinesJson;
    const generation = resumes ? state.generation : ++this.#nextCandidateGeneration;
    const leaseId = resumes ? state.lease_id! : this.#randomUUID();
    const expiresAt = maximumLeaseExpiresAt ?? Number.MAX_SAFE_INTEGER;
    const candidate = {
      ...initial,
      ...(maximumLeaseExpiresAt === undefined ? {} : { maximumLeaseExpiresAt }),
      routeId,
      leaseId,
      generation,
      lease_expires_at: expiresAt,
      ...(frame.runtime_id === undefined ? {} : { runtimeId: frame.runtime_id }),
      ...(frame.command_recovery === true ? { commandRecovery: true as const } : {}),
      ...(frame.turn_lifecycle === true ? { turnLifecycle: true as const } : {}),
      ...(frame.connection_id === undefined ? {} : { hostConnectionId: frame.connection_id }),
      ...(frame.diagnostics === true ? { diagnostics: true as const } : {}),
    } satisfies HostedToolsSocketAttachment;
    this.context.writeAttachment(socket, candidate);
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
      if (frame.command_recovery && !this.#persistence.generationCalls) {
        throw new Error("command recovery requires durable call enumeration");
      }
      if (initial.connectGrantId !== undefined && (frame.machines?.length ?? 0) > 0) {
        throw new Error("Connect tool hosts cannot publish account machine metadata");
      }
      if ((frame.machines?.length ?? 0) > 0
        && (frame.machines?.length !== 1 || frame.attachment_id !== frame.machines[0]?.id)) {
        throw new Error("an account machine route requires one machine whose id equals attachment_id");
      }
      if (initial.publisherIdentity !== undefined
        && (frame.attachment_id !== initial.publisherIdentity.machineId
          || frame.machines?.length !== 1
          || frame.machines[0]?.id !== initial.publisherIdentity.machineId
          || frame.runtime_id !== initial.publisherIdentity.runtimeId)) {
        throw new Error("catalog must match the publisher's connect-time machine and runtime identity");
      }
      if (initial.expectedAttachmentId !== undefined
        && (frame.attachment_id !== initial.expectedAttachmentId
          || frame.machines?.length !== 1
          || frame.machines[0]?.id !== initial.expectedAttachmentId)) {
        throw new Error("leased tool attachment must publish its exact assigned machine ID");
      }
      if (initial.expectedAttachmentId !== undefined) {
        const extra = frame.tools.find((entry) => (
          !MACHINE_TOOL_NAMES.has(entry.definition.name)
          && !entry.definition.name.startsWith("mcp__cua_repl__")
        ));
        if (extra !== undefined) {
          throw new Error(
            "leased tool attachments may publish only canonical machine primitives",
          );
        }
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
    let commitGuard: (() => boolean) | void = undefined;
    if (this.#beforeCatalogPublish) {
      try {
        commitGuard = await this.#beforeCatalogPublish({
          routeId,
          attachmentId: frame.attachment_id,
          runtimeId: frame.runtime_id,
          publisherIdentity: initial.publisherIdentity,
          machine,
          definitions: candidateDefinitions,
          generation,
          leaseId,
        });
      } catch (error) {
        throw new HostedToolsProtocolError(
          "catalog_contract_mismatch",
          `candidate catalog admission failed: ${errorMessage(error)}`,
        );
      }
    }
    // Only this route's ownership matters. An unrelated attachment may have
    // retired while admission awaited its owner directory.
    const current = this.#persistence.state(routeId) ?? emptyState(routeId);
    if ((current.lease_id === null && current.lease_expires_at === REVOKED_ROUTE_LEASE_EXPIRES_AT)
      || current.lease_id !== state.lease_id || current.generation !== state.generation
      || current.catalog_json !== state.catalog_json) {
      throw new HostedToolsProtocolError("route_revoked", "tool attachment ownership changed before publication");
    }
    const currentCandidate = this.#attachment(socket);
    if (socket.readyState !== OPEN || currentCandidate?.routeId !== routeId
      || currentCandidate.leaseId !== leaseId || currentCandidate.generation !== generation
      || currentCandidate.active === true) {
      throw new HostedToolsProtocolError("stale_socket", "candidate socket disconnected or changed before publication");
    }
    const now = this.#now();
    if (expiresAt <= now) {
      throw new HostedToolsProtocolError("route_revoked", "tool attachment lease expired before publication");
    }
    const replaced = !resumes && state.lease_id ? state : undefined;
    const oldSockets = this.context.sockets().filter(existing => {
      const old = this.#attachment(existing);
      return existing !== socket && old?.active && old.leaseId === state.lease_id && old.generation === state.generation;
    });
    // Publications are queued within this broker, but admission may await an
    // external owner. Recheck exposed names before committing that admission.
    const currentExposedNames = new Set(this.#publicCatalogBindings(routeId, true)
      .map(binding => binding.entry.definition.name));
    const conflictingDefinition = candidateDefinitions.find(entry => currentExposedNames.has(entry.definition.name));
    if (conflictingDefinition) {
      throw new HostedToolsProtocolError(
        "catalog_contract_mismatch",
        `tool name ${conflictingDefinition.definition.name} is already exposed by another attachment`,
      );
    }
    // No asynchronous work may occur between this owner fence and publication.
    if (commitGuard && !commitGuard()) {
      throw new HostedToolsProtocolError("route_revoked", "tool attachment admission was superseded before publication");
    }
    // Fence the physical attachment before ready; the ownership epoch may stay unchanged.
    for (const old of oldSockets) this.context.writeAttachment(old, { ...this.#attachment(old), active: false });
    try { this.#send(socket, { type: "ready" }); }
    catch (error) {
      for (const old of oldSockets) this.context.writeAttachment(old, { ...this.#attachment(old), active: true });
      throw error;
    }
    if (replaced?.lease_id) this.#observeGenerationCalls(replaced.lease_id, replaced.generation, "host_replaced");
    this.#persistence.transaction(() => {
      if (!resumes && state.lease_id) {
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
        machines_json: machinesJson,
        runtime_id: frame.runtime_id ?? null,
        command_recovery: frame.command_recovery === true ? 1 : 0,
        connect_grant_id: initial.connectGrantId ?? null,
      });
    });
    if (replaced?.lease_id) {
      const outcome = hostedToolsAmbiguous("Hosted Tools call became ambiguous when its host was replaced");
      this.#resolveGeneration(replaced.lease_id, replaced.generation, outcome);
    }
    for (const existing of oldSockets) {
      this.#observeConnection("replaced", this.#attachment(existing), {
        reason_code: "host_replaced", close_code: resumes ? 1012 : 1008,
      }, state);
      closeSocket(existing, resumes ? 1012 : 1008, resumes ? "Hosted Tools runtime reattached" : "Hosted Tools attachment replaced");
    }
    this.context.writeAttachment(
      socket,
      {
        ...candidate,
        active: true,
        ...(frame.machines === undefined ? {} : { machines: frame.machines }),
      } satisfies HostedToolsSocketAttachment,
    );
    this.#observeConnection(resumes ? "resumed" : "ready", this.#attachment(socket));
    if (this.#canRecover(this.#persistence.state(routeId)!)) {
      this.#restoreGeneration(this.#persistence.state(routeId)!);
      this.#requestRecovery(socket, this.#persistence.state(routeId)!);
    }
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
    this.#observeConnection("draining", this.#attachment(socket), { reason_code: "host_draining" }, state);
    this.#observeGenerationCalls(state.lease_id!, state.generation, "host_draining", "connection_draining");
    this.#notifyCatalogChanged();
    this.#send(socket, { type: "draining" });
  }

  #completeResult(
    socket: HostedToolsSocket,
    frame: Extract<HostedToolsHostFrame, { type: "result" }>,
    timing?: HostFrameTiming,
  ): void {
    const resultAt = performance.now();
    const receiveTiming = this.#frameObservation(frame.call_id, timing);
    const attachment = this.#activeAttachment(socket);
    const row = this.#persistence.call(frame.call_id);
    const stored = JSON.stringify(frame.outcome);
    if (!row
      || row.lease_id !== attachment.leaseId
      || row.generation !== attachment.generation
      || row.host_id !== attachment.sessionId
      || (row.host_runtime_id != null && row.host_runtime_id !== attachment.runtimeId)) {
      throw new HostedToolsProtocolError("unknown_call", "result does not match an admitted pinned call");
    }
    if (row.state === "ambiguous") {
      const receiptJson = JSON.stringify({ type: "result", outcome: frame.outcome });
      const recorded = this.#persistence.recordLateReceipt(row.call_id, receiptJson, this.#now());
      if (!recorded || recorded.receipt_json !== receiptJson) {
        throw new HostedToolsProtocolError("result_conflict", "late terminal receipt conflicts with retained proof");
      }
      this.#ackResult(socket, frame);
      this.#observe("late_receipt", row, { ...receiveTiming, outcome: frame.outcome.status, ...(frame.timing ? { host_timing: frame.timing } : {}) });
      return;
    }
    if (row.state !== "dispatched") {
      if (row.result_json === stored && row.state === outcomeState(frame.outcome)) {
        this.#ackResult(socket, frame);
        this.#observe("receipt_replay", row, { ...receiveTiming, outcome: frame.outcome.status, ...(frame.timing ? { host_timing: frame.timing } : {}) });
        return;
      }
      throw new HostedToolsProtocolError("result_conflict", "terminal call result cannot be changed");
    }
    if (this.#now() >= row.deadline_at) {
      this.#finishAmbiguous(row, "Hosted Tools call result arrived after its durable deadline", "call_deadline");
      const receiptJson = JSON.stringify({ type: "result", outcome: frame.outcome });
      const recorded = this.#persistence.recordLateReceipt(row.call_id, receiptJson, this.#now());
      if (!recorded || recorded.receipt_json !== receiptJson) {
        throw new HostedToolsProtocolError("result_conflict", "late terminal receipt conflicts with retained proof");
      }
      this.#ackResult(socket, frame);
      this.#observe("late_receipt", row, { ...receiveTiming, outcome: frame.outcome.status, ...(frame.timing ? { host_timing: frame.timing } : {}) });
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
    this.#ackResult(socket, frame);
    pending?.resolve(frame.outcome);
    this.#observe("receipt", { ...row, thread_id: row.thread_id ?? pending?.threadId }, {
      ...receiveTiming,
      outcome: frame.outcome.status,
      ...(frame.outcome.status === "completed" ? { success: frame.outcome.output.success } : {}),
      ...(frame.timing ? { host_timing: frame.timing } : {}),
      ...(pending && !pending.restored ? {
        admission_ms: Math.max(0, pending.dispatchedAt - pending.receivedAt),
        roundtrip_ms: Math.max(0, resultAt - pending.dispatchedAt),
        settlement_ms: Math.max(0, performance.now() - resultAt),
        ...(frame.timing ? { transit_return_overhead_ms: resultAt - pending.dispatchedAt - frame.timing.host_elapsed_ms } : {}),
      } : {}),
    });
    if (pending && !pending.restored && this.#onCallTiming) {
      try {
        this.#onCallTiming({ session_id: row.session_id, source_call_id: row.source_call_id,
          transport_call_id: row.call_id, admission_ms: pending.dispatchedAt - pending.receivedAt,
          roundtrip_ms: resultAt - pending.dispatchedAt, settlement_ms: performance.now() - resultAt });
      } catch { /* Diagnostics must never change a durable call outcome. */ }
    }
  }

  #ackResult(socket: HostedToolsSocket, frame: Extract<HostedToolsHostFrame, { type: "result" }>): void {
    let row: HostedToolsCallRow | undefined;
    try { row = this.#persistence.call(frame.call_id); } catch { /* Passive correlation read. */ }
    if (row) this.#observe("ack_attempt", row);
    try {
      this.#send(socket, {
        type: "ack",
        call_id: frame.call_id,
      });
      if (row) this.#observe("ack_sent", row);
    } catch {
      if (row) this.#observe("ack_failed", row, { reason_code: "ack_send_failed" });
      this.#retire(socket, "result acknowledgement delivery failed", "ack_send_failed");
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
      // Process IDs belong to the executor runtime, not its WebSocket lease.
      // Keep commands/CUA generation-pinned and fence legacy hosts on reconnect.
      routeToken: JSON.stringify(binding.machine && binding.wireName === "write_stdin" && binding.runtimeId
        ? [binding.routeId, "process-runtime", binding.runtimeId, binding.wireName]
        : [binding.routeId, binding.generation, binding.leaseId, binding.wireName]),
      ...(binding.connectGrantId === undefined ? {} : { connectGrantId: binding.connectGrantId }),
      ...(binding.appToolCatalogDigest === undefined
        ? {}
        : { appToolCatalogDigest: binding.appToolCatalogDigest }),
      canonicalName: binding.wireName,
      providerDefinition: binding.providerDefinition,
      ...(binding.machine === undefined ? {} : { machine: binding.machine }),
      entry: binding.entry,
      invoke: (request: HostedToolsInvokeRequest) => this.#invoke(binding, request),
    });
  }

  #codeTool(name: string, prepared: HostedToolsPreparedTool): HostedToolsCodeTool {
    return Object.freeze({
      name,
      parallelSafe: prepared.entry.parallel_safe,
      definition: { ...prepared.providerDefinition, defer_loading: true as const },
      routeToken: prepared.routeToken,
      ...(prepared.canonicalName === "write_stdin" ? { processSessionKey: prepared.routeToken } : {}),
      provider: prepared.entry.provider,
      remoteName: prepared.entry.remote_name,
      summary: prepared.entry.summary,
      timeoutMs: prepared.entry.timeout_ms,
      handler: async (
        input: unknown,
        context: HostedToolsInvocationContext,
      ) => {
        if (!this.#entryAllowed(
          prepared.entry,
          prepared.connectGrantId,
          prepared.appToolCatalogDigest,
          context,
        )) {
          return toolResult("Hosted tool is outside the active grant", {
            status: "unavailable",
            message: "Hosted tool is outside the active grant",
          }, false, null);
        }
        const outcome = await prepared.invoke({
          sessionId: context.sessionId,
          ...(context.threadId === undefined ? {} : { threadId: context.threadId }),
          callId: context.callId,
          ...(context.turnId === undefined ? {} : { turnId: context.turnId }),
          model: context.model ?? "unknown",
          input: input as Record<string, unknown> | string,
          // This adapter generates the budget; replay must retain the admitted
          // default across releases. Explicit invoke budgets and input remain
          // subject to the strict immutable-call comparison below.
          outputTokenBudget: this.#persistence.callBySource(context.sessionId, context.callId)
            ?.output_token_budget ?? Number.MAX_SAFE_INTEGER,
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

  async #invoke(
    binding: HostedToolsCatalogBinding,
    request: HostedToolsInvokeRequest,
  ): Promise<HostedToolsInvocationOutcome> {
    const started = performance.now();
    const identity = {
      session_id: request.sessionId, source_call_id: request.callId,
      thread_id: request.threadId, name: binding.wireName,
      lease_id: binding.leaseId, generation: binding.generation,
      connection_id: binding.connectionId,
      host_connection_id: binding.hostConnectionId, host_runtime_id: binding.runtimeId,
      hand_id: binding.machine?.id,
    };
    this.#observe("received", identity);
    try {
      const socket = this.#socketForState(this.#persistence.state(binding.routeId));
      if (socket && this.#attachment(socket)?.renewalToken) {
        try { await this.#validateLeasedAttachment(socket); }
        catch (error) {
          this.#fence(socket, errorMessage(error),
            error instanceof HostedToolsProtocolError && error.code === "route_revoked" ? 1008 : 1011,
            error instanceof HostedToolsProtocolError && error.code === "route_revoked" ? "route_revoked" : "lease_validation_unavailable");
          return hostedToolsUnavailable("Hosted Tools authorization could not be validated before dispatch");
        }
      }
      const outcome = await this.#invokeCall(binding, request);
      try {
        const row = this.#persistence.callBySource(request.sessionId, request.callId);
        this.#observe("terminal", { ...identity, ...row }, {
          outcome: outcome.status, duration_ms: Math.max(0, performance.now() - started),
          ...(outcome.status === "completed" ? { success: outcome.output.success } : {}),
        });
      } catch { /* A diagnostic ledger read must not change a settled outcome. */ }
      return outcome;
    } catch (error) {
      this.#observe("terminal", identity, { outcome: "failed", duration_ms: Math.max(0, performance.now() - started) });
      throw error;
    }
  }

  #observe(
    stage: HostedToolsCallObservation["stage"],
    row: Pick<HostedToolsCallRow, "name" | "session_id" | "source_call_id">
      & { [K in "thread_id" | "call_id" | "lease_id" | "generation" | "connection_id" | "host_connection_id" | "host_runtime_id" | "hand_id"]?: HostedToolsCallRow[K] | undefined },
    fields: Omit<HostedToolsCallObservation, "stage" | "tool" | "session_id" | "source_call_id" | "thread_id" | "transport_call_id"> = {},
  ): void {
    if (!this.#onCallObservation) return;
    try {
      const socket = (row.connection_id && row.hand_id) || !row.lease_id || row.generation === undefined ? undefined
        : this.context.sockets().find(candidate => {
          const attachment = this.#attachment(candidate);
          return attachment !== undefined && attachment.leaseId === row.lease_id && attachment.generation === row.generation;
        });
      const attachment = socket ? this.#attachment(socket) : undefined;
      const connectionId = row.connection_id ?? attachment?.connectionId;
      const handId = row.hand_id ?? attachment?.machines?.[0]?.id ?? attachment?.expectedAttachmentId;
      this.#onCallObservation(Object.freeze({
        ...fields, stage,
        tool: MACHINE_TOOL_NAMES.has(row.name) ? row.name : "other",
        ...(row.generation === undefined ? {} : { connection_generation: row.generation, runtime_generation: row.generation }),
        ...safeObservationIds({ session_id: row.session_id, source_call_id: row.source_call_id,
          transport_call_id: row.call_id, thread_id: row.thread_id, lease_id: row.lease_id,
          connection_id: connectionId,
          hand_id: handId,
          host_connection_id: row.host_connection_id, host_runtime_id: row.host_runtime_id }),
      }));
    } catch { /* Passive diagnostics cannot alter admission or settlement. */ }
  }

  #observeConnection(
    stage: HostedToolsConnectionObservation["stage"],
    attachment?: HostedToolsSocketAttachment,
    fields: Pick<HostedToolsConnectionObservation, "reason_code" | "close_code"> = {},
    retainedState?: HostedToolsStateRow,
  ): void {
    if (!this.#onConnectionObservation) return;
    try {
      this.#onConnectionObservation(this.#connectionObservation(stage, attachment, fields, retainedState));
    } catch { /* Connection diagnostics cannot alter lease ownership. */ }
  }

  #connectionObservation(
    stage: HostedToolsConnectionObservation["stage"],
    attachment?: HostedToolsSocketAttachment,
    fields: Pick<HostedToolsConnectionObservation, "reason_code" | "close_code" | "active" | "connected"> = {},
    retainedState?: HostedToolsStateRow,
  ): HostedToolsConnectionObservation {
    const state = retainedState ?? (attachment?.routeId === undefined ? undefined : this.#persistence.state(attachment.routeId));
    const leaseId = attachment?.leaseId ?? state?.lease_id;
    const generation = attachment?.generation ?? state?.generation;
    const exactState = state && state.lease_id === leaseId && state.generation === generation ? state : undefined;
    const leaseExpiresAt = exactState?.lease_expires_at ?? attachment?.lease_expires_at;
    let handId = attachment?.machines?.[0]?.id ?? attachment?.expectedAttachmentId;
    if (handId === undefined && exactState?.machines_json) {
      const machines: unknown = JSON.parse(exactState.machines_json);
      const machine = Array.isArray(machines) ? objectRecord(machines[0]) : undefined;
      if (typeof machine?.id === "string") handId = machine.id;
    }
    const pendingCount = leaseId && generation !== undefined ? this.#persistence.activeCallCount(leaseId, generation) : 0;
    return Object.freeze({
      ...fields, stage,
      ...safeObservationIds({ connection_id: attachment?.connectionId, lease_id: leaseId,
        host_connection_id: attachment?.hostConnectionId, host_runtime_id: attachment?.runtimeId ?? exactState?.runtime_id, hand_id: handId }),
      ...(generation === undefined ? {} : { connection_generation: generation, runtime_generation: generation }),
      ...(leaseExpiresAt === undefined ? {} : { lease_expires_at: leaseExpiresAt }),
      ...(attachment?.last_heartbeat_at === undefined ? {} : { last_heartbeat_at: attachment.last_heartbeat_at,
        heartbeat_age_ms: Math.max(0, this.#now() - attachment.last_heartbeat_at) }),
      heartbeat_count: attachment?.heartbeat_count ?? 0, pending_call_count: pendingCount,
    });
  }

  #observeGenerationCalls(leaseId: string, generation: number, reasonCode: HostedToolsDiagnosticReason,
    stage: "transport_lost" | "connection_draining" = "transport_lost"): void {
    if (!this.#onCallObservation) return;
    try {
      const rows = new Map<string, HostedToolsCallRow>();
      for (const row of this.#persistence.generationCalls?.(leaseId, generation) ?? []) {
        if (row.lease_id === leaseId && row.generation === generation && row.state === "dispatched") rows.set(row.call_id, row);
      }
      for (const [callId, pending] of this.#pending) {
        if (pending.leaseId !== leaseId || pending.generation !== generation) continue;
        const row = this.#persistence.call(callId);
        if (row?.state === "dispatched") rows.set(callId, { ...row, thread_id: row.thread_id ?? pending.threadId ?? null });
      }
      for (const row of rows.values()) this.#observe(stage, row, { reason_code: reasonCode,
        ...(stage === "transport_lost" ? { outcome: "ambiguous" as const } : {}) });
    } catch { /* Diagnostic enumeration cannot change generation retirement. */ }
  }

  async #invokeCall(
    binding: HostedToolsCatalogBinding,
    request: HostedToolsInvokeRequest,
  ): Promise<HostedToolsInvocationOutcome> {
    const receivedAt = performance.now();
    const rebindProcess = () => {
      if (!binding.machine || binding.wireName !== "write_stdin" || !binding.runtimeId) return;
      // Rebind only the transport of the exact process-owning runtime. A new
      // runtime can reuse numeric process IDs and must never receive this poll
      // or stdin. Already-admitted calls still resolve through their ledger.
      const current = this.#catalogBindings().find(candidate => candidate.routeId === binding.routeId
        && candidate.machine?.id === binding.machine!.id && candidate.wireName === binding.wireName
        && candidate.runtimeId === binding.runtimeId);
      if (current) binding = current;
    };
    rebindProcess();
    let retained = this.#persistence.callBySource(request.sessionId, request.callId);
    if (!retained && !this.#routingSocketForState(this.#persistence.state(binding.routeId))) {
      // A never-admitted call may wait (bounded) for the same runtime epoch to
      // resume after transient transport loss. Replacement or retirement ends
      // the wait at once; the caller then refreshes with ledger evidence.
      await this.#awaitRouteResume(binding, request);
      rebindProcess();
      retained = this.#persistence.callBySource(request.sessionId, request.callId);
    }
    const diagnosticIdentity = {
      name: binding.wireName, session_id: request.sessionId, source_call_id: request.callId, thread_id: request.threadId,
      lease_id: retained?.lease_id ?? binding.leaseId, generation: retained?.generation ?? binding.generation,
      connection_id: retained?.connection_id ?? binding.connectionId,
      host_connection_id: retained?.host_connection_id ?? binding.hostConnectionId,
      host_runtime_id: retained?.host_runtime_id ?? binding.runtimeId,
      hand_id: retained?.hand_id ?? binding.machine?.id,
    };
    if (!retained && !this.#routingSocketForState(this.#persistence.state(binding.routeId))) {
      this.#observe("admission_failed", diagnosticIdentity, { reason_code: "attachment_unavailable", outcome: "unavailable" });
      return preAdmissionUnavailable("Hosted machine is reconnecting");
    }

    const leaseId = binding.leaseId;
    const now = this.#now();
    const deadlineAt = retained
      ? retained.deadline_at
      : Math.min(
        request.deadlineAt ?? Number.MAX_SAFE_INTEGER,
        Math.min(Number.MAX_SAFE_INTEGER, now + binding.entry.timeout_ms),
      );
    const outputByteBudget = request.outputByteBudget ?? Number.MAX_SAFE_INTEGER;
    const transportCallId = retained?.call_id ?? this.#randomUUID();
    const hostId = retained?.host_id ?? binding.hostId;
    const pinnedLeaseId = retained?.lease_id ?? binding.leaseId;
    const generation = retained?.generation ?? binding.generation;
    let call: Extract<HostedToolsManagedFrame, { type: "call" }>;
    try {
      call = parseHostedToolsManagedFrame(JSON.stringify({
        type: "call",
        session_id: request.sessionId,
        ...(request.turnId === undefined ? {} : { turn_id: request.turnId }),
        call_id: transportCallId,
        model: request.model,
        name: binding.wireName,
        input: request.input,
        output_token_budget: request.outputTokenBudget,
        output_byte_budget: outputByteBudget,
        deadline_at: deadlineAt,
      })) as Extract<HostedToolsManagedFrame, { type: "call" }>;
    } catch (error) {
      this.#observe("admission_failed", { ...diagnosticIdentity, call_id: transportCallId }, { reason_code: "invalid_call", outcome: "unavailable" });
      return Promise.resolve(hostedToolsUnavailable(`Hosted Tools call was invalid before dispatch: ${errorMessage(error)}`));
    }
    const inputJson = JSON.stringify(call.input);
    const proposed: HostedToolsCallRow = {
      call_id: call.call_id,
      session_id: call.session_id,
      source_call_id: request.callId,
      ...(request.threadId === undefined ? {} : { thread_id: request.threadId }),
      turn_id: call.turn_id ?? null,
      host_id: hostId,
      lease_id: pinnedLeaseId,
      generation,
      connection_id: retained?.connection_id ?? binding.connectionId ?? null,
      host_connection_id: retained?.host_connection_id ?? binding.hostConnectionId ?? null,
      host_runtime_id: retained?.host_runtime_id ?? binding.runtimeId ?? null,
      hand_id: retained?.hand_id ?? safeObservationIds({ hand_id: binding.machine?.id }).hand_id ?? null,
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
    if (retained) return this.#repeatedCall(retained, proposed, binding, request.signal);
    const existing = this.#persistence.call(call.call_id);
    if (existing) return this.#repeatedCall(existing, proposed, binding, request.signal);
    if (!this.#attachmentIsPresent(binding, now)) {
      this.#observe("admission_failed", proposed, { reason_code: "attachment_unavailable", outcome: "unavailable" });
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
      if (socket) this.#fence(socket, "Hosted Tools generation exhausted its durable call ledger", 1008, "generation_limit");
      else if (state?.lease_id === leaseId && state.generation === binding.generation) {
        this.#retireState(state, "Hosted Tools generation exhausted its durable call ledger", "generation_limit");
      }
      this.#observe("admission_failed", proposed, { reason_code: "generation_limit", outcome: "unavailable" });
      return Promise.resolve(hostedToolsUnavailable("Hosted Tools generation reached its durable call limit"));
    }
    try {
      this.#persistence.insertCall(proposed, now);
    } catch {
      const recovered = this.#persistence.callBySource(request.sessionId, request.callId)
        ?? this.#persistence.call(call.call_id);
      if (recovered) return this.#repeatedCall(recovered, proposed, binding, request.signal);
      this.#observe("admission_failed", proposed, { reason_code: "admission_uncertain", outcome: "ambiguous" });
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools admission may have persisted; replay is unsafe"));
    }
    this.#observe("admitted", proposed);
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
        deadlineAt <= dispatchNow ? "call_deadline" : current && current.lease_expires_at <= dispatchNow ? "lease_expired" : "attachment_unavailable",
      ));
    }
    if (this.#maxInFlight !== undefined
      && this.#persistence.activeCallCount(leaseId, binding.generation) > this.#maxInFlight) {
      return Promise.resolve(this.#finishBeforeDispatch(
        proposed,
        "unavailable",
        hostedToolsUnavailable("Hosted Tools host is at its bounded in-flight limit"),
        "in_flight_limit",
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
      this.#observe("admission_failed", proposed, { reason_code: "dispatch_ownership_lost", outcome: "ambiguous" });
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools call lost durable dispatch ownership"));
    }
    let resolve!: (outcome: HostedToolCallOutcome) => void;
    const promise = new Promise<HostedToolCallOutcome>((completed) => { resolve = completed; });
    const pending: PendingCall = {
      ...(request.threadId === undefined ? {} : { threadId: request.threadId }),
      receivedAt,
      dispatchedAt: performance.now(),
      leaseId,
      generation: binding.generation,
      deadlineAt,
      promise,
      resolve,
    };
    this.#pending.set(call.call_id, pending);
    this.#observe("dispatched", proposed, { admission_ms: Math.max(0, pending.dispatchedAt - receivedAt) });
    this.#attachAbort(call.call_id, pending, request.signal);
    this.#armExpiry(call.call_id, pending);
    try {
      this.#observe("send_started", proposed);
      this.#send(socket, call);
      if (call.turn_id !== undefined && this.#attachment(socket)?.turnLifecycle) {
        const key = JSON.stringify([call.session_id, call.turn_id]);
        let sockets = this.#turnSockets.get(key);
        if (!sockets) { sockets = new Set(); this.#turnSockets.set(key, sockets); }
        sockets.add(socket);
      }
      this.#observe("sent", proposed);
    } catch {
      this.#observe("send_failed", proposed, { reason_code: "call_send_failed" });
      this.#retire(socket, "call delivery failed", "call_send_failed");
      closeSocket(socket, 1011, "Hosted Tools call delivery failed");
    }
    return promise;
  }

  async #awaitRouteResume(binding: HostedToolsCatalogBinding, request: HostedToolsInvokeRequest): Promise<void> {
    if (this.#reconnectAdmissionWaitMs <= 0 || request.signal?.aborted) return;
    const exact = (state: HostedToolsStateRow | undefined) => state !== undefined && state.host_id === binding.hostId
      && state.lease_id === binding.leaseId && state.generation === binding.generation;
    const initial = this.#persistence.state(binding.routeId);
    if (!exact(initial) || !this.#canRecover(initial!)) return;
    const startedAt = this.#now();
    const until = Math.min(startedAt + this.#reconnectAdmissionWaitMs, request.deadlineAt ?? Number.MAX_SAFE_INTEGER);
    const signal = request.signal;
    while (this.#now() < until && !signal?.aborted) {
      await new Promise<void>(resolve => {
        const done = () => { clearTimeout(timer); signal?.removeEventListener("abort", done); resolve(); };
        const timer = setTimeout(done, Math.max(1, Math.min(50, until - this.#now())));
        signal?.addEventListener("abort", done, { once: true });
      });
      const state = this.#persistence.state(binding.routeId);
      if (!exact(state)) return;
      if (this.#routingSocketForState(state)) return;
    }
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

  #attachAbort(callId: string, pending: PendingCall, signal?: AbortSignal): void {
    if (!signal) return;
    if (signal.aborted) { this.cancel(callId); return; }
    const cancel = () => { this.cancel(callId); };
    signal.addEventListener("abort", cancel, { once: true });
    const removePrevious = pending.removeAbort;
    pending.removeAbort = () => { removePrevious?.(); signal.removeEventListener("abort", cancel); };
  }

  async #repeatedCall(existing: HostedToolsCallRow, proposed: HostedToolsCallRow, binding: HostedToolsCatalogBinding, signal?: AbortSignal): Promise<HostedToolsInvocationOutcome> {
    const matches = sameImmutableCall(existing, proposed);
    this.#observe("replay", { ...existing, thread_id: existing.thread_id ?? proposed.thread_id },
      matches ? {} : { reason_code: "call_conflict", conflict_fields: immutableCallDifferences(existing, proposed) });
    if (!matches) {
      const state = this.#stateForLease(existing.lease_id, existing.generation);
      const socket = this.#socketForState(state);
      if (socket) this.#fence(socket, "call ID was reused with different immutable fields", 1008, "call_conflict");
      return Promise.resolve(hostedToolsAmbiguous("Hosted Tools call ID conflicts with retained durable state"));
    }
    const owner = this.#stateForLease(existing.lease_id, existing.generation);
    const pending = this.#pending.get(existing.call_id)
      ?? (existing.state === "dispatched" && owner && this.#canRecover(owner)
        && owner.runtime_id === existing.host_runtime_id ? this.#restorePending(existing) : undefined);
    if (pending && existing.state === "dispatched") this.#attachAbort(existing.call_id, pending, signal);
    const outcome = existing.result_json
      ? JSON.parse(existing.result_json) as HostedToolCallOutcome
      : existing.state === "dispatched" && pending
        ? await pending.promise
        : existing.state === "admitted"
          ? hostedToolsUnavailable("Hosted Tools call was admitted but never dispatched")
          : hostedToolsAmbiguous("Hosted Tools call has no retained terminal receipt");
    if (binding.machine && binding.wireName === "exec_command"
      && (existing.lease_id !== binding.leaseId || existing.generation !== binding.generation)
      && outcome.status === "completed"
      && outcome.output.structured_result !== null
      && typeof outcome.output.structured_result === "object"
      && "session_id" in outcome.output.structured_result) {
      // Old receipts retain their transport lease, not their process runtime.
      // Never attach an old numeric process ID to the caller's refreshed route.
      // Already-bound sessions use their original runtime token independently.
      return hostedToolsAmbiguous("The retained command receipt belongs to an earlier Hand connection and cannot prove process ownership. Use its original saved process session if available. The command was not resent.");
    }
    return outcome;
  }

  #finishBeforeDispatch(
    row: HostedToolsCallRow,
    state: "unavailable" | "cancelled",
    outcome: HostedToolCallOutcome,
    reasonCode: HostedToolsDiagnosticReason = state === "cancelled" ? "cancelled_before_dispatch" : "attachment_unavailable",
  ): HostedToolCallOutcome {
    this.#persistence.transitionCall(row.call_id, ["admitted"], state, JSON.stringify(outcome), this.#now());
    this.#observe("admission_failed", row, { outcome: outcome.status, reason_code: reasonCode });
    return outcome;
  }

  async #renewLeaseAuthority(renewal: HostedToolsLeasedAttachmentRenewal): Promise<number | undefined> {
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        this.#renewLeasedAttachment?.(renewal),
        new Promise<never>((_resolve, reject) => {
          timeout = setTimeout(() => reject(new Error("Hosted Tools authorization validation timed out")), 5_000);
        }),
      ]);
    } finally { if (timeout !== undefined) clearTimeout(timeout); }
  }

  async #checkCachedLease(socket: HostedToolsSocket): Promise<void> {
    try { await this.#validateLeasedAttachment(socket); }
    catch (error) {
      // A successor socket may have taken over while the authority was queried.
      if (error instanceof HostedToolsProtocolError && error.code === "stale_socket") return;
      this.#fence(socket, errorMessage(error),
        error instanceof HostedToolsProtocolError && error.code === "route_revoked" ? 1008 : 1011,
        error instanceof HostedToolsProtocolError && error.code === "route_revoked" ? "route_revoked" : "lease_validation_unavailable");
    }
  }

  #armLeaseValidation(state: HostedToolsStateRow): void {
    if (!state.lease_id) return;
    const existing = this.#leaseValidationTimers.get(state.lease_id);
    if (existing !== undefined) clearTimeout(existing);
    this.#leaseValidationTimers.delete(state.lease_id);
    if (![...this.#pending.values()].some(pending => pending.leaseId === state.lease_id && pending.generation === state.generation)) return;
    const socket = this.#socketForState(state);
    if (!socket || !this.#attachment(socket)?.renewalToken) return;
    const leaseId = state.lease_id;
    this.#leaseValidationTimers.set(leaseId, setTimeout(() => {
      this.#leaseValidationTimers.delete(leaseId);
      const current = this.#stateForLease(leaseId, state.generation);
      if (!current || this.#socketForState(current) !== socket) return;
      if (current.lease_expires_at > this.#now()) { this.#armLeaseValidation(current); return; }
      void this.#checkCachedLease(socket);
    }, Math.min(2_147_483_647, Math.max(1, state.lease_expires_at - this.#now()))));
  }

  #armExpiry(callId: string, pending: PendingCall): void {
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    // The admitted deadline has its own timer: authority I/O must never delay it.
    pending.timeout = setTimeout(() => {
      if (this.#pending.get(callId) !== pending) return;
      if (pending.deadlineAt > this.#now()) { this.#armExpiry(callId, pending); return; }
      const row = this.#persistence.call(callId);
      if (row) {
        this.cancel(callId);
        this.#finishAmbiguous(row, "Hosted Tools call deadline expired after dispatch", "call_deadline");
      }
    }, Math.min(2_147_483_647, Math.max(1, pending.deadlineAt - this.#now())));
    const state = this.#stateForLease(pending.leaseId, pending.generation);
    if (state) this.#armLeaseValidation(state);
  }

  #finishAmbiguous(row: HostedToolsCallRow, message: string, reasonCode: HostedToolsDiagnosticReason = "attachment_unavailable"): void {
    const outcome = hostedToolsAmbiguous(message);
    this.#observe("transport_lost", row, { reason_code: reasonCode, outcome: "ambiguous" });
    this.#persistence.transitionCall(
      row.call_id,
      ["dispatched"],
      "ambiguous",
      JSON.stringify(outcome),
      this.#now(),
    );
    this.#takePending(row.call_id)?.resolve(outcome);
  }

  #retire(socket: HostedToolsSocket, reason: string, reasonCode: HostedToolsDiagnosticReason = "transport_closed"): void {
    for (const [key, sockets] of this.#turnSockets) {
      sockets.delete(socket);
      if (!sockets.size) this.#turnSockets.delete(key);
    }
    const attachment = this.#attachment(socket);
    if (reasonCode === "call_send_failed" || reasonCode === "cancel_send_failed"
      || reasonCode === "ack_send_failed" || reasonCode === "lease_validation_unavailable") {
      this.#observeConnection("error", attachment, { reason_code: reasonCode, close_code: 1011 });
    }
    if (!attachment?.active || !attachment.leaseId || attachment.generation === undefined) return;
    const state = attachment.routeId === undefined
      ? undefined
      : this.#persistence.state(attachment.routeId);
    this.context.writeAttachment(socket, { ...attachment, active: false });
    if (state && this.#canRecover(state) && reasonCode === "lease_validation_unavailable" && !attachment.draining
      && state.lease_id === attachment.leaseId && state.generation === attachment.generation) {
      // Unavailable authority is not proof of revocation. Settle current waiters
      // finitely, but retain the epoch so later validated reconnects can ACK
      // journal receipts without issuing the command again.
      const outcome = hostedToolsAmbiguous("Hosted Tools authorization could not be validated after dispatch");
      this.#persistence.markGenerationAmbiguous(state.lease_id!, state.generation, JSON.stringify(outcome), this.#now());
      this.#resolveGeneration(state.lease_id!, state.generation, outcome);
      this.#notifyCatalogChanged();
      return;
    }
    const transient = ["transport_closed", "transport_error", "call_send_failed", "cancel_send_failed", "ack_send_failed"].includes(reasonCode);
    if (state && this.#canRecover(state) && transient && !attachment.draining
      && state.lease_id === attachment.leaseId && state.generation === attachment.generation) {
      for (const row of this.#persistence.generationCalls!(state.lease_id!, state.generation)) {
        if (row.state === "dispatched") this.#observe("transport_lost", row, { reason_code: reasonCode });
      }
      this.#notifyCatalogChanged();
      return;
    }
    if (state) this.#retireState(state, reason, reasonCode, attachment.leaseId, attachment.generation);
  }

  #retireState(
    state: HostedToolsStateRow,
    reason: string,
    reasonCode: HostedToolsDiagnosticReason = "transport_closed",
    leaseId = state.lease_id ?? undefined,
    generation = state.generation,
  ): void {
    if (!leaseId) return;
    this.#observeGenerationCalls(leaseId, generation, reasonCode);
    const socket = this.context.sockets().find(candidate => {
      const attachment = this.#attachment(candidate);
      return attachment?.leaseId === leaseId && attachment.generation === generation;
    });
    if (reasonCode === "lease_expired") {
      this.#observeConnection("lease_expired", socket ? this.#attachment(socket) : undefined, { reason_code: reasonCode }, state);
    } else if (!socket) {
      this.#observeConnection("fenced", undefined, { reason_code: reasonCode }, { ...state, lease_id: leaseId, generation });
    }
    const outcome = hostedToolsAmbiguous(`Hosted Tools outcome is ambiguous after transport loss: ${reason}`);
    this.#persistence.transaction(() => {
      this.#persistence.markGenerationAmbiguous(leaseId, generation, JSON.stringify(outcome), this.#now());
      this.#persistence.clearHost(leaseId, generation);
    });
    this.#resolveGeneration(leaseId, generation, outcome);
    this.#notifyCatalogChanged();
  }

  #resolveGeneration(leaseId: string, generation: number, outcome: HostedToolCallOutcome): void {
    for (const [callId, pending] of this.#pending) {
      if (pending.leaseId !== leaseId || pending.generation !== generation) continue;
      this.#takePending(callId)?.resolve(outcome);
    }
  }

  #takePending(callId: string): PendingCall | undefined {
    const pending = this.#pending.get(callId);
    if (!pending) return undefined;
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    pending.removeAbort?.();
    this.#pending.delete(callId);
    if (![...this.#pending.values()].some(call => call.leaseId === pending.leaseId && call.generation === pending.generation)) {
      const timer = this.#leaseValidationTimers.get(pending.leaseId);
      if (timer !== undefined) clearTimeout(timer);
      this.#leaseValidationTimers.delete(pending.leaseId);
    }
    return pending;
  }

  #fence(socket: HostedToolsSocket, reason: string, code = 1008, reasonCode: HostedToolsDiagnosticReason = "protocol_error"): void {
    const attachment = this.#attachment(socket);
    this.#observeConnection("fenced", attachment, { reason_code: reasonCode, close_code: code });
    if (attachment?.leaseId && attachment.generation !== undefined) {
      this.#retire(socket, reason, reasonCode);
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
      if (socket && this.#attachment(socket)?.renewalToken) return this.#routingSocketForState(state);
      if (socket) this.#fence(socket, "Hosted Tools lease expired", 1012, "lease_expired");
      else if (!this.#canRecover(state)) this.#retireState(state, "Hosted Tools lease expired", "lease_expired");
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
      if (state.route_id === excludeRouteId || !state.catalog_json) continue;
      const socket = this.#liveRoutingSocketForState(state);
      const savedMachines = state.machines_json ? JSON.parse(state.machines_json) as HostedMachine[] : [];
      if (!socket && savedMachines.length === 0) continue;
      const attachment = socket ? this.#attachment(socket) : undefined;
      // The live lookup already checked exact lease/generation ownership.
      // A draining active socket still supplies its grant/digest, as before,
      // even though it is no longer eligible for new dispatch.
      const activeSocket = socket ?? this.#socketForState(state);
      const activeAttachment = attachment
        ?? (activeSocket === undefined ? undefined : this.#attachment(activeSocket));
      const connectGrantId = activeAttachment?.connectGrantId === undefined ? undefined
        : isConnectGrantId(activeAttachment.connectGrantId) ? activeAttachment.connectGrantId : INVALID_CONNECT_GRANT_ID;
      const appToolCatalogDigest = activeAttachment?.appToolCatalogDigest;
      let entries: HostedToolCatalogEntry[];
      try {
        entries = JSON.parse(state.catalog_json) as HostedToolCatalogEntry[];
      } catch {
        continue;
      }
      for (const entry of entries) {
        if (disabledBrowserTool(entry.definition.name)) continue;
        const machine = attachment?.machines?.[0] ?? savedMachines[0];
        bindings.push(Object.freeze({
          routeId: state.route_id,
          hostId: state.host_id ?? "offline",
          leaseId: state.lease_id ?? "offline",
          generation: state.generation,
          wireName: entry.definition.name,
          ...(attachment?.connectionId === undefined ? {} : { connectionId: attachment.connectionId }),
          ...((attachment?.runtimeId ?? state.runtime_id) == null ? {} : { runtimeId: attachment?.runtimeId ?? state.runtime_id! }),
          ...(attachment?.hostConnectionId === undefined ? {} : { hostConnectionId: attachment.hostConnectionId }),
          providerDefinition: entry.definition,
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
      const savedMachines = state.machines_json ? JSON.parse(state.machines_json) as HostedMachine[] : [];
      if (!socket && savedMachines.length === 0) continue;
      const attachment = socket ? this.#attachment(socket) : undefined;
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
    try { socket.send(JSON.stringify(frame)); }
    catch (error) {
      const code = frame.type === "ready" ? "ready_send_failed"
        : frame.type === "draining" ? "drain_send_failed" : undefined;
      if (code) throw new HostedToolsProtocolError(code, "Hosted Tools control frame delivery failed");
      throw error;
    }
  }

  #notifyCatalogChanged(): void {
    this.#onCatalogChanged?.(this.#definitions());
  }
}

function safeObservationIds(values: Record<string, string | null | undefined>): Record<string, string> {
  return Object.fromEntries(Object.entries(values).filter((entry): entry is [string, string] =>
    typeof entry[1] === "string" && /^[A-Za-z0-9][A-Za-z0-9_.:/-]{0,127}$/.test(entry[1])));
}

function emptyState(routeId: string): HostedToolsStateRow {
  return {
    route_id: routeId,
    generation: 0,
    host_id: null,
    lease_id: null,
    lease_expires_at: 0,
    catalog_json: null,
    machines_json: null,
  };
}

function scopedRouteId(connectGrantId: string | undefined, attachmentId: string | undefined): string {
  return `${connectGrantId === undefined ? "user" : "connect"}:${attachmentId ?? LEGACY_ROUTE_ID}`;
}

function reservedMachineEntry(
  entry: HostedToolCatalogEntry,
  machine: HostedMachine | undefined,
): boolean {
  return machine !== undefined && MACHINE_TOOL_NAMES.has(entry.definition.name)
    && !entry.definition.name.startsWith("mcp__cua_repl__");
}

function reservedMachineBinding(binding: HostedToolsCatalogBinding): boolean {
  return binding.machine !== undefined && MACHINE_TOOL_NAMES.has(binding.wireName)
    && !binding.wireName.startsWith("mcp__cua_repl__");
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
    description: entry.definition.name.startsWith("mcp__cua_repl__")
      ? entry.definition.description
      : `Routes to ${routeName}. ${entry.definition.description}`,
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

function canonicalJson(value: unknown): string {
  return JSON.stringify(value, (_key, item: unknown) => {
    if (item !== null && typeof item === "object" && !Array.isArray(item)) {
      return Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b)));
    }
    return item;
  });
}

const IMMUTABLE_CALL_FIELDS = [
  "call_id", "session_id", "source_call_id", "turn_id", "host_id", "lease_id",
  "generation", "model", "name", "input_json", "output_token_budget",
  "output_byte_budget", "deadline_at",
] as const satisfies readonly (keyof HostedToolsCallRow)[];
type ImmutableCallField = (typeof IMMUTABLE_CALL_FIELDS)[number];

function immutableCallDifferences(left: HostedToolsCallRow, right: HostedToolsCallRow): readonly ImmutableCallField[] {
  return Object.freeze(IMMUTABLE_CALL_FIELDS.filter(field => field === "turn_id"
    ? (left.turn_id ?? null) !== (right.turn_id ?? null)
    : left[field] !== right[field]));
}

function sameImmutableCall(left: HostedToolsCallRow, right: HostedToolsCallRow): boolean {
  return left.call_id === right.call_id
    && left.session_id === right.session_id
    && left.source_call_id === right.source_call_id
    && (left.turn_id ?? null) === (right.turn_id ?? null)
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
