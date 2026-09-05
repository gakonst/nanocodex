import { DurableObject } from "cloudflare:workers";

const ROOM_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}~[A-Za-z0-9_-]{43}$/;
const REQUEST_ID = /^[A-Za-z0-9._:~-]{1,256}$/;
const HASH = /^[0-9a-f]{64}$/;
const HOUR_MS = 60 * 60_000;
export const MULTIPLAYER_ROOM_LEASE_MS = 2 * HOUR_MS;
const MAX_ACTIVE_ROOMS = 16;
const MAX_ROOM_CREATIONS_PER_HOUR = 32;
const MAX_AGENT_TURNS_PER_HOUR = 240;

type CounterRow = { count: number; window_start: number };
type RoomReservationRow = {
  create_id_hash: string;
  request_hash: string;
  room_id: string;
  created_at: number;
  expires_at: number;
};
type MultiplayerQuotaEnv = Record<string, never>;

/**
 * One deployment-wide Durable Object owns the hard public-demo budget. Edge
 * Rate Limit bindings remain the cheap abuse filter; this object is the
 * authoritative cross-location ceiling for room allocation and model turns.
 */
export class MultiplayerQuota extends DurableObject<MultiplayerQuotaEnv> {
  #roomLifecycleTail = Promise.resolve();

