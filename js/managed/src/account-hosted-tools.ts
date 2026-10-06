import { DurableObject } from "cloudflare:workers";
import { CUA_JS_NAME, CUA_RESET_NAME } from "nanocodex-computer/contract";
import { HandPaths } from "./hand-paths";
import { HandRemoteBroker, REMOTE_VM_ASSERTION, type RemoteVMPublisher } from "./hand-remote";
import { validRecordingCapability, screenTool, type ScreenTarget } from "./hand-remote-agent";
import { HandHosts, boundedJSON } from "./hand-hosts";
import { remoteICE, type RemoteICEEnv } from "./hand-remote-ice";
import {
  HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE,
  HOSTED_MACHINE_TOOL_NAMES,
  type HostedMachine,
  type HostedMachineToolName,
  type HostedToolsCatalogValidator,
  type HostedToolsCatalogCandidate,
  type HostedToolsCodeDefinition,
  type HostedToolsCodeTool,
  type HostedToolsDynamicProvider,
} from "nanocodex-tools/hosted";
import type { SubagentToolContext } from "nanocodex-tools";

import { isUserId } from "./account-auth";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";
import { inventoryEntry, mergeInventory, WorkspaceHandRegistry, HAND_INVENTORY_DEADLINE_MS, WORKSPACE_INVENTORY_CONCURRENCY, type HandInventoryEntry, type HandInventory } from "./hand-inventory";
import { HostedToolsBroker } from "./hosted-tools-broker";
import { observeHandCall, observeHandSummary } from "./hand-call-observation";
import { annotateToolSpan, traceToolInvocation } from "./tool-tracing";
import { DiagnosticJournal, diagnosticScope } from "./diagnostic-journal";
import { RegionalHandDirectory, HAND_RELAY_REGION_HEADER, handRelayName, isHandRelayRegion,
  relayRouteToken, parseRelayRouteToken, publisherIdentity, validPublisherId,
  type HandPublication, type HandRelayRegion, type RegionalHandEnv } from "./regional-hand-routing";
import type { RegionalHandRelay } from "./regional-hand-relay";

const OWNER_ASSERTION = "x-nanocodex-owner-id";
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const PROCESS_SESSION_TOOL = Symbol.for("nanocodex.processSessionTool");

type AccountHostedTool = HostedToolsCatalogCandidate & Readonly<{
  route_token: string;
}>;

type AccountHostedMachine = Readonly<{
  online: boolean;
  machine: HostedMachine;
  tools: readonly Readonly<{
    name: HostedMachineToolName;
    parallel_safe: boolean;
    definition?: HostedToolsCodeDefinition;
    route_token: string;
  }>[];
}>;

type AccountHostedToolsSnapshot = Readonly<{
  tools: readonly AccountHostedTool[];
  machines: readonly AccountHostedMachine[];
  screens?: readonly ScreenTarget[];
  publications?: readonly HandPublication[];
  mount_roots?: Readonly<Record<string, string>>;
  inventory_unknown_ids?: readonly string[];
}>;

type RoutedHostedTool = HostedToolsCodeTool & Readonly<{
  provider: string;
  remoteName: string;
  summary?: string;
  timeoutMs: number;
}>;

type AccountHostedToolsEnv = RemoteICEEnv & RegionalHandEnv & {
  NANOCODEX_SESSIONS?: DurableObjectNamespace<import("./index").DurableAgentSession>;
};

type InvocationRequest = Readonly<{
  owner_id: string;
  name: string;
  input: unknown;
  session_id: string;
  thread_id?: string;
  turn_id?: string;
  call_id: string;
  model?: string;
  machine_id?: string;
  route_token: string;
}>;

type InvocationResult = Readonly<{
  output: unknown;
  structured_result: unknown;
  success: boolean;
  metadata: unknown;
  value: unknown;
  pre_admission_unavailable?: true;
  process_route_token?: string;
}>;

type InvocationContext = Readonly<{
  sessionId: string;
  turnId?: string;
  callId: string;
  model?: string;
  signal?: AbortSignal;
  subagent?: SubagentToolContext;
}>;

type AuthorizationContext = Pick<InvocationContext, "sessionId" | "subagent">;

/** One account-owned reverse attachment shared by every managed agent in that account. */
export class AccountHostedTools extends DurableObject<AccountHostedToolsEnv> {
  readonly #broker: HostedToolsBroker;
  readonly #remote: HandRemoteBroker;
  readonly #handHosts: HandHosts;
  readonly #diagnostics: DiagnosticJournal;
  #ownerId: string | undefined;
  readonly #regional: boolean;
  readonly #directory: RegionalHandDirectory;
  #publicationQueue: Promise<unknown> = Promise.resolve();
  #region: HandRelayRegion | undefined;

