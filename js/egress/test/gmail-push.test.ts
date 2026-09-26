import { describe, expect, it } from "vitest";
import { GmailPushMailbox, type GmailPushEnv } from "../src/gmail-push";

// Protocol isolation exercises persistence across object eviction and upstream
// failures without a live Gmail mailbox or a model run.
function fixture() {
  const data = new Map<string, unknown>();
  let alarm: number | null = null;
  const storage = {
    get: async (key: string) => structuredClone(data.get(key)),
    put: async (key: string, value: unknown) => { data.set(key, structuredClone(value)); },
    deleteAll: async () => { data.clear(); },
    delete: async (key: string) => data.delete(key),
    list: async (options: { prefix: string; limit: number; startAfter?: string }) => new Map([...data].filter(([k]) => k.startsWith(options.prefix) && (!options.startAfter || k > options.startAfter)).sort(([a], [b]) => a.localeCompare(b)).slice(0, options.limit)),
    setAlarm: async (at: number) => { alarm = at; },
    deleteAlarm: async () => { alarm = null; },
  };
  const calls: Request[] = [], wakes: Record<string, unknown>[] = [];
  let history: (url: URL) => Response = () => Response.json({ historyId: "12", history: [{ id: "12", messagesAdded: [{ message: { id: "m1", threadId: "t1", labelIds: ["INBOX"] } }] }] });
  let message: (url: URL) => Response = url => Response.json({ id: url.pathname.split("/").pop(), payload: { mimeType: "text/plain", headers: [], body: { data: btoa("Full message body") } } });
  let watchStatus = 200;
  let wakeStatus = 202;
  let wakeBody: unknown;
  const env = {
    GMAIL_PUSH_TOPIC: "projects/test/topics/gmail",
    USER_CONNECTORS: { idFromName: (name: string) => name, get: () => ({ fetch: async (request: Request) => {
      calls.push(request);
      const url = new URL(request.url);
      if (url.pathname.endsWith("/profile")) return Response.json({ emailAddress: "mail@example.test", historyId: "20" });
      if (url.pathname.endsWith("/watch") && watchStatus !== 200) return new Response(null, { status: watchStatus });
      if (url.pathname.endsWith("/watch")) return Response.json({ historyId: "10", expiration: String(Date.now() + 7 * 86400000) });
      if (url.pathname.endsWith("/stop")) return new Response(null, { status: 204 });
      if (url.pathname.includes("/messages/")) return message(url);
      return history(url);
    } }) },
    MANAGED_AGENT_OWNERSHIP: { fetch: async (request: Request) => {
      wakes.push(await request.json() as Record<string, unknown>);
      return Response.json(wakeBody ?? { status: wakeStatus === 202 ? "accepted" : "busy" }, { status: wakeStatus });
    } },
  } as unknown as GmailPushEnv;
  const state = { storage } as unknown as DurableObjectState;
  let object = new GmailPushMailbox(state, env);
  return {
    calls, wakes, env, get alarm() { return alarm; },
    message: (fn: typeof message) => { message = fn; },
    history: (fn: typeof history) => { history = fn; },
    watchStatus: (status: number) => { watchStatus = status; },
    wakeStatus: (status: number) => { wakeStatus = status; },
    wakeBody: (body: unknown) => { wakeBody = body; },
    restart: () => { object = new GmailPushMailbox(state, env); },
    alarmRun: () => object.alarm(),
    request: (path: string, method = "GET", body?: unknown) => object.fetch(new Request(`https://gmail-push.internal${path}`, { method, ...(body === undefined ? {} : { body: JSON.stringify(body) }) })),
  };
}
const config = { userId: "user-test", connectionId: "connection-test", agentId: "11111111-1111-4111-8111-111111111111", email: "mail@example.test", topic: "projects/attacker/topics/ignored" };
const notify = { emailAddress: config.email, historyId: "12" };

