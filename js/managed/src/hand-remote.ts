import { recordingAvailable, validRecordingCapability, screenAction, screenResult, screenResultMatches, screenResultShape, screenTool, type AgentScreenResult, type ScreenResultShape, type ScreenTarget } from "./hand-remote-agent";

/** Native human media/input use WebRTC. Cloudflare sandboxes explicitly use scoped HTTPS frames.
 * Bounded agent screenshots are independent of the human media transport. */
const TAG = "hand-remote";
const MAX_CONNECTIONS = 64;
const LEASE_MS = 30_000;
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const noStore = { "cache-control": "no-store" };
export const REMOTE_VM_ASSERTION = "x-nanocodex-remote-vm";
/** anySurface: an enrolled Hand device publishes its native surfaces, bound only to its machine. */
export type RemoteVMPublisher = { machineId: string; machineName?: string; routeId: string; expiresAt: number; surfaceKind?: "desktop"; anySurface?: true };

type Surface = { id: string; name: string; kind: "desktop" | "window" | "phone" | "vm"; width: number; height: number; controllable: boolean; agent_tools?: boolean; recording?: ScreenTarget["recording"]; recordingCapabilities?: Record<string, unknown>; broadcast?: boolean; playback?: boolean; transport?: "frames-v1"; frame_window?: number };
/** Authority decision for one complete host catalog, awaited before it becomes visible. */
export type HandRemoteCatalogClaim = Readonly<{ machineId: string; generation: string; connectionId: string; sequence: number; surfaces: readonly string[]; routeId?: string }>;
/** Host-originated HLS playback status. Never carries upload URLs or tokens. */
export type HandRemoteHostResult = Readonly<{ type: "broadcast_result"; target: "hls"; request_id: string; stream_id: string;
  status: "starting" | "live" | "reconnecting" | "stopped" | "failed"; error?: string; machine_id: string; generation: string }>;
export type HandRemoteHostCommand = Readonly<{ action: "start" | "stop" | "status"; request_id: string; stream_id: string;
  preset?: string; upload?: Readonly<{ url: string; token: string; expires_at: number }> }>;
export type HandRemoteHooks = Readonly<{
  onObservation?: (observation: HandRemoteObservation) => void;
  /** Resolve false to reject; throwing also rejects. Must not reenter this broker synchronously. */
  claimCatalog?: (claim: HandRemoteCatalogClaim) => Promise<boolean>;
  /** After a granted catalog replaced every older local host of its machine. */
  onClaimPublished?: (claim: HandRemoteCatalogClaim) => void;
  /** Durable, strictly increasing host-socket sequence for claim fencing. */
  nextSequence?: () => number;
  onHostResult?: (result: HandRemoteHostResult) => void;
  /** Late host result with no local pending call; the ledger decides whether it matches a retained identity. */
  onLateResult?: (result: HandRemoteLateResult) => void;
  /** Regional brokers prefix connection IDs and generations ("rs.<region>.") so Workers route without an owner hop. */
  idPrefix?: () => string;
}>;
const HLS_STATUSES = ["starting", "live", "reconnecting", "stopped", "failed"];
const HLS_ERRORS = ["invalid_request", "unsupported", "busy", "capture_failed", "encoder_failed", "upload_rejected", "expired", "broadcast_failed"];
type Attachment = {
  kind: typeof TAG; role: "host" | "viewer"; id: string; generation: string; expiresAt: number;
  machineId?: string; machineName?: string; surfaces?: Surface[]; hostId?: string; surfaceId?: string;
  rateWindow: number; rateCount: number;
  vm?: RemoteVMPublisher;
  transport?: "frames-v1";
  framePending?: boolean | number;
  frameWindow?: number;
  broadcastRequest?: string;
  renewalCount?: number;
  /** A complete catalog is awaiting owner authority; it is not yet visible. */
  claiming?: boolean;
  claimMachineId?: string;
  sequence?: number;
  /** Viewer-only bounded signal phase bits (offer, answer, host candidate, viewer candidate) and admission time. */
  signalSeen?: number;
  admittedAt?: number;
};
type Context = Pick<DurableObjectState, "acceptWebSocket" | "getWebSockets">;

/**
 * Durable receipt ledger for one exact screen call identity. admit() runs
 * synchronously in the same turn as the busy/stale/fence checks and before the
 * agent_call frame is sent: only "admitted" permits the send. settle() runs
 * synchronously inside finish(), before any response is built, so a result can
 * never escape without first being retained.
 */
export type HandRemoteCallLedger = Readonly<{
  admit(call: Readonly<{ requestId: string; connectionId: string; generation: string; target: ScreenTarget; deadlineAt: number }> & ScreenResultShape): "admitted" | "fenced" | "duplicate";
  settle(requestId: string, result: AgentScreenResult): void;
}>;
export type HandRemoteCallContext = Readonly<{ threadId?: string; callId?: string; turnId?: string; ledger?: HandRemoteCallLedger }>;
/** A host result for a request this broker instance no longer holds (for example after eviction). */
export type HandRemoteLateResult = Readonly<{ requestId: string; connectionId: string; generation: string; result: AgentScreenResult }>;
export type HandRemoteReasonCode = "connection_closed" | "websocket_closed" | "websocket_error"
  | "publisher_revoked" | "host_replaced" | "host_disconnected" | "viewer_closed" | "lease_expired"
  | "invalid_signaling" | "send_failed" | "stale_catalog" | "invalid_input" | "invalid_agent"
  | "host_unavailable" | "busy" | "not_controllable" | "aborted" | "timeout"
  | "result_ok" | "result_busy" | "result_invalid" | "result_unavailable" | "result_cancelled"
  | "retained_socket" | "claim_rejected" | "duplicate_call" | "call_fenced";
