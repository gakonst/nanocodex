Warning: truncated output (original token count: 63211)
Total output lines: 6280

import {
  env,
  SELF as RAW_SELF,
  evictDurableObject,
  runDurableObjectAlarm,
  runInDurableObject,
} from "cloudflare:test";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { MemoryScope, NanocodexSession, UserAccount, type Env } from "../src/index";
import { DurableEventLog } from "../src/durable-events";
import { ManagedEventArchive } from "../src/managed-event-archive";
import type { OrganizationCapability } from "../src/account-auth";

const testEnv = env as unknown as Env;
const USER_ID = "11111111-1111-4111-8111-111111111111";
const API_KEY = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
const OTHER_USER_ID = "33333333-3333-4333-8333-333333333333";
const OTHER_API_KEY = `ncx_live_${"o".repeat(12)}_${"p".repeat(43)}`;
const READ_ONLY_API_KEY = `ncx_live_${"r".repeat(12)}_${"q".repeat(43)}`;
const WRITE_ONLY_API_KEY = `ncx_live_${"w".repeat(12)}_${"v".repeat(43)}`;
const AGENT_TOOLS_API_KEY = `ncx_live_${"t".repeat(12)}_${"u".repeat(43)}`;
const HOSTED_PASSKEY_PUBLIC_KEY = "0x046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
const HOSTED_PASSKEY_ADDRESS = "0xd3a9f047ad43d7e2e4e7e491f1fe2e657a2651b6";
const OWNER_CAPABILITIES = [
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
] as const satisfies readonly OrganizationCapability[];
const createdAgents = new Set<string>();
const CONNECT_GRANT_ID = `0x${"a".repeat(64)}`;
const SELF = { fetch: managedFetch };

beforeAll(async () => {
  await seedApiKey(USER_ID, API_KEY);
  await seedApiKey(OTHER_USER_ID, OTHER_API_KEY);
  await seedApiKey(USER_ID, READ_ONLY_API_KEY, ["agents:read"]);
  await seedApiKey(USER_ID, WRITE_ONLY_API_KEY, ["agents:write"]);
  await seedApiKey(USER_ID, AGENT_TOOLS_API_KEY, ["agents:write", "tools:use"]);
});

afterEach(async () => {
  await Promise.all([...createdAgents].map(async (id) => {
    let deleted = await SELF.fetch(`https://example.test/v1/agents/${id}`, { method: "DELETE" });
    if (deleted.status === 503) {
      await deleted.body?.cancel();
      const session = testEnv.NANOCODEX_SESSIONS.getByName(id);
      await runCleanupAlarmsUntilDeleted(session);
      deleted = await SELF.fetch(`https://example.test/v1/agents/${id}`, { method: "DELETE" });
    }
    if (deleted.status !== 204 && deleted.status !== 404) {
      throw new Error(`failed to clean up managed agent ${id}: HTTP ${deleted.status}: ${await deleted.text()}`);
    }
    createdAgents.delete(id);
  }));
});

