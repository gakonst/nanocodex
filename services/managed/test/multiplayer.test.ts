import { env } from "cloudflare:workers";
import {
  evictDurableObject,
  runDurableObjectAlarm,
  runInDurableObject,
  SELF,
} from "cloudflare:test";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { Env } from "../src/index";
import { MultiplayerRoom } from "../src/multiplayer-room";
import { ROOM_ENDED_CLOSE_CODE } from "../src/multiplayer-protocol";

const ORIGIN = "https://example.test";
const admin = { authorization: "Bearer test-admin-token" };
const testEnv = env as unknown as Env;
const USER_ID = "22222222-2222-4222-8222-222222222222";
const API_KEY = `ncx_live_${"r".repeat(12)}_${"m".repeat(43)}`;
const allocator = {
  authorization: `Bearer ${API_KEY}`,
};
const rooms = new Set<string>();

beforeAll(async () => seedApiKey(USER_ID, API_KEY));

afterEach(async () => {
  await Promise.all([...rooms].map(async (roomId) => {
    await SELF.fetch(`${ORIGIN}/v1/rooms/${roomId}`, { method: "DELETE", headers: admin });
    rooms.delete(roomId);
  }));
});

describe("durable Multiplayer rooms", () => {
  it("rejects a forged signed-shape room locator before Durable Object allocation", async () => {
    const forged = `018f25e8-7b51-7a32-8c4d-0123456789ab~${"A".repeat(43)}`;
    const response = await SELF.fetch(`${ORIGIN}/v1/rooms/${forged}`);
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({ error: "not_found" });
  });

  it("protects creation and never exposes the private managed agent", async () => {
    expect((await SELF.fetch(`${ORIGIN}/v1/rooms`, { method: "POST" })).status).toBe(401);

    const owner = await createRoom("Ada");
    expect(owner.room_id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}~[A-Za-z0-9_-]{43}$/,
    );
    expect(owner.websocket_url).toBe(`wss://example.test/v1/rooms/${owner.room_id}/ws`);
    expect(owner.invite_url).toContain(`/multiplayer?room=${owner.room_id}#invite=`);
    expect(JSON.stringify(owner)).not.toContain("agent_id");
    expect(JSON.stringify(owner)).not.toContain("NANOCODEX_OPENAI_API_KEY");
    expect(owner.cookie).toContain("HttpOnly");
    expect(owner.cookie).toContain("SameSite=Strict");
  });

  it("replays one lost or concurrent creation receipt across Durable Object restart", async () => {
    const createId = randomCreateId();
    const [first, concurrent] = await Promise.all([
      createRoomResponse("Ada", createId),
      createRoomResponse("Ada", createId),
    ]);
    expect([first.status, concurrent.status]).toEqual([201, 201]);
    const firstCookie = first.headers.get("set-cookie");
    const concurrentCookie = concurrent.headers.get("set-cookie");
    expect(firstCookie).toBeTruthy();
    expect(concurrentCookie).toBe(firstCookie);
    const [firstReceipt, concurrentReceipt] = await Promise.all([
      first.json<Omit<RoomReceipt, "cookie">>(),
      concurrent.json<Omit<RoomReceipt, "cookie">>(),
    ]);
    expect(concurrentReceipt).toEqual(firstReceipt);
    rooms.add(firstReceipt.room_id);

    const room = testEnv.NANOCODEX_ROOMS.getByName(firstReceipt.room_id);
    const agentId = await runInDurableObject(room, (_instance, state) => (
      state.storage.sql.exec<{ agent_id: string }>(
        "SELECT agent_id FROM room_state WHERE singleton = 1",
      ).toArray()[0]!.agent_id
    ));
    const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
    const durable = await runInDurableObject(quota, (_instance, state) => ({
      creation: state.storage.sql.exec<{
        create_id_hash: string;
        request_hash: string;
      }>(
        `SELECT create_id_hash, request_hash
         FROM multiplayer_room_reservations WHERE room_id = ?`,
        firstReceipt.room_id,
      ).toArray()[0]!,
      creationColumns: state.storage.sql.exec<{ name: string }>(
        "PRAGMA table_info(multiplayer_room_reservations)",
      ).toArray().map((column) => column.name),
      creationCount: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM multiplayer_room_reservations WHERE room_id = ?",
        firstReceipt.room_id,
      ).toArray()[0]!.count,
      leaseCount: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM multiplayer_rooms WHERE room_id = ?",
        firstReceipt.room_id,
      ).toArray()[0]!.count,
    }));
    expect(durable.creation.create_id_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(durable.creation.request_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(JSON.stringify(durable.creation)).not.toContain(createId);
    expect(JSON.stringify(durable.creation)).not.toContain(firstReceipt.invite);
    expect(JSON.stringify(durable.creation)).not.toContain(cookiePair(firstCookie!).split("=", 2)[1]);
    expect(JSON.stringify(durable.creation)).not.toContain(agentId);
    expect(durable.creationColumns).toEqual([
      "create_id_hash",
      "request_hash",
      "room_id",
      "created_at",
      "expires_at",
    ]);
    expect(durable.creationCount).toBe(1);
    expect(durable.leaseCount).toBe(1);
    expect(await roomJoinState(firstReceipt.room_id)).toEqual({
      events: 1,
      members: 1,
      receipts: 0,
      redemptions: 0,
    });
    expect(await sessionStateCount(agentId)).toBe(1);

    await Promise.all([evictDurableObject(quota), evictDurableObject(room)]);
    const afterRestart = await createRoomResponse("Ada", createId);
    expect(afterRestart.status).toBe(201);
    expect(afterRestart.headers.get("set-cookie")).toBe(firstCookie);
    expect(await afterRestart.json()).toEqual(firstReceipt);
    expect(await quotaLeaseCount(firstReceipt.room_id)).toBe(1);
    expect(await sessionStateCount(agentId)).toBe(1);
  });

  it("conflicts a changed creation payload without allocating another room or agent", async () => {
    const createId = randomCreateId();
    const first = await createRoomResponse("Ada", createId);
    expect(first.status).toBe(201);
    const receipt = await first.json<Omit<RoomReceipt, "cookie">>();
    rooms.add(receipt.room_id);
    const room = testEnv.NANOCODEX_ROOMS.getByName(receipt.room_id);
    const agentId = await runInDurableObject(room, (_instance, state) => (
      state.storage.sql.exec<{ agent_id: string }>(
        "SELECT agent_id FROM room_state WHERE singleton = 1",
      ).toArray()[0]!.agent_id
    ));

    const changed = await createRoomResponse("Grace", createId);
    expect(changed.status).toBe(409);
    expect(await changed.json()).toEqual({ error: "create_id_conflict" });
    expect(await roomJoinState(receipt.room_id)).toEqual({
      events: 1,
      members: 1,
      receipts: 0,
      redemptions: 0,
    });
    expect(await quotaLeaseCount(receipt.room_id)).toBe(1);
    expect(await sessionStateCount(agentId)).toBe(1);
  });

  it("rejects provider/runtime fields and queries outside the room protocol", async () => {
    const owner = await createRoom("Ada");
    const forgedJoin = await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}/join`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        invite: owner.invite,
        display_name: "Mallory",
        join_id: "j".repeat(43),
        endpoint: "https://attacker.test",
      }),
    });
    expect(forgedJoin.status).toBe(400);
    expect(await forgedJoin.json()).toEqual({ error: "invalid_request" });

    for (const request of [
      new Request(`${ORIGIN}/v1/rooms?provider=openai`, { method: "POST", headers: allocator }),
      new Request(`${ORIGIN}/v1/rooms/${owner.room_id}?agent_id=forged`, {
        headers: { cookie: cookiePair(owner.cookie) },
      }),
      new Request(`${ORIGIN}/v1/rooms/${owner.room_id}/ws?cursor=0&auth_mode=chatgpt`, {
        headers: {
          cookie: cookiePair(owner.cookie),
          origin: ORIGIN,
          upgrade: "websocket",
        },
      }),
    ]) {
      const response = await SELF.fetch(request);
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_request" });
    }
  });

  it("binds a service-bound allocation to its authenticated public origin", async () => {
    const publicOrigin = "https://play.example:8443";
    const response = await SELF.fetch(`${publicOrigin}/v1/rooms`, {
      method: "POST",
      headers: {
        ...allocator,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        create_id: randomCreateId(),
        display_name: "Ada",
      }),
    });
    expect(response.status).toBe(201);
    const cookie = response.headers.get("set-cookie");
    expect(cookie).toBeTruthy();
    const receipt = await response.json<Omit<RoomReceipt, "cookie">>();
    rooms.add(receipt.room_id);
    expect(receipt.websocket_url).toBe(`${publicOrigin.replace("https:", "wss:")}/v1/rooms/${receipt.room_id}/ws`);
    expect(receipt.invite_url).toContain(`${publicOrigin}/multiplayer?room=${receipt.room_id}#invite=`);
    expect(await runInDurableObject(
      testEnv.NANOCODEX_ROOMS.getByName(receipt.room_id),
      async (_instance, state) => state.storage.sql.exec<{ public_origin: string }>(
        "SELECT public_origin FROM room_state WHERE singleton = 1",
      ).toArray()[0]?.public_origin,
    )).toBe(publicOrigin);

    const upgraded = await SELF.fetch(receipt.websocket_url.replace("wss:", "https:"), {
      headers: {
        cookie: cookiePair(cookie!),
        origin: publicOrigin,
        upgrade: "websocket",
      },
    });
    expect(upgraded.status).toBe(101);
    upgraded.webSocket?.accept();
    upgraded.webSocket?.close(1000, "done");

    const invalid = await SELF.fetch("https://managed.internal/v1/rooms", {
      method: "POST",
      headers: { ...allocator, "content-type": "application/json" },
      body: JSON.stringify({
        create_id: randomCreateId(),
        public_origin: "https://play.example/path",
      }),
    });
    expect(invalid.status).toBe(400);
    expect(await invalid.json()).toEqual({ error: "invalid_request" });
  });

  it("retries a lost idempotent reservation without another allocation", async () => {
    const originalQuota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA;
    const createId = randomCreateId();
    let roomId: string | undefined;
    testEnv.NANOCODEX_MULTIPLAYER_QUOTA = {
      getByName(name: string) {
        const quota = originalQuota.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === "POST" && new URL(request.url).pathname === "/rooms") {
              const reservation = await request.clone().json<{ room_id: string }>();
              const response = await quota.fetch(input, init);
              if (response.ok) {
                roomId = reservation.room_id;
                await response.body?.cancel();
                throw new Error("injected lost quota reservation response");
              }
              return response;
            }
            return quota.fetch(input, init);
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_MULTIPLAYER_QUOTA"];

    let creation: Response | undefined;
    try {
      creation = await SELF.fetch(`${ORIGIN}/v1/rooms`, {
        method: "POST",
        headers: { ...allocator, "content-type": "application/json" },
        body: JSON.stringify({ create_id: createId, display_name: "Ada" }),
      });
    } finally {
      testEnv.NANOCODEX_MULTIPLAYER_QUOTA = originalQuota;
    }
    expect(creation!.status).toBe(503);
    expect(await creation!.json()).toEqual({ error: "multiplayer_capacity_unavailable" });
    expect(roomId).toBeTruthy();
    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId!);
    expect(await quotaLeaseCount(roomId!)).toBe(1);
    expect(await roomCleanupState(room)).toEqual({ alarm: null, rooms: 0, status: null });
    await evictDurableObject(originalQuota.getByName("global"));

    const recovered = await createRoomResponse("Ada", createId);
    expect(recovered.status).toBe(201);
    const cookie = recovered.headers.get("set-cookie");
    expect(cookie).toBeTruthy();
    const receipt = await recovered.json<Omit<RoomReceipt, "cookie">>();
    expect(receipt.room_id).toBe(roomId);
    rooms.add(receipt.room_id);
    const agentId = await runInDurableObject(room, (_instance, state) => (
      state.storage.sql.exec<{ agent_id: string }>(
        "SELECT agent_id FROM room_state WHERE singleton = 1",
      ).toArray()[0]!.agent_id
    ));
    expect(await quotaLeaseCount(receipt.room_id)).toBe(1);
    expect(await roomJoinState(receipt.room_id)).toEqual({
      events: 1,
      members: 1,
      receipts: 0,
      redemptions: 0,
    });
    expect(await sessionStateCount(agentId)).toBe(1);
  });

  it("bounds quota reservation headers and retries the same deterministic create", async () => {
    const originalQuota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA;
    const originalTimeout = testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS;
    const createId = randomCreateId();
    testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = "20";
    testEnv.NANOCODEX_MULTIPLAYER_QUOTA = {
      getByName() {
        return { fetch: () => new Promise<Response>(() => {}) } as unknown as DurableObjectStub;
      },
    } as unknown as Env["NANOCODEX_MULTIPLAYER_QUOTA"];

    try {
      const failed = await within(createRoomResponse("Ada", createId), "quota reservation headers");
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "multiplayer_capacity_unavailable" });
    } finally {
      testEnv.NANOCODEX_MULTIPLAYER_QUOTA = originalQuota;
      testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = originalTimeout;
    }

    const recovered = await createRoomResponse("Ada", createId);
    expect(recovered.status).toBe(201);
    const receipt = await recovered.json<Omit<RoomReceipt, "cookie">>();
    rooms.add(receipt.room_id);
    await deleteTestRoom(receipt.room_id);
  });

  it("bounds quota receipt settlement after an ambiguous committed reservation", async () => {
    const originalQuota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA;
    const originalTimeout = testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS;
    const createId = randomCreateId();
    let reservedRoomId: string | undefined;
    testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = "20";
    testEnv.NANOCODEX_MULTIPLAYER_QUOTA = {
      getByName(name: string) {
        const quota = originalQuota.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === "POST" && new URL(request.url).pathname === "/rooms") {
              reservedRoomId = (await request.clone().json<{ room_id: string }>()).room_id;
              const committed = await quota.fetch(input, init);
              expect(committed.ok).toBe(true);
              await committed.body?.cancel();
              return new Response(noncooperativeBody(), {
                status: 201,
                headers: { "content-type": "application/json" },
              });
            }
            return quota.fetch(input, init);
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_MULTIPLAYER_QUOTA"];

    let failed: Response | undefined;
    try {
      failed = await within(createRoomResponse("Ada", createId), "quota receipt body");
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "multiplayer_capacity_unavailable" });
    } finally {
      testEnv.NANOCODEX_MULTIPLAYER_QUOTA = originalQuota;
      testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = originalTimeout;
    }
    expect(await quotaLeaseCount(reservedRoomId!)).toBe(1);

    const recovered = await createRoomResponse("Ada", createId);
    expect(recovered.status).toBe(201);
    const receipt = await recovered.json<Omit<RoomReceipt, "cookie">>();
    expect(receipt.room_id).toBe(reservedRoomId);
    rooms.add(receipt.room_id);
    await deleteTestRoom(receipt.room_id);
  });

  it("bounds room initialization receipt parsing and reconciles the committed room", async () => {
    const originalFetch = MultiplayerRoom.prototype.fetch;
    const originalTimeout = testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS;
    const createId = randomCreateId();
    testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = "20";
    const fetchSpy = vi.spyOn(MultiplayerRoom.prototype, "fetch").mockImplementation(
      async function (this: MultiplayerRoom, request: Request): Promise<Response> {
        if (request.method === "PUT" && new URL(request.url).pathname === "/initialize") {
          const committed = await originalFetch.call(this, request);
          expect(committed.ok).toBe(true);
          await committed.body?.cancel();
          return new Response(noncooperativeBody(), {
            status: 201,
            headers: { "content-type": "application/json" },
          });
        }
        return originalFetch.call(this, request);
      },
    );

    try {
      const failed = await within(createRoomResponse("Ada", createId), "room receipt body");
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "room_initialization_failed" });
    } finally {
      fetchSpy.mockRestore();
      testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = originalTimeout;
    }

    const recovered = await createRoomResponse("Ada", createId);
    expect(recovered.status).toBe(201);
    const receipt = await recovered.json<Omit<RoomReceipt, "cookie">>();
    rooms.add(receipt.room_id);
    expect(await quotaLeaseCount(receipt.room_id)).toBe(1);
    await deleteTestRoom(receipt.room_id);
  });

  it("bounds nonsettling room initialization headers and reuses its reservation", async () => {
    const originalRooms = testEnv.NANOCODEX_ROOMS;
    const originalTimeout = testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS;
    const createId = randomCreateId();
    let roomId: string | undefined;
    testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = "20";
    testEnv.NANOCODEX_ROOMS = {
      getByName(name: string) {
        roomId = name;
        return { fetch: () => new Promise<Response>(() => {}) } as unknown as DurableObjectStub;
      },
    } as unknown as Env["NANOCODEX_ROOMS"];

    try {
      const failed = await within(createRoomResponse("Ada", createId), "room initialization headers");
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "room_initialization_failed" });
      expect(await quotaLeaseCount(roomId!)).toBe(1);
    } finally {
      testEnv.NANOCODEX_ROOMS = originalRooms;
      testEnv.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS = originalTimeout;
    }

    const recovered = await createRoomResponse("Ada", createId);
    expect(recovered.status).toBe(201);
    const receipt = await recovered.json<Omit<RoomReceipt, "cookie">>();
    expect(receipt.room_id).toBe(roomId);
    rooms.add(receipt.room_id);
    await deleteTestRoom(receipt.room_id);
  });

  it("serializes quota release behind an in-flight room reservation", async () => {
    const { roomId } = testRoomIdentity();
    const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
    const ordering = await runInDurableObject(quota, async (instance) => {
      const body = JSON.stringify({ room_id: roomId, expires_at: Date.now() + 60_000 });
      let bodyReadStarted!: () => void;
      const started = new Promise<void>((resolve) => {
        bodyReadStarted = resolve;
      });
      let releaseBody!: () => void;
      const blocked = new Promise<void>((resolve) => {
        releaseBody = resolve;
      });
      const encoded = new TextEncoder().encode(body);
      const requestBody = new ReadableStream<Uint8Array>({
        start(controller) {
          bodyReadStarted();
          void blocked.then(() => {
            controller.enqueue(encoded);
            controller.close();
          });
        },
      });
      const reserving = instance.fetch(new Request("https://quota.internal/rooms", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: requestBody,
      }));
      await started;
      let releaseSettled = false;
      const releasing = instance.fetch(new Request(
        `https://quota.internal/rooms/${encodeURIComponent(roomId)}`,
        { method: "DELETE" },
      )).finally(() => {
        releaseSettled = true;
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
      const overtookReservation = releaseSettled;
      releaseBody();
      const [reserved, released] = await Promise.all([reserving, releasing]);
      return {
        overtookReservation,
        released: released.status,
        reserved: reserved.status,
      };
    });
    expect(ordering).toEqual({ overtookReservation: false, released: 204, reserved: 201 });
    expect(await quotaLeaseCount(roomId)).toBe(0);
  });

  it("keeps one quota lease for an exact reservation replay and conflicts changed identity", async () => {
    const { roomId } = testRoomIdentity();
    const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
    const createIdHash = randomHash();
    const requestHash = randomHash();
    const reservation = {
      room_id: roomId,
      expires_at: Date.now() + 60_000,
      create_id_hash: createIdHash,
      request_hash: requestHash,
    };
    const first = await quota.fetch("https://quota.internal/rooms", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(reservation),
    });
    expect(first.status).toBe(201);
    expect(await first.json()).toMatchObject({ room_id: roomId, replayed: false });
    const replay = await quota.fetch("https://quota.internal/rooms", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(reservation),
    });
    expect(replay.status).toBe(200);
    expect(await replay.json()).toMatchObject({ room_id: roomId, replayed: true });
    const conflict = await quota.fetch("https://quota.internal/rooms", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...reservation, request_hash: randomHash() }),
    });
    expect(conflict.status).toBe(409);
    expect(await conflict.json()).toEqual({ error: "create_id_conflict" });
    expect(await quotaLeaseCount(roomId)).toBe(1);
    expect((await quota.fetch(
      `https://quota.internal/rooms/${encodeURIComponent(roomId)}`,
      { method: "DELETE" },
    )).status).toBe(204);
    expect(await quotaLeaseCount(roomId)).toBe(0);
  });

  it("serializes admin cleanup behind initialization before its ownership write", async () => {
    const { roomId, agentId } = testRoomIdentity();
    rooms.add(roomId);
    const reservation = await testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global").fetch(
      "https://quota.internal/rooms",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ room_id: roomId, expires_at: Date.now() + 60_000 }),
      },
    );
    expect(reservation.status).toBe(201);
    await reservation.body?.cancel();

    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId);
    const ordering = await runInDurableObject(room, async (instance) => {
      const body = JSON.stringify({
        room_id: roomId,
        agent_id: agentId,
        owner_id: USER_ID,
        public_origin: ORIGIN,
        owner_name: "Ada",
        create_id_hash: randomHash(),
        request_hash: randomHash(),
        invite: "i".repeat(43),
        member_id: crypto.randomUUID(),
        member_token: "m".repeat(43),
      });
      let bodyReadStarted!: () => void;
      const started = new Promise<void>((resolve) => {
        bodyReadStarted = resolve;
      });
      let releaseBody!: () => void;
      const blocked = new Promise<void>((resolve) => {
        releaseBody = resolve;
      });
      const encoded = new TextEncoder().encode(body);
      const requestBody = new ReadableStream<Uint8Array>({
        start(controller) {
          bodyReadStarted();
          void blocked.then(() => {
            controller.enqueue(encoded);
            controller.close();
          });
        },
      });
      const request = new Request("https://room.internal/initialize", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: requestBody,
      });
      const initializing = instance.fetch(request);
      await started;
      let cleanupSettled = false;
      const cleaning = instance.fetch(new Request("https://room.internal/admin", {
        method: "DELETE",
      })).finally(() => {
        cleanupSettled = true;
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
      const overtookInitialization = cleanupSettled;
      releaseBody();
      const [initialized, cleaned] = await Promise.all([initializing, cleaning]);
      return {
        cleaned: cleaned.status,
        initialized: initialized.status,
        overtookInitialization,
      };
    });
    expect(ordering).toEqual({ cleaned: 204, initialized: 201, overtookInitialization: false });
    expect(await roomCleanupState(room)).toEqual({ alarm: null, rooms: 0, status: null });
    expect(await quotaLeaseCount(roomId)).toBe(0);
    expect(await sessionStateCount(agentId)).toBe(0);
    rooms.delete(roomId);
  });

  it("keeps the initialization watchdog out of the live request", async () => {
    const { roomId, agentId } = testRoomIdentity();
    rooms.add(roomId);
    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId);
    const observation = await runInDurableObject(room, async (instance, state) => {
      const mutable = instance as unknown as {
        env: Env;
        fetch(request: Request): Promise<Response>;
      };
      const originalEnv = mutable.env;
      let childStarted!: () => void;
      const started = new Promise<void>((resolve) => {
        childStarted = resolve;
      });
      let releaseChild!: () => void;
      const blocked = new Promise<void>((resolve) => {
        releaseChild = resolve;
      });
      mutable.env = {
        ...originalEnv,
        NANOCODEX_SESSIONS: {
          idFromName(name: string) {
            return originalEnv.NANOCODEX_SESSIONS.idFromName(name);
          },
          getByName(name: string) {
            const session = originalEnv.NANOCODEX_SESSIONS.getByName(name);
            return {
              async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
                const request = new Request(input, init);
                if (request.method === "PUT" && new URL(request.url).pathname === "/initialize") {
                  childStarted();
                  await blocked;
                }
                return session.fetch(input, init);
              },
            } as DurableObjectStub;
          },
        } as Env["NANOCODEX_SESSIONS"],
      };
      const initializing = mutable.fetch(new Request("https://room.internal/initialize", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          room_id: roomId,
          agent_id: agentId,
          owner_id: USER_ID,
          public_origin: ORIGIN,
          owner_name: "Ada",
          create_id_hash: randomHash(),
          request_hash: randomHash(),
          invite: "i".repeat(43),
          member_id: crypto.randomUUID(),
          member_token: "m".repeat(43),
        }),
      }));
      await started;
      const now = Date.now();
      const alarm = await state.storage.getAlarm();
      releaseChild();
      try {
        const response = await initializing;
        return { alarmDelay: alarm === null ? null : alarm - now, status: response.status };
      } finally {
        mutable.env = originalEnv;
      }
    });
    expect(observation.status).toBe(201);
    expect(observation.alarmDelay).not.toBeNull();
    expect(observation.alarmDelay!).toBeGreaterThan(500);
  });

  it("retains a child tombstone across eviction so held late initialization cannot recreate it", async () => {
    const { roomId, agentId } = testRoomIdentity();
    rooms.add(roomId);
    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId);
    const first = await runInDurableObject(room, async (instance) => {
      const mutable = instance as unknown as {
        env: Env;
        fetch(request: Request): Promise<Response>;
      };
      const originalEnv = mutable.env;
      let childStarted!: () => void;
      const started = new Promise<void>((resolve) => { childStarted = resolve; });
      let releaseChild!: () => void;
      const held = new Promise<void>((resolve) => { releaseChild = resolve; });
      const child = originalEnv.NANOCODEX_SESSIONS.getByName(agentId);
      mutable.env = {
        ...originalEnv,
        NANOCODEX_SESSIONS: {
          idFromName(name: string) {
            return originalEnv.NANOCODEX_SESSIONS.idFromName(name);
          },
          getByName(name: string) {
            const session = originalEnv.NANOCODEX_SESSIONS.getByName(name);
            return {
              async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
                const request = new Request(input, init);
                if (request.method === "PUT"
                  && new URL(request.url).pathname === "/initialize") {
                  childStarted();
                  await held;
                }
                return session.fetch(input, init);
              },
            } as DurableObjectStub;
          },
        } as Env["NANOCODEX_SESSIONS"],
      };
      const initializing = mutable.fetch(new Request("https://room.internal/initialize", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          room_id: roomId,
          agent_id: agentId,
          owner_id: USER_ID,
          public_origin: ORIGIN,
          owner_name: "Ada",
          create_id_hash: randomHash(),
          request_hash: randomHash(),
          invite: "i".repeat(43),
          member_id: crypto.randomUUID(),
          member_token: "m".repeat(43),
        }),
      }));
      await started;
      const deleted = await child.fetch("https://session.internal/session", {
        method: "DELETE",
      });
      releaseChild();
      try {
        const initialized = await initializing;
        return { deleted: deleted.status, initialized: initialized.status };
      } finally {
        mutable.env = originalEnv;
      }
    });

    expect(first).toEqual({ deleted: 204, initialized: 503 });
    const child = testEnv.NANOCODEX_SESSIONS.getByName(agentId);
    expect(await sessionStateCount(agentId)).toBe(0);
    expect(await sessionInitializationState(agentId)).toBe("deleted");

    await Promise.all([evictDurableObject(child), evictDurableObject(room)]);
    const coldRetry = await child.fetch("https://session.internal/initialize", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        session_id: agentId,
        owner_id: USER_ID,
        public_origin: ORIGIN,
        runtime_profile: "multiplayer",
      }),
    });
    expect(coldRetry.status).toBe(409);
    expect(await runDurableObjectAlarm(room)).toBe(true);
    expect(await sessionStateCount(agentId)).toBe(0);
    expect(await sessionInitializationState(agentId)).toBe("deleted");
    expect(await roomCleanupState(room)).toMatchObject({ rooms: 1, status: "initializing" });

    const cleaned = await room.fetch(
      `https://room.internal/admin?room_id=${encodeURIComponent(roomId)}`,
      {
        method: "DELETE",
      },
    );
    expect(cleaned.status).toBe(204);
    rooms.delete(roomId);
    expect(await sessionStateCount(agentId)).toBe(0);
    expect(await sessionInitializationState(agentId)).toBe("deleted");
  });

  it("keeps an ambiguously initialized room leased for exact create replay", async () => {
    const originalFetch = MultiplayerRoom.prototype.fetch;
    let roomId: string | undefined;
    let agentId: string | undefined;
    const cleanupOrder: string[] = [];
    const fetchSpy = vi.spyOn(MultiplayerRoom.prototype, "fetch").mockImplementation(
      async function (this: MultiplayerRoom, request: Request): Promise<Response> {
        const path = new URL(request.url).pathname;
        if (request.method === "PUT" && path === "/initialize") {
          const initialization = await request.clone().json<{
            room_id: string;
            agent_id: string;
          }>();
          const response = await originalFetch.call(this, request);
          if (response.ok) {
            roomId = initialization.room_id;
            agentId = initialization.agent_id;
            rooms.add(initialization.room_id);
            cleanupOrder.push("initialization_committed");
            await response.body?.cancel();
            return new Response("injected ambiguous initialization response", {
              status: 201,
              headers: { "content-type": "application/json" },
            });
          }
          return response;
        }
        return originalFetch.call(this, request);
      },
    );

    let creation: Response | undefined;
    try {
      creation = await SELF.fetch(`${ORIGIN}/v1/rooms`, {
        method: "POST",
        headers: { ...allocator, "content-type": "application/json" },
        body: JSON.stringify({ create_id: randomCreateId(), display_name: "Ada" }),
      });
    } finally {
      fetchSpy.mockRestore();
    }
    expect(creation!.status).toBe(503);
    expect(await creation!.json()).toEqual({ error: "room_initialization_failed" });
    expect(cleanupOrder).toEqual(["initialization_committed"]);
    expect(roomId).toBeTruthy();
    expect(agentId).toBeTruthy();
    rooms.add(roomId!);

    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId!);
    expect(await roomCleanupState(room)).toMatchObject({ rooms: 1, status: "ready" });
    expect(await quotaLeaseCount(roomId!)).toBe(1);
    expect(await sessionStateCount(agentId!)).toBe(1);

    const cleaned = await SELF.fetch(`${ORIGIN}/v1/rooms/${roomId}`, {
      method: "DELETE",
      headers: admin,
    });
    expect(cleaned.status).toBe(204);
    rooms.delete(roomId!);
    expect(await roomCleanupState(room)).toEqual({ alarm: null, rooms: 0, status: null });
    expect(await quotaLeaseCount(roomId!)).toBe(0);
    expect(await sessionStateCount(agentId!)).toBe(0);
  });

  it("deletes an expired initializing room instead of retrying initialization", async () => {
    const owner = await createRoom("Ada");
    const stub = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    await runInDurableObject(stub, async (instance, state) => {
      state.storage.sql.exec(
        "UPDATE room_state SET status = 'initializing', expires_at = ? WHERE singleton = 1",
        Date.now() - 1,
      );
      await instance.alarm();
    });
    expect((await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
      headers: { cookie: cookiePair(owner.cookie) },
    })).status).toBe(404);
    expect(await runInDurableObject(stub, async (_instance, state) => ({
      alarm: await state.storage.getAlarm(),
      rooms: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM room_state",
      ).toArray()[0]!.count,
    }))).toEqual({ alarm: null, rooms: 0 });
    rooms.delete(owner.room_id);
  });

  it("retries quota release faults without losing the durable cleanup owner", async () => {
    const owner = await createRoom("Ada");
    const room = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    const agentId = await runInDurableObject(room, (_instance, state) => (
      state.storage.sql.exec<{ agent_id: string }>(
        "SELECT agent_id FROM room_state WHERE singleton = 1",
      ).toArray()[0]!.agent_id
    ));
    const childDeletes: Array<{ agentId: string; status: number }> = [];
    let quotaReleaseAttempts = 0;
    let originalRoomEnv: Env | undefined;
    let originalQuota: Env["NANOCODEX_MULTIPLAYER_QUOTA"] | undefined;
    let originalSessions: Env["NANOCODEX_SESSIONS"] | undefined;

    const failed = await runInDurableObject(room, async (instance) => {
      const mutable = instance as unknown as {
        env: Env;
        fetch(request: Request): Promise<Response>;
      };
      originalRoomEnv = mutable.env;
      originalQuota = originalRoomEnv.NANOCODEX_MULTIPLAYER_QUOTA;
      originalSessions = originalRoomEnv.NANOCODEX_SESSIONS;
      const faultingQuota = {
        getByName(name: string) {
          const quota = originalQuota!.getByName(name);
          return {
            async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
              const request = new Request(input, init);
              if (request.method === "DELETE" && new URL(request.url).pathname.startsWith("/rooms/")) {
                quotaReleaseAttempts += 1;
                if (quotaReleaseAttempts === 1) {
                  return Response.json({ error: "injected_quota_failure" }, { status: 503 });
                }
                if (quotaReleaseAttempts === 2) {
                  throw new Error("injected quota transport failure");
                }
              }
              return quota.fetch(input, init);
            },
          } as DurableObjectStub;
        },
      } as Env["NANOCODEX_MULTIPLAYER_QUOTA"];
      const observingSessions = {
        idFromName(name: string) {
          return originalSessions!.idFromName(name);
        },
        getByName(name: string) {
          const session = originalSessions!.getByName(name);
          return {
            async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
              const request = new Request(input, init);
              const response = await session.fetch(input, init);
              if (request.method === "DELETE" && new URL(request.url).pathname === "/session") {
                childDeletes.push({ agentId: name, status: response.status });
              }
              return response;
            },
          } as DurableObjectStub;
        },
      } as Env["NANOCODEX_SESSIONS"];
      mutable.env = {
        ...originalRoomEnv,
        NANOCODEX_MULTIPLAYER_QUOTA: faultingQuota,
        NANOCODEX_SESSIONS: observingSessions,
      };
      return mutable.fetch(new Request("https://room.internal/admin", { method: "DELETE" }));
    });
    expect(failed.status).toBe(503);
    expect(await failed.json()).toEqual({ error: "agent_cleanup_pending" });
    expect(await roomCleanupState(room)).toMatchObject({ rooms: 1, status: "deleting" });
    expect((await roomCleanupState(room)).alarm).not.toBeNull();
    expect(await quotaLeaseCount(owner.room_id)).toBe(1);
    expect(await sessionStateCount(agentId)).toBe(0);

    expect(await runDurableObjectAlarm(room)).toBe(true);
    expect(await roomCleanupState(room)).toMatchObject({ rooms: 1, status: "deleting" });
    expect((await roomCleanupState(room)).alarm).not.toBeNull();
    expect(await quotaLeaseCount(owner.room_id)).toBe(1);

    expect(await runDurableObjectAlarm(room)).toBe(true);
    await runInDurableObject(room, (instance) => {
      (instance as unknown as { env: Env }).env = originalRoomEnv!;
    });
    expect(await roomCleanupState(room)).toEqual({ alarm: null, rooms: 0, status: null });
    expect(await quotaLeaseCount(owner.room_id)).toBe(0);
    expect(await sessionStateCount(agentId)).toBe(0);
    expect(quotaReleaseAttempts).toBe(3);
    expect(childDeletes).toEqual([{ agentId, status: 204 }]);
    rooms.delete(owner.room_id);
  });

  it("durably retries quota cleanup when initialization never owned a room", async () => {
    const { roomId } = testRoomIdentity();
    const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
    const reservation = await quota.fetch("https://quota.internal/rooms", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ room_id: roomId, expires_at: Date.now() + 60_000 }),
    });
    expect(reservation.status).toBe(201);
    await reservation.body?.cancel();

    const room = testEnv.NANOCODEX_ROOMS.getByName(roomId);
    let releaseAttempts = 0;
    const failed = await runInDurableObject(room, async (instance) => {
      const mutable = instance as unknown as { env: Env; fetch(request: Request): Promise<Response> };
      const originalRoomEnv = mutable.env;
      const originalQuota = originalRoomEnv.NANOCODEX_MULTIPLAYER_QUOTA;
      mutable.env = {
        ...originalRoomEnv,
        NANOCODEX_MULTIPLAYER_QUOTA: {
          getByName(name: string) {
            const target = originalQuota.getByName(name);
            return {
              async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
                const request = new Request(input, init);
                if (request.method === "DELETE") {
                  releaseAttempts += 1;
                  if (releaseAttempts === 1) {
                    return Response.json({ error: "injected_quota_failure" }, { status: 503 });
                  }
                }
                return target.fetch(input, init);
              },
            } as DurableObjectStub;
          },
        } as Env["NANOCODEX_MULTIPLAYER_QUOTA"],
      };
      return mutable.fetch(new Request(
        `https://room.internal/admin?room_id=${encodeURIComponent(roomId)}`,
        { method: "DELETE" },
      ));
    });
    expect(failed.status).toBe(503);
    expect(await failed.json()).toEqual({ error: "agent_cleanup_pending" });
    expect(await runInDurableObject(room, async (_instance, state) => ({
      alarm: await state.storage.getAlarm(),
      cleanup: await state.storage.get("nanocodex:room-quota-cleanup"),
      rooms: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM room_state",
      ).toArray()[0]!.count,
    }))).toMatchObject({ cleanup: { room_id: roomId }, rooms: 0 });
    expect((await runInDurableObject(
      room,
      async (_instance, state) => state.storage.getAlarm(),
    ))).not.toBeNull();
    expect(await quotaLeaseCount(roomId)).toBe(1);

    await evictDurableObject(room);
    expect(await runDurableObjectAlarm(room)).toBe(true);
    expect(await runInDurableObject(room, async (_instance, state) => ({
      alarm: await state.storage.getAlarm(),
      cleanup: await state.storage.get("nanocodex:room-quota-cleanup"),
    }))).toEqual({ alarm: null, cleanup: undefined });
    expect(await quotaLeaseCount(roomId)).toBe(0);
    expect(releaseAttempts).toBe(1);
  });

  it("atomically bounds parallel reusable-invite admission", async () => {
    const owner = await createRoom("Ada");
    const attempts = await Promise.all(Array.from({ length: 36 }, (_, index) => (
      joinResponse(owner, `Guest ${index}`)
    )));
    expect(attempts.filter((response) => response.status === 201)).toHaveLength(31);
    expect(attempts.filter((response) => response.status === 410)).toHaveLength(5);
    const memberIds = await Promise.all(attempts
      .filter((response) => response.status === 201)
      .map(async (response) => (await response.json<{ member_id: string }>()).member_id));
    expect(new Set(memberIds).size).toBe(31);
    await Promise.all(attempts
      .filter((response) => response.status !== 201)
      .map((response) => response.body?.cancel()));

    const exhausted = await joinResponse(owner, "Too Late");
    expect(exhausted.status).toBe(410);
    expect(await exhausted.json()).toEqual({ error: "invite_exhausted" });
  });

  it("preserves an authenticated owner who opens the room's own invite", async () => {
    const owner = await createRoom("Ada");
    const ownerJoinId = randomJoinId();
    const response = await joinResponse(owner, "Ada", {
      cookie: owner.cookie,
      joinId: ownerJoinId,
    });
    expect(response.status).toBe(200);
    expect((await response.json<{ member_id: string }>()).member_id).toBe(owner.member_id);
    expect(cookiePair(response.headers.get("set-cookie")!)).toBe(cookiePair(owner.cookie));

    const durable = await roomJoinState(owner.room_id);
    expect(durable).toEqual({ events: 1, members: 1, receipts: 0, redemptions: 0 });
    const connection = await connectWithReady(owner.websocket_url, response.headers.get("set-cookie")!);
    expect(connection.ready).toMatchObject({
      member_id: owner.member_id,
      can_target_agent: true,
      can_end_room: true,
    });
    connection.socket.close(1000, "owner preserved");

    const guestJoinId = randomJoinId();
    const guest = await joinResponse(owner, "Bob", { joinId: guestJoinId });
    expect(guest.status).toBe(201);
    await guest.body?.cancel();
    const staleGuestReceipt = await joinResponse(owner, "Ada", {
      cookie: owner.cookie,
      joinId: guestJoinId,
    });
    expect(staleGuestReceipt.status).toBe(200);
    expect((await staleGuestReceipt.json<{ member_id: string }>()).member_id).toBe(owner.member_id);
    expect(cookiePair(staleGuestReceipt.headers.get("set-cookie")!)).toBe(cookiePair(owner.cookie));
  });

  it("replays a lost guest join receipt after invite expiry and exhaustion", async () => {
    const owner = await createRoom("Ada");
    const joinId = randomJoinId();
    const first = await joinResponse(owner, "Bob", { joinId });
    expect(first.status).toBe(201);
    const firstCookie = first.headers.get("set-cookie")!;
    const firstReceipt = await first.json<{ member_id: string }>();
    const memberToken = cookiePair(firstCookie).split("=", 2)[1]!;
    expect(memberToken).toMatch(/^[A-Za-z0-9_-]{43}$/);

    const room = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    await runInDurableObject(room, (_instance, state) => {
      state.storage.sql.exec(
        "UPDATE room_invite_state SET redemptions = 31 WHERE singleton = 1",
      );
      const member = state.storage.sql.exec<{ token_hash: string }>(
        "SELECT token_hash FROM room_members WHERE id = ?",
        firstReceipt.member_id,
      ).toArray()[0]!;
      const receipt = state.storage.sql.exec<{ join_id_hash: string; request_hash: string }>(
        "SELECT join_id_hash, request_hash FROM room_join_receipts WHERE member_id = ?",
        firstReceipt.member_id,
      ).toArray()[0]!;
      expect(member.token_hash).toMatch(/^[0-9a-f]{64}$/);
      expect(member.token_hash).not.toBe(memberToken);
      expect(receipt.join_id_hash).toMatch(/^[0-9a-f]{64}$/);
      expect(receipt.request_hash).toMatch(/^[0-9a-f]{64}$/);
      expect([receipt.join_id_hash, receipt.request_hash]).not.toContain(joinId);
      expect([receipt.join_id_hash, receipt.request_hash]).not.toContain(owner.invite);
      expect([receipt.join_id_hash, receipt.request_hash]).not.toContain(memberToken);
      expect(state.storage.sql.exec<{ name: string }>(
        "PRAGMA table_info(room_join_receipts)",
      ).toArray().map((column) => column.name)).toEqual([
        "join_id_hash",
        "request_hash",
        "member_id",
      ]);
    });
    await evictDurableObject(room);

    const exhausted = await joinResponse(owner, "Too Late", { joinId: randomJoinId() });
    expect(exhausted.status).toBe(410);
    expect(await exhausted.json()).toEqual({ error: "invite_exhausted" });
    await runInDurableObject(room, (_instance, state) => {
      state.storage.sql.exec("UPDATE room_state SET invite_expires_at = 0 WHERE singleton = 1");
    });
    const expired = await joinResponse(owner, "Also Too Late", { joinId: randomJoinId() });
    expect(expired.status).toBe(410);
    expect(await expired.json()).toEqual({ error: "invite_expired" });

    const replay = await joinResponse(owner, "Bob", { joinId });
    expect(replay.status).toBe(200);
    expect(await replay.json()).toMatchObject({ member_id: firstReceipt.member_id });
    expect(cookiePair(replay.headers.get("set-cookie")!)).toBe(cookiePair(firstCookie));
    expect(await roomJoinState(owner.room_id)).toEqual({
      events: 2,
      members: 2,
      receipts: 1,
      redemptions: 31,
    });
  });

  it("conflicts when a join id is reused with a changed invite or display name", async () => {
    const owner = await createRoom("Ada");
    const joinId = randomJoinId();
    const joined = await joinResponse(owner, "Bob", { joinId });
    expect(joined.status).toBe(201);
    await joined.body?.cancel();

    for (const response of [
      await joinResponse(owner, "Grace", { joinId }),
      await joinResponse(owner, "Bob", { invite: "x".repeat(43), joinId }),
    ]) {
      expect(response.status).toBe(409);
      expect(await response.json()).toEqual({ error: "join_id_conflict" });
    }
    expect(await roomJoinState(owner.room_id)).toEqual({
      events: 2,
      members: 2,
      receipts: 1,
      redemptions: 1,
    });
  });

  it("converges concurrent copies of one join request to one durable admission", async () => {
    const owner = await createRoom("Ada");
    const joinId = randomJoinId();
    const responses = await Promise.all([
      joinResponse(owner, "Bob", { joinId }),
      joinResponse(owner, "Bob", { joinId }),
    ]);
    expect(responses.map((response) => response.status).sort()).toEqual([200, 201]);
    const cookies = responses.map((response) => cookiePair(response.headers.get("set-cookie")!));
    expect(cookies[0]).toBe(cookies[1]);
    const receipts = await Promise.all(responses.map((response) => (
      response.json<{ member_id: string }>()
    )));
    expect(receipts[0]!.member_id).toBe(receipts[1]!.member_id);
    expect(await roomJoinState(owner.room_id)).toEqual({
      events: 2,
      members: 2,
      receipts: 1,
      redemptions: 1,
    });
  });

  it("broadcasts one ordered durable chat to N players and survives eviction", async () => {
    const owner = await createRoom("Ada");
    const bob = await joinRoom(owner, "Bob");
    const grace = await joinRoom(owner, "Grace");
    const adaSocket = await connect(owner.websocket_url, owner.cookie);
    const bobSocket = await connect(bob.websocket_url, bob.cookie);
    const graceSocket = await connect(grace.websocket_url, grace.cookie);

    try {
      const sockets = [adaSocket, bobSocket, graceSocket];
      const command = { type: "say", id: "ada-1", text: "hello room", target: "room" };
      const observed = sockets.map((socket) => roomEvent(socket, "ada-1"));
      adaSocket.send(JSON.stringify(command));
      const events = await Promise.all(observed);
      expect(events.map((event) => event.cursor)).toEqual([
        events[0]!.cursor,
        events[0]!.cursor,
        events[0]!.cursor,
      ]);
      expect(events[0]?.event).toMatchObject({
        type: "member_message",
        id: "ada-1",
        text: "hello room",
        target: "room",
        member: { name: "Ada" },
      });

      adaSocket.send(JSON.stringify(command));
      expect(await nextWhere(adaSocket, (message) => message.type === "accepted" && message.id === "ada-1"))
        .toMatchObject({ cursor: events[0]!.cursor, replayed: true });
      adaSocket.send(JSON.stringify({ ...command, text: "different" }));
      expect(await nextWhere(adaSocket, (message) => message.type === "error"))
        .toMatchObject({ code: "message_id_conflict" });

      await evictDurableObject(testEnv.NANOCODEX_ROOMS.getByName(owner.room_id));
      const afterEviction = sockets.map((socket) => roomEvent(socket, "grace-after-eviction"));
      graceSocket.send(JSON.stringify({
        type: "say",
        id: "grace-after-eviction",
        text: "still here",
        target: "room",
      }));
      const restored = await Promise.all(afterEviction);
      expect(restored.every((event) => BigInt(event.cursor) > BigInt(events[0]!.cursor))).toBe(true);
      expect(restored.map((event) => event.cursor)).toEqual([
        restored[0]!.cursor,
        restored[0]!.cursor,
        restored[0]!.cursor,
      ]);
    } finally {
      adaSocket.close(1000, "done");
      bobSocket.close(1000, "done");
      graceSocket.close(1000, "done");
    }
  });

  it("serializes conflicting say commands in arrival order across hashing", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      const firstText = `first-${"x".repeat(12_000)}`;
      const committed = roomEvent(socket, "fifo-conflict");
      const accepted = nextWhere(
        socket,
        (message) => message.type === "accepted" && message.id === "fifo-conflict",
      );
      const conflicted = nextWhere(
        socket,
        (message) => message.type === "error" && message.code === "message_id_conflict",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: "fifo-conflict",
        text: firstText,
        target: "room",
      }));
      socket.send(JSON.stringify({
        type: "say",
        id: "fifo-conflict",
        text: "second",
        target: "room",
      }));
      expect(await accepted).toMatchObject({ replayed: false });
      expect((await committed).event.text).toBe(firstText);
      expect(await conflicted).toMatchObject({ code: "message_id_conflict" });

      const replayed = nextWhere(
        socket,
        (message) => message.type === "accepted" && message.id === "fifo-conflict",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: "fifo-conflict",
        text: firstText,
        target: "room",
      }));
      expect(await replayed).toMatchObject({ replayed: true });
    } finally {
      socket.close(1000, "done");
    }
  });

  it("catches restored hibernating sockets up from their durable attachment", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      for (let index = 0; index < 20; index += 1) {
        const id = `replay-${index}`;
        const accepted = nextWhere(
          socket,
          (message) => message.type === "accepted" && message.id === id,
        );
        const observed = roomEvent(socket, id);
        socket.send(JSON.stringify({ type: "say", id, text: id, target: "room" }));
        await Promise.all([accepted, observed]);
      }

      const replayedTail = roomEvent(socket, "replay-19");
      const replayPaused = nextWhere(
        socket,
        (message) => message.type === "replay_paused",
      );
      const stub = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
      await runInDurableObject(stub, (_instance, state) => {
        const restored = state.getWebSockets("member");
        expect(restored).toHaveLength(1);
        restored[0]!.serializeAttachment({
          memberId: owner.member_id,
          after: "0",
          replayPaused: false,
        });
      });
      await evictDurableObject(stub);
      socket.send(JSON.stringify({ type: "ping", nonce: "wake-restored-room" }));
      const fence = await replayPaused;
      expect(BigInt(String(fence.cursor))).toBeGreaterThan(0n);
      socket.send(JSON.stringify({ type: "ack", cursor: fence.cursor }));
      expect((await replayedTail).event).toMatchObject({
        type: "member_message",
        id: "replay-19",
      });
    } finally {
      socket.close(1000, "done");
    }
  });

  it("durably limits ordinary chat events across eviction", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      for (let index = 0; index < 30; index += 1) {
        const id = `chat-event-${index}`;
        const accepted = nextWhere(
          socket,
          (message) => message.type === "accepted" && message.id === id,
        );
        socket.send(JSON.stringify({ type: "say", id, text: id, target: "room" }));
        await accepted;
      }
      await evictDurableObject(testEnv.NANOCODEX_ROOMS.getByName(owner.room_id));
      const limited = nextWhere(
        socket,
        (message) => message.type === "error" && message.code === "chat_rate_limited",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: "chat-event-overflow",
        text: "one too many",
        target: "room",
      }));
      expect(await limited).toMatchObject({
        code: "chat_rate_limited",
        id: "chat-event-overflow",
      });
    } finally {
      socket.close(1000, "done");
    }
  });

  it("durably limits ordinary chat bytes", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      const fullMessage = "x".repeat(16 * 1024);
      for (let index = 0; index < 4; index += 1) {
        const id = `chat-bytes-${index}`;
        const accepted = nextWhere(
          socket,
          (message) => message.type === "accepted" && message.id === id,
        );
        socket.send(JSON.stringify({ type: "say", id, text: fullMessage, target: "room" }));
        await accepted;
      }
      await evictDurableObject(testEnv.NANOCODEX_ROOMS.getByName(owner.room_id));
      const limited = nextWhere(
        socket,
        (message) => message.type === "error" && message.code === "chat_rate_limited",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: "chat-bytes-overflow",
        text: "x",
        target: "room",
      }));
      expect(await limited).toMatchObject({ code: "chat_rate_limited" });
    } finally {
      socket.close(1000, "done");
    }
  });

  it("lets every member ask the agent while reserving destructive room ownership", async () => {
    const owner = await createRoom("Ada");
    const bob = await joinRoom(owner, "Bob");
    const ownerConnection = await connectWithReady(owner.websocket_url, owner.cookie);
    const bobConnection = await connectWithReady(bob.websocket_url, bob.cookie);
    expect(ownerConnection.ready.can_target_agent).toBe(true);
    expect(bobConnection.ready.can_target_agent).toBe(true);
    expect(ownerConnection.ready.can_end_room).toBe(true);
    expect(bobConnection.ready.can_end_room).toBe(false);
    const guestAccepted = nextWhere(
      bobConnection.socket,
      (message) => message.type === "accepted" && message.id === "guest-agent-attempt",
    );
    const guestMessage = roomEvent(ownerConnection.socket, "guest-agent-attempt");
    bobConnection.socket.send(JSON.stringify({
      type: "say",
      id: "guest-agent-attempt",
      text: "reply with ROOM_MEMBER_AGENT_OK",
      target: "agent",
    }));
    expect(await guestAccepted).toMatchObject({ replayed: false });
    const committed = await guestMessage;
    expect(committed.event).toMatchObject({
      type: "member_message",
      member: { id: bob.member_id, name: "Bob" },
      target: "agent",
    });
    expect((await agentReply(ownerConnection.socket, committed.cursor)).event).toMatchObject({
      type: "agent_message",
      reply_to: committed.cursor,
    });
    ownerConnection.socket.close(1000, "permission checked");
    bobConnection.socket.close(1000, "permission checked");

    const forbidden = await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
      method: "DELETE",
      headers: { cookie: cookiePair(bob.cookie) },
    });
    expect(forbidden.status).toBe(403);
    expect(await forbidden.json()).toEqual({ error: "owner_required" });

    const sockets: WebSocket[] = [];
    try {
      const admissions = await Promise.all(
        Array.from({ length: 8 }, () => upgradeRoom(owner.websocket_url, owner.cookie)),
      );
      expect(admissions.filter((response) => response.status === 101)).toHaveLength(4);
      expect(admissions.filter((response) => response.status === 429)).toHaveLength(4);
      for (const response of admissions) {
        if (response.status !== 101) continue;
        const socket = response.webSocket!;
        socket.accept();
        sockets.push(socket);
      }
      const overflow = await SELF.fetch(owner.websocket_url.replace("wss:", "https:"), {
        headers: {
          cookie: cookiePair(owner.cookie),
          origin: ORIGIN,
          upgrade: "websocket",
        },
      });
      expect(overflow.status).toBe(429);
      expect(await overflow.text()).toBe("Member connection limit reached");
    } finally {
      for (const socket of sockets) socket.close(1000, "done");
    }

    const deleted = await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
      method: "DELETE",
      headers: { cookie: cookiePair(owner.cookie) },
    });
    expect(deleted.status).toBe(204);
    rooms.delete(owner.room_id);
  });

  it("terminates every live owner tab with an explicit room-ended status", async () => {
    const owner = await createRoom("Ada");
    const [first, second] = await Promise.all([
      connectWithReady(owner.websocket_url, owner.cookie),
      connectWithReady(owner.websocket_url, owner.cookie),
    ]);
    try {
      const ended = [
        nextWhere(first.socket, (message) => message.type === "room_ended"),
        nextWhere(second.socket, (message) => message.type === "room_ended"),
      ];
      const closed = [nextClose(first.socket), nextClose(second.socket)];
      const deleted = await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
        method: "DELETE",
        headers: { cookie: cookiePair(owner.cookie) },
      });
      expect(deleted.status).toBe(204);
      expect(await Promise.all(ended)).toEqual([
        { type: "room_ended" },
        { type: "room_ended" },
      ]);
      for (const event of await Promise.all(closed)) {
        expect(event.code).toBe(ROOM_ENDED_CLOSE_CODE);
        expect(event.reason).toBe("room ended");
      }
      rooms.delete(owner.room_id);
    } finally {
      first.socket.close(1000, "done");
      second.socket.close(1000, "done");
    }
  });

  it("durably meters operator-funded agent turns per member", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      for (let index = 0; index < 6; index += 1) {
        const id = `metered-${index}`;
        const accepted = nextWhere(
          socket,
          (message) => message.type === "accepted" && message.id === id,
        );
        socket.send(JSON.stringify({ type: "say", id, text: `turn ${index}`, target: "agent" }));
        expect(await accepted).toMatchObject({ id, replayed: false });
      }
      const limited = nextWhere(
        socket,
        (message) => message.type === "error" && message.code === "agent_rate_limited",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: "metered-overflow",
        text: "one too many",
        target: "agent",
      }));
      expect(await limited).toMatchObject({
        type: "error",
        code: "agent_rate_limited",
      });
    } finally {
      socket.close(1000, "done");
    }
  }, 15_000);

  it("commits local agent intent before consuming the global turn quota", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    const clientId = "full-log-agent-intent";
    const stub = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    try {
      await runInDurableObject(stub, (_instance, state) => {
        state.storage.sql.exec(
          "UPDATE managed_event_meta SET total_bytes = ? WHERE singleton = 1",
          64 * 1024 * 1024,
        );
      });
      const rejected = nextWhere(
        socket,
        (message) => message.type === "error" && message.code === "event_log_full",
      );
      socket.send(JSON.stringify({
        type: "say",
        id: clientId,
        text: "must fail before global admission",
        target: "agent",
      }));
      expect(await rejected).toMatchObject({ code: "event_log_full" });
      expect(await runInDurableObject(stub, (_instance, state) => ({
        jobs: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM room_agent_jobs",
        ).toArray()[0]!.count,
        keys: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM room_message_keys",
        ).toArray()[0]!.count,
        limits: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM room_agent_rate_limits",
        ).toArray()[0]!.count,
      }))).toEqual({ jobs: 0, keys: 0, limits: 0 });

      const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
      const admission = await quota.fetch("https://quota.internal/agent-turn", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          room_id: owner.room_id,
          request_id: `${owner.room_id}:${owner.member_id}:${clientId}`,
        }),
      });
      expect(admission.status).toBe(201);
      await admission.body?.cancel();
    } finally {
      socket.close(1000, "done");
    }
  });

  it("records a definitive global quota denial without fencing the room FIFO", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    const room = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    const quota = testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
    const quotaWindow = Math.floor(Date.now() / (60 * 60_000)) * 60 * 60_000;
    try {
      await runInDurableObject(quota, (_instance, state) => {
        state.storage.sql.exec(
          `INSERT INTO multiplayer_quota_counters (scope, window_start, count)
           VALUES ('agent-turns', ?, 240)
           ON CONFLICT(scope) DO UPDATE SET window_start = excluded.window_start, count = excluded.count`,
          quotaWindow,
        );
      });

      const limitedSource = roomEvent(socket, "globally-limited");
      const limitedTerminal = nextWhere(socket, (message) => {
        const event = message.event as Record<string, unknown> | undefined;
        return message.type === "room_event"
          && event?.type === "agent_error"
          && event.code === "rate_limited";
      });
      socket.send(JSON.stringify({
        type: "say",
        id: "globally-limited",
        text: "the global budget is exhausted",
        target: "agent",
      }));
      const source = await limitedSource;
      expect(await limitedTerminal).toMatchObject({
        event: { type: "agent_error", code: "rate_limited", reply_to: source.cursor },
      });
      expect(await runInDurableObject(room, (_instance, state) => (
        state.storage.sql.exec<{ state: string }>(
          "SELECT state FROM room_agent_jobs WHERE source_cursor = CAST(? AS INTEGER)",
          source.cursor,
        ).toArray()[0]!.state
      ))).toBe("completed");

      await runInDurableObject(quota, (_instance, state) => {
        state.storage.sql.exec(
          "DELETE FROM multiplayer_quota_counters WHERE scope = 'agent-turns'",
        );
      });
      const recoveredSource = roomEvent(socket, "after-global-reset");
      socket.send(JSON.stringify({
        type: "say",
        id: "after-global-reset",
        text: "the global budget is available again",
        target: "agent",
      }));
      const recovered = await recoveredSource;
      expect((await agentReply(socket, recovered.cursor)).event).toMatchObject({
        type: "agent_message",
        reply_to: recovered.cursor,
      });
    } finally {
      await runInDurableObject(quota, (_instance, state) => {
        state.storage.sql.exec(
          "DELETE FROM multiplayer_agent_admissions WHERE room_id = ?",
          owner.room_id,
        );
        state.storage.sql.exec(
          "DELETE FROM multiplayer_quota_counters WHERE scope = 'agent-turns'",
        );
      });
      socket.close(1000, "done");
    }
  });

  it("projects a durable blocked event before fencing the agent FIFO", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    const stub = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
    try {
      const sourceCursor = await runInDurableObject(stub, async (_instance, state) => {
        const messageJson = JSON.stringify({
          type: "member_message",
          id: "expired-agent-job",
          member: { id: owner.member_id, name: "Ada" },
          text: "this accepted job is too old to execute",
          target: "agent",
        });
        const cursor = state.storage.transactionSync(() => {
          const inserted = state.storage.sql.exec<{ cursor: string }>(
            `INSERT INTO managed_events (turn_id, message_json, created_at)
             VALUES (NULL, ?, ?)
             RETURNING CAST(cursor AS TEXT) AS cursor`,
            messageJson,
            Date.now(),
          ).toArray()[0]!.cursor;
          state.storage.sql.exec(
            "UPDATE managed_event_meta SET total_bytes = total_bytes + ? WHERE singleton = 1",
            new TextEncoder().encode(messageJson).byteLength,
          );
          state.storage.sql.exec(
            `INSERT INTO room_agent_jobs (
               source_cursor, turn_id, state, attempts, created_at, updated_at
             ) VALUES (CAST(? AS INTEGER), ?, 'pending', 0, ?, ?)`,
            inserted,
            `room-${inserted}`,
            Date.now() - 11 * 60_000,
            Date.now(),
          );
          return inserted;
        });
        await state.storage.setAlarm(Date.now() + 1);
        return cursor;
      });
      const blocked = nextWhere(socket, (message) => {
        const event = message.event as Record<string, unknown> | undefined;
        return message.type === "room_event"
          && event?.type === "agent_error"
          && event.reply_to === sourceCursor;
      });
      await runDurableObjectAlarm(stub);
      expect(await blocked).toMatchObject({
        event: { type: "agent_error", code: "blocked", reply_to: sourceCursor },
      });
      expect(await runInDurableObject(stub, (_instance, state) => ({
        state: state.storage.sql.exec<{ state: string }>(
          "SELECT state FROM room_agent_jobs WHERE source_cursor = CAST(? AS INTEGER)",
          sourceCursor,
        ).toArray()[0]!.state,
        terminals: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_events WHERE turn_id = ?",
          `room-${sourceCursor}`,
        ).toArray()[0]!.count,
      }))).toEqual({ state: "blocked", terminals: 1 });
    } finally {
      socket.close(1000, "done");
    }
  });

  it("recovers an accepted agent job after eviction and duplicate replay", async () => {
    const owner = await createRoom("Ada");
    const socket = await connect(owner.websocket_url, owner.cookie);
    try {
      const command = {
        type: "say",
        id: "accepted-before-eviction",
        text: "reply after room recovery",
        target: "agent",
      };
      const accepted = nextWhere(
        socket,
        (message) => message.type === "accepted" && message.id === command.id,
      );
      const source = roomEvent(socket, command.id);
      socket.send(JSON.stringify(command));
      const receipt = await accepted;
      const human = await source;
      expect(receipt).toMatchObject({ cursor: human.cursor, replayed: false });
      expect((await agentReply(socket, human.cursor)).event).toMatchObject({
        type: "agent_message",
        reply_to: human.cursor,
      });

      // Recreate the crash boundary after the child committed its typed
      // terminal but before the room projected it. The outbox row and event-log
      // byte accounting are rewound atomically; the managed child is untouched.
      const stub = testEnv.NANOCODEX_ROOMS.getByName(owner.room_id);
      await runInDurableObject(stub, async (_instance, state) => {
        const turnId = `room-${human.cursor}`;
        state.storage.transactionSync(() => {
          const projected = state.storage.sql.exec<{ bytes: number }>(
            `SELECT LENGTH(CAST(message_json AS BLOB)) AS bytes
             FROM managed_events WHERE turn_id = ?`,
            turnId,
          ).toArray()[0];
          expect(projected).toBeTruthy();
          state.storage.sql.exec(
            "UPDATE managed_event_meta SET total_bytes = total_bytes - ? WHERE singleton = 1",
            projected!.bytes,
          );
          state.storage.sql.exec("DELETE FROM managed_events WHERE turn_id = ?", turnId);
          state.storage.sql.exec(
            `UPDATE room_agent_jobs SET state = 'submitted', updated_at = ?
             WHERE turn_id = ?`,
            Date.now(),
            turnId,
          );
          const server = state.getWebSockets("member")[0];
          expect(server).toBeTruthy();
          server!.serializeAttachment({ memberId: owner.member_id, after: human.cursor });
        });
        await state.storage.setAlarm(Date.now() + 1);
      });

      const reply = agentReply(socket, human.cursor);
      await evictDurableObject(stub);
      const duplicate = nextWhere(
        socket,
        (message) => message.type === "accepted" && message.id === command.id,
      );
      socket.send(JSON.stringify(command));
      expect(await duplicate).toMatchObject({ cursor: human.cursor, replayed: true });
      expect((await reply).event).toMatchObject({
        type: "agent_message",
        reply_to: human.cursor,
      });
    } finally {
      socket.close(1000, "done");
    }
  }, 15_000);

  it("keeps Just Bash available while connector calls fail closed for the whole room", async () => {
    const owner = await createRoom("Ada");
    const bob = await joinRoom(owner, "Bob");
    const adaSocket = await connect(owner.websocket_url, owner.cookie);
    const bobSocket = await connect(bob.websocket_url, bob.cookie);
    try {
      const humanMessage = roomEvent(adaSocket, "ask-agent");
      const bobReplyPending = agentReply(bobSocket);
      adaSocket.send(JSON.stringify({
        type: "say",
        id: "ask-agent",
        text: "E2E_MULTIPLAYER_NO_CONNECTORS",
        target: "agent",
      }));
      const human = await humanMessage;
      const replyAda = await agentReply(adaSocket, human.cursor);
      const replyBob = await bobReplyPending;
      expect(replyAda.cursor).toBe(replyBob.cursor);
      expect(replyBob.event.reply_to).toBe(human.cursor);
      expect(replyAda.event).toMatchObject({
        type: "agent_message",
        reply_to: human.cursor,
      });
      const encoded = JSON.stringify(replyAda);
      expect(encoded).toContain("MULTIPLAYER_CONNECTORS_BLOCKED");
      expect(encoded).not.toContain("MULTIPLAYER_CONNECTORS_EXPOSED");
      expect(encoded).not.toContain("test-openai-key");
      expect(encoded).not.toContain("NANOCODEX_OPENAI_API_KEY");
      expect(encoded).not.toContain("agent_id");

      const deleted = await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
        method: "DELETE",
        headers: admin,
      });
      expect(deleted.status).toBe(204);
      rooms.delete(owner.room_id);
      expect((await SELF.fetch(`${ORIGIN}/v1/rooms/${owner.room_id}`, {
        headers: { cookie: cookiePair(owner.cookie) },
      })).status).toBe(404);
    } finally {
      adaSocket.close(1000, "done");
      bobSocket.close(1000, "done");
    }
  }, 15_000);
});