export type HandRemoteObservation = Readonly<{
  stage: "snapshot" | "connection.accepted" | "connection.ready" | "connection.published" | "connection.replaced"
    | "connection.renewed" | "connection.closed" | "connection.lease_expired" | "connection.fenced"
    | "connection.resumed" | "connection.transport_loss" | "call.received" | "call.admitted" | "call.send_started" | "call.sent"
    | "call.receipt" | "call.terminal" | "call.cancel" | "call.timeout" | "call.transport_loss"
    | "signal.offer_relayed" | "signal.answer_relayed" | "signal.first_candidate";
  /** Signal phases only: direction and broker-local elapsed time since viewer admission. Never SDP or candidates. */
  signal_direction?: "host_to_viewer" | "viewer_to_host"; signal_elapsed_ms?: number;
  connection_id?: string; host_connection_id?: string; remote_generation?: string; role?: "host" | "viewer";
  hand_id?: string; connected?: boolean; active?: boolean;
  lease_expires_at?: number; pending_calls: number; reason_code?: HandRemoteReasonCode;
  close_code?: number; renewal_count?: number; request_id?: string;
  runtime_session_id?: string; source_call_id?: string; parent_call_id?: string; thread_id?: string; turn_id?: string;
}>;
type CallObservation = Pick<HandRemoteObservation, "request_id" | "runtime_session_id" | "source_call_id" | "parent_call_id" | "thread_id" | "turn_id">;
type ObservationFields = Omit<Partial<HandRemoteObservation>, "stage" | "connection_id" | "host_connection_id"
  | "remote_generation" | "role" | "hand_id" | "connected" | "active" | "lease_expires_at" | "pending_calls">;
const REASON_CODES: ReadonlySet<string> = new Set<HandRemoteReasonCode>([
  "connection_closed", "websocket_closed", "websocket_error", "publisher_revoked", "host_replaced",
  "host_disconnected", "viewer_closed", "lease_expired", "invalid_signaling", "send_failed", "stale_catalog",
  "invalid_input", "invalid_agent", "host_unavailable", "busy", "not_controllable", "aborted", "timeout",
  "result_ok", "result_busy", "result_invalid", "result_unavailable", "result_cancelled", "retained_socket", "claim_rejected",
  "duplicate_call", "call_fenced",
]);

export class HandRemoteBroker {
  private readonly sendFailures = new WeakSet<object>();
  private readonly pending = new Map<string, { socket: WebSocket; expectsImage: boolean; recording: boolean; observation: CallObservation;
    finish(result: AgentScreenResult, reasonCode?: HandRemoteReasonCode): void }>();
  private readonly onObservation?: (observation: HandRemoteObservation) => void;
  private readonly hooks: HandRemoteHooks;
  /** Local claims are granted in socket arrival order; a later catalog never waits behind a fence of itself. */
  private claims: Promise<unknown> = Promise.resolve();
  constructor(private readonly context: Context, hooks?: HandRemoteHooks | ((observation: HandRemoteObservation) => void)) {
    this.hooks = typeof hooks === "function" ? { onObservation: hooks } : hooks ?? {};
    this.onObservation = this.hooks.onObservation;
    // Socket attachments survive hibernation; pending calls do not. Resumption
    // provides no evidence of whether an earlier call executed or completed.
    for (const socket of context.getWebSockets(TAG)) {
      const state = this.attachment(socket);
      if (state && state.expiresAt > 0) this.observe("connection.resumed", state, { reason_code: "retained_socket" });
      // A claim interrupted by eviction has an unknown grant: never publish it late.
      if (state?.claiming && state.expiresAt > 0) this.close(socket, "Host publication rejected", "claim_rejected");
    }
  }

  owns(socket: WebSocket): boolean { return this.attachment(socket) !== undefined; }

  /** Retained socket state, leases and local pending calls, without mutating either.
   * A valid lease is not evidence of host execution or network liveness. */
  connectionDiagnostics(): readonly HandRemoteObservation[] {
    return this.context.getWebSockets(TAG).flatMap(socket => {
      const state = this.attachment(socket);
      if (!state) return [];
      const snapshot = this.projectObservation("snapshot", state);
      const connected = socket.readyState === WebSocket.OPEN;
      return [{ ...snapshot, connected, active: connected && snapshot.active === true }];
    });
  }

  list(includeUnsupported = false): ScreenTarget[] {
    this.sweep();
    return this.hosts().flatMap(({ state }) => (state.surfaces ?? []).filter(surface => includeUnsupported || surface.transport !== "frames-v1"
      || (cloudflarePublisher(state) && cloudflareFrames(state.machineId!, surface.kind))).map(surface => ({
      ...surface, machine_id: state.machineId!, machine_name: state.machineName!, generation: state.generation,
    })));
  }

  tools() { return this.list(true).filter(target => target.agent_tools).map(screenTool); }

  revokePublisher(routeId: string): number {
    let closed = 0;
    for (const socket of this.context.getWebSockets(TAG)) {
      if (this.attachment(socket)?.vm?.routeId === routeId) { this.close(socket, "Hand revoked", "publisher_revoked"); closed++; }
    }
    return closed;
  }

  /** True while a scoped (server/VM) publisher, not an enrolled device, holds this machine. */
  scopedPublisher(machineId: string): boolean {
    return this.context.getWebSockets(TAG).some(socket => {
      const state = this.attachment(socket);
      return state?.role === "host" && state.expiresAt > Date.now() && state.vm?.machineId === machineId && !state.vm.anySurface;
    });
  }

  /** Close every host publication of a machine except `keepGeneration`, including
   * hosts still awaiting a claim. Their viewers are fenced with them. */
  fenceMachine(machineId: string, keepGeneration?: string, reasonCode: HandRemoteReasonCode = "host_replaced"): string[] {
    const fenced: string[] = [];
    for (const socket of this.context.getWebSockets(TAG)) {
      const state = this.attachment(socket);
      if (state?.role !== "host" || state.expiresAt <= 0 || state.generation === keepGeneration) continue;
      if (state.machineId !== machineId && state.claimMachineId !== machineId) continue;
      if (reasonCode === "host_replaced") this.observe("connection.replaced", state, { reason_code: "host_replaced" });
      this.close(socket, reasonCode === "publisher_revoked" ? "Hand revoked" : "Host replaced", reasonCode);
      fenced.push(state.generation);
    }
    return fenced;
  }

  /** Current visible publication of a machine, if any. */
  publication(machineId: string): { generation: string; connectionId: string } | undefined {
    this.sweep();
    const host = this.hosts().find(({ state }) => state.machineId === machineId);
    return host ? { generation: host.state.generation, connectionId: host.state.id } : undefined;
  }