  constructor(ctx: DurableObjectState, env: MultiplayerQuotaEnv) {
    super(ctx, env);
    this.ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS multiplayer_rooms (
        room_id TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS multiplayer_quota_counters (
        scope TEXT PRIMARY KEY,
        window_start INTEGER NOT NULL,
        count INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS multiplayer_agent_admissions (
        request_id TEXT PRIMARY KEY,
        room_id TEXT NOT NULL,
        admitted_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS multiplayer_room_reservations (
        create_id_hash TEXT PRIMARY KEY,
        request_hash TEXT NOT NULL,
        room_id TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
      );
    `);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === "/rooms") {
      return this.#queueRoomLifecycle(() => this.#reserveRoom(request));
    }
    const roomMatch = url.pathname.match(/^\/rooms\/(.+)$/);
    if (request.method === "DELETE" && roomMatch) {
      const roomId = decodeURIComponent(roomMatch[1]!);
      if (!ROOM_ID.test(roomId)) return json({ error: "invalid_room" }, 400);
      return this.#queueRoomLifecycle(() => {
        this.#prune(Date.now());
        this.ctx.storage.sql.exec(
          "DELETE FROM multiplayer_room_reservations WHERE room_id = ?",
          roomId,
        );
        this.ctx.storage.sql.exec("DELETE FROM multiplayer_rooms WHERE room_id = ?", roomId);
        return new Response(null, { status: 204 });
      });
    }
    if (request.method === "POST" && url.pathname === "/agent-turn") {
      return this.#admitAgentTurn(request);
    }
    if (request.method === "GET" && url.pathname === "/status") {
      const now = Date.now();
      this.#prune(now);
      return json({
        active_rooms: this.#activeRooms(),
        agent_turns_this_hour: this.#counter("agent-turns", hourWindow(now)).count,
        limits: {
          active_rooms: MAX_ACTIVE_ROOMS,
          room_creations_per_hour: MAX_ROOM_CREATIONS_PER_HOUR,
          agent_turns_per_hour: MAX_AGENT_TURNS_PER_HOUR,
        },
      });
    }
    return json({ error: "not_found" }, 404);
  }

  async #reserveRoom(request: Request): Promise<Response> {
    const value = await boundedJson(request);
    if (value instanceof Response) return value;
    const roomId = value.room_id;
    const expiresAt = value.expires_at;
    const createIdHash = value.create_id_hash;
    const requestHash = value.request_hash;
    const creationReservation = [createIdHash, requestHash].some((field) => field !== undefined);
    const now = Date.now();
    if (typeof roomId !== "string" || !ROOM_ID.test(roomId)
      || !Number.isSafeInteger(expiresAt)
      || (expiresAt as number) <= now
      || (expiresAt as number) > now + MULTIPLAYER_ROOM_LEASE_MS
      || (creationReservation && (
        typeof createIdHash !== "string" || !HASH.test(createIdHash)
        || typeof requestHash !== "string" || !HASH.test(requestHash)
      ))
      || Object.keys(value).some((key) => ![
        "room_id",
        "expires_at",
        "create_id_hash",
        "request_hash",
      ].includes(key))) {
      return json({ error: "invalid_room_reservation" }, 400);
    }

    let status = 201;
    let limited: "active_rooms" | "room_creations" | undefined;
    let conflict = false;
    let reservedExpiresAt = expiresAt as number;
    this.ctx.storage.transactionSync(() => {
      this.#prune(now);
      if (creationReservation) {
        const existingCreation = this.#creation(createIdHash as string);
        if (existingCreation) {
          if (existingCreation.request_hash !== requestHash
            || existingCreation.room_id !== roomId) {
            conflict = true;
            return;
          }
          status = 200;
          reservedExpiresAt = existingCreation.expires_at;
          return;
        }
      }
      const existing = this.ctx.storage.sql.exec<{ expires_at: number }>(
        "SELECT expires_at FROM multiplayer_rooms WHERE room_id = ?",
        roomId,
      ).toArray()[0];
      if (existing) {
        if (creationReservation) {
          conflict = true;
          return;
        }
        status = 200;
        reservedExpiresAt = existing.expires_at;
        return;
      }
      if (this.#activeRooms() >= MAX_ACTIVE_ROOMS) {
        limited = "active_rooms";
        return;
      }
      const window = hourWindow(now);
      const creations = this.#counter("room-creations", window);
      if (creations.count >= MAX_ROOM_CREATIONS_PER_HOUR) {
        limited = "room_creations";
        return;
      }
      this.#increment("room-creations", window);
      this.ctx.storage.sql.exec(
        "INSERT INTO multiplayer_rooms (room_id, created_at, expires_at) VALUES (?, ?, ?)",
        roomId,
        now,
        expiresAt,
      );
      if (creationReservation) {
        this.ctx.storage.sql.exec(
          `INSERT INTO multiplayer_room_reservations (
             create_id_hash, request_hash, room_id, created_at, expires_at
           ) VALUES (?, ?, ?, ?, ?)`,
          createIdHash as string,
          requestHash as string,
          roomId,
          now,
          expiresAt,
        );
      }
    });
    if (conflict) return json({ error: "create_id_conflict" }, 409);
    if (limited) return limitedResponse(limited, now);
    return json({
      room_id: roomId,
      expires_at: reservedExpiresAt,
      replayed: status === 200,
    }, status);
  }

  #queueRoomLifecycle<T>(operation: () => T | Promise<T>): Promise<T> {
    const task = this.#roomLifecycleTail.then(operation);
    this.#roomLifecycleTail = task.then(() => undefined, () => undefined);
    return task;
  }

  async #admitAgentTurn(request: Request): Promise<Response> {
    const value = await boundedJson(request);
    if (value instanceof Response) return value;
    const roomId = value.room_id;
    const requestId = value.request_id;
    if (typeof roomId !== "string" || !ROOM_ID.test(roomId)
      || typeof requestId !== "string" || !REQUEST_ID.test(requestId)) {
      return json({ error: "invalid_agent_admission" }, 400);
    }
    const now = Date.now();
    let status = 201;
    let unavailable = false;
    let limited = false;
    this.ctx.storage.transactionSync(() => {
      this.#prune(now);
      const existing = this.ctx.storage.sql.exec<{ room_id: string }>(
        "SELECT room_id FROM multiplayer_agent_admissions WHERE request_id = ?",
        requestId,
      ).toArray()[0];
      if (existing) {
        if (existing.room_id !== roomId) unavailable = true;
        else status = 200;
        return;
      }
      const room = this.ctx.storage.sql.exec<{ room_id: string }>(
        "SELECT room_id FROM multiplayer_rooms WHERE room_id = ? AND expires_at > ?",
        roomId,
        now,
      ).toArray()[0];
      if (!room) {
        unavailable = true;
        return;
      }
      const window = hourWindow(now);
      if (this.#counter("agent-turns", window).count >= MAX_AGENT_TURNS_PER_HOUR) {
        limited = true;
        return;
      }
      this.#increment("agent-turns", window);
      this.ctx.storage.sql.exec(
        "INSERT INTO multiplayer_agent_admissions (request_id, room_id, admitted_at) VALUES (?, ?, ?)",
        requestId,
        roomId,
        now,
      );
    });
    if (unavailable) return json({ error: "room_not_reserved" }, 409);
    if (limited) return limitedResponse("agent_turns", now);
    return json({ admitted: true, replayed: status === 200 }, status);
  }

  #prune(now: number): void {
    this.ctx.storage.sql.exec("DELETE FROM multiplayer_rooms WHERE expires_at <= ?", now);
    this.ctx.storage.sql.exec(
      "DELETE FROM multiplayer_room_reservations WHERE expires_at <= ?",
      now,
    );
    this.ctx.storage.sql.exec(
      "DELETE FROM multiplayer_agent_admissions WHERE admitted_at < ?",
      now - MULTIPLAYER_ROOM_LEASE_MS,
    );
    this.ctx.storage.sql.exec(
      "DELETE FROM multiplayer_quota_counters WHERE window_start < ?",
      hourWindow(now) - HOUR_MS,
    );
  }

  #activeRooms(): number {
    return this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM multiplayer_rooms",
    ).toArray()[0]?.count ?? 0;
  }

  #creation(createIdHash: string): RoomReservationRow | undefined {
    return this.ctx.storage.sql.exec<RoomReservationRow>(
      `SELECT create_id_hash, request_hash, room_id, created_at, expires_at
       FROM multiplayer_room_reservations WHERE create_id_hash = ?`,
      createIdHash,
    ).toArray()[0];
  }

  #counter(scope: string, windowStart: number): CounterRow {
    return this.ctx.storage.sql.exec<CounterRow>(
      "SELECT window_start, count FROM multiplayer_quota_counters WHERE scope = ? AND window_start = ?",
      scope,
      windowStart,
    ).toArray()[0] ?? { count: 0, window_start: windowStart };
  }

  #increment(scope: string, windowStart: number): void {
    this.ctx.storage.sql.exec(
      `INSERT INTO multiplayer_quota_counters (scope, window_start, count)
       VALUES (?, ?, 1)
       ON CONFLICT(scope) DO UPDATE SET
         window_start = excluded.window_start,
         count = CASE
           WHEN multiplayer_quota_counters.window_start = excluded.window_start
           THEN multiplayer_quota_counters.count + 1
           ELSE 1
         END`,
      scope,
      windowStart,
    );
  }
}

async function boundedJson(request: Request): Promise<Record<string, unknown> | Response> {
  const declared = Number(request.headers.get("content-length") ?? 0);
  if (Number.isFinite(declared) && declared > 4_096) return json({ error: "request_too_large" }, 413);
  const encoded = await request.text();
  if (new TextEncoder().encode(encoded).byteLength > 4_096) return json({ error: "request_too_large" }, 413);
  try {
    const value = JSON.parse(encoded) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid");
    return value as Record<string, unknown>;
  } catch {
    return json({ error: "invalid_json" }, 400);
  }
}

function hourWindow(now: number): number {
  return Math.floor(now / HOUR_MS) * HOUR_MS;
}

function limitedResponse(scope: string, now: number): Response {
  const retryAfter = Math.max(1, Math.ceil((hourWindow(now) + HOUR_MS - now) / 1_000));
  return json({ error: "multiplayer_capacity_reached", scope, retry_after: retryAfter }, 429, {
    "retry-after": String(retryAfter),
  });
}

function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", ...Object.fromEntries(new Headers(headers)) },
  });
}