type RoomReceipt = {
  room_id: string;
  member_id: string;
  invite: string;
  invite_url: string;
  websocket_url: string;
  cookie: string;
};

type MemberReceipt = {
  member_id: string;
  websocket_url: string;
  cookie: string;
};

async function roomCleanupState(room: DurableObjectStub): Promise<{
  alarm: number | null;
  rooms: number;
  status: string | null;
}> {
  return runInDurableObject(room, async (_instance, state) => {
    const row = state.storage.sql.exec<{ count: number; status: string | null }>(
      `SELECT COUNT(*) AS count, MAX(status) AS status
       FROM room_state`,
    ).toArray()[0]!;
    return {
      alarm: await state.storage.getAlarm(),
      rooms: row.count,
      status: row.status,
    };
  });
}

async function quotaLeaseCount(roomId: string): Promise<number> {
  return runInDurableObject(
    testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global"),
    (_instance, state) => state.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM multiplayer_rooms WHERE room_id = ?",
      roomId,
    ).toArray()[0]!.count,
  );
}

async function sessionStateCount(agentId: string): Promise<number> {
  return runInDurableObject(
    testEnv.NANOCODEX_SESSIONS.getByName(agentId),
    (_instance, state) => state.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM session_state",
    ).toArray()[0]!.count,
  );
}

