import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";

const EVENT_COUNT = 160;
const EVENT_TEXT_BYTES = 64 * 1024;
const WINDOW_BYTES = 2 * 1024 * 1024;

type Connection = { cursors: string[]; bytes: number; code: number | undefined };

it("a long thread's reconnecting socket replays bounded windows from its cursor without reconstructing the agent", { timeout: 60_000 }, async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
    await session.fetch(new Request("https://session.internal/state"));
    // A restored long thread: 233 accepted turns and about 10 MB of history.
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile,
       accepted_turns, completed_turns, last_active)
      VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', 233, 233, ?)`,
    crypto.randomUUID(), owner, organization, team, Date.now());
    for (let index = 0; index < EVENT_COUNT; index++) {
      state.storage.sql.exec("INSERT INTO managed_events (turn_id, message_json, created_at) VALUES (NULL, ?, ?)",
        JSON.stringify({ type: "status", text: String(index % 10).repeat(EVENT_TEXT_BYTES) }), Date.now());
    }
    const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
      "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": JSON.stringify(["agents:read", "agents:write", "tools:use"]),
      "x-nanocodex-prepare": "active-conversation" };
    // Runtime construction starts with the credential broker and discovery.
    const broker = vi.fn(async () => new Response(null, { status: 503 }));
    const discovery = vi.fn(async () => new Response(null, { status: 503 }));
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtimeEnv, NANOCODEX: { fetch: broker },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: discovery }) } } });
    const connect = async (cursor: string): Promise<Connection> => {
      const response = await session.fetch(new Request(`https://session.internal/socket?cursor=${cursor}`,
        { headers: { upgrade: "websocket", ...headers } }));
      expect(response.status).toBe(101);
      const socket = response.webSocket!;
      const connection: Connection = { cursors: [], bytes: 0, code: undefined };
      const closed = new Promise<void>((resolve) => {
        socket.addEventListener("message", (event) => {
          const text = event.data as string;
          const message = JSON.parse(text) as { type: string; cursor?: string };
          if (message.cursor === undefined) return;
          connection.cursors.push(message.cursor);
          connection.bytes += new TextEncoder().encode(text).byteLength;
          // Caught up: a real client keeps the socket for live events.
          if (message.cursor === String(EVENT_COUNT)) {
            socket.close(1000);
            connection.code = 1000;
            resolve();
          }
        });
        socket.addEventListener("close", (event) => { connection.code ??= event.code; resolve(); });
      });
      socket.accept();
      await closed;
      return connection;
    };
    const connections: Connection[] = [];
    let cursor = "0";
    while (cursor !== String(EVENT_COUNT) && connections.length < 64) {
      const connection = await connect(cursor);
      connections.push(connection);
      cursor = connection.cursors.at(-1) ?? cursor;
    }
    const delivered = connections.flatMap((connection) => connection.cursors);
    // Lossless, ordered and without duplicates across reconnects.
    expect(delivered).toEqual(Array.from({ length: EVENT_COUNT }, (_, index) => String(index + 1)));
    expect(connections.length).toBeGreaterThanOrEqual(5);
    for (const connection of connections.slice(0, -1)) {
      expect(connection.code).toBe(1013);
      expect(connection.bytes).toBeLessThanOrEqual(WINDOW_BYTES);
    }
    // No reconnect rebuilt the restored runtime or its startup discovery.
    expect(broker).not.toHaveBeenCalled();
    expect(discovery).not.toHaveBeenCalled();

    // A window interrupted by an isolate reset halves the next one.
    state.storage.kv.put("managed.socket_replay_guard", { token: "reset-isolate", strikes: 0, at: Date.now() });
    const degraded = await connect("0");
    expect(degraded.code).toBe(1013);
    expect(degraded.bytes).toBeLessThanOrEqual(WINDOW_BYTES / 2);
    expect(degraded.bytes).toBeGreaterThan(0);
    // The finished window decays the strike.
    expect(state.storage.kv.get("managed.socket_replay_guard")).toBeUndefined();
    await state.storage.deleteAlarm();
  });
});