  /** Broker-originated HLS command to the authenticated host socket. The upload
   * token travels only over this socket and is never retained in attachments. */
  sendHostCommand(machineId: string, surfaceId: string, generation: string | undefined, command: HandRemoteHostCommand): Response {
    this.sweep();
    const host = this.hosts().find(({ state }) => state.machineId === machineId && state.surfaces?.some(surface => surface.id === surfaceId));
    if (!host) return Response.json({ error: "not_found" }, { status: 404, headers: noStore });
    if (generation !== undefined && generation !== host.state.generation) return Response.json({ error: "stale_generation" }, { status: 409, headers: noStore });
    if (!host.state.surfaces!.find(surface => surface.id === surfaceId)!.playback) return Response.json({ error: "unsupported" }, { status: 409, headers: noStore });
    try {
      this.send(host.socket, { type: "broadcast", target: "hls", action: command.action, request_id: command.request_id, surface_id: surfaceId,
        stream_id: command.stream_id, ...(command.action === "start" ? { preset: command.preset, upload: command.upload } : {}) });
    } catch { return Response.json({ error: "host_unavailable" }, { status: 503, headers: noStore }); }
    return Response.json({ generation: host.state.generation }, { headers: noStore });
  }

  async invoke(name: string, route: string, input: unknown, agentId: string, signal: AbortSignal, context?: HandRemoteCallContext): Promise<Response | undefined> {
    if (!route.startsWith("screen:v1:")) return undefined;
    const id = crypto.randomUUID();
    const observation: CallObservation = { request_id: id,
      ...(safeIdentity(agentId) ? { runtime_session_id: agentId } : {}),
      ...(safeIdentity(context?.callId) ? { source_call_id: context!.callId, parent_call_id: context!.callId } : {}),
      ...(safeIdentity(context?.threadId) ? { thread_id: context!.threadId } : {}),
      ...(safeIdentity(context?.turnId) ? { turn_id: context!.turnId } : {}),
    };
    this.observe("call.received", undefined, observation);
    const target = this.list(true).find(target => target.agent_tools && screenTool(target).definition.name === name && screenTool(target).route_token === route);
    if (!target) {
      this.observe("call.terminal", undefined, { ...observation, reason_code: "stale_catalog" });
      return Response.json({ error: "stale_catalog" }, { status: 409 });
    }
    let action;
    try { action = screenAction(input); } catch {
      this.observe("call.terminal", undefined, { ...observation, reason_code: "invalid_input" });
      return Response.json(screenResult({ status: "invalid" }, target));
    }
    if (!ID.test(agentId)) {
      this.observe("call.terminal", undefined, { ...observation, reason_code: "invalid_agent" });
      return Response.json({ error: "invalid_agent" }, { status: 400 });
    }
    const host = this.hosts().find(({ state }) => state.machineId === target.machine_id && state.generation === target.generation);
    if (!host) {
      this.observe("call.terminal", undefined, { ...observation, reason_code: "host_unavailable" });
      return Response.json({ error: "unavailable" }, { status: 404 });
    }
    if (this.pending.size >= 32 || [...this.pending.values()].some(pending => pending.socket === host.socket)) {
      this.observe("call.terminal", host.state, { ...observation, reason_code: "busy" });
      return Response.json(screenResult({ status: "busy" }, target));
    }
    if ((action.action === "recording" && !recordingAvailable(target.recording)) || (action.action !== "recording" && action.action !== "observe" && action.action !== "release" && !target.controllable)) {
      this.observe("call.terminal", host.state, { ...observation, reason_code: "not_controllable" });
      return Response.json(screenResult({ status: "unavailable" }, target));
    }
    const ledger = context?.ledger;
    const deadlineAt = Date.now() + 9000, shape = screenResultShape(action);
    if (ledger) {
      // Synchronous with every check above: a fenced or duplicate identity is never sent.
      let admission: "admitted" | "fenced" | "duplicate";
      try { admission = ledger.admit({ requestId: id, connectionId: host.state.id, generation: host.state.generation, target, deadlineAt, ...shape }); }
      catch { admission = "duplicate"; }
      if (admission !== "admitted") {
        this.observe("call.terminal", host.state, { ...observation, reason_code: admission === "fenced" ? "call_fenced" : "duplicate_call" });
        return Response.json({ error: admission === "fenced" ? "call_fenced" : "duplicate_call",
          admission: admission === "fenced" ? "none" : "retained" }, { status: 409, headers: noStore });
      }
    }
    // A caller that already cancelled never sends this action. Admission runs
    // first, so a fenced identity still reads as call_fenced and an admitted one
    // retains its cancelled receipt.
    if (signal.aborted) {
      try { ledger?.settle(id, { status: "cancelled" }); } catch { /* The ledger row stays unresolved. */ }
      this.observe("call.terminal", host.state, { ...observation, reason_code: "aborted" });
      return Response.json(screenResult({ status: "cancelled" }, target), { headers: noStore });
    }
    // Never retry after admission: a lost response must not replay a click.
    const result = await new Promise<AgentScreenResult>(resolve => {
      const finish = (result: AgentScreenResult, reasonCode: HandRemoteReasonCode = resultReason(result.status)) => {
        if (!this.pending.delete(id)) return;
        this.observe("call.terminal", host.state, { ...observation, reason_code: reasonCode });
        clearTimeout(timer); signal.removeEventListener("abort", abort);
        // Retain before the result can leave this broker. A failed write keeps
        // the identity running in the ledger, which later reads as unknown.
        try { ledger?.settle(id, result); } catch { /* The ledger row stays unresolved. */ }
        resolve(result);
      };
      const cancel = (reasonCode: "aborted" | "timeout") => {
        this.observe(reasonCode === "timeout" ? "call.timeout" : "call.cancel", host.state, { ...observation, reason_code: reasonCode });
        try { this.send(host.socket, { type: "agent_cancel", request_id: id }); } catch {
          this.observe("call.transport_loss", host.state, { ...observation, reason_code: "send_failed" });
        }
        finish({ status: "cancelled" }, reasonCode);
      };
      const abort = () => cancel("aborted");
      const timer = setTimeout(() => cancel("timeout"), Math.max(0, deadlineAt - Date.now()));
      this.pending.set(id, { socket: host.socket, ...shape, observation, finish });
      this.observe("call.admitted", host.state, observation);
      signal.addEventListener("abort", abort, { once: true });
      try {
        this.observe("call.send_started", host.state, observation);
        this.send(host.socket, { type: "agent_call", request_id: id, agent_id: agentId, surface_id: target.id,
          generation: target.generation, deadline_at: deadlineAt - 1000, input: action });
        // send() acceptance is not an execution acknowledgment from the host.
        this.observe("call.sent", host.state, observation);
      } catch {
        this.observe("call.transport_loss", host.state, { ...observation, reason_code: "send_failed" });
        finish({ status: "unavailable" }, "send_failed");
      }
    });
    return Response.json(screenResult(result, target), { headers: noStore });
  }

