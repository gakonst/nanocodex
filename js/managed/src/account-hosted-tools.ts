import { DurableObject } from "cloudflare:workers";
import { HandRemoteBroker, REMOTE_VM_ASSERTION, type RemoteVMPublisher } from "./hand-remote";
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
import { fetchResponseWithDeadline } from "./deadline";
import { HostedToolsBroker } from "./hosted-tools-broker";

const OWNER_ASSERTION = "x-nanocodex-owner-id";
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");

type AccountHostedTool = HostedToolsCatalogCandidate & Readonly<{
  route_token: string;
}>;

type AccountHostedMachine = Readonly<{
  online: boolean;
  machine: HostedMachine;
  tools: readonly Readonly<{
    name: HostedMachineToolName;
    parallel_safe: boolean;
    route_token: string;
  }>[];
}>;

type AccountHostedToolsSnapshot = Readonly<{
  tools: readonly AccountHostedTool[];
  machines: readonly AccountHostedMachine[];
}>;

type RoutedHostedTool = HostedToolsCodeTool & Readonly<{
  provider: string;
  remoteName: string;
  summary?: string;
  timeoutMs: number;
}>;

type AccountHostedToolsEnv = RemoteICEEnv;

type InvocationRequest = Readonly<{
  owner_id: string;
  name: string;
  input: unknown;
  session_id: string;
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
}>;

