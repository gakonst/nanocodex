import {
  env,
  SELF as RAW_SELF,
  evictDurableObject,
  runDurableObjectAlarm,
  runInDurableObject,
} from "cloudflare:test";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { NanocodexSession, UserAccount, type Env } from "../src/index";
import { ManagedEventArchive } from "../src/managed-event-archive";

const testEnv = env as unknown as Env;
const USER_ID = "11111111-1111-4111-8111-111111111111";
const API_KEY = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
const OTHER_USER_ID = "33333333-3333-4333-8333-333333333333";
const OTHER_API_KEY = `ncx_live_${"o".repeat(12)}_${"p".repeat(43)}`;
const createdAgents = new Set<string>();
const SELF = { fetch: managedFetch };

beforeAll(async () => {
  await seedApiKey(USER_ID, API_KEY);
  await seedApiKey(OTHER_USER_ID, OTHER_API_KEY);
});

afterEach(async () => {
  await Promise.all([...createdAgents].map(async (id) => {
    await SELF.fetch(`https://example.test/v1/agents/${id}`, { method: "DELETE" });
    createdAgents.delete(id);
  }));
});

describe("managed agents REST and resumable SSE", () => {
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

    await waitForScheduledAlarm(session);
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
    expect(callback.status).toBe(303);
    expect(callback.headers.get("location")).toBe(
      "https://example.test/agent?thread=connector&connector=github&connector_result=connected",
    );

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

  it("rejects malformed bearer authentication instead of minting a browser identity", async () => {
    const response = await RAW_SELF.fetch("https://example.test/v1/me", {
      headers: { authorization: "Bearer malformed" },
    });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: "unauthorized" });
    expect(response.headers.get("set-cookie")).toBeNull();
  });

  it("rejects and clears stale account cookies instead of minting a replacement identity", async () => {
    const expiredToken = `a_${"e".repeat(43)}`;
    const auth = testEnv.NANOCODEX_AUTH.getByName("account");
    const stored = await auth.fetch(
      `https://do.invalid/set?key=${encodeURIComponent(`session:${expiredToken}`)}`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          value: {
            userId: "44444444-4444-4444-8444-444444444444",
            issuedAt: 1,
            expiresAt: 2,
          },
          ttl: 60,
        }),
      },
    );
    expect(stored.ok).toBe(true);

    for (const token of [
      "malformed",
      `a_${"z".repeat(43)}`,
      expiredToken,
      "z".repeat(43),
    ]) {
      const response = await RAW_SELF.fetch("https://example.test/v1/me", {
        headers: { cookie: `other=value; nanocodex_account=${token}` },
      });
      expect(response.status).toBe(401);
      expect(await response.json()).toEqual({ error: "invalid_session" });
      expect(response.headers.get("set-cookie")).toMatch(
        /^nanocodex_account=; Path=\/; Max-Age=0; HttpOnly; SameSite=Lax; Secure$/,
      );
    }

    const unrelatedCookie = await RAW_SELF.fetch("https://example.test/v1/me", {
      headers: { cookie: "other=value" },
    });
    expect(unrelatedCookie.status).toBe(200);
    expect(unrelatedCookie.headers.get("set-cookie")).toMatch(
      /^nanocodex_account=a_[A-Za-z0-9_-]{43};/,
    );
  });

  it("recognizes passkey sessions even when their random token begins with the anonymous prefix", async () => {
    const tokens = ["w".repeat(43), `a_${"w".repeat(41)}`];
    for (const [index, token] of tokens.entries()) {
      const userId = `22222222-2222-4222-8222-22222222222${index}`;
      await seedPasskeySession(userId, token);
      const response = await RAW_SELF.fetch("https://example.test/v1/me", {
        headers: { cookie: `nanocodex_account=${token}` },
      });
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({
        user: { id: userId, persistent: true },
        authentication: "account_session",
      });
      expect(response.headers.get("set-cookie")).toBeNull();
    }
  });

  it("forwards a browser account search body exactly once without leaking its credential", async () => {
    const session = await RAW_SELF.fetch("https://example.test/v1/me");
    const cookie = session.headers.get("set-cookie")?.split(";", 1)[0];
    const account = await session.json<{ user: { id: string } }>();
    expect(cookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);

    const body = '{\n  "id": "browser-search",\n  "commands": { "search_query": [{ "q": "Rust 🦀" }] }\n}';
    const response = await RAW_SELF.fetch("https://nanocodex.internal/v1/search", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        cookie: cookie!,
        "content-type": "application/json",
        origin: "https://example.test",
        "x-nanocodex-subject": "browser-controlled-subject",
      },
      body,
    });

    expect(response.status).toBe(200);
    const forwarded = await response.json<{
      body: string;
      cookie: string | null;
      origin: string | null;
      subject: string;
    }>();
    expect(forwarded).toMatchObject({
      body,
      cookie: null,
      origin: "https://example.test",
    });
    expect(forwarded.subject).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(forwarded.subject).not.toBe("browser-controlled-subject");
    expect(JSON.stringify(forwarded)).not.toContain(cookie!);
    expect(JSON.stringify(forwarded)).not.toContain(account.user.id);

  });

  it("keeps subject binding off healthy browser model upgrades and self-heals only a missing subject", async () => {
    const session = await RAW_SELF.fetch("https://example.test/v1/me");
    const cookie = session.headers.get("set-cookie")?.split(";", 1)[0];
    expect(cookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);

    const originalBroker = testEnv.NANOCODEX;
    let bindingAttempts = 0;
    let modelAttempts = 0;
    let subject: string | undefined;
    let denyNextModel = false;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const url = new URL(request.url);
        if (request.method === "PUT" && url.pathname.startsWith("/subjects/")) {
          bindingAttempts += 1;
        }
        if (request.method === "GET" && /^\/users\/[^/]+\/credentials$/.test(url.pathname)) {
          return Response.json({ active: "chatgpt", ready: true });
        }
        if (url.hostname === "nanocodex.internal" && url.pathname === "/v1/responses") {
          modelAttempts += 1;
          subject = request.headers.get("x-nanocodex-subject") ?? undefined;
          if (denyNextModel) {
            denyNextModel = false;
            return Response.json({ error: "required_header_mismatch" }, { status: 403 });
          }
        }
        if (url.hostname === "nanocodex.internal" && url.pathname === "/v1/search") {
          modelAttempts += 1;
          subject = request.headers.get("x-nanocodex-subject") ?? undefined;
          if (denyNextModel) {
            denyNextModel = false;
            return Response.json({ error: "required_header_mismatch" }, { status: 403 });
          }
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    const accountCookie = async (): Promise<string> => {
      const account = await RAW_SELF.fetch("https://example.test/v1/me");
      const accountCookie = account.headers.get("set-cookie")?.split(";", 1)[0];
      expect(accountCookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);
      return accountCookie!;
    };
    const search = (accountCookie: string, id: string) => RAW_SELF.fetch(
      "https://nanocodex.internal/v1/search",
      {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          cookie: accountCookie,
          "content-type": "application/json",
        },
        body: JSON.stringify({ id }),
      },
    );

    try {
      const ready = await RAW_SELF.fetch(
        "https://managed.internal/v1/credentials",
        { headers: { cookie: cookie! } },
      );
      expect(ready.status).toBe(200);
      expect(await ready.json()).toEqual({ active: "chatgpt", ready: true });
      expect(bindingAttempts).toBe(1);

      const sockets = await Promise.all(Array.from({ length: 36 }, (_, index) => (
        RAW_SELF.fetch("https://nanocodex.internal/v1/responses", {
          headers: {
            authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
            cookie: cookie!,
            "openai-beta": "responses_websockets=2026-02-06",
            "session-id": `browser-session-${index}`,
            "thread-id": `browser-session-${index}`,
            upgrade: "websocket",
          },
        })
      )));
      expect(sockets.every(({ status }) => status === 101)).toBe(true);
      for (const response of sockets) {
        response.webSocket!.accept();
        response.webSocket!.close();
      }
      expect(bindingAttempts).toBe(1);
      expect(modelAttempts).toBe(36);
      expect(subject).toMatch(/^[A-Za-z0-9_-]{43}$/);
      const readySubject = subject;

      const coldSearchCookie = await accountCookie();
      const healed = await search(coldSearchCookie, "self-heal-browser-subject");
      expect(healed.status).toBe(200);
      const healedSearch = await healed.json<{ body: string; subject: string }>();
      expect(healedSearch).toMatchObject({
        body: JSON.stringify({ id: "self-heal-browser-subject" }),
      });
      expect(healedSearch.subject).toMatch(/^[A-Za-z0-9_-]{43}$/);
      expect(healedSearch.subject).not.toBe(readySubject);
      expect(bindingAttempts).toBe(2);
      expect(modelAttempts).toBe(38);

      const coldSocketCookie = await accountCookie();
      const healedSocket = await RAW_SELF.fetch("https://nanocodex.internal/v1/responses", {
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          cookie: coldSocketCookie,
          "openai-beta": "responses_websockets=2026-02-06",
          "session-id": "cold-browser-session",
          "thread-id": "cold-browser-session",
          upgrade: "websocket",
        },
      });
      expect(healedSocket.status).toBe(101);
      healedSocket.webSocket!.accept();
      healedSocket.webSocket!.close();
      expect(bindingAttempts).toBe(3);
      expect(modelAttempts).toBe(40);

      denyNextModel = true;
      const definitive = await search(cookie!, "do-not-bind-other-403");
      expect(definitive.status).toBe(403);
      expect(await definitive.json()).toEqual({ error: "required_header_mismatch" });
      expect(bindingAttempts).toBe(3);
      expect(modelAttempts).toBe(41);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("forwards browser Realtime calls through the same opaque account subject", async () => {
    const session = await RAW_SELF.fetch("https://example.test/v1/me");
    const cookie = session.headers.get("set-cookie")?.split(";", 1)[0];
    const account = await session.json<{ user: { id: string } }>();
    const body = JSON.stringify({ sdp: "v=0", session: { delegation: { type: "client" } } });
    const response = await RAW_SELF.fetch("https://nanocodex.internal/v1/realtime/calls", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        cookie: cookie!,
        "content-type": "application/json",
        origin: "https://example.test",
        "x-nanocodex-subject": "browser-controlled-subject",
        "x-session-id": "voice-session",
      },
      body,
    });

    expect(response.status).toBe(200);
    const forwarded = await response.json<{
      body: string;
      cookie: string | null;
      origin: string | null;
      session: string;
      subject: string;
    }>();
    expect(forwarded).toMatchObject({
      body,
      cookie: null,
      origin: "https://example.test",
      session: "voice-session",
    });
    expect(forwarded.subject).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(forwarded.subject).not.toBe("browser-controlled-subject");
    expect(JSON.stringify(forwarded)).not.toContain(cookie!);
    expect(JSON.stringify(forwarded)).not.toContain(account.user.id);

    const created = await RAW_SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: { cookie: cookie!, origin: "https://example.test" },
    });
    expect(created.status).toBe(201);
    const agent = await created.json<AgentReceipt>();
    const managed = await RAW_SELF.fetch("https://nanocodex.internal/v1/realtime/calls", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        cookie: cookie!,
        "content-type": "application/json",
        origin: "https://example.test",
        "x-nanocodex-agent-id": agent.agent_id,
        "x-session-id": agent.agent_id,
        "session-id": agent.agent_id,
        "thread-id": agent.agent_id,
      },
      body,
    });
    expect(managed.status).toBe(200);
    const managedForwarded = await managed.json<{
      agent: string | null;
      subject: string;
    }>();
    expect(managedForwarded.agent).toBeNull();
    expect(managedForwarded.subject).toBe(
      testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString(),
    );

    const mismatched = await RAW_SELF.fetch("https://nanocodex.internal/v1/realtime/calls", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        cookie: cookie!,
        "content-type": "application/json",
        origin: "https://example.test",
        "x-nanocodex-agent-id": agent.agent_id,
        "x-session-id": "wrong-session",
        "session-id": agent.agent_id,
        "thread-id": agent.agent_id,
      },
      body,
    });
    expect(mismatched.status).toBe(404);
    expect(await mismatched.json()).toEqual({ error: "not_found" });
  });

  it("upgrades browser Realtime sidebands through the same opaque account subject", async () => {
    const session = await RAW_SELF.fetch("https://example.test/v1/me");
    const cookie = session.headers.get("set-cookie")?.split(";", 1)[0];
    const account = await session.json<{ user: { id: string } }>();
    const response = await RAW_SELF.fetch("https://nanocodex.internal/v1/realtime/sideband", {
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        cookie: cookie!,
        upgrade: "websocket",
        "x-nanocodex-realtime-call-id": "rtc_test",
        "x-nanocodex-subject": "browser-controlled-subject",
        "x-session-id": "voice-session",
      },
    });

    expect(response.status).toBe(101);
    const socket = response.webSocket;
    expect(socket).toBeDefined();
    socket!.accept();
    const forwarded = await new Promise<{
      callId: string;
      cookie: string | null;
      session: string;
      subject: string;
    }>((resolve) => {
      socket!.addEventListener("message", (event) => resolve(JSON.parse(String(event.data))));
    });
    socket!.close();
    expect(forwarded).toMatchObject({
      callId: "rtc_test",
      cookie: null,
      session: "voice-session",
    });
    expect(forwarded.subject).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(forwarded.subject).not.toBe("browser-controlled-subject");
    expect(JSON.stringify(forwarded)).not.toContain(cookie!);
    expect(JSON.stringify(forwarded)).not.toContain(account.user.id);
  });

  it("opens agent-scoped Realtime calls and sidebands with an account API key", async () => {
    const agent = await createAgent();
    const route = agent.events_url.replace(/\/events$/, "/realtime");
    const callBody = publicRealtimeCallBody("v=0\r\no=mobile");
    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const removedSubject = await testEnv.NANOCODEX.fetch(
      `https://broker.internal/subjects/${subject}`,
      { method: "DELETE" },
    );
    expect(removedSubject.status).toBe(204);
    const call = await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "session-id": "caller-chosen-session",
        "thread-id": "caller-chosen-thread",
        "x-nanocodex-agent-id": "caller-chosen-agent",
        "x-nanocodex-subject": "caller-chosen-subject",
        "x-session-id": "caller-chosen-realtime-session",
      },
      body: callBody,
    });
    expect(call.status).toBe(200);
    expect(call.headers.get("location")).toBeNull();
    expect(call.headers.get("x-nanocodex-realtime-location")).toBe(
      "/backend-api/codex/realtime/calls/rtc_test",
    );
    expect(call.headers.get("authorization")).toBeNull();
    expect(call.headers.get("chatgpt-account-id")).toBeNull();
    expect(call.headers.get("set-cookie")).toBeNull();
    expect(await call.json()).toEqual({
      agent: agent.agent_id,
      body: callBody,
      cookie: null,
      lifecycleSession: agent.agent_id,
      openAiAlpha: "quicksilver=v2",
      origin: null,
      session: agent.agent_id,
      subject,
      thread: agent.agent_id,
    });

    const sideband = await SELF.fetch(`${route}/sideband?call_id=rtc_mobile`, {
      headers: {
        upgrade: "websocket",
        "session-id": "caller-chosen-session",
        "x-nanocodex-subject": "caller-chosen-subject",
      },
    });
    expect(sideband.status).toBe(101);
    expect(sideband.webSocket).toBeDefined();
    sideband.webSocket!.accept();
    const forwarded = await new Promise<Record<string, unknown>>((resolve) => {
      sideband.webSocket!.addEventListener("message", (event) => {
        resolve(JSON.parse(String(event.data)));
      });
    });
    sideband.webSocket!.close();
    expect(forwarded).toEqual({
      agent: agent.agent_id,
      callId: "rtc_mobile",
      cookie: null,
      lifecycleSession: agent.agent_id,
      openAiAlpha: "quicksilver=v2",
      session: agent.agent_id,
      subject,
      thread: agent.agent_id,
    });
    expect(JSON.stringify(forwarded)).not.toContain(API_KEY);
  });

  it("hides agent-scoped Realtime routes from other owners and missing agents", async () => {
    const agent = await createAgent();
    const route = agent.events_url.replace(/\/events$/, "/realtime");
    const body = publicRealtimeCallBody();
    const otherOwnerCall = await RAW_SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${OTHER_API_KEY}`,
        "content-type": "application/json",
      },
      body,
    });
    expect(otherOwnerCall.status).toBe(404);
    expect(await otherOwnerCall.json()).toEqual({ error: "not_found" });
    const otherOwnerSideband = await RAW_SELF.fetch(
      `${route}/sideband?call_id=rtc_other`,
      { headers: { authorization: `Bearer ${OTHER_API_KEY}`, upgrade: "websocket" } },
    );
    expect(otherOwnerSideband.status).toBe(404);

    const missing = "019d2f5d-7491-7000-8000-000000000099";
    const missingCall = await SELF.fetch(
      `https://example.test/v1/agents/${missing}/realtime/calls`,
      { method: "POST", headers: { "content-type": "application/json" }, body },
    );
    expect(missingCall.status).toBe(404);
    const missingSideband = await SELF.fetch(
      `https://example.test/v1/agents/${missing}/realtime/sideband?call_id=rtc_missing`,
      { headers: { upgrade: "websocket" } },
    );
    expect(missingSideband.status).toBe(404);
  });

  it("validates the public Realtime method, call body, upgrade, and browser origin", async () => {
    const agent = await createAgent();
    const route = agent.events_url.replace(/\/events$/, "/realtime");
    expect((await SELF.fetch(`${route}/calls`)).status).toBe(405);
    expect((await SELF.fetch(`${route}/sideband?call_id=rtc_test`, {
      method: "POST",
    })).status).toBe(405);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "text/plain" },
      body: "v=0",
    })).status).toBe(415);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{",
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        call_body: JSON.stringify({ sdp: "v=0", session: {} }),
        realtime_session_id: agent.agent_id,
      }),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls?subject=caller`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: publicRealtimeCallBody(),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        ...JSON.parse(publicRealtimeCallBody()),
        session: {
          ...JSON.parse(publicRealtimeCallBody()).session,
          model: "caller-selected-model",
        },
      }),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        ...JSON.parse(publicRealtimeCallBody()),
        session: {
          ...JSON.parse(publicRealtimeCallBody()).session,
          audio: { output: { voice: "caller-selected-voice" } },
        },
      }),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        ...JSON.parse(publicRealtimeCallBody()),
        session: {
          ...JSON.parse(publicRealtimeCallBody()).session,
          instructions: "i".repeat(32 * 1024 + 1),
        },
      }),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        ...JSON.parse(publicRealtimeCallBody()),
        session: {
          ...JSON.parse(publicRealtimeCallBody()).session,
          destination: "https://attacker.test",
        },
      }),
    })).status).toBe(400);
    expect((await SELF.fetch(`${route}/sideband?call_id=rtc_test`)).status).toBe(426);
    expect((await SELF.fetch(`${route}/sideband?call_id=..%2Fprovider`, {
      headers: { upgrade: "websocket" },
    })).status).toBe(400);
    expect((await SELF.fetch(
      `${route}/sideband?call_id=rtc_test&session_id=caller`,
      { headers: { upgrade: "websocket" } },
    )).status).toBe(400);

    const token = "v".repeat(43);
    await seedPasskeySession(USER_ID, token);
    const cookie = `nanocodex_account=${token}`;
    const crossOrigin = await RAW_SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: {
        cookie,
        "content-type": "application/json",
        origin: "https://attacker.test",
      },
      body: publicRealtimeCallBody(),
    });
    expect(crossOrigin.status).toBe(403);

    const browserCall = await RAW_SELF.fetch(`${route}/calls`, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin: "https://example.test" },
      body: publicRealtimeCallBody(),
    });
    expect(browserCall.status).toBe(200);
    const browserSideband = await RAW_SELF.fetch(`${route}/sideband?call_id=rtc_browser`, {
      headers: { cookie, origin: "https://example.test", upgrade: "websocket" },
    });
    expect(browserSideband.status).toBe(101);
    browserSideband.webSocket!.accept();
    browserSideband.webSocket!.close();
    const crossOriginSideband = await RAW_SELF.fetch(`${route}/sideband?call_id=rtc_browser`, {
      headers: { cookie, origin: "https://attacker.test", upgrade: "websocket" },
    });
    expect(crossOriginSideband.status).toBe(403);
  });

  it("lets anonymous and passkey cookies use browser-local and managed-durable runtimes", async () => {
    const anonymous = await RAW_SELF.fetch("https://example.test/v1/me");
    const anonymousCookie = anonymous.headers.get("set-cookie")?.split(";", 1)[0];
    expect(anonymousCookie).toMatch(/^nanocodex_account=a_[A-Za-z0-9_-]{43}$/);

    const passkeyUserId = "44444444-4444-4444-8444-444444444444";
    const passkeyToken = `a_${"z".repeat(41)}`;
    await seedPasskeySession(passkeyUserId, passkeyToken);
    const principals = [
      { kind: "anonymous", cookie: anonymousCookie! },
      { kind: "passkey", cookie: `nanocodex_account=${passkeyToken}` },
    ];

    for (const principal of principals) {
      const search = await RAW_SELF.fetch("https://nanocodex.internal/v1/search", {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          cookie: principal.cookie,
          "content-type": "application/json",
          origin: "https://example.test",
        },
        body: JSON.stringify({ mode: principal.kind }),
      });
      expect(search.status).toBe(200);
      expect(await search.json()).toMatchObject({
        body: JSON.stringify({ mode: principal.kind }),
        cookie: null,
        origin: "https://example.test",
      });

      const created = await RAW_SELF.fetch("https://example.test/v1/agents", {
        method: "POST",
        headers: { cookie: principal.cookie, origin: "https://example.test" },
      });
      expect(created.status).toBe(201);
      const receipt = await created.json<AgentReceipt>();
      const listed = await RAW_SELF.fetch("https://example.test/v1/agents", {
        headers: { cookie: principal.cookie },
      });
      expect(await listed.json()).toEqual({
        data: [receipt.agent_id],
        summaries: {
          [receipt.agent_id]: expect.objectContaining({ title: "", turn_count: 0 }),
        },
      });
      const replay = await RAW_SELF.fetch(receipt.events_url.replace(/\/events$/, ""), {
        headers: { cookie: principal.cookie },
      });
      expect(replay.status).toBe(200);
      await replay.body?.cancel();
      const deleted = await RAW_SELF.fetch(receipt.events_url.replace(/\/events$/, ""), {
        method: "DELETE",
        headers: { cookie: principal.cookie, origin: "https://example.test" },
      });
      expect(deleted.status).toBe(204);
    }
  });

  it("accepts Connect ownership only on the trusted service-binding origin", async () => {
    const publicAttempt = await RAW_SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: { "x-nanocodex-connect-user": USER_ID },
    });
    expect(publicAttempt.status).toBe(401);

    const created = await RAW_SELF.fetch("https://nanocodex.internal/v1/agents", {
      method: "POST",
      headers: { "x-nanocodex-connect-user": USER_ID },
    });
    expect(created.status).toBe(201);
    const receipt = await created.json<AgentReceipt>();
    createdAgents.add(receipt.agent_id);

    const state = await RAW_SELF.fetch(
      `https://nanocodex.internal/v1/agents/${receipt.agent_id}`,
      { headers: { "x-nanocodex-connect-user": USER_ID } },
    );
    expect(state.status).toBe(200);

    const otherAccount = await RAW_SELF.fetch(
      `https://nanocodex.internal/v1/agents/${receipt.agent_id}`,
      { headers: { "x-nanocodex-connect-user": OTHER_USER_ID } },
    );
    expect(otherAccount.status).toBe(404);
  });

  it("runs the default network tools inside a managed durable agent", async () => {
    const agent = await createAgent();
    const accepted = await submit(agent, "turn-managed-web", "E2E_MANAGED_WEB");
    const events = sseReader(await SELF.fetch(
      `${agent.events_url}?cursor=${accepted.accepted_cursor}`,
    ));
    let event;
    do {
      event = await nextWithin(events, "managed web tool completion");
    } while (event.data.type !== "turn_completed");
    expect(event.data).toMatchObject({
      id: "turn-managed-web",
      final_message: "MANAGED_WEB_OK",
      type: "turn_completed",
    });
    await events.cancel();
  });

  it("runs the normal tool composition through durable in-process Just Bash", async () => {
    const agent = await createAgent();
    const accepted = await submit(agent, "turn-computer-runtime", "E2E_COMPUTER_RUNTIME");
    const events = sseReader(await SELF.fetch(
      `${agent.events_url}?cursor=${accepted.accepted_cursor}`,
    ));
    let event;
    const observed: unknown[] = [];
    do {
      event = await nextWithin(events, "Computer runtime tool completion");
      observed.push(event.data);
    } while (event.data.type !== "turn_completed");
    expect(JSON.stringify(observed)).toContain("COMPUTER_RUNTIME_OK");
    expect(event.data).toMatchObject({
      id: "turn-computer-runtime",
      final_message: "COMPUTER_TOOLS_OK",
      type: "turn_completed",
    });
    await events.cancel();
  });

  it("does not let an unrelated bearer mint managed agents", async () => {
    const response = await RAW_SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: {
        authorization: `Bearer ncx_live_${"x".repeat(12)}_${"y".repeat(43)}`,
      },
    });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: "unauthorized" });
  });

  it("requires the account API key in addition to the routing UUID", async () => {
    const agent = await createAgent();
    const stateUrl = agent.events_url.replace(/\/events$/, "");
    const missing = await RAW_SELF.fetch(stateUrl);
    expect(missing.status).toBe(401);
    expect(await missing.json()).toEqual({ error: "unauthorized" });
    const wrong = await RAW_SELF.fetch(stateUrl, { headers: {
      authorization: `Bearer ncx_live_${"x".repeat(12)}_${"y".repeat(43)}`,
    } });
    expect(wrong.status).toBe(401);
    expect((await SELF.fetch(stateUrl)).status).toBe(200);
  });

  it("keeps managed realtime mutations same-origin, owner-asserted, and bounded", async () => {
    const agent = await createAgent();
    const realtimeUrl = agent.events_url.replace(/\/events$/, "/realtime/start");
    const token = "r".repeat(43);
    await seedPasskeySession(USER_ID, token);
    const cookie = `nanocodex_account=${token}`;
    const body = JSON.stringify({
      operation_id: "voice-origin-operation",
      voice_session_id: "voice-origin-session",
    });

    for (const origin of [undefined, "https://attacker.test"]) {
      const response = await RAW_SELF.fetch(realtimeUrl, {
        method: "POST",
        headers: {
          cookie,
          "content-type": "application/json",
          ...(origin === undefined ? {} : { origin }),
        },
        body,
      });
      expect(response.status).toBe(403);
      expect(await response.json()).toEqual({ error: "forbidden_origin" });
    }

    const otherOwner = await RAW_SELF.fetch(realtimeUrl, {
      method: "POST",
      headers: {
        authorization: `Bearer ${OTHER_API_KEY}`,
        "content-type": "application/json",
        origin: "https://example.test",
      },
      body,
    });
    expect(otherOwner.status).toBe(404);
    expect(await otherOwner.json()).toEqual({ error: "not_found" });

    const internal = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const missingAssertion = await internal.fetch("https://session.internal/realtime/start", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
    });
    expect(missingAssertion.status).toBe(404);

    const oversized = await SELF.fetch(realtimeUrl, {
      method: "POST",
      headers: {
        "content-length": String(64 * 1024 + 1),
        "content-type": "application/json",
        origin: "https://example.test",
      },
      body,
    });
    expect(oversized.status).toBe(413);
    expect(await oversized.json()).toMatchObject({ error: "request_too_large" });

    const forbiddenAudio = await SELF.fetch(realtimeUrl, {
      method: "POST",
      headers: { "content-type": "application/json", origin: "https://example.test" },
      body: JSON.stringify({
        audio: "provider-or-audio-data-must-not-cross-this-route",
        operation_id: "voice-audio-operation",
        voice_session_id: "voice-origin-session",
      }),
    });
    expect(forbiddenAudio.status).toBe(400);
    expect(await forbiddenAudio.json()).toMatchObject({ error: "invalid_request" });
  });

  it("starts and stops the canonical managed realtime lifecycle idempotently", async () => {
    const agent = await createAgent();
    const started = await managedRealtime(agent, "start", {
      operation_id: "voice-start-operation",
      voice_session_id: "voice-lifecycle-session",
    });
    expect(started.status).toBe(200);
    const startValue = await started.json<ManagedRealtimeLifecycleResponse>();
    expect(startValue).toMatchObject({
      operation_id: "voice-start-operation",
      voice_session_id: "voice-lifecycle-session",
      context: { workspace: "." },
    });
    expect(JSON.stringify(startValue.context.history)).toContain("Realtime conversation started.");

    const duplicateStart = await managedRealtime(agent, "start", {
      operation_id: "voice-start-operation",
      voice_session_id: "voice-lifecycle-session",
    });
    expect(await duplicateStart.json()).toEqual(startValue);

    const stopped = await managedRealtime(agent, "stop", {
      operation_id: "voice-stop-operation",
      voice_session_id: "voice-lifecycle-session",
    });
    expect(stopped.status).toBe(200);
    const stopValue = await stopped.json<ManagedRealtimeLifecycleResponse>();
    expect(stopValue).toMatchObject({
      operation_id: "voice-stop-operation",
      stopped: true,
      voice_session_id: "voice-lifecycle-session",
      context: { workspace: "." },
    });
    expect(JSON.stringify(stopValue.context.history)).toContain("Realtime conversation ended.");

    const duplicateStop = await managedRealtime(agent, "stop", {
      operation_id: "voice-stop-operation",
      voice_session_id: "voice-lifecycle-session",
    });
    expect(await duplicateStop.json()).toEqual(stopValue);

    const operationCount = await runInDurableObject(
      testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id),
      (_instance, state) => state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_realtime_operations",
      ).one().count,
    );
    expect(operationCount).toBe(2);

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const archived = await session.fetch("https://session.internal/realtime/archive", {
      method: "POST",
    });
    expect(archived.status).toBe(200);
    expect(await archived.json()).toMatchObject({
      archived_receipts: 1,
      objects: 1,
      sealed: true,
    });
    expect(await runInDurableObject(
      session,
      (_instance, state) => state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_realtime_operations",
      ).one().count,
    )).toBe(1);

    const archivedStartReplay = await managedRealtime(agent, "start", {
      operation_id: "voice-start-operation",
      voice_session_id: "voice-lifecycle-session",
    });
    expect(await archivedStartReplay.json()).toEqual(startValue);
    const capacity = await (await session.fetch("https://session.internal/capacity")).json<{
      archived_realtime: { archived_receipts: number; objects: number };
    }>();
    expect(capacity.archived_realtime).toMatchObject({ archived_receipts: 1, objects: 1 });
  });

  it("ends realtime before releasing the managed lease", async () => {
    const agent = await createAgent();
    const first = await managedRealtime(agent, "start", {
      operation_id: "voice-order-first-start",
      voice_session_id: "voice-order-first",
    });
    expect(first.status).toBe(200);

    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const blockLeaseDeletion = () => runInDurableObject(
      session,
      (_instance, state) => state.storage.sql.exec(
        `CREATE TRIGGER block_managed_realtime_lease_delete
         BEFORE DELETE ON managed_realtime_session
         BEGIN
           SELECT RAISE(FAIL, 'managed realtime lease deletion blocked');
         END`,
      ),
    );
    const allowLeaseDeletion = () => runInDurableObject(
      session,
      (_instance, state) => state.storage.sql.exec(
        "DROP TRIGGER block_managed_realtime_lease_delete",
      ),
    );
    const activeVoiceSession = () => runInDurableObject(
      session,
      (_instance, state) => state.storage.sql.exec<{ voice_session_id: string }>(
        "SELECT voice_session_id FROM managed_realtime_session WHERE singleton = 1",
      ).one().voice_session_id,
    );

    await blockLeaseDeletion();
    const blockedReplacement = await managedRealtime(agent, "start", {
      operation_id: "voice-order-second-start",
      voice_session_id: "voice-order-second",
    });
    expect(blockedReplacement.status).toBe(500);
    expect(await activeVoiceSession()).toBe("voice-order-first");

    await allowLeaseDeletion();
    const replacement = await managedRealtime(agent, "start", {
      operation_id: "voice-order-second-start",
      voice_session_id: "voice-order-second",
    });
    expect(replacement.status).toBe(200);
    const replacementValue = await replacement.json<ManagedRealtimeLifecycleResponse>();
    expect(JSON.stringify(replacementValue.context.history).match(
      /Realtime conversation ended\./g,
    )).toHaveLength(2);

    await blockLeaseDeletion();
    const blockedStop = await managedRealtime(agent, "stop", {
      operation_id: "voice-order-stop",
      voice_session_id: "voice-order-second",
    });
    expect(blockedStop.status).toBe(500);
    expect(await activeVoiceSession()).toBe("voice-order-second");

    await allowLeaseDeletion();
    const stopped = await managedRealtime(agent, "stop", {
      operation_id: "voice-order-stop",
      voice_session_id: "voice-order-second",
    });
    expect(stopped.status).toBe(200);
    const stopValue = await stopped.json<ManagedRealtimeLifecycleResponse>();
    expect(JSON.stringify(stopValue.context.history).match(
      /Realtime conversation ended\./g,
    )).toHaveLength(4);
  });

  it("atomically starts or steers realtime delegation under managed turn ownership", async () => {
    const agent = await createAgent();
    const lifecycle = await managedRealtime(agent, "start", {
      operation_id: "voice-delegation-start",
      voice_session_id: "voice-delegation-session",
    });
    expect(lifecycle.status).toBe(200);
    const first = await managedRealtime(agent, "delegate", {
      input: "First realtime delegation",
      operation_id: "voice-delegate-started",
      voice_session_id: "voice-delegation-session",
    });
    const started = await first.json<ManagedRealtimeRouteResponse>();
    expect(first.status, JSON.stringify(started)).toBe(202);
    expect(started).toMatchObject({
      operation_id: "voice-delegate-started",
      route: "started",
      voice_session_id: "voice-delegation-session",
    });
    expect(started.turn_id).toMatch(/^realtime:[0-9a-f]{48}$/);

    const second = await managedRealtime(agent, "delegate", {
      input: "Second realtime delegation steers the active work",
      operation_id: "voice-delegate-steered",
      voice_session_id: "voice-delegation-session",
    });
    expect(second.status).toBe(202);
    const steered = await second.json<ManagedRealtimeRouteResponse>();
    expect(steered).toEqual({
      operation_id: "voice-delegate-steered",
      route: "steered",
      turn_id: started.turn_id,
      voice_session_id: "voice-delegation-session",
    });

    await waitForHistoryEvent(agent, ({ event, turn_id }) => (
      event?.type === "run.steered" && turn_id === started.turn_id
    ));
    await waitForTurnState(agent, started.turn_id, "completed");
    const history = await managedHistory(agent);
    expect(history.data).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "turn_accepted", turn_id: started.turn_id }),
      expect.objectContaining({ event: expect.objectContaining({ type: "run.started" }), turn_id: started.turn_id }),
      expect.objectContaining({ event: expect.objectContaining({ type: "run.completed" }), turn_id: started.turn_id }),
      expect.objectContaining({ type: "turn_completed", turn_id: started.turn_id }),
    ]));

    const duplicateSteer = await managedRealtime(agent, "delegate", {
      input: "Second realtime delegation steers the active work",
      operation_id: "voice-delegate-steered",
      voice_session_id: "voice-delegation-session",
    });
    expect(await duplicateSteer.json()).toEqual(steered);
    const conflict = await managedRealtime(agent, "delegate", {
      input: "Different input cannot reuse the operation identity",
      operation_id: "voice-delegate-steered",
      voice_session_id: "voice-delegation-session",
    });
    expect(conflict.status).toBe(409);
    expect(await conflict.json()).toMatchObject({ error: "idempotency_conflict" });
  });

  it("rejects an in-flight realtime identity conflict before joining its promise", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalBroker = testEnv.NANOCODEX;
    let releaseUpgrade!: () => void;
    const heldUpgrade = new Promise<void>((resolve) => { releaseUpgrade = resolve; });
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          await heldUpgrade;
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      const started = managedRealtime(agent, "start", {
        operation_id: "voice-inflight-conflict",
        voice_session_id: "voice-inflight-session",
      });
      for (let attempt = 0; attempt < 100; attempt += 1) {
        const pending = await runInDurableObject(
          session,
          (_instance, state) => state.storage.sql.exec<{ count: number }>(
            "SELECT COUNT(*) AS count FROM managed_realtime_operations WHERE state = 'pending'",
          ).one().count,
        );
        if (pending === 1) break;
        await scheduler.wait(5);
      }
      const conflict = await managedRealtime(agent, "stop", {
        operation_id: "voice-inflight-conflict",
        voice_session_id: "voice-inflight-session",
      });
      expect(conflict.status).toBe(409);
      expect(await conflict.json()).toMatchObject({ error: "idempotency_conflict" });
      releaseUpgrade();
      expect((await started).status).toBe(200);
    } finally {
      releaseUpgrade();
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("bounds unique pending realtime operations before retaining more work", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    await runInDurableObject(session, (_instance, state) => {
      for (let index = 0; index < 32; index += 1) {
        state.storage.sql.exec(
          `INSERT INTO managed_realtime_operations (
             voice_session_id, operation_id, kind, request_hash, state,
             response_json, created_at, updated_at
           ) VALUES (?, ?, 'start', ?, 'pending', NULL, ?, ?)`,
          `voice-cap-${index}`,
          `operation-cap-${index}`,
          "0".repeat(64),
          Date.now(),
          Date.now(),
        );
      }
    });
    const rejected = await managedRealtime(agent, "start", {
      operation_id: "operation-over-cap",
      voice_session_id: "voice-over-cap",
    });
    expect(rejected.status).toBe(429);
    expect(await rejected.json()).toMatchObject({ error: "realtime_queue_full" });
  });

  it("fences stale managed voice sessions and never replays a pending mutation", async () => {
    const agent = await createAgent();
    const first = await managedRealtime(agent, "start", {
      operation_id: "voice-lease-first-start",
      voice_session_id: "voice-lease-first",
    });
    expect(first.status).toBe(200);

    const repeatedIdentity = await managedRealtime(agent, "start", {
      operation_id: "voice-lease-first-start-again",
      voice_session_id: "voice-lease-first",
    });
    expect(repeatedIdentity.status).toBe(409);
    expect(await repeatedIdentity.json()).toMatchObject({ error: "voice_session_active" });

    const replacement = await managedRealtime(agent, "start", {
      operation_id: "voice-lease-second-start",
      voice_session_id: "voice-lease-second",
    });
    expect(replacement.status).toBe(200);

    const staleDelegate = await managedRealtime(agent, "delegate", {
      input: "stale voice must not route",
      operation_id: "voice-lease-stale-delegate",
      voice_session_id: "voice-lease-first",
    });
    expect(staleDelegate.status).toBe(409);
    expect(await staleDelegate.json()).toMatchObject({ error: "voice_session_inactive" });

    const staleStop = await managedRealtime(agent, "stop", {
      operation_id: "voice-lease-stale-stop",
      voice_session_id: "voice-lease-first",
    });
    expect(staleStop.status).toBe(200);
    expect(await staleStop.json()).toMatchObject({ stale: true, stopped: false });

    const pendingRequestHash = await testHash(JSON.stringify({
      kind: "stop",
      operation_id: "voice-lease-pending-stop",
      voice_session_id: "voice-lease-second",
    }));
    await runInDurableObject(
      testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id),
      (_instance, state) => state.storage.sql.exec(
        `INSERT INTO managed_realtime_operations (
           voice_session_id, operation_id, kind, request_hash, state, response_json, created_at, updated_at
         ) VALUES (?, ?, 'stop', ?, 'pending', NULL, ?, ?)`,
        "voice-lease-second",
        "voice-lease-pending-stop",
        pendingRequestHash,
        Date.now(),
        Date.now(),
      ),
    );
    const pending = await managedRealtime(agent, "stop", {
      operation_id: "voice-lease-pending-stop",
      voice_session_id: "voice-lease-second",
    });
    expect(pending.status).toBe(409);
    expect(await pending.json()).toMatchObject({ error: "operation_pending" });

    const stopped = await managedRealtime(agent, "stop", {
      operation_id: "voice-lease-second-stop",
      voice_session_id: "voice-lease-second",
    });
    expect(stopped.status).toBe(200);
    expect(await stopped.json()).toMatchObject({ stopped: true });
  });

  it("forwards one owner-asserted session request and overwrites caller assertions", async () => {
    const agent = await createAgent();
    const stateUrl = agent.events_url.replace(/\/events$/, "");
    const originalFetch = NanocodexSession.prototype.fetch;
    const forwarded: Array<{ owner: string | null; path: string }> = [];
    const fetchSpy = vi.spyOn(NanocodexSession.prototype, "fetch").mockImplementation(
      async function (this: NanocodexSession, request: Request): Promise<Response> {
        forwarded.push({
          owner: request.headers.get("x-nanocodex-owner-id"),
          path: new URL(request.url).pathname,
        });
        return originalFetch.call(this, request);
      },
    );
    try {
      const owner = await SELF.fetch(stateUrl, {
        headers: { "x-nanocodex-owner-id": OTHER_USER_ID },
      });
      expect(owner.status).toBe(200);
      expect(forwarded).toEqual([{ owner: USER_ID, path: "/state" }]);

      forwarded.length = 0;
      const other = await RAW_SELF.fetch(stateUrl, {
        headers: {
          authorization: `Bearer ${OTHER_API_KEY}`,
          "x-nanocodex-owner-id": USER_ID,
        },
      });
      expect(other.status).toBe(404);
      expect(await other.json()).toEqual({ error: "not_found" });
      expect(forwarded).toEqual([{ owner: OTHER_USER_ID, path: "/state" }]);
    } finally {
      fetchSpy.mockRestore();
    }
  });

  it("starts credential binding and session initialization before either settles", async () => {
    const originalBroker = testEnv.NANOCODEX;
    const originalSessions = testEnv.NANOCODEX_SESSIONS;
    const started = new Set<"binding" | "initialization">();
    let release!: () => void;
    const bothStarted = new Promise<void>((resolve) => { release = resolve; });
    const markStarted = async (operation: "binding" | "initialization") => {
      started.add(operation);
      if (started.size === 2) release();
      await bothStarted;
    };
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.method === "PUT" && new URL(request.url).pathname.startsWith("/subjects/")) {
          await markStarted("binding");
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;
    testEnv.NANOCODEX_SESSIONS = {
      idFromName(name: string) {
        return originalSessions.idFromName(name);
      },
      getByName(name: string) {
        const session = originalSessions.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === "PUT" && new URL(request.url).pathname === "/initialize") {
              await markStarted("initialization");
            }
            return session.fetch(input, init);
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_SESSIONS"];

    try {
      const response = await SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      expect(response.status).toBe(201);
      const receipt = await response.json<AgentReceipt>();
      createdAgents.add(receipt.agent_id);
      expect(started).toEqual(new Set(["binding", "initialization"]));
    } finally {
      testEnv.NANOCODEX = originalBroker;
      testEnv.NANOCODEX_SESSIONS = originalSessions;
    }
  });

  it("fences a held bind and compensates it before watchdog cleanup completes", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const originalBroker = testEnv.NANOCODEX;
    let subject: string | undefined;
    let bindStarted!: () => void;
    let releaseBind!: () => void;
    const started = new Promise<void>((resolve) => { bindStarted = resolve; });
    const release = new Promise<void>((resolve) => { releaseBind = resolve; });
    let unbinds = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const match = new URL(request.url).pathname.match(/^\/subjects\/([A-Za-z0-9_-]+)$/);
        if (match && request.method === "PUT") {
          subject = match[1];
          bindStarted();
          await release;
        }
        if (match && request.method === "DELETE") unbinds += 1;
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    try {
      const creation = SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      await within(started, "held credential bind");
      const session = sessionForSubject(subject!);
      expireCredentialPreparation();
      const alarm = runDurableObjectAlarm(session);
      await waitForCleanupDeletion(session);
      releaseBind();

      expect(await alarm).toBe(true);
      const response = await creation;
      expect(response.status).toBe(503);
      expect(unbinds).toBe(1);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });
    } finally {
      releaseBind?.();
      testEnv.NANOCODEX = originalBroker;
      vi.useRealTimers();
    }
  });

  it("fences a held account attach and detaches it before watchdog cleanup completes", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const originalBroker = testEnv.NANOCODEX;
    const originalUserFetch = UserAccount.prototype.fetch;
    let subject: string | undefined;
    let attachStarted!: () => void;
    let releaseAttach!: () => void;
    const started = new Promise<void>((resolve) => { attachStarted = resolve; });
    const release = new Promise<void>((resolve) => { releaseAttach = resolve; });
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const match = new URL(request.url).pathname.match(/^\/subjects\/([A-Za-z0-9_-]+)$/);
        if (match && request.method === "PUT") subject = match[1];
        return originalBroker.fetch(request);
      },
    } as Fetcher;
    const attachSpy = vi.spyOn(UserAccount.prototype, "fetch").mockImplementation(
      async function (this: UserAccount, request: Request): Promise<Response> {
        if (request.method === "POST" && new URL(request.url).pathname === "/agents") {
          attachStarted();
          await release;
        }
        return originalUserFetch.call(this, request);
      },
    );

    try {
      const creation = SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      await within(started, "held account attach");
      const session = sessionForSubject(subject!);
      expireCredentialPreparation();
      const alarm = runDurableObjectAlarm(session);
      await waitForCleanupDeletion(session);
      releaseAttach();

      expect(await alarm).toBe(true);
      expect((await creation).status).toBe(503);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });
    } finally {
      releaseAttach?.();
      attachSpy.mockRestore();
      testEnv.NANOCODEX = originalBroker;
      vi.useRealTimers();
    }
  });

  it("bounds a nonsettling bind and completes watchdog-owned cleanup", async () => {
    const originalBroker = testEnv.NANOCODEX;
    const originalTimeout = testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS;
    let subject: string | undefined;
    let bindStarted!: () => void;
    const started = new Promise<void>((resolve) => { bindStarted = resolve; });
    testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = "25";
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const match = new URL(request.url).pathname.match(/^\/subjects\/([A-Za-z0-9_-]+)$/);
        if (match && request.method === "PUT") {
          subject = match[1];
          bindStarted();
          return new Promise<Response>(() => {});
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      const creation = SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      await within(started, "nonsettling credential bind");
      const response = await within(creation, "bounded credential bind");
      expect(response.status).toBe(503);
      const session = sessionForSubject(subject!);
      await runCleanupAlarmsUntilDeleted(session);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });
    } finally {
      testEnv.NANOCODEX = originalBroker;
      testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = originalTimeout;
    }
  });

  it("makes a nonsettling cold rebind retryable on its bounded deadline", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    await evictDurableObject(session);
    const originalBroker = testEnv.NANOCODEX;
    const originalTimeout = testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS;
    let bindStarted!: () => void;
    const started = new Promise<void>((resolve) => { bindStarted = resolve; });
    testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = "25";
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.method === "PUT" && new URL(request.url).pathname.startsWith("/subjects/")) {
          bindStarted();
          return new Promise<Response>(() => {});
        }
        return originalBroker.fetch(input, init);
      },
    } as Fetcher;

    try {
      await submit(agent, "turn-bounded-cold-rebind", "retry after a nonsettling rebind");
      await within(started, "nonsettling cold rebind");
      const retryable = await waitForTurnState(agent, "turn-bounded-cold-rebind", "retryable");
      expect(retryable.attempt_count).toBe(1);
      expect(retryable.error).toMatch(/credential subject binding timed out/i);
      expect(retryable.retry_at).not.toBeNull();
    } finally {
      testEnv.NANOCODEX = originalBroker;
      testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = originalTimeout;
    }
  });

  it("tombstones an agent before a timed-out late account attach can commit", async () => {
    const originalTimeout = testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS;
    const originalUserFetch = UserAccount.prototype.fetch;
    let attachedAgentId: string | undefined;
    let attachStarted!: () => void;
    const started = new Promise<void>((resolve) => { attachStarted = resolve; });
    testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = "25";
    const attachSpy = vi.spyOn(UserAccount.prototype, "fetch").mockImplementation(
      async function (this: UserAccount, request: Request): Promise<Response> {
        if (request.method === "POST" && new URL(request.url).pathname === "/agents") {
          attachedAgentId = (await request.clone().json<{ agentId: string }>()).agentId;
          attachStarted();
          return new Promise<Response>(() => {});
        }
        return originalUserFetch.call(this, request);
      },
    );

    try {
      const creation = SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      await within(started, "nonsettling account attach");
      const response = await within(creation, "bounded account attach");
      expect(response.status).toBe(503);
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });
    } finally {
      attachSpy.mockRestore();
      testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = originalTimeout;
    }

    const lateAttach = await testEnv.NANOCODEX_USERS.getByName(USER_ID).fetch(
      "https://user.internal/agents",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ agentId: attachedAgentId }),
      },
    );
    expect(lateAttach.status).toBe(410);
  });

  it.each([
    ["cleanup preparation after a lost response", "PUT", "/credential-binding", "lost"],
    ["cleanup preparation after HTTP 503", "PUT", "/credential-binding", "unavailable"],
    ["credential binding after a lost response", "POST", "/credential-binding/bind", "lost"],
    ["credential binding after HTTP 503", "POST", "/credential-binding/bind", "unavailable"],
    ["initialization after a lost response", "PUT", "/initialize", "lost"],
    ["initialization after HTTP 503", "PUT", "/initialize", "unavailable"],
    ["cleanup commit after a lost response", "POST", "/credential-binding/commit", "lost"],
    ["cleanup commit after HTTP 503", "POST", "/credential-binding/commit", "unavailable"],
  ])("replays %s", async (_operation, method, path, failure) => {
    const originalSessions = testEnv.NANOCODEX_SESSIONS;
    const attemptedAgentIds: string[] = [];
    const attemptedBodies: string[] = [];
    testEnv.NANOCODEX_SESSIONS = {
      idFromName(name: string) { return originalSessions.idFromName(name); },
      getByName(name: string) {
        const session = originalSessions.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === method && new URL(request.url).pathname === path) {
              attemptedAgentIds.push(name);
              attemptedBodies.push(await request.clone().text());
              if (attemptedAgentIds.length === 1 && failure === "unavailable") {
                return Response.json({ error: "injected_unavailable" }, { status: 503 });
              }
              const response = await session.fetch(request);
              if (attemptedAgentIds.length === 1) {
                await response.body?.cancel();
                throw new Error(`injected lost ${path} response`);
              }
              return response;
            }
            return session.fetch(request);
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_SESSIONS"];

    try {
      const response = await SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      expect(response.status).toBe(201);
      const receipt = await response.json<AgentReceipt>();
      createdAgents.add(receipt.agent_id);
      expect(attemptedAgentIds).toEqual([receipt.agent_id, receipt.agent_id]);
      expect(attemptedBodies[1]).toBe(attemptedBodies[0]);
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toMatchObject({
        data: [receipt.agent_id],
        summaries: { [receipt.agent_id]: { title: "", turn_count: 0 } },
      });
    } finally {
      testEnv.NANOCODEX_SESSIONS = originalSessions;
    }
  });

  it("derives one agent identity from a caller idempotency key", async () => {
    const create = () => SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: { "idempotency-key": "create-request-stable-1" },
    });
    const [first, replay] = await Promise.all([create(), create()]);
    expect(first.status, await first.clone().text()).toBe(201);
    expect(replay.status, await replay.clone().text()).toBe(201);
    const [firstReceipt, replayReceipt] = await Promise.all([
      first.json<AgentReceipt>(),
      replay.json<AgentReceipt>(),
    ]);
    expect(replayReceipt).toEqual(firstReceipt);
    createdAgents.add(firstReceipt.agent_id);
    expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toMatchObject({
      data: [firstReceipt.agent_id],
    });

    const invalid = await SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: { "idempotency-key": "contains a space" },
    });
    expect(invalid.status).toBe(400);
    expect(await invalid.json()).toEqual({ error: "invalid_idempotency_key" });
  });

  it("returns a stable conflict when a deleted create identity is replayed", async () => {
    const create = () => SELF.fetch("https://example.test/v1/agents", {
      method: "POST",
      headers: { "idempotency-key": "create-request-permanently-deleted" },
    });
    const created = await create();
    expect(created.status, await created.clone().text()).toBe(201);
    const receipt = await created.json<AgentReceipt>();
    createdAgents.add(receipt.agent_id);

    const deleted = await SELF.fetch(`https://example.test/v1/agents/${receipt.agent_id}`, {
      method: "DELETE",
    });
    expect(deleted.status).toBe(204);

    const replay = await create();
    expect(replay.status).toBe(409);
    expect(await replay.json()).toEqual({ error: "agent_creation_expired" });
  });

  it("absorbs four transient cleanup-preparation responses", async () => {
    const originalSessions = testEnv.NANOCODEX_SESSIONS;
    let attempts = 0;
    let forwardedStatus: number | undefined;
    testEnv.NANOCODEX_SESSIONS = {
      idFromName(name: string) { return originalSessions.idFromName(name); },
      getByName(name: string) {
        const session = originalSessions.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === "PUT"
              && new URL(request.url).pathname === "/credential-binding") {
              attempts += 1;
              if (attempts < 5) return new Response(null, { status: 503 });
            }
            const response = await session.fetch(request);
            forwardedStatus = response.status;
            return response;
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_SESSIONS"];

    try {
      const response = await SELF.fetch("https://example.test/v1/agents", {
        method: "POST",
        headers: { "idempotency-key": "create-request-transient-prepare" },
      });
      expect(
        response.status,
        `${await response.clone().text()} attempts=${attempts} forwarded=${forwardedStatus}`,
      ).toBe(201);
      const receipt = await response.json<AgentReceipt>();
      createdAgents.add(receipt.agent_id);
      expect(attempts).toBe(5);
    } finally {
      testEnv.NANOCODEX_SESSIONS = originalSessions;
    }
  });

  it("resumes a keyed create after an exhausted cleanup-commit stage", async () => {
    const originalSessions = testEnv.NANOCODEX_SESSIONS;
    const attemptedAgentIds: string[] = [];
    let commitAttempts = 0;
    testEnv.NANOCODEX_SESSIONS = {
      idFromName(name: string) { return originalSessions.idFromName(name); },
      getByName(name: string) {
        const session = originalSessions.getByName(name);
        return {
          async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
            const request = new Request(input, init);
            if (request.method === "POST"
              && new URL(request.url).pathname === "/credential-binding/commit") {
              commitAttempts += 1;
              attemptedAgentIds.push(name);
              if (commitAttempts <= 5) return new Response(null, { status: 503 });
            }
            return session.fetch(request);
          },
        } as DurableObjectStub;
      },
    } as Env["NANOCODEX_SESSIONS"];

    try {
      const create = () => SELF.fetch("https://example.test/v1/agents", {
        method: "POST",
        headers: { "idempotency-key": "create-request-resume-commit" },
      });
      const interrupted = await create();
      expect(interrupted.status).toBe(503);
      expect(await interrupted.json()).toEqual({ error: "agent cleanup commit failed" });

      const resumed = await create();
      expect(resumed.status, await resumed.clone().text()).toBe(201);
      const receipt = await resumed.json<AgentReceipt>();
      createdAgents.add(receipt.agent_id);
      expect(commitAttempts).toBe(6);
      expect(new Set(attemptedAgentIds)).toEqual(new Set([receipt.agent_id]));
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toMatchObject({
        data: [receipt.agent_id],
      });
    } finally {
      testEnv.NANOCODEX_SESSIONS = originalSessions;
    }
  });

  it("retains durable cleanup ownership when binding and the first unbind both fail", async () => {
    const originalBroker = testEnv.NANOCODEX;
    let session: DurableObjectStub<NanocodexSession> | undefined;
    let subject: string | undefined;
    let unbindAttempts = 0;
    let rejectUnbind = true;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const subjectRoute = new URL(request.url).pathname.startsWith("/subjects/");
        if (subjectRoute && request.method === "PUT") {
          subject = new URL(request.url).pathname.split("/").at(-1);
          return Response.json({ error: "injected_bind_failure" }, { status: 503 });
        }
        if (subjectRoute && request.method === "DELETE") {
          unbindAttempts += 1;
          if (rejectUnbind) {
            return Response.json({ error: "injected_unbind_failure" }, { status: 503 });
          }
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    try {
      const response = await SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({ error: "credential_broker_unavailable" });
      session = sessionForSubject(subject!);
      expect(unbindAttempts).toBe(1);
      expect(await cleanupMarkers(session!)).toEqual({ binding: true, deleting: true });

      rejectUnbind = false;
      expect(await runDurableObjectAlarm(session!)).toBe(true);
      expect(unbindAttempts).toBe(2);
      expect(await cleanupMarkers(session!)).toEqual({ binding: false, deleting: false });
      const unbound = await originalBroker.fetch("https://nanocodex.internal/v1/search", {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "content-type": "application/json",
          "x-nanocodex-subject": subject!,
        },
        body: "{}",
      });
      expect(unbound.status).toBe(403);

      const replay = await session!.fetch("https://session.internal/session", { method: "DELETE" });
      expect(replay.status).toBe(204);
      expect(unbindAttempts).toBe(2);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("keeps a bound subject owned after initialization and first cleanup fail", async () => {
    const originalBroker = testEnv.NANOCODEX;
    let session: DurableObjectStub<NanocodexSession> | undefined;
    let subject: string | undefined;
    let unbindAttempts = 0;
    let rejectUnbind = true;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const match = new URL(request.url).pathname.match(/^\/subjects\/([A-Za-z0-9_-]+)$/);
        if (match && request.method === "PUT") subject = match[1];
        if (match && request.method === "DELETE") {
          unbindAttempts += 1;
          if (rejectUnbind) {
            return Response.json({ error: "injected_unbind_failure" }, { status: 503 });
          }
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;
    const originalFetch = NanocodexSession.prototype.fetch;
    const fetchSpy = vi.spyOn(NanocodexSession.prototype, "fetch").mockImplementation(
      async function (this: NanocodexSession, request: Request): Promise<Response> {
        if (request.method === "PUT" && new URL(request.url).pathname === "/initialize") {
          return Response.json({ error: "injected_initialize_failure" }, { status: 503 });
        }
        return originalFetch.call(this, request);
      },
    );

    try {
      const response = await SELF.fetch("https://example.test/v1/agents", { method: "POST" });
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({ error: "agent initialization failed" });
      expect(fetchSpy.mock.calls.map(([request]) => (
        `${request.method} ${new URL(request.url).pathname}`
      ))).toContain("DELETE /session");
      expect(subject).toMatch(/^[A-Za-z0-9_-]{43,128}$/);
      session = sessionForSubject(subject!);
      expect(await cleanupMarkers(session!)).toEqual({ binding: true, deleting: true });
      expect(unbindAttempts).toBe(1);

      const stillBound = await originalBroker.fetch("https://nanocodex.internal/v1/search", {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "content-type": "application/json",
          "x-nanocodex-subject": subject!,
        },
        body: "{}",
      });
      expect(stillBound.status).toBe(200);

      rejectUnbind = false;
      expect(await runDurableObjectAlarm(session!)).toBe(true);
      expect(unbindAttempts).toBe(2);
      expect(await cleanupMarkers(session!)).toEqual({ binding: false, deleting: false });

      const replay = await session!.fetch("https://session.internal/session", { method: "DELETE" });
      expect(replay.status).toBe(204);
      expect(unbindAttempts).toBe(2);
    } finally {
      fetchSpy.mockRestore();
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("retries failed public delete cleanup from the durable marker idempotently", async () => {
    const agent = await createAgent();
    const originalBroker = testEnv.NANOCODEX;
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    let unbindAttempts = 0;
    let rejectUnbind = true;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.method === "DELETE"
          && new URL(request.url).pathname.startsWith("/subjects/")) {
          unbindAttempts += 1;
          if (rejectUnbind) {
            return Response.json({ error: "injected_unbind_failure" }, { status: 503 });
          }
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    try {
      const deleted = await SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
      });
      expect(deleted.status).toBe(503);
      expect(await deleted.json()).toEqual({ error: "session_cleanup_pending" });
      expect(unbindAttempts).toBe(1);
      expect(await cleanupMarkers(session)).toEqual({ binding: true, deleting: true });

      rejectUnbind = false;
      expect(await runDurableObjectAlarm(session)).toBe(true);
      createdAgents.delete(agent.agent_id);
      expect(unbindAttempts).toBe(2);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });

      const replay = await session.fetch("https://session.internal/session", { method: "DELETE" });
      expect(replay.status).toBe(204);
      expect(unbindAttempts).toBe(2);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("rebinds a retained pre-marker session before every cold model transport", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const originalBroker = testEnv.NANOCODEX;

    const unbound = await originalBroker.fetch(`https://broker.internal/subjects/${subject}`, {
      method: "DELETE",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ user_id: USER_ID }),
    });
    expect(unbound.status).toBe(204);
    await runInDurableObject(session, async (_instance, state) => {
      await state.storage.delete("nanocodex:credential-binding");
    });
    expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
    const missing = await originalBroker.fetch("https://nanocodex.internal/v1/search", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "content-type": "application/json",
        "x-nanocodex-subject": subject,
      },
      body: "{}",
    });
    expect(missing.status).toBe(403);

    const order: Array<"bind" | "transport" | "unbind"> = [];
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        const url = new URL(request.url);
        if (request.method === "PUT" && url.pathname === `/subjects/${subject}`) {
          order.push("bind");
        }
        if (request.method === "DELETE" && url.pathname === `/subjects/${subject}`) {
          order.push("unbind");
        }
        if (request.method === "GET"
          && url.pathname === "/v1/responses"
          && request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          order.push("transport");
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    try {
      await evictDurableObject(session);
      await submit(agent, "retained-rebind-first", "first retained startup");
      await waitForTurnState(agent, "retained-rebind-first", "completed");
      expect(order).toEqual(["bind", "transport"]);
      expect(await cleanupMarkers(session)).toEqual({ binding: true, deleting: false });

      await evictDurableObject(session);
      await submit(agent, "retained-rebind-second", "second retained startup");
      await waitForTurnState(agent, "retained-rebind-second", "completed");
      expect(order).toEqual(["bind", "transport", "bind", "transport"]);

      const deleted = await SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
      });
      expect(deleted.status).toBe(204);
      createdAgents.delete(agent.agent_id);
      expect(order).toEqual(["bind", "transport", "bind", "transport", "unbind"]);
      const removed = await originalBroker.fetch("https://nanocodex.internal/v1/search", {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "content-type": "application/json",
          "x-nanocodex-subject": subject,
        },
        body: "{}",
      });
      expect(removed.status).toBe(403);
    } finally {
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("derives legacy cleanup ownership from session_state on direct delete", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const subject = testEnv.NANOCODEX_SESSIONS.idFromName(agent.agent_id).toString();
    const originalBroker = testEnv.NANOCODEX;
    const originalUserFetch = UserAccount.prototype.fetch;
    let unboundSubject: string | undefined;
    let detachedAgent: string | undefined;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.method === "DELETE") {
          unboundSubject = new URL(request.url).pathname.match(/^\/subjects\/(.+)$/)?.[1];
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;
    const accountSpy = vi.spyOn(UserAccount.prototype, "fetch").mockImplementation(
      async function (this: UserAccount, request: Request): Promise<Response> {
        if (request.method === "DELETE") {
          detachedAgent = new URL(request.url).pathname.match(/^\/agents\/(.+)$/)?.[1];
        }
        return originalUserFetch.call(this, request);
      },
    );

    try {
      await runInDurableObject(session, async (_instance, state) => {
        await state.storage.delete("nanocodex:credential-binding");
      });
      await evictDurableObject(session);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });

      const deleted = await SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
      });
      expect(deleted.status).toBe(204);
      createdAgents.delete(agent.agent_id);
      expect(unboundSubject).toBe(subject);
      expect(detachedAgent).toBe(agent.agent_id);
      expect(await (await SELF.fetch("https://example.test/v1/agents")).json()).toEqual({
        data: [],
        summaries: {},
      });
    } finally {
      accountSpy.mockRestore();
      testEnv.NANOCODEX = originalBroker;
    }
  });

  it("retains fenced deletion ownership and retries after workspace cleanup times out", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalTimeout = testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS;
    testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = "20";

    try {
      const failed = await runInDurableObject(session, async (instance) => {
        const mutable = instance as unknown as { fetch(request: Request): Promise<Response> };
        const workspaceSymbol = Object.getOwnPropertySymbols(instance)
          .find((symbol) => symbol.description === "workspace");
        expect(workspaceSymbol).toBeTruthy();
        const workspace = (instance as unknown as Record<symbol, {
          fs: { rm(...args: unknown[]): Promise<void> };
        }>)[workspaceSymbol!];
        const originalRm = workspace.fs.rm;
        workspace.fs.rm = () => new Promise<void>(() => {});
        try {
          return await within(
            mutable.fetch(new Request("https://session.internal/session", { method: "DELETE" })),
            "workspace deletion attempt",
          );
        } finally {
          workspace.fs.rm = originalRm;
        }
      });
      expect(failed.status).toBe(503);
      expect(await cleanupMarkers(session)).toEqual({ binding: true, deleting: true });
      expect(await runDurableObjectAlarm(session)).toBe(true);
      expect(await cleanupMarkers(session)).toEqual({ binding: false, deleting: false });
      expect(await runInDurableObject(session, (_instance, state) => (
        state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM session_state",
        ).toArray()[0]!.count
      ))).toBe(0);
      createdAgents.delete(agent.agent_id);
    } finally {
      testEnv.MANAGED_OWNERSHIP_IO_TIMEOUT_MS = originalTimeout;
    }
  });

  it("lists only the current user's agents and hides them from other users", async () => {
    const agent = await createAgent();
    const mine = await SELF.fetch("https://example.test/v1/agents");
    expect(mine.status).toBe(200);
    expect(await mine.json()).toEqual({
      data: [agent.agent_id],
      summaries: {
        [agent.agent_id]: expect.objectContaining({ title: "", turn_count: 0 }),
      },
    });

    const other = await RAW_SELF.fetch(agent.events_url.replace(/\/events$/, ""), {
      headers: { authorization: `Bearer ${OTHER_API_KEY}` },
    });
    expect(other.status).toBe(404);
    expect(await other.json()).toEqual({ error: "not_found" });

    const otherList = await RAW_SELF.fetch("https://example.test/v1/agents", {
      headers: { authorization: `Bearer ${OTHER_API_KEY}` },
    });
    expect(await otherList.json()).toEqual({ data: [], summaries: {} });
  });

  it("keeps a large per-account agent registry row-oriented", async () => {
    const account = testEnv.NANOCODEX_USERS.getByName(`registry-${crypto.randomUUID()}`);
    const ids = Array.from({ length: 4_096 }, (_, index) => (
      `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`
    ));
    await runInDurableObject(account, (_instance, state) => {
      const now = Date.now();
      for (const agentId of ids) {
        state.storage.sql.exec(
          `INSERT INTO user_agents
             (id, title, created_at, updated_at, turn_count, deleted_at)
           VALUES (?, '', ?, ?, 0, NULL)`,
          agentId,
          now,
          now,
        );
      }
    });

    const listed = await (await account.fetch("https://user.internal/agents"))
      .json<Array<{ id: string }>>();
    expect(listed).toHaveLength(ids.length);
    expect(listed[0]?.id).toBe(ids[0]);
    expect(listed.at(-1)?.id).toBe(ids.at(-1));

    expect((await account.fetch(`https://user.internal/agents/${ids[2_048]}`, {
      method: "DELETE",
    })).status).toBe(204);
    expect((await account.fetch("https://user.internal/agents", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agentId: ids[2_048] }),
    })).status).toBe(410);
  });

  it("adopts the aggregate account registry without losing deletion fences", async () => {
    const account = testEnv.NANOCODEX_USERS.getByName(`registry-migration-${crypto.randomUUID()}`);
    const activeId = "10000000-0000-4000-8000-000000000001";
    const deletedId = "10000000-0000-4000-8000-000000000002";
    await runInDurableObject(account, async (_instance, state) => {
      await state.storage.put({
        agents: [activeId],
        agentSummaries: {
          [activeId]: {
            id: activeId,
            title: "Retained title",
            createdAt: 10,
            updatedAt: 20,
            turnCount: 3,
          },
        },
        [`agent-tombstone:${deletedId}`]: true,
      });
    });
    await evictDurableObject(account);

    expect(await (await account.fetch("https://user.internal/agents")).json()).toEqual([{
      id: activeId,
      title: "Retained title",
      createdAt: 10,
      updatedAt: 20,
      turnCount: 3,
    }]);
    expect((await account.fetch("https://user.internal/agents", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agentId: deletedId }),
    })).status).toBe(410);
    expect(await runInDurableObject(account, async (_instance, state) => ({
      agents: await state.storage.get("agents"),
      summaries: await state.storage.get("agentSummaries"),
      tombstone: await state.storage.get(`agent-tombstone:${deletedId}`),
    }))).toEqual({ agents: undefined, summaries: undefined, tombstone: undefined });
  });

  it("lists durable conversation summaries without probing every agent session", async () => {
    const agent = await createAgent();
    await submit(agent, "summary-turn", "Build the measured thing");
    const duplicate = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "request-summary-turn",
      },
      body: JSON.stringify({ id: "summary-turn", input: "Build the measured thing" }),
    });
    expect(duplicate.status).toBe(200);
    let summary: { title?: string; turn_count?: number } | undefined;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      const listed = await (await SELF.fetch("https://example.test/v1/agents")).json<{
        summaries: Record<string, { title: string; turn_count: number }>;
      }>();
      summary = listed.summaries[agent.agent_id];
      if (summary?.turn_count === 1) break;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    expect(summary).toMatchObject({ title: "Build the measured thing", turn_count: 1 });
  });

  it("carries Unicode conversation summaries through an ASCII-only internal header", async () => {
    const agent = await createAgent();
    const prompt = "Ship 🦀 a durable conversation title that is deliberately longer than fifty-six characters";
    const response = await testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id).fetch(
      "https://session.internal/turns?public_origin=https%3A%2F%2Fexample.test",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "idempotency-key": "request-unicode-summary-turn",
          "x-nanocodex-owner-id": USER_ID,
        },
        body: JSON.stringify({ id: "unicode-summary-turn", input: prompt }),
      },
    );

    expect(response.status).toBe(202);
    const header = response.headers.get("x-nanocodex-turn-summary");
    expect(header).toMatch(/^[\x20-\x7e]+$/);
    expect(JSON.parse(header!)).toMatchObject({
      title: expect.stringMatching(/^Ship 🦀 .+…$/u),
      turnCount: 1,
    });
    await response.body?.cancel();
  });

  it("reports acceptance for prompts containing lone UTF-16 surrogates", async () => {
    const agent = await createAgent();
    const response = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "request-surrogate-turn",
      },
      body: JSON.stringify({ id: "surrogate-turn", input: "title \ud800 tail" }),
    });

    expect(response.status).toBe(202);
    expect(await response.json<ManagedTurnView>()).toMatchObject({ turn_id: "surrogate-turn" });
  });

  it("requires stable identifiers and strictly validates structured prompt content", async () => {
    const agent = await createAgent();
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const missingIdentifier = await SELF.fetch(turnsUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ input: "hello" }),
    });
    expect(missingIdentifier.status).toBe(400);
    expect(await missingIdentifier.json()).toMatchObject({ error: "idempotency_required" });

    const invalidInputs: unknown[] = [
      " ",
      [],
      [{ type: "text", text: "hello", extra: true }],
      [{ type: "image", image_url: "https://example.test/image.png", detail: "huge" }],
      [{ type: "audio" }],
      [{ type: "video", video_url: "https://example.test/video.mp4" }],
    ];
    for (const [index, input] of invalidInputs.entries()) {
      const response = await SELF.fetch(turnsUrl, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id: `invalid-${index}`, input }),
      });
      expect(response.status).toBe(400);
      expect(await response.json()).toMatchObject({
        error: index === 0 ? "empty_prompt" : "invalid_prompt",
      });
    }
  });

  it("atomically accepts turns and binds idempotency keys to normalized input", async () => {
    const agent = await createAgent();
    const request = {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "incoming-request-7",
      },
      body: JSON.stringify({ id: "turn-7", input: "write hello.txt" }),
    } satisfies RequestInit;

    const accepted = await SELF.fetch(`${agent.events_url.replace(/\/events$/, "/turns")}`, request);
    expect(accepted.status).toBe(202);
    const first = await accepted.json<ManagedTurnView>();
    expect(first).toMatchObject({
      turn_id: "turn-7",
      state: "accepted",
      input: "write hello.txt",
      terminal_cursor: null,
    });
    expect(BigInt(first.accepted_cursor)).toBeGreaterThan(0n);

    const replay = await SELF.fetch(`${agent.events_url.replace(/\/events$/, "/turns")}`, request);
    expect(replay.status).toBe(200);
    expect(await replay.json()).toMatchObject({
      turn_id: "turn-7",
      accepted_cursor: first.accepted_cursor,
    });

    const conflict = await SELF.fetch(agent.events_url.replace(/\/events$/, "/turns"), {
      ...request,
      body: JSON.stringify({ id: "turn-7", input: "different input" }),
    });
    expect(conflict.status).toBe(409);
    expect(await conflict.json()).toMatchObject({ error: "idempotency_conflict" });

    const state = await SELF.fetch(
      agent.events_url.replace(/\/events$/, "/turns/turn-7"),
    );
    expect(state.status).toBe(200);
    expect(await state.json()).toMatchObject({
      turn_id: "turn-7",
      input: "write hello.txt",
      accepted_cursor: first.accepted_cursor,
    });
  });

  it("does not allow a turn id or idempotency key to be aliased", async () => {
    const agent = await createAgent();
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const firstRequest = {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "stable-key",
      },
      body: JSON.stringify({ id: "stable-turn", input: "hello" }),
    } satisfies RequestInit;
    expect((await SELF.fetch(turnsUrl, firstRequest)).status).toBe(202);

    const changedKey = await SELF.fetch(turnsUrl, {
      ...firstRequest,
      headers: {
        "content-type": "application/json",
        "idempotency-key": "different-key",
      },
    });
    expect(changedKey.status).toBe(409);
    expect(await changedKey.json()).toMatchObject({ error: "idempotency_conflict" });

    const changedId = await SELF.fetch(turnsUrl, {
      ...firstRequest,
      body: JSON.stringify({ id: "different-turn", input: "hello" }),
    });
    expect(changedId.status).toBe(409);
    expect(await changedId.json()).toMatchObject({ error: "idempotency_conflict" });

    const generated = await SELF.fetch(turnsUrl, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "generated-turn-key",
      },
      body: JSON.stringify({ input: "generated id" }),
    });
    expect(generated.status).toBe(202);
    const generatedTurn = await generated.json<ManagedTurnView>();
    const replay = await SELF.fetch(turnsUrl, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": "generated-turn-key",
      },
      body: JSON.stringify({ input: "generated id" }),
    });
    expect(replay.status).toBe(200);
    expect(await replay.json()).toMatchObject({ turn_id: generatedTurn.turn_id });
  });

  it("serializes truly concurrent duplicate, conflicting, and aliased submissions", async () => {
    const agent = await createAgent();
    const turnsUrl = agent.events_url.replace(/\/events$/, "/turns");
    const request = (id: string | undefined, input: string, key: string) => ({
      method: "POST",
      headers: {
        "content-type": "application/json",
        "idempotency-key": key,
      },
      body: JSON.stringify({ ...(id === undefined ? {} : { id }), input }),
    } satisfies RequestInit);

    const duplicates = await Promise.all(Array.from({ length: 8 }, () => (
      SELF.fetch(turnsUrl, request("concurrent-same", "same input", "concurrent-same-key"))
    )));
    expect(duplicates.map(({ status }) => status).sort()).toEqual([
      200, 200, 200, 200, 200, 200, 200, 202,
    ]);
    const duplicateViews = await Promise.all(
      duplicates.map((response) => response.json<ManagedTurnView>()),
    );
    expect(new Set(duplicateViews.map(({ accepted_cursor }) => accepted_cursor))).toEqual(
      new Set([duplicateViews[0]!.accepted_cursor]),
    );

    const conflictingInput = await Promise.all([
      SELF.fetch(turnsUrl, request("concurrent-conflict", "left", "concurrent-conflict-key")),
      SELF.fetch(turnsUrl, request("concurrent-conflict", "right", "concurrent-conflict-key")),
    ]);
    expect(conflictingInput.map(({ status }) => status).sort()).toEqual([202, 409]);
    const conflictBody = await conflictingInput.find(({ status }) => status === 409)!.json();
    expect(conflictBody).toMatchObject({ error: "idempotency_conflict" });
    const persistedConflict = await (
      await SELF.fetch(`${turnsUrl}/concurrent-conflict`)
    ).json<ManagedTurnView>();
    expect(["left", "right"]).toContain(persistedConflict.input);

    const changedKeys = await Promise.all([
      SELF.fetch(turnsUrl, request("concurrent-key-alias", "one turn", "first-key")),
      SELF.fetch(turnsUrl, request("concurrent-key-alias", "one turn", "second-key")),
    ]);
    expect(changedKeys.map(({ status }) => status).sort()).toEqual([202, 409]);

    const changedIds = await Promise.all([
      SELF.fetch(turnsUrl, request("concurrent-id-left", "one request", "shared-key")),
      SELF.fetch(turnsUrl, request("concurrent-id-right", "one request", "shared-key")),
    ]);
    expect(changedIds.map(({ status }) => status).sort()).toEqual([202, 409]);

    const generated = await Promise.all(Array.from({ length: 4 }, () => (
      SELF.fetch(turnsUrl, request(undefined, "generated once", "generated-concurrent-key"))
    )));
    expect(generated.map(({ status }) => status).sort()).toEqual([200, 200, 200, 202]);
    const generatedViews = await Promise.all(generated.map((response) => response.json<ManagedTurnView>()));
    expect(new Set(generatedViews.map(({ turn_id }) => turn_id))).toEqual(
      new Set([generatedViews[0]!.turn_id]),
    );

    const history = await (
      await SELF.fetch(`${agent.events_url}/history?limit=256`)
    ).json<{ data: Array<{ type: string; turn_id?: string }> }>();
    const acceptances = history.data.filter(({ type }) => type === "turn_accepted");
    expect(acceptances.filter(({ turn_id }) => turn_id === "concurrent-same")).toHaveLength(1);
    expect(acceptances.filter(({ turn_id }) => turn_id === "concurrent-conflict")).toHaveLength(1);
    expect(acceptances.filter(({ turn_id }) => turn_id === "concurrent-key-alias")).toHaveLength(1);
    expect(acceptances.filter(({ turn_id }) => (
      turn_id === "concurrent-id-left" || turn_id === "concurrent-id-right"
    ))).toHaveLength(1);
    expect(acceptances.filter(({ turn_id }) => turn_id === generatedViews[0]!.turn_id)).toHaveLength(1);
  });

  it("persists cancellation intent and its resumable event before acknowledging", async () => {
    const agent = await createAgent();
    const accepted = await submit(agent, "turn-cancel", "wait for cancellation");
    const events = sseReader(await SELF.fetch(`${agent.events_url}?cursor=${accepted.accepted_cursor}`));

    const cancelled = await SELF.fetch(
      agent.events_url.replace(/\/events$/, "/turns/turn-cancel/cancel"),
      { method: "POST" },
    );
    expect(cancelled.status).toBe(202);
    expect(await cancelled.json()).toEqual({ turn_id: "turn-cancel", state: "cancelling" });

    let event;
    do {
      event = await nextWithin(events, "durable cancellation intent");
    } while (event.data.type !== "turn_cancelling");
    expect(event).toMatchObject({
      id: event.data.cursor,
      event: "turn_cancelling",
      data: { id: "turn-cancel", turn_id: "turn-cancel", type: "turn_cancelling" },
    });
    await events.cancel();

    const state = await SELF.fetch(
      agent.events_url.replace(/\/events$/, "/turns/turn-cancel"),
    );
    expect(state.status).toBe(200);
    expect(["cancelling", "cancelled"]).toContain(
      (await state.json<ManagedTurnView>()).state,
    );
  });

  it("uses Last-Event-ID before the query cursor and rejects cursors ahead of storage", async () => {
    const agent = await createAgent();
    await submit(agent, "turn-a", "alpha");

    const response = await SELF.fetch(`${agent.events_url}?cursor=not-a-cursor`, {
      headers: { "last-event-id": "0" },
    });
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("text/event-stream; charset=utf-8");
    expect(response.headers.get("cache-control")).toContain("no-store");
    const stream = sseReader(response);
    let accepted;
    do {
      accepted = await nextWithin(stream, "turn acceptance");
    } while (accepted.data.type !== "turn_accepted");
    expect(accepted).toMatchObject({
      id: accepted.data.cursor,
      event: "turn_accepted",
      data: { id: "turn-a", type: "turn_accepted" },
    });
    await stream.cancel();

    const invalid = await SELF.fetch(`${agent.events_url}?cursor=-1`);
    expect(invalid.status).toBe(400);
    expect(await invalid.json()).toEqual({ error: "invalid_cursor" });

    const ahead = await SELF.fetch(`${agent.events_url}?cursor=9223372036854775807`);
    expect(ahead.status).toBe(409);
    expect(await ahead.json()).toMatchObject({ error: "cursor_ahead" });
  });

  it("tails atomically from the latest durable cursor", async () => {
    const agent = await createAgent();
    const response = await SELF.fetch(`${agent.events_url}?cursor=latest`);
    expect(response.status).toBe(200);
    const stream = sseReader(response);
    await submit(agent, "turn-latest", "after tail attachment");

    let accepted;
    do {
      accepted = await nextWithin(stream, "latest turn acceptance");
    } while (accepted.data.type !== "turn_accepted");
    expect(accepted).toMatchObject({
      id: accepted.data.cursor,
      event: "turn_accepted",
      data: { id: "turn-latest", type: "turn_accepted" },
    });
    await stream.cancel();
  });

  it("resumes exactly once from the concrete latest control cursor after disconnect", async () => {
    const agent = await createAgent();
    const disconnect = new AbortController();
    const latest = sseFrameReader(await SELF.fetch(`${agent.events_url}?cursor=latest`, {
      signal: disconnect.signal,
    }));
    const controlFrame = await within(latest.nextFrame(), "latest control cursor");
    const cursor = controlFrame.match(/(?:^|\n): cursor ([0-9]+)(?:\n|$)/)?.[1];
    expect(cursor).toMatch(/^[0-9]+$/);
    disconnect.abort();
    await latest.cancel();

    const accepted = await submit(agent, "turn-latest-resume", "resume me exactly once");

    const resumed = sseReader(await SELF.fetch(`${agent.events_url}?cursor=ignored`, {
      headers: { "last-event-id": cursor! },
    }));
    const progress: Array<{ id: string; data: Record<string, unknown> }> = [];
    while (true) {
      const event = await nextWithin(resumed, "latest cursor resumed progress");
      progress.push(event);
      if (event.data.type === "turn_completed" && event.data.id === "turn-latest-resume") break;
    }
    await resumed.cancel();

    const cursors = progress.map(({ id: eventCursor }) => BigInt(eventCursor));
    expect(cursors.every((eventCursor) => eventCursor > BigInt(cursor!))).toBe(true);
    expect(cursors).toEqual([...cursors].sort((left, right) => left < right ? -1 : 1));
    expect(new Set(cursors.map(String)).size).toBe(cursors.length);
    expect(progress.filter(({ data }) => (
      data.type === "turn_accepted" && data.id === "turn-latest-resume"
    ))).toHaveLength(1);
    expect(progress.find(({ data }) => data.type === "turn_accepted")?.id).toBe(
      accepted.accepted_cursor,
    );
    expect(progress.filter(({ data }) => (
      data.type === "turn_completed" && data.id === "turn-latest-resume"
    ))).toHaveLength(1);
  });

  it("does not reuse an idle Agent while alarm shutdown races the next admission", async () => {
    const agent = await createAgent();
    await submit(agent, "turn-before-alarm", "finish before alarm");
    await waitForTurnState(agent, "turn-before-alarm", "completed");
    const id = new URL(agent.events_url).pathname.split("/").at(-2)!;
    const stub = testEnv.NANOCODEX_SESSIONS.getByName(id);
    const originalAlarm = NanocodexSession.prototype.alarm;
    let entered!: () => void;
    const alarmEntered = new Promise<void>((resolve) => { entered = resolve; });
    const alarmSpy = vi.spyOn(NanocodexSession.prototype, "alarm").mockImplementation(
      async function (this: NanocodexSession): Promise<void> {
        entered();
        return originalAlarm.call(this);
      },
    );
    try {
      const alarm = runDurableObjectAlarm(stub);
      await within(alarmEntered, "idle alarm entry");
      const accepted = await submit(agent, "turn-during-alarm", "survive alarm replacement");
      expect(await within(alarm, "concurrent idle alarm")).toBe(true);
      const completed = await waitForTurnState(agent, "turn-during-alarm", "completed");
      expect(completed.accepted_cursor).toBe(accepted.accepted_cursor);

      const history = await (
        await SELF.fetch(`${agent.events_url}/history?limit=256`)
      ).json<{ data: Array<{ type: string; turn_id?: string }> }>();
      expect(history.data.filter(({ type, turn_id }) => (
        turn_id === "turn-during-alarm" && type === "turn_accepted"
      ))).toHaveLength(1);
      expect(history.data.some(({ type, turn_id }) => (
        turn_id === "turn-during-alarm"
          && (type === "turn_failed" || type === "turn_retryable" || type === "turn_blocked")
      ))).toBe(false);
    } finally {
      alarmSpy.mockRestore();
    }
  });

  it("does not idle-shutdown durable accepted work after in-memory ownership is lost", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const id = "turn-cold-alarm-recovery";
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      await runInDurableObject(session, async (instance, state) => {
        const now = Date.now();
        state.storage.sql.exec(
          `INSERT INTO managed_turns (
             id, request_key, request_hash, input_json, state,
             accepted_cursor, may_have_inner_operation, created_at, accepted_at, updated_at
           ) VALUES (?, ?, ?, ?, 'accepted', 1, 0, ?, ?, ?)`,
          id,
          `request-${id}`,
          `hash-${id}`,
          JSON.stringify("recover accepted work before considering idle shutdown"),
          now,
          now,
          now,
        );

        await instance.alarm();
      });

      expect(info.mock.calls.some(([entry]) => (
        entry && typeof entry === "object"
          && (entry as { type?: unknown }).type === "managed.capacity"
          && (entry as { reason?: unknown }).reason === "idle_shutdown"
      ))).toBe(false);
      await waitForTurnState(agent, id, "completed");
    } finally {
      info.mockRestore();
    }
  });

  it("does not construct a replacement after deletion supersedes cold construction", async () => {
    const agent = await createAgent();
    const session = testEnv.NANOCODEX_SESSIONS.getByName(agent.agent_id);
    const originalBroker = testEnv.NANOCODEX;
    let bindingCount = 0;
    let transportCount = 0;
    testEnv.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const request = new Request(input, init);
        if (request.method === "PUT" && new URL(request.url).pathname.startsWith("/subjects/")) {
          bindingCount += 1;
          await scheduler.wait(250);
        }
        if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
          transportCount += 1;
        }
        return originalBroker.fetch(request);
      },
    } as Fetcher;

    try {
      await submit(agent, "turn-delete-during-create", "construction must stay fenced");
      for (let attempt = 0; attempt < 100 && bindingCount === 0; attempt += 1) {
        await scheduler.wait(5);
      }
      expect(bindingCount).toBe(1);
      const deletion = SELF.fetch(`https://example.test/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
      });
      await waitForCleanupDeletion(session);
      expect((await within(deletion, "delete superseded construction")).status).toBe(204);
      createdAgents.delete(agent.agent_id);
      expect(transportCount).toBe(0);
    } finally {
      testEnv.NANOCODEX = originalBroker;
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
      column: state.storage.sql.exec<{ name: string }>(
        "PRAGMA table_info(managed_turns)",
      ).toArray().some(({ name }) => name === "may_have_inner_operation"),
      marker: state.storage.sql.exec<{ may_have_inner_operation: number }>(
        "SELECT may_have_inner_operation FROM managed_turns WHERE id = 'legacy-unfinished-turn'",
      ).one().may_have_inner_operation,
    }))).toEqual({ column: true, marker: 1 });
  });

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
        vi.setSystemTime(turn.retry_at!);
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
    await runInDurableObject(session, (_instance, state) => {
      state.storage.sql.exec(
        `UPDATE managed_turns
         SET state = 'retryable', terminal_json = NULL, terminal_cursor = NULL,
             error = 'injected outer projection gap', retry_at = 0
         WHERE id = ?`,
        replayId,
      );
    });

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
    expect(replayWindow.find(({ event }) => event?.type === "run.completed")).toMatchObject({
      turn_id: replayId,
    });
    expect(replayWindow.find(({ event, turn_id }) => (
      event?.type === "run.started" && turn_id === "turn-after-raw-replay"
    ))).toBeTruthy();
    expect(replayWindow.find(({ event, turn_id }) => (
      event?.type === "run.completed" && turn_id === "turn-after-raw-replay"
    ))).toBeTruthy();
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

async function seedApiKey(userId: string, token: string): Promise<void> {
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
      }),
    },
  );
  expect(record.status).toBe(201);
}

async function seedPasskeySession(userId: string, token: string): Promise<void> {
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
          publicKey: "0x01",
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
): Promise<ManagedTurnView> {
  const deadline = Date.now() + 4_000;
  while (Date.now() < deadline) {
    const response = await SELF.fetch(agent.events_url.replace(/\/events$/, `/turns/${id}`));
    if (response.ok) {
      const turn = await response.json<ManagedTurnView>();
      if (turn.state === expected) return turn;
      if (turn.state === "failed" || turn.state === "blocked") {
        throw new Error(`turn ${id} entered ${turn.state} while waiting for ${expected}`);
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
): Promise<{ id: string; event: string; data: Record<string, unknown> }> {
  return within(reader.next(), stage);
}

async function within<Result>(promise: Promise<Result>, stage: string): Promise<Result> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`timed out waiting for ${stage}`)), 2_000);
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