  fetch(request: Request, vm?: RemoteVMPublisher): Response {
    this.sweep();
    const url = new URL(request.url);
    if (vm && (url.pathname !== "/hands/host" || vm.expiresAt <= Date.now())) return this.forbidden();
    if (url.pathname === "/hands/screens" && request.method === "GET" && !url.search) {
      return Response.json({ surfaces: this.list() }, { headers: noStore });
    }
    if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
      return Response.json({ error: "invalid_request" }, { status: 400, headers: noStore });
    }
    if (this.context.getWebSockets(TAG).length >= MAX_CONNECTIONS) {
      return Response.json({ error: "remote_capacity" }, { status: 429, headers: noStore });
    }
    const prefix = this.hooks.idPrefix?.() ?? "";
    const state: Attachment = { kind: TAG, role: "host", id: prefix + crypto.randomUUID(), generation: prefix + crypto.randomUUID(),
      expiresAt: Date.now() + LEASE_MS, rateWindow: Date.now(), rateCount: 0 };
    if (url.pathname === "/hands/host" && this.hooks.nextSequence) state.sequence = this.hooks.nextSequence();
    if (vm) {
      state.vm = vm;
      state.expiresAt = Math.min(state.expiresAt, vm.expiresAt);
      if (vm.machineName) state.machineName = vm.machineName;
    }
    let host: WebSocket | undefined;
    if (url.pathname === "/hands/view") {
      const machineId = url.searchParams.get("machine_id"), surfaceId = url.searchParams.get("surface_id"), generation = url.searchParams.get("generation");
      const initial = url.searchParams.get("frame_window");
      if ([...url.searchParams].length !== (initial === null ? 3 : 4) || !machineId || !surfaceId || !generation) return this.invalid();
      const selected = this.hosts().find(({ state }) => state.machineId === machineId && state.generation === generation
        && state.surfaces?.some(surface => surface.id === surfaceId));
      if (!selected) return Response.json({ error: "remote_unavailable" }, { status: 409, headers: noStore });
      const surface = selected.state.surfaces!.find(surface => surface.id === surfaceId)!;
      // Fence legacy native frame publications retained across broker upgrades.
      if (surface.transport === "frames-v1" && (!cloudflarePublisher(selected.state) || !cloudflareFrames(selected.state.machineId!, surface.kind))) {
        return Response.json({ error: "native_video_required" }, { status: 409, headers: noStore });
      }
      host = selected.socket;
      Object.assign(state, { role: "viewer", hostId: selected.state.id, generation, machineId, surfaceId,
        transport: selected.state.surfaces!.find(surface => surface.id === surfaceId)!.transport,
        frameWindow: selected.state.surfaces!.find(surface => surface.id === surfaceId)!.frame_window ?? 1 });
      if (initial !== null) {
        if (state.transport !== "frames-v1" || state.frameWindow! <= 1 || !/^[1-6]$/.test(initial) || Number(initial) > state.frameWindow!) return this.invalid();
        state.framePending = Number(initial);
      }
    } else if (url.pathname !== "/hands/host" || url.search) return this.invalid();
    const [client, server] = Object.values(new WebSocketPair());
    this.context.acceptWebSocket(server, [TAG]);
    if (host) { state.signalSeen = 0; state.admittedAt = Date.now(); }
    server.serializeAttachment(state);
    // Signaling-critical sends precede every diagnostic write.
    this.send(server, { type: "ready", connection_id: state.id, generation: state.generation, expires_at: state.expiresAt });
    if (host) {
      this.send(host, { type: "viewer", viewer_id: state.id, surface_id: state.surfaceId, generation: state.generation });
      if (state.framePending) this.send(host, { type: "frame_request", viewer_id: state.id, count: state.framePending });
    }
    this.observe("connection.accepted", state);
    this.observe("connection.ready", state);
    return new Response(null, { status: 101, webSocket: client });
  }

  /** The caller must freshly authenticate this HTTP request, even with a live socket. */
  renew(connectionId: string, canPublish = false, vm?: RemoteVMPublisher): Response {
    this.sweep();
    const socket = this.context.getWebSockets(TAG).find(socket => {
      const state = this.attachment(socket);
      return state?.id === connectionId && state.expiresAt > Date.now();
    });
    if (!socket) return Response.json({ error: "remote_unavailable" }, { status: 409, headers: noStore });
    const state = this.attachment(socket)!;
    if (vm && (state.role !== "host" || state.vm?.machineId !== vm.machineId
      || state.vm.routeId !== vm.routeId || vm.expiresAt <= Date.now())) return this.forbidden();
    // Account credentials cannot extend a VM publication past its allocation lease.
    if (state.vm && !vm) return this.forbidden();
    if (state.role === "host" && !canPublish) return Response.json({ error: "forbidden" }, { status: 403, headers: noStore });
    state.expiresAt = Date.now() + LEASE_MS;
    if (vm) {
      state.vm = vm;
      state.expiresAt = Math.min(state.expiresAt, vm.expiresAt);
      if (vm.machineName) state.machineName = vm.machineName;
    }
    state.renewalCount = (state.renewalCount ?? 0) + 1;
    socket.serializeAttachment(state);
    this.send(socket, { type: "renewed", expires_at: state.expiresAt });
    // Summarize steady renewal traffic rather than journaling every heartbeat.
    if (state.renewalCount === 1 || state.renewalCount % 16 === 0) {
      this.observe("connection.renewed", state, { renewal_count: state.renewalCount });
    }
    return Response.json({ expires_at: state.expiresAt }, { headers: noStore });
  }

  async message(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    this.sweep();
    const initial = this.attachment(socket);
    if (!initial || initial.expiresAt <= Date.now()) return;
    let state: Attachment = initial;
    try {
      if (typeof message !== "string" || new TextEncoder().encode(message).length > 750_000) throw new Error();
      const value = JSON.parse(message);
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
      if (value.type === "agent_result" && state.role === "host") {
        exact(value, ["type", "request_id", "status", "jpeg", "width", "height", "observation", "recording"]);
        if (typeof value.request_id !== "string" || !["ok", "busy", "invalid", "unavailable", "cancelled"].includes(value.status)) throw new Error();
        if (value.jpeg !== undefined && (value.status !== "ok" || typeof value.jpeg !== "string"
          || value.jpeg.length > 700_000 || !/^\/9j\/[A-Za-z0-9+/]*={0,2}$/.test(value.jpeg)
          || ![value.width, value.height].every(n => Number.isInteger(n) && n > 0 && n <= 4096))) throw new Error();
        const pending = this.pending.get(value.request_id);
        if (pending?.socket === socket) {
          if (!screenResultMatches(value, pending)) throw new Error();
          this.observe("call.receipt", state, { ...pending.observation, reason_code: resultReason(value.status) });
          pending.finish(value as AgentScreenResult);
        } else if (!pending && ID.test(value.request_id) && this.hooks.onLateResult) {
          // The ledger accepts it only for a retained identity of this exact host
          // connection whose admitted result shape it matches (screenResultMatches).
          try { this.hooks.onLateResult({ requestId: value.request_id, connectionId: state.id, generation: state.generation,
            result: value as AgentScreenResult }); } catch { /* A late receipt never fences the host. */ }
        }
        return;
      }
      if (value.type !== "frame" && new TextEncoder().encode(message).length > 70_000) throw new Error();
      if (Date.now() - state.rateWindow >= 1000) { state.rateWindow = Date.now(); state.rateCount = 0; }
      if (++state.rateCount > 160) throw new Error();
      socket.serializeAttachment(state);
      if (value.type === "broadcast_result" && value.target === "hls" && state.role === "host") {
        exact(value, ["type", "target", "request_id", "stream_id", "status", "error"]);
        if (!state.surfaces || typeof value.request_id !== "string" || !ID.test(value.request_id)
          || typeof value.stream_id !== "string" || !ID.test(value.stream_id) || !HLS_STATUSES.includes(value.status)
          || (value.error !== undefined && typeof value.error !== "string")) throw new Error();
        // Native errors may echo upload URLs; only protocol codes are forwarded.
        const result: HandRemoteHostResult = { type: "broadcast_result", target: "hls", request_id: value.request_id,
          stream_id: value.stream_id, status: value.status, machine_id: state.machineId!, generation: state.generation,
          ...(value.error === undefined ? {} : { error: HLS_ERRORS.includes(value.error) ? value.error : "broadcast_failed" }) };
        try { this.hooks.onHostResult?.(result); } catch { /* Status forwarding never fences the host. */ }
        return;
      }
      if (["broadcast", "broadcast_result"].includes(value.type)) {
        this.relayBroadcast(socket, state, value); return;
      }
      if (["frame_request", "frame", "control", "input"].includes(value.type)) {
        this.relayFrameMessage(socket, state, value); return;
      }
      if (value.type === "catalog" && state.role === "host" && state.surfaces === undefined && !state.claiming) {
        exact(value, ["type", "machine_id", "machine_name", "surfaces"]);
        if (typeof value.machine_id !== "string" || !ID.test(value.machine_id) || typeof value.machine_name !== "string" || !value.machine_name.trim()
          || new TextEncoder().encode(value.machine_name).length > 128) throw new Error();
        const surfaces = normalizeSurfaces(value.surfaces);
        if (surfaces.some(surface => surface.transport === "frames-v1" && (!cloudflarePublisher(state) || !cloudflareFrames(value.machine_id, surface.kind)))) throw new Error();
        if (state.vm && (value.machine_id !== state.vm.machineId
          || (!state.vm.anySurface && surfaces.some(surface => surface.kind !== (state.vm!.surfaceKind ?? "vm"))))) throw new Error();
        let claimed: HandRemoteCatalogClaim | undefined;
        if (this.hooks.claimCatalog) {
          // Owner authority decides cross-region placement before visibility.
          Object.assign(state, { claiming: true, claimMachineId: value.machine_id }); socket.serializeAttachment(state);
          const claim: HandRemoteCatalogClaim = { machineId: value.machine_id, generation: state.generation, connectionId: state.id,
            sequence: state.sequence ?? 0, surfaces: surfaces.map(surface => surface.id), ...(state.vm ? { routeId: state.vm.routeId } : {}) };
          const decision = this.claims.then(async () => {
            // A fence while queued already decided this publisher; never ask authority for it.
            const queued = this.attachment(socket);
            if (!queued || queued.id !== claim.connectionId || queued.expiresAt <= Date.now()) return false;
            return await this.hooks.claimCatalog!(claim) === true;
          });
          this.claims = decision.catch(() => undefined);
          let granted = false;
          try { granted = await decision; } catch { granted = false; }
          const current = this.attachment(socket);
          // Closed, expired or fenced while awaiting: never publish late.
          if (!current || current.id !== state.id || current.expiresAt <= Date.now()) return;
          if (!granted) { this.close(socket, "Host publication rejected", "claim_rejected"); return; }
          delete current.claiming; delete current.claimMachineId; state = current;
          claimed = claim;
        }
        // Publish only a complete validated catalog. Replacement fences every old viewer.
        for (const old of this.hosts().filter(({ state: old }) => old.machineId === value.machine_id)) {
          this.observe("connection.replaced", old.state, { reason_code: "host_replaced" });
          this.close(old.socket, "Host replaced", "host_replaced");
        }
        Object.assign(state, { machineId: value.machine_id, machineName: state.vm?.machineName ?? value.machine_name, surfaces });
        socket.serializeAttachment(state);
        this.send(socket, { type: "published", generation: state.generation });
        if (claimed) try { this.hooks.onClaimPublished?.(claimed); } catch { /* A later retry fences explicitly. */ }
        this.observe("connection.published", state);
        return;
      }
      if (value.type === "ping") {
        exact(value, ["type"]); this.send(socket, { type: "pong" }); return;
      }
      if (value.type === "close_viewer" && state.role === "host") {
        exact(value, ["type", "viewer_id"]);
        if (typeof value.viewer_id !== "string" || !ID.test(value.viewer_id)) throw new Error();
        const viewer = this.context.getWebSockets(TAG).find(peer => {
          const candidate = this.attachment(peer);
          return candidate?.role === "viewer" && candidate.id === value.viewer_id && candidate.hostId === state.id
            && candidate.generation === state.generation && candidate.expiresAt > Date.now();
        });
        if (viewer) this.close(viewer, "Screen connection unavailable", "viewer_closed");
        return;
      }
      if (value.type !== "signal" || state.transport === "frames-v1") throw new Error();
      exact(value, state.role === "host" ? ["type", "viewer_id", "signal"] : ["type", "signal"]);
      const signal = normalizeSignal(value.signal, state.role);
      if (state.role === "viewer") {
        const host = this.hosts().find(({ state: host }) => host.id === state.hostId && host.generation === state.generation);
        if (!host) throw new Error();
        this.send(host.socket, { type: "signal", viewer_id: state.id, signal });
        this.signalPhase(socket, state, signal, "viewer_to_host");
      } else {
        if (!state.surfaces || typeof value.viewer_id !== "string") throw new Error();
        const viewer = this.context.getWebSockets(TAG).find(peer => {
          const candidate = this.attachment(peer);
          return candidate?.role === "viewer" && candidate.id === value.viewer_id && candidate.hostId === state.id
            && candidate.generation === state.generation && candidate.expiresAt > Date.now();
        });
        // A viewer may disconnect while its offer is being prepared.
        if (viewer) {
          const target = this.attachment(viewer)!;
          if (target.transport === "frames-v1") throw new Error();
          this.send(viewer, { type: "signal", signal });
          this.signalPhase(viewer, target, signal, "host_to_viewer");
        }
      }
    } catch (error) {
      const reasonCode = error !== null && typeof error === "object" && this.sendFailures.has(error) ? "send_failed" : "invalid_signaling";
      this.close(socket, "Invalid remote signaling", reasonCode);
    }
  }

  close(socket: WebSocket, reason = "Remote connection closed", reasonCode: HandRemoteReasonCode = "connection_closed", code?: number): void {
    const state = this.attachment(socket);
    if (!state || state.expiresAt === 0) return;
    const safeReason: HandRemoteReasonCode = REASON_CODES.has(reasonCode) ? reasonCode : "connection_closed";
    const closure = { reason_code: safeReason,
      close_code: typeof code === "number" && Number.isInteger(code) && code >= 1000 && code <= 4999 ? code : 1008 };
    if (safeReason === "lease_expired") this.observe("connection.lease_expired", state, closure);
    if (["host_replaced", "publisher_revoked", "lease_expired"].includes(safeReason)) this.observe("connection.fenced", state, closure);
    this.observe("connection.closed", state, closure);
    state.expiresAt = 0; socket.serializeAttachment(state);
    for (const pending of this.pending.values()) {
      if (pending.socket === socket) {
        this.observe("call.transport_loss", state, { ...pending.observation, ...closure });
        pending.finish({ status: "unavailable" }, safeReason);
      }
    }
    if (state.role === "host") {
      for (const peer of this.context.getWebSockets(TAG)) {
        const viewer = this.attachment(peer);
        if (viewer?.hostId === state.id) {
          const viewerReason = safeReason === "connection_closed" || safeReason === "websocket_closed" || safeReason === "websocket_error"
            ? "host_disconnected" : safeReason;
          if (viewer.expiresAt !== 0) {
            this.observe("connection.fenced", viewer, { reason_code: viewerReason });
            this.observe("connection.closed", viewer, { reason_code: viewerReason, close_code: 1008 });
          }
          viewer.expiresAt = 0; peer.serializeAttachment(viewer);
          try { peer.close(1008, reason); } catch { /* Already closed. */ }
        }
      }
    } else {
      const host = this.hosts().find(({ state: host }) => host.id === state.hostId);
      if (host) this.send(host.socket, { type: "viewer_left", viewer_id: state.id });
    }
    try { socket.close(1008, reason); } catch { /* Already closed. */ }
  }

  /** Only the leased viewer's selected publication can receive stream credentials. */
  private relayBroadcast(socket: WebSocket, state: Attachment, value: Record<string, any>): void {
    if (state.role === "viewer") {
      if (value.type !== "broadcast") throw new Error();
      exact(value, ["type", "request_id", "action", "url", "preset"]);
      if (typeof value.request_id !== "string" || !ID.test(value.request_id)
        || !["start", "stop", "status"].includes(value.action)) throw new Error();
      if (value.action === "start") {
        if (typeof value.url !== "string" || new TextEncoder().encode(value.url).length > 4096 || /[\s\x00-\x1f\x7f]/.test(value.url)) throw new Error();
        const endpoint = new URL(value.url);
        if (!["rtmp:", "rtmps:"].includes(endpoint.protocol) || !endpoint.hostname || endpoint.username || endpoint.password || value.url.includes("#") || /^rtmps?:\/\/[^/?#]*@/i.test(value.url) || !endpoint.pathname.replaceAll("/", "")
          || (value.preset !== undefined && !["source", "1080p", "720p", "twitch", "x"].includes(value.preset))) throw new Error();
      } else if (value.url !== undefined || value.preset !== undefined) throw new Error();
      const host = this.hosts().find(({ state: host }) => host.id === state.hostId && host.generation === state.generation);
      const surface = host?.state.surfaces?.find(surface => surface.id === state.surfaceId);
      if (!host || !surface) throw new Error();
      if (!surface.broadcast) {
        this.send(socket, { type: "broadcast_result", request_id: value.request_id, status: "failed", error: "unsupported" });
        return;
      }
      // Persist only correlation, never the endpoint or its stream key. Status
      // recovery uses a fresh request after reconnecting or Worker hibernation.
      state.broadcastRequest = value.request_id; socket.serializeAttachment(state);
      this.send(host.socket, { ...value, viewer_id: state.id, surface_id: state.surfaceId });
      return;
    }
    if (value.type !== "broadcast_result") throw new Error();
    exact(value, ["type", "viewer_id", "request_id", "status", "preset", "width", "height", "fps", "bitrate_kbps", "audio", "error"]);
    if (typeof value.viewer_id !== "string" || !ID.test(value.viewer_id)
      || typeof value.request_id !== "string" || !ID.test(value.request_id)
      || (value.audio !== undefined && typeof value.audio !== "boolean")
      || !["idle", "starting", "live", "reconnecting", "stopping", "failed", "stopped"].includes(value.status)
      || (value.preset !== undefined && !["source", "1080p", "720p", "twitch", "x"].includes(value.preset))) throw new Error();
    for (const [key, max] of [["width", 16384], ["height", 16384], ["fps", 240], ["bitrate_kbps", 1_000_000]] as const) {
      if (value[key] !== undefined && (typeof value[key] !== "number" || !Number.isInteger(value[key]) || value[key] < 0 || value[key] > max)) throw new Error();
    }
    // Native libraries may include the secret URL in their error text. Only
    // protocol error codes cross back into the browser.
    const safe = { ...value };
    delete safe.viewer_id;
    if (value.error !== undefined) safe.error = ["unsupported", "invalid_request", "unavailable", "busy", "capture_failed", "encoder_failed", "connection_failed", "broadcast_failed"].includes(value.error) ? value.error : "broadcast_failed";
    const viewer = this.context.getWebSockets(TAG).find(peer => {
      const candidate = this.attachment(peer);
      return candidate?.role === "viewer" && candidate.id === value.viewer_id && candidate.hostId === state.id
        && candidate.generation === state.generation && candidate.expiresAt > Date.now()
        && candidate.broadcastRequest === value.request_id
        && state.surfaces?.some(surface => surface.id === candidate.surfaceId && surface.broadcast);
    });
    if (viewer) {
      const attachment = this.attachment(viewer)!;
      delete attachment.broadcastRequest; viewer.serializeAttachment(attachment);
      this.send(viewer, safe);
    }
  }

  /** Pull-based frames use the same account, publication and authorization lease. */
  private relayFrameMessage(socket: WebSocket, state: Attachment, value: Record<string, any>): void {
    // Also stop already-admitted legacy native frame viewers after an upgrade.
    if (!(state.role === "viewer" ? cloudflareFrames(state.machineId ?? "", "desktop") : cloudflarePublisher(state))) throw new Error();
    if (state.role === "viewer") {
      if (state.transport !== "frames-v1" || !["frame_request", "control", "input"].includes(value.type)) throw new Error();
      exact(value, value.type === "frame_request" ? ["type", "count"] : ["type", "data"]);
      const host = this.hosts().find(({ state: host }) => host.id === state.hostId && host.generation === state.generation);
      if (!host || !cloudflarePublisher(host.state)) throw new Error();
      if (value.type === "frame_request") {
        const count = value.count ?? 1, window = state.frameWindow ?? 1;
        if ((value.count !== undefined && window === 1) || !Number.isInteger(count) || count < 1 || count > window) throw new Error();
        const pending = Number(state.framePending ?? 0);
        if (value.count === undefined && pending >= window) return;
        if (pending + count > window) throw new Error();
        state.framePending = pending + count; socket.serializeAttachment(state);
      } else if (!value.data || typeof value.data !== "object" || Array.isArray(value.data)
        || new TextEncoder().encode(JSON.stringify(value.data)).length > 8192) throw new Error();
      this.send(host.socket, { ...value, viewer_id: state.id });
      return;
    }
    if (!["frame", "control"].includes(value.type) || typeof value.viewer_id !== "string") throw new Error();
    const viewer = this.context.getWebSockets(TAG).find(peer => {
      const candidate = this.attachment(peer);
      return candidate?.role === "viewer" && candidate.id === value.viewer_id && candidate.hostId === state.id
        && candidate.generation === state.generation && candidate.expiresAt > Date.now() && candidate.transport === "frames-v1";
    });
    if (!viewer) return; // The requested frame may finish after its viewer leaves.
    if (value.type === "frame") {
      exact(value, ["type", "viewer_id", "jpeg", "width", "height"]);
      const target = this.attachment(viewer)!;
      if (!target.framePending || typeof value.jpeg !== "string" || value.jpeg.length > 700_000
        || !/^\/9j\/[A-Za-z0-9+/]*={0,2}$/.test(value.jpeg)
        || ![value.width, value.height].every(n => Number.isInteger(n) && n > 0 && n <= 1280)) throw new Error();
      target.framePending = Number(target.framePending) - 1; viewer.serializeAttachment(target);
      this.send(viewer, { type: "frame", jpeg: value.jpeg, width: value.width, height: value.height });
    } else {
      exact(value, ["type", "viewer_id", "data"]);
      if (!value.data || !["granted", "denied", "revoked"].includes(value.data.type)) throw new Error();
      exact(value.data, ["type", "generation"]);
      if (value.data.type === "granted" && value.data.generation === undefined) throw new Error();
      if (value.data.generation !== undefined && (typeof value.data.generation !== "string" || !ID.test(value.data.generation))) throw new Error();
      this.send(viewer, { type: "control", data: value.data });
    }
  }

  /** At most four observations per viewer: offer, answer and first candidate per direction. */
  private signalPhase(viewer: WebSocket, state: Attachment, signal: unknown, direction: "host_to_viewer" | "viewer_to_host"): void {
    if (state.signalSeen === undefined || state.admittedAt === undefined) return;
    const type = (signal as { type?: unknown }).type;
    const [bit, stage] = type === "offer" ? [1, "signal.offer_relayed"] as const : type === "answer" ? [2, "signal.answer_relayed"] as const
      : direction === "host_to_viewer" ? [4, "signal.first_candidate"] as const : [8, "signal.first_candidate"] as const;
    if (state.signalSeen & bit) return;
    state.signalSeen |= bit; viewer.serializeAttachment(state);
    this.observe(stage, state, { signal_direction: direction, signal_elapsed_ms: Math.max(0, Date.now() - state.admittedAt) });
  }

  private sweep(): void {
    for (const socket of this.context.getWebSockets(TAG)) {
      const state = this.attachment(socket);
      if (state && state.expiresAt > 0 && state.expiresAt <= Date.now()) this.close(socket, "Authorization expired", "lease_expired");
    }
  }
  private hosts(): { socket: WebSocket; state: Attachment }[] {
    return this.context.getWebSockets(TAG).flatMap(socket => {
      const state = this.attachment(socket);
      return state?.role === "host" && state.surfaces && state.expiresAt > Date.now() ? [{ socket, state }] : [];
    });
  }
  private attachment(socket: WebSocket): Attachment | undefined {
    const state = socket.deserializeAttachment();
    return state?.kind === TAG ? state : undefined;
  }
  private projectObservation(stage: HandRemoteObservation["stage"], state?: Attachment,
    fields: ObservationFields = {}): HandRemoteObservation {
    const pendingCalls = state ? [...this.pending.values()].filter(pending => this.attachment(pending.socket)?.id === state.id).length : this.pending.size;
    const handId = state?.machineId ?? state?.vm?.machineId;
    const active = state !== undefined && state.expiresAt > Date.now();
    return { stage, ...fields, pending_calls: pendingCalls,
      ...(state ? { connection_id: state.id, remote_generation: state.generation, role: state.role, lease_expires_at: state.expiresAt } : {}),
      ...(state?.hostId ? { host_connection_id: state.hostId } : {}),
      ...(typeof handId === "string" && ID.test(handId) ? { hand_id: handId } : {}),
      ...(stage === "snapshot" && state ? { active } : {}),
    };
  }
  private observe(stage: HandRemoteObservation["stage"], state?: Attachment, fields: ObservationFields = {}): void {
    if (!this.onObservation) return;
    try {
      this.onObservation(this.projectObservation(stage, state, fields));
    } catch { /* Passive diagnostics never fail, retry, or cancel a real call. */ }
  }
  private send(socket: WebSocket, value: unknown): void {
    const message = JSON.stringify(value);
    try { socket.send(message); } catch (error) {
      if (error !== null && typeof error === "object") this.sendFailures.add(error);
      try { this.observe("connection.transport_loss", this.attachment(socket), { reason_code: "send_failed" }); } catch { /* Preserve the send failure. */ }
      throw error;
    }
  }
  private invalid(): Response { return Response.json({ error: "invalid_request" }, { status: 400, headers: noStore }); }
  private forbidden(): Response { return Response.json({ error: "forbidden" }, { status: 403, headers: noStore }); }
}

