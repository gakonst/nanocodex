import { describe, expect, it } from "vitest";
import { env as workerEnv, runInDurableObject } from "cloudflare:test";
import { ensureAccount, type AccountAuthEnv, type Principal } from "../src/account-auth";
import { initializeGmailDecisionTraces, recordGmailDecisionTrace } from "../src/gmail-firehose-traces";
import { initializeTodoInbox, routeTodoRequest, proposeTodoDecision } from "../src/todo-inbox";

import { proposeGmailReplyDecisions } from "../src/gmail-firehose-decisions";

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

  it("completes and undoes a capture with durable retries, version conflicts and owner authorization", async () => {
    const f = await fixture(), me = owner(f.user);
    const created = await f.call(me, "POST", "", { body: "Follow up after the meeting", operation_id: crypto.randomUUID() });
    const { item } = await created!.json() as { item: { id: string } };
    const path = `/items/${item.id}`;
    const complete = { version: 1, status: "done", operation_id: crypto.randomUUID() };
    expect((await f.call(null, "PATCH", path, complete))?.status).toBe(401);
    expect((await f.call(owner(f.user, ["agents:read"]), "PATCH", path, complete))?.status).toBe(403);
    expect((await f.call({ ...me, connectGrant: {} as any }, "PATCH", path, complete))?.status).toBe(403);
    expect((await f.call({ ...me, kind: "account_session" }, "PATCH", path, complete, { origin: "https://unrelated.test" }))?.status).toBe(403);
    expect((await f.call(owner(f.other), "PATCH", path, complete))?.status).toBe(404);
    const done = await f.call(me, "PATCH", path, complete);
    expect(done?.status).toBe(200);
    const receipt = await done!.json();
    expect(receipt).toMatchObject({ item: { id: item.id, status: "done", version: 2 } });
    const retry = await f.call(me, "PATCH", path, { ...complete, operation_id: complete.operation_id.toUpperCase() });
    expect(await retry!.json()).toEqual(receipt);
    expect((await f.call(me, "PATCH", path, { ...complete, operation_id: crypto.randomUUID() }))?.status).toBe(409);
    expect((await f.call(me, "PATCH", path, { ...complete, status: "captured" }))?.status).toBe(409);
    expect((await f.call(me, "PATCH", path, { ...complete, version: 2, status: "watching", operation_id: crypto.randomUUID() }))?.status).toBe(400);
    const undo = await f.call({ ...me, kind: "account_session" }, "PATCH", path,
      { version: 2, status: "captured", operation_id: crypto.randomUUID() }, { origin: "https://example.test" });
    expect(undo?.status).toBe(200);
    expect(await undo!.json()).toMatchObject({ item: { id: item.id, status: "captured", version: 3 } });
    // A delayed retry must return its original receipt without completing the item again.
    expect(await (await f.call(me, "PATCH", path, complete))!.json()).toEqual(receipt);
    const snapshot = await (await f.call(me, "GET", ""))!.json() as any;
    expect(snapshot.items).toMatchObject([{ id: item.id, status: "captured", version: 3 }]);
    console.log(JSON.stringify({ journey: "todo-capture-complete-undo", complete: "done/v2", undo: "captured/v3", delayed_retry: "original receipt; persisted captured/v3", unauthorized: [401, 403, 404], stale: 409 }));
  });

  it("migrates legacy decisions and exposes exact Gmail references without guessing from subjects", async () => {
    const f = await fixture(), producer = env.NANOCODEX_USERS.getByName(f.user);
    const legacy = { source_key: "legacy-reference", title: "Reply requested: Same subject", context: "Legacy email",
      source_label: "Gmail", source_url: "https://mail.google.com/", choices: [{ id: "later", title: "Later" }] };
    const old = await producer.proposeTodoDecision(legacy);
    await runInDurableObject(producer, (_, state) => {
      for (const column of ["source_connection_id", "source_thread_id", "source_message_id"])
        state.storage.sql.exec(`ALTER TABLE todo_decisions DROP COLUMN ${column}`);
      initializeTodoInbox(state.storage); initializeTodoInbox(state.storage);
    });
    const before = await (await f.call(owner(f.user), "GET", ""))!.json() as any;
    expect(before.decisions[0]).toMatchObject({ id: old.id, source_connection_id: null, source_thread_id: null, source_message_id: null });
    const batch = JSON.stringify({ type: "gmail.history", connectionId: "connection-1", messages: [
      { id: "message-1", threadId: "thread-1", status: "ok", headers: { from: "person@example.test", subject: "Same subject" }, body: "Please reply." },
      { id: "message-2", threadId: "../invalid", status: "ok", headers: { from: "person@example.test", subject: "Same subject" }, body: "Please reply." },
    ] });
    const count = await proposeGmailReplyDecisions(batch,
      { run: async () => ({ state: "Completed", result: { answers: { action: { choice: "reply_requested", confidence: 0.99 } } } }) },
      producer, () => {}, { has: () => false, mark: () => {} }, async () => {});
    expect(count).toBe(2);
    const after = await (await f.call(owner(f.user), "GET", ""))!.json() as any;
    expect(after.decisions.find((entry: any) => entry.source_message_id === "message-1"))
      .toMatchObject({ source_connection_id: "connection-1", source_thread_id: "thread-1" });
    expect(after.decisions.find((entry: any) => entry.source_message_id === "message-2"))
      .toMatchObject({ source_connection_id: "connection-1", source_thread_id: null });
    expect(after.decisions.find((entry: any) => entry.id === old.id)).toMatchObject({ source_connection_id: null });
    const enriched = { ...legacy, source_connection_id: "connection-1", source_thread_id: "thread-1", source_message_id: "message-1" };
    expect((await producer.proposeTodoDecision(enriched)).id).toBe(old.id);
    expect((await producer.proposeTodoDecision(legacy)).id).toBe(old.id);
    await runInDurableObject(producer, (_, state) => {
      expect(() => proposeTodoDecision(state.storage, { ...enriched, source_connection_id: "connection-2" })).toThrow("todo_source_conflict");
    });
    const final = await (await f.call(owner(f.user), "GET", ""))!.json() as any;
    expect(final.decisions.find((entry: any) => entry.id === old.id)).toMatchObject({ source_connection_id: "connection-1", source_thread_id: "thread-1" });
    console.log(JSON.stringify({ journey: "gmail-source-linkage", migration: "nullable legacy refs preserved", producer: "exact connection/message/thread refs", malformed_thread: null, conflicting_reference: "rejected" }));
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