type InvocationContext = Readonly<{
  sessionId: string;
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

  constructor(ctx: DurableObjectState, env: AccountHostedToolsEnv) {
    super(ctx, env);
    this.#broker = new HostedToolsBroker(ctx, { resumeRetainedSockets: true });
    this.#remote = new HandRemoteBroker(ctx);
    this.#handHosts = new HandHosts(ctx.storage, this.#remote);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const sandboxHost = url.pathname.match(/^\/sandbox-hand-hosts\/([^/]+)$/);
    if (sandboxHost) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (url.search || !isUserId(ownerId) || !await this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
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
      if (url.search || !isUserId(ownerId) || !await this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      return this.#handHosts.setupLock(request, setup[1]!);
    }
    if (url.pathname === "/hand-hosts" || url.pathname.startsWith("/hand-hosts/")) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      const publisher = url.pathname.match(/^\/hand-hosts\/([^/]+)\/hands\/(host|ice|renew)$/);
      if (publisher) {
        if (url.search || !await this.#owns(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
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
      if (!management || !await this.#claim(ownerId)) return Response.json({ error: "not_found" }, { status: 404 });
      return this.#handHosts.manage(request, management[1]);
    }
    if (url.pathname === "/hands" || url.pathname.startsWith("/hands/")) {
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !await this.#claim(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      let vm: RemoteVMPublisher | undefined;
      const encodedVM = request.headers.get(REMOTE_VM_ASSERTION);
      if (encodedVM !== null) {
        try {
          vm = JSON.parse(encodedVM);
          if (!vm || typeof vm.machineId !== "string" || typeof vm.routeId !== "string"
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
    if (request.method === "GET" && url.pathname === "/tool-host") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const ownerId = request.headers.get(OWNER_ASSERTION);
      if (!isUserId(ownerId) || !await this.#claim(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      return this.#broker.upgrade(ownerId);
    }
    if (request.method === "POST" && url.pathname === "/snapshot") {
      const ownerId = await ownerFromBody(request);
      if (!ownerId || !await this.#owns(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      const provider = this.#broker.provider();
      return Response.json({
        tools: [...provider.definitions().flatMap((definition) => {
          const tool = provider.resolve(definition.name) as RoutedHostedTool | undefined;
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
        machines: this.#broker.machines().map((machine) => ({
          machine,
          online: this.#broker.machineOnline(machine.id),
          tools: HOSTED_MACHINE_TOOL_NAMES.flatMap((name) => {
            const tool = this.#broker.machineTool(machine.id, name);
            return tool?.routeToken === undefined ? [] : [{
              name,
              parallel_safe: tool.parallelSafe,
              route_token: tool.routeToken,
            }];
          }),
        })),
      } satisfies AccountHostedToolsSnapshot, {
        headers: { "cache-control": "no-store" },
      });
    }
    if (request.method === "POST" && url.pathname === "/invoke") {
      let invocation: InvocationRequest;
      try { invocation = await request.json<InvocationRequest>(); }
      catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      if (!isUserId(invocation.owner_id) || !await this.#owns(invocation.owner_id)
        || typeof invocation.name !== "string" || typeof invocation.session_id !== "string"
        || typeof invocation.call_id !== "string" || typeof invocation.route_token !== "string") {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      if (invocation.machine_id === undefined) {
        const remote = await this.#remote.invoke(invocation.name, invocation.route_token,
          invocation.input, invocation.session_id, request.signal);
        if (remote) return remote;
      }
      const machineName = HOSTED_MACHINE_TOOL_NAMES.find((name) => name === invocation.name);
      const tool = invocation.machine_id === undefined
        ? this.#broker.provider().resolve(invocation.name)
        : machineName === undefined
          ? undefined
          : this.#broker.machineTool(invocation.machine_id, machineName);
      if (!tool) return Response.json({ error: "tool_unavailable" }, { status: 404 });
      if (tool.routeToken !== invocation.route_token) {
        return Response.json({ error: "stale_catalog" }, { status: 409 });
      }
      const result = await tool.handler(invocation.input, {
        sessionId: invocation.session_id,
        callId: invocation.call_id,
        model: invocation.model,
        signal: request.signal,
      });
      const branded = result as Record<PropertyKey, unknown>;
      return Response.json({
        output: branded.output,
        structured_result: branded.structuredResult,
        success: branded.success === true,
        metadata: branded.metadata,
        value: branded.value,
        ...(branded[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE] === true
          ? { pre_admission_unavailable: true as const }
          : {}),
      } satisfies InvocationResult, {
        headers: { "cache-control": "no-store" },
      });
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  alarm(): void { this.#broker.expire(); }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (this.#remote.owns(socket)) { this.#remote.message(socket, message); return; }
    await this.#broker.webSocketMessage(socket, message);
  }

  webSocketClose(socket: WebSocket, code: number, reason: string): void {
    if (this.#remote.owns(socket)) { this.#remote.close(socket); return; }
    this.#broker.webSocketClose(socket, code, reason);
  }

  webSocketError(socket: WebSocket): void {
    if (this.#remote.owns(socket)) { this.#remote.close(socket); return; }
    this.#broker.webSocketError(socket);
  }

  async #claim(ownerId: string): Promise<boolean> {
    const retained = await this.ctx.storage.get<string>("owner_id");
    if (retained !== undefined) return retained === ownerId;
    await this.ctx.storage.put("owner_id", ownerId);
    return true;
  }

  async #owns(ownerId: string): Promise<boolean> {
    return await this.ctx.storage.get<string>("owner_id") === ownerId;
  }
}

/** Dynamic provider proxy from one agent DO to its account's shared hand DO. */
export class AccountHostedToolsProvider implements HostedToolsDynamicProvider {
  readonly sourceId = "account-hands";
  readonly #namespace: DurableObjectNamespace<AccountHostedTools>;
  readonly #ownerId: string;
  readonly #allowed: (context?: AuthorizationContext) => boolean;
  #definitions: readonly HostedToolsCodeDefinition[] = [];
  #candidates: readonly HostedToolsCatalogCandidate[] = [];
  #machines: readonly HostedMachine[] = [];
  #tools = new Map<string, RoutedHostedTool>();
  #machineTools = new Map<string, HostedToolsCodeTool>();
  #validator: HostedToolsCatalogValidator | undefined;
  #refreshing?: Promise<void>;
  #loaded = false;

  constructor(
    namespace: DurableObjectNamespace<AccountHostedTools>,
    ownerId: string,
    allowed: (context?: AuthorizationContext) => boolean,
  ) {
    this.#namespace = namespace;
    this.#ownerId = ownerId;
    this.#allowed = allowed;
  }

  definitions(): readonly HostedToolsCodeDefinition[] {
    return this.#allowed() ? this.#definitions : [];
  }

  resolve(name: string): HostedToolsCodeTool | undefined {
    return this.#allowed() ? this.#tools.get(name) : undefined;
  }

  machines(context?: AuthorizationContext): readonly HostedMachine[] {
    return this.#allowed(context) ? this.#machines : [];
  }

  machineTool(
    machineId: string,
    name: HostedMachineToolName,
    context?: AuthorizationContext,
  ): HostedToolsCodeTool | undefined {
    return this.#allowed(context) ? this.#machineTools.get(machineToolKey(machineId, name)) : undefined;
  }

  settled(): Promise<void> {
    return this.#loaded ? Promise.resolve() : this.refresh();
  }

  refresh(): Promise<void> {
    if (this.#refreshing) return this.#refreshing;
    const refreshing = this.#load().then(() => { this.#loaded = true; }).finally(() => {
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

  async #load(): Promise<void> {
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
    const tools = new Map<string, RoutedHostedTool>();
    for (const entry of snapshot.tools) {
      const definition = entry.definition;
      const tool: RoutedHostedTool = {
        name: definition.name,
        parallelSafe: entry.parallel_safe,
        provider: entry.provider,
        remoteName: entry.remote_name,
        timeoutMs: entry.timeout_ms,
        routeToken: entry.route_token,
        ...(entry.summary === undefined ? {} : { summary: entry.summary }),
        handler: (
          input: unknown,
          context: InvocationContext,
        ) => this.#invoke(definition.name, entry.route_token, input, context),
      };
      tools.set(definition.name, Object.freeze(tool));
    }
    this.#definitions = Object.freeze(snapshot.tools
      .map((entry) => entry.definition)
      .filter((definition) => tools.has(definition.name)));
    this.#candidates = Object.freeze(snapshot.tools
      .filter((entry) => tools.has(entry.definition.name)));
    const machineTools = new Map<string, HostedToolsCodeTool>();
    for (const entry of snapshot.machines) {
      for (const route of entry.tools) {
        machineTools.set(machineToolKey(entry.machine.id, route.name), Object.freeze({
          name: route.name,
          parallelSafe: route.parallel_safe,
          routeToken: route.route_token,
          handler: (
            input: unknown,
            context: InvocationContext,
          ) => this.#invoke(route.name, route.route_token, input, context, entry.machine.id),
        }));
      }
    }
    this.#machines = Object.freeze(snapshot.machines.map(({ machine }) => machine));
    this.#tools = tools;
    this.#machineTools = machineTools;
  }

  async #invoke(
    name: string,
    routeToken: string,
    input: unknown,
    context: InvocationContext,
    machineId?: string,
  ): Promise<unknown> {
    if (!this.#allowed(context)) {
      return failedToolResult("Account hand is outside the active grant", "unavailable", true);
    }
    let response: Response;
    try {
      response = await this.#namespace.getByName(this.#ownerId).fetch("https://account-tools.internal/invoke", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          owner_id: this.#ownerId,
          name,
          input,
          session_id: context.sessionId,
          call_id: context.callId,
          model: context.model,
          ...(machineId === undefined ? {} : { machine_id: machineId }),
          route_token: routeToken,
        } satisfies InvocationRequest),
        signal: context.signal,
      });
    } catch (error) {
      if (machineId !== undefined) throw Object.assign(new Error("Account hand transport interrupted", { cause: error }), {
        code: "host_interrupted",
      });
      return failedToolResult("Account hand invocation outcome is unknown", "ambiguous");
    }
    if (!response.ok) {
      try { await response.body?.cancel(); } catch { /* No call was admitted for 404/409. */ }
      const preAdmission = response.status === 404 || response.status === 409;
      if (machineId !== undefined && (preAdmission || response.status >= 500)) {
        // No tool result exists. The Rust owner retains this effect and reopens
        // the runtime; its existing session/call identity resolves any receipt.
        throw Object.assign(new Error("Account hand is not ready"), { code: "host_interrupted" });
      }
      return failedToolResult("Account hand is unavailable", "unavailable", preAdmission);
    }
    try {
      const result = await response.json<InvocationResult>();
      if (!result || typeof result !== "object" || typeof result.success !== "boolean"
        || !Object.hasOwn(result, "output") || !Object.hasOwn(result, "structured_result")
        || !Object.hasOwn(result, "metadata") || !Object.hasOwn(result, "value")) {
        throw new Error("invalid account hand result");
      }
      if (machineId !== undefined && result.pre_admission_unavailable === true) {
        throw Object.assign(new Error("Account hand is reconnecting"), { code: "host_interrupted" });
      }
      const branded = {
        [TOOL_RESULT]: true,
        output: result.output,
        structuredResult: result.structured_result,
        success: result.success,
        metadata: result.metadata,
        value: result.value,
        ...(result.pre_admission_unavailable === true
          ? { [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true as const }
          : {}),
      };
      return Object.freeze(branded);
    } catch (error) {
      if (machineId !== undefined) throw Object.assign(new Error("Account hand transport interrupted", { cause: error }), {
        code: "host_interrupted",
      });
      return failedToolResult("Account hand invocation outcome is unknown", "ambiguous");
    }
  }
}

function machineToolKey(machineId: string, name: HostedMachineToolName): string {
  return `${machineId}\u0000${name}`;
}

function validSnapshot(snapshot: unknown): snapshot is AccountHostedToolsSnapshot {
  if (!snapshot || typeof snapshot !== "object") return false;
  const candidate = snapshot as Partial<AccountHostedToolsSnapshot>;
  if (!Array.isArray(candidate.tools) || !Array.isArray(candidate.machines)) return false;
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
