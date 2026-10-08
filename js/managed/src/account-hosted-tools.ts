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
import { HostedToolsBroker } from "./hosted-tools-broker";
import { observeHandCall, observeHandSummary } from "./hand-call-observation";
import { annotateToolSpan, traceToolInvocation } from "./tool-tracing";
import { DiagnosticJournal, diagnosticScope } from "./diagnostic-journal";
import { RegionalHandDirectory, HAND_RELAY_REGION_HEADER, handRelayName, isHandRelayRegion,
  relayRouteToken, parseRelayRouteToken, publisherIdentity, validPublisherId,
  type HandPublication, type HandRelayLocation, type HandRelayRegion, type RegionalHandEnv } from "./regional-hand-routing";
import type { RegionalHandRelay } from "./regional-hand-relay";
import { RegionalScreenAuthority, SCREEN_DIRECTORY_HEADER, regionalScreenPrefix, regionalScreenRegion, screenAuthorized,
  type RegionalScreenEnv, type ScreenFenceReason } from "./regional-screen-routing";
import { recordScreenPlaybackHostResult, type ScreenPlaybackEnv } from "./screen-playback";

type RetirementPublication = Pick<HandPublication, "route_id" | "publication_id" | "runtime_id" | "region"> & { machine: Pick<HostedMachine, "id"> };

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
  publications?: readonly HandPublication[];
  mount_roots?: Readonly<Record<string, string>>;
  /** Historical roots of the same identities; never projected as Hands. */
  mount_aliases?: Readonly<Record<string, readonly string[]>>;
  /**
   * Every identity the owner's registry still holds, assigned a root or not.
   * Present only on complete owner views; never merged from a selected-Hand lookup.
   */
  mount_registry?: Readonly<{ ids: readonly string[]; observed_at: number }>;
  inventory_unknown_ids?: readonly string[];
}>;

type RoutedHostedTool = HostedToolsCodeTool & Readonly<{
  provider: string;
  remoteName: string;
  summary?: string;
  timeoutMs: number;
}>;

