import { describe, expect, it } from "vitest";
import { env as workerEnv, runInDurableObject } from "cloudflare:test";
import { ensureAccount, type AccountAuthEnv, type Principal } from "../src/account-auth";
import { initializeGmailDecisionTraces, recordGmailDecisionTrace } from "../src/gmail-firehose-traces";
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

  it("adds recent private diagnostics without duplicating decisions or changing old arrays", async () => {
    const f = await fixture(), producer = env.NANOCODEX_USERS.getByName(f.user);
    const trace = (index: number) => ({ source_key: `gmail:gmail-reply-triage-v1:${index.toString(16).padStart(64,"0")}`,
      policy_version: "gmail-reply-triage-v1", outcome: "no_reply", reason: "no_reply", classifier_outcome: "success",
      confidence: 0.95, reply_probability: 0.05, duration_ms: 10, decision_id: null,
      sender: "Person <person@example.test>", subject: "News", source_url: "https://mail.google.com/mail/u/0/#all/abc123" } as const);
    for (let i = 0; i < 103; i++) await producer.recordTodoDecisionTrace(trace(i));
    const decision = await producer.proposeTodoDecision({ source_key: trace(102).source_key, title: "Reply?", context: "Review",
      source_label: "Gmail", source_url: "https://mail.google.com/", choices: [{id:"later",title:"Later"}] });
    await producer.recordTodoDecisionTrace({...trace(101),outcome:"reply",reason:"explicit_reply",decision_id:decision.id});
    await producer.recordTodoDecisionTrace({...trace(100),outcome:"unavailable",reason:"low_confidence"});
    // A rolling-deployment retry from an older producer must not erase headers.
    const {sender,subject,source_url,...legacyRetry} = {...trace(100),outcome:"unavailable" as const,reason:"low_confidence" as const};
    await producer.recordTodoDecisionTrace(legacyRetry);
    const response = (await f.call(owner(f.user),"GET",""))!;
    expect(response.headers.get("cache-control")).toBe("no-store");
    const feed = await response.json() as any;
    expect(feed.items).toEqual([]); expect(feed.decisions).toHaveLength(1);
    expect(feed.traces).toHaveLength(100);
    expect(feed.feed_bounds).toEqual({traces:"recent",trace_limit:100});
    expect(feed.traces[0]).toMatchObject({sender:trace(0).sender,subject:"News",outcome:"unavailable",reason:"low_confidence"});
    expect(feed.traces.every((t:any)=>t.outcome!=="reply")).toBe(true);
    expect(feed.traces[0]).not.toHaveProperty("source_key");
    expect((await (await f.call(owner(f.other),"GET",""))!.json() as any).traces).toEqual([]);
    expect((await f.call({...owner(f.user),connectGrant:{} as any},"GET",""))?.status).toBe(403);
    expect((await f.call(owner(f.user,[]),"GET",""))?.status).toBe(403);
    await runInDurableObject(producer, (_, state) => {
      expect(()=>recordGmailDecisionTrace(state.storage,{...trace(200),source_url:"https://evil.test/"})).toThrow();
      expect(()=>recordGmailDecisionTrace(state.storage,{...trace(200),sender:"x".repeat(257)})).toThrow();
      expect(()=>recordGmailDecisionTrace(state.storage,{...trace(200),body:"private"} as any)).toThrow();
      state.storage.sql.exec("UPDATE gmail_decision_traces SET observed_at = 1");
    });
    expect((await (await f.call(owner(f.user),"GET",""))!.json() as any).traces).toEqual([]);
  });

  it("migrates old trace storage idempotently and reads missing display metadata", async () => {
    const f = await fixture(), producer = env.NANOCODEX_USERS.getByName(f.user);
    await runInDurableObject(producer, (_, state) => {
      state.storage.sql.exec("DROP TABLE gmail_decision_traces");
      state.storage.sql.exec(`CREATE TABLE gmail_decision_traces (
        id INTEGER PRIMARY KEY AUTOINCREMENT, source_key TEXT NOT NULL UNIQUE, policy_version TEXT NOT NULL,
        outcome TEXT NOT NULL, reason TEXT NOT NULL, classifier_outcome TEXT NOT NULL, confidence REAL,
        reply_probability REAL, duration_ms INTEGER NOT NULL, decision_id TEXT, first_at INTEGER NOT NULL,
        observed_at INTEGER NOT NULL, seen_count INTEGER NOT NULL DEFAULT 1)`);
      state.storage.sql.exec(`INSERT INTO gmail_decision_traces (source_key,policy_version,outcome,reason,classifier_outcome,duration_ms,first_at,observed_at)
        VALUES ('legacy','gmail-reply-triage-v1','filtered','missing_body','not_requested',0,?,?)`,Date.now(),Date.now());
      initializeGmailDecisionTraces(state.storage); initializeGmailDecisionTraces(state.storage);
    });
    const feed = await (await f.call(owner(f.user),"GET",""))!.json() as any;
    expect(feed.traces).toMatchObject([{sender:"",subject:"",source_url:"",outcome:"filtered",reason:"missing_body"}]);
    const audit = await (await f.call(owner(f.user),"GET","/traces"))!.json() as any;
    expect(audit.traces[0]).toMatchObject({source_key:"legacy",sender:"",subject:""});
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