describe("managed agents REST and resumable SSE", () => {
  it("chunks and exactly replays an oversized managed event", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const payload = `${"a".repeat(255_999)}😀${"b".repeat(2 * 1024 * 1024)}`;
    const inserted = await runInDurableObject(session, (_instance, state) => {
      const log = new DurableEventLog<{ payload: string; type: string }>(state.storage);
      const before = log.totalBytes();
      const event = log.record({ type: "api.event", payload });
      const chunks = state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_event_chunks WHERE cursor = CAST(? AS INTEGER)",
        event.cursor,
      ).one().count;
      const delta = log.totalBytes() - before;
      const history = log.history(undefined, 1).data[0]!.message.payload;
      const page = log.page((BigInt(event.cursor) - 1n).toString(), 1)[0]!.message.payload;
      log.record({ type: "stream_failed", payload: "archive-tail" });
      return {
        chunks,
        cursor: event.cursor,
        delta,
        history,
        page,
        totalBytes: log.totalBytes(),
      };
    });
    expect(inserted).toMatchObject({ chunks: expect.any(Number), history: payload, page: payload });
    expect(inserted.chunks).toBeGreaterThan(1);
    expect(inserted.delta).toBe(new TextEncoder().encode(JSON.stringify({
      type: "api.event",
      payload,
    })).byteLength);
    const capacity = await (await session.fetch("https://session.internal/capacity")).json<{
      managed_events: { bytes: number; rows: number };
    }>();
    expect(capacity.managed_events).toEqual({ bytes: inserted.totalBytes, rows: 3 });

    const history = await managedHistory(agent);
    expect(history.data.find(({ cursor }) => cursor === inserted.cursor)).toMatchObject({ payload });
    const sealed = await session.fetch("https://session.internal/events/archive", { method: "POST" });
    expect(await sealed.json()).toMatchObject({ sealed: true });
    expect(await runInDurableObject(session, (_instance, state) => state.storage.sql.exec<{
      count: number;
    }>("SELECT COUNT(*) AS count FROM managed_event_chunks").one().count)).toBe(0);
    expect((await managedHistory(agent)).data.find(
      ({ cursor }) => cursor === inserted.cursor,
    )).toMatchObject({ payload });
  }, 30_000);

  it("chunks frozen dispatch input outside the managed turn row", async () => {
    const agent = await createAgent();
    const id = "turn-large-dispatch-input";
    const input = `LARGE_DISPATCH ${"x".repeat(999_000)}`;
    await submit(agent, id, input);
    await waitForTurnState(agent, id, "completed", 10_000);

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const stored = await runInDurableObject(session, (_instance, state) => ({
      chunks: state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_turn_dispatch_chunks WHERE turn_id = ?",
        id,
      ).one().count,
      largestChunk: state.storage.sql.exec<{ bytes: number }>(
        `SELECT COALESCE(MAX(LENGTH(CAST(input_json AS BLOB))), 0) AS bytes
         FROM managed_turn_dispatch_chunks WHERE turn_id = ?`,
        id,
      ).one().bytes,
      rawInputBytes: state.storage.sql.exec<{ bytes: number }>(
        `SELECT LENGTH(CAST(input_json AS BLOB)) AS bytes
         FROM managed_turns WHERE id = ?`,
        id,
      ).one().bytes,
    }));
    expect(stored.rawInputBytes).toBeGreaterThan(990_000);
    expect(stored.chunks).toBeGreaterThan(1);
    expect(stored.largestChunk).toBeLessThanOrEqual(256_000);
  }, 30_000);

  it("attributes the private runtime session to its public managed turn", async () => {
    const agent = await createAgent();
    const turnId = "turn-runtime-attribution";
    await submit(agent, turnId, "CAPACITY_ACCOUNTING");
    await waitForTurnState(agent, turnId, "completed", 10_000);

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const identities = await runInDurableObject(session, (_instance, state) =>
      state.storage.sql.exec<{ managed_id: string; runtime_id: string }>(
        `SELECT session_state.session_id AS managed_id,
                nanocodex_cloudflare_agent.session_id AS runtime_id
         FROM session_state, nanocodex_cloudflare_agent`,
      ).toArray()[0]!,
    );
    expect(identities.runtime_id).not.toBe(identities.managed_id);

    const history = await managedHistory(agent);
    expect(history.data).toEqual(expect.arrayContaining([
      expect.objectContaining({
        event: expect.objectContaining({ type: "run.started" }),
        turn_id: turnId,
      }),
      expect.objectContaining({
        event: expect.objectContaining({ type: "run.completed" }),
        turn_id: turnId,
      }),
    ]));
  });

  it("indexes managed lifecycle logs by account, team, thread, and turn without prompt data", async () => {
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const prompt = "OBSERVABILITY_PROMPT_MUST_NOT_BE_LOGGED";
    try {
      const agent = await createAgent();
      const turnId = "turn-observability-coordinates";
      await submit(agent, turnId, prompt);
      await waitForTurnState(agent, turnId, "completed", 10_000);

      const entries = info.mock.calls.flatMap(([entry]) => (
        entry && typeof entry === "object" ? [entry as Record<string, unknown>] : []
      ));
      expect(entries).toEqual(expect.arrayContaining([
        expect.objectContaining({
          type: "managed.agent.created",
          user_id: USER_ID,
          organization_id: expect.stringMatching(/^[0-9a-f-]{36}$/),
          team_id: expect.stringMatching(/^[0-9a-f-]{36}$/),
          agent_id: agent.agent_id,
          thread_id: agent.agent_id,
        }),
        expect.objectContaining({
          type: "managed.turn.accepted",
          user_id: USER_ID,
          agent_id: agent.agent_id,
          thread_id: agent.agent_id,
          turn_id: turnId,
        }),
        expect.objectContaining({
          type: "managed.turn.transition",
          user_id: USER_ID,
          agent_id: agent.agent_id,
          turn_id: turnId,
          state: "completed",
          terminal: true,
        }),
      ]));
      expect(JSON.stringify(entries)).not.toContain(prompt);
    } finally {
      info.mockRestore();
    }
  });

  it("accounts for each independently growing per-agent durable payload", async () => {
    const agent = await createAgent();
    await submit(agent, "turn-capacity", "CAPACITY_ACCOUNTING");
    await waitForTurnState(agent, "turn-capacity", "completed");

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const response = await session.fetch("https://session.internal/capacity");
    expect(response.status).toBe(200);
    const capacity = await response.json<{
      archived_turns: { archived_bytes: number; archived_receipts: number; objects: number };
      database_size_bytes: number;
      journal: { bytes: number; max_batch_bytes: number; revision: string; rows: number };
      known_payload_bytes: number;
      managed_events: { bytes: number; rows: number };
      raw_events: { bytes: number; rows: number };
      turns: {
        blocked_rows: number;
        input_bytes: number;
        terminal_bytes: number;
        terminal_rows: number;
        total_rows: number;
        unfinished_rows: number;
      };
      unattributed_database_bytes: number;
    }>();

    expect(capacity.database_size_bytes).toBeGreaterThan(0);
    expect(capacity.journal).toMatchObject({ rows: expect.any(Number) });
    expect(capacity.journal.rows).toBeGreaterThan(0);
    expect(capacity.journal.bytes).toBeGreaterThan(0);
    expect(capacity.journal.max_batch_bytes).toBeGreaterThan(0);
    expect(BigInt(capacity.journal.revision)).toBeGreaterThan(0n);
    expect(capacity.managed_events.rows).toBeGreaterThan(0);
    expect(capacity.managed_events.bytes).toBeGreaterThan(0);
    expect(capacity.raw_events).toEqual({ rows: 0, bytes: 0 });
    expect(capacity.turns).toMatchObject({
      blocked_rows: 0,
      terminal_rows: 1,
      total_rows: 1,
      unfinished_rows: 0,
    });
    expect(capacity.turns.input_bytes).toBeGreaterThan(0);
    expect(capacity.turns.terminal_bytes).toBeGreaterThan(0);
    expect(capacity.archived_turns).toEqual({
      archived_bytes: 0,
      archived_receipts: 0,
      objects: 0,
    });
    expect(capacity.known_payload_bytes).toBeGreaterThan(0);
    expect(capacity.database_size_bytes).toBeGreaterThanOrEqual(capacity.known_payload_bytes);
    expect(capacity.unattributed_database_bytes).toBe(
      capacity.database_size_bytes - capacity.known_payload_bytes,
    );
  });

  it("seals immutable event prefixes and replays one exact R2 and SQLite cursor space", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    for (let index = 0; index < 3; index += 1) {
      const id = `turn-archive-${index}`;
      await submit(agent, id, `ARCHIVE_${index}`);
      await waitForTurnState(agent, id, "completed");
      const sealed = await session.fetch("https://session.internal/events/archive", {
        method: "POST",
      });
      expect(sealed.status).toBe(200);
      expect(await sealed.json()).toMatchObject({ sealed: true });
    }

    const complete = await managedHistory(agent);
    const capacity = await (await session.fetch("https://session.internal/capacity")).json<{
      archived_events: {
        archived_events: number;
        archived_through: string;
        recent_descriptors: number;
        segments: number;
      };
      managed_events: { rows: number };
    }>();
    expect(capacity.archived_events).toMatchObject({
      archived_events: expect.any(Number),
      archived_through: expect.stringMatching(/^[1-9][0-9]*$/),
      recent_descriptors: 3,
      segments: 3,
    });
    expect(capacity.archived_events.archived_events).toBeGreaterThan(0);
    expect(capacity.managed_events.rows).toBe(1);

    const paged: ManagedHistoryEvent[] = [];
    let before: string | undefined;
    while (true) {
      const query = new URL(`${agent.events_url}/history`);
      query.searchParams.set("limit", "3");
      if (before !== undefined) query.searchParams.set("before", before);
      const response = await SELF.fetch(query);
      expect(response.status).toBe(200);
      const page = await response.json<ManagedHistory>();
      paged.unshift(...page.data);
      if (!page.has_more) break;
      before = page.data[0]!.cursor;
    }
    expect(paged.map(({ cursor }) => cursor)).toEqual(
      complete.data.map(({ cursor }) => cursor),
    );

    const replay = sseReader(await SELF.fetch(`${agent.events_url}?cursor=0`));
    const replayed: string[] = [];
    while (replayed.length < complete.data.length) {
      replayed.push((await nextWithin(replay, "archived SSE replay")).id);
    }
    await replay.cancel();
    expect(replayed).toEqual(complete.data.map(({ cursor }) => cursor));

    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const prefix = `agents/${subject}/managed-events/`;
    await testEnv.NANOCODEX_HISTORY.put(`${prefix}segments/orphan.json`, "orphan");
    expect((await testEnv.NANOCODEX_HISTORY.list({ prefix })).objects.length).toBeGreaterThan(0);
    const deleted = await SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
      method: "DELETE",
    });
    expect(deleted.status).toBe(204);
    createdAgents.delete(agent.agent_id);
    expect((await testEnv.NANOCODEX_HISTORY.list({ prefix })).objects).toHaveLength(0);
  }, 30_000);

  it("bounds the hot archive manifest after immutable index rollover", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const messageJson = JSON.stringify({ type: "stream_failed", error: "rollover" });
    const messageBytes = new TextEncoder().encode(messageJson).byteLength;
    for (let index = 0; index < 34; index += 1) {
      await runInDurableObject(session, (_instance, state) => {
        state.storage.transactionSync(() => {
          state.storage.sql.exec(
            "INSERT INTO managed_events (turn_id, message_json, created_at) VALUES (NULL, ?, ?)",
            messageJson,
            Date.now() + index,
          );
          state.storage.sql.exec(
            "UPDATE managed_event_meta SET total_bytes = total_bytes + ? WHERE singleton = 1",
            messageBytes,
          );
        });
      });
      const response = await session.fetch("https://session.internal/events/archive", {
        method: "POST",
      });
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ sealed: true });
    }

    const capacity = await (await session.fetch("https://session.internal/capacity")).json<{
      archived_events: {
        index_nodes: number;
        recent_descriptors: number;
        segments: number;
      };
      managed_events: { rows: number };
    }>();
    expect(capacity.archived_events).toMatchObject({
      index_nodes: 2,
      recent_descriptors: 2,
      segments: 34,
    });
    expect(capacity.managed_events.rows).toBe(1);

    const history = await managedHistory(agent);
    expect(history.data).toHaveLength(35);
    expect(history.data.map(({ cursor }) => BigInt(cursor))).toEqual(
      [...history.data.map(({ cursor }) => BigInt(cursor))]
        .sort((left, right) => left < right ? -1 : 1),
    );

    const replay = sseReader(await SELF.fetch(`${agent.events_url}?cursor=0`));
    const replayed: string[] = [];
    while (replayed.length < history.data.length) {
      replayed.push((await nextWithin(replay, "indexed archive SSE replay")).id);
    }
    await replay.cancel();
    expect(replayed).toEqual(history.data.map(({ cursor }) => cursor));
  }, 45_000);

  it("keeps SQLite authoritative when a seal source changes after the R2 put", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const messageJson = JSON.stringify({ type: "stream_failed", error: "fence" });
    const messageBytes = new TextEncoder().encode(messageJson).byteLength;
    await runInDurableObject(session, (_instance, state) => {
      state.storage.transactionSync(() => {
        state.storage.sql.exec(
          "INSERT INTO managed_events (turn_id, message_json, created_at) VALUES (NULL, ?, ?)",
          messageJson,
          Date.now(),
        );
        state.storage.sql.exec(
          "UPDATE managed_event_meta SET total_bytes = total_bytes + ? WHERE singleton = 1",
          messageBytes,
        );
      });
    });

    const result = await runInDurableObject(session, async (_instance, state) => {
      let notifyUploaded!: () => void;
      let releasePut!: () => void;
      const uploaded = new Promise<void>((resolve) => { notifyUploaded = resolve; });
      const held = new Promise<void>((resolve) => { releasePut = resolve; });
      const delayedBucket = new Proxy(testEnv.NANOCODEX_HISTORY, {
        get(target, property) {
          if (property === "put") {
            return async (...args: Parameters<R2Bucket["put"]>) => {
              const stored = await target.put(...args);
              notifyUploaded();
              await held;
              return stored;
            };
          }
          const value = Reflect.get(target, property);
          return typeof value === "function" ? value.bind(target) : value;
        },
      });
      const archive = new ManagedEventArchive<{ type: string }>(
        state.storage,
        delayedBucket,
        state.id.toString(),
      );
      const sealing = archive.seal(true);
      await uploaded;
      state.storage.sql.exec(
        "UPDATE managed_events SET created_at = created_at + 1 WHERE cursor = (SELECT MIN(cursor) FROM managed_events)",
      );
      releasePut();
      let failure = "";
      try { await sealing; } catch (error) {
        failure = error instanceof Error ? error.message : String(error);
      }
      state.storage.sql.exec(
        "UPDATE managed_events SET created_at = created_at - 1 WHERE cursor = (SELECT MIN(cursor) FROM managed_events)",
      );
      return { capacity: archive.capacity(), failure };
    });
    expect(result.failure).toContain("source prefix changed before commit");
    expect(result.capacity).toMatchObject({
      archived_events: 0,
      archived_through: "0",
      segments: 0,
    });

    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const prefix = `agents/${subject}/managed-events/`;
    expect((await testEnv.NANOCODEX_HISTORY.list({ prefix })).objects).toHaveLength(1);
    expect((await managedHistory(agent)).data).toHaveLength(2);
  }, 30_000);

  it("archives a byte-heavy tail even when it is smaller than the recent event window", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const result = await runInDurableObject(session, async (_instance, state) => {
      const archive = new ManagedEventArchive<{ type: string }>(
        state.storage,
        testEnv.NANOCODEX_HISTORY,
        state.id.toString(),
        { recentEventCount: 512, sealThresholdBytes: 1, segmentTargetBytes: 1024 * 1024 },
      );
      const sealed = await archive.seal(false);
      const localRows = state.storage.sql.exec<{ rows: number }>(
        "SELECT COUNT(*) AS rows FROM managed_events",
      ).one().rows;
      return { localRows, sealed };
    });
    expect(result.sealed).toMatchObject({ archived_events: 1, sealed: true });
    expect(result.localRows).toBe(0);
    const history = await managedHistory(agent);
    expect(history.data).toHaveLength(1);
    expect(history.data[0]).toMatchObject({ type: "agent_created" });
  });

  it("archives terminal receipts without weakening exact idempotency", async () => {
    const agent = await createAgent();
    for (let index = 0; index < 3; index += 1) {
      const id = `turn-receipt-${index}`;
      await submit(agent, id, `RECEIPT_${index}`);
      await waitForTurnState(agent, id, "completed");
    }
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const sealed = await session.fetch("https://session.internal/turns/archive", {
      method: "POST",
    });
    expect(sealed.status).toBe(200);
    expect(await sealed.json()).toMatchObject({
      archived_receipts: 2,
      objects: 4,
      sealed: true,
    });

    const capacity = await (await session.fetch("https://session.internal/capacity")).json<{
      archived_turns: { archived_receipts: number; objects: number };
      turns: { terminal_rows: number; total_rows: number };
    }>();
    expect(capacity.archived_turns).toMatchObject({ archived_receipts: 2, objects: 4 });
    expect(capacity.turns).toMatchObject({ terminal_rows: 1, total_rows: 1 });

    const oldTurnUrl = agent.events_url.replace(/\/events$/, "/turns/turn-receipt-0");
    const retained = await SELF.fetch(oldTurnUrl);
    expect(retained.status).toBe(200);
    expect(await retained.json()).toMatchObject({
      state: "completed",
      turn_id: "turn-receipt-0",
    });

    const replay = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "request-turn-receipt-0",
      },
      body: JSON.stringify({ id: "turn-receipt-0", input: "RECEIPT_0" }),
    });
    expect(replay.status).toBe(200);
    expect(replay.headers.get("x-nanocodex-turn-created")).toBeNull();

    const conflictingKey = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "request-turn-receipt-0",
      },
      body: JSON.stringify({ id: "another-turn", input: "RECEIPT_0" }),
    });
    expect(conflictingKey.status).toBe(409);
    expect(await conflictingKey.json()).toMatchObject({ error: "idempotency_conflict" });

    const state = await (await session.fetch("https://session.internal/state")).json<{
      completed_turns: number;
      first_prompt: string;
    }>();
    expect(state).toMatchObject({ completed_turns: 3, first_prompt: "RECEIPT_0" });

    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const idHash = await testHash("turn-receipt-0");
    await testEnv.NANOCODEX_HISTORY.put(
      `agents/${subject}/managed-turns/by-id/${idHash}.json`,
      "corrupt",
    );
    const corrupt = await SELF.fetch(oldTurnUrl);
    expect(corrupt.status).toBe(503);
    expect(await corrupt.json()).toMatchObject({ error: "turn_archive_unavailable" });
  }, 30_000);

  it("compacts a terminal journal prefix and cold-reopens from the retained checkpoint", async () => {
    const agent = await createAgent();
    for (let index = 0; index < 22; index += 1) {
      const id = `turn-compaction-${index}`;
      await submit(agent, id, `COMPACTION_${index}`);
      await waitForTurnState(agent, id, "completed");
    }

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const before = await (await session.fetch("https://session.internal/capacity")).json<{
      journal: { revision: string; rows: number };
    }>();
    expect(BigInt(before.journal.revision)).toBeGreaterThanOrEqual(66n);
    expect(BigInt(before.journal.rows)).toBeLessThan(BigInt(before.journal.revision));

    let pendingProjections = Number.POSITIVE_INFINITY;
    for (let attempt = 0; attempt < 4 && pendingProjections > 0; attempt += 1) {
      await runInDurableObject(session, async (_instance, state) => {
        await state.storage.setAlarm(Date.now());
      });
      await runDurableObjectAlarm(session);
      pendingProjections = await runInDurableObject(session, (_instance, state) => (
        state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM history_projection_outbox",
        ).one().count
      ));
    }
    expect(pendingProjections).toBe(0);
    await runInDurableObject(session, async (_instance, state) => {
      state.storage.sql.exec(
        "UPDATE session_state SET last_active = ? WHERE singleton = 1",
        Date.now() - 60_000,
      );
      await state.storage.setAlarm(Date.now());
    });
    await runDurableObjectAlarm(session);
    await evictDurableObject(session);

    await submit(agent, "turn-after-compaction-reopen", "AFTER_COMPACTION_REOPEN");
    await waitForTurnState(agent, "turn-after-compaction-reopen", "completed");
    const after = await (await session.fetch("https://session.internal/capacity")).json<{
      journal: { revision: string; rows: number };
      turns: { terminal_rows: number };
    }>();
    expect(BigInt(after.journal.revision)).toBeGreaterThan(BigInt(before.journal.revision));
    expect(BigInt(after.journal.rows)).toBeLessThan(BigInt(after.journal.revision));
    expect(after.turns.terminal_rows).toBe(23);
  }, 60_000);

  it("compacts multiple retained journal batches before cold Agent construction", async () => {
    const agent = await createAgent();
    await submit(agent, "turn-before-cold-preconstruction", "BEFORE_COLD_PRECONSTRUCTION");
    await waitForTurnState(agent, "turn-before-cold-preconstruction", "completed");

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const before = await (await session.fetch("https://session.internal/capacity")).json<{
      journal: { revision: string; rows: number };
    }>();
    expect(before.journal.rows).toBeGreaterThan(1);
    await evictDurableObject(session);

    await submit(agent, "turn-after-cold-preconstruction", "AFTER_COLD_PRECONSTRUCTION");
    await waitForTurnState(agent, "turn-after-cold-preconstruction", "completed");
    const after = await (await session.fetch("https://session.internal/capacity")).json<{
      journal: { revision: string; rows: number };
    }>();
    const appended = Number(BigInt(after.journal.revision) - BigInt(before.journal.revision));
    expect(appended).toBeGreaterThan(0);
    expect(after.journal.rows).toBe(1 + appended);
  }, 30_000);

  it("keeps connector OAuth state and credentials behind a persistent account", async () => {
    const publicEgressEnvelope = JSON.stringify({
      thread_id: "77777777-7777-4777-8777-777777777777",
      url: "https://example.com/public",
      method: "GET",
      headers: { accept: "application/json" },
    });
    const unauthenticated = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: { origin: "https://example.test", "content-type": "application/json" },
      body: publicEgressEnvelope,
    });
    expect(unauthenticated.status).toBe(401);
    expect(await unauthenticated.json()).toEqual({ error: "unauthorized" });

    const apiKeyPrincipal = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: {
        authorization: `Bearer ${API_KEY}`,
        origin: "https://example.test",
        "content-type": "application/json",
      },
      body: publicEgressEnvelope,
    });
    expect(apiKeyPrincipal.status).toBe(401);

    const session = await RAW_SELF.fetch("https://example.test/v1/me");
    const anonymousCookie = session.headers.get("set-cookie")?.split(";", 1)[0];
    expect(anonymousCookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);

    const anonymous = await RAW_SELF.fetch("https://example.test/v1/connectors", {
      headers: { cookie: anonymousCookie! },
    });
    expect(anonymous.status).toBe(401);
    expect(await anonymous.json()).toEqual({ error: "unauthorized" });

    const anonymousEgress = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: {
        cookie: anonymousCookie!,
        origin: "https://example.test",
        "content-type": "application/json",
      },
      body: publicEgressEnvelope,
    });
    expect(anonymousEgress.status).toBe(401);

    const connectorUserId = "55555555-5555-4555-8555-555555555555";
    const connectorToken = "c".repeat(43);
    await seedPasskeySession(connectorUserId, connectorToken);
    const cookie = `nanocodex_account=${connectorToken}`;

    const initial = await RAW_SELF.fetch("https://example.test/v1/connectors", {
      headers: { cookie },
    });
    expect(initial.status).toBe(200);
    expect(await initial.json()).toMatchObject({
      connectors: {
        github: { connected: false },
        gmail: { connected: false },
        gdrive: { connected: false },
        x: { connected: false },
      },
    });

    const missingOrigin = await RAW_SELF.fetch("https://example.test/v1/connectors/github", {
      method: "POST",
      headers: { cookie, "content-type": "application/json" },
      body: JSON.stringify({ return_to: "/agent" }),
    });
    expect(missingOrigin.status).toBe(403);

    const started = await RAW_SELF.fetch("https://example.test/v1/connectors/github", {
      method: "POST",
      headers: {
        cookie,
        "content-type": "application/json",
        origin: "https://example.test",
      },
      body: JSON.stringify({ return_to: "/agent?thread=connector" }),
    });
    expect(started.status).toBe(200);
    const authorization = new URL((await started.json<{ authorization_url: string }>()).authorization_url);
    expect(authorization.origin).toBe("https://provider.test");
    expect(authorization.searchParams.get("redirect_uri")).toBe(
      "https://example.test/v1/connectors/github/callback",
    );

    const callback = await RAW_SELF.fetch(
      `https://example.test/v1/connectors/github/callback?${new URLSearchParams({
        code: "authorization-code",
        state: authorization.searchParams.get("state")!,
      })}`,
      { headers: { cookie }, redirect: "manual" },
    );
    expect(callback.status).toBe(200);
    expect(callback.headers.get("content-type")).toBe("text/html; charset=utf-8");
    const completionPage = await callback.text();
    expect(completionPage).toContain('"type":"nanocodex:connector-complete"');
    expect(completionPage).toContain('"connector":"github"');
    expect(completionPage).toContain('"result":"success"');
    expect(completionPage).toContain("window.close()");

    const connected = await RAW_SELF.fetch("https://example.test/v1/connectors", {
      headers: { cookie },
    });
    expect(await connected.json()).toMatchObject({
      connectors: { github: { connected: true, label: "Nano Cat" } },
    });

    const egressEnvelope = JSON.stringify({
      thread_id: "77777777-7777-4777-8777-777777777777",
      url: "https://api.github.com/repos/gakonst/nanocodex",
      method: "GET",
      headers: { accept: "application/json" },
    });
    const crossOriginRead = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: { cookie, origin: "https://attacker.test", "content-type": "application/json" },
      body: egressEnvelope,
    });
    expect(crossOriginRead.status).toBe(403);

    const githubRead = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
      body: egressEnvelope,
    });
    expect(githubRead.status).toBe(200);
    expect(githubRead.headers.get("cache-control")).toBe("no-store");
    const githubValue = await githubRead.json<{ subject: string }>();
    expect(githubValue).toMatchObject({
      cookie: null,
      full_name: "gakonst/nanocodex",
      subject: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/),
    });

    const forgedOuterPrincipal = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: {
        authorization: `Bearer ${OTHER_API_KEY}`,
        cookie,
        origin: "https://example.test",
        "content-type": "application/json",
        "x-nanocodex-subject": "b".repeat(43),
      },
      body: egressEnvelope,
    });
    expect(forgedOuterPrincipal.status).toBe(200);
    expect((await forgedOuterPrincipal.json<{ subject: string }>()).subject).toBe(githubValue.subject);

    for (const forged of [
      { authorization: "Bearer attacker" },
      { cookie: "nanocodex_account=attacker" },
      { "proxy-authorization": "Basic attacker" },
      { "x-nanocodex-subject": "a".repeat(43) },
    ]) {
      const response = await RAW_SELF.fetch("https://example.test/v1/egress", {
        method: "POST",
        headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
        body: JSON.stringify({
          ...JSON.parse(egressEnvelope),
          headers: forged,
        }),
      });
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_headers" });
    }
    for (const field of ["principal", "subject", "user_id"]) {
      const response = await RAW_SELF.fetch("https://example.test/v1/egress", {
        method: "POST",
        headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
        body: JSON.stringify({
          ...JSON.parse(egressEnvelope),
          [field]: connectorUserId,
        }),
      });
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_request" });
    }

    const deniedDestination = await RAW_SELF.fetch("https://example.test/v1/egress", {
      method: "POST",
      headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
      body: JSON.stringify({
        thread_id: "77777777-7777-4777-8777-777777777777",
        url: "http://127.0.0.1/private",
      }),
    });
    expect(deniedDestination.status).toBe(403);

    const disconnected = await RAW_SELF.fetch("https://example.test/v1/connectors/github", {
      method: "DELETE",
      headers: { cookie, origin: "https://example.test" },
    });
    expect(disconnected.status).toBe(204);
  });

  it("projects an owner-bound MCP catalog without broker-private connection material", async () => {
    const userId = "66666666-6666-4666-8666-666666666666";
    const token = "m".repeat(43);
    const cookie = `nanocodex_account=${token}`;
    const connectionId = "L".repeat(43);
    await seedPasskeySession(userId, token);

    const originalBroker = testEnv.NANOCODEX;
    const brokerRequests: Array<{ body: string; method: string; url: string }> = [];
    let listing: "valid" | "invalid" | "failed" = "valid";
    let createdId: string | undefined;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        brokerRequests.push({
          body: await request.text(),
          method: request.method,
          url: request.url,
        });
        if (request.method === "GET") {
          if (listing === "failed") {
            return Response.json({ access_token: "must-not-leak" }, { status: 503 });
          }
          return Response.json({
            endpoint: "https://must-not-leak.example/mcp",
            mcp_connections: [
              {
                id: connectionId,
                name: listing === "invalid" ? "\u0000unsafe" : "Linear workspace",
                status: "connected",
                endpoint: "https://must-not-leak.example/mcp",
                requested_scopes: ["admin"],
                access_token: "must-not-leak",
              },
              {
                id: "b".repeat(43),
                name: "Linear workspace",
                status: "authorization_required",
              },
              {
                id: "c".repeat(43),
                name: "Linear workspace",
                status: "revoked",
              },
            ],
          });
        }
        if (request.method === "PUT") {
          createdId = request.url.split("/").at(-1);
          return Response.json({
            mcp_connections: [{
              id: createdId,
              name: "mcp.linear.app",
              status: "authorization_required",
              endpoint: "https://must-not-leak.example/mcp",
              access_token: "must-not-leak",
            }],
          });
        }
        if (request.method === "POST" && request.url.endsWith("/start")) {
          return Response.json({
            mcp_connections: [{ id: createdId, name: "mcp.linear.app", status: "authorization_required" }],
            authorization_url: "https://provider.test/authorize?state=broker-state",
            access_token: "must-not-leak",
          });
        }
        if (request.method === "POST" && request.url.endsWith("/callback")) {
          return Response.json({
            mcp_connections: [{ id: createdId, name: "mcp.linear.app", status: "connected" }],
            return_to: "/connect?section=connectors",
            access_token: "must-not-leak",
          });
        }
        if (request.method === "DELETE") {
          return Response.json({
            mcp_connections: [{ id: connectionId, name: "Linear workspace", status: "revoked" }],
            refresh_token: "must-not-leak",
          });
        }
        return Response.json({ error: "method_not_allowed" }, { status: 405 });
      },
    } as Fetcher;

    try {
      const anonymous = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections");
      expect(anonymous.status).toBe(401);
      expect(brokerRequests).toHaveLength(0);

      const listed = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        headers: { cookie },
      });
      expect(listed.status).toBe(200);
      expect(listed.headers.get("cache-control")).toBe("no-store");
      expect(await listed.json()).toEqual({
        mcp_connections: [
          { id: connectionId, name: "Linear workspace", status: "connected" },
          { id: "b".repeat(43), name: "Linear workspace", status: "authorization_required" },
        ],
      });
      expect(brokerRequests).toHaveLength(1);
      expect(brokerRequests[0]?.url).toBe(
        `https://broker.internal/users/${userId}/mcp-connections`,
      );
      expect(brokerRequests[0]?.body).toBe("");

      const deniedCreate = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        method: "POST",
        headers: { cookie, "content-type": "application/json" },
        body: JSON.stringify({ target: "mcp.linear.app" }),
      });
      expect(deniedCreate.status).toBe(403);
      expect(brokerRequests).toHaveLength(1);

      const invalidTarget = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        method: "POST",
        headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
        body: JSON.stringify({ target: "https://mcp.linear.app/mcp?token=must-not-leak" }),
      });
      expect(invalidTarget.status).toBe(400);
      expect(await invalidTarget.json()).toEqual({ error: "invalid_mcp_target" });
      expect(brokerRequests).toHaveLength(1);

      const created = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        method: "POST",
        headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
        body: JSON.stringify({ target: "mcp.linear.app" }),
      });
      expect(created.status).toBe(201);
      const createdBody = await created.json<{ mcp_connection: { id: string; name: string; status: string } }>();
      expect(createdBody).toMatchObject({
        mcp_connection: { id: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/), name: "mcp.linear.app", status: "authorization_required" },
      });
      expect(JSON.stringify(createdBody)).not.toContain("must-not-leak");
      expect(brokerRequests).toHaveLength(2);
      expect(brokerRequests[1]?.method).toBe("PUT");
      expect(brokerRequests[1]?.url).toBe(
        `https://broker.internal/users/${userId}/mcp-connections/${createdBody.mcp_connection.id}`,
      );
      expect(JSON.parse(brokerRequests[1]!.body)).toEqual({
        endpoint: "https://mcp.linear.app/mcp",
        name: "mcp.linear.app",
      });

      const started = await RAW_SELF.fetch(
        `https://example.test/v1/connectors/mcp-connections/${createdBody.mcp_connection.id}/start`,
        {
          method: "POST",
          headers: { cookie, origin: "https://example.test", "content-type": "application/json" },
          body: JSON.stringify({ return_to: "/connect?section=connectors" }),
        },
      );
      expect(started.status).toBe(200);
      expect(await started.json()).toEqual({
        mcp_connection: createdBody.mcp_connection,
        authorization_url: "https://provider.test/authorize?state=broker-state",
      });
      expect(brokerRequests).toHaveLength(3);
      expect(JSON.parse(brokerRequests[2]!.body)).toEqual({
        redirect_uri: `https://example.test/v1/connectors/mcp-connections/${createdBody.mcp_connection.id}/callback`,
        return_to: "/connect?section=connectors",
      });

      const callback = await RAW_SELF.fetch(
        `https://example.test/v1/connectors/mcp-connections/${createdBody.mcp_connection.id}/callback?code=authorization-code&state=broker-state`,
        { headers: { cookie }, redirect: "manual" },
      );
      expect(callback.status).toBe(303);
      const callbackLocation = new URL(callback.headers.get("location")!);
      expect(callbackLocation.pathname).toBe("/connect");
      expect(callbackLocation.searchParams.get("section")).toBe("connectors");
      expect(callbackLocation.searchParams.get("mcp_connection")).toBe(createdBody.mcp_connection.id);
      expect(callbackLocation.searchParams.get("mcp_result")).toBe("connected");
      expect(brokerRequests).toHaveLength(4);
      expect(JSON.parse(brokerRequests[3]!.body)).toEqual({
        code: "authorization-code",
        state: "broker-state",
        error: null,
        error_description: null,
      });

      for (const origin of [undefined, "https://attacker.test"]) {
        const denied = await RAW_SELF.fetch(
          `https://example.test/v1/connectors/mcp-connections/${connectionId}`,
          {
            method: "DELETE",
            headers: { cookie, ...(origin ? { origin } : {}) },
          },
        );
        expect(denied.status).toBe(403);
      }
      expect(brokerRequests).toHaveLength(4);

      const disconnected = await RAW_SELF.fetch(
        `https://example.test/v1/connectors/mcp-connections/${connectionId}`,
        { method: "DELETE", headers: { cookie, origin: "https://example.test" } },
      );
      expect(disconnected.status).toBe(204);
      expect(await disconnected.text()).toBe("");
      expect(brokerRequests).toHaveLength(5);
      expect(brokerRequests[4]?.url).toBe(
        `https://broker.internal/users/${userId}/mcp-connections/${connectionId}`,
      );
      expect(brokerRequests[4]?.method).toBe("DELETE");
      expect(brokerRequests[4]?.body).toBe("");

      listing = "invalid";
      const invalid = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        headers: { cookie },
      });
      expect(invalid.status).toBe(502);
      expect(await invalid.json()).toEqual({ error: "mcp_broker_invalid" });

      listing = "failed";
      const failed = await RAW_SELF.fetch("https://example.test/v1/connectors/mcp-connections", {
        headers: { cookie },
      });
      expect(failed.status).toBe(502);
      expect(await failed.json()).toEqual({ error: "mcp_broker_failed" });
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("requires a persistent account for credential mutations without requiring bodies", async () => {
    const anonymousSession = await RAW_SELF.fetch("https://example.test/v1/me");
    const anonymousCookie = anonymousSession.headers.get("set-cookie")?.split(";", 1)[0];
    expect(anonymousCookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);

    const originalBroker = testEnv.NANOCODEX;
    const brokerRequests: Request[] = [];
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        brokerRequests.push(request);
        return new Response(null, { status: 204 });
      },
    } as Fetcher;

    try {
      const anonymousDelete = await RAW_SELF.fetch("https://example.test/v1/credentials/openai", {
        method: "DELETE",
        headers: { cookie: anonymousCookie!, origin: "https://example.test" },
      });
      expect(anonymousDelete.status).toBe(401);
      expect(brokerRequests).toHaveLength(0);

      const anonymousPut = await RAW_SELF.fetch("https://example.test/v1/credentials/openai", {
        method: "PUT",
        headers: {
          cookie: anonymousCookie!,
          origin: "https://example.test",
          "content-type": "application/json",
        },
        body: JSON.stringify({ api_key: "sk-anonymous-must-not-store" }),
      });
      expect(anonymousPut.status).toBe(401);
      expect(brokerRequests).toHaveLength(0);

      const anonymousApiKey = await RAW_SELF.fetch("https://example.test/v1/api-keys", {
        method: "POST",
        headers: {
          cookie: anonymousCookie!,
          origin: "https://example.test",
          "content-type": "application/json",
        },
        body: JSON.stringify({ label: "must-not-create" }),
      });
      expect(anonymousApiKey.status).toBe(401);
      expect(brokerRequests).toHaveLength(0);

      const userId = "88888888-8888-4888-8888-888888888888";
      const token = "p".repeat(43);
      await seedPasskeySession(userId, token);
      const cookie = `nanocodex_account=${token}`;

      const persistentPut = await RAW_SELF.fetch("https://example.test/v1/credentials/openai", {
        method: "PUT",
        headers: {
          cookie,
          origin: "https://example.test",
          "content-type": "application/json",
        },
        body: JSON.stringify({ api_key: "sk-persistent-secret" }),
      });
      expect(persistentPut.status).toBe(204);

      const persistentDelete = await RAW_SELF.fetch("https://example.test/v1/credentials/openai", {
        method: "DELETE",
        headers: { cookie, origin: "https://example.test" },
      });
      expect(persistentDelete.status).toBe(204);

      const localClaim = await RAW_SELF.fetch("https://example.test/v1/credentials/local-claim", {
        method: "POST",
        headers: { cookie, origin: "https://example.test" },
      });
      expect(localClaim.status).toBe(204);
      expect(brokerRequests).toHaveLength(3);
      expect(brokerRequests[0]?.body).not.toBeNull();
      expect(brokerRequests[1]?.body).toBeNull();
      expect(brokerRequests[2]?.body).toBeNull();
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("bootstraps one browser identity and binds passkey options to it", async () => {
    const first = await RAW_SELF.fetch("https://example.test/v1/me");
    expect(first.status).toBe(200);
    const cookie = first.headers.get("set-cookie");
    expect(cookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43};/);
    const account = await first.json<{
      user: { id: string; persistent: boolean };
      authentication: string;
    }>();
    expect(account).toMatchObject({
      user: { persistent: false },
      authentication: "account_session",
    });

    const cookieHeader = cookie!.split(";", 1)[0]!;
    const repeated = await RAW_SELF.fetch("https://example.test/v1/me", {
      headers: { cookie: cookieHeader },
    });
    expect((await repeated.json<{ user: { id: string } }>()).user.id).toBe(account.user.id);
    expect(repeated.headers.get("set-cookie")).toBeNull();

    const options = await RAW_SELF.fetch("https://example.test/webauthn/register/options", {
      method: "POST",
      headers: {
        cookie: cookieHeader,
        "content-type": "application/json",
        origin: "https://example.test",
      },
      body: JSON.stringify({ name: "attacker", userId: crypto.randomUUID() }),
    });
    expect(options.status).toBe(200);
    const creation = await options.json<{ options: { publicKey?: { user: { id: string } } } }>();
    const encodedUserId = creation.options.publicKey?.user.id;
    expect(encodedUserId).toBeTruthy();
    const base64UserId = encodedUserId!.replaceAll("-", "+").replaceAll("_", "/");
    const decodedUserId = new TextDecoder().decode(
      Uint8Array.from(atob(base64UserId.padEnd(Math.ceil(base64UserId.length / 4) * 4, "=")),
        (character) => character.charCodeAt(0)),
    );
    expect(decodedUserId).toBe(account.user.id);
  });

  it("links a Tempo account to one persistent Nanocodex profile through a one-time code", async () => {
    const userId = "66666666-6666-4666-8666-666666666666";
    const sessionToken = "l".repeat(43);
    const cookie = `nanocodex_account=${sessionToken}`;
    const accountAddress = "0x1111111111111111111111111111111111111111";
    const state = "s".repeat(43);
    await seedPasskeySession(userId, sessionToken);

    const authorize = new URL("https://example.test/v1/connect/account-link");
    authorize.searchParams.set("account_address", accountAddress);
    authorize.searchParams.set("app_id", "atlas-workspace");
    authorize.searchParams.set("return_origin", "https://nanocodex.gakonst.workers.dev");
    authorize.searchParams.set("state", state);

    const unauthenticated = await RAW_SELF.fetch(authorize);
    expect(unauthenticated.status).toBe(401);
    expect(await unauthenticated.text()).not.toContain(userId);

    const confirmation = await RAW_SELF.fetch(authorize, { headers: { cookie } });
    expect(confirmation.status).toBe(200);
    expect(confirmation.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    expect(confirmation.headers.get("referrer-policy")).toBe("same-origin");
    const confirmationHtml = await confirmation.text();
    expect(confirmationHtml).toContain("Use Nanocodex profile");
    expect(confirmationHtml).not.toContain(userId);
    const intent = confirmationHtml.match(/name="intent" value="([A-Za-z0-9_-]{43})"/)?.[1];
    expect(intent).toBeTruthy();

    const crossOrigin = await RAW_SELF.fetch(authorize, {
      method: "POST",
      headers: { cookie, origin: "https://attacker.test" },
      body: new URLSearchParams({ intent: intent! }),
    });
    expect(crossOrigin.status).toBe(403);

    const authorized = await RAW_SELF.fetch(authorize, {
      method: "POST",
      headers: {
        cookie,
        origin: "https://example.test",
        "content-type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({ intent: intent! }),
    });
    expect(authorized.status).toBe(200);
    const authorizedHtml = await authorized.text();
    expect(authorizedHtml).not.toContain(userId);
    const code = authorizedHtml.match(/"code":"([A-Za-z0-9_-]{43})"/)?.[1];
    expect(code).toBeTruthy();

    const wrongContext = await RAW_SELF.fetch(
      "https://nanocodex.internal/connect/account-links/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: "atlas-workspace",
          code,
          state: "w".repeat(43),
        }),
      },
    );
    expect(wrongContext.status).toBe(403);

    const exchange = await RAW_SELF.fetch(
      "https://nanocodex.internal/connect/account-links/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: "atlas-workspace",
          code,
          state,
        }),
      },
    );
    expect(exchange.status).toBe(200);
    expect(await exchange.json()).toEqual({ linked: true, user_id: userId });

    const resolve = await RAW_SELF.fetch(
      `https://nanocodex.internal/connect/account-links/resolve?account_address=${accountAddress}`,
    );
    expect(await resolve.json()).toEqual({ linked: true, user_id: userId });

    const replay = await RAW_SELF.fetch(
      "https://nanocodex.internal/connect/account-links/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: "atlas-workspace",
          code,
          state,
        }),
      },
    );
    expect(replay.status).toBe(403);

    const unlinked = await RAW_SELF.fetch(
      `https://example.test/v1/connect/account-links/${accountAddress}`,
      { method: "DELETE", headers: { cookie, origin: "https://example.test" } },
    );
    expect(unlinked.status).toBe(204);
    const missing = await RAW_SELF.fetch(
      `https://nanocodex.internal/connect/account-links/resolve?account_address=${accountAddress}`,
    );
    expect(missing.status).toBe(404);
  });

  it("authorizes the signed Connect account from its existing first-party session", async () => {
    const userId = "77777777-7777-4777-8777-777777777777";
    const sessionToken = "m".repeat(43);
    const cookie = `nanocodex_account=${sessionToken}`;
    const accountAddress = "0x2222222222222222222222222222222222222222";
    const state = "t".repeat(43);
    await seedPasskeySession(userId, sessionToken);

    const authorize = new URL("https://example.test/v1/connect/account-link/authorize");
    authorize.searchParams.set("account_address", accountAddress);
    authorize.searchParams.set("app_id", "atlas-workspace");
    authorize.searchParams.set("return_origin", "https://nanocodex.gakonst.workers.dev");
    authorize.searchParams.set("state", state);

    expect((await RAW_SELF.fetch(authorize, { method: "POST" })).status).toBe(401);
    expect((await RAW_SELF.fetch(authorize, {
      method: "POST",
      headers: { cookie, origin: "https://attacker.test" },
    })).status).toBe(403);

    const authorized = await RAW_SELF.fetch(authorize, {
      method: "POST",
      headers: { cookie, origin: "https://example.test" },
    });
    expect(authorized.status).toBe(200);
    const body = await authorized.json<{ code: string; state: string }>();
    expect(body.state).toBe(state);
    expect(body.code).toMatch(/^[A-Za-z0-9_-]{43}$/);

    const exchange = await RAW_SELF.fetch(
      "https://nanocodex.internal/connect/account-links/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: "atlas-workspace",
          code: body.code,
          state,
        }),
      },
    );
    expect(exchange.status).toBe(200);
    expect(await exchange.json()).toEqual({ linked: true, user_id: userId });
  });

  it("issues and atomically exchanges a hosted authorization derived from the active passkey", async () => {
    const userId = "88888888-8888-4888-8888-888888888888";
    const otherUserId = "99999999-9999-4999-8999-999999999999";
    const token = "h".repeat(43);
    const otherToken = "j".repeat(43);
    const cookie = `nanocodex_account=${token}`;
    const resources = [
      "urn:nanocodex:capability:agent.run",
      "urn:nanocodex:connectors:github",
    ];
    await seedPasskeySession(userId, token, HOSTED_PASSKEY_PUBLIC_KEY);

    const authorize = (cookieHeader: string, body: Record<string, unknown>) => RAW_SELF.fetch(
      "https://example.test/v1/connect/hosted-authorization/authorize",
      {
        method: "POST",
        headers: {
          cookie: cookieHeader,
          "content-type": "application/json",
          origin: "https://example.test",
        },
        body: JSON.stringify(body),
      },
    );
    const authorizationBody = {
      account_address: HOSTED_PASSKEY_ADDRESS.toUpperCase().replace("0X", "0x"),
      app_id: "nanocodex-cli",
      app_origin: "https://cli.nanocodex.xyz",
      resources,
    };

    const unauthenticated = await authorize("", authorizationBody);
    expect(unauthenticated.status).toBe(401);

    const crossOrigin = await RAW_SELF.fetch(
      "https://example.test/v1/connect/hosted-authorization/authorize",
      {
        method: "POST",
        headers: { cookie, "content-type": "application/json", origin: "https://attacker.test" },
        body: JSON.stringify(authorizationBody),
      },
    );
    expect(crossOrigin.status).toBe(403);

    const wrongAddress = await authorize(cookie, {
      ...authorizationBody,
      account_address: "0x1111111111111111111111111111111111111111",
    });
    expect(wrongAddress.status).toBe(403);
    expect(await wrongAddress.json()).toEqual({ error: "account_address_mismatch" });

    const first = await authorize(cookie, authorizationBody);
    expect(first.status).toBe(200);
    const firstBody = await first.json<{ code: string }>();
    expect(firstBody).toEqual({ code: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/) });

    const exchange = (code: string, requestedResources = resources) => RAW_SELF.fetch(
      "https://nanocodex.internal/connect/hosted-authorizations/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: HOSTED_PASSKEY_ADDRESS,
          app_id: "nanocodex-cli",
          app_origin: "https://cli.nanocodex.xyz",
          code,
          resources: requestedResources,
        }),
      },
    );

    const mismatched = await exchange(firstBody.code, [...resources].reverse());
    expect(mismatched.status).toBe(403);
    expect((await exchange(firstBody.code)).status).toBe(403);

    const secondBody = await (await authorize(cookie, authorizationBody)).json<{ code: string }>();
    const linked = await exchange(secondBody.code);
    expect(linked.status).toBe(200);
    expect(await linked.json()).toEqual({
      linked: true,
      user_id: userId,
      account_address: HOSTED_PASSKEY_ADDRESS,
      resources,
    });
    expect((await exchange(secondBody.code)).status).toBe(403);

    const reuseBody = await (await authorize(cookie, authorizationBody)).json<{ code: string }>();
    expect((await exchange(reuseBody.code)).status).toBe(200);

    await seedPasskeySession(otherUserId, otherToken, HOSTED_PASSKEY_PUBLIC_KEY);
    const conflicting = await authorize(`nanocodex_account=${otherToken}`, authorizationBody);
    const conflictingBody = await conflicting.json<{ code: string }>();
    expect((await exchange(conflictingBody.code)).status).toBe(409);
  });

  it("strictly bounds hosted authorization authority and rejects MPP", async () => {
    const userId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const token = "i".repeat(43);
    const cookie = `nanocodex_account=${token}`;
    await seedPasskeySession(userId, token, HOSTED_PASSKEY_PUBLIC_KEY);
    const authorize = (body: unknown) => RAW_SELF.fetch(
     …33211 tokens truncated…Env.NANOCODEX.fetch("https://broker.internal/test/hold-subject-bind", {
      method: "POST",
    });

    try {
      await submit(agent, "turn-delete-during-create", "construction must stay fenced");
      let held: { binds: number; responses: number; subject?: string; unbinds: number } = {
        binds: 0,
        responses: 0,
        unbinds: 0,
      };
      while (!held.subject) {
        held = await (await testEnv.NANOCODEX.fetch(
          "https://broker.internal/test/hold-subject-bind",
        )).json<typeof held>();
        if (!held.subject) await scheduler.wait(1);
      }
      expect(held.binds).toBe(1);
      const deletion = SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
      });
      await waitForCleanupDeletion(session);
      await testEnv.NANOCODEX.fetch("https://broker.internal/test/hold-subject-bind", {
        method: "DELETE",
      });
      expect((await within(deletion, "delete superseded construction")).status).toBe(204);
      createdAgents.delete(agent.agent_id);
      held = await (await testEnv.NANOCODEX.fetch(
        "https://broker.internal/test/hold-subject-bind",
      )).json<typeof held>();
      expect(held.responses).toBe(0);
    } finally {
      await testEnv.NANOCODEX.fetch("https://broker.internal/test/hold-subject-bind", {
        method: "DELETE",
      });
    }
  });

  it("does not publish a construction that resolves after a newer deletion generation", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalTimeout = testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS;
    let releaseConstruction!: () => void;
    const heldConstruction = new Promise<void>((resolve) => { releaseConstruction = resolve; });
    let constructionStarted!: () => void;
    const started = new Promise<void>((resolve) => { constructionStarted = resolve; });
    let restoreWorkspace = () => {};
    await runInDurableObject(session, (instance) => {
      const workspaceSymbol = Object.getOwnPropertySymbols(instance)
        .find((symbol) => symbol.description === "workspace");
      expect(workspaceSymbol).toBeTruthy();
      const workspace = (instance as unknown as Record<symbol, {
        fs: { lstat(path: string): Promise<unknown> };
      }>)[workspaceSymbol!];
      const originalLstat = workspace.fs.lstat;
      let held = false;
      workspace.fs.lstat = async (path: string) => {
        if (!held && path === "/workspace") {
          held = true;
          constructionStarted();
          await heldConstruction;
        }
        return originalLstat.call(workspace.fs, path);
      };
      restoreWorkspace = () => { workspace.fs.lstat = originalLstat; };
    });
    testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = "20";

    try {
      await submit(agent, "turn-late-construction", "never publish this runtime");
      await within(started, "held agent construction");

      const firstDeletion = await SELF.fetch(
        `https://example.test/v1/agents/${agent.agent_id}`,
        { method: "DELETE" },
      );
      expect(firstDeletion.status).toBe(503);
      expect(await firstDeletion.json()).toEqual({ error: "session_cleanup_pending" });
      expect(await cleanupMarkers(session)).toEqual({ binding: true, deleting: true });

      await runCleanupAlarmsUntilDeleted(session);
      createdAgents.delete(agent.agent_id);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await runInDurableObject(session, (_instance, state) => ({
        events: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_events",
        ).toArray()[0]!.count,
        session: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM session_state",
        ).toArray()[0]!.count,
        turns: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_turns",
        ).toArray()[0]!.count,
      }))).toEqual({ events: 0, session: 0, turns: 0 });

      releaseConstruction();
      restoreWorkspace();
      await scheduler.wait(100);
      expect(await runInDurableObject(session, (_instance, state) => ({
        events: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_events",
        ).toArray()[0]!.count,
        session: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM session_state",
        ).toArray()[0]!.count,
        turns: state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_turns",
        ).toArray()[0]!.count,
      }))).toEqual({ events: 0, session: 0, turns: 0 });

      const rejected = await session.fetch("https://session.internal/turns", {
        method: "POST",
        headers: { "content-type": "application/json", "idempotency-key": "after-deletion" },
        body: JSON.stringify({ id: "after-deletion", input: "must not be accepted" }),
      });
      expect(rejected.status).toBe(409);
      expect(await rejected.json()).toMatchObject({ error: "agent_deleting" });
    } finally {
      releaseConstruction();
      restoreWorkspace();
      testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = originalTimeout;
    }
  });

  it("retries a pre-admission HTTP 503 through a concurrent alarm without false failure", async () => {
    const agent = await createAgent();
    const id = "turn-pre-admission-503";
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const request = {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${id}`,
      },
      body: JSON.stringify({ id, input: "retry after startup failure" }),
    } satisfies RequestInit;
    const originalBroker = testEnv.NANOCODEX;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          return Response.json({ error: "injected_startup_failure" }, { status: 503 });
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;
    try {
      expect((await SELF.fetch(turnsUrl, request)).status).toBe(202);
      const retryable = await waitForTurnState(agent, id, "retryable");
      expect(retryable.error).toMatch(/HTTP 503/i);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }

    const sessionId = new URL(agent.events_url).pathname.split("/").at(-2)!;
    const stub = testEnv.NANOCODEX_SESSIONS.getByName(sessionId);
    await within(waitForScheduledAlarm(stub), "retry alarm scheduling");
    const [alarm, replay] = await Promise.all([
      runDurableObjectAlarm(stub),
      SELF.fetch(turnsUrl, request),
    ]);
    expect(alarm).toBe(true);
    expect(replay.status).toBe(200);
    await waitForTurnState(agent, id, "completed");

    const history = await (
      await SELF.fetch(`${agent.events_url}/history?limit=256`)
    ).json<{ data: Array<{ type: string; turn_id?: string }> }>();
    expect(history.data.filter(({ type, turn_id }) => (
      turn_id === id && type === "turn_accepted"
    ))).toHaveLength(1);
    expect(history.data.filter(({ type, turn_id }) => (
      turn_id === id && type === "turn_retryable"
    ))).toHaveLength(1);
    expect(history.data.filter(({ type, turn_id }) => (
      turn_id === id && type === "turn_completed"
    ))).toHaveLength(1);
    expect(history.data.some(({ type, turn_id }) => (
      turn_id === id && (type === "turn_failed" || type === "turn_blocked")
    ))).toBe(false);
  });

  it("cancels a pre-runtime retry without reconstructing the unavailable agent", async () => {
    const agent = await createAgent();
    const id = "turn-cancel-before-runtime";
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const turnRequest = {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${id}`,
      },
      body: JSON.stringify({ id, input: "cancel through a reconstruction outage" }),
    } satisfies RequestInit;
    const cancelUrl = `${turnsUrl}/${id}/cancel`;
    const originalBroker = testEnv.NANOCODEX;
    let upgradeCount = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          upgradeCount += 1;
          return Response.json({ error: "injected_cancellation_reconstruction_outage" }, { status: 503 });
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      expect((await SELF.fetch(turnsUrl, turnRequest)).status).toBe(202);
      const retryable = await waitForTurnAttempt(agent, id, 1);
      expect(retryable.state).toBe("retryable");
      expect(retryable.error).toMatch(/HTTP 503/i);
      const upgradesBeforeCancel = upgradeCount;

      expect((await SELF.fetch(cancelUrl, { method: "POST" })).status).toBe(202);
      expect((await SELF.fetch(turnsUrl, turnRequest)).status).toBe(200);
      expect((await SELF.fetch(cancelUrl, { method: "POST" })).status).toBe(200);
      const cancelled = await waitForTurnState(agent, id, "cancelled");
      expect(cancelled).toMatchObject({
        attempt_count: 1,
        retry_at: null,
        state: "cancelled",
      });
      await scheduler.wait(25);
      expect(upgradeCount).toBe(upgradesBeforeCancel);

      const replay = await (
        await SELF.fetch(`${turnsUrl}/${id}`)
      ).json<ManagedTurnView>();
      expect(replay).toMatchObject({
        attempt_count: 1,
        retry_at: null,
        state: "cancelled",
      });
      const history = await managedHistory(agent);
      expect(history.data.filter(({ type, turn_id }) => (
        type === "turn_accepted" && turn_id === id
      ))).toHaveLength(1);
      expect(history.data.filter(({ type, turn_id }) => (
        type === "turn_retryable" && turn_id === id
      ))).toHaveLength(1);
      expect(history.data.filter(({ type, turn_id }) => (
        type === "turn_cancelling" && turn_id === id
      ))).toHaveLength(1);
      expect(history.data.filter(({ type, turn_id }) => (
        type === "turn_cancelled" && turn_id === id
      ))).toHaveLength(1);
      expect(history.data.some(({ type, turn_id }) => (
        turn_id === id && (type === "turn_failed" || type === "turn_blocked")
      ))).toBe(false);
      expect(await runInDurableObject(
        testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id),
        (_instance, state) => state.storage.sql.exec<{
          may_have_inner_operation: number;
          state: string;
        }>(
          "SELECT may_have_inner_operation, state FROM managed_turns WHERE id = ?",
          id,
        ).one(),
      )).toEqual({
        may_have_inner_operation: 0,
        state: "cancelled",
      });
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("joins an in-flight construction before cancelling it as outer-only", async () => {
    const agent = await createAgent();
    const id = "turn-cancel-during-construction";
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const originalBroker = testEnv.NANOCODEX;
    let releaseConstruction!: () => void;
    const heldConstruction = new Promise<Response>((resolve) => {
      releaseConstruction = () => resolve(Response.json(
        { error: "injected_held_startup_failure" },
        { status: 503 },
      ));
    });
    let constructionStarted!: () => void;
    const started = new Promise<void>((resolve) => { constructionStarted = resolve; });
    let upgradeCount = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          upgradeCount += 1;
          constructionStarted();
          return heldConstruction;
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      expect((await SELF.fetch(turnsUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "idempotency-key": `request-${id}`,
        },
        body: JSON.stringify({ id, input: "cancel while construction is held" }),
      })).status).toBe(202);
      await within(started, "held cancellation construction");

      expect((await SELF.fetch(`${turnsUrl}/${id}/cancel`, {
        method: "POST",
      })).status).toBe(202);
      expect(await (
        await SELF.fetch(`${turnsUrl}/${id}`)
      ).json<ManagedTurnView>()).toMatchObject({ state: "cancelling" });

      releaseConstruction();
      const cancelled = await waitForTurnState(agent, id, "cancelled");
      expect(cancelled).toMatchObject({
        retry_at: null,
        state: "cancelled",
      });
      expect(upgradeCount).toBe(1);
      expect(await runInDurableObject(
        testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id),
        (_instance, state) => state.storage.sql.exec<{ may_have_inner_operation: number }>(
          "SELECT may_have_inner_operation FROM managed_turns WHERE id = ?",
          id,
        ).one().may_have_inner_operation,
      )).toBe(0);
    } finally {
      releaseConstruction();
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("retains the cancellation retry deadline for a dispatched turn", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(Date.now() + 60_000);
    const agent = await createAgent();
    const id = "turn-cancel-after-dispatch";
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    let restoreJournal = () => {};

    try {
      expect((await SELF.fetch(turnsUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "idempotency-key": `request-${id}`,
        },
        body: JSON.stringify({ id, input: "wait for cancellation" }),
      })).status).toBe(202);
      for (let attempt = 0; attempt < 100; attempt += 1) {
        const dispatched = await runInDurableObject(session, (_instance, state) => (
          state.storage.sql.exec<{ may_have_inner_operation: number }>(
            "SELECT may_have_inner_operation FROM managed_turns WHERE id = ?",
            id,
          ).one().may_have_inner_operation
        ));
        if (dispatched === 1) break;
        await scheduler.wait(5);
      }
      expect(await runInDurableObject(session, (_instance, state) => (
        state.storage.sql.exec<{ may_have_inner_operation: number }>(
          "SELECT may_have_inner_operation FROM managed_turns WHERE id = ?",
          id,
        ).one().may_have_inner_operation
      ))).toBe(1);

      await runInDurableObject(session, (_instance, state) => {
        const sql = state.storage.sql as unknown as {
          exec(query: string, ...bindings: unknown[]): unknown;
        };
        const originalExec = sql.exec;
        let failed = false;
        sql.exec = function injectedJournalFailure(query, ...bindings) {
          if (!failed
            && query.includes("INSERT INTO nanocodex_journal_batches")
            && bindings.some((binding) => (
              typeof binding === "string" && binding.includes("\"operation_cancelled\"")
            ))) {
            failed = true;
            throw new Error("injected cancellation journal failure");
          }
          return originalExec.call(sql, query, ...bindings);
        };
        restoreJournal = () => { sql.exec = originalExec; };
      });

      expect((await SELF.fetch(`${turnsUrl}/${id}/cancel`, {
        method: "POST",
      })).status).toBe(202);
      const cancelling = await waitForTurnAttempt(agent, id, 1);
      expect(cancelling).toMatchObject({ state: "cancelling" });
      expect(cancelling.retry_at).not.toBeNull();
      const retryAt = cancelling.retry_at!;
      restoreJournal();

      for (let attempt = 0; attempt < 100; attempt += 1) {
        const alarm = await runInDurableObject(
          session,
          (_instance, state) => state.storage.getAlarm(),
        );
        if (alarm === retryAt) break;
        await scheduler.wait(5);
      }
      expect(await runInDurableObject(session, (_instance, state) => state.storage.getAlarm()))
        .toBe(retryAt);
      expect((await SELF.fetch(turnsUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "idempotency-key": "request-turn-after-cancel-retry",
        },
        body: JSON.stringify({
          id: "turn-after-cancel-retry",
          input: "run only after cancellation",
        }),
      })).status).toBe(202);
      await scheduler.wait(25);
      expect(await runInDurableObject(session, (_instance, state) => state.storage.getAlarm()))
        .toBe(retryAt);
      expect(await (
        await SELF.fetch(`${turnsUrl}/turn-after-cancel-retry`)
      ).json<ManagedTurnView>()).toMatchObject({ attempt_count: 0, state: "accepted" });

      vi.setSystemTime(retryAt);
      expect(await runDurableObjectAlarm(session)).toBe(true);
      await scheduler.wait(250);
      expect(await (
        await SELF.fetch(`${turnsUrl}/${id}`)
      ).json<ManagedTurnView>()).toMatchObject({ state: "cancelled" });
      await waitForTurnState(agent, "turn-after-cancel-retry", "completed");
    } finally {
      restoreJournal();
      errorSpy.mockRestore();
      vi.useRealTimers();
    }
  });

  it("does not dispatch a newer turn past a pre-runtime retry", async () => {
    const agent = await createAgent();
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const originalBroker = testEnv.NANOCODEX;
    let upgradeCount = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          upgradeCount += 1;
          return Response.json({ error: "injected_fifo_startup_failure" }, { status: 503 });
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    const submit = (id: string) => SELF.fetch(turnsUrl, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${id}`,
      },
      body: JSON.stringify({ id, input: `input for ${id}` }),
    });

    try {
      expect((await submit("turn-fifo-first")).status).toBe(202);
      await waitForTurnState(agent, "turn-fifo-first", "retryable");
      expect(upgradeCount).toBe(1);

      expect((await submit("turn-fifo-second")).status).toBe(202);
      await scheduler.wait(50);
      expect(upgradeCount).toBe(1);
      expect(await (
        await SELF.fetch(`${turnsUrl}/turn-fifo-second`)
      ).json<ManagedTurnView>()).toMatchObject({
        attempt_count: 0,
        state: "accepted",
      });

      testEnv.NANOCODEX = originalBroker;
      expect((await SELF.fetch(`${turnsUrl}/turn-fifo-first/cancel`, {
        method: "POST",
      })).status).toBe(202);
      await waitForTurnState(agent, "turn-fifo-first", "cancelled");
      await waitForTurnState(agent, "turn-fifo-second", "completed");

      const history = await managedHistory(agent);
      expect(history.data.some(({ type, turn_id }) => (
        type === "turn_blocked"
        && (turn_id === "turn-fifo-first" || turn_id === "turn-fifo-second")
      ))).toBe(false);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("conservatively marks legacy unfinished turns as possibly dispatched", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    await runInDurableObject(session, (_instance, state) => {
      state.storage.sql.exec("DROP TABLE managed_turns");
      state.storage.sql.exec(`
        CREATE TABLE managed_turns (
          id TEXT PRIMARY KEY,
          request_key TEXT,
          request_hash TEXT NOT NULL,
          input_json TEXT NOT NULL,
          state TEXT NOT NULL CHECK (
            state IN ('accepted', 'cancelling', 'retryable', 'blocked', 'completed', 'cancelled', 'failed')
          ),
          accepted_cursor INTEGER NOT NULL,
          terminal_json TEXT,
          terminal_cursor INTEGER,
          error TEXT,
          attempt_count INTEGER NOT NULL DEFAULT 0,
          retry_at INTEGER,
          created_at INTEGER NOT NULL,
          accepted_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL
        )
      `);
      state.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_key, request_hash, input_json, state, accepted_cursor,
           error, created_at, accepted_at, updated_at
         ) VALUES (?, NULL, ?, ?, 'blocked', 0, ?, ?, ?, ?)`,
        "legacy-unfinished-turn",
        "legacy-request-hash",
        JSON.stringify("legacy input"),
        "legacy row requires conservative reconciliation",
        Date.now(),
        Date.now(),
        Date.now(),
      );
    });

    await evictDurableObject(session);
    expect((await SELF.fetch(`${turnsUrl}/legacy-unfinished-turn`)).status).toBe(200);
    expect(await runInDurableObject(session, (_instance, state) => ({
      dispatchColumn: state.storage.sql.exec<{ name: string }>(
        "PRAGMA table_info(managed_turns)",
      ).toArray().some(({ name }) => name === "dispatch_input_chunks"),
      dispatchTable: state.storage.sql.exec<{ count: number }>(
        `SELECT COUNT(*) AS count FROM sqlite_master
         WHERE type = 'table' AND name = 'managed_turn_dispatch_chunks'`,
      ).one().count,
      ownershipColumn: state.storage.sql.exec<{ name: string }>(
        "PRAGMA table_info(managed_turns)",
      ).toArray().some(({ name }) => name === "may_have_inner_operation"),
      marker: state.storage.sql.exec<{ may_have_inner_operation: number }>(
        "SELECT may_have_inner_operation FROM managed_turns WHERE id = 'legacy-unfinished-turn'",
      ).one().may_have_inner_operation,
    }))).toEqual({
      dispatchColumn: true,
      dispatchTable: 1,
      marker: 1,
      ownershipColumn: true,
    });
  });

  it("quarantines legacy realtime rows whose managed and durable identities diverged", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const legacyId = `realtime:${"a".repeat(48)}`;
    await runInDurableObject(session, (_instance, state) => {
      state.storage.sql.exec("DROP TABLE managed_turns");
      state.storage.sql.exec(`
        CREATE TABLE managed_turns (
          id TEXT PRIMARY KEY,
          request_key TEXT,
          request_hash TEXT NOT NULL,
          input_json TEXT NOT NULL,
          authorization_json TEXT NOT NULL,
          state TEXT NOT NULL CHECK (
            state IN ('accepted', 'cancelling', 'retryable', 'blocked', 'completed', 'cancelled', 'failed')
          ),
          accepted_cursor INTEGER NOT NULL,
          terminal_json TEXT,
          terminal_cursor INTEGER,
          error TEXT,
          may_have_inner_operation INTEGER NOT NULL DEFAULT 1,
          attempt_count INTEGER NOT NULL DEFAULT 0,
          retry_at INTEGER,
          created_at INTEGER NOT NULL,
          accepted_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL
        )
      `);
      state.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_key, request_hash, input_json, authorization_json, state,
           accepted_cursor, created_at, accepted_at, updated_at
         ) VALUES (?, 'realtime:legacy-voice:legacy-operation', ?, ?, '{}',
                   'accepted', 0, ?, ?, ?)`,
        legacyId,
        "legacy-request-hash",
        JSON.stringify("legacy routed input"),
        Date.now(),
        Date.now(),
        Date.now(),
      );
    });

    await evictDurableObject(session);
    expect(await runInDurableObject(session, (_instance, state) => ({
      journalRows: state.storage.sql.exec<{ count: number }>(
        `SELECT COUNT(*) AS count FROM sqlite_master
         WHERE type = 'table' AND name = 'nanocodex_journals'`,
      ).one().count,
      row: state.storage.sql.exec<{ error: string; state: string }>(
        "SELECT state, error FROM managed_turns WHERE id = ?",
        legacyId,
      ).one(),
    }))).toEqual({
      journalRows: 0,
      row: {
        error: "pre-upgrade realtime turn has an indeterminate durable operation identity",
        state: "blocked",
      },
    });
  }, 30_000);

  it("does not let an idempotent submission bypass a durable admission retry deadline", async () => {
    const agent = await createAgent();
    const id = "turn-admission-retry-deadline";
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const request = {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${id}`,
      },
      body: JSON.stringify({ id, input: "respect the retained admission deadline" }),
    } satisfies RequestInit;
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalBroker = testEnv.NANOCODEX;
    let upgradeCount = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          upgradeCount += 1;
          return Response.json({ error: "injected_admission_outage" }, { status: 503 });
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      expect((await SELF.fetch(turnsUrl, request)).status).toBe(202);
      const retryable = await waitForTurnAttempt(agent, id, 1);
      expect(retryable.state).toBe("retryable");
      expect(retryable.retry_at).not.toBeNull();
      await waitForScheduledAlarm(session);
      const retryAt = await runInDurableObject(session, async (_instance, state) => {
        const deadline = Date.now() + 60_000;
        state.storage.sql.exec(
          "UPDATE managed_turns SET retry_at = ? WHERE id = ?",
          deadline,
          id,
        );
        await state.storage.setAlarm(deadline);
        return deadline;
      });
      const upgradesBeforeDeadline = upgradeCount;

      expect((await SELF.fetch(turnsUrl, request)).status).toBe(200);
      expect(await runDurableObjectAlarm(session)).toBe(true);
      await scheduler.wait(25);
      const beforeDeadline = await (
        await SELF.fetch(`${turnsUrl}/${id}`)
      ).json<ManagedTurnView>();
      expect(beforeDeadline).toMatchObject({
        attempt_count: 1,
        retry_at: retryAt,
        state: "retryable",
      });
      expect(upgradeCount).toBe(upgradesBeforeDeadline);
      expect(await runInDurableObject(session, (_instance, state) => state.storage.getAlarm()))
        .toBe(retryAt);

      await runInDurableObject(session, async (_instance, state) => {
        const due = Date.now() - 1;
        state.storage.sql.exec(
          "UPDATE managed_turns SET retry_at = ? WHERE id = ?",
          due,
          id,
        );
        await state.storage.setAlarm(Date.now() + 60_000);
      });
      expect(await runDurableObjectAlarm(session)).toBe(true);
      const retried = await waitForTurnAttempt(agent, id, 2);
      expect(retried.state).toBe("retryable");
      expect(upgradeCount).toBe(upgradesBeforeDeadline + 1);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("keeps infrastructure failures retryable after the former eight-attempt ceiling", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const agent = await createAgent();
    const id = "turn-many-infra-retries";
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalBroker = testEnv.NANOCODEX;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          return Response.json({ error: "injected_persistent_outage" }, { status: 503 });
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      await submit(agent, id, "remain retryable under capped backoff");
      let turn = await waitForTurnAttempt(agent, id, 1);
      for (let expected = 2; expected <= 10; expected += 1) {
        expect(turn.state).toBe("retryable");
        expect(turn.retry_at).not.toBeNull();
        expect(turn.retry_at! - turn.updated_at).toBeLessThanOrEqual(60_000);
        // The alarm owner fences scheduled work at `now + 1`, so advance one
        // millisecond past the durable retry boundary before firing it.
        vi.setSystemTime(turn.retry_at! + 1);
        expect(await runDurableObjectAlarm(session)).toBe(true);
        turn = await waitForTurnAttempt(agent, id, expected);
      }
      expect(turn).toMatchObject({ attempt_count: 10, state: "retryable" });
      expect(turn.error).not.toMatch(/retry limit reached/i);
      const history = await managedHistory(agent);
      expect(history.data.filter(({ type, turn_id }) => (
        type === "turn_retryable" && turn_id === id
      ))).toHaveLength(1);
    } finally {
      testEnv.NANOCODEX = originalBroker;
      vi.useRealTimers();
    }
  });

  it("keeps accepted siblings retryable when a fenced turn requires Agent reopen", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    await submit(agent, "turn-reopen-owner", "force owner fencing after provider admission");
    await new Promise((resolve) => setTimeout(resolve, 100));
    await submit(agent, "turn-reopen-sibling", "remain retryable across sibling reopen");
    await runInDurableObject(session, (_instance, state) => {
      state.storage.sql.exec(
        `UPDATE nanocodex_journal_owners
         SET owner_id = 'injected-new-owner', fence = CAST(fence AS INTEGER) + 1`,
      );
    });

    await waitForHistoryEvent(agent, ({ type, turn_id }) => (
      type === "turn_retryable" && turn_id === "turn-reopen-sibling"
    ));
    const history = await managedHistory(agent);
    expect(history.data.some(({ type, turn_id }) => (
      type === "turn_cancelled" && turn_id === "turn-reopen-sibling"
    ))).toBe(false);
  });

  it("attributes replay-only raw terminals before the following turn can start", async () => {
    const agent = await createAgent();
    const replayId = "turn-raw-replay";
    await submit(agent, replayId, "retain this exact completed operation");
    await waitForTurnState(agent, replayId, "completed");
    await waitForHistoryEvent(agent, ({ event, turn_id }) => (
      event?.type === "run.completed" && turn_id === replayId
    ));
    const beforeReplay = BigInt((await managedHistory(agent)).latest_cursor);
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const frozen = await runInDurableObject(session, (_instance, state) => ({
      completedTurns: state.storage.sql.exec<{ completed_turns: number }>(
        "SELECT completed_turns FROM session_state WHERE singleton = 1",
      ).one().completed_turns,
      dispatchInputJson: state.storage.sql.exec<{ input_json: string }>(
        `SELECT input_json FROM managed_turn_dispatch_chunks
         WHERE turn_id = ? ORDER BY chunk_index`,
        replayId,
      ).toArray().map(({ input_json }) => input_json).join(""),
      journalRevision: state.storage.sql.exec<{ revision: string }>(
        "SELECT revision FROM nanocodex_journals",
      ).one().revision,
      terminalJson: state.storage.sql.exec<{ terminal_json: string }>(
        "SELECT terminal_json FROM managed_turns WHERE id = ?",
        replayId,
      ).one().terminal_json,
    }));
    expect(frozen.completedTurns).toBe(1);
    expect(frozen.dispatchInputJson).toContain("<account_info>");
    expect(frozen.dispatchInputJson).not.toContain("<memory_review_checkpoint>");
    await runInDurableObject(session, (_instance, state) => {
      state.storage.sql.exec(
        `UPDATE managed_turns
         SET state = 'retryable', terminal_json = NULL, terminal_cursor = NULL,
             error = 'injected outer projection gap', retry_at = 0
         WHERE id = ?`,
        replayId,
      );
    });
    await evictDurableObject(session);

    const replay = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${replayId}`,
      },
      body: JSON.stringify({ id: replayId, input: "retain this exact completed operation" }),
    });
    expect(replay.status).toBe(200);
    await waitForTurnState(agent, replayId, "completed");
    expect(await runInDurableObject(session, (_instance, state) => ({
      dispatchInputJson: state.storage.sql.exec<{ input_json: string }>(
        `SELECT input_json FROM managed_turn_dispatch_chunks
         WHERE turn_id = ? ORDER BY chunk_index`,
        replayId,
      ).toArray().map(({ input_json }) => input_json).join(""),
      journalRevision: state.storage.sql.exec<{ revision: string }>(
        "SELECT revision FROM nanocodex_journals",
      ).one().revision,
      terminalJson: state.storage.sql.exec<{ terminal_json: string }>(
        "SELECT terminal_json FROM managed_turns WHERE id = ?",
        replayId,
      ).one().terminal_json,
    }))).toEqual({
      dispatchInputJson: frozen.dispatchInputJson,
      journalRevision: frozen.journalRevision,
      terminalJson: frozen.terminalJson,
    });
    await submit(agent, "turn-after-raw-replay", "start only after replay attribution");
    await waitForTurnState(agent, "turn-after-raw-replay", "completed");
    await waitForHistoryEvent(agent, ({ cursor, event, turn_id }) => (
      BigInt(cursor) > beforeReplay
        && event?.type === "run.completed"
        && turn_id === "turn-after-raw-replay"
    ));

    const replayWindow = (await managedHistory(agent)).data.filter(({ cursor }) => (
      BigInt(cursor) > beforeReplay
    ));
    const replayCompleted = replayWindow.filter(({ event, turn_id }) => (
      event?.type === "run.completed" && turn_id === replayId
    ));
    expect(replayCompleted).toHaveLength(1);
    const followingStarted = replayWindow.find(({ event, turn_id }) => (
      event?.type === "run.started" && turn_id === "turn-after-raw-replay"
    ));
    const followingCompleted = replayWindow.find(({ event, turn_id }) => (
      event?.type === "run.completed" && turn_id === "turn-after-raw-replay"
    ));
    expect(followingStarted).toBeTruthy();
    expect(followingCompleted).toBeTruthy();
    expect(BigInt(replayCompleted[0]!.cursor)).toBeLessThan(BigInt(followingStarted!.cursor));
    expect(BigInt(followingStarted!.cursor)).toBeLessThan(BigInt(followingCompleted!.cursor));
  });

  it("recovers and freezes a pre-upgrade durable dispatch form", async () => {
    const agent = await createAgent();
    const id = "turn-legacy-dispatch-input";
    const input = "retain the pre-upgrade dispatch input";
    await submit(agent, id, input);
    await waitForTurnState(agent, id, "completed");
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const original = await runInDurableObject(session, (_instance, state) => (
      state.storage.sql.exec<{ input_json: string }>(
        `SELECT input_json FROM managed_turn_dispatch_chunks
         WHERE turn_id = ? ORDER BY chunk_index`,
        id,
      ).toArray().map(({ input_json }) => input_json).join("")
    ));
    expect(original).not.toContain("<memory_review_checkpoint>");

    await runInDurableObject(session, (_instance, state) => {
      state.storage.transactionSync(() => {
        state.storage.sql.exec(
          "DELETE FROM managed_turn_dispatch_chunks WHERE turn_id = ?",
          id,
        );
        state.storage.sql.exec(
          `UPDATE managed_turns
           SET dispatch_input_chunks = NULL, state = 'retryable', terminal_json = NULL,
               terminal_cursor = NULL, error = 'injected pre-upgrade projection gap', retry_at = 0
           WHERE id = ?`,
          id,
        );
      });
    });
    await evictDurableObject(session);

    const replay = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": `request-${id}`,
      },
      body: JSON.stringify({ id, input }),
    });
    expect(replay.status).toBe(200);
    await waitForTurnState(agent, id, "completed");
    expect(await runInDurableObject(session, (_instance, state) => (
      state.storage.sql.exec<{ input_json: string }>(
        `SELECT input_json FROM managed_turn_dispatch_chunks
         WHERE turn_id = ? ORDER BY chunk_index`,
        id,
      ).toArray().map(({ input_json }) => input_json).join("")
    ))).toBe(original);
  });

  it("persists cursors across eviction and tails strictly after the acknowledged cursor", async () => {
    const agent = await createAgent();
    const id = new URL(agent.events_url).pathname.split("/").at(-2)!;
    await within(
      evictDurableObject(testEnv.NANOCODEX_SESSIONS.getByName(id)),
      "durable object eviction",
    );

    const replay = sseReader(await SELF.fetch(`${agent.events_url}?cursor=0`));
    const restored = await nextWithin(replay, "post-eviction replay");
    expect(restored.data).toMatchObject({
      agent_id: agent.agent_id,
      cursor: restored.id,
      type: "agent_created",
    });
    await replay.cancel();

    const resumed = sseReader(await SELF.fetch(`${agent.events_url}?cursor=not-used`, {
      headers: { "last-event-id": restored.id },
    }));
    const first = await submit(agent, "turn-one", "one");
    let previous = BigInt(restored.id);
    let next;
    do {
      next = await nextWithin(resumed, "live tail");
      expect(BigInt(next.id)).toBeGreaterThan(previous);
      previous = BigInt(next.id);
    } while (next.data.type !== "turn_accepted" || next.data.id !== "turn-one");
    expect(next.id).toBe(first.accepted_cursor);
    await resumed.cancel();
  });

  it("replays multi-digit cursors in numeric rather than lexical order", async () => {
    const agent = await createAgent();
    await submit(agent, "ordered-turn", "produce a complete event lifecycle");
    const agentUrl = agent.events_url.replace(/\/events$/, "");
    let latest = 0n;
    for (let attempt = 0; attempt < 80 && latest < 12n; attempt += 1) {
      const state = await (await SELF.fetch(agentUrl)).json<{ latest_event_cursor: string }>();
      latest = BigInt(state.latest_event_cursor);
      if (latest < 12n) await new Promise((resolve) => setTimeout(resolve, 25));
    }
    expect(latest).toBeGreaterThanOrEqual(12n);

    const replay = sseReader(await SELF.fetch(`${agent.events_url}?cursor=0`));
    let previous = 0n;
    while (previous < latest) {
      const event = await nextWithin(replay, "numeric cursor replay");
      const cursor = BigInt(event.id);
      expect(cursor).toBeGreaterThan(previous);
      previous = cursor;
    }
    await replay.cancel();
  });

  it("pages a bounded recent event window and then strictly older history", async () => {
    const agent = await createAgent();
    for (let index = 0; index < 4; index += 1) {
      await submit(agent, `history-${index}`, `history prompt ${index}`);
    }

    const recentResponse = await SELF.fetch(`${agent.events_url}/history?limit=2`);
    expect(recentResponse.status).toBe(200);
    expect(recentResponse.headers.get("cache-control")).toContain("no-store");
    const recent = await recentResponse.json<{
      data: Array<{ cursor: string }>;
      has_more: boolean;
      latest_cursor: string;
    }>();
    expect(recent.data).toHaveLength(2);
    expect(recent.has_more).toBe(true);
    expect(recent.data.map((event) => BigInt(event.cursor))).toEqual(
      [...recent.data].map((event) => BigInt(event.cursor)).sort((a, b) => a < b ? -1 : 1),
    );
    expect(recent.latest_cursor).toBe(recent.data.at(-1)?.cursor);

    const before = recent.data[0]!.cursor;
    const older = await (await SELF.fetch(
      `${agent.events_url}/history?before=${before}&limit=2`,
    )).json<{ data: Array<{ cursor: string }> }>();
    expect(older.data).toHaveLength(2);
    expect(older.data.every((event) => BigInt(event.cursor) < BigInt(before))).toBe(true);
    expect(new Set([...older.data, ...recent.data].map((event) => event.cursor)).size).toBe(4);

    expect((await SELF.fetch(`${agent.events_url}/history?before=0&limit=2`)).status).toBe(400);
    expect((await SELF.fetch(`${agent.events_url}/history?limit=257`)).status).toBe(400);
  });

  it("bounds request bodies and clears managed state on deletion", async () => {
    const agent = await createAgent();
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    expect((await SELF.fetch(turnsUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{",
    })).status).toBe(400);
    expect((await SELF.fetch(turnsUrl, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "content-length": String(1024 * 1024 + 1),
      },
      body: "{}",
    })).status).toBe(413);

    await submit(agent, "turn-delete", "delete me");
    const id = new URL(agent.events_url).pathname.split("/").at(-2)!;
    const deleted = await SELF.fetch(`https://example.test/v1/agents/${id}`, { method: "DELETE" });
    expect(deleted.status).toBe(204);
    createdAgents.delete(id);
    expect((await SELF.fetch(`https://example.test/v1/agents/${id}`)).status).toBe(404);
    expect((await SELF.fetch(agent.events_url)).status).toBe(404);
  });

  it("bounds concurrent resumable event subscribers", async () => {
    const agent = await createAgent();
    const responses: Response[] = [];
    try {
      for (let index = 0; index < 32; index += 1) {
        const response = await SELF.fetch(`${agent.events_url}?cursor=0`);
        expect(response.status).toBe(200);
        responses.push(response);
      }
      const rejected = await SELF.fetch(`${agent.events_url}?cursor=0`);
      expect(rejected.status).toBe(429);
      expect(rejected.headers.get("retry-after")).toBe("1");
      expect(await rejected.json()).toEqual({ error: "event_stream_limit", limit: 32 });
    } finally {
      await Promise.all(responses.map((response) => response.body?.cancel()));
    }
  });
});

