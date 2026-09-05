import { env, runInDurableObject } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

import { VmHostPool, type VmHostPoolEnv } from "../src/vm-host-pool";

const OWNER_A = "11111111-1111-4111-8111-111111111111";
const OWNER_B = "22222222-2222-4222-8222-222222222222";
const HOST_A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const HOST_B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const HOST_C = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const FACTORY_A = "garage-mac";
const FACTORY_B = "linux-builder";
const MOUNT_A = "10000000-0000-4000-8000-000000000001";
const MOUNT_B = "10000000-0000-4000-8000-000000000002";
const MOUNT_C = "10000000-0000-4000-8000-000000000003";
const LOCATOR = "p".repeat(43);
const ORIGIN = "https://managed.example";

afterEach(() => vi.useRealTimers());

describe("VM host pool", () => {
  it("upgrades retained host and allocation tables with public origins", async () => {
    const stub = pool(crypto.randomUUID());
    await runInDurableObject(stub, async (_pool, state) => {
      state.storage.sql.exec(`
        DROP TABLE vm_allocations;
        DROP TABLE vm_hosts;
        CREATE TABLE vm_hosts (
          host_id TEXT PRIMARY KEY,
          factory_name TEXT NOT NULL UNIQUE,
          donor_id TEXT NOT NULL,
          max_vms INTEGER NOT NULL,
          vm_cpus INTEGER NOT NULL,
          vm_memory_mib INTEGER NOT NULL,
          epoch INTEGER NOT NULL DEFAULT 0,
          lease_id TEXT,
          lease_expires_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE vm_allocations (
          allocation_id TEXT PRIMARY KEY,
          generation INTEGER NOT NULL,
          factory_name TEXT NOT NULL,
          owner_id TEXT NOT NULL,
          organization_id TEXT NOT NULL,
          team_id TEXT NOT NULL,
          authorization_epoch INTEGER NOT NULL,
          agent_id TEXT NOT NULL,
          mount_id TEXT NOT NULL,
          pool_locator TEXT NOT NULL,
          machine_id TEXT NOT NULL,
          host_id TEXT NOT NULL,
          lease_id TEXT,
          host_epoch INTEGER,
          slot INTEGER NOT NULL,
          bearer TEXT NOT NULL,
          state TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL,
          UNIQUE(owner_id, agent_id, mount_id)
        );
      `);

      new VmHostPool(state, env as unknown as VmHostPoolEnv);

      for (const table of ["vm_hosts", "vm_allocations"]) {
        const columns = state.storage.sql.exec<{ name: string }>(
          `PRAGMA table_info(${table})`,
        ).toArray().map((column) => column.name);
        expect(columns).toContain("public_origin");
      }
    });
  });

  it("selects exact factories without overcommit and releases only after acknowledgement", async () => {
    const stub = pool(crypto.randomUUID());
    const hostA = await connectHost(stub, HOST_A, "donor-a", 1);
    const hostB = await connectHost(stub, HOST_B, "donor-b", 1);

    const provisionA = nextFrame(hostA.socket);
    const acquiredA = await acquire(stub, MOUNT_A);
    expect(acquiredA.response.status).toBe(201);
    expect(acquiredA.body).toMatchObject({
      factory_name: FACTORY_A,
      generation: 1,
      host_id: HOST_A,
      machine_id: `vm:${MOUNT_A}`,
      route_id: expect.stringMatching(/^vm-host:/),
      slot: 0,
    });
    const allocationA = allocationIdentity(acquiredA.body, MOUNT_A);
    const frameA = await provisionA;
    expect(frameA).toMatchObject({
      type: "provision",
      lease_id: hostA.lease.lease_id,
      epoch: hostA.lease.epoch,
      allocation_id: allocationA.allocation_id,
      generation: 1,
      slot: 0,
      machine_id: `vm:${MOUNT_A}`,
    });
    expect(frameA).not.toHaveProperty("owner_id");
    expect(frameA).not.toHaveProperty("agent_id");
    expect(frameA).not.toHaveProperty("mount_id");
    const attachment = frameA.tool_attachment as { url: string; bearer: string };
    expect(attachment.bearer).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(new URL(attachment.url).pathname).toBe(
      `/v1/vm-host-attachments/${LOCATOR}/${allocationA.allocation_id}/tool-host`,
    );

    const invalid = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: allocationA.allocation_id, bearer: "x".repeat(43) }),
    });
    expect(invalid.status).toBe(404);
    const valid = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: allocationA.allocation_id, bearer: attachment.bearer }),
    });
    expect(valid.status).toBe(200);
    await expect(valid.json()).resolves.toMatchObject({
      valid: true,
      owner_id: OWNER_A,
      agent_id: "agent-a",
      mount_id: MOUNT_A,
      public_origin: ORIGIN,
      route_id: `vm-host:${allocationA.allocation_id}:1`,
    });

    hostA.socket.send(JSON.stringify({
      type: "provisioned",
      lease_id: hostA.lease.lease_id,
      epoch: hostA.lease.epoch,
      allocation_id: allocationA.allocation_id,
      generation: 1,
      machine_id: `vm:${MOUNT_A}`,
    }));
    await eventuallyReady(stub, allocationA);

    const replay = await acquire(stub, MOUNT_A);
    expect(replay.response.status).toBe(200);
    expect(replay.body).toEqual(acquiredA.body);

    const provisionB = nextFrame(hostB.socket);
    const acquiredB = await acquire(stub, MOUNT_B, FACTORY_B);
    expect(acquiredB.body).toMatchObject({ host_id: HOST_B, slot: 0 });
    await expect(provisionB).resolves.toMatchObject({
      type: "provision",
      allocation_id: acquiredB.body.allocation_id,
    });

    const unavailable = await acquire(stub, MOUNT_C);
    expect(unavailable.response.status).toBe(409);
    expect(unavailable.body).toEqual({ error: "factory_unavailable" });

    const missing = await acquire(
      stub,
      "10000000-0000-4000-8000-000000000004",
      "missing-factory",
    );
    expect(missing.response.status).toBe(404);
    expect(missing.body).toEqual({ error: "factory_not_found" });

    const emptyPoolMissing = await pool(crypto.randomUUID()).fetch(
      "https://pool.internal/acquire",
      {
        method: "POST",
        body: JSON.stringify(acquireBody(
          "10000000-0000-4000-8000-000000000005",
          { factory_name: "system-only" },
        )),
      },
    );
    expect(emptyPoolMissing.status).toBe(404);
    await expect(emptyPoolMissing.json()).resolves.toEqual({ error: "factory_not_found" });

    const retargetedReplay = await acquire(stub, MOUNT_A, FACTORY_B);
    expect(retargetedReplay.response.status).toBe(409);
    expect(retargetedReplay.body).toEqual({ error: "allocation_conflict" });

    const releaseFrame = nextFrame(hostA.socket);
    const releasing = await stub.fetch("https://pool.internal/release", {
      method: "POST",
      body: JSON.stringify(allocationA),
    });
    expect(releasing.status).toBe(202);
    await expect(releaseFrame).resolves.toMatchObject({
      type: "release",
      allocation_id: allocationA.allocation_id,
    });
    expect((await acquire(stub, MOUNT_C)).response.status).toBe(409);

    hostA.socket.send(JSON.stringify({
      type: "released",
      lease_id: hostA.lease.lease_id,
      epoch: hostA.lease.epoch,
      allocation_id: allocationA.allocation_id,
      generation: 1,
      machine_id: `vm:${MOUNT_A}`,
    }));
    await eventuallyState(stub, allocationA, "released");

    const reusedSlotFrame = nextFrame(hostA.socket);
    const acquiredC = await acquire(stub, MOUNT_C);
    expect(acquiredC.body).toMatchObject({ host_id: HOST_A, slot: 0 });
    await expect(reusedSlotFrame).resolves.toMatchObject({
      type: "provision",
      allocation_id: acquiredC.body.allocation_id,
    });

    hostA.socket.close(1000, "test complete");
    hostB.socket.close(1000, "test complete");
  });

  it("releases a committed acquire by its pre-acquire intent and tombstones release-before-acquire", async () => {
    const stub = pool(crypto.randomUUID());
    const host = await connectHost(stub, HOST_A, "donor-a", 2);
    const provision = nextFrame(host.socket);
    const acquired = await acquire(stub, MOUNT_A);
    const provisioned = await provision;

    const release = nextFrame(host.socket);
    const releasedByIntent = await stub.fetch("https://pool.internal/release-intent", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_A)),
    });
    expect(releasedByIntent.status).toBe(202);
    await expect(release).resolves.toMatchObject({
      type: "release",
      allocation_id: acquired.body.allocation_id,
      generation: acquired.body.generation,
      machine_id: provisioned.machine_id,
    });

    const cancelledBeforeAcquire = await stub.fetch("https://pool.internal/release-intent", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_B)),
    });
    expect(cancelledBeforeAcquire.status).toBe(200);
    await expect(cancelledBeforeAcquire.json()).resolves.toEqual({ state: "released" });
    const lateAcquire = await acquire(stub, MOUNT_B);
    expect(lateAcquire.response.status).toBe(409);
    expect(lateAcquire.body).toEqual({ error: "allocation_released" });

    host.socket.close(1000, "test complete");
  });

  it("keeps the exact epoch bearer valid past its initial lease when control pings renew", async () => {
    vi.useFakeTimers();
    const startedAt = 2_000_000;
    vi.setSystemTime(startedAt);
    const stub = pool(crypto.randomUUID());
    const host = await connectHost(stub, HOST_A, "donor-a", 1);
    const provision = nextFrame(host.socket);
    const acquired = await acquire(stub, MOUNT_A);
    const frame = await provision;
    const bearer = (frame.tool_attachment as { bearer: string }).bearer;

    vi.setSystemTime(startedAt + 40_000);
    const pong = nextFrame(host.socket);
    host.socket.send(JSON.stringify({
      type: "ping",
      lease_id: host.lease.lease_id,
      epoch: host.lease.epoch,
      nonce: "renew-control",
    }));
    await expect(pong).resolves.toMatchObject({
      type: "pong",
      expires_at: startedAt + 100_000,
      nonce: "renew-control",
    });

    vi.setSystemTime(startedAt + 61_000);
    const validated = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: acquired.body.allocation_id, bearer }),
    });
    expect(validated.status).toBe(200);
    await expect(validated.json()).resolves.toMatchObject({
      allocation_id: acquired.body.allocation_id,
      generation: 1,
      route_id: `vm-host:${acquired.body.allocation_id}:1`,
      lease_expires_at: startedAt + 100_000,
    });

    host.socket.close(1000, "test complete");
  });

  it("fences only a replaced host identity and rejects donor or shape takeover", async () => {
    const stub = pool(crypto.randomUUID());
    const original = await connectHost(stub, HOST_A, "donor-a", 2);
    const independent = await connectHost(stub, HOST_B, "donor-b", 2);

    const fenced = nextFrame(original.socket);
    const replacement = await connectHost(stub, HOST_A, "donor-a", 2);
    expect(replacement.lease.epoch).toBe(original.lease.epoch + 1);
    await expect(fenced).resolves.toMatchObject({
      type: "fenced",
      epoch: replacement.lease.epoch,
    });

    const pong = nextFrame(independent.socket);
    independent.socket.send(JSON.stringify({
      type: "ping",
      lease_id: independent.lease.lease_id,
      epoch: independent.lease.epoch,
      nonce: "still-live",
    }));
    await expect(pong).resolves.toMatchObject({ type: "pong", nonce: "still-live" });

    const donorTakeover = await upgrade(stub, "donor-other");
    const donorError = nextFrame(donorTakeover);
    donorTakeover.send(JSON.stringify(attach(HOST_A, 2)));
    await expect(donorError).resolves.toMatchObject({ type: "error", code: "host_conflict" });

    const shapeTakeover = await upgrade(stub, "donor-a");
    const shapeError = nextFrame(shapeTakeover);
    shapeTakeover.send(JSON.stringify({ ...attach(HOST_A, 2), vm: { cpus: 4, memory_mib: 2048 } }));
    await expect(shapeError).resolves.toMatchObject({ type: "error", code: "host_shape_conflict" });

    const renamed = await upgrade(stub, "donor-a");
    const renamedError = nextFrame(renamed);
    renamed.send(JSON.stringify(attach(HOST_A, 2, "renamed-factory")));
    await expect(renamedError).resolves.toMatchObject({
      type: "error",
      code: "host_name_conflict",
    });

    const duplicateName = await upgrade(stub, "donor-c");
    const duplicateNameError = nextFrame(duplicateName);
    duplicateName.send(JSON.stringify(attach(HOST_C, 2, FACTORY_A)));
    await expect(duplicateNameError).resolves.toMatchObject({
      type: "error",
      code: "factory_name_conflict",
    });

    replacement.socket.close(1000, "test complete");
    independent.socket.close(1000, "test complete");
    donorTakeover.close(1000, "test complete");
    shapeTakeover.close(1000, "test complete");
    renamed.close(1000, "test complete");
    duplicateName.close(1000, "test complete");
  });

  it("reconciles retained allocations with an epoch-bound bearer and enforces the scope claim", async () => {
    const stub = pool(crypto.randomUUID());
    const first = await connectHost(stub, HOST_A, "donor-a", 2);
    const initialProvision = nextFrame(first.socket);
    const acquired = await acquire(stub, MOUNT_A);
    const allocation = allocationIdentity(acquired.body, MOUNT_A);
    const provision = await initialProvision;
    const bearer = (provision.tool_attachment as { bearer: string }).bearer;

    const denied = await stub.fetch("https://pool.internal/acquire", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_B, { owner_id: OWNER_B })),
    });
    expect(denied.status).toBe(404);

    const replacement = await connectHost(stub, HOST_A, "donor-a", 2);
    const replayedProvision = nextFrame(replacement.socket);
    replacement.socket.send(JSON.stringify({
      type: "reconcile",
      lease_id: replacement.lease.lease_id,
      epoch: replacement.lease.epoch,
      allocations: [{
        allocation_id: allocation.allocation_id,
        generation: allocation.generation,
        machine_id: `vm:${MOUNT_A}`,
        state: "ready",
      }],
    }));
    const redriven = await replayedProvision;
    expect(redriven).toMatchObject({
      type: "provision",
      allocation_id: allocation.allocation_id,
      tool_attachment: { bearer: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/) },
    });
    const successorBearer = (redriven.tool_attachment as { bearer: string }).bearer;
    expect(successorBearer).not.toBe(bearer);
    replacement.socket.send(JSON.stringify({
      type: "provisioned",
      lease_id: replacement.lease.lease_id,
      epoch: replacement.lease.epoch,
      allocation_id: allocation.allocation_id,
      generation: allocation.generation,
      machine_id: `vm:${MOUNT_A}`,
    }));
    await eventuallyReady(stub, allocation);

    const predecessor = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: allocation.allocation_id, bearer }),
    });
    expect(predecessor.status).toBe(404);
    const successor = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: allocation.allocation_id, bearer: successorBearer }),
    });
    expect(successor.status).toBe(200);
    await expect(successor.json()).resolves.toMatchObject({
      route_id: `vm-host:${allocation.allocation_id}:${replacement.lease.epoch}`,
    });

    const releaseFrame = nextFrame(replacement.socket);
    await stub.fetch("https://pool.internal/release", {
      method: "POST",
      body: JSON.stringify(allocation),
    });
    const release = await releaseFrame;
    expect(release).toMatchObject({ type: "release", allocation_id: allocation.allocation_id });

    const replayedRelease = nextFrame(replacement.socket);
    replacement.socket.send(JSON.stringify({
      type: "reconcile",
      lease_id: replacement.lease.lease_id,
      epoch: replacement.lease.epoch,
      allocations: [{
        allocation_id: allocation.allocation_id,
        generation: allocation.generation,
        machine_id: `vm:${MOUNT_A}`,
        state: "ready",
      }],
    }));
    await expect(replayedRelease).resolves.toMatchObject({
      type: "release",
      allocation_id: allocation.allocation_id,
    });

    first.socket.close(1000, "test complete");
    replacement.socket.close(1000, "test complete");
  });

  it("moves only a reconnecting host and its allocations to that host's current origin", async () => {
    const stub = pool(crypto.randomUUID());
    const firstA = await connectHost(stub, HOST_A, "donor-a", 1);
    const hostB = await connectHost(stub, HOST_B, "donor-b", 1);
    const firstProvision = nextFrame(firstA.socket);
    const allocationA = await acquire(stub, MOUNT_A);
    await firstProvision;

    const movedOrigin = "https://moved.example";
    const replacementA = await connectHost(
      stub,
      HOST_A,
      "donor-a",
      1,
      FACTORY_A,
      movedOrigin,
    );
    const movedProvision = nextFrame(replacementA.socket);
    replacementA.socket.send(JSON.stringify({
      type: "reconcile",
      lease_id: replacementA.lease.lease_id,
      epoch: replacementA.lease.epoch,
      allocations: [{
        allocation_id: allocationA.body.allocation_id,
        generation: 1,
        machine_id: `vm:${MOUNT_A}`,
        state: "ready",
      }],
    }));
    const moved = await movedProvision;
    expect(new URL((moved.tool_attachment as { url: string }).url).origin).toBe(
      movedOrigin.replace("https:", "wss:"),
    );

    const provisionB = nextFrame(hostB.socket);
    await acquire(stub, MOUNT_B, FACTORY_B);
    const unchanged = await provisionB;
    expect(new URL((unchanged.tool_attachment as { url: string }).url).origin).toBe(
      ORIGIN.replace("https:", "wss:"),
    );

    firstA.socket.close(1000, "test complete");
    replacementA.socket.close(1000, "test complete");
    hostB.socket.close(1000, "test complete");
  });

  it("keeps agent capacity exclusive while system capacity accepts distinct users", async () => {
    const agentLocator = "a".repeat(43);
    const agentStub = pool(crypto.randomUUID());
    const agentHost = await connectScopedHost(agentStub, HOST_A, 2, {
      "x-nanocodex-pool-scope": "agent",
      "x-nanocodex-pool-owner": OWNER_A,
      "x-nanocodex-pool-agent": "agent-a",
      "x-nanocodex-donor-id": OWNER_A,
      "x-nanocodex-pool-locator": agentLocator,
    });
    const agentProvision = nextFrame(agentHost.socket);
    const admitted = await agentStub.fetch("https://pool.internal/acquire", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_A, { pool_locator: agentLocator })),
    });
    expect(admitted.status).toBe(201);
    const admittedBody = await admitted.json<Record<string, unknown>>();
    const agentProvisionFrame = await agentProvision;
    expect(agentProvisionFrame).toMatchObject({ type: "provision" });
    const sibling = await agentStub.fetch("https://pool.internal/acquire", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_B, {
        agent_id: "agent-b",
        pool_locator: agentLocator,
      })),
    });
    expect(sibling.status).toBe(404);
    agentHost.socket.close(1000, "control lease ended");
    await eventuallyAttachmentDenied(
      agentStub,
      admittedBody.allocation_id as string,
      (agentProvisionFrame.tool_attachment as { bearer: string }).bearer,
    );

    const systemLocator = "s".repeat(43);
    const systemStub = pool(crypto.randomUUID());
    const systemHost = await connectScopedHost(systemStub, HOST_B, 2, {
      "x-nanocodex-pool-scope": "system",
      "x-nanocodex-donor-id": "system",
      "x-nanocodex-pool-locator": systemLocator,
    }, FACTORY_A);
    const firstProvision = nextFrame(systemHost.socket);
    const first = await systemStub.fetch("https://pool.internal/acquire", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_B, { pool_locator: systemLocator })),
    });
    expect(first.status).toBe(201);
    await expect(firstProvision).resolves.toMatchObject({ type: "provision" });
    const secondProvision = nextFrame(systemHost.socket);
    const second = await systemStub.fetch("https://pool.internal/acquire", {
      method: "POST",
      body: JSON.stringify(acquireBody(MOUNT_C, {
        owner_id: OWNER_B,
        agent_id: "agent-b",
        pool_locator: systemLocator,
      })),
    });
    expect(second.status).toBe(201);
    await expect(secondProvision).resolves.toMatchObject({ type: "provision" });

    systemHost.socket.close(1000, "test complete");
  });
});