async function sessionInitializationState(agentId: string): Promise<string | undefined> {
  return runInDurableObject(
    testEnv.NANOCODEX_SESSIONS.getByName(agentId),
    (_instance, state) => state.storage.sql.exec<{ state: string }>(
      "SELECT state FROM session_initialization_ownership WHERE singleton = 1",
    ).toArray()[0]?.state,
  );
}

function testRoomIdentity(): { roomId: string; agentId: string } {
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const agentId = `0198d214-0d9d-7a45-8a89-${suffix}`;
  return { agentId, roomId: `${agentId}~${"A".repeat(43)}` };
}

async function createRoom(
  displayName: string,
  createId = randomCreateId(),
): Promise<RoomReceipt> {
  const response = await createRoomResponse(displayName, createId);
  expect(response.status).toBe(201);
  const cookie = response.headers.get("set-cookie");
  expect(cookie).toBeTruthy();
  const receipt = await response.json<Omit<RoomReceipt, "cookie">>();
  rooms.add(receipt.room_id);
  return { ...receipt, cookie: cookie! };
}

function createRoomResponse(displayName: string, createId: string): Promise<Response> {
  return SELF.fetch(`${ORIGIN}/v1/rooms`, {
    method: "POST",
    headers: { ...allocator, "content-type": "application/json" },
    body: JSON.stringify({ create_id: createId, display_name: displayName }),
  });
}