type AgentReceipt = {
  agent_id: string;
  events_url: string;
  websocket_url: string;
};

type ManagedTurnView = {
  accepted_cursor: string;
  attempt_count: number;
  error?: string;
  input: unknown;
  retry_at: number | null;
  state: string;
  terminal_cursor: string | null;
  turn_id: string;
  updated_at: number;
};

type ManagedHistoryEvent = {
  cursor: string;
  event?: { type?: string };
  turn_id?: string;
  type: string;
};

type ManagedHistory = {
  data: ManagedHistoryEvent[];
  has_more: boolean;
  latest_cursor: string;
};

type ManagedRealtimeLifecycleResponse = {
  context: { history: unknown[]; workspace: string };
  operation_id: string;
  stopped?: boolean;
  voice_session_id: string;
};

type ManagedRealtimeRouteResponse = {
  operation_id: string;
  route: "started" | "steered";
  turn_id: string;
  voice_session_id: string;
};

type HistorySearchBody = {
  query: string;
  results: Array<Record<string, unknown>>;
  citations: Array<Record<string, unknown>>;
};

async function createAgent(): Promise<AgentReceipt> {
  const response = await SELF.fetch("https://example.test/v1/agents", {
    method: "POST",
  });
  expect(response.status).toBe(201);
  const receipt = await response.json<AgentReceipt>();
  createdAgents.add(receipt.agent_id);
  return receipt;
}

