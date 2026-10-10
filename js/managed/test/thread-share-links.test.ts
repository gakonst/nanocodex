import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import type { Principal } from "../src/account-auth";
import { DurableEventLog } from "../src/durable-events";
import { threadSharingTools } from "../src/thread-sharing-tool";

const owner: Principal = {
  kind: "account_session", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:11111111-1111-4111-8111-111111111111", credentialId: "test-owner",
  authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
};
const other: Principal = { ...owner, userId: "22222222-2222-4222-8222-222222222222", subjectId: "user:22222222-2222-4222-8222-222222222222" };
let id = crypto.randomUUID();
let secondId = crypto.randomUUID();
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const api = (path: string, method = "GET", actor?: Principal, body?: unknown, token?: string, origin?: string) =>
  worker.fetch(new Request(`https://nanocodex.example${path}`, {
    method, headers: {
      ...(body === undefined ? {} : { "content-type": "application/json" }),
      ...(token === undefined ? {} : { authorization: `Bearer ${token}` }),
      ...(origin === undefined ? {} : { origin }),
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor);

async function seed(agentId = id) {
  await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
    // Since 9d8b63102 a fresh session creates its schema on its first request.
    await session.fetch(new Request("https://session.internal/sites"));
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
      VALUES (1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,
      agentId, owner.userId, owner.organizationId, owner.teamId, Date.now());
    const log = new DurableEventLog<{ type: string; [key: string]: unknown }>(state.storage);
    log.record({ type: "turn_accepted", id: "synthetic-turn", input: "synthetic hello", replayed: false, hidden: "SECRET_ACCEPTED_METADATA" }, "synthetic-turn");
    log.record({ type: "event", event: { type: "tool.result", payload: { call_id: "synthetic-tool", result: "SECRET_TOOL_OUTPUT", secret: "HIDDEN_TOOL_METADATA" } } }, "synthetic-turn");
    log.record({ type: "turn_completed", id: "synthetic-turn", final_message: "synthetic answer", usage: { secret: "SECRET_USAGE" } }, "synthetic-turn");
  });
}

it("owner issues scoped bearer links, normal transcript projection and immediate revocation", async () => {
  id = crypto.randomUUID(); secondId = crypto.randomUUID(); await seed(); await seed(secondId);
  const path = `/v1/agents/${id}/share-links`;
  expect((await api(path, "POST", other, { permission: "read" }, undefined, "https://nanocodex.example")).status).toBe(404);
  expect((await api(path, "POST", { ...owner, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } }, { permission: "read" }, undefined, "https://nanocodex.example")).status).toBe(403);
  expect((await api(path, "POST", owner, { permission: "admin" }, undefined, "https://nanocodex.example")).status).toBe(400);
  expect((await api(path, "POST", owner, { permission: "read" }, undefined, "https://other.example")).status).toBe(403);
  const created = await api(path, "POST", owner, { permission: "read" }, undefined, "https://nanocodex.example");
  expect(created.status).toBe(201);
  const link = await created.json<{ id: string; permission: string; url: string }>();
  const token = new URL(link.url).hash.slice("#token=".length);
  expect(token).toMatch(/^nsl_[A-Za-z0-9_-]{43}$/);
  expect(link.permission).toBe("read");
  const listed = await (await api(path, "GET", owner)).json<{ data: unknown[] }>();
  expect(listed.data).toHaveLength(1);
  expect(JSON.stringify(listed)).not.toContain(token);
  expect((await api(`/v1/shared/${id}`, "GET", undefined, undefined, token)).status).toBe(200);
  const history = await api(`/v1/shared/${id}/events/history`, "GET", undefined, undefined, token);
  const page = await history.json<{ data: unknown[] }>();
  expect(page.data).toHaveLength(3);
  expect(JSON.stringify(page)).toContain("SECRET_TOOL_OUTPUT");
  expect(JSON.stringify(page)).not.toContain("SECRET_USAGE");
  expect(JSON.stringify(page)).not.toContain("HIDDEN_TOOL_METADATA");
  expect(JSON.stringify(page)).not.toContain("SECRET_ACCEPTED_METADATA");
  expect(page.data).toEqual([
    expect.objectContaining({ type: "turn_accepted", id: "synthetic-turn", input: "synthetic hello" }),
    expect.objectContaining({ type: "event", event: expect.objectContaining({ type: "tool.result" }) }),
    expect.objectContaining({ type: "turn_completed", id: "synthetic-turn", final_message: "synthetic answer" }),
  ]);
  expect(Object.keys(page.data[0] as object).sort()).toEqual(["created_at", "cursor", "id", "input", "turn_id", "type"]);
  expect(Object.keys(page.data[2] as object).sort()).toEqual(["created_at", "cursor", "final_message", "id", "turn_id", "type"]);
  expect((await api(`/v1/shared/${id}/turns`, "POST", undefined, { id: "read-denied", input: "forbidden" }, token, "https://nanocodex.example")).status).toBe(403);
  expect((await api(`/v1/shared/${id}?token=${token}`)).status).toBe(404);
  expect(history.headers.get("cache-control")).toBe("no-store");
  const newest = await (await api(`/v1/shared/${id}/events/history?limit=1`, "GET", undefined, undefined, token)).json<{
    data: { type: string }[]; has_more: boolean; next_cursor: string;
  }>();
  expect(newest.data.map(event => event.type)).toEqual(["turn_completed"]);
  const hiddenOnly = await (await api(`/v1/shared/${id}/events/history?limit=1&before=${newest.next_cursor}`, "GET", undefined, undefined, token)).json<{
    data: unknown[]; has_more: boolean; next_cursor: string;
  }>();
  expect(hiddenOnly.data).toHaveLength(1);
  expect(hiddenOnly.has_more).toBe(true);
  const oldest = await (await api(`/v1/shared/${id}/events/history?limit=1&before=${hiddenOnly.next_cursor}`, "GET", undefined, undefined, token)).json<{
    data: { type: string }[];
  }>();
  expect(oldest.data.map(event => event.type)).toEqual(["turn_accepted"]);
  expect((await api(`/v1/shared/${secondId}`, "GET", undefined, undefined, token)).status).toBe(404);
  expect((await api(`/v1/shared/${id}/turns`, "POST", undefined, { id: "guest-test", input: "hello" }, token, "https://nanocodex.example")).status).toBe(403);
  expect((await api(`/v1/agents/${id}/settings`, "PATCH", undefined, {}, token, "https://nanocodex.example")).status).not.toBe(200);
  expect((await api(`${path}/${link.id}`, "DELETE", owner, undefined, undefined, "https://nanocodex.example")).status).toBe(204);
  expect((await api(`/v1/shared/${id}`, "GET", undefined, undefined, token)).status).toBe(404);
});