async function seedApiKey(userId: string, token: string): Promise<void> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const digest = btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const account = await testEnv.NANOCODEX_USERS.getByName(userId).fetch(
    "https://user.internal/account",
    {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: userId, persistent: true }),
    },
  );
  expect(account.ok).toBe(true);
  const accountRecord = await account.json<{ organizationId: string }>();
  const organization = await testEnv.NANOCODEX_ORGANIZATIONS.getByName(
    accountRecord.organizationId,
  ).fetch("https://organization.internal/metadata");
  expect(organization.ok).toBe(true);
  const metadata = await organization.json<{ rootTeam: { id: string } }>();
  const key = testEnv.NANOCODEX_API_KEYS.getByName(digest);
  await key.fetch("https://api-key.internal/record", { method: "DELETE" });
  const record = await key.fetch(
    "https://api-key.internal/record",
    {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        id: "r".repeat(12),
        label: "room test",
        prefix: `ncx_live_${"r".repeat(12)}`,
        createdAt: Date.now(),
        digest,
        userId,
        organizationId: accountRecord.organizationId,
        teamId: metadata.rootTeam.id,
        role: "owner",
        authorizationEpoch: 1,
        capabilities: [
          "agents:read",
          "agents:write",
          "api_keys:read",
          "api_keys:write",
          "history:read",
          "memory:read",
          "memory:write",
          "tools:use",
          "organization:read",
          "organization:write",
        ],
      }),
    },
  );
  expect(record.status).toBe(201);
}