async function managedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const request = new Request(input, init);
  const headers = new Headers(request.headers);
  headers.set("authorization", `Bearer ${API_KEY}`);
  return RAW_SELF.fetch(new Request(request, { headers }));
}

function connectGrantHeaders(
  connectors: readonly string[],
  userId = USER_ID,
): Headers {
  return new Headers({
    "x-nanocodex-connect-user": userId,
    "x-nanocodex-connect-grant-id": CONNECT_GRANT_ID,
    "x-nanocodex-connect-capabilities": JSON.stringify([
      "agents:read",
      "agents:write",
      "tools:use",
    ]),
    "x-nanocodex-connect-connectors": JSON.stringify(connectors),
    "x-nanocodex-connect-mcp-ids": JSON.stringify(["m".repeat(43)]),
  });
}

async function seedApiKey(
  userId: string,
  token: string,
  capabilities: readonly OrganizationCapability[] = OWNER_CAPABILITIES,
): Promise<void> {
  const digestBytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)));
  let binary = "";
  for (const byte of digestBytes) binary += String.fromCharCode(byte);
  const digest = btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const account = testEnv.NANOCODEX_USERS.getByName(userId);
  const provisioned = await account.fetch("https://user.internal/account", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ id: userId, persistent: true }),
  });
  expect(provisioned.ok).toBe(true);
  const accountRecord = await provisioned.json<{ organizationId: string }>();
  const organization = testEnv.NANOCODEX_ORGANIZATIONS.getByName(accountRecord.organizationId);
  const organizationRecord = await organization.fetch("https://organization.internal/metadata");
  expect(organizationRecord.ok).toBe(true);
  const metadata = await organizationRecord.json<{ rootTeam: { id: string } }>();
  const key = testEnv.NANOCODEX_API_KEYS.getByName(digest);
  await key.fetch("https://api-key.internal/record", { method: "DELETE" });
  const record = await key.fetch(
    "https://api-key.internal/record",
    {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        id: token.match(/^ncx_live_([A-Za-z0-9_-]{12})_/)?.[1],
        label: "test",
        prefix: token.slice(0, "ncx_live_".length + 12),
        createdAt: Date.now(),
        digest,
        userId,
        organizationId: accountRecord.organizationId,
        teamId: metadata.rootTeam.id,
        role: "owner",
        authorizationEpoch: 1,
        capabilities,
      }),
    },
  );
  expect(record.status).toBe(201);
}