function pool(name: string): DurableObjectStub<VmHostPool> {
  const namespace = (env as unknown as {
    NANOCODEX_VM_HOST_POOLS: DurableObjectNamespace<VmHostPool>;
  }).NANOCODEX_VM_HOST_POOLS;
  return namespace.getByName(name);
}

async function connectHost(
  stub: DurableObjectStub<VmHostPool>,
  hostId: string,
  donorId: string,
  maxVms: number,
  factoryName = hostId === HOST_A ? FACTORY_A : FACTORY_B,
  publicOrigin = ORIGIN,
): Promise<{ socket: WebSocket; lease: LeaseFrame }> {
  const socket = await upgrade(stub, donorId, publicOrigin);
  const leased = nextFrame(socket);
  socket.send(JSON.stringify(attach(hostId, maxVms, factoryName)));
  const lease = await leased;
  expect(lease).toMatchObject({
    type: "lease",
    protocol_version: 1,
    max_vms: maxVms,
    vm: { cpus: 2, memory_mib: 1024 },
  });
  return { socket, lease: lease as unknown as LeaseFrame };
}

async function connectScopedHost(
  stub: DurableObjectStub<VmHostPool>,
  hostId: string,
  maxVms: number,
  headers: Record<string, string>,
  factoryName = hostId === HOST_A ? FACTORY_A : FACTORY_B,
): Promise<{ socket: WebSocket; lease: LeaseFrame }> {
  const response = await stub.fetch("https://pool.internal/host", {
    headers: {
      upgrade: "websocket",
      "x-nanocodex-public-origin": ORIGIN,
      ...headers,
    },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  socket.accept();
  const leased = nextFrame(socket);
  socket.send(JSON.stringify(attach(hostId, maxVms, factoryName)));
  return { socket, lease: await leased as unknown as LeaseFrame };
}

type LeaseFrame = {
  type: "lease";
  lease_id: string;
  epoch: number;
  expires_at: number;
};

async function upgrade(
  stub: DurableObjectStub<VmHostPool>,
  donorId: string,
  publicOrigin = ORIGIN,
): Promise<WebSocket> {
  const response = await stub.fetch("https://pool.internal/host", {
    headers: {
      upgrade: "websocket",
      "x-nanocodex-pool-scope": "account",
      "x-nanocodex-pool-owner": OWNER_A,
      "x-nanocodex-donor-id": donorId,
      "x-nanocodex-public-origin": publicOrigin,
      "x-nanocodex-pool-locator": LOCATOR,
    },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  socket.accept();
  return socket;
}

function attach(
  hostId: string,
  maxVms: number,
  factoryName = hostId === HOST_A ? FACTORY_A : FACTORY_B,
) {
  return {
    type: "attach",
    protocol_version: 1,
    host_id: hostId,
    factory_name: factoryName,
    max_vms: maxVms,
    vm: { cpus: 2, memory_mib: 1024 },
  };
}

async function acquire(
  stub: DurableObjectStub<VmHostPool>,
  mountId: string,
  factoryName = FACTORY_A,
): Promise<{ response: Response; body: Record<string, unknown> }> {
  const response = await stub.fetch("https://pool.internal/acquire", {
    method: "POST",
    body: JSON.stringify(acquireBody(mountId, { factory_name: factoryName })),
  });
  return { response, body: await response.clone().json<Record<string, unknown>>() };
}

function acquireBody(mountId: string, overrides: Record<string, unknown> = {}) {
  return {
    factory_name: FACTORY_A,
    owner_id: OWNER_A,
    organization_id: "org-a",
    team_id: "team-a",
    authorization_epoch: 3,
    agent_id: "agent-a",
    mount_id: mountId,
    pool_locator: LOCATOR,
    ...overrides,
  };
}

function allocationIdentity(body: Record<string, unknown>, mountId: string) {
  return {
    owner_id: OWNER_A,
    agent_id: "agent-a",
    mount_id: mountId,
    allocation_id: body.allocation_id as string,
    generation: body.generation as number,
    pool_locator: LOCATOR,
  };
}

async function eventuallyReady(
  stub: DurableObjectStub<VmHostPool>,
  identity: ReturnType<typeof allocationIdentity>,
): Promise<void> {
  await eventuallyState(stub, identity, "ready");
}

async function eventuallyState(
  stub: DurableObjectStub<VmHostPool>,
  identity: ReturnType<typeof allocationIdentity>,
  state: string,
): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const response = await stub.fetch("https://pool.internal/ready", {
      method: "POST",
      body: JSON.stringify(identity),
    });
    const body = await response.json<{ state: string }>();
    if (body.state === state) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`allocation did not reach ${state}`);
}

function nextFrame(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const onMessage = (event: MessageEvent) => {
      cleanup();
      try { resolve(JSON.parse(String(event.data)) as Record<string, unknown>); }
      catch (error) { reject(error); }
    };
    const onError = () => {
      cleanup();
      reject(new Error("VM host socket failed"));
    };
    const cleanup = () => {
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("error", onError);
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
  });
}

async function eventuallyAttachmentDenied(
  stub: DurableObjectStub<VmHostPool>,
  allocationId: string,
  bearer: string,
): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const response = await stub.fetch("https://pool.internal/validate-attachment", {
      method: "POST",
      body: JSON.stringify({ allocation_id: allocationId, bearer }),
    });
    if (response.status === 404) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("attachment bearer remained valid after its control lease ended");
}