  constructor(ctx: DurableObjectState, env: AccountHostedToolsEnv, regional = false) {
    super(ctx, env);
    this.#regional = regional;
    this.#directory = new RegionalHandDirectory(ctx.storage);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_local_publications (
      route_id TEXT PRIMARY KEY, candidate_id TEXT, publication_json TEXT
    )`);
    this.#region = ctx.storage.kv.get<HandRelayRegion>("regional_hand_region");
    // Ownership is immutable; a new instance reloads it after eviction/restart.
    this.#ownerId = ctx.storage.kv.get<string>("owner_id");
    this.#diagnostics = new DiagnosticJournal(ctx.storage, "hand.broker");
    this.#broker = new HostedToolsBroker(ctx, { resumeRetainedSockets: true,
      beforeCatalogPublish: candidate => this.#admitPublication(candidate),
      onCallObservation: (observation) => {
        try { annotateToolSpan({ "nanocodex.thread_id": observation.thread_id,
          "nanocodex.runtime_session_id": observation.session_id,
          "nanocodex.tool_call_id": observation.source_call_id,
          "nanocodex.transport_call_id": observation.transport_call_id,
          "nanocodex.hand_id": observation.hand_id,
          "nanocodex.connection_id": observation.connection_id,
          "nanocodex.host_connection_id": observation.host_connection_id,
          "nanocodex.host_runtime_id": observation.host_runtime_id,
          "nanocodex.lease_id": observation.lease_id,
          "nanocodex.connection_generation": observation.connection_generation,
          "nanocodex.runtime_generation": observation.runtime_generation,
          "nanocodex.hand.host_stage": observation.host_stage,
          "nanocodex.hand.reason_code": observation.reason_code,
          "nanocodex.hand.stage": observation.stage, "nanocodex.hand.roundtrip_ms": observation.roundtrip_ms,
          "nanocodex.hand.execution_ms": observation.host_timing?.execution_ms }); }
        catch { /* Span annotation must not suppress the correlated log. */ }
        const record = { type: "hand.call.broker", ...observation };
        this.#diagnostics.record(record);
        console.info(record);
      },
      onConnectionObservation: observation => {
        const record = { type: "hand.connection", ...observation };
        this.#diagnostics.record(record);
        console.info(record);
      },
    });
    this.#remote = new HandRemoteBroker(ctx, observation => {
      const record = { type: "hand.remote", ...observation };
      this.#diagnostics.record(record);
      try { console.info(record); } catch { /* Remote diagnostics cannot change a socket outcome. */ }
    });
    this.#handHosts = new HandHosts(ctx.storage, this.#remote);
  }

  /** Discovery returns only its public projection in one RPC reply. */
  async listMachines(ownerId: string) {
    if (!isUserId(ownerId) || !this.#owns(ownerId)) return [];
    const snapshot = await this.#snapshot();
    const roots = new HandPaths(this.ctx.storage).assign(snapshot.machines.map(entry => entry.machine));
    return snapshot.machines.filter(entry => entry.online)
      .map(({ machine }) => ({ id: machine.id, name: machine.name, capabilities: machine.capabilities, workspace: roots.get(machine.id)! }));
  }

  /** Internal publication RPC; immutable account ownership fences the registry. */
  registerWorkspaceHands(ownerId: string, sessionId: string, entries: readonly HandInventoryEntry[]): boolean {
    if (!isUserId(ownerId) || !isUserId(sessionId) || !this.#claim(ownerId)) return false;
    return new WorkspaceHandRegistry(this.ctx.storage).register(sessionId, entries);
  }

  async handInventory(ownerId: string): Promise<HandInventory> {
    if (!isUserId(ownerId) || !this.#claim(ownerId)) return mergeInventory([], false);
    const registry = new WorkspaceHandRegistry(this.ctx.storage);
    const sessions = registry.entries();
    let complete = registry.complete;
    const sources: HandInventoryEntry[][] = [];
    const deadline = Date.now() + HAND_INVENTORY_DEADLINE_MS;
    const account = (async () => {
      try {
        const snapshot = await withHardDeadline("account Hand inventory", HAND_INVENTORY_DEADLINE_MS,
          () => this.#snapshot());
        const unknown = new Set(snapshot.inventory_unknown_ids ?? []);
        if (unknown.size) complete = false;
        sources.push(snapshot.machines.map(({ machine, online }) => inventoryEntry(machine, unknown.has(machine.id) ? null : online)));
      } catch {
        complete = false;
        // Preserve retained identities when regional discovery itself failed.
        const local = this.#localSnapshot().machines.map(({ machine }) => inventoryEntry(machine, null));
        const retained = this.#directory.entries().map(({ machine }) => inventoryEntry(machine, null));
        sources.push([...local, ...retained]);
      }
    })();
    let next = 0;
    const workers = Array.from({ length: Math.min(WORKSPACE_INVENTORY_CONCURRENCY, sessions.length) }, async () => {
      while (next < sessions.length) {
        const retained = sessions[next++]!;
        try {
          if (!this.env.NANOCODEX_SESSIONS || Date.now() >= deadline) throw new Error("workspace inventory unavailable");
          const result = await withHardDeadline("workspace Hand inventory", Math.max(1, deadline - Date.now()),
            () => this.env.NANOCODEX_SESSIONS!.getByName(retained.sessionId).listWorkspaceHands(ownerId));
          if (!result.complete) {
            complete = false;
            sources.push(retained.entries.map(entry => ({ ...entry, online: null, health: "unknown" })));
          }
          sources.push(result.data);
        } catch {
          complete = false;
          sources.push(retained.entries.map(entry => ({ ...entry, online: null, health: "unknown" })));
        }
      }
    });
    await Promise.all([account, ...workers]);
    return mergeInventory(sources, complete);
  }

  async fetch(request: Request): Promise<Response> {
    return diagnosticScope(this.#diagnostics, () => this.#fetchRequest(request));
  }

  async #fetchRequest(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/regional/")) return this.#regionalRequest(request, url);
    if (this.#regional && !["/tool-host", "/snapshot", "/invoke", "/diagnostics"].includes(url.pathname)) {
      return Response.json({ error: "not_found" }, { status: 404 });
    }
    if (url.pathname === "/diagnostics") {
      if (request.method !== "GET") return Response.json({ error: "method_not_allowed" }, { status: 405 });
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !this.#owns(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      const thread = url.searchParams.get("thread_id"), after = url.searchParams.get("after") ?? "0", limit = url.searchParams.get("limit") ?? "256";
      if (!thread || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(thread)
        || [...url.searchParams.keys()].some(key => !["thread_id", "after", "limit"].includes(key) || url.searchParams.getAll(key).length !== 1)
        || !/^(0|[1-9][0-9]*)$/.test(after) || !/^[1-9][0-9]*$/.test(limit)
        || !Number.isSafeInteger(Number(after)) || !Number.isSafeInteger(Number(limit)) || Number(limit) > 1_024)
        return Response.json({ error: "invalid_diagnostics_page" }, { status: 400 });
      return Response.json({ ...this.#diagnostics.page(thread, Number(after), Number(limit), true),
        connections: this.#broker.connectionDiagnostics(),
        remote_connections: this.#remote.connectionDiagnostics(),
      }, { headers: { "cache-control": "no-store" } });
    }
    const sandboxHost = url.pathname.match(/^\/sandbox-hand-hosts\/([^/]+)$/);
    if (sandboxHost) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (url.search || !isUserId(ownerId) || !this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      if (request.method === "DELETE") return this.#handHosts.manage(request, sandboxHost[1]);
      if (request.method !== "PUT") return Response.json({ error: "invalid_request" }, { status: 400 });
      let body;
      try { body = await boundedJSON(request); } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      if (!body || typeof body !== "object" || Array.isArray(body) || Object.keys(body).length !== 2
        || !("name" in body) || !("machine_id" in body) || typeof body.machine_id !== "string"
        || !/^cf:[A-Za-z0-9][A-Za-z0-9._:-]{0,120}$/.test(body.machine_id)) return Response.json({ error: "invalid_request" }, { status: 400 });
      return this.#handHosts.manage(new Request(request.url, { method: "PUT", body: JSON.stringify({ name: body.name }) }), sandboxHost[1], body.machine_id);
    }
    const setup = url.pathname.match(/^\/hand-host-setups\/([^/]+)$/);
    if (setup) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (url.search || !isUserId(ownerId) || !this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      return this.#handHosts.setupLock(request, setup[1]!);
    }
    if (url.pathname === "/hand-hosts" || url.pathname.startsWith("/hand-hosts/")) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      const publisher = url.pathname.match(/^\/hand-hosts\/([^/]+)\/hands\/(host|ice|renew)$/);
      if (publisher) {
        if (url.search || !this.#owns(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
        const scope = await this.#handHosts.authorize(request, publisher[1]!);
        if (!scope) return Response.json({ error: "unauthorized" }, { status: 401 });
        const endpoint = publisher[2]!;
        if (endpoint !== "host" && request.method !== "POST") return Response.json({ error: "invalid_request" }, { status: 400 });
        if (endpoint === "ice") return remoteICE(this.env, ownerId);
        if (endpoint === "renew") {
          let body;
          try { body = await boundedJSON(request); } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
          if (!body || typeof body !== "object" || Array.isArray(body) || Object.keys(body).length !== 1
            || !("connection_id" in body) || typeof body.connection_id !== "string") return Response.json({ error: "invalid_request" }, { status: 400 });
          // Recheck after reading the body: rotation/revocation may have occurred.
          const fresh = await this.#handHosts.authorize(request, publisher[1]!);
          if (!fresh) return Response.json({ error: "unauthorized" }, { status: 401 });
          return this.#remote.renew(body.connection_id, true, fresh);
        }
        return this.#remote.fetch(new Request("https://account-tools.internal/hands/host", request), scope);
      }
      const management = url.pathname.match(/^\/hand-hosts(?:\/([^/]+))?$/);
      if (!management || !this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      return this.#handHosts.manage(request, management[1]);
    }
    if (url.pathname === "/hands" || url.pathname.startsWith("/hands/")) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !this.#claim(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      let vm: RemoteVMPublisher | undefined;
      const encodedVM = request.headers.get(REMOTE_VM_ASSERTION);
      if (encodedVM !== null) {
        try {
          vm = JSON.parse(encodedVM);
          if (!vm || typeof vm.machineId !== "string" || typeof vm.routeId !== "string"
            || (vm.machineName !== undefined && (typeof vm.machineName !== "string"
              || !vm.machineName.trim() || new TextEncoder().encode(vm.machineName).length > 128))
            || !Number.isSafeInteger(vm.expiresAt) || vm.expiresAt <= Date.now()) throw new Error();
        } catch { return Response.json({ error: "forbidden" }, { status: 403 }); }
      }
      if (url.pathname === "/hands/renew" && request.method === "POST" && !url.search) {
        try {
          const reader = request.body?.getReader();
          if (!reader) throw new Error();
          let bytes = new Uint8Array();
          try {
            while (true) {
              const { done, value } = await reader.read();
              if (done) break;
              if (bytes.byteLength + value.byteLength > 256) throw new Error();
              const next = new Uint8Array(bytes.byteLength + value.byteLength); next.set(bytes); next.set(value, bytes.byteLength); bytes = next;
            }
          } finally { await reader.cancel(); reader.releaseLock(); }
          const body = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
          if (typeof body?.connection_id !== "string" || Object.keys(body).length !== 1) throw new Error();
          const capabilities: unknown = JSON.parse(request.headers.get("x-nanocodex-capabilities") ?? "[]");
          return this.#remote.renew(body.connection_id, Array.isArray(capabilities) && capabilities.includes("agents:write"), vm);
        } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      }
      return this.#remote.fetch(request, vm);
    }
    if (url.pathname === "/hosted-tool-stats") {
      if (request.method !== "GET" || url.search) return Response.json({ error: "invalid_request" }, { status: 400 });
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !this.#claim(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      const to = Date.now();
      const from = to - 24 * 60 * 60 * 1000;
      // One persisted row per (session_id, source_call_id). Aggregate in SQL;
      // never fetch input, result, receipt, machine or call identities.
      const data = this.ctx.storage.sql.exec<{
        name: string; state: string; calls: number; tool_failed: number;
        tool_ambiguous: number; tool_unavailable: number; tool_failed_other: number;
        cua_cdp_dispatch_deadline: number; cua_js_kernel_timeout: number; late_receipts: number;
        pre_dispatch_unavailable: number; post_dispatch_unavailable: number; unknown_dispatch_unavailable: number;
        duration_count: number;
        total_duration_ms: number | null; avg_duration_ms: number | null;
        min_duration_ms: number | null; max_duration_ms: number | null;
      }>(`SELECT name, state, COUNT(*) AS calls,
          SUM(CASE WHEN state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0 THEN 1 ELSE 0 END) AS tool_failed,
          SUM(CASE WHEN state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0
            AND json_extract(result_json, '$.output.structured_result.status') = 'ambiguous'
            THEN 1 ELSE 0 END) AS tool_ambiguous,
          SUM(CASE WHEN state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0
            AND json_extract(result_json, '$.output.structured_result.status') = 'unavailable'
            THEN 1 ELSE 0 END) AS tool_unavailable,
          SUM(CASE WHEN state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0
            AND COALESCE(json_extract(result_json, '$.output.structured_result.status'), '') NOT IN ('ambiguous', 'unavailable')
            THEN 1 ELSE 0 END) AS tool_failed_other,
          -- Narrow, owner-only retrospective buckets for two observed CUA error strings.
          -- These are subsets of completed unsuccessful calls, not additional calls.
          -- Search only the persisted result and return counts, never message text.
          SUM(CASE WHEN name = 'mcp__cua_repl__js' AND state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0
            AND instr(result_json, 'CDP operation exceeded its deadline before command dispatch') > 0
            THEN 1 ELSE 0 END) AS cua_cdp_dispatch_deadline,
          SUM(CASE WHEN name = 'mcp__cua_repl__js' AND state = 'completed' AND json_valid(result_json)
            AND json_extract(result_json, '$.output.success') = 0
            AND instr(result_json, 'js execution timed out; kernel reset') > 0
            THEN 1 ELSE 0 END) AS cua_js_kernel_timeout,
          SUM(CASE WHEN state = 'ambiguous' AND receipt_json IS NOT NULL THEN 1 ELSE 0 END) AS late_receipts,
          SUM(CASE WHEN state = 'unavailable' AND dispatched_at = 0 THEN 1 ELSE 0 END) AS pre_dispatch_unavailable,
          SUM(CASE WHEN state = 'unavailable' AND dispatched_at > 0 THEN 1 ELSE 0 END) AS post_dispatch_unavailable,
          SUM(CASE WHEN state = 'unavailable' AND dispatched_at IS NULL THEN 1 ELSE 0 END) AS unknown_dispatch_unavailable,
          COUNT(CASE WHEN state IN ('completed', 'unavailable', 'ambiguous', 'cancelled') THEN 1 END) AS duration_count,
          SUM(CASE WHEN state IN ('completed', 'unavailable', 'ambiguous', 'cancelled')
            THEN MAX(0, updated_at - created_at) END) AS total_duration_ms,
          AVG(CASE WHEN state IN ('completed', 'unavailable', 'ambiguous', 'cancelled')
            THEN MAX(0, updated_at - created_at) END) AS avg_duration_ms,
          MIN(CASE WHEN state IN ('completed', 'unavailable', 'ambiguous', 'cancelled')
            THEN MAX(0, updated_at - created_at) END) AS min_duration_ms,
          MAX(CASE WHEN state IN ('completed', 'unavailable', 'ambiguous', 'cancelled')
            THEN MAX(0, updated_at - created_at) END) AS max_duration_ms
        FROM hosted_tool_calls
        WHERE created_at >= ? AND created_at <= ?
        GROUP BY name, state ORDER BY name, state`, from, to).toArray();
      return Response.json({ window: { from, to }, total_calls: data.reduce((sum, row) => sum + row.calls, 0), data },
        { headers: { "cache-control": "no-store" } });
    }
    if (request.method === "GET" && url.pathname === "/tool-host") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !this.#claim(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      const identity = publisherIdentity(request.headers);
      if (identity === false || (this.#regional && !identity)) return Response.json({ error: "invalid_publisher_identity" }, { status: 400 });
      if (this.#regional) {
        const region = request.headers.get(HAND_RELAY_REGION_HEADER);
        if (!isHandRelayRegion(region) || (this.#region && this.#region !== region)) return Response.json({ error: "not_found" }, { status: 404 });
        this.#region = region;
        this.ctx.storage.kv.put("regional_hand_region", region);
      }
      return this.#broker.upgrade(ownerId, undefined, undefined, undefined, undefined, identity || undefined);
    }
    if (request.method === "POST" && url.pathname === "/snapshot") {
      const ownerId = await ownerFromBody(request);
      if (!ownerId || !this.#owns(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      return Response.json(await this.#snapshot(), { headers: { "cache-control": "no-store" } });
    }

    if (request.method === "POST" && url.pathname === "/invoke") {
      const startedAt = performance.now();
      let invocation: InvocationRequest;
      try { invocation = await request.json<InvocationRequest>(); }
      catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      const decodedAt = performance.now();
      if (!isUserId(invocation.owner_id) || !this.#owns(invocation.owner_id)
        || typeof invocation.name !== "string" || typeof invocation.session_id !== "string"
        || typeof invocation.call_id !== "string" || typeof invocation.route_token !== "string") {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      if (invocation.thread_id !== undefined && (typeof invocation.thread_id !== "string"
        || !/^[A-Za-z0-9_./:-]{1,128}$/.test(invocation.thread_id))) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      const correlation = { session_id: invocation.session_id, thread_id: invocation.thread_id,
        source_call_id: invocation.call_id, turn_id: invocation.turn_id };
      const ownedAt = performance.now();
      observeHandCall("account.decode_input", invocation.name, startedAt, "ok", invocation.call_id, correlation, decodedAt);
      observeHandCall("account.ownership", invocation.name, decodedAt, "ok", invocation.call_id, correlation, ownedAt);
      if (invocation.machine_id === undefined && invocation.route_token.startsWith("screen:v1:")) {
        const remote = await traceToolInvocation("hand.account.invoke", invocation.thread_id, invocation.name, {
          sessionId: invocation.session_id, callId: invocation.call_id, turnId: invocation.turn_id,
        }, () => this.#remote.invoke(invocation.name, invocation.route_token,
          invocation.input, invocation.session_id, request.signal,
          { threadId: invocation.thread_id, callId: invocation.call_id, turnId: invocation.turn_id }));
        if (remote) return remote;
      }
      const catalog = this.#broker.catalogSnapshot();
      const machineName = HOSTED_MACHINE_TOOL_NAMES.find((name) => name === invocation.name);
      const tool = invocation.machine_id === undefined
        ? catalog.resolve(invocation.name)
        : machineName === undefined
          ? undefined
          : catalog.machineTool(invocation.machine_id, machineName);
      if (!tool) {
        observeHandCall("account.resolve", invocation.name, ownedAt, "unavailable", invocation.call_id, correlation);
        return Response.json({ error: "tool_unavailable" }, { status: 404 });
      }
      if (tool.routeToken !== invocation.route_token) {
        observeHandCall("account.resolve", invocation.name, ownedAt, "unavailable", invocation.call_id, correlation);
        return Response.json({ error: "stale_catalog" }, { status: 409 });
      }
      // Capture the process owner's route before invoking. Exec can wait while
      // a replacement host publishes, and the caller may have refreshed an old
      // command route before admission. Its original snapshot is insufficient.
      const processRoute = invocation.machine_id !== undefined && invocation.name === "exec_command"
        ? catalog.machineTool(invocation.machine_id, "write_stdin")?.routeToken : undefined;
      const resolvedAt = performance.now();
      observeHandCall("account.resolve", invocation.name, ownedAt, "ok", invocation.call_id, correlation);
      let result;
      try { result = await traceToolInvocation("hand.account.invoke", invocation.thread_id, invocation.name, {
        sessionId: invocation.session_id, callId: invocation.call_id, turnId: invocation.turn_id,
      }, () => tool.handler(invocation.input, {
        sessionId: invocation.session_id,
        ...(invocation.thread_id === undefined ? {} : { threadId: invocation.thread_id }),
        ...(invocation.turn_id === undefined ? {} : { turnId: invocation.turn_id }),
        callId: invocation.call_id,
        model: invocation.model,
        signal: request.signal,
      })); } catch (error) {
        observeHandCall("account.handler", invocation.name, resolvedAt, request.signal.aborted ? "cancelled" : "failed", invocation.call_id, correlation);
        observeHandSummary("hand.call.account", invocation.name, correlation, { input_decode_ms: decodedAt - startedAt, ownership_ms: ownedAt - decodedAt,
          resolve_ms: resolvedAt - ownedAt, handler_ms: performance.now() - resolvedAt, total_ms: performance.now() - startedAt },
          request.signal.aborted ? "cancelled" : "failed");
        throw error;
      }
      const branded = result as Record<PropertyKey, unknown>;
      const failureStatus = (branded.structuredResult as { status?: unknown } | null)?.status;
      observeHandCall("account.handler", invocation.name, resolvedAt, branded.success === true ? "ok"
        : branded[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE] === true ? "unavailable"
        : failureStatus === "ambiguous" ? "ambiguous"
        : failureStatus === "unavailable" ? "unavailable" : failureStatus === "cancelled" ? "cancelled" : "failed", invocation.call_id, correlation);
      observeHandSummary("hand.call.account", invocation.name, correlation, {
        input_decode_ms: decodedAt - startedAt, ownership_ms: ownedAt - decodedAt, resolve_ms: resolvedAt - ownedAt,
        handler_ms: performance.now() - resolvedAt, total_ms: performance.now() - startedAt },
        branded.success === true ? "ok" : failureStatus === "ambiguous" ? "ambiguous"
          : failureStatus === "unavailable" ? "unavailable" : failureStatus === "cancelled" ? "cancelled" : "failed");
      return Response.json({
        output: branded.output,
        structured_result: branded.structuredResult,
        success: branded.success === true,
        metadata: branded.metadata,
        value: branded.value,
        ...(processRoute === undefined ? {} : { process_route_token: processRoute }),
        ...(branded[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE] === true
          ? { pre_admission_unavailable: true as const }
          : {}),
      } satisfies InvocationResult, {
        headers: { "cache-control": "no-store" },
      });
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  #localSnapshot(): AccountHostedToolsSnapshot {
    const catalog = this.#broker.catalogSnapshot();
    return {
        screens: this.#remote.list(true).filter(target => target.agent_tools),
        tools: [...catalog.definitions().flatMap((definition) => {
          const tool = catalog.resolve(definition.name) as RoutedHostedTool | undefined;
          return tool?.routeToken === undefined ? [] : [{
            definition,
            parallel_safe: tool.parallelSafe,
            provider: tool.provider,
            remote_name: tool.remoteName,
            summary: tool.summary,
            timeout_ms: tool.timeoutMs,
            route_token: tool.routeToken,
          } satisfies AccountHostedTool];
        }), ...this.#remote.tools()],
        machines: catalog.machines().map(({ machine, online }) => ({
          machine,
          online,
          tools: HOSTED_MACHINE_TOOL_NAMES.flatMap((name) => {
            const tool = catalog.machineTool(machine.id, name);
            return tool?.routeToken === undefined ? [] : [{
              name,
              parallel_safe: tool.parallelSafe,
              definition: tool.definition,
              route_token: tool.routeToken,
            }];
          }),
        })),
      };
  }

  async #snapshot(): Promise<AccountHostedToolsSnapshot> {
    const local = this.#localSnapshot();
    if (this.#regional) return { ...local, publications: this.ctx.storage.sql.exec<{ publication_json: string }>(
      "SELECT publication_json FROM regional_local_publications WHERE publication_json IS NOT NULL").toArray().map(row => JSON.parse(row.publication_json) as HandPublication) };
    const directory = this.#directory.entries();
    if (!directory.length) return this.#withRoots(local);
    const regions = [...new Set(directory.filter(entry => !entry.pending && entry.region !== "legacy").map(entry => entry.region as HandRelayRegion))];
    const snapshots = await Promise.all(regions.map(async region => {
      try {
        if (!this.env.NANOCODEX_HAND_RELAYS) return undefined;
        const response = await fetchResponseWithDeadline(this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(this.#ownerId!, region)),
          "https://account-tools.internal/snapshot", { method: "POST", headers: { "content-type": "application/json" },
            body: JSON.stringify({ owner_id: this.#ownerId }) }, 5_000, "regional Hand discovery",
          async response => response.ok ? response.json<AccountHostedToolsSnapshot>() : undefined);
        return response ? { region, snapshot: response } : undefined;
      } catch { return undefined; }
    }));
    const inventoryUnknownIds: string[] = [];
    const regionalNames = new Set(directory.filter(entry => entry.region !== "legacy" || entry.pending).flatMap(entry => entry.tool_names));
    const tools = local.tools.filter(tool => !regionalNames.has(tool.definition.name));
    const machines = new Map(local.machines.filter(entry => {
      const selected = directory.find(candidate => candidate.machine.id === entry.machine.id);
      return !selected || (!selected.pending && selected.region === "legacy");
    }).map(entry => [entry.machine.id, entry]));
    for (const selected of directory) {
      if (selected.region === "legacy" && !selected.pending) continue;
      const remote = snapshots.find(snapshot => snapshot?.region === selected.region)?.snapshot;
      if (selected.pending || (selected.region !== "legacy" && !remote)) inventoryUnknownIds.push(selected.machine.id);
      const current = !selected.pending && remote?.publications?.some(publication => publication.machine.id === selected.machine.id
        && publication.publication_id === selected.publication_id);
      const machine = current ? remote?.machines.find(entry => entry.machine.id === selected.machine.id) : undefined;
      const region = selected.region;
      machines.set(selected.machine.id, machine && region !== "legacy" ? { ...machine,
        tools: machine.tools.map(tool => ({ ...tool, route_token: relayRouteToken(region, tool.route_token) })) }
        : { machine: selected.machine, online: false, tools: [] });
      if (current && remote && region !== "legacy") for (const tool of remote.tools) {
        if (selected.tool_names.includes(tool.definition.name)) tools.push({ ...tool, route_token: relayRouteToken(region, tool.route_token) });
      }
    }
    return this.#withRoots({ tools, machines: [...machines.values()], screens: local.screens, inventory_unknown_ids: inventoryUnknownIds });
  }

  #withRoots(snapshot: AccountHostedToolsSnapshot): AccountHostedToolsSnapshot {
    return { ...snapshot, mount_roots: Object.fromEntries(new HandPaths(this.ctx.storage).assign(snapshot.machines.map(entry => entry.machine))) };
  }

  async #admitPublication(candidate: Parameters<NonNullable<import("./hosted-tools-broker").HostedToolsBrokerOptions["beforeCatalogPublish"]>>[0]): Promise<() => boolean> {
    if (!candidate.machine) {
      if (this.#regional) throw new Error("regional publishers require one native Hand");
      const names = new Set(candidate.definitions.map(entry => entry.definition.name));
      if (this.#directory.entries().some(entry => entry.tool_names.some(name => names.has(name)))) {
        throw new Error("tool name is already exposed by an account Hand");
      }
      // A regional claim can arrive while this broker awaits this guard.
      // Recheck immediately before the local catalog commits.
      return () => !this.#directory.entries().some(entry => entry.tool_names.some(name => names.has(name)));
    }
    const publication: HandPublication = { route_id: candidate.routeId, publication_id: crypto.randomUUID(),
      region: this.#regional ? this.#region! : "legacy", machine: candidate.machine,
      tool_names: candidate.definitions.map(entry => entry.definition.name), runtime_id: candidate.runtimeId };
    if (this.#regional && !this.#region) throw new Error("regional identity is missing");
    this.ctx.storage.sql.exec("INSERT INTO regional_local_publications(route_id,candidate_id) VALUES(?,?) ON CONFLICT(route_id) DO UPDATE SET candidate_id=excluded.candidate_id", candidate.routeId, publication.publication_id);
    try {
      if (this.#regional) {
        const response = await fetchResponseWithDeadline(this.env.NANOCODEX_ACCOUNT_TOOLS!.getByName(this.#ownerId!), "https://account-tools.internal/regional/claim", {
          method: "POST", headers: { [OWNER_ASSERTION]: this.#ownerId!, "content-type": "application/json" }, body: JSON.stringify(publication),
        }, 10_000, "Hand publication", async response => response.ok);
        if (!response) throw new Error("account Hand publication rejected");
      } else await this.#queueClaim(publication);
    } catch (error) {
      this.ctx.storage.sql.exec("UPDATE regional_local_publications SET candidate_id=NULL WHERE route_id=? AND candidate_id=?", candidate.routeId, publication.publication_id);
      this.ctx.storage.sql.exec("DELETE FROM regional_local_publications WHERE candidate_id IS NULL AND publication_json IS NULL");
      throw error;
    }
    return () => {
      const local = this.ctx.storage.sql.exec<{ candidate_id: string | null }>("SELECT candidate_id FROM regional_local_publications WHERE route_id=?", candidate.routeId).toArray()[0];
      if (local?.candidate_id !== publication.publication_id) return false;
      this.ctx.storage.sql.exec("UPDATE regional_local_publications SET publication_json=? WHERE route_id=?", JSON.stringify(publication), candidate.routeId);
      return true;
    };
  }

  #queueClaim(publication: HandPublication): Promise<void> {
    const result = this.#publicationQueue.then(async () => {
      // A retained legacy publisher must explicitly drain its catalog before a
      // regional runtime can replace it; transport loss is not a drain. In particular, never send it a terminal policy close.
      if (publication.region !== "legacy" && this.#broker.machines().some(machine => machine.id === publication.machine.id)) {
        throw new Error("legacy Hand requires drain before regional placement");
      }
      if (publication.region !== "legacy") {
        const localNames = new Set(this.#broker.reservedToolNames());
        if (publication.tool_names.some(name => localNames.has(name))) throw new Error("tool name is already exposed by a legacy attachment");
      }
      await this.#directory.claim(publication, async previous => {
        if (previous.region === "legacy") { this.#fencePublication(previous); return; }
        if (!this.env.NANOCODEX_HAND_RELAYS) throw new Error("regional relay unavailable");
        const fenced = await fetchResponseWithDeadline(this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(this.#ownerId!, previous.region)), "https://account-tools.internal/regional/fence", {
          method: "POST", headers: { [OWNER_ASSERTION]: this.#ownerId!, "content-type": "application/json" }, body: JSON.stringify(previous),
        }, 10_000, "Hand publication fence", async response => response.ok);
        if (!fenced) throw new Error("previous Hand publication could not be fenced");
      });
    });
    this.#publicationQueue = result.catch(() => {});
    return result;
  }

  #fencePublication(publication: HandPublication): void {
    const local = this.ctx.storage.sql.exec<{ candidate_id: string | null; publication_json: string | null }>(
      "SELECT candidate_id,publication_json FROM regional_local_publications WHERE route_id=?", publication.route_id).toArray()[0];
    if (local?.candidate_id === publication.publication_id) {
      this.ctx.storage.sql.exec("UPDATE regional_local_publications SET candidate_id=NULL WHERE route_id=?", publication.route_id);
    }
    const active = local?.publication_json ? JSON.parse(local.publication_json) as HandPublication : undefined;
    if (active?.publication_id === publication.publication_id) {
      this.#broker.retireRoute(publication.route_id, "Hand publisher replaced in another region", publication.region === "legacy" ? 1012 : 1008);
      this.ctx.storage.sql.exec("UPDATE regional_local_publications SET publication_json=NULL WHERE route_id=?", publication.route_id);
    }
    this.ctx.storage.sql.exec("DELETE FROM regional_local_publications WHERE candidate_id IS NULL AND publication_json IS NULL");
  }

  async #regionalRequest(request: Request, url: URL): Promise<Response> {
    const owner = request.headers.get(OWNER_ASSERTION);
    if (!isUserId(owner) || !this.#claim(owner)) return Response.json({ error: "not_found" }, { status: 404 });
    if (url.pathname === "/regional/status" && !this.#regional && request.method === "GET" && !url.search) {
      const rows = this.ctx.storage.sql.exec<{ runtime_id: string | null; machines_json: string | null }>(
        "SELECT runtime_id,machines_json FROM hosted_tool_routes WHERE machines_json IS NOT NULL").toArray();
      const legacy = rows.flatMap(row => (JSON.parse(row.machines_json!) as HostedMachine[]).map(machine => {
        const pending = this.ctx.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id=? AND state IN ('admitted','dispatched')", machine.id, row.runtime_id).toArray()[0]!.count;
        const online = this.#broker.machineOnline(machine.id);
        return { machine_id: machine.id, runtime_id: row.runtime_id, online, pending_calls: pending, retirable: !online && pending === 0 && row.runtime_id !== null };
      }));
      return Response.json({ legacy }, { headers: { "cache-control": "no-store" } });
    }
    if (request.method !== "POST" || url.search) return Response.json({ error: "invalid_request" }, { status: 400 });
    let body: Record<string, unknown>;
    try { body = await boundedJSON(request) as Record<string, unknown>; } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
    if (!body || typeof body !== "object" || Array.isArray(body)) return Response.json({ error: "invalid_request" }, { status: 400 });
    if (url.pathname === "/regional/select" && !this.#regional) {
      if (!validPublisherId(body.machine_id) || !validPublisherId(body.runtime_id)
        || (body.region !== "legacy" && !isHandRelayRegion(body.region))) return Response.json({ error: "invalid_request" }, { status: 400 });
      // Upgrade-era legacy sockets predate the directory. Import their exact
      // runtime placement from the broker ledger before considering new regions.
      const states = this.ctx.storage.sql.exec<{ runtime_id: string | null; machines_json: string | null }>("SELECT runtime_id,machines_json FROM hosted_tool_routes").toArray();
      const legacy = states.find(state => state.machines_json && (JSON.parse(state.machines_json) as HostedMachine[]).some(machine => machine.id === body.machine_id));
      if (legacy?.runtime_id && !this.#directory.retired(body.machine_id, legacy.runtime_id)) this.#directory.select(body.machine_id, legacy.runtime_id, "legacy");
      if (this.#directory.retired(body.machine_id, body.runtime_id)) return Response.json({ error: "hand_runtime_superseded" }, { status: 409 });
      const pinned = this.#directory.placement(body.machine_id, body.runtime_id);
      if (!pinned && body.region !== "legacy" && this.#broker.machines().some(machine => machine.id === body.machine_id)) {
        return Response.json({ error: "legacy_hand_requires_drain" }, { status: 409 });
      }
      return Response.json({ region: pinned ?? this.#directory.select(body.machine_id, body.runtime_id, body.region) });
    }
    if (url.pathname === "/regional/retire" && !this.#regional) {
      if (!validPublisherId(body.machine_id) || !validPublisherId(body.runtime_id) || Object.keys(body).length !== 2) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      if (this.#directory.retired(body.machine_id, body.runtime_id)
        && this.#directory.placement(body.machine_id, body.runtime_id) === "legacy") {
        return Response.json({ retired: true, machine_id: body.machine_id, runtime_id: body.runtime_id });
      }
      const rows = this.ctx.storage.sql.exec<{ route_id: string; runtime_id: string | null; machines_json: string | null; lease_id: string | null; generation: number }>(
        "SELECT route_id,runtime_id,machines_json,lease_id,generation FROM hosted_tool_routes").toArray();
      const route = rows.find(row => row.machines_json && (JSON.parse(row.machines_json) as HostedMachine[]).some(machine => machine.id === body.machine_id));
      if (!route || route.runtime_id !== body.runtime_id) {
        return Response.json({ error: "legacy_runtime_not_found" }, { status: 409 });
      }
      if (this.#broker.machineOnline(body.machine_id)) return Response.json({ error: "legacy_runtime_still_connected" }, { status: 409 });
      const pending = this.ctx.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id=? AND state IN ('admitted','dispatched')", body.machine_id, body.runtime_id).toArray()[0]!.count;
      if (pending) return Response.json({ error: "legacy_runtime_has_pending_calls" }, { status: 409 });
      this.#broker.retireRoute(route.route_id, "Owner explicitly retired disconnected legacy runtime", 1012);
      this.#directory.retire(body.machine_id, body.runtime_id);
      return Response.json({ retired: true, machine_id: body.machine_id, runtime_id: body.runtime_id });
    }
    const publication = body as unknown as HandPublication;
    if (!validPublisherId(publication.machine?.id) || !validPublisherId(publication.publication_id)
      || typeof publication.machine.name !== "string" || !Array.isArray(publication.machine.capabilities)
      || typeof publication.route_id !== "string" || publication.route_id.length > 512
      || (publication.region !== "legacy" && !isHandRelayRegion(publication.region))
      || !Array.isArray(publication.tool_names) || publication.tool_names.length > 256
      || publication.tool_names.some(name => typeof name !== "string" || name.length > 256)
      || (publication.runtime_id !== undefined && !validPublisherId(publication.runtime_id))) return Response.json({ error: "invalid_request" }, { status: 400 });
    if (url.pathname === "/regional/fence") {
      this.#fencePublication(publication);
      return Response.json({ fenced: true });
    }
    if (url.pathname === "/regional/claim" && !this.#regional) {
      try { await this.#queueClaim(publication); return Response.json({ admitted: true }); }
      catch { return Response.json({ error: "hand_publication_conflict" }, { status: 409 }); }
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  alarm(): void { this.#broker.expire(); }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (this.#remote.owns(socket)) { this.#remote.message(socket, message); return; }
    await this.#broker.webSocketMessage(socket, message);
  }

  webSocketClose(socket: WebSocket, code: number, reason: string): void {
    if (this.#remote.owns(socket)) { this.#remote.close(socket, undefined, "websocket_closed", code); return; }
    console.warn({ type: "hand.socket.closed", code,
      pending: this.#broker.hasPendingCalls() });
    this.#broker.webSocketClose(socket, code, reason);
  }

  webSocketError(socket: WebSocket): void {
    if (this.#remote.owns(socket)) { this.#remote.close(socket, undefined, "websocket_error"); return; }
    console.warn({ type: "hand.socket.error", pending: this.#broker.hasPendingCalls() });
    this.#broker.webSocketError(socket);
  }

  #claim(ownerId: string): boolean {
    if (this.#ownerId !== undefined) return this.#ownerId === ownerId;
    const retained = this.ctx.storage.transactionSync(() => {
      const retained = this.ctx.storage.kv.get<string>("owner_id");
      if (retained !== undefined) return retained;
      this.ctx.storage.kv.put("owner_id", ownerId);
      return ownerId;
    });
    this.#ownerId = retained;
    return retained === ownerId;
  }

  #owns(ownerId: string): boolean {
    return this.#ownerId === ownerId;
  }
}

/** Caller-owned durable effect routing survives provider refresh and session restart. */
export class AccountHostedToolsCallRoutes {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS account_hand_call_routes (
      session_id TEXT NOT NULL, call_id TEXT NOT NULL, name TEXT NOT NULL,
      machine_id TEXT NOT NULL, route_token TEXT NOT NULL,
      PRIMARY KEY(session_id, call_id)
    )`);
  }
  pin(sessionId: string, callId: string, name: string, machineId: string | undefined, routeToken: string): string {
    this.storage.sql.exec("INSERT OR IGNORE INTO account_hand_call_routes VALUES (?, ?, ?, ?, ?)", sessionId, callId, name, machineId ?? "", routeToken);
    const retained = this.storage.sql.exec<{ name: string; machine_id: string; route_token: string }>(
      "SELECT name,machine_id,route_token FROM account_hand_call_routes WHERE session_id=? AND call_id=?", sessionId, callId).toArray()[0]!;
    if (retained.name !== name || retained.machine_id !== (machineId ?? "")) throw new Error("Hand call identity was reused for another tool or machine");
    return retained.route_token;
  }
}