describe("Gmail push history protocol", () => {
  it("verifies the real mailbox and uses only the configured topic and connection", async () => {
    const f = fixture();
    expect((await f.request("/configure", "POST", { ...config, email: "other@example.test" })).status).toBe(409);
    expect(f.calls.some(r => r.url.endsWith("/watch"))).toBe(false);
    expect((await f.request("/configure", "POST", config)).status).toBe(200);
    expect(await f.calls.find(r => r.url.endsWith("/watch"))!.json()).toMatchObject({ topicName: f.env.GMAIL_PUSH_TOPIC, labelIds: ["INBOX"], labelFilterBehavior: "include" });
    expect(f.calls.every(r => r.headers.get("x-nanocodex-connector-connection") === config.connectionId)).toBe(true);
    expect((await f.request("/notify", "POST", { ...notify, emailAddress: "other@example.test" })).status).toBe(204);
  });

  it("keeps a durable outbox and cursor through busy delivery, retries identical event after eviction, and deduplicates old notifications", async () => {
    const f = fixture();
    await f.request("/configure", "POST", config);
    f.wakeStatus(200);
    await f.request("/notify", "POST", notify);
    await f.alarmRun();
    expect(f.wakes).toHaveLength(1);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10", pending: true });
    expect(f.alarm).not.toBeNull();
    f.restart(); f.wakeStatus(202);
    await f.alarmRun();
    expect(f.wakes[1]).toEqual(f.wakes[0]);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "12", pending: false });
    await f.request("/notify", "POST", notify); await f.alarmRun();
    expect(f.wakes).toHaveLength(2);
  });

  it("drains pending wakes during renewal failure and persists independent renewal backoff", async () => {
    const f = fixture();
    await f.request("/configure", "POST", config);
    f.wakeStatus(503);
    await f.request("/notify", "POST", notify);
    await f.alarmRun();
    const originalWake = f.wakes[0];
    const { vi } = await import("vitest");
    let now = Date.now() + 86400001;
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    try {
      f.watchStatus(403); f.wakeStatus(202); f.restart();
      await f.alarmRun();
      expect(f.wakes).toHaveLength(2);
      expect(f.wakes[1]).toEqual(originalWake);
      expect(await (await f.request("/status")).json()).toMatchObject({
        cursor: "12", pending: false, lastError: null, renewalError: "gmail_watch_retry",
      });
      expect(f.alarm).toBe(now + 60000);
      f.restart(); now += 1000;
      await f.alarmRun();
      expect(f.calls.filter(r => r.url.endsWith("/watch"))).toHaveLength(2);
      now += 59000;
      await f.alarmRun();
      expect(f.calls.filter(r => r.url.endsWith("/watch"))).toHaveLength(3);
      expect(f.alarm).toBe(now + 120000);
      f.restart(); f.watchStatus(200); now += 120000;
      await f.alarmRun();
      expect(await (await f.request("/status")).json()).toMatchObject({ renewalError: null });
      expect(f.alarm).toBeGreaterThan(now + 120000);
    } finally { clock.mockRestore(); }
  });

  it("bounds each page, persists continuation, and advances only after all events are acknowledged", async () => {
    const f = fixture();
    await f.request("/configure", "POST", config);
    f.history(url => {
      expect(Number(url.searchParams.get("maxResults"))).toBeLessThanOrEqual(100);
      return Response.json(url.searchParams.has("pageToken")
        ? { historyId: "15", history: [{ id: "15", messagesAdded: [{ message: { id: "last", labelIds: ["INBOX"] } }] }] }
        : { historyId: "15", nextPageToken: "second", history: [{ id: "11", messagesAdded: [{ message: { id: "first", labelIds: ["INBOX"] } }] }] });
    });
    await f.request("/notify", "POST", { ...notify, historyId: "15" });
    await f.alarmRun();
    await f.alarmRun();
    expect(f.wakes).toHaveLength(2);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "15", pending: false });
  });

  it("emits an explicit resync event on expired history and only commits the new baseline after delivery", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.history(() => new Response(null, { status: 404 })); f.wakeStatus(503);
    await f.request("/notify", "POST", notify); await f.alarmRun();
    expect(JSON.stringify(f.wakes[0])).toContain("gmail.resync");
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10", pending: true });
    f.restart(); f.wakeStatus(202); await f.alarmRun();
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "20", pending: false });
  });

  it("retries provider failures, renews watches daily without replacing cursor, and disables alarms", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.history(() => new Response(null, { status: 429 }));
    await f.request("/notify", "POST", notify); await f.alarmRun();
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10" });
    expect(f.alarm).not.toBeNull();
    const { vi } = await import("vitest");
    const now = Date.now(); const clock = vi.spyOn(Date, "now").mockReturnValue(now + 86400001);
    try { await f.alarmRun(); } finally { clock.mockRestore(); }
    expect(f.calls.filter(r => r.url.endsWith("/watch"))).toHaveLength(2);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10" });
    expect((await f.request("/configure", "DELETE")).status).toBe(200);
    expect(f.alarm).toBeNull();
    expect((await f.request("/notify", "POST", notify)).status).toBe(204);
    await f.alarmRun(); expect(await (await f.request("/status")).json()).toMatchObject({ enabled: false });
  });
  it("ignores draft, sent and label changes; deduplicates inbox messages across history pages", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    const message = (id: string, labelIds: string[]) => ({ message: { id, labelIds } });
    f.history(url => {
      expect(url.searchParams.get("historyTypes")).toBe("messageAdded");
      return Response.json(url.searchParams.has("pageToken")
        ? { historyId: "16", history: [{ id: "16", messagesAdded: [message("inbound", ["INBOX"])] }] }
        : { historyId: "16", nextPageToken: "more", history: [{ id: "12",
          messagesAdded: [message("draft", ["INBOX", "DRAFT"]), message("sent", ["SENT"]), message("inbound", ["INBOX"])],
          labelsAdded: [message("label", ["INBOX"])] }] });
    });
    await f.request("/notify", "POST", { ...notify, historyId: "16" }); await f.alarmRun();
    await f.alarmRun();
    expect(f.wakes).toHaveLength(1);
    expect(JSON.parse(f.wakes[0]!.input as string).messageIds).toEqual(["inbound"]);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "16" });
  });

  it("durably drains more than 100 incoming messages without truncation or premature cursor advancement", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.history(() => Response.json({ historyId: "30", history: [{ id: "30", messagesAdded:
      Array.from({ length: 105 }, (_, i) => ({ message: { id: `message${i}`, labelIds: ["INBOX"] } })) }] }));
    await f.request("/notify", "POST", { ...notify, historyId: "30" }); await f.alarmRun();
    expect(f.wakes.length).toBe(1);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10" });
    for (let i = 0; i < 21; i++) { f.restart(); await f.alarmRun(); }
    expect(f.wakes).toHaveLength(21);
    const delivered = f.wakes.flatMap(w => JSON.parse(w.input as string).messageIds as string[]);
    expect(new Set(delivered).size).toBe(105);
    expect(delivered).toHaveLength(105);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "30", pending: false });
  });

  it("silently reconciles missed notifications hourly and skips empty history pages", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.history(() => Response.json({ historyId: "22", history: [{ id: "22", labelsAdded: [{ message: { id: "own-label", labelIds: ["INBOX"] } }] }] }));
    const { vi } = await import("vitest"); const now = Date.now();
    const clock = vi.spyOn(Date, "now").mockReturnValue(now + 3600001);
    try { await f.alarmRun(); } finally { clock.mockRestore(); }
    expect(f.wakes).toHaveLength(0);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "22" });
    expect(f.calls.filter(r => r.url.endsWith("/watch"))).toHaveLength(1);
  });

  it("retains its outbox for an invalid HTTP success body and accepts an explicit duplicate receipt", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.wakeStatus(200); f.wakeBody({ accepted: true });
    await f.request("/notify", "POST", notify); await f.alarmRun();
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "10", pending: true });
    f.restart(); f.wakeBody({ status: "duplicate" }); await f.alarmRun();
    expect(f.wakes[1]).toEqual(f.wakes[0]);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "12", pending: false });
  });

  it("accepts Gmail history's minimal id/threadId messages using the server-side inbox filter", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    f.history(url => {
      expect(url.searchParams.get("labelId")).toBe("INBOX");
      expect(url.searchParams.get("historyTypes")).toBe("messageAdded");
      return Response.json({ historyId: "12", history: [{ id: "12",
        messagesAdded: [{ message: { id: "minimal", threadId: "thread" } }] }] });
    });
    await f.request("/notify", "POST", notify); await f.alarmRun();
    expect(f.wakes).toHaveLength(1);
    expect(JSON.parse(f.wakes[0]!.input as string).messageIds).toEqual(["minimal"]);
    expect(await (await f.request("/status")).json()).toMatchObject({ cursor: "12", pending: false });
  });

  it("atomically refuses a disable for a different agent", async () => {
    const f = fixture(); await f.request("/configure", "POST", config);
    expect((await f.request("/configure", "DELETE", { agentId: "22222222-2222-4222-8222-222222222222" })).status).toBe(409);
    expect(await (await f.request("/status")).json()).toMatchObject({ enabled: true, agentId: config.agentId });
    expect(f.calls.some(r => r.url.endsWith("/stop"))).toBe(false);
    expect((await f.request("/configure", "DELETE", { agentId: config.agentId })).status).toBe(200);
  });

});