function cloudflarePublisher(state: Attachment): boolean {
  return state.vm?.surfaceKind === "desktop" && state.vm.routeId.startsWith("hand-host:")
    && cloudflareFrames(state.vm.machineId, "desktop");
}

function cloudflareFrames(machineId: string, kind: string): boolean {
  return /^cf:[A-Za-z0-9][A-Za-z0-9._:-]{0,120}$/.test(machineId) && ["desktop", "vm"].includes(kind);
}

function safeIdentity(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_./:-]{1,128}$/.test(value);
}
function resultReason(status: AgentScreenResult["status"]): HandRemoteReasonCode {
  switch (status) {
    case "ok": return "result_ok";
    case "busy": return "result_busy";
    case "invalid": return "result_invalid";
    case "cancelled": return "result_cancelled";
    default: return "result_unavailable";
  }
}

function exact(value: Record<string, unknown>, allowed: string[]): void {
  if (Object.keys(value).some(key => !allowed.includes(key))) throw new Error("Unexpected field");
}
function normalizeSurfaces(value: unknown): Surface[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > 8) throw new Error("Invalid surfaces");
  const ids = new Set();
  return value.map(surface => {
    if (!surface || typeof surface !== "object") throw new Error();
    exact(surface, ["id", "name", "kind", "width", "height", "controllable", "agent_tools", "recording", "recordingCapabilities", "broadcast", "playback", "transport", "frame_window"]);
    if (typeof surface.id !== "string" || !ID.test(surface.id) || ids.has(surface.id)
      || typeof surface.name !== "string" || !surface.name.trim() || new TextEncoder().encode(surface.name).length > 128
      || !["desktop", "window", "phone", "vm"].includes(surface.kind) || typeof surface.controllable !== "boolean"
      || (surface.recording !== undefined && !validRecordingCapability(surface.recording))
      || (surface.recordingCapabilities !== undefined && (!surface.recordingCapabilities || typeof surface.recordingCapabilities !== "object"
        || !validRecordingCapability(surface.recordingCapabilities) || surface.recordingCapabilities.available !== surface.recording))
      || (surface.broadcast !== undefined && typeof surface.broadcast !== "boolean")
      || (surface.playback !== undefined && typeof surface.playback !== "boolean")
      || (surface.agent_tools !== undefined && typeof surface.agent_tools !== "boolean")
      || (surface.transport !== undefined && surface.transport !== "frames-v1")
      || (surface.frame_window !== undefined && (surface.transport !== "frames-v1"
        || !Number.isInteger(surface.frame_window) || surface.frame_window < 1 || surface.frame_window > 6))
      || ![surface.width, surface.height].every(n => Number.isInteger(n) && n > 0 && n <= 16384)) throw new Error();
    ids.add(surface.id); return surface as Surface;
  });
}
function normalizeSignal(value: unknown, role: "host" | "viewer"): unknown {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
  const signal = value as Record<string, unknown>;
  if (signal.type === "candidate") {
    exact(signal, ["type", "candidate", "sdpMid", "sdpMLineIndex"]);
    if (typeof signal.candidate !== "string" || signal.candidate.length > 4096
      || (signal.sdpMid != null && (typeof signal.sdpMid !== "string" || signal.sdpMid.length > 128))
      || !Number.isInteger(signal.sdpMLineIndex) || Number(signal.sdpMLineIndex) < 0 || Number(signal.sdpMLineIndex) > 32) throw new Error();
  } else {
    exact(signal, ["type", "sdp"]);
    if (signal.type !== (role === "host" ? "offer" : "answer") || typeof signal.sdp !== "string"
      || !signal.sdp || new TextEncoder().encode(signal.sdp).length > 65_536) throw new Error();
  }
  return signal;
}