async function seedPasskeySession(
  userId: string,
  token: string,
  publicKey = "0x01",
): Promise<void> {
  const account = testEnv.NANOCODEX_USERS.getByName(userId);
  const provisioned = await account.fetch("https://user.internal/account", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ id: userId, persistent: true }),
  });
  expect(provisioned.ok).toBe(true);

  const encodedUserId = btoa(userId).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const now = Math.floor(Date.now() / 1_000);
  const auth = testEnv.NANOCODEX_AUTH.getByName("webauthn");
  const stored = await auth.fetch(
    `https://do.invalid/set?key=${encodeURIComponent(`session:${token}`)}`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        value: {
          credentialId: `credential-${token}`,
          publicKey,
          userId: encodedUserId,
          issuedAt: now,
          expiresAt: now + 60,
        },
        ttl: 60,
      }),
    },
  );
  expect(stored.ok).toBe(true);
}

async function seedBrowserAccountSession(userId: string, token: string): Promise<void> {
  const now = Math.floor(Date.now() / 1_000);
  const auth = testEnv.NANOCODEX_AUTH.getByName("account");
  const stored = await auth.fetch(
    `https://do.invalid/set?key=${encodeURIComponent(`session:${token}`)}`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        value: { userId, issuedAt: now, expiresAt: now + 60 },
        ttl: 60,
      }),
    },
  );
  expect(stored.ok).toBe(true);
}