/** Dynamic provider proxy from one agent DO to its account's shared hand DO. */
export class AccountHostedToolsProvider implements HostedToolsDynamicProvider {
  readonly sourceId = "account-hands";
  readonly #namespace: DurableObjectNamespace<AccountHostedTools>;
  readonly #relays: DurableObjectNamespace<RegionalHandRelay> | undefined;
  readonly #callRoutes: AccountHostedToolsCallRoutes | undefined;
  readonly #ownerId: string;
  readonly #threadId: string | undefined;
  readonly #allowed: (context?: AuthorizationContext) => boolean;
  #definitions: readonly HostedToolsCodeDefinition[] = [];
  #candidates: readonly HostedToolsCatalogCandidate[] = [];
  #machines: readonly HostedMachine[] = [];
  #machineRoots: ReadonlyMap<string, string> = new Map();
  #onlineMachineIds = new Set<string>();
  #tools = new Map<string, RoutedHostedTool>();
  #machineTools = new Map<string, HostedToolsCodeTool>();
  #screenTools = new Map<string, HostedToolsCodeTool>();
  #screenMachines: readonly HostedMachine[] = [];
  #validator: HostedToolsCatalogValidator | undefined;
  #refreshing?: Promise<void>;
  #optionalRetryAt = 0;
  #loadedAt = 0;
  #generation = 0;
  #refreshGeneration = 0;