it("treats a self-addressed delivery in INBOX as incoming even when also SENT", async () => {
  const f = fixture();
  await f.request("/configure", "POST", config);
  f.history(() => Response.json({ historyId: "12", history: [{ id: "12", messagesAdded: [{ message: { id: "selftest", threadId: "selfthread", labelIds: ["SENT", "INBOX"] } }] }] }));
  await f.request("/notify", "POST", notify);
  await f.alarmRun();
  expect(f.wakes).toHaveLength(1);
  expect(JSON.parse(f.wakes[0]!.input as string).messageIds).toEqual(["selftest"]);
});
it("persists only explicit CRM opt-in and forwards it identically across busy retries", async () => {
  const f = fixture();
  expect((await f.request("/configure", "POST", { ...config, crm: "true" })).status).toBe(400);
  expect(f.calls).toHaveLength(0);
  expect((await f.request("/configure", "POST", { ...config, crm: true })).status).toBe(200);
  f.wakeStatus(200);
  await f.request("/notify", "POST", notify); await f.alarmRun();
  expect(JSON.parse(f.wakes[0]!.input as string).crm).toBe(true);
  f.restart(); f.wakeStatus(202); await f.alarmRun();
  expect(f.wakes[1]).toEqual(f.wakes[0]);
  expect(await (await f.request("/status")).json()).toMatchObject({ crm: true });
  const generic = fixture();
  await generic.request("/configure", "POST", config);
  await generic.request("/notify", "POST", notify); await generic.alarmRun();
  expect(JSON.parse(generic.wakes[0]!.input as string).crm).not.toBe(true);
});
it("continues bounded CRM work promptly without committing its event or increasing busy backoff", async () => {
  const f = fixture();
  await f.request("/configure", "POST", { ...config, crm: true });
  f.history(() => Response.json({ historyId: "12", history: [{id:"12",messagesAdded:Array.from({length:5},(_,i)=>({message:{id:`m${i}`,labelIds:["INBOX"]}}))}] }));
  await f.request("/notify", "POST", notify);
  const { vi } = await import("vitest"); const now = Date.now();
  const clock = vi.spyOn(Date,"now").mockReturnValue(now);
  try {
    f.wakeStatus(200); f.wakeBody({status:"busy",progress:true});
    for (let i=0;i<19;i++) {
      await f.alarmRun(); f.restart();
      expect(f.alarm).toBe(now+1000);
      expect(await (await f.request("/status")).json()).toMatchObject({cursor:"10",pending:true,lastError:null});
    }
    expect(f.wakes.every(wake=>JSON.stringify(wake)===JSON.stringify(f.wakes[0]))).toBe(true);
    f.wakeBody({status:"busy"}); await f.alarmRun();
    expect(f.alarm).toBe(now+2000);
    f.wakeBody({status:"accepted"}); await f.alarmRun();
    expect(await (await f.request("/status")).json()).toMatchObject({cursor:"12",pending:false});
  } finally { clock.mockRestore(); }
});