async function submit(agent: AgentReceipt, id: string, input: string): Promise<ManagedTurnView> {
  const response = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "idempotency-key": `request-${id}`,
    },
    body: JSON.stringify({ id, input }),
  });
  expect(response.status).toBe(202);
  return response.json<ManagedTurnView>();
}

async function managedRealtime(
  agent: AgentReceipt,
  action: "start" | "delegate" | "stop",
  body: Readonly<{
    input?: string;
    operation_id: string;
    voice_session_id: string;
  }>,
): Promise<Response> {
  return SELF.fetch(agent.events_url.replace(/\/events$/, `/realtime/${action}`), {
    method: "POST",
    headers: { "content-type": "application/json", origin: "https://example.test" },
    body: JSON.stringify(body),
  });
}

function publicRealtimeCallBody(sdp = "v=0"): string {
  return JSON.stringify({
    sdp,
    session: {
      model: "gpt-live-1-codex",
      instructions: "Canonical Codex Realtime instructions for the managed transport test.",
      audio: { output: { voice: "cove" } },
      delegation: { type: "client" },
    },
  });
}

async function submitWithApiKey(
  agent: AgentReceipt,
  id: string,
  input: string,
  apiKey: string,
): Promise<ManagedTurnView> {
  const response = await RAW_SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
      "idempotency-key": `request-${id}`,
    },
    body: JSON.stringify({ id, input }),
  });
  expect(response.status).toBe(202);
  return response.json<ManagedTurnView>();
}