  constructor(
    namespace: DurableObjectNamespace<AccountHostedTools>,
    ownerId: string,
    allowed: (context?: AuthorizationContext) => boolean,
    threadId?: string,
    relays?: DurableObjectNamespace<RegionalHandRelay>,
    callRoutes?: AccountHostedToolsCallRoutes,
  ) {
    this.#namespace = namespace;
    this.#relays = relays;
    this.#callRoutes = callRoutes;
    this.#ownerId = ownerId;
    this.#threadId = threadId;
    this.#allowed = allowed;
  }

  definitions(): readonly HostedToolsCodeDefinition[] {
    return this.#allowed() ? this.#definitions : [];
  }

  resolve(name: string): HostedToolsCodeTool | undefined {
    const tool = this.#allowed() ? this.#tools.get(name) : undefined;
    return tool?.provider === "screens" ? undefined : tool;
  }

  machines(context?: AuthorizationContext): readonly HostedMachine[] {
    return this.#allowed(context) ? this.#machines : [];
  }

  machineRoots(): ReadonlyMap<string, string> {
    return this.#allowed() ? this.#machineRoots : new Map();
  }

  machineOnline(machineId: string, context?: AuthorizationContext): boolean {
    return this.#allowed(context) && this.#onlineMachineIds.has(machineId);
  }