async function joinRoom(room: RoomReceipt, displayName: string): Promise<MemberReceipt> {
  const response = await joinResponse(room, displayName);
  expect(response.status).toBe(201);
  const cookie = response.headers.get("set-cookie");
  expect(cookie).toBeTruthy();
  return { ...(await response.json<Omit<MemberReceipt, "cookie">>()), cookie: cookie! };
}

async function joinResponse(
  room: RoomReceipt,
  displayName: string,
  options: { cookie?: string; invite?: string; joinId?: string } = {},
): Promise<Response> {
  const headers = new Headers({ "content-type": "application/json" });
  if (options.cookie) headers.set("cookie", cookiePair(options.cookie));
  return SELF.fetch(`${ORIGIN}/v1/rooms/${room.room_id}/join`, {
    method: "POST",
    headers,
    body: JSON.stringify({
      invite: options.invite ?? room.invite,
      display_name: displayName,
      join_id: options.joinId ?? randomJoinId(),
    }),
  });
}

async function roomJoinState(roomId: string): Promise<{
  events: number;
  members: number;
  receipts: number;
  redemptions: number;
}> {
  return runInDurableObject(
    testEnv.NANOCODEX_ROOMS.getByName(roomId),
    (_instance, state) => ({
      events: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_events",
      ).toArray()[0]!.count,
      members: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM room_members",
      ).toArray()[0]!.count,
      receipts: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM room_join_receipts",
      ).toArray()[0]!.count,
      redemptions: state.storage.sql.exec<{ redemptions: number }>(
        "SELECT redemptions FROM room_invite_state WHERE singleton = 1",
      ).toArray()[0]!.redemptions,
    }),
  );
}