type AccountHostedToolsEnv = RemoteICEEnv & RegionalHandEnv & RegionalScreenEnv & Partial<ScreenPlaybackEnv> & {
  NANOCODEX_ACCOUNT_TOOLS?: DurableObjectNamespace<AccountHostedTools>;
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
  readonly #shares: HandShareStore;
  readonly #sharedScreens = new Map<string, AbortController>();
  readonly #broker: HostedToolsBroker;
  readonly #remote: HandRemoteBroker;
  readonly #handHosts: HandHosts;
  readonly #diagnostics: DiagnosticJournal;
  #ownerId: string | undefined;
  readonly #regional: boolean;
  readonly #directory: RegionalHandDirectory;
  #handPathsValue?: HandPaths;
  /** One instance per object keeps reclamation ordered against this instance's assignments. */
  get #handPaths(): HandPaths { return this.#handPathsValue ??= new HandPaths(this.ctx.storage); }
  #publicationQueue: Promise<unknown> = Promise.resolve();
  #region: HandRelayRegion | undefined;
  /** Owner only: which location/generation may publish each machine's screens. */
  readonly #screens: RegionalScreenAuthority | undefined;

  constructor(ctx: DurableObjectState, env: AccountHostedToolsEnv, regional = false) {
    super(ctx, env);
    this.#shares = new HandShareStore(ctx.storage);
    this.#regional = regional;
    this.#directory = new RegionalHandDirectory(ctx.storage);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_local_publications (
      route_id TEXT PRIMARY KEY, candidate_id TEXT, publication_json TEXT
    )`);
    this.#region = ctx.storage.kv.get<HandRelayRegion>("regional_hand_region");
    // Thread-local tool hosts are not account Hands. Retire their derived index,
    // including overflow state, without changing any native routes or sessions.
    ctx.storage.sql.exec("DROP TABLE IF EXISTS workspace_hand_inventory");
    ctx.storage.kv.delete("workspace_hand_inventory_overflow");
    // Ownership is immutable; a new instance reloads it after eviction/restart.
    this.#ownerId = ctx.storage.kv.get<string>("owner_id");
    this.#diagnostics = new DiagnosticJournal(ctx.storage, "hand.broker");
    this.#broker = new HostedToolsBroker(ctx, { resumeRetainedSockets: true,
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
    this.#screens = regional ? undefined : new RegionalScreenAuthority(ctx.storage, (location, machineId, keep, reason) => this.#fenceScreens(location, machineId, keep, reason));
    this.#remote = new HandRemoteBroker(ctx, {
      onObservation: observation => {
        const record = { type: "hand.remote", ...(regional && this.#region ? { relay_region: this.#region } : {}), ...observation };
        try { console.info(record); } catch { /* Remote diagnostics cannot change a socket outcome. */ }
        this.#diagnostics.record(record);
      },
      claimCatalog: claim => regional ? this.#claimRegionalScreen(claim.machineId, claim.generation, claim.sequence)
        : this.#screens!.claim(claim.machineId, "legacy", claim.generation, claim.sequence),
      onClaimPublished: claim => { if (regional) void this.#confirmRegionalScreen(claim.machineId, claim.generation).catch(() => undefined); },
      nextSequence: () => this.#screenSequence(1),
      onHostResult: result => {
        const playback = this.env.NANOCODEX_SCREEN_PLAYBACK, owner = this.#ownerId;
        if (!playback || !owner) return;
        // Status only; never awaited by the host socket.
        void recordScreenPlaybackHostResult({ NANOCODEX_SCREEN_PLAYBACK: playback }, result, owner).catch(() => false);
      },
      idPrefix: () => regional && this.#region ? regionalScreenPrefix(this.#region) : "",
    });
    this.#handHosts = new HandHosts(ctx.storage, this.#remote);
  }

  async createHandShare(ownerId: string, machineId: string) {
    if (this.#regional || !isUserId(ownerId) || !this.#claim(ownerId) || typeof machineId !== "string"
      || machineId.startsWith("shared:")) return { error: "not_found" } as const;
    const snapshot = await this.#ownedSnapshot(machineId);
    if (!snapshot.machines.some(entry => entry.machine.id === machineId)
      && !snapshot.screens?.some(screen => screen.machine_id === machineId)) return { error: "not_found" } as const;
    if (this.#shares.list().filter(share => share.revoked_at === null).length >= 100) return { error: "share_limit" } as const;
    const share = await this.#shares.create(machineId);
    return share ? { id: share.id, token: share.token, machine_id: share.machine_id } : { error: "share_limit" } as const;
  }

  async listHandShares(ownerId: string) {
    return !this.#regional && isUserId(ownerId) && this.#owns(ownerId) ? this.#shares.list() : [];
  }

  async revokeHandShare(ownerId: string, id: string): Promise<boolean> {
    return !this.#regional && isUserId(ownerId) && this.#owns(ownerId) && this.#shares.revoke(id);
  }

  async redeemHandShare(recipientId: string, ownerId: string, token: string) {
    if (this.#regional || !isUserId(recipientId) || !isUserId(ownerId) || recipientId === ownerId
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
    if (this.#regional || !isUserId(ownerId) || !this.#owns(ownerId) || !isUserId(recipientId)
      || recipientId === ownerId) return undefined;
    return this.#shares.redeem(recipientId, token);
  }

  async hasHandShare(ownerId: string, recipientId: string, id: string): Promise<boolean> {
    return !this.#regional && this.#owns(ownerId) && !!this.#shares.grant(id, recipientId);
  }

  async sharedHandSnapshot(ownerId: string, recipientId: string, id: string): Promise<SharedMachineSnapshot | undefined> {
    if (this.#regional || !this.#owns(ownerId) || !isUserId(recipientId)) return undefined;
    const grant = this.#shares.grant(id, recipientId);
    if (!grant) return undefined;
    const snapshot = await this.#ownedSnapshot(grant.machine_id);
    // Recheck after asynchronous regional discovery; revocation may have interleaved.
    if (!this.#shares.grant(id, recipientId)) return undefined;
    const alias = `shared:${id}`;
    const screens = (snapshot.screens ?? []).filter(target => target.machine_id === grant.machine_id).flatMap(target => {
      const exposed = { ...target, machine_id: alias, machine_name: `${target.machine_name} (shared)` };
      const original = screenTool(target);
      const published = snapshot.tools.find(tool => tool.provider === "screens"
        && tool.definition.name === original.definition.name
        && (parseRelayRouteToken(tool.route_token)?.token ?? tool.route_token) === original.route_token);
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
    if (this.#regional || !this.#owns(ownerId) || !isUserId(recipientId)) return unavailable();
    const sharedSession = await sharedHandSession(recipientId, invocation.session_id);
    const route = this.#shares.resolve(invocation.route_token);
    const grant = route && this.#shares.grant(route.share_id, recipientId);
    if (!route || !grant || invocation.machine_id !== `shared:${grant.id}` || route.name !== invocation.name
      ) return unavailable();
    const screenRelay = parseRelayRouteToken(route.route_token);
    const screenRoute = screenRelay?.token ?? route.route_token;
    if (screenRoute.startsWith("screen:v1:")) {
      const snapshot = await this.#ownedSnapshot(grant.machine_id);
      const published = snapshot.tools.find(tool => tool.provider === "screens" && tool.route_token === route.route_token);
      if (!published || !this.#shares.grant(grant.id, recipientId)) return unavailable();
      if (screenRelay && !this.env.NANOCODEX_HAND_RELAYS) return unavailable();
      const forwarded = new Request("https://account-tools.internal/invoke", { method: "POST", signal,
        headers: { "content-type": "application/json" }, body: JSON.stringify({ ...invocation,
          owner_id: ownerId, machine_id: undefined, name: published.definition.name,
          route_token: screenRoute, session_id: sharedSession }) });
      return screenRelay
        ? this.env.NANOCODEX_HAND_RELAYS!.getByName(handRelayName(ownerId, screenRelay.region)).fetch(forwarded)
        : this.fetch(forwarded);
    }
    if (!sharedMachineTool(route.name)) return unavailable();
    const relay = parseRelayRouteToken(route.route_token);
    if (relay && !this.env.NANOCODEX_HAND_RELAYS) return unavailable();
    // The stored route, never recipient input, selects the machine, relay and tool.
    const forwarded = new Request("https://account-tools.internal/invoke", { method: "POST",
      signal, headers: { "content-type": "application/json" }, body: JSON.stringify({ ...invocation,
        owner_id: ownerId, machine_id: grant.machine_id, route_token: relay?.token ?? route.route_token,
        session_id: sharedSession }) });
    if (relay) this.#shares.turnTarget(sharedSession, invocation.turn_id,
      ownerId, relay.region, sharedSession);
    const response = relay
      ? await this.env.NANOCODEX_HAND_RELAYS!.getByName(handRelayName(ownerId, relay.region)).fetch(forwarded)
      : await this.fetch(forwarded);
    if (!response.ok) return response;
    const result = await response.json<InvocationResult>();
    return Response.json({ ...result, ...(result.process_route_token === undefined ? {} : {
      process_route_token: this.#shares.route(grant.id, "write_stdin", relay
        ? relayRouteToken(relay.region, result.process_route_token) : result.process_route_token),
    }) }, { headers: { "cache-control": "no-store" } });
  }

  async cancelSharedHand(ownerId: string, recipientId: string, invocation: InvocationRequest): Promise<void> {
    if (this.#regional || !this.#owns(ownerId) || !isUserId(recipientId)) return;
    const route = this.#shares.resolve(invocation.route_token);
    const grant = route && this.#shares.grant(route.share_id, recipientId, true);
    if (!route || !grant || invocation.machine_id !== `shared:${grant.id}` || invocation.name !== route.name) return;
    const session = await sharedHandSession(recipientId, invocation.session_id);
    this.#sharedScreens.get(JSON.stringify([session, invocation.call_id]))?.abort();
    const relay = parseRelayRouteToken(route.route_token);
    const request = new Request("https://account-tools.internal/cancel-invocation", {
      method:"POST",headers:{"content-type":"application/json"},
      body:JSON.stringify({owner_id:ownerId,session_id:session,call_id:invocation.call_id}),
    });
    if (relay) await this.env.NANOCODEX_HAND_RELAYS?.getByName(handRelayName(ownerId,relay.region)).fetch(request);
    else await this.fetch(request);
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
      // Preserve account identities when regional discovery itself failed.
      const local = this.#localSnapshot().machines.map(({ machine }) => inventoryEntry(machine, null));
      const retained = this.#directory.entries().map(({ machine }) => inventoryEntry(machine, null));
      return mergeInventory([local, retained], false);
    }
  }

  /**
   * Owner-initiated removal of one Hand from the routed catalog and from
   * regional routing. A Hand the owner no longer controls must still be
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
      for (const publication of selected ? [selected, ...selected.previous] : []) {
        if (publication.region === "legacy") continue;
        try {
          if (!this.env.NANOCODEX_HAND_RELAYS) throw new Error("relay unavailable");
          const status = await fetchResponseWithDeadline(
            this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(ownerId, publication.region)),
            "https://account-tools.internal/regional/forget", {
              method: "POST", headers: { [OWNER_ASSERTION]: ownerId, "content-type": "application/json" },
              body: JSON.stringify({ machine_id: machineId, publication_id: publication.publication_id,
                route_id: publication.route_id, runtime_id: publication.runtime_id, region: publication.region, force }),
            }, 5_000, "forget regional Hand", async response => response.status);
          if (status === 409) return { error: "hand_online" } as const;
          if (status !== 200) throw new Error("relay removal unconfirmed");
        } catch {
          if (!force) return { error: "hand_unknown" } as const;
          // Forced removal withdraws account routing even if the device's relay
          // cannot be reached. Retired runtime tombstones reject later claims.
        }
      }
      // Withdraw screen authority in every location; unconfirmed relays stay fenced-pending and unlisted.
      const screens = await this.#screens!.revoke(machineId);
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
      // Presence can change while earlier removals await another relay.
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
    if (this.#regional && !["/tool-host", "/snapshot", "/invoke", "/cancel-invocation", "/turn-ended", "/diagnostics",
      "/hands/host", "/hands/view", "/hands/renew", "/hands/screens"].includes(url.pathname)) {
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
      if (this.#regional) {
        // Regional relays serve only native account publishers and their viewers;
        // VM and server publishers keep owner-local revocation.
        const region = request.headers.get(HAND_RELAY_REGION_HEADER);
        if (vm || !isHandRelayRegion(region) || (this.#region && this.#region !== region)) return Response.json({ error: "not_found" }, { status: 404 });
        if (!this.#region) { this.#region = region; this.ctx.storage.kv.put("regional_hand_region", region); }
      } else if (url.pathname === "/hands/screens" && request.method === "GET" && !url.search && !vm) {
        // Only the current authority is listed. Workers merge regional catalogs.
        if (this.#screens!.pending()) void this.#screens!.retryPending().catch(() => undefined);
        const authority = this.#screens!.hosts();
        const surfaces = this.#remote.list().filter(target => screenAuthorized(authority, "legacy", target.machine_id, target.generation));
        return Response.json({ surfaces, ...(request.headers.get(SCREEN_DIRECTORY_HEADER) === "1" ? {
          regional_hosts: Object.fromEntries([...authority].filter(([, host]) => host.region !== "legacy" && host.generation)) } : {}) },
        { headers: { "cache-control": "no-store" } });
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
      const body = await request.json<{ owner_id?: unknown; machine_id?: unknown }>();
      const ownerId = body.owner_id;
      if (!isUserId(ownerId) || !this.#owns(ownerId)) {
        return Response.json({ error: "not_found" }, { status: 404 });
      }
      if (body.machine_id !== undefined && (typeof body.machine_id !== "string" || !body.machine_id || body.machine_id.length > 256)) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      return Response.json(await this.#snapshot(body.machine_id as string | undefined), { headers: { "cache-control": "no-store" } });
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
        const stub = target.region === "account"
          ? this.env.NANOCODEX_ACCOUNT_TOOLS?.getByName(target.owner_id)
          : this.env.NANOCODEX_HAND_RELAYS?.getByName(handRelayName(target.owner_id, target.region as HandRelayRegion));
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
      if (body.machine_id?.startsWith("shared:")) {
        const share = this.#shares.received().find(entry => body.machine_id === `shared:${entry.id}`);
        if (share) await this.env.NANOCODEX_ACCOUNT_TOOLS?.getByName(share.owner_id).cancelSharedHand(share.owner_id, body.owner_id, body);
      } else {
        this.#sharedScreens.get(JSON.stringify([body.session_id, body.call_id]))?.abort();
        const row = this.ctx.storage.sql.exec<{call_id:string}>("SELECT call_id FROM hosted_tool_calls WHERE session_id=? AND source_call_id=?", body.session_id, body.call_id).toArray()[0];
        if (row) this.#broker.cancel(row.call_id);
      }
      return new Response(null,{status:204});
    }
    if (request.method === "POST" && url.pathname === "/shared-invoke") {
      const body = await request.json<{owner_id:string;recipient_id:string;invocation:InvocationRequest}>();
      return this.#invokeSharedHand(body.owner_id, body.recipient_id, body.invocation, request.signal);
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
        if (this.#regional || !share || !this.env.NANOCODEX_ACCOUNT_TOOLS) {
          return Response.json({ error: "tool_unavailable" }, { status: 404 });
        }
        this.#shares.turnTarget(invocation.session_id, invocation.turn_id, share.owner_id, "account",
          await sharedHandSession(invocation.owner_id, invocation.session_id));
        return this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(share.owner_id).fetch("https://account-tools.internal/shared-invoke", {
          method:"POST", signal:request.signal, headers:{"content-type":"application/json"},
          body:JSON.stringify({owner_id:share.owner_id,recipient_id:invocation.owner_id,invocation}),
        });
      }
      const correlation = { session_id: invocation.session_id, thread_id: invocation.thread_id,
        source_call_id: invocation.call_id, turn_id: invocation.turn_id };
      const ownedAt = performance.now();
      observeHandCall("account.decode_input", invocation.name, startedAt, "ok", invocation.call_id, correlation, decodedAt);
      observeHandCall("account.ownership", invocation.name, decodedAt, "ok", invocation.call_id, correlation, ownedAt);
      if (invocation.machine_id === undefined && invocation.route_token.startsWith("screen:v1:")) {
        const key = JSON.stringify([invocation.session_id, invocation.call_id]);
        const controller = new AbortController(); this.#sharedScreens.set(key, controller);
        try {
          const remote = await traceToolInvocation("hand.account.invoke", invocation.thread_id, invocation.name, {
            sessionId: invocation.session_id, callId: invocation.call_id, turnId: invocation.turn_id,
          }, () => this.#remote.invoke(invocation.name, invocation.route_token,
            invocation.input, invocation.session_id, AbortSignal.any([request.signal, controller.signal]),
            { threadId: invocation.thread_id, callId: invocation.call_id, turnId: invocation.turn_id }));
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
    const authority = this.#screens?.hosts();
    const screens = this.#remote.list(true).filter(target => target.agent_tools
      && (!authority || screenAuthorized(authority, "legacy", target.machine_id, target.generation)));
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

  async #snapshot(machineId?: string): Promise<AccountHostedToolsSnapshot> {
    const owned = await this.#ownedSnapshot(machineId);
    if (this.#regional || !this.env.NANOCODEX_ACCOUNT_TOOLS || !this.#ownerId) return owned;
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

  async #ownedSnapshot(machineId?: string): Promise<AccountHostedToolsSnapshot> {
    const full = this.#localSnapshot();
    // A selected route needs only its current publication and capabilities.
    // Inventory remains explicit; this request never probes unrelated regions.
    const local = machineId === undefined ? full : {
      ...full,
      tools: full.tools.filter(tool => (full.screens ?? []).some(target => target.machine_id === machineId
        && screenTool(target).route_token === tool.route_token && tool.provider === "screens")),
      screens: (full.screens ?? []).filter(target => target.machine_id === machineId),
      machines: full.machines.filter(entry => entry.machine.id === machineId),
    };
    if (this.#regional) return { ...local, publications: this.ctx.storage.sql.exec<{ publication_json: string }>(
      "SELECT publication_json FROM regional_local_publications WHERE publication_json IS NOT NULL").toArray().map(row => JSON.parse(row.publication_json) as HandPublication) };
    const directory = this.#directory.entries().filter(entry => machineId === undefined || entry.machine.id === machineId);
    const screenAuthority = [...this.#screens!.hosts()].filter(([machine, host]) => host.region !== "legacy" && host.generation
      && (machineId === undefined || machine === machineId));
    if (!directory.length && !screenAuthority.length) return this.#withRoots(local, machineId === undefined);
    const regions = [...new Set([...directory.filter(entry => !entry.pending && entry.region !== "legacy").map(entry => entry.region as HandRelayRegion),
      ...screenAuthority.map(([, host]) => host.region as HandRelayRegion)])];
    const snapshots = await Promise.all(regions.map(async region => {
      try {
        if (!this.env.NANOCODEX_HAND_RELAYS) return undefined;
        const response = await fetchResponseWithDeadline(this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(this.#ownerId!, region)),
          "https://account-tools.internal/snapshot", { method: "POST", headers: { "content-type": "application/json" },
            body: JSON.stringify({ owner_id: this.#ownerId, ...(machineId === undefined ? {} : { machine_id: machineId }) }) }, 5_000, "regional Hand discovery",
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
    // Regional screens: only the owner's current authority, routed through its relay.
    const screens = [...local.screens ?? []];
    for (const [machine, host] of screenAuthority) {
      const remote = snapshots.find(snapshot => snapshot?.region === host.region)?.snapshot;
      for (const target of remote?.screens ?? []) {
        if (target.machine_id !== machine || target.generation !== host.generation) continue;
        screens.push(target);
        const route = screenTool(target).route_token;
        for (const tool of remote!.tools) if (tool.provider === "screens" && tool.route_token === route) {
          tools.push({ ...tool, route_token: relayRouteToken(host.region as HandRelayRegion, tool.route_token) });
        }
      }
    }
    return this.#withRoots({ tools, machines: [...machines.values()], screens, inventory_unknown_ids: inventoryUnknownIds },
      machineId === undefined);
  }

  #withRoots(snapshot: AccountHostedToolsSnapshot, complete = false): AccountHostedToolsSnapshot {
    // Read the registry synchronously with assignment: no owner deletion or
    // publication can interleave between the two.
    const registry = this.#regional ? undefined : this.#registry();
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
   * Every identity the account still owns, independent of presence, relay
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

  #fencePublication(publication: RetirementPublication): void {
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

  // No awaits between this check and the exact local fence. Unknown or changing
  // publications must never be mistaken for disconnected hardware.
  #regionalRetirementStatus(publication: RetirementPublication) {
    const local = this.ctx.storage.sql.exec<{ candidate_id: string | null; publication_json: string | null }>(
      "SELECT candidate_id,publication_json FROM regional_local_publications WHERE route_id=?", publication.route_id).toArray()[0];
    const active = local?.publication_json ? JSON.parse(local.publication_json) as HandPublication : undefined;
    const changed = (active !== undefined && active.publication_id !== publication.publication_id)
      || (local?.candidate_id != null && local.candidate_id !== publication.publication_id);
    const online = this.#broker.machineOnline(publication.machine.id);
    const pending = this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id IS ? AND state IN ('admitted','dispatched')", publication.machine.id, publication.runtime_id ?? null).toArray()[0]!.count;
    return { online, pending_calls: pending, publication_changed: changed, retirable: !changed && !online && pending === 0 };
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

  async #regionalRetirementRPC(publication: HandPublication, operation: "inspect" | "retire-inactive", abandonPending = false) {
    if (publication.region === "legacy" || !this.env.NANOCODEX_HAND_RELAYS) throw new Error("regional relay unavailable");
    return fetchResponseWithDeadline(this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(this.#ownerId!, publication.region)),
      `https://account-tools.internal/regional/${operation}`, {
        method: "POST", headers: { [OWNER_ASSERTION]: this.#ownerId!, "content-type": "application/json" },
        // Device metadata and catalog names are not retirement authority and may
        // exceed the bounded control endpoint even for ordinary native Hands.
        body: JSON.stringify({ machine_id: publication.machine.id, runtime_id: publication.runtime_id,
          route_id: publication.route_id, publication_id: publication.publication_id, region: publication.region,
          ...(abandonPending ? { abandon_pending: true } : {}) }),
      }, 5_000, "regional Hand retirement", async response => {
        if (!response.ok) throw new Error("regional publication is not inactive");
        return response.json<{ online: boolean; pending_calls: number; retirable: boolean; retired?: boolean }>();
      });
  }

  async #retireRegional(body: Record<string, unknown>): Promise<Response> {
    if ((body.abandon_pending !== undefined && body.abandon_pending !== true)
      || Object.keys(body).length !== (body.abandon_pending === true ? 5 : 4) || !validPublisherId(body.machine_id) || !validPublisherId(body.runtime_id)
      || !validPublisherId(body.publication_id) || !isHandRelayRegion(body.region)) return Response.json({ error: "invalid_request" }, { status: 400 });
    const result = this.#publicationQueue.then(async () => {
      const receiptKey = `regional_retirement:${body.machine_id}`;
      const receipt = this.ctx.storage.kv.get<{ publication_id: string; runtime_id: string; region: string }>(receiptKey);
      if (receipt && receipt.publication_id === body.publication_id && receipt.runtime_id === body.runtime_id && receipt.region === body.region) {
        return Response.json({ retired: true, ...body });
      }
      const current = this.#directory.entries().find(entry => entry.machine.id === body.machine_id);
      if (!current || current.pending || current.publication_id !== body.publication_id
        || current.runtime_id !== body.runtime_id || current.region !== body.region) {
        return Response.json({ error: "regional_publication_changed" }, { status: 409 });
      }
      try {
        const status = await this.#regionalRetirementRPC(current, "retire-inactive", body.abandon_pending === true);
        if (!status.retired) throw new Error("retirement not confirmed");
      } catch { return Response.json({ error: "regional_retirement_unconfirmed" }, { status: 409 }); }
      this.ctx.storage.transactionSync(() => {
        this.#directory.retirePublication(current);
        this.ctx.storage.kv.put(receiptKey, { publication_id: body.publication_id, runtime_id: body.runtime_id, region: body.region });
      });
      return Response.json({ retired: true, ...body });
    });
    this.#publicationQueue = result.then(() => {}, () => {});
    return result;
  }

  async #regionalRequest(request: Request, url: URL): Promise<Response> {
    const owner = request.headers.get(OWNER_ASSERTION);
    if (!isUserId(owner) || !this.#claim(owner)) return Response.json({ error: "not_found" }, { status: 404 });
    if (url.pathname === "/regional/status" && !this.#regional && request.method === "GET" && !url.search) {
      const rows = this.ctx.storage.sql.exec<{ runtime_id: string | null; generation: number; machines_json: string | null }>(
        "SELECT runtime_id,generation,machines_json FROM hosted_tool_routes WHERE machines_json IS NOT NULL").toArray();
      const legacy = rows.flatMap(row => (JSON.parse(row.machines_json!) as HostedMachine[]).map(machine => {
        const pending = this.ctx.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM hosted_tool_calls WHERE hand_id=? AND host_runtime_id IS ? AND state IN ('admitted','dispatched')", machine.id, row.runtime_id).toArray()[0]!.count;
        const online = this.#broker.machineOnline(machine.id);
        return { machine_id: machine.id, runtime_id: row.runtime_id, generation: row.generation, online, pending_calls: pending, retirable: !online && pending === 0 };
      }));
      const regional = await Promise.all(this.#directory.entries().filter(entry => entry.region !== "legacy").map(async entry => {
        const identity = { machine_id: entry.machine.id, runtime_id: entry.runtime_id ?? null, publication_id: entry.publication_id, region: entry.region };
        if (!entry.pending && entry.runtime_id) try {
          const status = await this.#regionalRetirementRPC(entry, "inspect");
          return { ...identity, ...status, status: "confirmed", pending_publication: false };
        } catch { /* Discovery failure is unknown, never evidence of inactivity. */ }
        return { ...identity, status: "unknown", online: null, pending_calls: null, pending_publication: entry.pending, retirable: false };
      }));
      return Response.json({ legacy, regional }, { headers: { "cache-control": "no-store" } });
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
      if (body.publication_id !== undefined || body.region !== undefined) return this.#retireRegional(body);
      const unversioned = body.runtime_id === null;
      const abandonPending = body.abandon_pending === true;
      if ((body.abandon_pending !== undefined && !abandonPending) || !validPublisherId(body.machine_id) || (unversioned
        ? !Number.isSafeInteger(body.generation) || (body.generation as number) <= 0 || Object.keys(body).length !== (abandonPending ? 4 : 3)
        : !validPublisherId(body.runtime_id) || Object.keys(body).length !== (abandonPending ? 3 : 2))) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      if (!unversioned && this.#directory.retired(body.machine_id, body.runtime_id as string)
        && this.#directory.placement(body.machine_id, body.runtime_id as string) === "legacy") {
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
    if (this.#regional && url.pathname === "/regional/forget") {
      if (!validPublisherId(body.machine_id) || !validPublisherId(body.runtime_id) || !validPublisherId(body.publication_id)
        || body.region !== this.#region || typeof body.route_id !== "string" || body.route_id.length > 512
        || typeof body.force !== "boolean" || Object.keys(body).length !== 6) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      const publication: RetirementPublication = { machine: { id: body.machine_id }, runtime_id: body.runtime_id,
        publication_id: body.publication_id, route_id: body.route_id, region: this.#region! };
      const status = this.#regionalRetirementStatus(publication);
      if (status.publication_changed || (!body.force && (status.online || status.pending_calls > 0))) {
        return Response.json({ error: "regional_publication_not_inactive" }, { status: 409 });
      }
      this.ctx.storage.transactionSync(() => {
        this.#fencePublication(publication);
        this.#broker.retireRoute(publication.route_id, "Owner forgot this Hand");
        if (body.force) this.#settleAbandonedCalls(body.machine_id as string, body.runtime_id as string);
      });
      return Response.json({ forgotten: true });
    }
    if (this.#regional && (url.pathname === "/regional/inspect" || url.pathname === "/regional/retire-inactive")) {
      const abandonPending = body.abandon_pending === true;
      if (!validPublisherId(body.machine_id) || !validPublisherId(body.runtime_id) || !validPublisherId(body.publication_id)
        || body.region !== this.#region || typeof body.route_id !== "string" || body.route_id.length > 512
        || (body.abandon_pending !== undefined && !abandonPending)
        || Object.keys(body).length !== (abandonPending ? 6 : 5)) return Response.json({ error: "invalid_request" }, { status: 400 });
      const publication: RetirementPublication = { machine: { id: body.machine_id }, runtime_id: body.runtime_id,
        publication_id: body.publication_id, route_id: body.route_id, region: this.#region! };
      const status = this.#regionalRetirementStatus(publication);
      if (url.pathname === "/regional/inspect") return Response.json(status);
      if (status.online || status.publication_changed || (status.pending_calls > 0 && !abandonPending))
        return Response.json({ error: "regional_publication_not_inactive" }, { status: 409 });
      this.ctx.storage.transactionSync(() => {
        this.#fencePublication(publication);
        if (abandonPending) this.#settleAbandonedCalls(body.machine_id as string, body.runtime_id as string);
      });
      return Response.json({ ...status, retired: true });
    }
    if (url.pathname === "/regional/screen-claim" && !this.#regional) {
      const region = body.region, generation = body.generation;
      if (!validPublisherId(body.machine_id) || !isHandRelayRegion(region) || typeof generation !== "string"
        || !validPublisherId(generation) || regionalScreenRegion(generation) !== region
        || !Number.isSafeInteger(body.sequence) || (body.sequence as number) < 1 || Object.keys(body).length !== 4) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      const granted = await this.#screens!.claim(body.machine_id, region, generation, body.sequence as number);
      // Authority decisions only: no endpoint, SDP or credential data.
      try { console.info({ type: "hand.screen.claim", hand_id: body.machine_id, region, sequence: body.sequence, granted }); } catch { /* Passive. */ }
      return Response.json({ granted }, { headers: { "cache-control": "no-store" } });
    }
    if (url.pathname === "/regional/screen-confirm" && !this.#regional) {
      if (!validPublisherId(body.machine_id) || !isHandRelayRegion(body.region) || typeof body.generation !== "string"
        || Object.keys(body).length !== 3) return Response.json({ error: "invalid_request" }, { status: 400 });
      await this.#screens!.confirm(body.machine_id, body.region, body.generation);
      return Response.json({ confirmed: true });
    }
    if (url.pathname === "/regional/screen-fence" && this.#regional) {
      if (!validPublisherId(body.machine_id) || (body.keep !== undefined && !validPublisherId(body.keep))
        || !["host_replaced", "publisher_revoked"].includes(body.reason as string)) return Response.json({ error: "invalid_request" }, { status: 400 });
      this.#remote.fenceMachine(body.machine_id, body.keep as string | undefined, body.reason as ScreenFenceReason);
      return Response.json({ fenced_through: this.#screenSequence(0) });
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

  /** Owner fence of one screen location. Legacy is this object: synchronous, never a self fetch. */
  /** Durable monotonic host-socket counter; `step` 0 reads the current high-water mark. */
  #screenSequence(step: 0 | 1): number {
    const next = (this.ctx.storage.kv.get<number>("screen_host_sequence") ?? 0) + step;
    if (step) this.ctx.storage.kv.put("screen_host_sequence", next);
    return next;
  }

  async #fenceScreens(location: HandRelayLocation, machineId: string, keep: string | undefined, reason: ScreenFenceReason): Promise<number | false> {
    if (location === "legacy") { this.#remote.fenceMachine(machineId, keep, reason); return this.#screenSequence(0); }
    if (!this.env.NANOCODEX_HAND_RELAYS || !this.#ownerId) return false;
    return fetchResponseWithDeadline(this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(this.#ownerId, location)),
      "https://account-tools.internal/regional/screen-fence", { method: "POST",
        headers: { [OWNER_ASSERTION]: this.#ownerId, "content-type": "application/json" },
        body: JSON.stringify({ machine_id: machineId, ...(keep === undefined ? {} : { keep }), reason }) }, 5_000, "fence regional screen",
      async response => response.ok ? (await response.json<{ fenced_through: number }>()).fenced_through : false).catch(() => false as const);
  }

  /** Relay publication admission. Rejection or uncertainty closes the publisher. */
  async #confirmRegionalScreen(machineId: string, generation: string): Promise<void> {
    if (!this.#ownerId || !this.#region || !this.env.NANOCODEX_ACCOUNT_TOOLS) return;
    await fetchResponseWithDeadline(this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(this.#ownerId),
      "https://account-tools.internal/regional/screen-confirm", { method: "POST",
        headers: { [OWNER_ASSERTION]: this.#ownerId, "content-type": "application/json" },
        body: JSON.stringify({ machine_id: machineId, region: this.#region, generation }) }, 5_000, "confirm regional screen", () => undefined);
  }

  async #claimRegionalScreen(machineId: string, generation: string, sequence: number): Promise<boolean> {
    if (!this.#ownerId || !this.#region || !this.env.NANOCODEX_ACCOUNT_TOOLS) return false;
    return fetchResponseWithDeadline(this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(this.#ownerId),
      "https://account-tools.internal/regional/screen-claim", { method: "POST",
        headers: { [OWNER_ASSERTION]: this.#ownerId, "content-type": "application/json" },
        body: JSON.stringify({ machine_id: machineId, region: this.#region, generation, sequence }) }, 8_000, "claim regional screen",
      async response => response.ok && (await response.json<{ granted?: unknown }>()).granted === true);
  }

  /** Portable playback command (internal binding only); the owner forwards to the authority's relay. */
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
    const authority = this.#screens?.hosts().get(body.machine_id);
    if (authority && authority.region !== "legacy") {
      if (!this.env.NANOCODEX_HAND_RELAYS) return Response.json({ error: "host_unavailable" }, { status: 503 });
      if (generation !== undefined && generation !== authority.generation) return Response.json({ error: "stale_generation" }, { status: 409 });
      return this.env.NANOCODEX_HAND_RELAYS.getByName(handRelayName(owner, authority.region)).fetch("https://account-tools.internal/screens/host-command", {
        method: "POST", headers: { [OWNER_ASSERTION]: owner, "content-type": "application/json" },
        body: JSON.stringify({ ...body, generation: authority.generation }) });
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
type HandFailureReason = "route_unavailable_after_recovery" | "route_replaced" | "route_unpublished" | "route_refresh_failed"
  | "process_runtime_replaced" | "transport_failed" | "outcome_unknown";

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
  readonly #relays: DurableObjectNamespace<RegionalHandRelay> | undefined;
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

  async endTurn(sessionId: string, turnId: string, hookEventName: "Stop" | "Interrupt" | "SubagentStop"): Promise<void> {
    const key = JSON.stringify([sessionId, turnId]);
    const targets = this.#turnTargets.get(key);
    this.#turnTargets.delete(key);
    await Promise.all([...targets?.values() ?? []].map(async target => {
      const response = await target.fetch("https://account-tools.internal/turn-ended", {
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

  /** Fresh selected-machine lookup. Never joins a slow full inventory request. */
  async refreshMachine(machineId: string, context: AuthorizationContext, computer = false): Promise<void> {
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
    const snapshot = await fetchResponseWithDeadline(
      this.#namespace.getByName(this.#ownerId), "https://account-tools.internal/snapshot",
      { method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ owner_id: this.#ownerId, machine_id: machineId }) },
      10_000, "selected Hand lookup", async response => {
        if (!response.ok) throw new Error("Selected Hand lookup unavailable");
        return response.json<unknown>();
      }).catch(error => {
        throw Object.assign(new Error("Selected Hand lookup interrupted", { cause: error }), { code: "host_interrupted" });
      });
    if (generation !== this.#generation || !this.#allowed(context)) throw new Error("Hand authorization changed during lookup");
    if (!validSnapshot(snapshot) || snapshot.inventory_unknown_ids?.includes(machineId)
      || snapshot.machines.some(entry => entry.machine.id !== machineId)
      || (!computer && (snapshot.machines.length !== 1 || snapshot.machines[0]?.online !== true)))
      throw new Error("Selected Hand route unavailable");
    // Replace only this machine's screen routes; unrelated catalogs and cells survive.
    const removed = new Set((this.#snapshot.screens ?? []).filter(target => target.machine_id === machineId)
      .map(target => screenTool(target).route_token));
    const screens = (snapshot.screens ?? []).filter(target => target.machine_id === machineId);
    const routes = new Set(screens.map(target => screenTool(target).route_token));
    // A selected lookup cannot prove the earlier full registry is still current.
    const { mount_registry: _stale, ...current } = this.#snapshot;
    this.#publish({ ...current,
      screens: [...(this.#snapshot.screens ?? []).filter(target => target.machine_id !== machineId), ...screens],
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
      // Regional screens keep their exact route inside the relay envelope.
      if (!tool || tool.provider !== "screens" || (tool.routeToken !== expected.route_token
        && parseRelayRouteToken(tool.routeToken ?? "")?.token !== expected.route_token)) continue;
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
    region: string | undefined,
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
    if (!route?.routeToken || route.routeToken === routeToken
      || parseRelayRouteToken(route.routeToken)?.region !== region) {
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
    const failed = (message: string, status: "ambiguous" | "unavailable", preAdmission = false, reason?: HandFailureReason): unknown => {
      observeHandCall("account.fetch", name, startedAt, status, context.callId, correlation);
      return failedToolResult(message, status, preAdmission, reason ?? (status === "ambiguous" ? "outcome_unknown" : undefined));
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
    if (context.turnId !== undefined) {
      const key = JSON.stringify([context.sessionId, context.turnId]);
      let targets = this.#turnTargets.get(key);
      if (!targets) { targets = new Map(); this.#turnTargets.set(key, targets); }
      targets.set(relay?.region ?? "account", target);
    }
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
      if (context.signal?.aborted && (routeToken.startsWith("shared:") || parseSharedScreenRoute(routeToken))) {
        try {
          await target.fetch("https://account-tools.internal/cancel-invocation", {
            method:"POST",headers:{"content-type":"application/json"},
            body:JSON.stringify({owner_id:this.#ownerId,session_id:context.sessionId,call_id:context.callId,
              machine_id:machineId,name,route_token:routeToken}),
          });
        } catch { /* Cancellation delivery is best effort; execution remains uncertain. */ }
      }
      timing.fetch_ms = performance.now() - startedAt;
      return failed("Hand connection failed after possible dispatch; execution outcome is unknown. The command was not resent.", "ambiguous", false, "transport_failed");
    }
    const responseAt = performance.now();
    timing.fetch_ms = responseAt - startedAt;
    if (response.ok) observeHandCall("account.fetch", name, startedAt, "ok", context.callId, correlation);
    if (!response.ok) {
      const preAdmission = response.status === 404 || response.status === 409;
      // Only the target shard's explicit ledger evidence proves non-admission;
      // a bare status, unreadable body or older account worker does not.
      let neverAdmitted = false;
      if (preAdmission) {
        try { neverAdmitted = (await response.json<{ admission?: unknown }>()).admission === "none"; }
        catch { /* Unknown evidence keeps the call pinned and its outcome unknown. */ }
      } else {
        try { await response.body?.cancel(); } catch { /* Body is irrelevant to a failed status. */ }
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
        if (route?.routeToken && route.routeToken !== routeToken
          && parseRelayRouteToken(route.routeToken)?.region === relay?.region) {
          return this.#invoke(name, route.routeToken, input, context, machineId, "fixed");
        }
      }
      if (neverAdmitted && this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        return this.#repinNeverAdmitted(name, routeToken, relay?.region, input, context, machineId, failed);
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
      // Report precise transport state instead of indefinitely replaying the turn.
      // Do not infer this from discovery or HTTP errors: an earlier attempt may
      // have been admitted and must retain its identity for receipt recovery.
      // The broker already waited (bounded) for the same runtime epoch. If the
      // physical Hand republished under a replacement route, move this
      // never-admitted call there once instead of asking the user to unblock.
      if (this.#callRoutes && routePolicy === "refresh" && name !== "write_stdin" && !context.signal?.aborted) {
        const moved = await this.#repinNeverAdmitted(name, routeToken, relay?.region, input, context, machineId, failed, true);
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
): unknown {
  const outcome = { status, message, admitted: preAdmissionUnavailable ? false as const : status === "ambiguous" ? "unknown" as const : undefined,
    resent: false as const, ...(reason === undefined ? {} : { reason }) };
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

/** Keep recipient sessions disjoint within the publisher's bounded wire identifier. */
async function sharedHandSession(recipientId: string, sessionId: string): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([recipientId, sessionId])));
  return "shared:" + Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
}