it("an owner API key can administer thread links from the TUI but no other key or delegated grant can", async () => {
  id = crypto.randomUUID(); await seed();
  const path = `/v1/agents/${id}/share-links`;
  const key: Principal = { ...owner, kind: "api_key", subjectId: `api_key:${crypto.randomUUID()}`, credentialId: "test-api-key" };
  expect((await api(path, "POST", { ...key, capabilities: ["agents:read"] }, { permission: "read" })).status).toBe(403);
  expect((await api(path, "POST", { ...key, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } }, { permission: "read" })).status).toBe(403);
  expect((await api(path, "POST", { ...key, userId: other.userId }, { permission: "read" })).status).toBe(404);
  const created = await api(path, "POST", key, { permission: "read" });
  expect(created.status).toBe(201);
  const link = await created.json<{ id: string; url: string }>();
  expect(link.url).toMatch(new RegExp(`^https://nanocodex.example/share/${id}#token=nsl_`));
  expect((await api(path, "GET", key)).status).toBe(200);
  expect((await api(`${path}/${link.id}`, "DELETE", key)).status).toBe(204);
  expect((await api(`/v1/shared/${id}`, "GET", undefined, undefined, new URL(link.url).hash.slice(7))).status).toBe(404);
});

it("write link admits real owner-thread turns, isolates identities, limits abuse and revokes immediately", async () => {
  id = crypto.randomUUID(); await seed();
  const path = `/v1/agents/${id}/share-links`;
  const created = await api(path, "POST", owner, { permission: "write" }, undefined, "https://nanocodex.example");
  const { id: linkId, url } = await created.json<{ id: string; url: string }>();
  const token = new URL(url).hash.slice(7);
  const turns = `/v1/shared/${id}/turns`;
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, token, "https://other.example")).status).toBe(403);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "  " }, token, "https://nanocodex.example")).status).toBe(400);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "hello", role: "assistant" }, token, "https://nanocodex.example")).status).toBe(400);
  expect((await api(turns, "POST", undefined, { id: "oversized", input: "a".repeat(33_000) }, token, "https://nanocodex.example")).status).toBe(413);
  const posted = await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, token, "https://nanocodex.example");
  expect(posted.status).toBe(202);
  expect(await posted.json()).toMatchObject({ turn_id: "guest-turn" });
  await runInDurableObject(sessions().getByName(id), async (_, state) => {
    const row = state.storage.sql.exec<{ authorization_json: string }>(
      "SELECT authorization_json FROM managed_turns WHERE id = 'guest-turn'").one();
    expect(JSON.parse(row.authorization_json)).toMatchObject({ guestShareLinkId: linkId,
      connectGrant: { connectors: ["chatgpt"], mcpIds: [] } });
    expect(state.storage.sql.exec("SELECT * FROM managed_share_turn_admissions WHERE turn_id = 'guest-turn'").toArray()).toHaveLength(1);
  });
  const replay = await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, token, "https://nanocodex.example");
  expect(replay.status).toBe(200);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "different" }, token, "https://nanocodex.example")).status).toBe(409);
  const second = await api(path, "POST", owner, { permission: "write" }, undefined, "https://nanocodex.example");
  const secondToken = new URL((await second.json<{ url: string }>()).url).hash.slice(7);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, secondToken, "https://nanocodex.example")).status).toBe(403);
  // Rate-limit state is retained on the same Durable Object and never counts a replay.
  await runInDurableObject(sessions().getByName(id), async (_, state) => {
    for (let i = 0; i < 19; i++) state.storage.sql.exec(
      "INSERT INTO managed_share_turn_admissions(turn_id,link_id,admitted_at) VALUES(?,?,?)", `prior-${i}`, linkId, Date.now());
  });
  expect((await api(turns, "POST", undefined, { id: "limited", input: "hello" }, token, "https://nanocodex.example")).status).toBe(429);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, token, "https://nanocodex.example")).status).toBe(200);
  expect((await api(`${path}/${linkId}`, "DELETE", owner, undefined, undefined, "https://nanocodex.example")).status).toBe(204);
  expect((await api(turns, "POST", undefined, { id: "guest-turn", input: "hello" }, token, "https://nanocodex.example")).status).toBe(404);
  expect((await api(turns, "POST", undefined, { id: "another", input: "hello" }, token, "https://nanocodex.example")).status).toBe(404);
  expect((await api(`/v1/shared/${id}/comments`, "GET", undefined, undefined, secondToken)).status).toBe(404);
});