// Failure modes defined before implementation: changed provider data on retry,
// MIME alternatives/attachments, deleted/denied/malformed responses, prompt overflow.
it("hydrates MIME alternatives before wake and persists the snapshot across busy eviction", async () => {
  const f = fixture(); await f.request("/configure", "POST", config);
  f.message(url => {
    expect(url.searchParams.get("format")).toBe("full");
    return Response.json({id:"m1", payload:{mimeType:"multipart/mixed",headers:[{name:"Subject",value:"Synthetic subject"}],parts:[
      {mimeType:"multipart/alternative",parts:[{mimeType:"text/html",body:{data:btoa("<p>Duplicate HTML</p>")}},{mimeType:"text/plain",body:{data:btoa("Complete plain body")}}]},
      {mimeType:"text/plain",filename:"attachment.txt",body:{attachmentId:"secret",data:btoa("Attachment content")}}
    ]}});
  });
  f.wakeStatus(200); await f.request("/notify", "POST", notify); await f.alarmRun();
  const input = JSON.parse(f.wakes[0]!.input as string);
  expect(input.messages[0]).toMatchObject({id:"m1",status:"ok",body:"Complete plain body",headers:{subject:"Synthetic subject"}});
  expect(JSON.stringify(input)).not.toContain("Duplicate HTML");
  expect(JSON.stringify(input)).not.toContain("Attachment content");
  f.message(() => { throw new Error("must not refetch"); }); f.restart(); f.wakeStatus(202); await f.alarmRun();
  expect(f.wakes[1]).toEqual(f.wakes[0]);
  expect(f.calls.filter(r=>r.url.includes("/messages/"))).toHaveLength(1);
  expect(f.calls.every(r=>r.headers.get("x-nanocodex-connector-connection")===config.connectionId)).toBe(true);
});
it("reports unavailable bodies explicitly and bounds Unicode/HTML content without fetching attachments", async () => {
  const f=fixture(); await f.request("/configure","POST",config);
  const ids=["html","missing","denied","bad","large","external"];
  f.history(()=>Response.json({historyId:"12",history:[{messagesAdded:ids.map(id=>({message:{id}}))}]}));
  f.message(url=> {
    const id=url.pathname.split("/").pop();
    if(id==="missing") return new Response(null,{status:404});
    if(id==="denied") return new Response("private provider error",{status:403});
    if(id==="bad") return Response.json({id,payload:{mimeType:"text/plain",body:{data:"%%%"}}});
    if(id==="external") return Response.json({id,payload:{mimeType:"text/plain",body:{attachmentId:"not-downloaded",size:123}}});
    if(id==="not-downloaded") return Response.json({data:btoa("External body")});
    const body=id==="html"?"<style>hidden</style><script>bad()</script><p>Hello &amp; goodbye</p>":"😀".repeat(40000);
    return Response.json({id,payload:{mimeType:id==="html"?"text/html":"text/plain",headers:[],body:{data:Buffer.from(body).toString("base64url")}}});
  });
  await f.request("/notify","POST",notify); await f.alarmRun();
  const input=JSON.parse(f.wakes[0]!.input as string);
  expect(input.messages[0].body).toBe("Hello & goodbye");
  expect(input.messages.slice(1,4).map((m:any)=>m.status)).toEqual(["missing","error","error"]);
  expect(input.messages[4].truncated).toBe(true);
  await f.alarmRun();
  expect(JSON.parse(f.wakes[1]!.input as string).messages[0]).toMatchObject({status:"ok",body:"External body"});
  expect(new TextEncoder().encode(f.wakes[0]!.input as string).length).toBeLessThanOrEqual(32768);
  expect(JSON.stringify(input)).not.toContain("private provider error");
  expect(f.calls.filter(r=>r.url.includes("/attachments/"))).toHaveLength(1);
});
it("decodes declared charsets and falls back from an empty plain alternative without related-resource duplication", async () => {
  const f=fixture(); await f.request("/configure","POST",config);
  f.message(()=>Response.json({id:"m1",payload:{mimeType:"multipart/mixed",parts:[
    {mimeType:"text/plain",headers:[{name:"Content-Type",value:"text/plain; charset=windows-1252"}],body:{data:btoa("caf\xe9 \x80")}},
    {mimeType:"multipart/alternative",parts:[{mimeType:"text/plain",body:{data:""}},{mimeType:"multipart/related",parts:[
      {mimeType:"text/html",body:{data:Buffer.from("<p>日本語</p>").toString("base64url")}},
      {mimeType:"text/plain",body:{data:btoa("resource")}}
    ]}]}
  ]}}));
  await f.request("/notify","POST",notify);await f.alarmRun();
  expect(JSON.parse(f.wakes[0]!.input as string).messages[0].body).toBe("café €\n\n日本語");
});
it("retries transient body failures before waking and eventually reports a persistent failure", async () => {
  const f=fixture();await f.request("/configure","POST",config);
  f.message(()=>new Response(null,{status:429}));
  await f.request("/notify","POST",notify);await f.alarmRun();
  expect(f.wakes).toHaveLength(0);f.restart();await f.alarmRun();
  expect(f.wakes).toHaveLength(0);f.restart();await f.alarmRun();
  expect(JSON.parse(f.wakes[0]!.input as string).messages[0].status).toBe("error");
});