  machineTool(
    machineId: string,
    name: HostedMachineToolName,
    context?: AuthorizationContext,
  ): HostedToolsCodeTool | undefined {
    // Retained machine catalogs can recover shell/process receipts, but an
    // offline CUA pair must not mask this Hand's currently published screen.
    // Already captured callers retain their original handlers and never switch
    // backend after an input action has been admitted.
    if ((name === CUA_JS_NAME || name === CUA_RESET_NAME) && !this.#onlineMachineIds.has(machineId)) return undefined;
    return this.#allowed(context) ? this.#machineTools.get(machineToolKey(machineId, name)) : undefined;
  }

  screenTool(machineId: string, context?: AuthorizationContext): HostedToolsCodeTool | undefined {
    return this.#allowed(context) ? this.#screenTools.get(machineId) : undefined;
  }

  screenMachines(context?: AuthorizationContext): readonly HostedMachine[] {
    return this.#allowed(context) ? this.#screenMachines : [];
  }

  settled(): Promise<void> {
    // Account inventory is optional. Tool-router readiness must not depend on
    // an account hand being reachable; explicit discovery still uses refresh().
    return Promise.resolve();
  }

  invalidate(options: { clearCatalog?: boolean } = {}): void {
    this.#loadedAt = 0;
    this.#optionalRetryAt = 0;
    this.#generation += 1;
    if (options.clearCatalog) this.#publish({ tools: [], machines: [] });
  }

  /** Demand-driven background refresh; explicit refresh bypasses failure backoff. */
  async refreshOptional(maxAgeMs: number): Promise<void> {
    if (Date.now() < this.#optionalRetryAt) return;
    const generation = this.#generation;
    try {
      await this.refresh(maxAgeMs);
    } catch (error) {
      if (generation === this.#generation) this.#optionalRetryAt = Date.now() + 10_000;
      throw error;
    }
  }

  refresh(maxAgeMs = 0): Promise<void> {
    if (this.#refreshing) return this.#refreshGeneration === this.#generation
      ? this.#refreshing : this.#refreshing.catch(() => {}).then(() => this.refresh(maxAgeMs));
    if (maxAgeMs > 0 && this.#loadedAt > 0 && Date.now() - this.#loadedAt < maxAgeMs) return Promise.resolve();
    const generation = this.#generation;
    this.#refreshGeneration = generation;
    const startedAt = Date.now();
    const refreshing = this.#load(generation).then(() => {
      if (generation === this.#generation) { this.#loadedAt = startedAt; this.#optionalRetryAt = 0; }
    }).finally(() => {
      if (this.#refreshing === refreshing) this.#refreshing = undefined;
    });
    this.#refreshing = refreshing;
    return refreshing;
  }

  setCatalogValidator(validator: HostedToolsCatalogValidator | undefined): void {
    this.#validator = validator;
    if (validator === undefined || this.#definitions.length === 0) return;
    try {
      if (validator(this.#candidates) === true) return;
    } catch { /* Invalid account catalogs fail closed below. */ }
    this.#publish({ tools: [], machines: [] });
  }

  async #load(generation: number): Promise<void> {
    let snapshot: unknown;
    try {
      snapshot = await fetchResponseWithDeadline(
        this.#namespace.getByName(this.#ownerId),
        "https://account-tools.internal/snapshot",
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ owner_id: this.#ownerId }),
        },
        10_000,
        "account hand discovery",
        async (response) => {
          if (response.status === 404) return { tools: [], machines: [] };
          if (!response.ok) throw new Error(`Account hand discovery failed: ${response.status}`);
          return response.json<unknown>();
        },
      );
    } catch (error) {
      throw Object.assign(new Error("Account hand discovery interrupted", { cause: error }), { code: "host_interrupted" });
    }
    if (generation !== this.#generation) return;
    if (!validSnapshot(snapshot)) {
      this.#publish({ tools: [], machines: [] });
      return;
    }
    try {
      if (this.#validator !== undefined && this.#validator(snapshot.tools) !== true) {
        this.#publish({ tools: [], machines: [] });
        return;
      }
    } catch {
      this.#publish({ tools: [], machines: [] });
      return;
    }
    this.#publish(snapshot);
  }

  #publish(snapshot: AccountHostedToolsSnapshot): void {
    this.#machineRoots = new Map(Object.entries(snapshot.mount_roots ?? {}));
    const tools = new Map<string, RoutedHostedTool>();
    for (const entry of snapshot.tools) {
      const definition = entry.definition;
      const tool: RoutedHostedTool = {
        name: definition.name,
        // Keep the admitted contract with the route: native-screen CUA
        // discovery must expose the exact screen schema, not only a callable.
        definition,
        parallelSafe: entry.parallel_safe,
        provider: entry.provider,
        remoteName: entry.remote_name,
        timeoutMs: entry.timeout_ms,
        routeToken: entry.route_token,
        ...(entry.summary === undefined ? {} : { summary: entry.summary }),
        handler: (
          input: unknown,
          context: InvocationContext,
        ) => this.#invoke(
          definition.name,
          entry.route_token,
          input,
          context,
          undefined,
          entry.provider === "screens" ? "screen" : "refresh",
        ),
      };
      tools.set(definition.name, Object.freeze(tool));
    }
    // Screen tools stay behind workdir-scoped CUA discovery; do not expose a
    // second model-facing route that bypasses the selected Hand's contract.
    this.#definitions = Object.freeze(snapshot.tools
      .filter((entry) => entry.provider !== "screens")
      .map((entry) => entry.definition)
      .filter((definition) => tools.has(definition.name)));
    this.#candidates = Object.freeze(snapshot.tools
      .filter((entry) => entry.provider !== "screens" && tools.has(entry.definition.name)));
    const machineTools = new Map<string, HostedToolsCodeTool>();
    for (const entry of snapshot.machines) {
      for (const route of entry.tools) {
        machineTools.set(machineToolKey(entry.machine.id, route.name), Object.freeze({
          name: route.name,
          parallelSafe: route.parallel_safe,
          definition: route.definition,
          routeToken: route.route_token,
          handler: (
            input: unknown,
            context: InvocationContext,
          ) => this.#invoke(route.name, route.route_token, input, context, entry.machine.id,
            route.name === "native_secure_input" ? "fixed" : "refresh"),
        }));
      }
    }
    const screenTools = new Map<string, HostedToolsCodeTool>();
    const screenMachines = new Map<string, HostedMachine>();
    const groups = new Map<string, ScreenTarget[]>();
    for (const target of snapshot.screens ?? []) {
      if (!target.agent_tools) continue;
      const group = groups.get(target.machine_id) ?? [];
      group.push(target);
      groups.set(target.machine_id, group);
    }
    for (const [machineId, targets] of groups) {
      // Prefer the whole desktop; never guess between multiple window surfaces.
      const target = targets.find(target => target.id === "desktop") ?? (targets.length === 1 ? targets[0] : undefined);
      if (!target) continue;
      const expected = screenTool(target);
      const tool = tools.get(expected.definition.name);
      if (!tool || tool.provider !== "screens" || tool.routeToken !== expected.route_token) continue;
      screenTools.set(machineId, tool);
      screenMachines.set(machineId, { id: machineId, name: target.machine_name,
        workspace: "/", capabilities: ["computer", "screen"] });
    }
    this.#machines = Object.freeze(snapshot.machines.map(({ machine, online }) => {
      const upstream = online === true && machineTools.has(machineToolKey(machine.id, CUA_JS_NAME))
        && machineTools.has(machineToolKey(machine.id, CUA_RESET_NAME));
      return { ...machine, capabilities: [...new Set([...machine.capabilities,
        ...(upstream ? ["computer"] : []), ...(screenTools.has(machine.id) ? ["computer", "screen"] : [])])] };
    }));
    this.#screenMachines = Object.freeze([...screenMachines.values()]);
    this.#onlineMachineIds = new Set(snapshot.machines
      .filter(({ online }) => online === true)
      .map(({ machine }) => machine.id));
    // A live screen cannot make an offline shell/VM factory look online.
    for (const id of screenTools.keys()) {
      if (!snapshot.machines.some(entry => entry.machine.id === id)) this.#onlineMachineIds.add(id);
    }
    this.#tools = tools;
    this.#machineTools = machineTools;
    this.#screenTools = screenTools;
  }

  async #invoke(
    name: string,
    routeToken: string,
    input: unknown,
    context: InvocationContext,
    machineId?: string,
    routePolicy: "refresh" | "fixed" | "screen" = "refresh",
  ): Promise<unknown> {
    const started = performance.now();
    const timing: { fetch_ms?: number; decode_ms?: number } = {};
    let outcome: "ok" | "failed" | "unavailable" | "ambiguous" | "cancelled" = "failed";
    try {
      const result = await traceToolInvocation("hand.provider.invoke", this.#threadId, name, context,
        () => this.#invokeAccount(name, routeToken, input, context, machineId, routePolicy, timing));
      const branded = result as Record<PropertyKey, unknown>;
      const status = (branded.structuredResult as { status?: unknown } | null)?.status;
      outcome = branded.success === true ? "ok" : status === "ambiguous" ? "ambiguous"
        : status === "unavailable" ? "unavailable" : status === "cancelled" ? "cancelled" : "failed";
      return result;
    } finally {
      observeHandSummary("hand.call.provider", name, { session_id: context.sessionId,
        thread_id: this.#threadId, source_call_id: context.callId, turn_id: context.turnId },
        { ...timing, total_ms: performance.now() - started }, outcome === "failed" && context.signal?.aborted ? "cancelled" : outcome);
    }
  }

  async #invokeAccount(
    name: string,
    routeToken: string,
    input: unknown,
    context: InvocationContext,
    machineId?: string,
    routePolicy: "refresh" | "fixed" | "screen" = "refresh",
    timing: { fetch_ms?: number; decode_ms?: number } = {},
  ): Promise<unknown> {
    const correlation = { session_id: context.sessionId, thread_id: this.#threadId, turn_id: context.turnId };
    const failed = (message: string, status: "ambiguous" | "unavailable", preAdmission = false): unknown => {
      observeHandCall("account.fetch", name, startedAt, status, context.callId, correlation);
      return failedToolResult(message, status, preAdmission);
    };
    const startedAt = performance.now();
    if (!this.#allowed(context)) {
      return failed("Account hand is outside the active grant", "unavailable", true);
    }
    if (routeToken.startsWith("hand-relay:") && !parseRelayRouteToken(routeToken)) return failed("Invalid regional Hand route", "unavailable", true);
    if (this.#callRoutes) {
      try { routeToken = this.#callRoutes.pin(context.sessionId, context.callId, name, machineId, routeToken); }
      catch { return failed("Hand call identity conflicts with its retained route", "ambiguous"); }
    }
    const relay = parseRelayRouteToken(routeToken);
    if (routeToken.startsWith("hand-relay:") && !relay) return failed("Invalid regional Hand route", "unavailable", true);
    if (relay && !this.#relays) return failed("Regional Hand relay is unavailable", "unavailable", true);
    const target = relay ? this.#relays!.getByName(handRelayName(this.#ownerId, relay.region)) : this.#namespace.getByName(this.#ownerId);
    let response: Response;
    try {
      response = await target.fetch("https://account-tools.internal/invoke", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          owner_id: this.#ownerId,
          name,
          input,
          session_id: context.sessionId,
          ...(this.#threadId === undefined ? {} : { thread_id: this.#threadId }),
          ...(context.turnId === undefined ? {} : { turn_id: context.turnId }),
          call_id: context.callId,
          model: context.model,
          ...(machineId === undefined ? {} : { machine_id: machineId }),
          route_token: relay?.token ?? routeToken,
        } satisfies InvocationRequest),
        signal: context.signal,
      });
    } catch {
      timing.fetch_ms = performance.now() - startedAt;
      return failed("Hand connection failed after possible dispatch; execution outcome is unknown. The command was not resent.", "ambiguous");
    }
    const responseAt = performance.now();
    timing.fetch_ms = responseAt - startedAt;
    if (response.ok) observeHandCall("account.fetch", name, startedAt, "ok", context.callId, correlation);
    if (!response.ok) {
      try { await response.body?.cancel(); } catch { /* No call was admitted for 404/409. */ }
      const preAdmission = response.status === 404 || response.status === 409;
      if (preAdmission && routePolicy === "screen") {
        // Screen routes fence one exact publication generation. Redirecting a
        // stale click to a replacement publisher would turn a safe routing
        // rejection into a new side effect on a different desktop.
        return failed(
          "The selected screen was disconnected or replaced before this action began. Observe the current screen before sending input.",
          "unavailable",
          true,
        );
      }
      if (preAdmission && !this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        // Only an explicit routing rejection permits local reconciliation. Keep
        // the original effect identity so the broker replays any prior receipt;
        // transport/decoding failures and server errors never trigger a retry.
        this.invalidate();
        try { await this.refresh(); }
        catch {
          return failed("Hand route refresh failed; execution outcome is unknown. The command was not resent.", "ambiguous");
        }
        // Personal/MCP tools use exposed names; shell tools use machine keys.
        const route = machineId === undefined
          ? this.#tools.get(name)
          : this.#machineTools.get(machineToolKey(machineId, name as HostedMachineToolName));
        // Receipt ledgers are shard-local. Even a routing rejection must never
        // move an existing effect identity to a different shard.
        if (route?.routeToken && route.routeToken !== routeToken
          && parseRelayRouteToken(route.routeToken)?.region === relay?.region) {
          return this.#invoke(name, route.routeToken, input, context, machineId, "fixed");
        }
      }
      if (machineId !== undefined && name === "write_stdin" && response.status === 409) {
        // A modern process route is stable across transport reconnects. A
        // changed token means a different runtime (or a legacy host without
        // continuity proof), whose numeric process IDs may have been reused.
        return failed("The Hand process runtime changed or cannot prove session continuity. This saved process session cannot be routed to the replacement; any earlier poll or stdin outcome remains unknown. The command and stdin were not resent.", "ambiguous");
      }
      // HTTP status alone cannot exclude an earlier dispatch of this call ID.
      // Preserve uncertainty locally instead of interrupting the agent runtime.
      return failed(`Hand request failed (HTTP ${response.status}); execution outcome is unknown. The command was not resent after possible dispatch.`, "ambiguous");
    }
    let result: InvocationResult;
    try {
      result = await response.json<InvocationResult>();
      if (!result || typeof result !== "object" || typeof result.success !== "boolean"
        || !Object.hasOwn(result, "output") || !Object.hasOwn(result, "structured_result")
        || !Object.hasOwn(result, "metadata") || !Object.hasOwn(result, "value")) {
        throw new Error("invalid account hand result");
      }
    } catch {
      observeHandCall("account.decode", name, responseAt, "ambiguous", context.callId, correlation);
      return failed("Hand response could not be decoded; execution outcome is unknown. The command was not resent.", "ambiguous");
    } finally {
      timing.decode_ms = performance.now() - responseAt;
    }
    if (relay && result.process_route_token) result = { ...result, process_route_token: relayRouteToken(relay.region, result.process_route_token) };
    const structuredStatus = result.structured_result && typeof result.structured_result === "object"
      ? (result.structured_result as { status?: unknown }).status : undefined;
    observeHandCall("account.decode", name, responseAt, result.pre_admission_unavailable === true ? "unavailable"
      : result.success ? "ok" : structuredStatus === "ambiguous" ? "ambiguous"
      : structuredStatus === "unavailable" ? "unavailable" : structuredStatus === "cancelled" ? "cancelled" : "failed", context.callId, correlation);
    if (machineId !== undefined && result.pre_admission_unavailable === true) {
      // The broker checked its call ledger: this invocation was never admitted.
      // Let the agent recover the hand instead of indefinitely replaying the turn.
      // Do not infer this from discovery or HTTP errors: an earlier attempt may
      // have been admitted and must retain its identity for receipt recovery.
      const reason = typeof result.output === "string" ? result.output : "hand unavailable";
      return failedToolResult(
        `Account hand ${machineId} did not start tool execution: ${reason}. Reconnect or restart this hand, or select another available hand.`,
        "unavailable",
        true,
      );
    }
    const branded = {
      [TOOL_RESULT]: true,
      output: result.output,
      structuredResult: result.structured_result,
      success: result.success,
      metadata: result.metadata,
      value: result.value,
      ...(machineId !== undefined && name === "exec_command" && typeof result.process_route_token === "string"
        ? { [PROCESS_SESSION_TOOL]: Object.freeze({
          handler: (input: unknown, context: InvocationContext) =>
            this.#invoke("write_stdin", result.process_route_token!, input, context, machineId, "fixed"),
        }) } : {}),
      ...(result.pre_admission_unavailable === true
        ? { [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true as const }
        : {}),
    };
    return Object.freeze(branded);
  }
}

function machineToolKey(machineId: string, name: HostedMachineToolName): string {
  return `${machineId}\u0000${name}`;
}

function validSnapshot(snapshot: unknown): snapshot is AccountHostedToolsSnapshot {
  if (!snapshot || typeof snapshot !== "object") return false;
  const candidate = snapshot as Partial<AccountHostedToolsSnapshot>;
  if (!Array.isArray(candidate.tools) || !Array.isArray(candidate.machines)) return false;
  if (candidate.screens !== undefined && (!Array.isArray(candidate.screens) || candidate.screens.some(target =>
    !target || typeof target.machine_id !== "string" || typeof target.machine_name !== "string"
    || typeof target.id !== "string" || typeof target.name !== "string" || typeof target.kind !== "string"
    || typeof target.generation !== "string" || typeof target.controllable !== "boolean"
    || (target.recording !== undefined && !validRecordingCapability(target.recording))
    || (target.recordingCapabilities !== undefined && (!target.recordingCapabilities || typeof target.recordingCapabilities !== "object"
      || !validRecordingCapability(target.recordingCapabilities) || target.recordingCapabilities.available !== target.recording))
    || typeof target.agent_tools !== "boolean" || !Number.isSafeInteger(target.width) || target.width < 1
    || !Number.isSafeInteger(target.height) || target.height < 1))) return false;
  const toolNames = new Set<string>();
  for (const entry of candidate.tools) {
    if (!entry || typeof entry !== "object" || typeof entry.route_token !== "string"
      || !entry.definition || typeof entry.definition.name !== "string"
      || toolNames.has(entry.definition.name)) return false;
    toolNames.add(entry.definition.name);
  }
  const machineIds = new Set<string>();
  for (const entry of candidate.machines) {
    if (!entry || typeof entry !== "object" || !entry.machine
      || typeof entry.machine.id !== "string" || machineIds.has(entry.machine.id)
      || !Array.isArray(entry.tools)) return false;
    machineIds.add(entry.machine.id);
    const names = new Set<HostedMachineToolName>();
    for (const tool of entry.tools) {
      if (!HOSTED_MACHINE_TOOL_NAMES.includes(tool?.name)
        || names.has(tool.name)
        || typeof tool.parallel_safe !== "boolean"
        || typeof tool.route_token !== "string") return false;
      names.add(tool.name);
    }
  }
  return true;
}

async function ownerFromBody(request: Request): Promise<string | undefined> {
  try {
    const body = await request.json<{ owner_id?: unknown }>();
    return isUserId(body.owner_id) ? body.owner_id : undefined;
  } catch {
    return undefined;
  }
}

function failedToolResult(
  message: string,
  status: "unavailable" | "ambiguous",
  preAdmissionUnavailable = false,
): unknown {
  const outcome = { status, message };
  return Object.freeze({
    [TOOL_RESULT]: true,
    output: message,
    structuredResult: outcome,
    success: false,
    metadata: null,
    value: outcome,
    ...(preAdmissionUnavailable ? { [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true as const } : {}),
  });
}