it("streams assistant and reasoning text with split-token redaction and closes the feed when its link is revoked", async () => {
  id = crypto.randomUUID(); await seed();
  // A real final-answer delta and an unrelated tool event share the durable log.
  await runInDurableObject(sessions().getByName(id), async (_, state) => {
    const log = new DurableEventLog<{ type: string; [key: string]: unknown }>(state.storage);
    log.record({ type: "event", event: { type: "assistant.delta", payload: {
      phase: "final_answer", text: "safe live text.", hidden: "SECRET_DELTA_METADATA",
    } } }, "synthetic-turn");
    log.record({ type: "event", event: { type: "reasoning.summary.delta", payload: { text: "Checking a plan" } } }, "synthetic-turn");
    for (const text of [" n", "s", "l_", "a".repeat(20), "a".repeat(23), " after token."]) {
      log.record({ type: "event", event: { type: "assistant.delta", payload: { phase: "final_answer", text } } }, "synthetic-turn");
    }
    for (const suffix of ["n", "ns", "nsl"]) {
      log.record({ type: "event", event: { type: "assistant.delta", payload: {
        phase: "commentary", item_id: suffix, text: ` ordinary ${suffix}`,
      } } }, "synthetic-turn");
    }
    log.record({ type: "event", event: { type: "assistant.message", payload: { phase: "final_answer", text: "safe live message", hidden: "SECRET_MESSAGE_METADATA" } } }, "synthetic-turn");
  });
  const path = `/v1/agents/${id}/share-links`;
  const created = await api(path, "POST", owner, { permission: "read" }, undefined, "https://nanocodex.example");
  expect(created.status).toBe(201);
  const link = await created.json<{ id: string; url: string }>();
  const token = new URL(link.url).hash.slice(7);
  expect((await api(`/v1/shared/${id}/events?after=invalid`, "GET", undefined, undefined, token)).status).toBe(400);
  expect((await api(`/v1/shared/${secondId}/events?after=0`, "GET", undefined, undefined, token)).status).toBe(404);
  const stream = await api(`/v1/shared/${id}/events?after=0`, "GET", undefined, undefined, token);
  expect(stream.status).toBe(200);
  expect(stream.headers.get("content-type")).toContain("text/event-stream");
  const reader = stream.body!.getReader();
  let transcript = "";
  for (let index = 0; index < 20 && !transcript.includes("safe live message"); index++) {
    const next = await reader.read();
    if (next.done) break;
    transcript += new TextDecoder().decode(next.value);
  }
  expect(transcript).toContain('event: turn_accepted');
  expect(transcript).toContain('event: turn_completed');
  expect(transcript).toContain('event: event');
  expect(transcript).toContain('"text":"safe live message"');
  expect(transcript).toContain("SECRET_TOOL_OUTPUT");
  expect(transcript).toContain("Checking a plan");
  expect(transcript).toContain("safe live text.");
  expect(transcript).not.toMatch(/SECRET_DELTA_METADATA|SECRET_MESSAGE_METADATA/);
  const streamedText = transcript.split("\n").filter(line => line.startsWith("data: "))
    .map(line => JSON.parse(line.slice(6)))
    .filter(row => row.event?.type === "assistant.delta").map(row => row.event.payload.text).join("");
  expect(streamedText).toBe("safe live text. nsl[redacted share token] after token. ordinary n ordinary ns ordinary nsl");
  const replay = await (await api(`/v1/shared/${id}/events?after=3`, "GET", undefined, undefined, token));
  const replayReader = replay.body!.getReader();
  let replayText = "";
  while (!replayText.includes("safe live message")) {
    const next = await replayReader.read();
    if (next.done) break;
    replayText += new TextDecoder().decode(next.value);
  }
  expect(replayText).toContain("safe live text.");
  expect(replayText).not.toContain(`nsl_${"a".repeat(43)}`);
  await replayReader.cancel();
  for (const after of ["6", "7", "8", "9"]) {
    const resumed = await api(`/v1/shared/${id}/events?after=${after}`, "GET", undefined, undefined, token);
    const resumedReader = resumed.body!.getReader();
    let resumedText = "";
    while (!resumedText.includes("safe live message")) {
      const next = await resumedReader.read();
      if (next.done) break;
      resumedText += new TextDecoder().decode(next.value);
    }
    expect(resumedText).not.toContain("a".repeat(20));
    await resumedReader.cancel();
  }
  console.log(JSON.stringify({ journey: "shared-stream", assistant: streamedText, reasoning: "Checking a plan", trailingText: ["n", "ns", "nsl"], replay: true, hiddenMetadata: false }));
  expect(transcript).not.toMatch(/SECRET_USAGE|SECRET_ACCEPTED_METADATA|nsl_/);
  // Anonymous guests are capped below the owner's stream capacity.
  const otherStreams: Response[] = [];
  for (let index = 0; index < 15; index++) {
    const next = await api(`/v1/shared/${id}/events?after=5`, "GET", undefined, undefined, token);
    expect(next.status).toBe(200);
    otherStreams.push(next);
  }
  const overLimit = await api(`/v1/shared/${id}/events?after=5`, "GET", undefined, undefined, token);
  expect(overLimit.status).toBe(429);
  expect(await overLimit.json()).toMatchObject({ error: "event_stream_limit", limit: 16 });
  const ownerFeed = await api(`/v1/agents/${id}/events?after=5`, "GET", owner);
  expect(ownerFeed.status).toBe(200);
  await ownerFeed.body?.cancel();
  expect((await api(`${path}/${link.id}`, "DELETE", owner, undefined, undefined, "https://nanocodex.example")).status).toBe(204);
  for (const feed of otherStreams) await feed.body?.cancel().catch(() => {});
  const closed = await reader.read().catch(() => ({ done: true }));
  expect(closed.done).toBe(true);
  expect((await api(`/v1/shared/${id}/events?after=0`, "GET", undefined, undefined, token)).status).toBe(404);
});

