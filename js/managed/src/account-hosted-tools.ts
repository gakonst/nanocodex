import { DurableObject } from "cloudflare:workers";
import { CUA_JS_NAME, CUA_RESET_NAME } from "nanocodex-computer/contract";
import { HandPaths, type HandRegistry } from "./hand-paths";
import { HandShareStore } from "./hand-share-store";
import { HandRemoteBroker, REMOTE_VM_ASSERTION, type RemoteVMPublisher } from "./hand-remote";
import { validRecordingCapability, screenTool, type ScreenTarget } from "./hand-remote-agent";
import { HandHosts, boundedJSON } from "./hand-hosts";
import { remoteICE, type RemoteICEEnv } from "./hand-remote-ice";
import {
  parseHostedToolsManagedFrame,
  hostedToolsAmbiguous,
  hostedToolsUnavailable,
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
import { inventoryEntry, mergeInventory, HAND_INVENTORY_DEADLINE_MS, type HandInventoryEntry, type HandInventory } from "./hand-inventory";
import { HostedToolsBroker, R2HostedToolsResultArchive } from "./hosted-tools-broker";
import { observeHandCall, observeHandSummary } from "./hand-call-observation";
import { annotateToolSpan, traceToolInvocation } from "./tool-tracing";
import { DiagnosticJournal, diagnosticScope } from "./diagnostic-journal";
import { HandDirectory, publisherIdentity, validPublisherId, type HandEnv, type HandPublication } from "./hand-directory";
import { ScreenAuthority, screenAuthorized, type ScreenFenceReason } from "./screen-authority";
import { recordScreenPlaybackHostResult, type ScreenPlaybackEnv } from "./screen-playback";
import { ScreenCallLedger } from "./screen-call-ledger";

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

type SharedMachineSnapshot = AccountHostedMachine & {
  shared_screens: readonly ScreenTarget[];
  shared_screen_tools: readonly AccountHostedTool[];
};

type AccountHostedToolsSnapshot = Readonly<{
  tools: readonly AccountHostedTool[];
  machines: readonly AccountHostedMachine[];
  screens?: readonly ScreenTarget[];
  mount_roots?: Readonly<Record<string, string>>;
  /** Historical roots of the same identities; never projected as Hands. */
  mount_aliases?: Readonly<Record<string, readonly string[]>>;
  /**
   * Every identity the owner's registry still holds, assigned a root or not.
   * Present only on complete owner views; never merged from a selected-Hand lookup.
   */
  mount_registry?: Readonly<{ ids: readonly string[]; observed_at: number }>;
  inventory_unknown_ids?: readonly string[];
  /**
   * A selected lookup that did not resolve screens (shell-only callers). The
   * caller keeps its previously published screens for that machine.
   */
  screens_omitted?: true;
}>;

type RoutedHostedTool = HostedToolsCodeTool & Readonly<{
  provider: string;
  remoteName: string;
  summary?: string;
  timeoutMs: number;
}>;

type AccountHostedToolsEnv = RemoteICEEnv & HandEnv & Partial<ScreenPlaybackEnv> & {
  NANOCODEX_ACCOUNT_TOOLS?: DurableObjectNamespace<AccountHostedTools>;
  NANOCODEX_SESSIONS?: DurableObjectNamespace<import("./index").DurableAgentSession>;
  /** Existing history bucket; holds complete Hand outcomes too large for a ledger row. */
  NANOCODEX_HISTORY?: R2Bucket;
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
  readonly #shares: HandShareStore;
  readonly #sharedScreens = new Map<string, AbortController>();
  /**
   * Explicit cancellation for in-flight invocations, keyed by exact source
   * identity. HTTP transport loss (request.signal) never cancels a Hand call;
   * only /cancel-invocation aborts these controllers.
   */
  readonly #invocations = new Map<string, { controller: AbortController; refs: number }>();
  /** Durable receipts for admitted screen actions; never redispatched. */
  readonly #screenCalls: ScreenCallLedger;
  readonly #broker: HostedToolsBroker;
  readonly #remote: HandRemoteBroker;
  readonly #handHosts: HandHosts;
  readonly #diagnostics: DiagnosticJournal;
  #ownerId: string | undefined;
  readonly #directory: HandDirectory;
  #handPathsValue?: HandPaths;
  /** One instance per object keeps reclamation ordered against this instance's assignments. */
  get #handPaths(): HandPaths { return this.#handPathsValue ??= new HandPaths(this.ctx.storage); }
  #publicationQueue: Promise<unknown> = Promise.resolve();
  /** Which generation may publish each machine's screens. */
  readonly #screens: ScreenAuthority;

  constructor(ctx: DurableObjectState, env: AccountHostedToolsEnv) {
    super(ctx, env);
    this.#shares = new HandShareStore(ctx.storage);
    this.#directory = new HandDirectory(ctx.storage);
    // Durable pre-admission fences: a cancelled or receipt-abandoned source call
    // identity can never be admitted later, even if its /invoke arrives late.
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS hosted_tool_call_fences (
      session_id TEXT NOT NULL, call_id TEXT NOT NULL, created_at INTEGER NOT NULL,
      PRIMARY KEY(session_id, call_id)
    )`);
    this.#screenCalls = new ScreenCallLedger(ctx.storage.sql);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_local_publications (
      route_id TEXT PRIMARY KEY, candidate_id TEXT, publication_json TEXT
    )`);
    // Regional relays are gone: drop relay-only state; every Hand publishes here.
    ctx.storage.kv.delete("regional_hand_region");
    ctx.storage.kv.delete("relay_sockets_retired_v1");
    // Thread-local tool hosts are not account Hands. Retire their derived index,
    // including overflow state, without changing any native routes or sessions.
    ctx.storage.sql.exec("DROP TABLE IF EXISTS workspace_hand_inventory");
    ctx.storage.kv.delete("workspace_hand_inventory_overflow");
    // Ownership is immutable; a new instance reloads it after eviction/restart.
    this.#ownerId = ctx.storage.kv.get<string>("owner_id");
    this.#diagnostics = new DiagnosticJournal(ctx.storage, "hand.broker");
    this.#broker = new HostedToolsBroker(ctx, { resumeRetainedSockets: true,
      ...(env.NANOCODEX_HISTORY
        ? { resultArchive: new R2HostedToolsResultArchive(env.NANOCODEX_HISTORY, ctx.id.toString()) } : {}),
      // Observed living-Hand reconnects take 2-7s (one 5s connect timeout plus
      // retry). New calls wait for that exact runtime epoch instead of telling
      // the model to ask the user for a reconnect. Admitted calls never wait here.
      reconnectAdmissionWaitMs: HAND_RECONNECT_ADMISSION_WAIT_MS,
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
    this.#screens = new ScreenAuthority(ctx.storage, (machineId, keep, reason) => {
      this.#remote.fenceMachine(machineId, keep, reason);
      return this.#screenSequence(0);
    });
    this.#remote = new HandRemoteBroker(ctx, {
      onObservation: observation => {
        const record = { type: "hand.remote", ...observation };
        try { console.info(record); } catch { /* Remote diagnostics cannot change a socket outcome. */ }
        this.#diagnostics.record(record);
      },
      claimCatalog: claim => this.#screens.claim(claim.machineId, claim.generation, claim.sequence),
      nextSequence: () => this.#screenSequence(1),
      onHostResult: result => {
        const playback = this.env.NANOCODEX_SCREEN_PLAYBACK, owner = this.#ownerId;
        if (!playback || !owner) return;
        // Status only; never awaited by the host socket.
        void recordScreenPlaybackHostResult({ NANOCODEX_SCREEN_PLAYBACK: playback }, result, owner).catch(() => false);
      },
      // A host result for a call this instance lost (eviction) settles only its exact retained identity.
      onLateResult: late => { this.#screenCalls.late(late); },
      idPrefix: () => "",
    });
    this.#handHosts = new HandHosts(ctx.storage, this.#remote);
  }

  async createHandShare(ownerId: string, machineId: string) {
    if (!isUserId(ownerId) || !this.#claim(ownerId) || typeof machineId !== "string"
      || machineId.startsWith("shared:")) return { error: "not_found" } as const;
    const snapshot = await this.#ownedSnapshot(machineId);
    if (!snapshot.machines.some(entry => entry.machine.id === machineId)
      && !snapshot.screens?.some(screen => screen.machine_id === machineId)) return { error: "not_found" } as const;
    if (this.#shares.list().filter(share => share.revoked_at === null).length >= 100) return { error: "share_limit" } as const;
    const share = await this.#shares.create(machineId);
    return share ? { id: share.id, token: share.token, machine_id: share.machine_id } : { error: "share_limit" } as const;
  }

  async listHandShares(ownerId: string) {
    return isUserId(ownerId) && this.#owns(ownerId) ? this.#shares.list() : [];
  }

  async revokeHandShare(ownerId: string, id: string): Promise<boolean> {
    return isUserId(ownerId) && this.#owns(ownerId) && this.#shares.revoke(id);
  }

  async redeemHandShare(recipientId: string, ownerId: string, token: string) {
    if (!isUserId(recipientId) || !isUserId(ownerId) || recipientId === ownerId
      || !this.#claim(recipientId) || !this.env.NANOCODEX_ACCOUNT_TOOLS) return { error: "not_found" } as const;
    const share = await this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(ownerId).acceptHandShare(ownerId, recipientId, token);
    if (!share) return { error: "not_found" } as const;
    if (!this.#shares.received().some(entry => entry.id === share.id) && !this.#shares.canReceive()) {
      await Promise.all(this.#shares.received().map(async received => {
        try {
          const active = await withHardDeadline<boolean>("shared Hand grant check", 5000, async () =>
            await this.env.NANOCODEX_ACCOUNT_TOOLS!.getByName(received.owner_id).hasHandShare(received.owner_id, recipientId, received.id));
          if (!active) this.#shares.forgetReceived(received.id);
        } catch { /* Keep references when the owner cannot confirm revocation. */ }
      }));
    }
    if (!this.#shares.receive(share.id, ownerId)) return { error: "share_limit" } as const;
    return { machine_id: `shared:${share.id}` };
  }

  /** Internal account-to-account RPC: membership is authoritative only on the owner. */
  async acceptHandShare(ownerId: string, recipientId: string, token: string) {
    if (!isUserId(ownerId) || !this.#owns(ownerId) || !isUserId(recipientId)
      || recipientId === ownerId) return undefined;
    return this.#shares.redeem(recipientId, token);
  }

  async hasHandShare(ownerId: string, recipientId: string, id: string): Promise<boolean> {
    return this.#owns(ownerId) && !!this.#shares.grant(id, recipientId);
  }

  async sharedHandSnapshot(ownerId: string, recipientId: string, id: string): Promise<SharedMachineSnapshot | undefined> {
    if (!this.#owns(ownerId) || !isUserId(recipientId)) return undefined;
    const grant = this.#shares.grant(id, recipientId);
    if (!grant) return undefined;
    const snapshot = await this.#ownedSnapshot(grant.machine_id);
    // Recheck after asynchronous discovery; revocation may have interleaved.
    if (!this.#shares.grant(id, recipientId)) return undefined;
    const alias = `shared:${id}`;
    const screens = (snapshot.screens ?? []).filter(target => target.machine_id === grant.machine_id).flatMap(target => {
      const exposed = { ...target, machine_id: alias, machine_name: `${target.machine_name} (shared)` };
      const original = screenTool(target);
      const published = snapshot.tools.find(tool => tool.provider === "screens"
        && tool.definition.name === original.definition.name
        && tool.route_token === original.route_token);
      return published ? [{ ...exposed, generation: this.#shares.route(id, screenTool(exposed).definition.name, published.route_token) }] : [];
    });
    const selected = snapshot.machines.find(entry => entry.machine.id === grant.machine_id);
    if (!selected && !screens.length) return undefined;
    return { online: selected?.online ?? true,
      machine: selected ? { ...selected.machine, id: alias, name: `${selected.machine.name} (shared)`,
        capabilities: selected.machine.capabilities.filter(value => !/vm|factory|secure_input/i.test(value)) }
        : { id: alias, name: screens[0]!.machine_name, workspace: "/", capabilities: ["computer", "screen"] },
      tools: (selected?.tools ?? []).filter(tool => sharedMachineTool(tool.name)).map(tool => ({ ...tool,
        route_token: this.#shares.route(id, tool.name, tool.route_token) })),
      shared_screens: screens, shared_screen_tools: screens.map(screenTool) };
  }

  async #invokeSharedHand(ownerId: string, recipientId: string, invocation: InvocationRequest, signal: AbortSignal): Promise<Response> {
    const unavailable = () => Response.json({ error: "tool_unavailable" }, { status: 404 });
    if (!this.#owns(ownerId) || !isUserId(recipientId)) return unavailable();
    const sharedSession = await sharedHandSession(recipientId, invocation.session_id);
    // Track the owner-side identity from arrival, so a concurrent receipt read
    // waits for this call instead of fencing it while discovery is awaited.
    const key = JSON.stringify([sharedSession, invocation.call_id]);
    let tracked = this.#invocations.get(key);
    if (!tracked) { tracked = { controller: new AbortController(), refs: 0 }; this.#invocations.set(key, tracked); }
    tracked.refs++;
    try { return await this.#invokeSharedRoute(ownerId, recipientId, invocation, sharedSession, signal); }
    finally { if (--tracked.refs === 0 && this.#invocations.get(key) === tracked) this.#invocations.delete(key); }
  }

  async #invokeSharedRoute(ownerId: string, recipientId: string, invocation: InvocationRequest, sharedSession: string, signal: AbortSignal): Promise<Response> {
    const unavailable = () => Response.json({ error: "tool_unavailable" }, { status: 404 });
    const route = this.#shares.resolve(invocation.route_token);
    const grant = route && this.#shares.grant(route.share_id, recipientId);
    if (!route || !grant || invocation.machine_id !== `shared:${grant.id}` || route.name !== invocation.name
      ) return unavailable();
    const screenRoute = route.route_token;
    if (screenRoute.startsWith("screen:v1:")) {
      const snapshot = await this.#ownedSnapshot(grant.machine_id);
      const published = snapshot.tools.find(tool => tool.provider === "screens" && tool.route_token === route.route_token);
      if (!published || !this.#shares.grant(grant.id, recipientId)) return unavailable();
      const forwarded = new Request("https://account-tools.internal/invoke", { method: "POST", signal,
        headers: { "content-type": "application/json" }, body: JSON.stringify({ ...invocation,
          owner_id: ownerId, machine_id: undefined, name: published.definition.name,
          route_token: screenRoute, session_id: sharedSession }) });
      return this.fetch(forwarded);
    }
    if (!sharedMachineTool(route.name)) return unavailable();
    if (route.route_token.startsWith("hand-relay:")) return unavailable();
    // The stored route, never recipient input, selects the machine and tool.
    const forwarded = new Request("https://account-tools.internal/invoke", { method: "POST",
      signal, headers: { "content-type": "application/json" }, body: JSON.stringify({ ...invocation,
        owner_id: ownerId, machine_id: grant.machine_id, route_token: route.route_token,
        session_id: sharedSession }) });
    const response = await this.fetch(forwarded);
    if (!response.ok) return response;
    const result = await response.json<InvocationResult>();
    return Response.json({ ...result, ...(result.process_route_token === undefined ? {} : {
      process_route_token: this.#shares.route(grant.id, "write_stdin", result.process_route_token),
    }) }, { headers: { "cache-control": "no-store" } });
  }

  /**
   * Explicit cancellation of a recipient's call on the owner. A revoked grant
   * may still cancel (it only reduces effects). Returns the owner's ledger
   * cancellation state, which the recipient reports unchanged.
   */
  async cancelSharedHand(ownerId: string, recipientId: string, invocation: InvocationRequest): Promise<string> {
    if (!this.#owns(ownerId) || !isUserId(recipientId)) return "unconfirmed";
    const route = this.#shares.resolve(invocation.route_token);
    const grant = route && this.#shares.grant(route.share_id, recipientId, true);
    if (!route || !grant || invocation.machine_id !== `shared:${grant.id}` || invocation.name !== route.name) return "unconfirmed";
    const session = await sharedHandSession(recipientId, invocation.session_id);
    // /cancel-invocation reads the ledger state before it aborts the in-flight call.
    const response = await this.fetch(new Request("https://account-tools.internal/cancel-invocation", {
      method:"POST",headers:{"content-type":"application/json"},
      body:JSON.stringify({owner_id:ownerId,session_id:session,call_id:invocation.call_id,route_token:route.route_token}),
    }));
    const value = response.ok ? await response.json<{ cancel?: unknown }>().catch(() => undefined) : undefined;
    return typeof value?.cancel === "string" && /^[a-z_]{1,32}$/.test(value.cancel) ? value.cancel : "unconfirmed";
  }

  /**
   * Receipt-only read of a recipient's shared call on the owner. Requires the
   * active (non-revoked) grant before and after every await; revocation
   * withholds the result. Never resolves a handler, admits or dispatches.
   */
  async #sharedHandReceipt(ownerId: string, recipientId: string, invocation: InvocationRequest, waitMs: number): Promise<Response> {
    const headers = { "cache-control": "no-store" };
    const unauthorized = () => Response.json({ error: "receipt_unauthorized" }, { status: 403, headers });
    if (!this.#owns(ownerId) || !isUserId(recipientId) || typeof invocation.route_token !== "string") return unauthorized();
    const route = this.#shares.resolve(invocation.route_token);
    const grant = route && this.#shares.grant(route.share_id, recipientId);
    if (!route || !grant || invocation.machine_id !== `shared:${grant.id}` || route.name !== invocation.name) return unauthorized();
    const sharedSession = await sharedHandSession(recipientId, invocation.session_id);
    if (!this.#shares.grant(grant.id, recipientId)) return unauthorized();
    let response: Response;
    if (route.route_token.startsWith("screen:v1:")) {
      // The stored owner route identifies the screen; the exposed alias name differs.
      response = await this.#screenReceipt({ ...invocation, owner_id: ownerId, machine_id: undefined,
        route_token: route.route_token, session_id: sharedSession }, waitMs, false);
    } else {
      if (!sharedMachineTool(route.name) || route.route_token.startsWith("hand-relay:")) return unauthorized();
      response = await this.#invocationReceipt({ ...invocation, owner_id: ownerId, machine_id: grant.machine_id,
        route_token: route.route_token, session_id: sharedSession }, waitMs);
    }
    // Revocation during the bounded wait withholds any result.
    if (!this.#shares.grant(grant.id, recipientId)) { try { await response.body?.cancel(); } catch { /* Discarded. */ } return unauthorized(); }
    if (response.status !== 200) return response;
    const value = await response.json<InvocationResult & { receipt?: unknown }>();
    if (!this.#shares.grant(grant.id, recipientId)) return unauthorized();
    if (value.receipt === "running") return Response.json(value, { headers });
    return Response.json({ ...value, ...(typeof value.process_route_token !== "string" ? {} : {
      process_route_token: this.#shares.route(grant.id, "write_stdin", value.process_route_token),
    }) }, { headers });
  }

  /** Discovery returns only its public projection in one RPC reply. */
  async listMachines(ownerId: string) {
    if (!isUserId(ownerId) || !this.#owns(ownerId)) return [];
    const snapshot = await this.#snapshot();
    const roots = new Map(Object.entries(snapshot.mount_roots ?? {}));
    return snapshot.machines.filter(entry => entry.online && roots.has(entry.machine.id))
      .map(({ machine }) => ({ id: machine.id, name: machine.name, capabilities: machine.capabilities, workspace: roots.get(machine.id)! }));
  }

  async handInventory(ownerId: string): Promise<HandInventory> {
    if (!isUserId(ownerId) || !this.#claim(ownerId)) return mergeInventory([], false);
    try {
      const snapshot = await withHardDeadline("account Hand inventory", HAND_INVENTORY_DEADLINE_MS,
        () => this.#snapshot());
      const unknown = new Set(snapshot.inventory_unknown_ids ?? []);
      return mergeInventory([snapshot.machines.map(({ machine, online }) =>
        inventoryEntry(machine, unknown.has(machine.id) ? null : online))], unknown.size === 0);
    } catch {
      // Preserve account identities when discovery itself failed.
      const local = this.#localSnapshot().machines.map(({ machine }) => inventoryEntry(machine, null));
      const retained = this.#directory.entries().map(({ machine }) => inventoryEntry(machine, null));
      return mergeInventory([local, retained], false);
    }
  }

  /**
   * Owner-initiated removal of one Hand from the routed catalog and from
   * Hand routing. A Hand the owner no longer controls must still be
   * evictable, so this never waits on the device; `force` is the caller's
   * acknowledgement that a live Hand is about to lose its account routing.
   */
  async forgetMachine(ownerId: string, machineId: string, force: boolean) {
    if (!isUserId(ownerId) || !this.#claim(ownerId)) return { error: "not_found" } as const;
    // Serialize with publication admission. A publisher must not reappear
    // between presence inspection and the account's removal fence.
    const result = this.#publicationQueue.then(async () => {
      const snapshot = await this.#snapshot(machineId);
      if (!force && snapshot.inventory_unknown_ids?.includes(machineId)) return { error: "hand_unknown" } as const;
      if (!force && snapshot.machines.some(entry => entry.machine.id === machineId && entry.online)) {
        return { error: "hand_online" } as const;
      }
      const selected = this.#directory.entries().find(entry => entry.machine.id === machineId);
      if (selected?.pending && !force) return { error: "hand_unknown" } as const;
      // Withdraw screen authority.
      const screens = await this.#screens.revoke(machineId);
      if (!screens) console.warn({ type: "hand.screen.revoke_pending" });
      return { forgotten: this.#forget(machineId) } as const;
    });
    this.#publicationQueue = result.then(() => {}, () => {});
    return result;
  }

  /** Bulk eviction of Hands observed definitively offline; unknown stays put. */
  async pruneMachines(ownerId: string) {
    if (!isUserId(ownerId) || !this.#claim(ownerId)) return { error: "not_found" } as const;
    const inventory = await this.handInventory(ownerId);
    const forgotten: string[] = [];
    let complete = inventory.complete;
    for (const entry of inventory.data) {
      if (entry.online !== false) continue;
      const result = await this.forgetMachine(ownerId, entry.id, false);
      if ("forgotten" in result && result.forgotten) forgotten.push(entry.id);
      if ("error" in result && result.error === "hand_unknown") complete = false;
    }
    return { forgotten, complete } as const;
  }

  #forget(machineId: string): boolean {
    return this.ctx.storage.transactionSync(() => {
      let removed = this.#directory.entries().some(entry => entry.machine.id === machineId);
      for (const row of this.ctx.storage.sql.exec<{ route_id: string; machines_json: string }>(
        "SELECT route_id,machines_json FROM hosted_tool_routes WHERE machines_json IS NOT NULL").toArray()) {
        const machines = JSON.parse(row.machines_json) as HostedMachine[];
        const kept = machines.filter(machine => machine.id !== machineId);
        if (kept.length === machines.length) continue;
        removed = true;
        if (kept.length === 0) {
          // Retire the publication, allowing a newly enrolled runtime to register.
          // Directory runtime tombstones reject the forgotten publishers.
          this.#broker.retireRoute(row.route_id, "Owner forgot this Hand");
          this.ctx.storage.sql.exec("DELETE FROM regional_local_publications WHERE route_id=?", row.route_id);
        } else {
          this.ctx.storage.sql.exec("UPDATE hosted_tool_routes SET machines_json=? WHERE route_id=?",
            JSON.stringify(kept), row.route_id);
        }
      }
      this.#shares.revokeMachine(machineId);
      this.#directory.forget(machineId);
      // Only this owner deletion releases the identity's current and historical paths.
      this.#handPaths.forget(machineId);
      return removed;
    });
  }

  async fetch(request: Request): Promise<Response> {
    return diagnosticScope(this.#diagnostics, () => this.#fetchRequest(request));
  }

  async #fetchRequest(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/regional/")) return this.#regionalRequest(request, url);
    if (url.pathname === "/screens/host-command") return this.#screenHostCommand(request);
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
      if (url.pathname === "/hands/screens" && request.method === "GET" && !url.search && !vm) {
        // Only the current authority is listed.
        const authority = this.#screens.hosts();
        const surfaces = this.#remote.list().filter(target => screenAuthorized(authority, target.machine_id, target.generation));
        return Response.json({ surfaces }, { headers: { "cache-control": "no-store" } });
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
      if (identity === false) return Response.json({ error: "invalid_publisher_identity" }, { status: 400 });
      // A runtime superseded by a newer runtime of the same machine never republishes.
      if (identity && this.#directory.retired(identity.machineId, identity.runtimeId)) {
        return Response.json({ error: "hand_runtime_superseded" }, { status: 409 });
      }
      return this.#broker.upgrade(ownerId, undefined, undefined, undefined, undefined, identity || undefined);
    }
    if (request.method === "POST" && url.pathname === "/snapshot") {
      const body = await request.json<{ owner_id?: unknown; machine_id?: unknown; screens?: unknown }>();
      const ownerId = body.owner_id;
      if (!isUserId(ownerId) || !this.#owns(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      if (body.machine_id !== undefined && (typeof body.machine_id !== "string" || !body.machine_id || body.machine_id.length > 256)) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      if (body.screens !== undefined && (body.screens !== false || body.machine_id === undefined)) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      return Response.json(await this.#snapshot(body.machine_id as string | undefined, body.screens !== false),
        { headers: { "cache-control": "no-store" } });
    }

    if (request.method === "POST" && url.pathname === "/turn-ended") {
      const body = await request.json<{ owner_id: string; frame: unknown }>();
      if (!isUserId(body.owner_id) || !this.#owns(body.owner_id)) return Response.json({ error: "not_found" }, { status: 404 });
      let frame;
      try { frame = parseHostedToolsManagedFrame(JSON.stringify(body.frame)); }
      catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      if (frame.type !== "turn_ended") return Response.json({ error: "invalid_request" }, { status: 400 });
      await this.#broker.endTurn(frame.session_id, frame.turn_id, frame.hook_event_name);
      await Promise.all(this.#shares.turnTargets(frame.session_id, frame.turn_id).map(async target => {
        // Retained targets from removed regional relays have nothing left to clean up.
        if (target.region !== "account") return;
        const stub = this.env.NANOCODEX_ACCOUNT_TOOLS?.getByName(target.owner_id);
        if (!stub) throw new Error("Shared Hand cleanup target unavailable");
        const response = await stub.fetch("https://account-tools.internal/turn-ended", {
          method: "POST", headers: {"content-type":"application/json"},
          body: JSON.stringify({owner_id:target.owner_id,frame:{...frame,session_id:target.remote_session_id}}),
        });
        if (!response.ok) throw new Error("Shared Hand cleanup failed");
      }));
      this.#shares.clearTurn(frame.session_id, frame.turn_id);
      return new Response(null, { status: 204 });
    }

    if (request.method === "POST" && url.pathname === "/cancel-invocation") {
      let body = await request.json<InvocationRequest>();
      if (!isUserId(body.owner_id) || !this.#owns(body.owner_id) || typeof body.session_id !== "string" || typeof body.call_id !== "string") {
        return Response.json({error:"not_found"},{status:404});
      }
      const screen = typeof body.route_token === "string" ? parseSharedScreenRoute(body.route_token) : undefined;
      if (screen) body = { ...body, machine_id: screen.machineId, route_token: screen.routeToken };
      let cancel: "forwarded" | "unconfirmed" | "requested" | "queued" | "not_delivered" | "not_dispatched" | "terminal" | "fenced" = "forwarded";
      if (body.machine_id?.startsWith("shared:")) {
        const share = this.#shares.received().find(entry => body.machine_id === `shared:${entry.id}`);
        if (share && this.env.NANOCODEX_ACCOUNT_TOOLS) {
          const owner = await this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(share.owner_id).cancelSharedHand(share.owner_id, body.owner_id, body);
          if (typeof owner === "string") cancel = owner as typeof cancel;
        }
      } else {
        // Screen actions keep their own ledger; read it before the abort settles them.
        const screen = this.#screenCalls.cancelState(body.session_id, body.call_id);
        this.#sharedScreens.get(JSON.stringify([body.session_id, body.call_id]))?.abort();
        this.#invocations.get(JSON.stringify([body.session_id, body.call_id]))?.controller.abort();
        const row = screen ? undefined : this.ctx.storage.sql.exec<{call_id:string;state:string}>("SELECT call_id,state FROM hosted_tool_calls WHERE session_id=? AND source_call_id=?", body.session_id, body.call_id).toArray()[0];
        // Admitted calls are cancelled through their ledger row; an identity
        // not yet admitted is fenced so a late /invoke can never dispatch it.
        if (screen) cancel = screen;
        else if (row) {
          const delivery = row.state === "dispatched" ? this.#broker.cancelDelivery(row.call_id) : undefined;
          cancel = row.state === "admitted" ? "not_dispatched" : row.state !== "dispatched" ? "terminal"
            : delivery === "sent" ? "requested" : delivery === "queued" ? "queued" : "not_delivered";
        }
        else {
          this.#fenceCall(body.session_id, body.call_id);
          // Older screen publishers had no durable call ledger. Absence does
          // not prove an action never ran, even though the fence blocks a late send.
          cancel = body.route_token?.startsWith("screen:v1:") ? "not_delivered" : "fenced";
        }
      }
      return Response.json({ cancel }, { headers: { "cache-control": "no-store" } });
    }
    if (request.method === "POST" && url.pathname === "/shared-invoke") {
      const body = await request.json<{owner_id:string;recipient_id:string;invocation:InvocationRequest}>();
      return this.#invokeSharedHand(body.owner_id, body.recipient_id, body.invocation, request.signal);
    }
    if (request.method === "POST" && url.pathname === "/shared-receipt") {
      let body: { owner_id?: unknown; recipient_id?: unknown; invocation?: InvocationRequest; wait_ms?: unknown };
      try { body = await request.json(); } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      if (typeof body.owner_id !== "string" || typeof body.recipient_id !== "string" || !body.invocation || typeof body.invocation !== "object"
        || typeof body.invocation.session_id !== "string" || typeof body.invocation.call_id !== "string" || typeof body.invocation.name !== "string") {
        return Response.json({ error: "receipt_unauthorized" }, { status: 403 });
      }
      return this.#sharedHandReceipt(body.owner_id, body.recipient_id, body.invocation,
        typeof body.wait_ms === "number" && Number.isFinite(body.wait_ms) ? body.wait_ms : 0);
    }
    if (request.method === "POST" && url.pathname === "/invoke-receipt") {
      let body: InvocationRequest & { wait_ms?: unknown };
      try { body = await request.json<InvocationRequest & { wait_ms?: unknown }>(); }
      catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      if (!isUserId(body.owner_id) || !this.#owns(body.owner_id) || typeof body.name !== "string"
        || typeof body.session_id !== "string" || typeof body.call_id !== "string" || typeof body.route_token !== "string"
        || (body.machine_id !== undefined && typeof body.machine_id !== "string")) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      return this.#invocationReceipt(body, typeof body.wait_ms === "number" && Number.isFinite(body.wait_ms) ? body.wait_ms : 0);
    }
    if (request.method === "POST" && url.pathname === "/invoke") {
      const startedAt = performance.now();
      let invocation: InvocationRequest;
      try { invocation = await request.json<InvocationRequest>(); }
      catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
      const decodedAt = performance.now();
      if (!isUserId(invocation.owner_id) || !this.#owns(invocation.owner_id)
        || typeof invocation.name !== "string" || typeof invocation.session_id !== "string"
        || typeof invocation.call_id !== "string" || typeof invocation.route_token !== "string"
        || (invocation.machine_id !== undefined && typeof invocation.machine_id !== "string")) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      if (invocation.thread_id !== undefined && (typeof invocation.thread_id !== "string"
        || !/^[A-Za-z0-9_./:-]{1,128}$/.test(invocation.thread_id))) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      const sharedScreen = parseSharedScreenRoute(invocation.route_token);
      if (sharedScreen) invocation = { ...invocation, machine_id: sharedScreen.machineId, route_token: sharedScreen.routeToken };
      if (invocation.route_token.startsWith("shared:") || invocation.machine_id?.startsWith("shared:")) {
        const share = this.#shares.received().find(entry => invocation.machine_id === `shared:${entry.id}`);
        if (!share || !this.env.NANOCODEX_ACCOUNT_TOOLS) {
          return Response.json({ error: "tool_unavailable" }, { status: 404 });
        }
        this.#shares.turnTarget(invocation.session_id, invocation.turn_id, share.owner_id, "account",
          await sharedHandSession(invocation.owner_id, invocation.session_id));
        // Transport loss on either hop never cancels the owner's call; only
        // /cancel-invocation does. A dropped inner hop is reported as such so
        // the caller reconciles the original identity's receipt instead.
        try {
          return await this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(share.owner_id).fetch("https://account-tools.internal/shared-invoke", {
            method:"POST", headers:{"content-type":"application/json"},
            body:JSON.stringify({owner_id:share.owner_id,recipient_id:invocation.owner_id,invocation}),
          });
        } catch (error) {
          try { console.info({ type: "hand.shared.transport_lost", tool: invocation.name, session_id: invocation.session_id,
            source_call_id: invocation.call_id, turn_id: invocation.turn_id, error: sanitizedHandError(error) }); }
          catch { /* Diagnostics never change the outcome. */ }
          return Response.json({ error: "shared_transport_lost", reconcile: "receipt" }, { status: 502, headers: { "cache-control": "no-store" } });
        }
      }
      const invocationKey = JSON.stringify([invocation.session_id, invocation.call_id]);
      let tracked = this.#invocations.get(invocationKey);
      if (!tracked) { tracked = { controller: new AbortController(), refs: 0 }; this.#invocations.set(invocationKey, tracked); }
      tracked.refs++;
      if (this.#callFenced(invocation.session_id, invocation.call_id)) tracked.controller.abort();
      const explicitCancel = tracked.controller.signal;
      try {
      const correlation = { session_id: invocation.session_id, thread_id: invocation.thread_id,
        source_call_id: invocation.call_id, turn_id: invocation.turn_id };
      const ownedAt = performance.now();
      observeHandCall("account.decode_input", invocation.name, startedAt, "ok", invocation.call_id, correlation, decodedAt);
      observeHandCall("account.ownership", invocation.name, decodedAt, "ok", invocation.call_id, correlation, ownedAt);
      if (invocation.machine_id === undefined && invocation.route_token.startsWith("screen:v1:")) {
        const key = JSON.stringify([invocation.session_id, invocation.call_id]);
        const inputDigest = await screenInputDigest(invocation.input);
        // An admitted screen identity is never sent again: replay its receipt.
        if (this.#screenCalls.has(invocation.session_id, invocation.call_id)) return this.#screenReplay(invocation, inputDigest);
        const identity = { sessionId: invocation.session_id, callId: invocation.call_id, name: invocation.name,
          routeToken: invocation.route_token, inputDigest };
        const controller = new AbortController(); this.#sharedScreens.set(key, controller);
        let admittedHere = false;
        try {
          const remote = await traceToolInvocation("hand.account.invoke", invocation.thread_id, invocation.name, {
            sessionId: invocation.session_id, callId: invocation.call_id, turnId: invocation.turn_id,
          }, () => this.#remote.invoke(invocation.name, invocation.route_token,
            invocation.input, invocation.session_id, AbortSignal.any([explicitCancel, controller.signal]),
            { threadId: invocation.thread_id, callId: invocation.call_id, turnId: invocation.turn_id, ledger: {
              admit: call => {
                const admission = this.#screenCalls.admit(identity, call, () => this.#callFenced(invocation.session_id, invocation.call_id));
                admittedHere = admission === "admitted";
                return admission;
              },
              settle: (requestId, result) => this.#screenCalls.settle(requestId, result),
            } }));
          if (remote && !admittedHere && this.#screenCalls.has(invocation.session_id, invocation.call_id)) {
            // A concurrent request admitted this identity first (this one may
            // have seen it as busy or duplicate): replay that call's receipt.
            try { await remote.body?.cancel(); } catch { /* Replaced by the retained receipt. */ }
            return this.#screenReplay(invocation, inputDigest);
          }
          if (remote) return remote;
        } finally { this.#sharedScreens.delete(key); }
      }
      const catalog = this.#broker.catalogSnapshot();
      const machineName = HOSTED_MACHINE_TOOL_NAMES.find((name) => name === invocation.name);
      const tool = invocation.machine_id === undefined
        ? catalog.resolve(invocation.name)
        : machineName === undefined
          ? undefined
          : catalog.machineTool(invocation.machine_id, machineName);
      // Rejections carry this shard's ledger evidence. Call rows are never
      // deleted and (session_id, source_call_id) is unique, so "none" proves
      // this call identity was never admitted here; callers may then repin it.
      const admitted = !tool || tool.routeToken !== invocation.route_token
        ? this.ctx.storage.sql.exec<{ name: string; hand_id: string | null }>(
          "SELECT name,hand_id FROM hosted_tool_calls WHERE session_id=? AND source_call_id=?",
          invocation.session_id, invocation.call_id).toArray()[0]
        : undefined;
      // Ledger replay is limited to commands retained for this exact physical
      // machine. Process polls/stdin and CUA stay on their original runtime route.
      const replayRetained = tool !== undefined && admitted !== undefined
        && invocation.machine_id !== undefined && invocation.name === "exec_command"
        && admitted.name === "exec_command" && admitted.hand_id === invocation.machine_id;
      if (!tool) {
        observeHandCall("account.resolve", invocation.name, ownedAt, "unavailable", invocation.call_id, correlation);
        return Response.json({ error: "tool_unavailable", admission: admitted ? "retained" : "none" }, { status: 404 });
      }
      if (tool.routeToken !== invocation.route_token && !replayRetained) {
        observeHandCall("account.resolve", invocation.name, ownedAt, "unavailable", invocation.call_id, correlation);
        return Response.json({ error: "stale_catalog", admission: admitted ? "retained" : "none" }, { status: 409 });
      }
      // A retained call under a stale route resolves only through the broker
      // ledger: its row pins the original lease/generation, so the current
      // binding can replay a receipt or report ambiguity but never redispatch.
      // Capture the process owner's route before invoking. Exec can wait while
      // a replacement host publishes, and the caller may have refreshed an old
      // command route before admission. Its original snapshot is insufficient.
      // A retained receipt replayed through a replacement route never gains the
      // replacement's process route: its process ownership stays with the
      // original runtime (the broker reports such receipts as ambiguous).
      const processRoute = invocation.machine_id !== undefined && invocation.name === "exec_command" && !replayRetained
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
        // Only explicit /cancel-invocation cancels. A lost managed->account
        // connection leaves the call running so its receipt can be reconciled.
        signal: explicitCancel,
      })); } catch (error) {
        observeHandCall("account.handler", invocation.name, resolvedAt, explicitCancel.aborted ? "cancelled" : "failed", invocation.call_id, correlation);
        observeHandSummary("hand.call.account", invocation.name, correlation, { input_decode_ms: decodedAt - startedAt, ownership_ms: ownedAt - decodedAt,
          resolve_ms: resolvedAt - ownedAt, handler_ms: performance.now() - resolvedAt, total_ms: performance.now() - startedAt },
          explicitCancel.aborted ? "cancelled" : "failed");
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
      } finally {
        if (--tracked.refs === 0 && this.#invocations.get(invocationKey) === tracked) this.#invocations.delete(invocationKey);
      }
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  #callFenced(sessionId: string, callId: string): boolean {
    return this.ctx.storage.sql.exec("SELECT 1 FROM hosted_tool_call_fences WHERE session_id=? AND call_id=?", sessionId, callId).toArray().length > 0;
  }

  /**
   * Fences are written only for identities with no call row and, like call
   * rows, are never evicted: eviction would void the "can never run" proof.
   */
  #fenceCall(sessionId: string, callId: string): void {
    this.ctx.storage.sql.exec("INSERT OR IGNORE INTO hosted_tool_call_fences VALUES (?, ?, ?)", sessionId, callId, Date.now());
  }

  /**
   * Receipt-only reconciliation after managed->account transport or decode loss.
   * Never resolves a tool handler, admits, dispatches, cancels or repins: it
   * reads the original call row and waits (bounded) only on its original
   * runtime. A missing row is fenced first, which makes "none" proof that this
   * identity can never run here.
   */
  async #invocationReceipt(body: InvocationRequest, requestedWaitMs: number): Promise<Response> {
    const headers = { "cache-control": "no-store" };
    const sharedScreen = parseSharedScreenRoute(body.route_token);
    if (sharedScreen) body = { ...body, machine_id: sharedScreen.machineId, route_token: sharedScreen.routeToken };
    // Shared Hands keep their ledgers on the owner, which rechecks the grant.
    if (body.route_token.startsWith("shared:") || body.machine_id?.startsWith("shared:")) {
      const share = this.#shares.received().find(entry => body.machine_id === `shared:${entry.id}`);
      if (!share || !this.env.NANOCODEX_ACCOUNT_TOOLS) return Response.json({ error: "receipt_unauthorized" }, { status: 403, headers });
      // A failed hop throws to the caller, which retries the read (bounded).
      return this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(share.owner_id).fetch("https://account-tools.internal/shared-receipt", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ owner_id: share.owner_id, recipient_id: body.owner_id, invocation: body, wait_ms: requestedWaitMs }),
      });
    }
    if (body.machine_id === undefined && body.route_token.startsWith("screen:v1:")) return this.#screenReceipt(body, requestedWaitMs, true);
    if (body.machine_id !== undefined && !HOSTED_MACHINE_TOOL_NAMES.some(name => name === body.name)) {
      return Response.json({ error: "receipt_unsupported" }, { status: 409, headers });
    }
    const waitUntil = Date.now() + Math.max(0, Math.min(HAND_RECEIPT_MAX_WAIT_MS, requestedWaitMs));
    const key = JSON.stringify([body.session_id, body.call_id]);
    const retained = () => this.ctx.storage.sql.exec<{ call_id: string }>(
      "SELECT call_id FROM hosted_tool_calls WHERE session_id=? AND source_call_id=?", body.session_id, body.call_id).toArray()[0];
    // The original /invoke may still be before durable admission (for example
    // waiting for the same runtime to resume). Wait for it to settle here.
    while (!retained() && this.#invocations.has(key) && Date.now() < waitUntil) await abortableDelay(50);
    if (!retained()) {
      if (this.#invocations.has(key)) return Response.json({ receipt: "running" }, { headers });
      this.#fenceCall(body.session_id, body.call_id);
      // A concurrent admission could only have happened before the fence.
      if (!retained()) return Response.json({ error: "receipt_missing", receipt: "missing", admission: "none" }, { status: 404, headers });
    }
    const receipt = await this.#broker.receipt({ sessionId: body.session_id, callId: body.call_id, name: body.name,
      routeToken: body.route_token, context: { sessionId: body.session_id },
      ...(body.machine_id === undefined ? {} : { machineId: body.machine_id }), waitMs: Math.max(0, waitUntil - Date.now()) });
    if (receipt.state === "running") return Response.json({ receipt: "running", deadline_at: receipt.deadlineAt }, { headers });
    if (receipt.state !== "settled") return Response.json({ error: "receipt_unresolved", admission: "retained" }, { status: 409, headers });
    const branded = receipt.result as Record<PropertyKey, unknown>;
    return Response.json({
      output: branded.output,
      structured_result: branded.structuredResult,
      success: branded.success === true,
      metadata: branded.metadata,
      value: branded.value,
      ...(receipt.processRouteToken === undefined ? {} : { process_route_token: receipt.processRouteToken }),
    } satisfies InvocationResult, { headers });
  }

  /**
   * Receipt-only read of one screen action identity from the durable screen
   * ledger. Never sends input. A missing identity is fenced against a late
   * send, but remains unknown: a pre-ledger version may already have acted.
   */
  async #screenReceipt(body: InvocationRequest, requestedWaitMs: number, matchName: boolean): Promise<Response> {
    const headers = { "cache-control": "no-store" };
    const waitUntil = Date.now() + Math.max(0, Math.min(HAND_RECEIPT_MAX_WAIT_MS, requestedWaitMs));
    const key = JSON.stringify([body.session_id, body.call_id]);
    // The original /invoke may still be before admission; wait for it to settle here.
    while (!this.#screenCalls.has(body.session_id, body.call_id) && this.#invocations.has(key) && Date.now() < waitUntil) await abortableDelay(50);
    if (!this.#screenCalls.has(body.session_id, body.call_id)) {
      if (this.#invocations.has(key)) return Response.json({ receipt: "running" }, { headers });
      this.#fenceCall(body.session_id, body.call_id);
      if (!this.#screenCalls.has(body.session_id, body.call_id)) {
        return Response.json({ error: "receipt_unresolved", admission: "unknown" }, { status: 409, headers });
      }
    }
    const receipt = await this.#screenCalls.receipt(body.session_id, body.call_id, body.route_token,
      matchName ? body.name : undefined, Math.max(0, waitUntil - Date.now()));
    if (receipt.state === "running") return Response.json({ receipt: "running", deadline_at: receipt.deadlineAt }, { headers });
    if (receipt.state !== "settled") return Response.json({ error: "receipt_unresolved", admission: "retained" }, { status: 409, headers });
    return Response.json(receipt.result satisfies InvocationResult, { headers });
  }

  /** A repeated /invoke of an admitted screen identity: its retained receipt, never a second send. */
  async #screenReplay(invocation: InvocationRequest, inputDigest: string): Promise<Response> {
    // A conflicting reuse of the identity (other route or input) never sees this result.
    const receipt = await this.#screenCalls.receipt(invocation.session_id, invocation.call_id, invocation.route_token,
      invocation.name, HAND_RECEIPT_MAX_WAIT_MS, inputDigest);
    if (receipt.state === "settled") return Response.json(receipt.result satisfies InvocationResult, { headers: { "cache-control": "no-store" } });
    return Response.json({ error: "duplicate_call", admission: "retained" }, { status: 409, headers: { "cache-control": "no-store" } });
  }

  #localSnapshot(): AccountHostedToolsSnapshot {
    const catalog = this.#broker.catalogSnapshot();
    const authority = this.#screens.hosts();
    const screens = this.#remote.list(true).filter(target => target.agent_tools
      && screenAuthorized(authority, target.machine_id, target.generation));
    return {
        screens,
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
        }), ...screens.map(screenTool)],
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

  async #snapshot(machineId?: string, screens = true): Promise<AccountHostedToolsSnapshot> {
    const owned = await this.#ownedSnapshot(machineId, screens);
    if (!this.env.NANOCODEX_ACCOUNT_TOOLS || !this.#ownerId) return owned;
    const received = this.#shares.received().filter(share => machineId === undefined || machineId === `shared:${share.id}`);
    const shared = await Promise.all(received.map(async share => {
      try {
        return await withHardDeadline<SharedMachineSnapshot | undefined>("shared Hand discovery", 5_000, async () =>
          this.env.NANOCODEX_ACCOUNT_TOOLS!.getByName(share.owner_id).sharedHandSnapshot(share.owner_id, this.#ownerId!, share.id));
      } catch { return undefined; }
    }));
    const available = shared.filter((entry): entry is SharedMachineSnapshot => entry !== undefined);
    return this.#withRoots({ ...owned,
      tools: [...owned.tools, ...available.flatMap(entry => entry.shared_screen_tools)],
      screens: [...owned.screens ?? [], ...available.flatMap(entry => entry.shared_screens)],
      machines: [...owned.machines, ...available.map(({ shared_screens, shared_screen_tools, ...entry }) => entry)] },
      machineId === undefined);
  }

  async #ownedSnapshot(machineId?: string, includeScreens = true): Promise<AccountHostedToolsSnapshot> {
    const full = this.#localSnapshot();
    // A selected route needs only its current publication and capabilities.
    // Inventory remains explicit; this request never probes unrelated regions.
    // Shell-only lookups also skip screen authority: its round trip
    // would only refresh routes the caller does not use.
    const screensOmitted = machineId !== undefined && !includeScreens;
    const local = machineId === undefined ? full : {
      ...full,
      tools: screensOmitted ? [] : full.tools.filter(tool => (full.screens ?? []).some(target => target.machine_id === machineId
        && screenTool(target).route_token === tool.route_token && tool.provider === "screens")),
      screens: screensOmitted ? [] : (full.screens ?? []).filter(target => target.machine_id === machineId),
      machines: full.machines.filter(entry => entry.machine.id === machineId),
      ...(screensOmitted ? { screens_omitted: true as const } : {}),
    };
    // A machine whose publication is being claimed is neither listed nor routable yet.
    const pending = this.#directory.entries().filter(entry => entry.pending
      && (machineId === undefined || entry.machine.id === machineId));
    if (!pending.length) return this.#withRoots(local, machineId === undefined);
    const pendingIds = new Set(pending.map(entry => entry.machine.id));
    const pendingNames = new Set(pending.flatMap(entry => entry.tool_names));
    return this.#withRoots({ ...local,
      tools: local.tools.filter(tool => !pendingNames.has(tool.definition.name)),
      machines: local.machines.filter(entry => !pendingIds.has(entry.machine.id)),
      inventory_unknown_ids: [...pendingIds] }, machineId === undefined);
  }

  #withRoots(snapshot: AccountHostedToolsSnapshot, complete = false): AccountHostedToolsSnapshot {
    // Read the registry synchronously with assignment: no owner deletion or
    // publication can interleave between the two.
    const registry = this.#registry();
    const machines = snapshot.machines.map(entry => entry.machine)
      .filter(machine => registry === undefined || registry.ids.has(machine.id));
    // A filtered view still knows the full ledger, but reclamation runs only on complete owner views.
    const assigned = this.#handPaths.resolve(machines, [], { registry: complete ? registry : undefined });
    const listed = (id: string) => registry === undefined ? machines.some(machine => machine.id === id) : registry.ids.has(id);
    return { ...snapshot,
      mount_roots: Object.fromEntries([...assigned.roots].filter(([id]) => listed(id))),
      mount_aliases: Object.fromEntries([...assigned.aliases].filter(([id]) => listed(id))),
      ...(complete && registry ? { mount_registry: { ids: [...registry.ids], observed_at: registry.observedAt } } : {}) };
  }

  /**
   * Every identity the account still owns, independent of presence,
   * reachability or duplicate-ID discovery fencing. Unreadable ledgers return
   * no registry, so nothing is reclaimed.
   */
  #registry(): HandRegistry | undefined {
    const ids = new Set<string>();
    try {
      for (const row of this.ctx.storage.sql.exec<{ machines_json: string }>(
        "SELECT machines_json FROM hosted_tool_routes WHERE machines_json IS NOT NULL").toArray()) {
        const machines = JSON.parse(row.machines_json) as unknown;
        if (!Array.isArray(machines)) return undefined;
        for (const machine of machines) {
          if (!machine || typeof machine.id !== "string") return undefined;
          ids.add(machine.id);
        }
      }
    } catch { return undefined; }
    for (const entry of this.#directory.entries()) ids.add(entry.machine.id);
    for (const share of this.#shares.received()) ids.add(`shared:${share.id}`);
    return { ids, observedAt: Date.now() };
  }

  async #admitPublication(candidate: Parameters<NonNullable<import("./hosted-tools-broker").HostedToolsBrokerOptions["beforeCatalogPublish"]>>[0]): Promise<() => boolean> {
    if (!candidate.machine) {
      const names = new Set(candidate.definitions.map(entry => entry.definition.name));
      if (this.#directory.entries().some(entry => entry.tool_names.some(name => names.has(name)))) {
        throw new Error("tool name is already exposed by an account Hand");
      }
      // A claim can complete while this broker awaits this guard.
      // Recheck immediately before the local catalog commits.
      return () => !this.#directory.entries().some(entry => entry.tool_names.some(name => names.has(name)));
    }
    const publication: HandPublication = { route_id: candidate.routeId, publication_id: crypto.randomUUID(),
      region: "legacy", machine: candidate.machine,
      tool_names: candidate.definitions.map(entry => entry.definition.name), runtime_id: candidate.runtimeId };
    this.ctx.storage.sql.exec("INSERT INTO regional_local_publications(route_id,candidate_id) VALUES(?,?) ON CONFLICT(route_id) DO UPDATE SET candidate_id=excluded.candidate_id", candidate.routeId, publication.publication_id);
    try {
      await this.#queueClaim(publication);
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
      this.#directory.claim(publication);
    });
    this.#publicationQueue = result.catch(() => {});
    return result;
  }

  #settleAbandonedCalls(machineId: string, runtimeId: string | null): void {
    // Explicit owner abandonment preserves the durable result. It never asserts
    // completion, erases receipts, or permits replay onto a replacement runtime.
    for (const state of ["admitted", "dispatched"] as const) {
      const message = "Owner retired the disconnected Hand before its command outcome could be confirmed";
      const outcome = state === "dispatched" ? hostedToolsAmbiguous(message) : hostedToolsUnavailable(message);
      this.ctx.storage.sql.exec(`UPDATE hosted_tool_calls SET state=?,result_json=?,updated_at=?
        WHERE hand_id=? AND host_runtime_id IS ? AND state=?`,
        state === "dispatched" ? "ambiguous" : "unavailable", JSON.stringify(outcome), Date.now(), machineId, runtimeId, state);
    }
  }

  async #regionalRequest(request: Request, url: URL): Promise<Response> {
    const owner = request.headers.get(OWNER_ASSERTION);
    if (!isUserId(owner) || !this.#claim(owner)) return Response.json({ error: "not_found" }, { status: 404 });
    if (url.pathname === "/regional/status" && request.method === "GET" && !url.search) {
      const rows = this.ctx.storage.sql.exec<{ runtime_id: string | null; generation: number; machines_json: string | null }>(
        "SELECT runtime_id,generation,machines_json FROM hosted_tool_routes WHERE machines_json IS NOT NULL").toArray();
      const legacy = rows.flatMap(row => (JSON.parse(row.machines_json!) as HostedMachine[]).map(machine => {
        const pending = this.ctx.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id IS ? AND state IN ('admitted','dispatched')", machine.id, row.runtime_id).toArray()[0]!.count;
        const online = this.#broker.machineOnline(machine.id);
        return { machine_id: machine.id, runtime_id: row.runtime_id, generation: row.generation, online, pending_calls: pending, retirable: !online && pending === 0 };
      }));
      return Response.json({ legacy, regional: [] }, { headers: { "cache-control": "no-store" } });
    }
    if (request.method !== "POST" || url.search) return Response.json({ error: "invalid_request" }, { status: 400 });
    let body: Record<string, unknown>;
    try { body = await boundedJSON(request) as Record<string, unknown>; } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
    if (!body || typeof body !== "object" || Array.isArray(body)) return Response.json({ error: "invalid_request" }, { status: 400 });
    if (url.pathname === "/regional/retire") {
      if (body.publication_id !== undefined || body.region !== undefined) return Response.json({ error: "invalid_request" }, { status: 400 });
      const unversioned = body.runtime_id === null;
      const abandonPending = body.abandon_pending === true;
      if ((body.abandon_pending !== undefined && !abandonPending) || !validPublisherId(body.machine_id) || (unversioned
        ? !Number.isSafeInteger(body.generation) || (body.generation as number) <= 0 || Object.keys(body).length !== (abandonPending ? 4 : 3)
        : !validPublisherId(body.runtime_id) || Object.keys(body).length !== (abandonPending ? 3 : 2))) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      if (!unversioned && this.#directory.retired(body.machine_id, body.runtime_id as string)) {
        return Response.json({ retired: true, machine_id: body.machine_id, runtime_id: body.runtime_id });
      }
      const rows = this.ctx.storage.sql.exec<{ route_id: string; runtime_id: string | null; machines_json: string | null; lease_id: string | null; generation: number }>(
        "SELECT route_id,runtime_id,machines_json,lease_id,generation FROM hosted_tool_routes").toArray();
      const route = rows.find(row => row.machines_json && (JSON.parse(row.machines_json) as HostedMachine[]).some(machine => machine.id === body.machine_id));
      // One receipt per machine bounds retained markers. Verify the ledger still
      // names that generation: a retry must never act on a later reconnect.
      const receiptKey = `legacy_retirement:${body.machine_id}`;
      const receipt = unversioned ? this.ctx.storage.kv.get<{ route_id: string; generation: number }>(receiptKey) : undefined;
      if (!route && receipt && receipt.generation === body.generation && rows.some(row =>
        row.route_id === receipt.route_id && row.generation === receipt.generation
        && row.runtime_id === null && row.machines_json === null && row.lease_id === null)) {
        return Response.json({ retired: true, machine_id: body.machine_id, runtime_id: null, generation: body.generation });
      }
      if (!route || route.runtime_id !== body.runtime_id) {
        return Response.json({ error: "legacy_runtime_not_found" }, { status: 409 });
      }
      if (unversioned && route.generation !== body.generation) return Response.json({ error: "legacy_generation_changed" }, { status: 409 });
      if (this.#broker.machineOnline(body.machine_id)) return Response.json({ error: "legacy_runtime_still_connected" }, { status: 409 });
      const pending = this.ctx.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id IS ? AND state IN ('admitted','dispatched')", body.machine_id, body.runtime_id).toArray()[0]!.count;
      if (pending && !abandonPending) return Response.json({ error: "legacy_runtime_has_pending_calls" }, { status: 409 });
      this.ctx.storage.transactionSync(() => {
        this.#broker.retireRoute(route.route_id, "Owner explicitly retired disconnected legacy runtime", 1012);
        if (abandonPending) this.#settleAbandonedCalls(body.machine_id as string, body.runtime_id as string | null);
        if (unversioned) this.ctx.storage.kv.put(receiptKey, { route_id: route.route_id, generation: route.generation });
        else this.#directory.retire(body.machine_id as string, body.runtime_id as string);
      });
      return Response.json({ retired: true, machine_id: body.machine_id, runtime_id: body.runtime_id,
        ...(unversioned ? { generation: body.generation } : {}) });
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  alarm(): void { this.#broker.expire(); }

  /** Durable monotonic host-socket counter; `step` 0 reads the current high-water mark. */
  #screenSequence(step: 0 | 1): number {
    const next = (this.ctx.storage.kv.get<number>("screen_host_sequence") ?? 0) + step;
    if (step) this.ctx.storage.kv.put("screen_host_sequence", next);
    return next;
  }

  /** Portable playback command (internal binding only). */
  async #screenHostCommand(request: Request): Promise<Response> {
    const owner = request.headers.get(OWNER_ASSERTION);
    if (request.method !== "POST" || !isUserId(owner) || !this.#owns(owner)) return Response.json({ error: "not_found" }, { status: 404 });
    let body: Record<string, any>;
    try { body = await boundedJSON(request) as Record<string, any>; } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
    const command = body?.command;
    const generation = body?.generation === undefined ? undefined : String(body.generation);
    if (!validPublisherId(body?.machine_id) || !validPublisherId(body?.surface_id) || (generation !== undefined && !validPublisherId(generation))
      || !command || typeof command !== "object" || command.type !== "broadcast" || command.target !== "hls"
      || (command.surface_id !== undefined && command.surface_id !== body.surface_id) || !["start", "stop", "status"].includes(command.action)
      || !validPublisherId(command.request_id) || !validPublisherId(command.stream_id)
      || (command.action === "start" && (typeof command.preset !== "string" || !command.upload || typeof command.upload.url !== "string"
        || typeof command.upload.token !== "string" || !Number.isSafeInteger(command.upload.expires_at)))) {
      return Response.json({ error: "invalid_request" }, { status: 400 });
    }
    return this.#remote.sendHostCommand(body.machine_id, body.surface_id, generation, { action: command.action, request_id: command.request_id,
      stream_id: command.stream_id, ...(command.action === "start" ? { preset: command.preset,
        upload: { url: command.upload.url, token: command.upload.token, expires_at: command.upload.expires_at } } : {}) });
  }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (this.#remote.owns(socket)) { await this.#remote.message(socket, message); return; }
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

const HAND_RECONNECT_ADMISSION_WAIT_MS = 10_000;
const SELECTED_LOOKUP_REUSE_MS = 30_000;
/** Bounded pause before the single fresh retry of a transiently unpublished selected route. */
const SELECTED_ROUTE_RETRY_MS = 1_500;

function abortableDelay(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const onAbort = () => { clearTimeout(timer); reject(signal!.reason); };
    const timer = setTimeout(() => { signal?.removeEventListener("abort", onAbort); resolve(); }, ms);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
type HandFailureReason = "route_unavailable_after_recovery" | "route_replaced" | "route_unpublished" | "route_refresh_failed"
  | "process_runtime_replaced" | "transport_failed" | "outcome_unknown" | "receipt_unrecoverable" | "receipt_missing"
  | "cancelled_after_dispatch" | "cancelled_before_start";
type HandReceiptFailure = "receipt_missing" | "receipt_unsupported" | "receipt_unresolved" | "receipt_unreachable"
  | "receipt_poll_limit" | "receipt_deadline" | "receipt_unauthorized";
/** Server-side bounded wait per receipt read; never beyond the call's own admitted deadline. */
const HAND_RECEIPT_WAIT_MS = 20_000;
const HAND_RECEIPT_MAX_WAIT_MS = 25_000;
const HAND_RECEIPT_FETCH_TIMEOUT_MS = HAND_RECEIPT_MAX_WAIT_MS + 10_000;
/** Absolute wall budget and poll cap; a running call is further bounded by its own deadline plus grace. */
const HAND_RECEIPT_MAX_TOTAL_MS = 30 * 60_000;
const HAND_RECEIPT_MAX_POLLS = 120;
const HAND_RECEIPT_DEADLINE_GRACE_MS = 15_000;
const HAND_RECEIPT_MAX_TRANSPORT_FAILURES = 3;
const HAND_RECEIPT_RETRY_MS = 500;
const HAND_CANCEL_DELIVERY_TIMEOUT_MS = 3_000;
const HAND_CANCEL_DELIVERY_ATTEMPTS = 3;

function validInvocationResult(result: unknown): result is InvocationResult {
  return !!result && typeof result === "object" && typeof (result as InvocationResult).success === "boolean"
    && Object.hasOwn(result, "output") && Object.hasOwn(result, "structured_result")
    && Object.hasOwn(result, "metadata") && Object.hasOwn(result, "value");
}

/** Fixed, model-visible failure class: the error's constructor name and a closed category. */
function handErrorClass(error: unknown, phase: "transport" | "decode"): string {
  const name = error instanceof Error && /^[A-Za-z]{1,40}$/.test(error.name) ? error.name : "Error";
  const text = error instanceof Error ? error.message : String(error);
  const flags = durableObjectErrorFlags(error);
  // Runtime-provided properties first; message text only as a fallback.
  const category = name === "AbortError" ? "aborted"
    : phase === "decode" || name === "SyntaxError" ? "decode_failed"
    : flags.overloaded ? "overloaded"
    : flags.durable_object_reset ? "object_reset"
    : /timed? ?out|deadline/i.test(text) ? "timeout"
    : /overload|too many|exceeded/i.test(text) ? "overloaded"
    : /durable object|reset because|code was updated|object.*(reset|evict)/i.test(text) ? "object_reset"
    : /network|connection|disconnect|socket|econn|stream|lost/i.test(text) ? "network_lost" : "other";
  return `${name}/${category}`;
}

/**
 * Safe boolean properties Cloudflare attaches to Durable Object stub errors.
 * A thrown stub is broken for later calls; idempotent reads use a fresh stub,
 * and an overloaded object is never retried.
 */
function durableObjectErrorFlags(error: unknown): { retryable: boolean; overloaded: boolean; durable_object_reset: boolean; remote: boolean } {
  const value = error && typeof error === "object" ? error as Record<string, unknown> : {};
  return { retryable: value.retryable === true, overloaded: value.overloaded === true,
    durable_object_reset: value.durableObjectReset === true, remote: value.remote === true };
}

/** Fixed class plus bounded sanitized cause, e.g. "TypeError/network_lost: Network connection lost". */
function handErrorDetail(error: unknown, phase: "transport" | "decode"): string {
  const message = sanitizedHandError(error).replace(/^[A-Za-z]{1,40}: /, "").slice(0, 120);
  const errorClass = handErrorClass(error, phase);
  return message && !/^[A-Za-z]{1,40}$/.test(sanitizedHandError(error)) ? `${errorClass}: ${message}` : errorClass;
}

/** Bounded error name and message without URLs, hosts, paths, long tokens or control characters. */
function sanitizedHandError(error: unknown): string {
  const name = error instanceof Error && /^[A-Za-z]{1,40}$/.test(error.name) ? error.name : "Error";
  const raw = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const message = raw.replace(/[a-z][a-z0-9+.-]*:\/\/\S+/gi, "[url]").replace(/[A-Za-z0-9_+/=-]{24,}/g, "[redacted]")
    .replace(/\b(?:[a-z0-9-]+\.)+[a-z]{2,}\b/gi, "[host]").replace(/(?:\/[\w.-]+){2,}\/?/g, "[path]")
    .replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 160);
  return message ? `${name}: ${message}` : name;
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
  /**
   * Compare-and-swap a call's route only after its shard ledger proved it was
   * never admitted. Concurrent repins converge on whichever route won.
   */
  repin(sessionId: string, callId: string, name: string, machineId: string | undefined, expected: string, next: string): string {
    this.storage.sql.exec("UPDATE account_hand_call_routes SET route_token=? WHERE session_id=? AND call_id=? AND route_token=?",
      next, sessionId, callId, expected);
    return this.pin(sessionId, callId, name, machineId, next);
  }
}

/** Dynamic provider proxy from one agent DO to its account's shared hand DO. */
export class AccountHostedToolsProvider implements HostedToolsDynamicProvider {
  readonly sourceId = "account-hands";
  readonly #turnTargets = new Map<string, Map<string, DurableObjectStub>>();
  readonly #namespace: DurableObjectNamespace<AccountHostedTools>;
  readonly #callRoutes: AccountHostedToolsCallRoutes | undefined;
  readonly #ownerId: string;
  readonly #threadId: string | undefined;
  readonly #allowed: (context?: AuthorizationContext) => boolean;
  #snapshot: AccountHostedToolsSnapshot = { tools: [], machines: [] };
  #definitions: readonly HostedToolsCodeDefinition[] = [];
  #candidates: readonly HostedToolsCatalogCandidate[] = [];
  #machines: readonly HostedMachine[] = [];
  #machineRoots: ReadonlyMap<string, string> = new Map();
  #machineAliases: ReadonlyMap<string, readonly string[]> = new Map();
  #machineRegistry: HandRegistry | undefined;
  #onlineMachineIds = new Set<string>();
  #tools = new Map<string, RoutedHostedTool>();
  #machineTools = new Map<string, HostedToolsCodeTool>();
  #screenTools = new Map<string, HostedToolsCodeTool>();
  #screenMachines: readonly HostedMachine[] = [];
  #validator: HostedToolsCatalogValidator | undefined;
  #refreshing?: Promise<void>;
  #recoveryRefreshing?: Promise<void>;
  #optionalRetryAt = 0;
  #loadedAt = 0;
  #generation = 0;
  #selectedFresh = new Map<string, { at: number; generation: number }>();
  #refreshGeneration = 0;

  constructor(
    namespace: DurableObjectNamespace<AccountHostedTools>,
    ownerId: string,
    allowed: (context?: AuthorizationContext) => boolean,
    threadId?: string,
    callRoutes?: AccountHostedToolsCallRoutes,
  ) {
    this.#namespace = namespace;
    this.#callRoutes = callRoutes;
    this.#ownerId = ownerId;
    this.#threadId = threadId;
    this.#allowed = allowed;
  }

  async endTurn(sessionId: string, turnId: string, hookEventName: "Stop" | "Interrupt" | "SubagentStop"): Promise<void> {
    const key = JSON.stringify([sessionId, turnId]);
    const targets = this.#turnTargets.get(key);
    this.#turnTargets.delete(key);
    // A stored stub may be broken by an object reset since the call; use a fresh one.
    await Promise.all([...targets?.keys() ?? []].map(async () => {
      const response = await this.#namespace.getByName(this.#ownerId).fetch("https://account-tools.internal/turn-ended", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ owner_id: this.#ownerId, frame: {
          type: "turn_ended", session_id: sessionId, turn_id: turnId, hook_event_name: hookEventName,
        } }),
      });
      if (!response.ok) throw new Error(`Hand turn cleanup failed (${response.status}); not retried`);
    }));
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

  /** Account-retained historical roots of the same identities. */
  machineAliases(): ReadonlyMap<string, readonly string[]> {
    return this.#allowed() ? this.#machineAliases : new Map();
  }

  /** The account's complete identity registry, or undefined for partial, failed or restricted views. */
  machineRegistry(): HandRegistry | undefined {
    return this.#allowed() ? this.#machineRegistry : undefined;
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

  /** Restore only the persisted executor route; no inventory or rerouting. */
  recoverProcessTool(machineId: string, key: string, context?: AuthorizationContext): HostedToolsCodeTool | undefined {
    if (!this.#allowed(context)) return undefined;
    let identity: unknown;
    try { identity = JSON.parse(key); } catch { return undefined; }
    if (!Array.isArray(identity) || identity.length !== 3 || identity[0] !== "account-process"
      || identity[1] !== machineId || typeof identity[2] !== "string") return undefined;
    return this.#processTool(machineId, identity[2]);
  }

  #processTool(machineId: string, routeToken: string): HostedToolsCodeTool {
    return Object.freeze({
      name: "write_stdin", parallelSafe: true,
      processSessionKey: JSON.stringify(["account-process", machineId, routeToken]),
      handler: (input: unknown, context: InvocationContext) =>
        this.#invoke("write_stdin", routeToken, input, context, machineId, "fixed"),
    });
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

  /** The current generation's in-flight full inventory refresh, if any. */
  pendingRefresh(): Promise<void> | undefined {
    return this.#refreshing !== undefined && this.#refreshGeneration === this.#generation ? this.#refreshing : undefined;
  }

  /** Fresh selected-machine lookup. Never joins a slow full inventory request. */
  async refreshMachine(machineId: string, context: AuthorizationContext, computer = false, screens = computer,
    signal?: AbortSignal): Promise<void> {
    if (!this.#allowed(context)) throw new Error("Hand access revoked");
    const generation = this.#generation;
    // Shell routes do not need a per-call cross-region lookup: a recent
    // successful lookup in this generation is reused, and a route that went
    // stale is repinned by #repinNeverAdmitted before admission.
    const fresh = this.#selectedFresh.get(machineId);
    if (!computer && fresh !== undefined && fresh.generation === generation
      && Date.now() - fresh.at < SELECTED_LOOKUP_REUSE_MS && this.#onlineMachineIds.has(machineId)
      && this.#machineTools.has(machineToolKey(machineId, "exec_command"))) return;
    this.#selectedFresh.delete(machineId);
    const lookup = () => fetchResponseWithDeadline(
      this.#namespace.getByName(this.#ownerId), "https://account-tools.internal/snapshot",
      { method: "POST", headers: { "content-type": "application/json" },
        // Shell-only lookups never wait on screen authority in another region.
        body: JSON.stringify({ owner_id: this.#ownerId, machine_id: machineId, ...(screens ? {} : { screens: false }) }) },
      10_000, "selected Hand lookup", async response => {
        if (!response.ok) throw new Error("Selected Hand lookup unavailable");
        return response.json<unknown>();
      }).catch(error => {
        throw Object.assign(new Error("Selected Hand lookup interrupted", { cause: error }), { code: "host_interrupted" });
      });
    const routable = (value: unknown): value is AccountHostedToolsSnapshot => validSnapshot(value)
      && !value.inventory_unknown_ids?.includes(machineId)
      && !value.machines.some(entry => entry.machine.id !== machineId)
      && (computer || (value.machines.length === 1 && value.machines[0]?.online === true));
    let snapshot: unknown;
    let interrupted: { error_class: string; error_flags: ReturnType<typeof durableObjectErrorFlags> } | undefined;
    const recordLookup = (outcome: string) => {
      if (!interrupted) return;
      // The sanitized initial cause is retained even when the retry succeeds.
      try { console.info({ type: "hand.selected_lookup.reconcile", hand_id: machineId, thread_id: this.#threadId,
        session_id: context.sessionId, ...interrupted, outcome }); }
      catch { /* Diagnostics never change routing. */ }
    };
    try { snapshot = await lookup(); }
    catch (error) {
      // Read-only and idempotent: an interrupted lookup (object reset or deploy)
      // gets the same one bounded retry, through a fresh stub. Never overloaded.
      const cause = (error as { cause?: unknown }).cause;
      interrupted = { error_class: handErrorDetail(cause, "transport"), error_flags: durableObjectErrorFlags(cause) };
      if (signal?.aborted || interrupted.error_flags.overloaded) { recordLookup("not_retried"); throw error; }
      snapshot = undefined;
    }
    if (generation !== this.#generation || !this.#allowed(context)) throw new Error("Hand authorization changed during lookup");
    if (!routable(snapshot)) {
      // A Hand socket replacement briefly unpublishes the route. Retry one
      // fresh lookup after a short bounded wait before failing the call.
      try {
        await abortableDelay(SELECTED_ROUTE_RETRY_MS, signal);
        snapshot = await lookup();
      } catch (error) { recordLookup("retry_failed"); throw error; }
      if (generation !== this.#generation || !this.#allowed(context)) throw new Error("Hand authorization changed during lookup");
      if (!routable(snapshot)) { recordLookup("unroutable"); throw new Error("Selected Hand route unavailable"); }
      recordLookup("recovered");
    }
    // Replace only this machine's screen routes; unrelated catalogs and cells survive.
    // A lookup that omitted screens keeps this machine's retained screen routes.
    const keepScreens = snapshot.screens_omitted === true;
    if (keepScreens && screens) throw new Error("Selected Hand lookup omitted requested screens");
    const removed = new Set(keepScreens ? [] : (this.#snapshot.screens ?? []).filter(target => target.machine_id === machineId)
      .map(target => screenTool(target).route_token));
    const selectedScreens = keepScreens ? [] : (snapshot.screens ?? []).filter(target => target.machine_id === machineId);
    const routes = new Set(selectedScreens.map(target => screenTool(target).route_token));
    // A selected lookup cannot prove the earlier full registry is still current.
    const { mount_registry: _stale, ...current } = this.#snapshot;
    this.#publish({ ...current,
      screens: [...(this.#snapshot.screens ?? []).filter(target => keepScreens || target.machine_id !== machineId), ...selectedScreens],
      tools: [...this.#snapshot.tools.filter(tool => tool.provider !== "screens" || !removed.has(tool.route_token)),
        ...snapshot.tools.filter(tool => tool.provider === "screens" && routes.has(tool.route_token))],
      machines: [...this.#snapshot.machines.filter(entry => entry.machine.id !== machineId), ...snapshot.machines],
      mount_roots: { ...this.#snapshot.mount_roots, ...snapshot.mount_roots },
      mount_aliases: { ...this.#snapshot.mount_aliases, ...snapshot.mount_aliases },
    });
    if (generation === this.#generation) this.#selectedFresh.set(machineId, { at: Date.now(), generation });
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
    this.#snapshot = snapshot;
    this.#machineRoots = new Map(Object.entries(snapshot.mount_roots ?? {}));
    this.#machineAliases = new Map(Object.entries(snapshot.mount_aliases ?? {})
      .filter((entry): entry is [string, string[]] => Array.isArray(entry[1]) && entry[1].every(root => typeof root === "string")));
    const registry = snapshot.mount_registry;
    this.#machineRegistry = registry && Array.isArray(registry.ids) && registry.ids.every(id => typeof id === "string")
      && Number.isFinite(registry.observed_at) ? { ids: new Set(registry.ids), observedAt: registry.observed_at } : undefined;
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
            route.name === "native_secure_input" || route.name === CUA_JS_NAME || route.name === CUA_RESET_NAME ? "fixed" : "refresh"),
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

  /**
   * Move a call that its shard ledger proved never admitted onto the Hand's
   * current same-shard route: one shared refresh, one compare-and-swap repin,
   * one fixed-policy attempt under the same call identity. The broker's unique
   * (session, call) ledger still fences any concurrent earlier admission.
   */
  async #repinNeverAdmitted(
    name: string,
    routeToken: string,
    input: unknown,
    context: InvocationContext,
    machineId: string | undefined,
    failed: (message: string, status: "ambiguous" | "unavailable", preAdmission?: boolean, reason?: HandFailureReason) => unknown,
    optional = false,
  ): Promise<unknown> {
    try {
      // Concurrent stale callers share one inventory load: only the first
      // caller that still sees the rejected route invalidates the snapshot.
      if (routeFor(this, name, machineId)?.routeToken === routeToken) {
        // A second invalidation would discard the first caller's in-flight
        // load. All recovery callers must await the same published generation.
        if (!this.#recoveryRefreshing) {
          this.invalidate();
          const refreshing = this.refresh().finally(() => {
            if (this.#recoveryRefreshing === refreshing) this.#recoveryRefreshing = undefined;
          });
          this.#recoveryRefreshing = refreshing;
        }
        await this.#recoveryRefreshing;
      } else await (this.#recoveryRefreshing ?? this.#refreshing);
    } catch {
      return optional ? undefined
        : failed("Hand route refresh failed before this call was admitted; nothing was sent.", "unavailable", true, "route_refresh_failed");
    }
    const route = routeFor(this, name, machineId);
    if (!route?.routeToken || route.routeToken === routeToken) {
      return optional ? undefined
        : failed("The Hand is not currently published on a reachable route; this call was not admitted and nothing was sent.", "unavailable", true, "route_unpublished");
    }
    let pinned: string;
    try { pinned = this.#callRoutes!.repin(context.sessionId, context.callId, name, machineId, routeToken, route.routeToken); }
    catch { return failed("Hand call identity conflicts with its retained route", "ambiguous"); }
    return this.#invoke(name, pinned, input, context, machineId, "fixed");
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
    const failed = (message: string, status: "ambiguous" | "unavailable", preAdmission = false, reason?: HandFailureReason,
      extra?: Readonly<Record<string, unknown>>): unknown => {
      observeHandCall("account.fetch", name, startedAt, status, context.callId, correlation);
      return failedToolResult(message, status, preAdmission, reason ?? (status === "ambiguous" ? "outcome_unknown" : undefined), extra);
    };
    const startedAt = performance.now();
    if (!this.#allowed(context)) {
      return failed("Account hand is outside the active grant", "unavailable", true);
    }
    // Routes minted by the removed regional relays are dead; nothing was sent.
    if (routeToken.startsWith("hand-relay:")) return failed("This Hand route is no longer published; refresh and retry.", "unavailable", true, "route_unpublished");
    if (this.#callRoutes) {
      try { routeToken = this.#callRoutes.pin(context.sessionId, context.callId, name, machineId, routeToken); }
      catch { return failed("Hand call identity conflicts with its retained route", "ambiguous"); }
    }
    const target = this.#namespace.getByName(this.#ownerId);
    if (context.turnId !== undefined) {
      const key = JSON.stringify([context.sessionId, context.turnId]);
      let targets = this.#turnTargets.get(key);
      if (!targets) { targets = new Map(); this.#turnTargets.set(key, targets); }
      targets.set("account", target);
    }
    const invocation = {
      owner_id: this.#ownerId,
      name,
      input,
      session_id: context.sessionId,
      ...(this.#threadId === undefined ? {} : { thread_id: this.#threadId }),
      ...(context.turnId === undefined ? {} : { turn_id: context.turnId }),
      call_id: context.callId,
      model: context.model,
      ...(machineId === undefined ? {} : { machine_id: machineId }),
      route_token: routeToken,
    } satisfies InvocationRequest;
    let response: Response;
    try {
      response = await target.fetch("https://account-tools.internal/invoke", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(invocation),
        signal: context.signal,
      });
    } catch (error) {
      timing.fetch_ms = performance.now() - startedAt;
      // Never re-POST /invoke after possible dispatch: reconcile its receipt.
      return this.#afterLostResponse(invocation, routePolicy, context, "transport", error, failed);
    }
    const responseAt = performance.now();
    timing.fetch_ms = responseAt - startedAt;
    if (response.ok) observeHandCall("account.fetch", name, startedAt, "ok", context.callId, correlation);
    if (!response.ok) {
      const preAdmission = response.status === 404 || response.status === 409;
      // Only the target shard's explicit ledger evidence proves non-admission;
      // a bare status, unreadable body or older account worker does not.
      let neverAdmitted = false, retained = false, fenced = false;
      if (preAdmission) {
        try {
          const rejection = await response.json<{ admission?: unknown; error?: unknown }>();
          neverAdmitted = rejection.admission === "none"; retained = rejection.admission === "retained";
          fenced = neverAdmitted && rejection.error === "call_fenced";
        }
        catch { /* Unknown evidence keeps the call pinned and its outcome unknown. */ }
      } else if (response.status === 502) {
        // The recipient lost its hop to the sharing owner after possible dispatch.
        let reconcile = false;
        try { reconcile = (await response.json<{ reconcile?: unknown }>()).reconcile === "receipt"; }
        catch { /* An unrecognized gateway failure stays an unknown outcome below. */ }
        if (reconcile) {
          return this.#afterLostResponse(invocation, routePolicy, context, "transport",
            new Error("Shared Hand owner connection lost"), failed);
        }
      } else {
        try { await response.body?.cancel(); } catch { /* Body is irrelevant to a failed status. */ }
      }
      if (preAdmission && routePolicy === "screen" && retained) {
        // The same screen identity was already admitted once; it is never resent.
        return failed("This screen action identity was already sent and its result is not available. Screen outcome is unknown; the action was not resent. Observe the screen before considering another input action.",
          "ambiguous");
      }
      if (preAdmission && routePolicy === "screen" && fenced) {
        return failed("This screen action identity is fenced against another send. Its earlier outcome is unknown; the action was not resent. Observe the screen before considering another input action.",
          "ambiguous");
      }
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
      if (neverAdmitted && !this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        // Only explicit ledger-backed non-admission permits local reconciliation. Keep
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
        if (route?.routeToken && route.routeToken !== routeToken) {
          return this.#invoke(name, route.routeToken, input, context, machineId, "fixed");
        }
      }
      if (neverAdmitted && this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        return this.#repinNeverAdmitted(name, routeToken, input, context, machineId, failed);
      }
      if (neverAdmitted && machineId !== undefined && name === "write_stdin" && response.status === 409) {
        return failed("The Hand process runtime changed before this poll or stdin was admitted; nothing was sent. This saved process session cannot be routed to the replacement runtime.", "unavailable", true, "process_runtime_replaced");
      }
      if (neverAdmitted) {
        // Fixed routes (CUA, secure input) stay pinned to their original runtime.
        return failed("The Hand route changed before this call was admitted; nothing was sent. This pinned route is not moved to a replacement runtime.", "unavailable", true, "route_replaced");
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
      if (!validInvocationResult(result)) throw new Error("invalid account hand result");
    } catch (error) {
      observeHandCall("account.decode", name, responseAt, "ambiguous", context.callId, correlation);
      return this.#afterLostResponse(invocation, routePolicy, context, "decode", error, failed);
    } finally {
      timing.decode_ms = performance.now() - responseAt;
    }
    const structuredStatus = result.structured_result && typeof result.structured_result === "object"
      ? (result.structured_result as { status?: unknown }).status : undefined;
    observeHandCall("account.decode", name, responseAt, result.pre_admission_unavailable === true ? "unavailable"
      : result.success ? "ok" : structuredStatus === "ambiguous" ? "ambiguous"
      : structuredStatus === "unavailable" ? "unavailable" : structuredStatus === "cancelled" ? "cancelled" : "failed", context.callId, correlation);
    if (machineId !== undefined && result.pre_admission_unavailable === true) {
      // The broker checked its call ledger: this invocation was never admitted.
      // Report precise transport state instead of indefinitely replaying the turn.
      // Do not infer this from discovery or HTTP errors: an earlier attempt may
      // have been admitted and must retain its identity for receipt recovery.
      // The broker already waited (bounded) for the same runtime epoch. If the
      // physical Hand republished under a replacement route, move this
      // never-admitted call there once instead of asking the user to unblock.
      if (this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        const moved = await this.#repinNeverAdmitted(name, routeToken, input, context, machineId, failed, true);
        if (moved !== undefined) return moved;
      }
      const reason = typeof result.output === "string" ? result.output : "hand unavailable";
      return failedToolResult(
        `Account hand ${machineId} did not start tool execution: ${reason}. The route remained unavailable after bounded automatic recovery; nothing ran and the command was not resent.`,
        "unavailable",
        true,
        "route_unavailable_after_recovery",
      );
    }
    // Authority is rechecked after the account round trip, which may have read
    // an archived receipt, as the receipt path does before returning output.
    if (!this.#allowed(context)) {
      return failedToolResult("Account hand tool authority changed before the result was returned. The result is withheld and the command was not resent.",
        "ambiguous");
    }
    return this.#brandedResult(result, name, machineId);
  }

  #brandedResult(result: InvocationResult, name: string, machineId: string | undefined): unknown {
    const branded = {
      [TOOL_RESULT]: true,
      output: result.output,
      structuredResult: result.structured_result,
      success: result.success,
      metadata: result.metadata,
      value: result.value,
      ...(machineId !== undefined && name === "exec_command" && typeof result.process_route_token === "string"
        ? { [PROCESS_SESSION_TOOL]: this.#processTool(machineId, result.process_route_token) } : {}),
      ...(result.pre_admission_unavailable === true
        ? { [HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]: true as const }
        : {}),
    };
    return Object.freeze(branded);
  }

  /**
   * The managed->account response was lost after possible dispatch. Explicit
   * cancellation is delivered as its own request; otherwise only the original
   * call identity's receipt is read. /invoke is never posted again here.
   */
  async #afterLostResponse(
    invocation: InvocationRequest,
    routePolicy: "refresh" | "fixed" | "screen",
    context: InvocationContext,
    phase: "transport" | "decode",
    error: unknown,
    failed: (message: string, status: "ambiguous" | "unavailable", preAdmission?: boolean, reason?: HandFailureReason,
      extra?: Readonly<Record<string, unknown>>) => unknown,
  ): Promise<unknown> {
    // Model-visible detail: a fixed class plus bounded, sanitized text.
    const telemetryError = sanitizedHandError(error);
    const detail = handErrorDetail(error, phase);
    const lost = phase === "decode" ? "Hand response could not be decoded" : "Hand connection failed after possible dispatch";
    const flags = durableObjectErrorFlags(error);
    const record = (outcome: string, fields: Readonly<Record<string, unknown>> = {}) => {
      // The sanitized initial cause is retained even when recovery succeeds.
      try { console.info({ type: "hand.receipt.reconcile", tool: invocation.name, session_id: invocation.session_id,
        thread_id: invocation.thread_id, turn_id: invocation.turn_id, source_call_id: invocation.call_id,
        hand_id: invocation.machine_id, phase, error_class: detail, error: telemetryError, error_flags: flags, outcome, ...fields }); }
      catch { /* Diagnostics never change the reconciled outcome. */ }
    };
    if (context.signal?.aborted) { record("cancel_requested"); return this.#cancelAfterLoss(invocation, failed, detail); }
    // Every route (owned, shared and screen) reconciles through the original
    // identity's receipt on its owner ledger; nothing is ever re-posted.
    let recovery: HandReceiptFailure = "receipt_unauthorized";
    let recoveryError: string | undefined;
    if (flags.overloaded) {
      // Cloudflare: an overloaded object must not receive retries, including receipt reads.
      recovery = "receipt_unreachable"; recoveryError = detail; record(recovery, { polls: 0 });
    } else if (this.#allowed(context)) {
      const reconciled = await this.#reconcileReceipt(invocation, context, failed, detail);
      if ("result" in reconciled) { record("recovered", { polls: reconciled.polls }); return reconciled.result; }
      record(reconciled.reason, { polls: reconciled.polls, ...(reconciled.error === undefined ? {} : { recovery_error: reconciled.error }) });
      recovery = reconciled.reason;
      recoveryError = reconciled.error;
      if (recovery === "receipt_missing") {
        this.invalidate();
        return failed(`${lost} (${detail}), and this call was never admitted by the Hand broker. It is now fenced so it can never run, and nothing was resent. Call environment to refresh Hand status, then retry on the same Hand if the work is still needed.`,
          "unavailable", false, "receipt_missing", { admitted: false, error: detail });
      }
    }
    this.invalidate();
    if (routePolicy === "screen") {
      return failed(`${lost} (${detail}) and automatic receipt recovery could not confirm the result (${recovery}${recoveryError ? `: ${recoveryError}` : ""}). Screen outcome is unknown; the action was not resent. Observe the screen before considering another input action.`,
        "ambiguous", false, "receipt_unrecoverable",
        { error: detail, recovery, ...(recoveryError === undefined ? {} : { recovery_error: recoveryError }) });
    }
    return failed(`${lost} (${detail}) and automatic receipt recovery could not confirm the result (${recovery}${recoveryError ? `: ${recoveryError}` : ""}). Execution outcome is unknown; the command was not resent. Call environment to refresh Hand status, then inspect this Hand's current state read-only (for example, check for the command's expected effects) before deciding whether to retry it on the same Hand. Do not switch to SSH or another Hand for this call.`,
      "ambiguous", false, "receipt_unrecoverable",
      { error: detail, recovery, ...(recoveryError === undefined ? {} : { recovery_error: recoveryError }) });
  }

  /**
   * A fresh stub to the same owner object for each receipt read or cancel
   * delivery. A stub whose call threw (object reset, deploy, disconnect) stays
   * broken; the object name, ledger and pinned call runtime never change.
   */
  #ownerStub(): DurableObjectStub<AccountHostedTools> { return this.#namespace.getByName(this.#ownerId); }

  async #cancelAfterLoss(
    invocation: InvocationRequest,
    failed: (message: string, status: "ambiguous" | "unavailable", preAdmission?: boolean, reason?: HandFailureReason,
      extra?: Readonly<Record<string, unknown>>) => unknown,
    detail: string,
  ): Promise<unknown> {
    // Idempotent and bounded: own deadline per attempt, never the caller's aborted signal.
    let cancel = "unconfirmed";
    for (let attempt = 0; attempt < HAND_CANCEL_DELIVERY_ATTEMPTS && cancel === "unconfirmed"; attempt++) {
      if (attempt > 0) await abortableDelay(HAND_RECEIPT_RETRY_MS * attempt);
      try {
        cancel = await fetchResponseWithDeadline(this.#ownerStub(), "https://account-tools.internal/cancel-invocation", {
          method: "POST", headers: { "content-type": "application/json" },
          body: JSON.stringify({ owner_id: invocation.owner_id, session_id: invocation.session_id, call_id: invocation.call_id,
            machine_id: invocation.machine_id, name: invocation.name, route_token: invocation.route_token }),
        }, HAND_CANCEL_DELIVERY_TIMEOUT_MS, "Hand cancellation delivery", async response => {
          if (!response.ok) return "unconfirmed";
          const value = await response.json<{ cancel?: unknown }>().catch(() => undefined);
          return typeof value?.cancel === "string" && /^[a-z_]{1,32}$/.test(value.cancel) ? value.cancel : "unconfirmed";
        });
      } catch (error) {
        // Retry through a fresh stub; never retry an overloaded object.
        if (durableObjectErrorFlags(error).overloaded) break;
      }
    }
    // Delivery state only: no inputs, outputs or credentials.
    try { console.info({ type: "hand.receipt.cancel", tool: invocation.name, session_id: invocation.session_id,
      thread_id: invocation.thread_id, turn_id: invocation.turn_id, source_call_id: invocation.call_id,
      hand_id: invocation.machine_id, error_class: detail, cancel }); }
    catch { /* Diagnostics never change the cancellation outcome. */ }
    if (cancel === "fenced" || cancel === "not_dispatched") {
      // Ledger-backed: the call never reached the Hand and now cannot start.
      return failed(`The call was cancelled before it started on the Hand (${detail}); it was fenced so it can never run, and nothing was resent.`,
        "unavailable", false, "cancelled_before_start", { admitted: false, error: detail, cancel });
    }
    const what = cancel === "requested" ? "was sent to the Hand"
      : cancel === "queued" ? "was recorded and will be delivered when the same Hand runtime reconnects"
      : cancel === "terminal" ? "found the call already settled"
      : cancel === "forwarded" ? "was forwarded to the sharing account"
      : cancel === "not_delivered" ? "could not reach the Hand" : "could not be confirmed";
    return failed(`The call was cancelled after possible dispatch (${detail}); cancellation ${what}. Execution outcome is unknown. The command was not resent.`,
      "ambiguous", false, "cancelled_after_dispatch", { error: detail, cancel });
  }

  /** Bounded receipt-only polling of the original call identity on its original shard and runtime. */
  async #reconcileReceipt(
    invocation: InvocationRequest,
    context: InvocationContext,
    failed: (message: string, status: "ambiguous" | "unavailable", preAdmission?: boolean, reason?: HandFailureReason,
      extra?: Readonly<Record<string, unknown>>) => unknown,
    detail: string,
  ): Promise<{ result: unknown; polls: number } | { reason: HandReceiptFailure; error?: string; polls: number }> {
    let transportFailures = 0;
    const startedAt = Date.now();
    // Absolute wall budget, narrowed to the call's own admitted deadline once known.
    let budgetEnd = startedAt + HAND_RECEIPT_MAX_TOTAL_MS;
    // A not-yet-admitted original /invoke has no deadline; bound its wait like admission.
    const admissionEnd = startedAt + HAND_RECONNECT_ADMISSION_WAIT_MS + HAND_RECEIPT_DEADLINE_GRACE_MS;
    const correlation = { session_id: invocation.session_id, thread_id: invocation.thread_id, turn_id: invocation.turn_id };
    for (let poll = 0; poll < HAND_RECEIPT_MAX_POLLS; poll++) {
      if (context.signal?.aborted) return { result: await this.#cancelAfterLoss(invocation, failed, detail), polls: poll };
      if (!this.#allowed(context)) return { reason: "receipt_unauthorized", polls: poll };
      if (Date.now() >= budgetEnd) return { reason: "receipt_deadline", polls: poll };
      const polledAt = Date.now();
      const pollStarted = performance.now();
      let reply: { status: number; value: Record<string, unknown> | undefined };
      try {
        // Each read is bounded by the remaining absolute budget as well.
        reply = await withHardDeadline("Hand receipt reconciliation",
          Math.max(1, Math.min(HAND_RECEIPT_FETCH_TIMEOUT_MS, budgetEnd - Date.now())), async signal => {
          const response = await this.#ownerStub().fetch("https://account-tools.internal/invoke-receipt", {
            method: "POST", headers: { "content-type": "application/json" },
            body: JSON.stringify({ ...invocation, wait_ms: Math.max(0, Math.min(HAND_RECEIPT_WAIT_MS, budgetEnd - Date.now())) }),
            // Aborting a receipt read never cancels the call itself.
            signal: context.signal ? AbortSignal.any([signal, context.signal]) : signal,
          });
          let value: unknown;
          try { value = await response.json(); } catch { value = undefined; }
          return { status: response.status, value: value && typeof value === "object" ? value as Record<string, unknown> : undefined };
        });
      } catch (error) {
        if (context.signal?.aborted) continue;
        const sanitized = handErrorDetail(error, "transport");
        if (durableObjectErrorFlags(error).overloaded) return { reason: "receipt_unreachable", error: sanitized, polls: poll + 1 };
        if (++transportFailures >= HAND_RECEIPT_MAX_TRANSPORT_FAILURES) return { reason: "receipt_unreachable", error: sanitized, polls: poll + 1 };
        try { await abortableDelay(HAND_RECEIPT_RETRY_MS * 2 ** (transportFailures - 1), context.signal); } catch { /* Cancellation is handled above. */ }
        continue;
      }
      transportFailures = 0;
      const value = reply.value;
      if (reply.status === 200 && value?.receipt === "running") {
        if (typeof value.deadline_at === "number" && Number.isFinite(value.deadline_at)) {
          budgetEnd = Math.min(budgetEnd, value.deadline_at + HAND_RECEIPT_DEADLINE_GRACE_MS);
        }
        observeHandCall("account.receipt", invocation.name, pollStarted, "ambiguous", invocation.call_id, correlation);
        if (typeof value.deadline_at !== "number" && Date.now() >= admissionEnd) return { reason: "receipt_deadline", polls: poll + 1 };
        // A server reply without its bounded wait (e.g. at the deadline) must not spin.
        if (Date.now() - polledAt < HAND_RECEIPT_RETRY_MS) {
          try { await abortableDelay(HAND_RECEIPT_RETRY_MS, context.signal); } catch { /* Handled above. */ }
        }
        continue;
      }
      if (reply.status === 200 && validInvocationResult(value)) {
        // Cancellation and authority are rechecked after the await, before any output is returned.
        if (context.signal?.aborted) return { result: await this.#cancelAfterLoss(invocation, failed, detail), polls: poll + 1 };
        if (!this.#allowed(context)) return { reason: "receipt_unauthorized", polls: poll + 1 };
        observeHandCall("account.receipt", invocation.name, pollStarted, "ok", invocation.call_id, correlation);
        return { result: this.#brandedResult(value, invocation.name, invocation.machine_id), polls: poll + 1 };
      }
      if (reply.status === 404 && value?.receipt === "missing" && value.admission === "none") return { reason: "receipt_missing", polls: poll + 1 };
      // A revoked or forgotten share withholds the result; the outcome stays unknown.
      if (reply.status === 403 && value?.error === "receipt_unauthorized") return { reason: "receipt_unauthorized", polls: poll + 1 };
      return { reason: reply.status === 409 && value?.error === "receipt_unsupported" ? "receipt_unsupported" : "receipt_unresolved", polls: poll + 1 };
    }
    return { reason: "receipt_poll_limit", polls: HAND_RECEIPT_MAX_POLLS };
  }
}

function routeFor(provider: { resolve(name: string): HostedToolsCodeTool | undefined;
  machineTool(machineId: string, name: HostedMachineToolName): HostedToolsCodeTool | undefined },
name: string, machineId: string | undefined): HostedToolsCodeTool | undefined {
  return machineId === undefined ? provider.resolve(name) : provider.machineTool(machineId, name as HostedMachineToolName);
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

/**
 * Tool-facing transport state: never an instruction to manage the Hand.
 * admitted=false is ledger-backed non-admission; "unknown" means the call may
 * have run and is never resent automatically.
 */
function failedToolResult(
  message: string,
  status: "unavailable" | "ambiguous",
  preAdmissionUnavailable = false,
  reason?: HandFailureReason,
  extra: Readonly<Record<string, unknown>> = {},
): unknown {
  const outcome = { status, message, admitted: preAdmissionUnavailable ? false as const : status === "ambiguous" ? "unknown" as const : undefined,
    resent: false as const, ...(reason === undefined ? {} : { reason }), ...extra };
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

/** Account credentials, enrollment and VM creation never cross a Hand grant. */
function sharedMachineTool(name: string): boolean {
  return name === "exec_command" || name === "write_stdin" || name === "preview"
    || name === CUA_JS_NAME || name === CUA_RESET_NAME;
}

function parseSharedScreenRoute(token: string): { machineId: string; routeToken: string } | undefined {
  if (!token.startsWith("screen:v1:")) return undefined;
  try {
    const value: unknown = JSON.parse(token.slice("screen:v1:".length));
    if (Array.isArray(value) && value.length === 3 && typeof value[0] === "string"
      && value[0].startsWith("shared:") && typeof value[2] === "string" && value[2].startsWith("shared:v1:")) {
      return { machineId: value[0], routeToken: value[2] };
    }
  } catch { /* Malformed routes cannot obtain a grant. */ }
  return undefined;
}

async function screenInputDigest(input: unknown): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(input ?? null)));
  return Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, "0")).join("");
}

/** Keep recipient sessions disjoint within the publisher's bounded wire identifier. */
async function sharedHandSession(recipientId: string, sessionId: string): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([recipientId, sessionId])));
  return "shared:" + Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
}
