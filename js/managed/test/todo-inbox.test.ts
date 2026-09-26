import { describe, expect, it } from "vitest";
import { env as workerEnv, runInDurableObject } from "cloudflare:test";
import { ensureAccount, type AccountAuthEnv, type Principal } from "../src/account-auth";
import { routeTodoRequest, proposeTodoDecision } from "../src/todo-inbox";

const env = workerEnv as unknown as AccountAuthEnv;
const owner = (userId: string, capabilities: Principal["capabilities"] = ["agents:read", "agents:write"]): Principal => ({
  kind: "api_key", userId, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
  role: "writer", subjectId: `api_key:${userId}`, credentialId: "test", authorizationEpoch: 1, capabilities,
});

async function fixture() {
  const user = crypto.randomUUID(), other = crypto.randomUUID();
  await ensureAccount(env, user, true); await ensureAccount(env, other, true);
  const call = (who: Principal | null, method: string, path: string, payload?: unknown, headers?: HeadersInit) => {
    const url = new URL("https://example.test/v1/todo" + path);
    return routeTodoRequest(new Request(url, { method, headers: { "content-type": "application/json", ...headers },
      body: payload === undefined ? undefined : JSON.stringify(payload) }), env, url, who);
  };
  return { user, other, call };
}

describe("account-owned TODO inbox", () => {
  it("persists capture across reads, isolates accounts, and reconciles duplicate submission", async () => {
    const f = await fixture(), me = owner(f.user), someoneElse = owner(f.other);
    const op = crypto.randomUUID(), body = { body: "Check the rollout", watch_hint: "When a release lands", operation_id: op };
    expect((await f.call(null, "GET", ""))?.status).toBe(401);
    expect((await f.call(owner(f.user, ["agents:read"]), "POST", "", body))?.status).toBe(403);
    const first = await f.call(me, "POST", "", body);
    expect(first?.status).toBe(201);
    const saved = await first!.json() as { item: { id: string; body: string; status: string } };
    expect(saved.item).toMatchObject({ body: body.body, status: "captured" });
    const retry = await f.call(me, "POST", "", { ...body, operation_id: op.toUpperCase() });
    expect((await retry!.json() as { item: { id: string } }).item.id).toBe(saved.item.id);
    expect((await f.call(me, "POST", "", { ...body, body: "Changed" }))?.status).toBe(409);
    expect((await (await f.call(me, "GET", ""))!.json() as { items: unknown[] }).items).toHaveLength(1);
    expect((await (await f.call(someoneElse, "GET", ""))!.json() as { items: unknown[] }).items).toHaveLength(0);
    expect((await f.call(me, "POST", "", { ...body, operation_id: crypto.randomUUID(), body: " ".repeat(30) }))?.status).toBe(400);
  });

  it("records one account-scoped decision and one versioned choice without executing a playbook", async () => {
    const f = await fixture(), me = owner(f.user);
    const capture = await f.call(me, "POST", "", { body: "Check this conversation", operation_id: crypto.randomUUID() });
    const captureID = (await capture!.json() as { item: { id: string } }).item.id;
    const producer = env.NANOCODEX_USERS.getByName(f.user);
    const payload = { todo_id: captureID, workflow_id: "outreach:fixture", source_key: "email:synthetic-thread:reply-1", title: "Reply to the invite?",
      context: "A reply arrived. Draft only; nothing is sent.", source_label: "Email",
      source_url: "https://mail.google.com/", choices: [{ id: "draft", title: "Draft a reply" }, { id: "later", title: "Not now" }] };
    const proposed = await producer.proposeTodoDecision(payload);
    expect((await producer.proposeTodoDecision(payload)).id).toBe(proposed.id);
    await runInDurableObject(producer, (_, state) => {
      expect(() => proposeTodoDecision(state.storage, { ...payload,
        choices: [{ id: "send", title: "Send now" }] })).toThrow("todo_source_conflict");
    });
    expect((await (await f.call(me, "GET", ""))!.json() as { decisions: Array<{ id: string; todo_id: string }> }).decisions[0]).toMatchObject({ id: proposed.id, todo_id: captureID });
    const visible = await (await f.call(me, "GET", ""))!.json() as { decisions: Record<string, unknown>[] };
    expect(visible.decisions[0]).not.toHaveProperty("source_key");
    expect(visible.decisions[0]).not.toHaveProperty("workflow_id");
    const op = crypto.randomUUID(), response = { version: 1, choice_id: "draft", text: null, operation_id: op };
    expect((await f.call(me, "POST", `/decisions/${proposed.id}/respond`, response))?.status).toBe(200);
    expect((await f.call(me, "POST", `/decisions/${proposed.id}/respond`, { ...response, operation_id: op.toUpperCase() }))?.status).toBe(200);
    expect((await f.call(me, "POST", `/decisions/${proposed.id}/respond`, { ...response, operation_id: crypto.randomUUID() }))?.status).toBe(409);
    expect((await (await f.call(me, "GET", ""))!.json() as { decisions: Array<{ status: string }> }).decisions[0]?.status).toBe("answered");
    expect((await (await f.call(owner(f.other), "GET", ""))!.json() as { decisions: unknown[] }).decisions).toHaveLength(0);
  });

  it("exposes only bounded owner-scoped metadata traces with cursor pagination", async () => {
    const f = await fixture(), producer = env.NANOCODEX_USERS.getByName(f.user);
    const proposal = (index:number) => ({source_key:`gmail:gmail-reply-triage-v1:${index.toString(16).padStart(64,"0")}`,
      policy_version:"gmail-reply-triage-v1", outcome:"no_reply", reason:"no_reply",
      classifier_outcome:"success", confidence:0.94, reply_probability:0.06, duration_ms:12,
      decision_id:null} as const);
    await producer.recordTodoDecisionTrace(proposal(1));
    await producer.recordTodoDecisionTrace(proposal(2));
    const first = await (await f.call(owner(f.user),"GET","/traces?limit=1"))!.json() as any;
    expect(first.traces).toHaveLength(1);
    expect(first.next_cursor).toBeTruthy();
    expect(first.traces[0]).toMatchObject({outcome:"no_reply",confidence:0.94});
    expect(JSON.stringify(first)).not.toContain("@example.test");
    const second = await (await f.call(owner(f.user),"GET",`/traces?limit=1&before=${first.next_cursor}`))!.json() as any;
    expect(second.traces).toHaveLength(1);
    expect(second.traces[0].id).not.toBe(first.traces[0].id);
    expect((await f.call(owner(f.other),"GET","/traces"))?.status).toBe(200);
    const other = await (await f.call(owner(f.other),"GET","/traces"))!.json() as any;
    expect(other.traces).toHaveLength(0);
    expect((await f.call(owner(f.user),"GET","?before=1"))?.status).toBe(404);
  });

  it("does not hide an older open decision behind 200 newer answered items", async () => {
    const f = await fixture(), me = owner(f.user), producer = env.NANOCODEX_USERS.getByName(f.user);
    const proposal = (index: number) => ({ source_key: `job:fixture:${index}`, title: `Choice ${index}`,
      context: "A test decision", source_label: "Job", source_url: "", choices: [{ id: "later", title: "Later" }] });
    const open = await producer.proposeTodoDecision(proposal(0));
    for (let i = 1; i <= 201; i++) {
      const item = await producer.proposeTodoDecision(proposal(i));
      const answer = await f.call(me, "POST", `/decisions/${item.id}/respond`, {
        version: 1, choice_id: "later", text: null, operation_id: crypto.randomUUID(),
      });
      expect(answer?.status).toBe(200);
    }
    const snapshot = await (await f.call(me, "GET", ""))!.json() as { decisions: Array<{ id: string; status: string }> };
    expect(snapshot.decisions[0]).toMatchObject({ id: open.id, status: "needs_you" });
  });

  it("does not let clients forge decisions or resolve unrecognized versions", async () => {
    const f = await fixture(), me = owner(f.user);
    expect((await f.call(me, "POST", "/decisions", { title: "Fake" }))?.status).toBe(404);
    expect((await f.call(me, "POST", `/decisions/${crypto.randomUUID()}/respond`, {
      version: 1, choice_id: "send", text: null, operation_id: crypto.randomUUID(),
    }))?.status).toBe(404);
    expect((await f.call(me, "GET", "", undefined, { origin: "https://unrelated.test" }))?.status).toBe(200);
    const session = { ...me, kind: "account_session" as const };
    expect((await f.call(session, "POST", "", { body: "No CSRF", operation_id: crypto.randomUUID() },
      { origin: "https://unrelated.test" }))?.status).toBe(403);
  });
});