it("root sharing tool manages scoped links and atomically closes all guest feeds", async () => {
  id = crypto.randomUUID(); secondId = crypto.randomUUID(); await seed(); await seed(secondId);
  const context = { sessionId: "synthetic-runtime", callId: "synthetic-call", parentCallId: "", model: "test", signal: new AbortController().signal };
  let actor: Principal | undefined = owner;
  const tool = threadSharingTools({ sessionId: id, ownerId: owner.userId,
    authorizationEpoch: owner.authorizationEpoch, origin: "https://nanocodex.example",
    authorization: () => actor,
    request: (request, principal) => worker.fetch(request, env as Parameters<typeof worker.fetch>[1], createExecutionContext(), principal),
  })[0]!;
  const invoke = (input: unknown, ctx = context) => tool.handler(input, ctx) as Promise<Record<string, any>>;
  const trace: unknown[] = [];
  const read = await invoke({ operation: "create" });
  const write = await invoke({ operation: "create", permission: "write" });
  expect(read).toMatchObject({ session_id: id, permission: "read" });
  expect(write).toMatchObject({ session_id: id, permission: "write" });
  const tokens = [read, write].map(link => new URL(link.url).hash.slice(7));
  const list = await invoke({ operation: "list" });
  expect(list.data).toHaveLength(2);
  expect(JSON.stringify(list)).not.toMatch(/nsl_|#token=/);
  trace.push({ operation: "create/list", permissions: [read.permission, write.permission], active: list.data.length, bearer_metadata: false });
  const target = await invoke({ operation: "create", session_id: secondId });
  expect(target.session_id).toBe(secondId);
  expect((await invoke({ operation: "list", session_id: secondId })).data).toHaveLength(1);
  for (const denied of [undefined, other, { ...owner, authorizationEpoch: 2 },
    { ...owner, capabilities: ["agents:read", "agents:write"] as const },
    { ...owner, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"] as const, mcpIds: [] } }]) {
    actor = denied;
    await expect(invoke({ operation: "revoke_all" })).rejects.toThrow(/authorization/);
  }
  actor = { ...owner, capabilities: ["agents:read", "tools:use"] };
  expect((await invoke({ operation: "list" })).data).toHaveLength(2);
  await expect(invoke({ operation: "create" })).rejects.toThrow(/agents:write/);
  await expect(invoke({ operation: "revoke_all" })).rejects.toThrow(/agents:write/);
  actor = owner;
  await expect(invoke({ operation: "revoke_all" }, { ...context, subagent: {} } as typeof context)).rejects.toThrow(/root authorization/);
  for (const input of [{ operation: "revoke_all", permission: "write" }, { operation: "revoke_all", link_id: read.id },
    { operation: "revoke_all", session_id: "../other" }, { operation: "create", owner_id: other.userId },
    { operation: "revoke" }, { operation: "create", permission: "admin" }]) {
    await expect(invoke(input)).rejects.toThrow(/argument/);
  }
  // Route-level owner/scope isolation remains authoritative for other threads.
  const foreignId = crypto.randomUUID();
  await runInDurableObject(sessions().getByName(foreignId), async (session, state) => {
    // Since 9d8b63102 a fresh session creates its schema on its first request.
    await session.fetch(new Request("https://session.internal/sites"));
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
      VALUES (1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,
      foreignId, other.userId, owner.organizationId, owner.teamId, Date.now());
  });
  await expect(invoke({ operation: "revoke_all", session_id: foreignId })).rejects.toThrow(/HTTP 404/);
  actor = { ...owner, teamId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc" };
  await expect(invoke({ operation: "revoke_all" })).rejects.toThrow(/HTTP 404/);
  actor = owner;
  expect((await invoke({ operation: "list" })).data).toHaveLength(2);
  trace.push({ operation: "authorization", root_only: true, connect_denied: true, readonly_mutation_denied: true, stale_epoch_denied: true, foreign_thread_denied: true });
  const feeds = [];
  for (const token of tokens) {
    const guest = `/v1/shared/${id}`;
    expect((await api(guest, "GET", undefined, undefined, token)).status).toBe(200);
    const feed = await api(`${guest}/events?after=3`, "GET", undefined, undefined, token);
    expect(feed.status).toBe(200);
    const reader = feed.body!.getReader();
    await reader.read(); // Initial SSE comment; next read waits for an event or close.
    feeds.push(reader);
  }
  const revoked = await invoke({ operation: "revoke_all" });
  expect(revoked).toMatchObject({ session_id: id, revoked_count: 2, active_links: 0 });
  expect(new Set(revoked.revoked_ids)).toEqual(new Set([read.id, write.id]));
  for (const reader of feeds) expect((await reader.read().catch(() => ({ done: true }))).done).toBe(true);
  for (const token of tokens) for (const path of [`/v1/shared/${id}`, `/v1/shared/${id}/events/history`, `/v1/shared/${id}/events?after=3`]) {
    expect((await api(path, "GET", undefined, undefined, token)).status).toBe(404);
  }
  expect((await invoke({ operation: "list" })).data).toEqual([]);
  expect(await invoke({ operation: "revoke_all" })).toMatchObject({ revoked_count: 0, active_links: 0 });
  expect((await invoke({ operation: "list", session_id: secondId })).data).toHaveLength(1);
  expect(await invoke({ operation: "revoke", session_id: secondId, link_id: target.id })).toMatchObject({ id: target.id, revoked: true });
  await expect(invoke({ operation: "revoke", session_id: secondId, link_id: target.id })).rejects.toThrow(/HTTP 404/);
  trace.push({ operation: "revoke_all", revoked: 2, closed_feeds: feeds.length, guest_routes_after_revoke: 404, replay_revoked: 0, other_thread_preserved: true, single_revoke_confirmed: true });
  console.info("thread-sharing journey: " + JSON.stringify(trace));
});

it("shared history and SSE cannot redistribute new write or cross-thread bearer links", async () => {
  id = crypto.randomUUID(); secondId = crypto.randomUUID(); await seed(); await seed(secondId);
  const context = { sessionId: "synthetic-runtime", callId: "synthetic-call", parentCallId: "", model: "test", signal: new AbortController().signal };
  const tool = threadSharingTools({ sessionId: id, ownerId: owner.userId,
    authorizationEpoch: owner.authorizationEpoch, origin: "https://nanocodex.example", authorization: () => owner,
    request: (request, principal) => worker.fetch(request, env as Parameters<typeof worker.fetch>[1], createExecutionContext(), principal),
  })[0]!;
  const invoke = (input: unknown) => tool.handler(input, context) as Promise<Record<string, any>>;
  const existing = await invoke({ operation: "create" });
  const elevated = await invoke({ operation: "create", permission: "write" });
  const foreign = await invoke({ operation: "create", session_id: secondId });
  const existingToken = new URL(existing.url).hash.slice(7);
  const elevatedToken = new URL(elevated.url).hash.slice(7);
  const foreignToken = new URL(foreign.url).hash.slice(7);
  await runInDurableObject(sessions().getByName(id), async (_, state) => {
    const log = new DurableEventLog<{ type: string; [key: string]: unknown }>(state.storage);
    state.storage.sql.exec("UPDATE session_state SET first_prompt=? WHERE singleton=1", elevated.url);
    log.record({ type: "event", event: { type: "tool.result", payload: { tool: "thread_sharing", call_id: "synthetic-create", result: elevated, structured_result: elevated } } }, "synthetic-create");
    log.record({ type: "event", event: { type: "tool.result", payload: { tool: "functions.exec", call_id: "synthetic-code", result: JSON.stringify(foreign), content: [{ text: foreign.url }], structured_result: { [foreign.url]: elevated.url } } } }, "synthetic-code");
    log.record({ type: "event", event: { type: "assistant.delta", payload: { phase: "final_answer", text: elevated.url.slice(0, elevated.url.indexOf("nsl_") + 12) } } }, "synthetic-reply");
    log.record({ type: "event", event: { type: "assistant.delta", payload: { phase: "final_answer", text: elevated.url.slice(elevated.url.indexOf("nsl_") + 12) } } }, "synthetic-reply");
    log.record({ type: "event", event: { type: "reasoning.summary.delta", payload: { text: foreign.url.slice(0, foreign.url.indexOf("nsl_") + 12) } } }, "synthetic-reply");
    log.record({ type: "event", event: { type: "reasoning.summary.delta", payload: { text: foreign.url.slice(foreign.url.indexOf("nsl_") + 12) } } }, "synthetic-reply");
    log.record({ type: "event", event: { type: "assistant.message", payload: { phase: "final_answer", text: elevated.url } } }, "synthetic-reply");
    log.record({ type: "turn_accepted", id: "synthetic-link-input", input: foreign.url, replayed: false }, "synthetic-link-input");
    log.record({ type: "turn_completed", id: "synthetic-reply", final_message: foreign.url + " redaction-end" }, "synthetic-reply");
  });
  const ownerHistory = await api(`/v1/agents/${id}/events/history`, "GET", owner);
  expect(ownerHistory.status).toBe(200);
  const privateTranscript = await ownerHistory.text();
  expect(privateTranscript).toContain(elevatedToken);
  expect(privateTranscript).toContain(foreignToken);
  const guest = `/v1/shared/${id}`;
  const metadata = await api(guest, "GET", undefined, undefined, existingToken);
  expect(metadata.status).toBe(200);
  expect(await metadata.text()).not.toMatch(/nsl_/);
  const history = await api(`${guest}/events/history`, "GET", undefined, undefined, existingToken);
  expect(history.status).toBe(200);
  const publicTranscript = await history.text();
  expect(publicTranscript).not.toMatch(/nsl_|#token=nsl_/);
  expect(publicTranscript).not.toMatch(/assistant\.delta|reasoning\.summary\.delta/);
  expect(publicTranscript).toContain("[redacted share token]");
  expect(publicTranscript).toContain("SECRET_TOOL_OUTPUT"); // Ordinary shared tool output still works.
  const feed = await api(`${guest}/events?after=0`, "GET", undefined, undefined, existingToken);
  expect(feed.status).toBe(200);
  const reader = feed.body!.getReader();
  let live = "";
  while (!live.includes("redaction-end")) {
    const next = await reader.read();
    expect(next.done).toBe(false);
    live += new TextDecoder().decode(next.value);
  }
  expect(live).not.toMatch(/nsl_/);
  for (const type of ["assistant.delta", "reasoning.summary.delta"]) {
    const text = live.split("\n").filter(line => line.startsWith("data: "))
      .map(line => JSON.parse(line.slice(6))).filter(row => row.event?.type === type)
      .map(row => row.event.payload.text).join("");
    expect(text).toContain("#token=nsl[redacted share token]");
    expect(text).not.toContain(elevatedToken);
    expect(text).not.toContain(foreignToken);
  }
  expect(live).toContain("[redacted share token]");
  await reader.cancel();
  await invoke({ operation: "revoke_all" });
  await invoke({ operation: "revoke_all", session_id: secondId });
  console.info("thread-sharing redaction journey: owner transcript intact; guest metadata, history and live SSE redact direct/nested/Code Mode/input/final bearer links and object keys");
});
