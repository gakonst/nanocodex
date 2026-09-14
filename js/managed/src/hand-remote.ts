import { screenAction, screenResult, screenTool, type AgentScreenResult, type ScreenTarget } from "./hand-remote-agent";

/** Human media/input use WebRTC. Bounded agent calls use the authenticated host socket. */
const TAG = "hand-remote";
const MAX_CONNECTIONS = 64;
const LEASE_MS = 30_000;
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const noStore = { "cache-control": "no-store" };
export const REMOTE_VM_ASSERTION = "x-nanocodex-remote-vm";
export type RemoteVMPublisher = { machineId: string; routeId: string; expiresAt: number; surfaceKind?: "desktop" };

type Surface = { id: string; name: string; kind: "desktop" | "window" | "phone" | "vm"; width: number; height: number; controllable: boolean; agent_tools?: boolean; transport?: "frames-v1" };
type Attachment = {
  kind: typeof TAG; role: "host" | "viewer"; id: string; generation: string; expiresAt: number;
  machineId?: string; machineName?: string; surfaces?: Surface[]; hostId?: string; surfaceId?: string;
  rateWindow: number; rateCount: number;
  vm?: RemoteVMPublisher;
  transport?: "frames-v1";
  framePending?: boolean;
};
type Context = Pick<DurableObjectState, "acceptWebSocket" | "getWebSockets">;

export class HandRemoteBroker {
  private readonly pending = new Map<string, { socket: WebSocket; expectsImage: boolean; finish(result: AgentScreenResult): void }>();
  constructor(private readonly context: Context) {}

  owns(socket: WebSocket): boolean { return this.attachment(socket) !== undefined; }

  list(): ScreenTarget[] {
    this.sweep();
    return this.hosts().flatMap(({ state }) => (state.surfaces ?? []).map(surface => ({
      ...surface, machine_id: state.machineId!, machine_name: state.machineName!, generation: state.generation,
    })));
  }

  tools() { return this.list().filter(target => target.agent_tools).map(screenTool); }

  revokePublisher(routeId: string): void {
    for (const socket of this.context.getWebSockets(TAG)) {
      if (this.attachment(socket)?.vm?.routeId === routeId) this.close(socket, "Hand revoked");
    }
  }