async function historyFindSessions(query: string, apiKey: string): Promise<HistorySearchBody> {
  const response = await RAW_SELF.fetch("https://example.test/v1/history/sessions/search", {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({ query, limit: 8 }),
  });
  expect(response.status).toBe(200);
  return response.json<HistorySearchBody>();
}

async function historyReadSession(sessionId: string, turnIds: string[], apiKey: string) {
  const response = await RAW_SELF.fetch(
    `https://example.test/v1/history/sessions/${sessionId}/read`,
    {
      method: "POST",
      headers: {
        authorization: `Bearer ${apiKey}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ turn_ids: turnIds }),
    },
  );
  expect(response.status).toBe(200);
  return response.json<{
    turns: Array<Record<string, unknown>>;
    citations: Array<Record<string, unknown>>;
  }>();
}

async function memoryRequest(apiKey: string, operation: unknown): Promise<Response> {
  return RAW_SELF.fetch("https://example.test/v1/memory", {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(operation),
  });
}

async function memoryJson(apiKey: string, operation: unknown): Promise<any> {
  const response = await memoryRequest(apiKey, operation);
  expect(response.status).toBe(200);
  return response.json();
}

async function eventuallyHistoryFindSessions(
  query: string,
  apiKey: string,
  ready: (body: HistorySearchBody) => boolean,
): Promise<HistorySearchBody> {
  let latest: HistorySearchBody | undefined;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    latest = await historyFindSessions(query, apiKey);
    if (ready(latest)) return latest;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`history search did not converge: ${JSON.stringify(latest)}`);
}

function sseReader(response: Response) {
  if (!response.body) throw new Error("SSE response has no body");
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  let buffer = "";
  return {
    async next(): Promise<{ id: string; event: string; data: Record<string, unknown> }> {
      while (true) {
        const boundary = buffer.indexOf("\n\n");
        if (boundary >= 0) {
          const frame = buffer.slice(0, boundary);
          buffer = buffer.slice(boundary + 2);
          const parsed = parseSseFrame(frame);
          if (parsed) return parsed;
          continue;
        }
        const chunk = await reader.read();
        if (chunk.done) throw new Error("SSE stream ended before the next event");
        buffer += chunk.value;
      }
    },
    cancel: () => reader.cancel(),
  };
}

async function testHash(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function sseFrameReader(response: Response) {
  if (!response.body) throw new Error("SSE response has no body");
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  let buffer = "";
  return {
    async nextFrame(): Promise<string> {
      while (true) {
        const boundary = buffer.indexOf("\n\n");
        if (boundary >= 0) {
          const frame = buffer.slice(0, boundary);
          buffer = buffer.slice(boundary + 2);
          return frame;
        }
        const chunk = await reader.read();
        if (chunk.done) throw new Error("SSE stream ended before the next frame");
        buffer += chunk.value;
      }
    },
    cancel: () => reader.cancel(),
  };
}

async function waitForTurnState(
  agent: AgentReceipt,
  id: string,
  expected: string,
  timeoutMs = 4_000,
): Promise<ManagedTurnView> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const response = await SELF.fetch(agent.events_url.replace(/\/events$/, `/turns/${id}`));
    if (response.ok) {
      const turn = await response.json<ManagedTurnView>();
      if (turn.state === expected) return turn;
      if (turn.state === "failed" || turn.state === "blocked") {
        throw new Error(
          `turn ${id} entered ${turn.state} while waiting for ${expected}: ${turn.error ?? "unknown error"}`,
        );
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`timed out waiting for turn ${id} to enter ${expected}`);
}

async function waitForTurnAttempt(
  agent: AgentReceipt,
  id: string,
  expected: number,
): Promise<ManagedTurnView> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const response = await SELF.fetch(agent.events_url.replace(/\/events$/, `/turns/${id}`));
    if (response.ok) {
      const turn = await response.json<ManagedTurnView>();
      if (turn.attempt_count >= expected) return turn;
      if (turn.state === "failed" || turn.state === "blocked" || turn.state === "cancelled") {
        throw new Error(`turn ${id} entered ${turn.state} before retry attempt ${expected}`);
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`timed out waiting for turn ${id} retry attempt ${expected}`);
}

async function managedHistory(agent: AgentReceipt): Promise<ManagedHistory> {
  const response = await SELF.fetch(`${agent.events_url}/history?limit=256`);
  expect(response.status).toBe(200);
  return response.json<ManagedHistory>();
}

async function waitForHistoryEvent(
  agent: AgentReceipt,
  predicate: (event: ManagedHistoryEvent) => boolean,
): Promise<ManagedHistoryEvent> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const event = (await managedHistory(agent)).data.find(predicate);
    if (event) return event;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("timed out waiting for managed history event");
}

async function waitForScheduledAlarm(
  stub: DurableObjectStub<NanocodexSession>,
): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    const alarm = await runInDurableObject(stub, (_instance, state) => state.storage.getAlarm());
    if (alarm !== null) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("timed out waiting for a scheduled Durable Object alarm");
}

async function cleanupMarkers(
  stub: DurableObjectStub<NanocodexSession>,
): Promise<{ binding: boolean; deleting: boolean }> {
  return runInDurableObject(stub, async (_instance, state) => ({
    binding: await state.storage.get("nanocodex:credential-binding") !== undefined,
    deleting: await state.storage.get("nanocodex:session-deleting") === true,
  }));
}

function expireCredentialPreparation(): void {
  vi.setSystemTime(Date.now() + 61_000);
}

async function waitForCleanupDeletion(
  stub: DurableObjectStub<NanocodexSession>,
): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    if ((await cleanupMarkers(stub)).deleting) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("timed out waiting for watchdog deletion");
}

async function runCleanupAlarmsUntilDeleted(
  stub: DurableObjectStub<NanocodexSession>,
): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    if (!(await cleanupMarkers(stub)).deleting) return;
    await runDurableObjectAlarm(stub);
    await scheduler.wait(10);
  }
  throw new Error("timed out running Durable Object cleanup alarms");
}

function sessionForSubject(subject: string): DurableObjectStub<NanocodexSession> {
  return testEnv.NANOCODEX_SESSIONS.get(
    testEnv.NANOCODEX_SESSIONS.idFromString(subject),
  );
}

async function nextWithin(
  reader: ReturnType<typeof sseReader>,
  stage: string,
  timeoutMs = 2_000,
): Promise<{ id: string; event: string; data: Record<string, unknown> }> {
  return within(reader.next(), stage, timeoutMs);
}

async function within<Result>(
  promise: Promise<Result>,
  stage: string,
  timeoutMs = 2_000,
): Promise<Result> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`timed out waiting for ${stage}`)), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout !== undefined) clearTimeout(timeout);
  }
}

function parseSseFrame(frame: string) {
  let id: string | undefined;
  let event: string | undefined;
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line.startsWith(":")) continue;
    const separator = line.indexOf(":");
    const field = separator < 0 ? line : line.slice(0, separator);
    const value = separator < 0 ? "" : line.slice(separator + 1).replace(/^ /, "");
    if (field === "id") id = value;
    else if (field === "event") event = value;
    else if (field === "data") data.push(value);
  }
  if (id === undefined || event === undefined || data.length === 0) return undefined;
  return { id, event, data: JSON.parse(data.join("\n")) as Record<string, unknown> };
}