function randomJoinId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function randomCreateId(): string {
  return randomJoinId();
}

function randomHash(): string {
  return [...crypto.getRandomValues(new Uint8Array(32))]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

async function deleteTestRoom(roomId: string): Promise<void> {
  const deleted = await SELF.fetch(`${ORIGIN}/v1/rooms/${roomId}`, {
    method: "DELETE",
    headers: admin,
  });
  expect(deleted.status).toBe(204);
  rooms.delete(roomId);
  await runInDurableObject(
    testEnv.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global"),
    (_instance, state) => {
      state.storage.sql.exec(
        "DELETE FROM multiplayer_room_reservations WHERE room_id = ?",
        roomId,
      );
      state.storage.sql.exec(
        `UPDATE multiplayer_quota_counters
         SET count = MAX(0, count - 1)
         WHERE scope = 'room-creations'`,
      );
    },
  );
}

function noncooperativeBody(): ReadableStream<Uint8Array> {
  return new ReadableStream({
    cancel() { return new Promise<void>(() => {}); },
  });
}

async function within<Result>(promise: Promise<Result>, operation: string): Promise<Result> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        // The application deadline is configured independently (20 ms in the
        // bounded-I/O tests). Leave enough harness headroom for a saturated CI
        // worker to schedule that deadline and return its 503 response.
        timer = setTimeout(() => reject(new Error(`${operation} test timed out`)), 5_000);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

async function connect(websocketUrl: string, cookie: string): Promise<WebSocket> {
  return (await connectWithReady(websocketUrl, cookie)).socket;
}

async function connectWithReady(websocketUrl: string, cookie: string): Promise<{
  socket: WebSocket;
  ready: Record<string, unknown>;
}> {
  const response = await upgradeRoom(websocketUrl, cookie);
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  socket.accept();
  const ready = await nextWhere(socket, (message) => message.type === "ready");
  return { socket, ready };
}

async function upgradeRoom(websocketUrl: string, cookie: string): Promise<Response> {
  return SELF.fetch(websocketUrl.replace("wss:", "https:"), {
    headers: {
      cookie: cookiePair(cookie),
      origin: ORIGIN,
      upgrade: "websocket",
    },
  });
}

function cookiePair(setCookie: string): string {
  return setCookie.split(";", 1)[0]!;
}

async function roomEvent(socket: WebSocket, id: string): Promise<{
  cursor: string;
  event: Record<string, unknown>;
}> {
  return nextWhere(socket, (message) => {
    const event = message.event as Record<string, unknown> | undefined;
    return message.type === "room_event" && event?.id === id;
  }) as Promise<{ cursor: string; event: Record<string, unknown> }>;
}

async function agentReply(socket: WebSocket, replyTo?: string): Promise<{
  cursor: string;
  event: Record<string, unknown>;
}> {
  return nextWhere(socket, (message) => {
    const event = message.event as Record<string, unknown> | undefined;
    return message.type === "room_event"
      && event?.type === "agent_message"
      && (replyTo === undefined || event.reply_to === replyTo);
  }, 12_000) as Promise<{ cursor: string; event: Record<string, unknown> }>;
}

function nextWhere(
  socket: WebSocket,
  predicate: (message: Record<string, unknown>) => boolean,
  timeoutMs = 3_000,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      socket.removeEventListener("message", onMessage);
      reject(new Error("timed out waiting for room message"));
    }, timeoutMs);
    const onMessage = (event: MessageEvent) => {
      const message = JSON.parse(String(event.data)) as Record<string, unknown>;
      if (!predicate(message)) return;
      clearTimeout(timeout);
      socket.removeEventListener("message", onMessage);
      resolve(message);
    };
    socket.addEventListener("message", onMessage);
  });
}

function nextClose(socket: WebSocket, timeoutMs = 3_000): Promise<CloseEvent> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      socket.removeEventListener("close", onClose);
      reject(new Error("timed out waiting for room socket close"));
    }, timeoutMs);
    const onClose = (event: CloseEvent) => {
      clearTimeout(timeout);
      socket.removeEventListener("close", onClose);
      resolve(event);
    };
    socket.addEventListener("close", onClose);
  });
}