  async invoke(name: string, route: string, input: unknown, agentId: string, signal: AbortSignal): Promise<Response | undefined> {
    if (!route.startsWith("screen:v1:")) return undefined;
    const target = this.list().find(target => target.agent_tools && screenTool(target).definition.name === name && screenTool(target).route_token === route);
    if (!target) return Response.json({ error: "stale_catalog" }, { status: 409 });
    let action;
    try { action = screenAction(input); } catch { return Response.json(screenResult({ status: "invalid" }, target)); }
    if (!ID.test(agentId)) return Response.json({ error: "invalid_agent" }, { status: 400 });
    const host = this.hosts().find(({ state }) => state.machineId === target.machine_id && state.generation === target.generation);
    if (!host) return Response.json({ error: "unavailable" }, { status: 404 });
    if (this.pending.size >= 32 || [...this.pending.values()].some(pending => pending.socket === host.socket)) {
      return Response.json(screenResult({ status: "busy" }, target));
    }
    if (action.action !== "observe" && action.action !== "release" && !target.controllable) {
      return Response.json(screenResult({ status: "unavailable" }, target));
    }
    // Never retry after admission: a lost response must not replay a click.
    const id = crypto.randomUUID();
    const result = await new Promise<AgentScreenResult>(resolve => {
      const finish = (result: AgentScreenResult) => {
        if (!this.pending.delete(id)) return;
        clearTimeout(timer); signal.removeEventListener("abort", abort); resolve(result);
      };
      const abort = () => {
        try { this.send(host.socket, { type: "agent_cancel", request_id: id }); } catch { /* Already disconnected. */ }
        finish({ status: "cancelled" });
      };
      const timer = setTimeout(abort, 9000);
      this.pending.set(id, { socket: host.socket, expectsImage: action.action !== "release", finish });
      signal.addEventListener("abort", abort, { once: true });
      if (signal.aborted) { abort(); return; }
      try {
        this.send(host.socket, { type: "agent_call", request_id: id, agent_id: agentId, surface_id: target.id,
          generation: target.generation, deadline_at: Date.now() + 8000, input: action });
      } catch { finish({ status: "unavailable" }); }
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
    const state: Attachment = { kind: TAG, role: "host", id: crypto.randomUUID(), generation: crypto.randomUUID(),
      expiresAt: Date.now() + LEASE_MS, rateWindow: Date.now(), rateCount: 0 };
    if (vm) { state.vm = vm; state.expiresAt = Math.min(state.expiresAt, vm.expiresAt); }
    let host: WebSocket | undefined;
    if (url.pathname === "/hands/view") {
      const machineId = url.searchParams.get("machine_id"), surfaceId = url.searchParams.get("surface_id"), generation = url.searchParams.get("generation");
      if ([...url.searchParams].length !== 3 || !machineId || !surfaceId || !generation) return this.invalid();
      const selected = this.hosts().find(({ state }) => state.machineId === machineId && state.generation === generation
        && state.surfaces?.some(surface => surface.id === surfaceId));
      if (!selected) return Response.json({ error: "remote_unavailable" }, { status: 409, headers: noStore });
      host = selected.socket;
      Object.assign(state, { role: "viewer", hostId: selected.state.id, generation, machineId, surfaceId,
        transport: selected.state.surfaces!.find(surface => surface.id === surfaceId)!.transport });
    } else if (url.pathname !== "/hands/host" || url.search) return this.invalid();
    const [client, server] = Object.values(new WebSocketPair());
    this.context.acceptWebSocket(server, [TAG]);
    server.serializeAttachment(state);
    this.send(server, { type: "ready", connection_id: state.id, generation: state.generation, expires_at: state.expiresAt });
    if (host) this.send(host, { type: "viewer", viewer_id: state.id, surface_id: state.surfaceId, generation: state.generation });
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
    if (vm) { state.vm = vm; state.expiresAt = Math.min(state.expiresAt, vm.expiresAt); }
    socket.serializeAttachment(state);
    this.send(socket, { type: "renewed", expires_at: state.expiresAt });
    return Response.json({ expires_at: state.expiresAt }, { headers: noStore });
  }

  message(socket: WebSocket, message: string | ArrayBuffer): void {
    this.sweep();
    const state = this.attachment(socket);
    if (!state || state.expiresAt <= Date.now()) return;
    try {
      if (typeof message !== "string" || new TextEncoder().encode(message).length > 750_000) throw new Error();
      const value = JSON.parse(message);
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
      if (value.type === "agent_result" && state.role === "host") {
        exact(value, ["type", "request_id", "status", "jpeg", "width", "height"]);
        if (typeof value.request_id !== "string" || !["ok", "busy", "invalid", "unavailable", "cancelled"].includes(value.status)) throw new Error();
        if (value.jpeg !== undefined && (value.status !== "ok" || typeof value.jpeg !== "string"
          || value.jpeg.length > 700_000 || !/^\/9j\/[A-Za-z0-9+/]*={0,2}$/.test(value.jpeg)
          || ![value.width, value.height].every(n => Number.isInteger(n) && n > 0 && n <= 4096))) throw new Error();
        const pending = this.pending.get(value.request_id);
        if (pending?.socket === socket) {
          if (pending.expectsImage && value.status === "ok" && value.jpeg === undefined) throw new Error();
          pending.finish(value as AgentScreenResult);
        }
        return;
      }
      if (value.type !== "frame" && new TextEncoder().encode(message).length > 70_000) throw new Error();
      if (Date.now() - state.rateWindow >= 1000) { state.rateWindow = Date.now(); state.rateCount = 0; }
      if (++state.rateCount > 160) throw new Error();
      socket.serializeAttachment(state);
      if (["frame_request", "frame", "control", "input"].includes(value.type)) {
        this.relayFrameMessage(socket, state, value); return;
      }
      if (value.type === "catalog" && state.role === "host" && state.surfaces === undefined) {
        exact(value, ["type", "machine_id", "machine_name", "surfaces"]);
        if (typeof value.machine_id !== "string" || !ID.test(value.machine_id) || typeof value.machine_name !== "string" || !value.machine_name.trim()
          || new TextEncoder().encode(value.machine_name).length > 128) throw new Error();
        const surfaces = normalizeSurfaces(value.surfaces);
        if (state.vm && (value.machine_id !== state.vm.machineId
          || surfaces.some(surface => surface.kind !== (state.vm!.surfaceKind ?? "vm")))) throw new Error();
        // Publish only a complete validated catalog. Replacement fences every old viewer.
        for (const old of this.hosts().filter(({ state: old }) => old.machineId === value.machine_id)) {
          this.close(old.socket, "Host replaced");
        }
        Object.assign(state, { machineId: value.machine_id, machineName: value.machine_name, surfaces });
        socket.serializeAttachment(state);
        this.send(socket, { type: "published", generation: state.generation });
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
        if (viewer) this.close(viewer, "Screen connection unavailable");
        return;
      }
      if (value.type !== "signal" || state.transport === "frames-v1") throw new Error();
      exact(value, state.role === "host" ? ["type", "viewer_id", "signal"] : ["type", "signal"]);
      const signal = normalizeSignal(value.signal, state.role);
      if (state.role === "viewer") {
        const host = this.hosts().find(({ state: host }) => host.id === state.hostId && host.generation === state.generation);
        if (!host) throw new Error();
        this.send(host.socket, { type: "signal", viewer_id: state.id, signal });
      } else {
        if (!state.surfaces || typeof value.viewer_id !== "string") throw new Error();
        const viewer = this.context.getWebSockets(TAG).find(peer => {
          const candidate = this.attachment(peer);
          return candidate?.role === "viewer" && candidate.id === value.viewer_id && candidate.hostId === state.id
            && candidate.generation === state.generation && candidate.expiresAt > Date.now();
        });
        // A viewer may disconnect while its offer is being prepared.
        if (viewer) {
          if (this.attachment(viewer)?.transport === "frames-v1") throw new Error();
          this.send(viewer, { type: "signal", signal });
        }
      }
    } catch { this.close(socket, "Invalid remote signaling"); }
  }

  close(socket: WebSocket, reason = "Remote connection closed"): void {
    const state = this.attachment(socket);
    if (!state || state.expiresAt === 0) return;
    state.expiresAt = 0; socket.serializeAttachment(state);
    for (const pending of this.pending.values()) {
      if (pending.socket === socket) pending.finish({ status: "unavailable" });
    }
    if (state.role === "host") {
      for (const peer of this.context.getWebSockets(TAG)) {
        const viewer = this.attachment(peer);
        if (viewer?.hostId === state.id) {
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

  /** Pull-based frames use the same account, publication and authorization lease. */
  private relayFrameMessage(socket: WebSocket, state: Attachment, value: Record<string, any>): void {
    if (state.role === "viewer") {
      if (state.transport !== "frames-v1" || !["frame_request", "control", "input"].includes(value.type)) throw new Error();
      exact(value, value.type === "frame_request" ? ["type"] : ["type", "data"]);
      const host = this.hosts().find(({ state: host }) => host.id === state.hostId && host.generation === state.generation);
      if (!host) throw new Error();
      if (value.type === "frame_request") {
        if (state.framePending) return;
        state.framePending = true; socket.serializeAttachment(state);
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
      target.framePending = false; viewer.serializeAttachment(target);
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

  private sweep(): void {
    for (const socket of this.context.getWebSockets(TAG)) {
      const state = this.attachment(socket);
      if (state && state.expiresAt > 0 && state.expiresAt <= Date.now()) this.close(socket, "Authorization expired");
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
  private send(socket: WebSocket, value: unknown): void { socket.send(JSON.stringify(value)); }
  private invalid(): Response { return Response.json({ error: "invalid_request" }, { status: 400, headers: noStore }); }
  private forbidden(): Response { return Response.json({ error: "forbidden" }, { status: 403, headers: noStore }); }
}

function exact(value: Record<string, unknown>, allowed: string[]): void {
  if (Object.keys(value).some(key => !allowed.includes(key))) throw new Error("Unexpected field");
}
function normalizeSurfaces(value: unknown): Surface[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > 8) throw new Error("Invalid surfaces");
  const ids = new Set();
  return value.map(surface => {
    if (!surface || typeof surface !== "object") throw new Error();
    exact(surface, ["id", "name", "kind", "width", "height", "controllable", "agent_tools", "transport"]);
    if (typeof surface.id !== "string" || !ID.test(surface.id) || ids.has(surface.id)
      || typeof surface.name !== "string" || !surface.name.trim() || new TextEncoder().encode(surface.name).length > 128
      || !["desktop", "window", "phone", "vm"].includes(surface.kind) || typeof surface.controllable !== "boolean"
      || (surface.agent_tools !== undefined && typeof surface.agent_tools !== "boolean")
      || (surface.transport !== undefined && surface.transport !== "frames-v1")
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
