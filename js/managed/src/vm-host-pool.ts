import { DurableObject } from "cloudflare:workers";

import { isUserId } from "./account-auth";
import { isVmFactoryName } from "./vm-factory-name";
import {
  VM_HOST_DONOR as DONOR_ID,
  VM_HOST_POOL_AGENT as POOL_AGENT,
  VM_HOST_POOL_LOCATOR as POOL_LOCATOR,
  VM_HOST_POOL_OWNER as POOL_OWNER,
  VM_HOST_POOL_SCOPE as POOL_SCOPE,
  VM_HOST_PUBLIC_ORIGIN as PUBLIC_ORIGIN,
  vmHostAttachmentRouteId,
} from "./vm-host-boundary";
import {
  VM_HOST_LEASE_MS,
  VM_HOST_PROTOCOL_VERSION,
  VmHostProtocolError,
  matchesVmHostLease,
  parseVmHostCommand,
  type VmHostCommand,
  type VmHostServerMessage,
  type VmShape,
} from "./vm-host-protocol";

const OPAQUE = /^[A-Za-z0-9_-]{43}$/;
const SAFE_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

type PoolScope = "agent" | "account" | "system";
type AllocationState = "provisioning" | "ready" | "releasing" | "released";

type PoolClaim = {
  scope: PoolScope;
  owner_id: string | null;
  agent_id: string | null;
  pool_locator: string;
};

type HostRow = {
  host_id: string;
  factory_name: string;
  donor_id: string;
  max_vms: number;
  vm_cpus: number;
  vm_memory_mib: number;
  public_origin: string;
  epoch: number;
  lease_id: string | null;
  lease_expires_at: number;
};

type AllocationRow = {
  allocation_id: string;
  generation: number;
  factory_name: string;
  owner_id: string;
  organization_id: string;
  team_id: string;
  authorization_epoch: number;
  agent_id: string;
  mount_id: string;
  public_origin: string;
  pool_locator: string;
  machine_id: string;
  host_id: string;
  lease_id: string | null;
  host_epoch: number | null;
  slot: number;
  bearer: string;
  state: AllocationState;
  created_at: number;
  updated_at: number;
};

type HostAttachment = {
  kind: "vm-host";
  donorId: string;
  publicOrigin: string;
  hostId?: string;
  leaseId?: string;
  epoch?: number;
};

type HostClaim = PoolClaim & { donorId: string; publicOrigin: string };

type AcquireRequest = {
  factory_name: string;
  owner_id: string;
  organization_id: string;
  team_id: string;
  authorization_epoch: number;
  agent_id: string;
  mount_id: string;
  pool_locator: string;
};

type AllocationIdentity = {
  owner_id: string;
  agent_id: string;
  mount_id: string;
  allocation_id: string;
  generation: number;
  pool_locator: string;
};

type AllocationReleaseIntent = AcquireRequest;

export type VmHostPoolEnv = {
  NANOCODEX_SESSIONS: DurableObjectNamespace;
};

/** A scope-keyed, durable scheduler for independently connected VM hosts. */
export class VmHostPool extends DurableObject<VmHostPoolEnv> {
  readonly #env: VmHostPoolEnv;

  constructor(ctx: DurableObjectState, env: VmHostPoolEnv) {
    super(ctx, env);
    this.#env = env;
    this.ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS vm_pool_claim (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        scope TEXT NOT NULL CHECK (scope IN ('agent', 'account', 'system')),
        owner_id TEXT,
        agent_id TEXT,
        pool_locator TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS vm_hosts (
        host_id TEXT PRIMARY KEY,
        factory_name TEXT NOT NULL UNIQUE,
        donor_id TEXT NOT NULL,
        max_vms INTEGER NOT NULL CHECK (max_vms BETWEEN 1 AND 64),
        vm_cpus INTEGER NOT NULL CHECK (vm_cpus BETWEEN 1 AND 64),
        vm_memory_mib INTEGER NOT NULL CHECK (vm_memory_mib BETWEEN 128 AND 262144),
        public_origin TEXT NOT NULL,
        epoch INTEGER NOT NULL DEFAULT 0,
        lease_id TEXT,
        lease_expires_at INTEGER NOT NULL DEFAULT 0
      );
      CREATE TABLE IF NOT EXISTS vm_allocations (
        allocation_id TEXT PRIMARY KEY,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        factory_name TEXT NOT NULL,
        owner_id TEXT NOT NULL,
        organization_id TEXT NOT NULL,
        team_id TEXT NOT NULL,
        authorization_epoch INTEGER NOT NULL CHECK (authorization_epoch >= 0),
        agent_id TEXT NOT NULL,
        mount_id TEXT NOT NULL,
        public_origin TEXT NOT NULL,
        pool_locator TEXT NOT NULL,
        machine_id TEXT NOT NULL,
        host_id TEXT NOT NULL,
        lease_id TEXT,
        host_epoch INTEGER,
        slot INTEGER NOT NULL CHECK (slot >= 0),
        bearer TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('provisioning', 'ready', 'releasing', 'released')),
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(owner_id, agent_id, mount_id),
        FOREIGN KEY(host_id) REFERENCES vm_hosts(host_id)
      );
      CREATE INDEX IF NOT EXISTS vm_allocations_host_state
        ON vm_allocations(host_id, state, slot);
      CREATE UNIQUE INDEX IF NOT EXISTS vm_allocations_live_slot
        ON vm_allocations(host_id, slot)
        WHERE state IN ('provisioning', 'ready', 'releasing');
      CREATE TABLE IF NOT EXISTS vm_allocation_release_intents (
        owner_id TEXT NOT NULL,
        organization_id TEXT NOT NULL,
        team_id TEXT NOT NULL,
        authorization_epoch INTEGER NOT NULL,
        agent_id TEXT NOT NULL,
        mount_id TEXT NOT NULL,
        pool_locator TEXT NOT NULL,
        factory_name TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(owner_id, agent_id, mount_id)
      );
    `);
    const hostColumns = new Set(this.ctx.storage.sql.exec<{ name: string }>(
      "PRAGMA table_info(vm_hosts)",
    ).toArray().map((column) => column.name));
    if (!hostColumns.has("public_origin")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE vm_hosts ADD COLUMN public_origin TEXT NOT NULL DEFAULT ''",
      );
    }
    const allocationColumns = new Set(this.ctx.storage.sql.exec<{ name: string }>(
      "PRAGMA table_info(vm_allocations)",
    ).toArray().map((column) => column.name));
    if (!allocationColumns.has("public_origin")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE vm_allocations ADD COLUMN public_origin TEXT NOT NULL DEFAULT ''",
      );
    }
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/host") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const claim = hostClaim(request.headers);
      if (!claim || !this.#claimPool(claim)) return notFound();
      const pair = new WebSocketPair();
      const [client, server] = Object.values(pair);
      server.serializeAttachment({
        kind: "vm-host",
        donorId: claim.donorId,
        publicOrigin: claim.publicOrigin,
      } satisfies HostAttachment);
      this.ctx.acceptWebSocket(server, ["vm-host"]);
      return new Response(null, { status: 101, webSocket: client });
    }
    if (request.method === "POST" && url.pathname === "/acquire") {
      const body = await readObject(request);
      const acquire = body && acquireRequest(body);
      if (!acquire) return notFound();
      const claim = this.#poolClaim();
      if (!claim) {
        return Response.json({ error: "factory_not_found" }, { status: 404 });
      }
      if (!this.#allows(acquire)) return notFound();
      return this.#acquire(acquire);
    }
    if (request.method === "POST" && url.pathname === "/release") {
      const body = await readObject(request);
      const identity = body && allocationIdentity(body);
      if (!identity || !this.#allows(identity)) return notFound();
      return this.#release(identity);
    }
    if (request.method === "POST" && url.pathname === "/release-intent") {
      const body = await readObject(request);
      const intent = body && acquireRequest(body);
      if (!intent || !this.#allows(intent)) return notFound();
      return this.#releaseIntent(intent);
    }
    if (request.method === "POST" && url.pathname === "/validate-attachment") {
      const body = await readObject(request);
      if (!body || !hasExactKeys(body, ["allocation_id", "bearer"])
        || !uuidV4(body.allocation_id) || !opaque(body.bearer)) return notFound();
      return this.#validateAttachment(body.allocation_id, body.bearer);
    }
    if (request.method === "POST" && url.pathname === "/ready") {
      const body = await readObject(request);
      const identity = body && allocationIdentity(body);
      if (!identity || !this.#allows(identity)) return notFound();
      return this.#ready(identity);
    }
    return notFound();
  }

  async alarm(): Promise<void> {
    const now = Date.now();
    const expired = this.ctx.storage.sql.exec<{
      host_id: string;
      lease_id: string;
      epoch: number;
    }>(
      `SELECT host_id, lease_id, epoch FROM vm_hosts
       WHERE lease_id IS NOT NULL AND lease_expires_at > 0 AND lease_expires_at <= ?`,
      now,
    ).toArray();
    for (const { host_id: hostId, lease_id: leaseId, epoch } of expired) {
      this.ctx.storage.sql.exec(
        `UPDATE vm_hosts SET lease_expires_at = 0
         WHERE host_id = ? AND lease_id = ? AND epoch = ? AND lease_expires_at <= ?`,
        hostId, leaseId, epoch, now,
      );
      const connected = this.#socketForHostLease(hostId, leaseId, epoch);
      if (connected) closeSocket(connected.socket, 1008, "VM host lease expired");
      await this.#revokeHostAttachments(
        hostId, leaseId, epoch, "VM host control lease expired",
      );
    }
    await this.#scheduleLeaseAlarm();
  }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const attachment = socket.deserializeAttachment() as HostAttachment | null;
    if (attachment?.kind !== "vm-host") {
      closeSocket(socket, 1008, "invalid VM host socket");
      return;
    }
    let command: VmHostCommand;
    try {
      command = parseVmHostCommand(typeof message === "string"
        ? message
        : new TextDecoder().decode(message));
    } catch (error) {
      const protocol = error instanceof VmHostProtocolError
        ? error
        : new VmHostProtocolError("invalid_message", errorMessage(error));
      this.#send(socket, { type: "error", code: protocol.code, message: protocol.message });
      return;
    }
    try {
      if (command.type === "attach") {
      await this.#attachHost(socket, attachment, command);
        return;
      }
      const host = this.#requireLease(socket, attachment, command.lease_id, command.epoch);
      if (command.type === "ping") {
        this.#renewLease(socket, host, command);
      } else if (command.type === "reconcile") {
        this.#reconcile(socket, host, command);
      } else if (command.type === "provisioned") {
        this.#provisioned(socket, host, command);
      } else {
        this.#released(socket, host, command);
      }
    } catch (error) {
      const protocol = error instanceof VmHostProtocolError
        ? error
        : new VmHostProtocolError("vm_host_failed", errorMessage(error));
      if (protocol.code !== "stale_lease") {
        this.#send(socket, { type: "error", code: protocol.code, message: protocol.message });
      }
    }
  }

  webSocketClose(socket: WebSocket): void { this.#retireSocket(socket); }

  webSocketError(socket: WebSocket): void { this.#retireSocket(socket); }

  #claimPool(candidate: HostClaim): boolean {
    return this.ctx.storage.transactionSync(() => {
      const current = this.#poolClaim();
      if (current) {
        return current.scope === candidate.scope
          && current.owner_id === candidate.owner_id
          && current.agent_id === candidate.agent_id
          && current.pool_locator === candidate.pool_locator;
      }
      this.ctx.storage.sql.exec(
        `INSERT INTO vm_pool_claim
          (singleton, scope, owner_id, agent_id, pool_locator)
         VALUES (1, ?, ?, ?, ?)`,
        candidate.scope,
        candidate.owner_id,
        candidate.agent_id,
        candidate.pool_locator,
      );
      return true;
    });
  }

  #allows(candidate: Pick<AcquireRequest, "owner_id" | "agent_id" | "pool_locator">): boolean {
    const claim = this.#poolClaim();
    if (!claim || claim.pool_locator !== candidate.pool_locator) return false;
    if (claim.scope === "system") return true;
    if (claim.owner_id !== candidate.owner_id) return false;
    return claim.scope === "account" || claim.agent_id === candidate.agent_id;
  }

  async #acquire(input: AcquireRequest): Promise<Response> {
    const allocationId = crypto.randomUUID();
    const bearer = randomOpaque();
    const now = Date.now();
    let created = false;
    let factoryExists = false;
    let cancelled = false;
    const allocation = this.ctx.storage.transactionSync(() => {
      const releaseIntent = this.#releaseIntentRow(input.owner_id, input.agent_id, input.mount_id);
      if (releaseIntent !== undefined) {
        if (!sameAcquire(releaseIntent, input)) return undefined;
        cancelled = true;
        return null;
      }
      const existing = this.ctx.storage.sql.exec<AllocationRow>(
        `SELECT * FROM vm_allocations
         WHERE owner_id = ? AND agent_id = ? AND mount_id = ?`,
        input.owner_id,
        input.agent_id,
        input.mount_id,
      ).toArray()[0];
      if (existing) {
        if (!sameAcquire(existing, input)) return undefined;
        return existing;
      }
      const host = this.ctx.storage.sql.exec<HostRow>(
        "SELECT * FROM vm_hosts WHERE factory_name = ?",
        input.factory_name,
      ).toArray()[0];
      if (!host) return null;
      factoryExists = true;
      if (this.#socketFor(host) === undefined) {
        return null;
      }
      const occupied = new Set(this.ctx.storage.sql.exec<{ slot: number }>(
        `SELECT slot FROM vm_allocations
         WHERE host_id = ? AND state IN ('provisioning', 'ready', 'releasing')`,
        host.host_id,
      ).toArray().map(({ slot }) => slot));
      let slot = 0;
      while (slot < host.max_vms && occupied.has(slot)) slot += 1;
      if (slot >= host.max_vms) return null;
      const row: AllocationRow = {
        allocation_id: allocationId,
        generation: 1,
        factory_name: input.factory_name,
        owner_id: input.owner_id,
        organization_id: input.organization_id,
        team_id: input.team_id,
        authorization_epoch: input.authorization_epoch,
        agent_id: input.agent_id,
        mount_id: input.mount_id,
        public_origin: host.public_origin,
        pool_locator: input.pool_locator,
        machine_id: `vm:${input.mount_id}`,
        host_id: host.host_id,
        lease_id: host.lease_id,
        host_epoch: host.epoch,
        slot,
        bearer,
        state: "provisioning",
        created_at: now,
        updated_at: now,
      };
      this.ctx.storage.sql.exec(
        `INSERT INTO vm_allocations (
           allocation_id, generation, factory_name, owner_id, organization_id, team_id,
           authorization_epoch, agent_id, mount_id, public_origin, pool_locator,
           machine_id, host_id, lease_id, host_epoch, slot, bearer,
           state, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
        row.allocation_id, row.generation, row.factory_name, row.owner_id,
        row.organization_id, row.team_id, row.authorization_epoch, row.agent_id,
        row.mount_id, row.public_origin, row.pool_locator, row.machine_id, row.host_id,
        row.lease_id, row.host_epoch, row.slot, row.bearer, row.state,
        row.created_at, row.updated_at,
      );
      created = true;
      return row;
    });
    if (allocation === undefined) {
      return Response.json({ error: "allocation_conflict" }, { status: 409 });
    }
    if (allocation === null) {
      if (cancelled) {
        return Response.json({ error: "allocation_released" }, { status: 409 });
      }
      return Response.json({
        error: factoryExists ? "factory_unavailable" : "factory_not_found",
      }, { status: factoryExists ? 409 : 404 });
    }
    if (allocation.state === "released") {
      return Response.json({ error: "allocation_released" }, { status: 409 });
    }
    if (created) this.#dispatchProvision(allocation);
    else this.#redrive(allocation);
    return Response.json(publicAllocation(allocation, this.#currentRouteId(allocation)), {
      status: created ? 201 : 200,
      headers: { "cache-control": "no-store" },
    });
  }

  #release(identity: AllocationIdentity): Response {
    let changed = false;
    const allocation = this.ctx.storage.transactionSync(() => {
      const row = this.#allocation(identity.allocation_id);
      if (!row || !sameIdentity(row, identity)) return undefined;
      if (row.state === "released" || row.state === "releasing") return row;
      this.ctx.storage.sql.exec(
        `UPDATE vm_allocations SET state = 'releasing', updated_at = ?
         WHERE allocation_id = ? AND generation = ?`,
        Date.now(), row.allocation_id, row.generation,
      );
      changed = true;
      return { ...row, state: "releasing" as const };
    });
    if (!allocation) return notFound();
    if (allocation.state === "releasing") this.#dispatchRelease(allocation);
    return Response.json({
      ...publicAllocation(allocation, this.#currentRouteId(allocation)),
      state: allocation.state,
    }, {
      status: changed ? 202 : 200,
      headers: { "cache-control": "no-store" },
    });
  }

  #releaseIntent(intent: AllocationReleaseIntent): Response {
    const allocation = this.ctx.storage.transactionSync(() => {
      const retained = this.#releaseIntentRow(intent.owner_id, intent.agent_id, intent.mount_id);
      if (retained !== undefined && !sameAcquire(retained, intent)) return undefined;
      if (retained === undefined) {
        this.ctx.storage.sql.exec(
          `INSERT INTO vm_allocation_release_intents (
             owner_id, organization_id, team_id, authorization_epoch, agent_id,
             mount_id, pool_locator, factory_name, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
          intent.owner_id,
          intent.organization_id,
          intent.team_id,
          intent.authorization_epoch,
          intent.agent_id,
          intent.mount_id,
          intent.pool_locator,
          intent.factory_name,
          Date.now(),
        );
      }
      const row = this.ctx.storage.sql.exec<AllocationRow>(
        `SELECT * FROM vm_allocations
         WHERE owner_id = ? AND agent_id = ? AND mount_id = ?`,
        intent.owner_id, intent.agent_id, intent.mount_id,
      ).toArray()[0];
      return row === undefined || sameAcquire(row, intent) ? row ?? null : undefined;
    });
    if (allocation === undefined) return notFound();
    if (allocation === null) {
      return Response.json({ state: "released" }, {
        headers: { "cache-control": "no-store" },
      });
    }
    return this.#release({
      owner_id: allocation.owner_id,
      agent_id: allocation.agent_id,
      mount_id: allocation.mount_id,
      allocation_id: allocation.allocation_id,
      generation: allocation.generation,
      pool_locator: allocation.pool_locator,
    });
  }

  async #validateAttachment(allocationId: string, bearer: string): Promise<Response> {
    const allocation = this.#allocation(allocationId);
    const host = allocation === undefined ? undefined : this.#host(allocation.host_id);
    const connected = host === undefined ? undefined : this.#socketFor(host);
    const valid = allocation !== undefined
      && host !== undefined
      && connected !== undefined
      && liveLease(connected.attachment, host, Date.now())
      && allocation.lease_id === host.lease_id
      && allocation.host_epoch === host.epoch
      && (allocation.state === "provisioning" || allocation.state === "ready")
      && constantTimeEqual(allocation.bearer, bearer);
    if (!valid || !allocation || !host) return notFound();
    return Response.json({
      valid: true,
      allocation_id: allocation.allocation_id,
      generation: allocation.generation,
      owner_id: allocation.owner_id,
      organization_id: allocation.organization_id,
      team_id: allocation.team_id,
      authorization_epoch: allocation.authorization_epoch,
      agent_id: allocation.agent_id,
      mount_id: allocation.mount_id,
      public_origin: allocation.public_origin,
      machine_id: allocation.machine_id,
      lease_expires_at: host.lease_expires_at,
      route_id: vmHostAttachmentRouteId(allocation.allocation_id, host.epoch),
    }, { headers: { "cache-control": "no-store" } });
  }

  #ready(identity: AllocationIdentity): Response {
    const allocation = this.#allocation(identity.allocation_id);
    if (!allocation || !sameIdentity(allocation, identity)) return notFound();
    const routeId = this.#currentRouteId(allocation);
    return Response.json({
      ...publicAllocation(allocation, routeId),
      ready: allocation.state === "ready" && routeId !== undefined,
      state: allocation.state,
    }, { headers: { "cache-control": "no-store" } });
  }

  async #attachHost(
    socket: WebSocket,
    attachment: HostAttachment,
    command: Extract<VmHostCommand, { type: "attach" }>,
  ): Promise<void> {
    if (attachment.hostId || attachment.leaseId || attachment.epoch) {
      throw new VmHostProtocolError("already_attached", "this socket already holds a VM host lease");
    }
    const existing = this.#host(command.host_id);
    const named = this.#factory(command.factory_name);
    if (existing && existing.donor_id !== attachment.donorId) {
      throw new VmHostProtocolError("host_conflict", "host_id is owned by a different donor");
    }
    if (existing && existing.factory_name !== command.factory_name) {
      throw new VmHostProtocolError(
        "host_name_conflict",
        "factory_name is immutable for an existing host_id",
      );
    }
    if (named && named.host_id !== command.host_id) {
      throw new VmHostProtocolError(
        "factory_name_conflict",
        "factory_name is already owned by a different host",
      );
    }
    if (existing && (existing.max_vms !== command.max_vms
      || existing.vm_cpus !== command.vm.cpus
      || existing.vm_memory_mib !== command.vm.memory_mib)) {
      throw new VmHostProtocolError("host_shape_conflict", "host capacity and VM shape are immutable");
    }
    if ((existing?.epoch ?? 0) >= Number.MAX_SAFE_INTEGER) {
      throw new VmHostProtocolError("lease_exhausted", "the VM host lease epoch is exhausted");
    }
    if (existing?.lease_id) {
      await this.#revokeHostAttachments(
        command.host_id,
        existing.lease_id,
        existing.epoch,
        "VM host control lease was replaced",
      );
      const current = this.#host(command.host_id);
      if (current?.lease_id !== existing.lease_id || current?.epoch !== existing.epoch) {
        throw new VmHostProtocolError(
          "host_replacement_raced",
          "another connection replaced this VM host while its old routes were fenced",
        );
      }
    }
    const leaseId = crypto.randomUUID();
    const epoch = (existing?.epoch ?? 0) + 1;
    const expiresAt = Date.now() + VM_HOST_LEASE_MS;
    this.ctx.storage.sql.exec(
      `INSERT INTO vm_hosts
         (host_id, factory_name, donor_id, max_vms, vm_cpus, vm_memory_mib,
          public_origin, epoch, lease_id, lease_expires_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(host_id) DO UPDATE SET
         public_origin = excluded.public_origin,
         epoch = excluded.epoch,
         lease_id = excluded.lease_id,
         lease_expires_at = excluded.lease_expires_at`,
      command.host_id, command.factory_name, attachment.donorId, command.max_vms,
      command.vm.cpus, command.vm.memory_mib, attachment.publicOrigin,
      epoch, leaseId, expiresAt,
    );
    this.ctx.storage.sql.exec(
      `UPDATE vm_allocations SET public_origin = ?, updated_at = ?
       WHERE host_id = ? AND state IN ('provisioning', 'ready', 'releasing')`,
      attachment.publicOrigin, Date.now(), command.host_id,
    );
    for (const candidate of this.ctx.getWebSockets("vm-host")) {
      if (candidate === socket) continue;
      const candidateAttachment = candidate.deserializeAttachment() as HostAttachment | null;
      if (candidateAttachment?.kind !== "vm-host" || candidateAttachment.hostId !== command.host_id) continue;
      try {
        this.#send(candidate, {
          type: "fenced",
          epoch,
          reason: "a newer connection acquired this VM host identity",
        });
      } catch { /* Closing is the authoritative fence. */ }
      closeSocket(candidate, 1008, "VM host lease replaced");
    }
    socket.serializeAttachment({
      ...attachment,
      hostId: command.host_id,
      leaseId,
      epoch,
    } satisfies HostAttachment);
    this.#send(socket, {
      type: "lease",
      protocol_version: VM_HOST_PROTOCOL_VERSION,
      lease_id: leaseId,
      epoch,
      expires_at: expiresAt,
      max_vms: command.max_vms,
      vm: command.vm,
    });
    this.ctx.waitUntil(this.#scheduleLeaseAlarm());
  }

  #requireLease(
    socket: WebSocket,
    attachment: HostAttachment,
    leaseId: string,
    epoch: number,
  ): HostRow {
    const host = attachment.hostId ? this.#host(attachment.hostId) : undefined;
    if (!host || attachment.leaseId !== leaseId || attachment.epoch !== epoch
      || !liveLease(attachment, host, Date.now())) {
      try {
        this.#send(socket, {
          type: "fenced",
          epoch: host?.epoch ?? 0,
          reason: "the VM host lease is stale or expired",
        });
      } catch { /* Closing is the authoritative fence. */ }
      closeSocket(socket, 1008, "stale VM host lease");
      throw new VmHostProtocolError("stale_lease", "the VM host lease is stale or expired");
    }
    const leaseExpiresAt = Date.now() + VM_HOST_LEASE_MS;
    this.ctx.storage.sql.exec(
      `UPDATE vm_hosts SET lease_expires_at = ?
       WHERE host_id = ? AND lease_id = ? AND epoch = ?`,
      leaseExpiresAt, host.host_id, leaseId, epoch,
    );
    this.ctx.waitUntil(this.#scheduleLeaseAlarm());
    return { ...host, lease_expires_at: leaseExpiresAt };
  }

  #renewLease(
    socket: WebSocket,
    host: HostRow,
    command: Extract<VmHostCommand, { type: "ping" }>,
  ): void {
    this.#send(socket, {
      type: "pong",
      lease_id: command.lease_id,
      epoch: command.epoch,
      expires_at: host.lease_expires_at,
      ...(command.nonce === undefined ? {} : { nonce: command.nonce }),
    });
  }

  #reconcile(
    socket: WebSocket,
    host: HostRow,
    command: Extract<VmHostCommand, { type: "reconcile" }>,
  ): void {
    const reported = new Map(command.allocations.map((entry) => [entry.allocation_id, entry]));
    const desired = this.ctx.storage.sql.exec<AllocationRow>(
      `SELECT * FROM vm_allocations
       WHERE host_id = ? AND state IN ('provisioning', 'ready', 'releasing')
       ORDER BY slot ASC`,
      host.host_id,
    ).toArray();
    const desiredIds = new Set(desired.map(({ allocation_id }) => allocation_id));
    for (const allocation of desired) {
      const rebound = allocation.lease_id !== host.lease_id
        || allocation.host_epoch !== host.epoch;
      const bound = this.#bindAllocation(allocation, host);
      const actual = reported.get(allocation.allocation_id);
      if (allocation.state === "releasing") {
        this.#sendRelease(socket, host, bound);
      } else if (rebound) {
        // A successor may already have the VM on disk, but it cannot keep using
        // the predecessor's attachment capability. Deliver the newly
        // epoch-bound bearer through the idempotent provision command.
        this.#sendProvision(socket, host, bound);
      } else if (actual && actual.generation === allocation.generation
        && actual.machine_id === allocation.machine_id) {
        if (allocation.state === "provisioning") this.#markReady(bound);
      } else {
        this.#sendProvision(socket, host, bound);
      }
    }
    for (const actual of command.allocations) {
      if (desiredIds.has(actual.allocation_id)) continue;
      this.#send(socket, {
        type: "release",
        lease_id: host.lease_id!,
        epoch: host.epoch,
        allocation_id: actual.allocation_id,
        generation: actual.generation,
        machine_id: actual.machine_id,
      });
    }
  }

  #provisioned(
    _socket: WebSocket,
    host: HostRow,
    command: Extract<VmHostCommand, { type: "provisioned" }>,
  ): void {
    const allocation = this.#allocation(command.allocation_id);
    if (!allocation || allocation.host_id !== host.host_id
      || allocation.generation !== command.generation
      || allocation.machine_id !== command.machine_id
      || allocation.state === "released") {
      throw new VmHostProtocolError("unknown_allocation", "allocation is not provisionable by this host");
    }
    if (allocation.state === "releasing") return;
    this.#markReady(allocation);
  }

  #released(
    _socket: WebSocket,
    host: HostRow,
    command: Extract<VmHostCommand, { type: "released" }>,
  ): void {
    const allocation = this.#allocation(command.allocation_id);
    if (!allocation || allocation.host_id !== host.host_id
      || allocation.generation !== command.generation
      || allocation.machine_id !== command.machine_id) {
      // Releasing a host-only allocation during reconcile is deliberately
      // idempotent and has no durable row to update.
      return;
    }
    if (allocation.state !== "releasing" && allocation.state !== "released") {
      throw new VmHostProtocolError("unexpected_release", "allocation was not pending release");
    }
    this.ctx.storage.sql.exec(
      `UPDATE vm_allocations SET state = 'released', lease_id = NULL,
         host_epoch = NULL, updated_at = ?
       WHERE allocation_id = ? AND generation = ?`,
      Date.now(), allocation.allocation_id, allocation.generation,
    );
  }

  #redrive(allocation: AllocationRow): void {
    if (allocation.state === "provisioning") this.#dispatchProvision(allocation);
    if (allocation.state === "releasing") this.#dispatchRelease(allocation);
  }

  #dispatchProvision(allocation: AllocationRow): void {
    const host = this.#host(allocation.host_id);
    if (!host) return;
    const connected = this.#socketFor(host);
    if (!connected) return;
    const bound = this.#bindAllocation(allocation, host);
    this.#sendProvision(connected.socket, host, bound);
  }

  #dispatchRelease(allocation: AllocationRow): void {
    const host = this.#host(allocation.host_id);
    if (!host) return;
    const connected = this.#socketFor(host);
    if (!connected) return;
    const bound = this.#bindAllocation(allocation, host);
    this.#sendRelease(connected.socket, host, bound);
  }

  #sendProvision(socket: WebSocket, host: HostRow, allocation: AllocationRow): void {
    this.#send(socket, {
      type: "provision",
      lease_id: host.lease_id!,
      epoch: host.epoch,
      allocation_id: allocation.allocation_id,
      generation: allocation.generation,
      slot: allocation.slot,
      machine_id: allocation.machine_id,
      tool_attachment: {
        url: attachmentUrl(allocation),
        bearer: allocation.bearer,
      },
    });
  }

  #sendRelease(socket: WebSocket, host: HostRow, allocation: AllocationRow): void {
    this.#send(socket, {
      type: "release",
      lease_id: host.lease_id!,
      epoch: host.epoch,
      allocation_id: allocation.allocation_id,
      generation: allocation.generation,
      machine_id: allocation.machine_id,
    });
  }

  #bindAllocation(allocation: AllocationRow, host: HostRow): AllocationRow {
    if (allocation.lease_id === host.lease_id && allocation.host_epoch === host.epoch) {
      return allocation;
    }
    const bearer = randomOpaque();
    const updatedAt = Date.now();
    this.ctx.storage.sql.exec(
      `UPDATE vm_allocations SET lease_id = ?, host_epoch = ?, bearer = ?, updated_at = ?
       WHERE allocation_id = ? AND generation = ? AND state != 'released'`,
      host.lease_id, host.epoch, bearer, updatedAt,
      allocation.allocation_id, allocation.generation,
    );
    return {
      ...allocation,
      lease_id: host.lease_id,
      host_epoch: host.epoch,
      bearer,
      updated_at: updatedAt,
    };
  }

  #markReady(allocation: AllocationRow): void {
    this.ctx.storage.sql.exec(
      `UPDATE vm_allocations SET state = 'ready', updated_at = ?
       WHERE allocation_id = ? AND generation = ? AND state = 'provisioning'`,
      Date.now(), allocation.allocation_id, allocation.generation,
    );
  }

  #retireSocket(socket: WebSocket): void {
    const attachment = socket.deserializeAttachment() as HostAttachment | null;
    if (attachment?.kind !== "vm-host" || !attachment.hostId
      || !attachment.leaseId || attachment.epoch === undefined) return;
    this.ctx.storage.sql.exec(
      `UPDATE vm_hosts SET lease_expires_at = 0
       WHERE host_id = ? AND lease_id = ? AND epoch = ?`,
      attachment.hostId, attachment.leaseId, attachment.epoch,
    );
    this.ctx.waitUntil(this.#revokeHostAttachments(
      attachment.hostId, attachment.leaseId, attachment.epoch,
      "VM host control socket closed",
    ).catch((error) => {
      console.warn({ type: "vm_host_attachment_revoke_failed", error: errorMessage(error) });
    }));
    this.ctx.waitUntil(this.#scheduleLeaseAlarm());
  }

  async #revokeHostAttachments(
    hostId: string,
    leaseId: string,
    hostEpoch: number,
    reason: string,
  ): Promise<void> {
    const allocations = this.ctx.storage.sql.exec<Pick<
      AllocationRow,
      "agent_id" | "allocation_id"
    >>(
      `SELECT agent_id, allocation_id FROM vm_allocations
       WHERE host_id = ? AND lease_id = ? AND host_epoch = ?
         AND state IN ('provisioning', 'ready', 'releasing')`,
      hostId, leaseId, hostEpoch,
    ).toArray();
    const responses = await Promise.all(allocations.map(({ agent_id: agentId, allocation_id: allocationId }) => (
      this.#env.NANOCODEX_SESSIONS.getByName(agentId).fetch(
        "https://session.internal/vm-host-revoke",
        {
          method: "POST",
          headers: {
            "x-nanocodex-vm-route-id": vmHostAttachmentRouteId(allocationId, hostEpoch),
            "x-nanocodex-vm-revoke-reason": reason,
          },
        },
      )
    )));
    await Promise.all(responses.map(async (response) => {
      if (!response.ok) {
        await response.body?.cancel();
        throw new Error(`VM host attachment revocation failed with HTTP ${response.status}`);
      }
      await response.body?.cancel();
    }));
  }

  async #scheduleLeaseAlarm(): Promise<void> {
    const next = this.ctx.storage.sql.exec<{ expires_at: number | null }>(
      `SELECT MIN(lease_expires_at) AS expires_at FROM vm_hosts
       WHERE lease_id IS NOT NULL AND lease_expires_at > ?`,
      Date.now(),
    ).toArray()[0]?.expires_at;
    if (next === null || next === undefined) await this.ctx.storage.deleteAlarm();
    else await this.ctx.storage.setAlarm(next);
  }

  #socketFor(host: HostRow): { socket: WebSocket; attachment: HostAttachment } | undefined {
    if (!host.lease_id || host.lease_expires_at < Date.now()) return undefined;
    const connected = this.#socketForHostLease(host.host_id, host.lease_id, host.epoch);
    return connected && connected.socket.readyState === WebSocket.OPEN ? connected : undefined;
  }

  #currentRouteId(allocation: AllocationRow): string | undefined {
    if (!allocation.lease_id || allocation.host_epoch === null) return undefined;
    const host = this.#host(allocation.host_id);
    if (!host || host.lease_id !== allocation.lease_id || host.epoch !== allocation.host_epoch
      || this.#socketFor(host) === undefined) return undefined;
    return vmHostAttachmentRouteId(allocation.allocation_id, allocation.host_epoch);
  }

  #socketForHostLease(
    hostId: string,
    leaseId: string,
    epoch: number,
  ): { socket: WebSocket; attachment: HostAttachment } | undefined {
    for (const socket of this.ctx.getWebSockets("vm-host")) {
      const attachment = socket.deserializeAttachment() as HostAttachment | null;
      if (attachment?.kind === "vm-host"
        && attachment.hostId === hostId
        && attachment.leaseId === leaseId
        && attachment.epoch === epoch) {
        return { socket, attachment };
      }
    }
    return undefined;
  }

  #poolClaim(): PoolClaim | undefined {
    return this.ctx.storage.sql.exec<PoolClaim>(
      "SELECT * FROM vm_pool_claim WHERE singleton = 1",
    ).toArray()[0];
  }

  #host(hostId: string): HostRow | undefined {
    return this.ctx.storage.sql.exec<HostRow>(
      "SELECT * FROM vm_hosts WHERE host_id = ?",
      hostId,
    ).toArray()[0];
  }

  #factory(factoryName: string): HostRow | undefined {
    return this.ctx.storage.sql.exec<HostRow>(
      "SELECT * FROM vm_hosts WHERE factory_name = ?",
      factoryName,
    ).toArray()[0];
  }

  #allocation(allocationId: string): AllocationRow | undefined {
    return this.ctx.storage.sql.exec<AllocationRow>(
      "SELECT * FROM vm_allocations WHERE allocation_id = ?",
      allocationId,
    ).toArray()[0];
  }

  #releaseIntentRow(
    ownerId: string,
    agentId: string,
    mountId: string,
  ): AllocationReleaseIntent | undefined {
    return this.ctx.storage.sql.exec<AllocationReleaseIntent>(
      `SELECT factory_name, owner_id, organization_id, team_id, authorization_epoch,
              agent_id, mount_id, pool_locator
       FROM vm_allocation_release_intents
       WHERE owner_id = ? AND agent_id = ? AND mount_id = ?`,
      ownerId, agentId, mountId,
    ).toArray()[0];
  }

  #send(socket: WebSocket, message: VmHostServerMessage): void {
    socket.send(JSON.stringify(message));
  }
}

function hostClaim(headers: Headers): HostClaim | undefined {
  const scope = headers.get(POOL_SCOPE);
  if (scope !== "agent" && scope !== "account" && scope !== "system") return undefined;
  const ownerId = headers.get(POOL_OWNER);
  const agentId = headers.get(POOL_AGENT);
  const donorId = headers.get(DONOR_ID);
  const publicOrigin = origin(headers.get(PUBLIC_ORIGIN));
  const locator = headers.get(POOL_LOCATOR);
  if (!donorId || !SAFE_ID.test(donorId) || !publicOrigin || !locator || !OPAQUE.test(locator)) {
    return undefined;
  }
  if (scope === "agent" && (!isUserId(ownerId) || !identifier(agentId))) return undefined;
  if (scope === "account" && (!isUserId(ownerId) || agentId !== null)) return undefined;
  if (scope === "system" && (ownerId !== null || agentId !== null || donorId !== "system")) return undefined;
  return {
    scope,
    owner_id: ownerId,
    agent_id: agentId,
    pool_locator: locator,
    donorId,
    publicOrigin,
  };
}

function acquireRequest(body: Record<string, unknown>): AcquireRequest | undefined {
  if (!hasExactKeys(body, [
    "factory_name", "owner_id", "organization_id", "team_id", "authorization_epoch", "agent_id",
    "mount_id", "pool_locator",
  ]) || !isVmFactoryName(body.factory_name) || !isUserId(body.owner_id) || !identifier(body.organization_id)
    || !identifier(body.team_id) || !nonNegativeInteger(body.authorization_epoch)
    || !identifier(body.agent_id) || !uuid(body.mount_id)
    || !opaque(body.pool_locator)) return undefined;
  return {
    factory_name: body.factory_name,
    owner_id: body.owner_id,
    organization_id: body.organization_id,
    team_id: body.team_id,
    authorization_epoch: body.authorization_epoch,
    agent_id: body.agent_id,
    mount_id: body.mount_id,
    pool_locator: body.pool_locator,
  };
}

function allocationIdentity(body: Record<string, unknown>): AllocationIdentity | undefined {
  if (!hasExactKeys(body, [
    "owner_id", "agent_id", "mount_id", "allocation_id", "generation", "pool_locator",
  ]) || !isUserId(body.owner_id) || !identifier(body.agent_id) || !uuid(body.mount_id)
    || !uuidV4(body.allocation_id) || !positiveInteger(body.generation)
    || !opaque(body.pool_locator)) return undefined;
  return body as AllocationIdentity;
}

function sameAcquire(
  row: Pick<AllocationRow, keyof AcquireRequest>,
  input: AcquireRequest,
): boolean {
  return row.factory_name === input.factory_name
    && row.owner_id === input.owner_id
    && row.organization_id === input.organization_id
    && row.team_id === input.team_id
    && row.authorization_epoch === input.authorization_epoch
    && row.agent_id === input.agent_id
    && row.mount_id === input.mount_id
    && row.pool_locator === input.pool_locator;
}

function sameIdentity(row: AllocationRow, identity: AllocationIdentity): boolean {
  return row.owner_id === identity.owner_id
    && row.agent_id === identity.agent_id
    && row.mount_id === identity.mount_id
    && row.allocation_id === identity.allocation_id
    && row.generation === identity.generation
    && row.pool_locator === identity.pool_locator;
}

function publicAllocation(row: AllocationRow, routeId?: string): Record<string, unknown> {
  return {
    allocation_id: row.allocation_id,
    generation: row.generation,
    factory_name: row.factory_name,
    machine_id: row.machine_id,
    host_id: row.host_id,
    slot: row.slot,
    ...(routeId === undefined ? {} : { route_id: routeId }),
  };
}

function attachmentUrl(row: AllocationRow): string {
  const url = new URL(
    `/v1/vm-host-attachments/${encodeURIComponent(row.pool_locator)}/${encodeURIComponent(row.allocation_id)}/tool-host`,
    row.public_origin,
  );
  if (url.protocol === "https:") url.protocol = "wss:";
  return url.toString();
}

async function readObject(request: Request): Promise<Record<string, unknown> | undefined> {
  try {
    const value = await request.json<unknown>();
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : undefined;
  } catch {
    return undefined;
  }
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const expected = new Set(keys);
  return Object.keys(value).length === keys.length
    && Object.keys(value).every((key) => expected.has(key));
}

function identifier(value: unknown): value is string {
  return typeof value === "string" && SAFE_ID.test(value);
}

function uuid(value: unknown): value is string {
  return typeof value === "string" && UUID.test(value);
}

function uuidV4(value: unknown): value is string {
  return typeof value === "string" && UUID_V4.test(value);
}

function opaque(value: unknown): value is string {
  return typeof value === "string" && OPAQUE.test(value);
}

function positiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 1;
}

function nonNegativeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function origin(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const parsed = new URL(value);
    if (parsed.protocol !== "https:" || parsed.origin !== value || parsed.username || parsed.password) {
      return undefined;
    }
    return parsed.origin;
  } catch {
    return undefined;
  }
}

function randomOpaque(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function constantTimeEqual(left: string, right: string): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left.charCodeAt(index) ^ right.charCodeAt(index);
  }
  return difference === 0;
}

function liveLease(attachment: HostAttachment, host: HostRow, now: number): boolean {
  return host.lease_id !== null
    && matchesVmHostLease(attachment, {
      host_id: host.host_id,
      lease_id: host.lease_id,
      epoch: host.epoch,
      lease_expires_at: host.lease_expires_at,
    }, now);
}

function notFound(): Response {
  return Response.json({ error: "not_found" }, { status: 404 });
}

function closeSocket(socket: WebSocket, code: number, reason: string): void {
  try { socket.close(code, reason); } catch { /* Already closed. */ }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
