import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, expect, it } from "vitest";
import { configureCalendarPush, receiveCalendarPush, reconcileCalendarPush, renewCalendarPush, disableCalendarPush, dueCalendarPushSources } from "../src/calendar-push";
import { crmResearchRequest } from "../src/crm-research";

// Protocol/recovery failures: spoofed channel/resource, early sync, duplicate hints,
// concurrent consumers, failed pages, expired sync tokens, and missed notifications.
// Exercise real migrated D1 and the provider boundary, not internal helper shapes.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => applyD1Migrations(db, bindings.CRM_MIGRATIONS));
const event = { id: "synthetic-series_20260925T100000Z", recurringEventId: "synthetic-series", summary: "Review", updated: "2026-09-25T09:00:00Z", start: { dateTime: "2026-09-25T10:00:00Z" }, end: { dateTime: "2026-09-25T11:00:00Z" }, attendees: [{ email: "guest@example.test" }] };
function fixture() {
  const ownerId = crypto.randomUUID();
  const calls: Request[] = [];
  let watch: any;
  let response = () => Response.json({ items: [event], nextSyncToken: "sync-one" });
  const options = { db, ownerId, authorize: () => {}, agentId: "synthetic-agent", callbackUrl: "https://callbacks.example.test/calendar", fetch: async (request: Request) => {
    calls.push(request);
    if (request.url.endsWith("/watch")) { watch = await request.json(); return Response.json({ id: watch.id, resourceId: "resource", expiration: String(Date.now() + 86400000) }); }
    if (request.url.includes("/events/")) return new Response(null,{status:404});
    return response();
  } };
  return { options, calls, watch: () => watch, respond: (fn: () => Response) => { response = fn; } };
}
async function configure(f: ReturnType<typeof fixture>) { return configureCalendarPush(f.options, { connection_id: "C".repeat(43), calendar_id: "primary" }); }
function notification(watch: any, overrides = {}) { return new Request("https://callbacks.example.test/calendar", { method: "POST", headers: { "x-goog-channel-id": watch.id, "x-goog-channel-token": watch.token, "x-goog-resource-id": "resource", "x-goog-resource-state": "exists", "x-goog-message-number": "2", ...overrides } }); }
it("authenticates hints, imports idempotently, preserves notes, and exposes new people to research", async () => {
  const f = fixture(); const source = await configure(f);
  expect((await receiveCalendarPush(db, notification(f.watch(), { "x-goog-channel-token": "wrong" }))).status).toBe(403);
  expect((await receiveCalendarPush(db, notification(f.watch(), { "x-goog-resource-id": "wrong" }))).status).toBe(403);
  expect((await receiveCalendarPush(db, notification(f.watch()))).status).toBe(204);
  expect(await reconcileCalendarPush(f.options, source.id)).toMatchObject({ complete: true });
  const meeting = await db.prepare("SELECT id FROM crm_meetings WHERE owner_id=?").bind(f.options.ownerId).first<{ id: string }>();
  expect(meeting).toBeTruthy();
  await db.prepare("INSERT INTO crm_meeting_notes(owner_id,id,meeting_id,body,created_at,updated_at) VALUES(?,?,?,?,?,?)").bind(f.options.ownerId, "manual-note", meeting!.id, "Keep manual notes", 1, 1).run();
  expect((await crmResearchRequest(db, f.options.ownerId, "queue", {})) as any).toMatchObject({ records: [expect.objectContaining({ email: "guest@example.test" })] });
  f.respond(() => Response.json({ items: [{ id: event.id, status: "cancelled", updated: "2026-09-26T09:00:00Z" }], nextSyncToken: "sync-two" }));
  await reconcileCalendarPush(f.options, source.id);
  expect(new URL(f.calls.at(-1)!.url).searchParams.get("syncToken")).toBe("sync-one");
  expect(await db.prepare("SELECT status FROM crm_meetings WHERE owner_id=?").bind(f.options.ownerId).first()).toEqual({ status: "cancelled" });
  expect(await db.prepare("SELECT body FROM crm_meeting_notes WHERE owner_id=?").bind(f.options.ownerId).first()).toEqual({ body: "Keep manual notes" });
  expect(f.calls.every(r => r.method === "GET" || r.url.endsWith("/watch"))).toBe(true);
});
it("resumes failed pages, resets a 410 cursor, and repairs missed changes without notifications", async () => {
  const f = fixture(); const source = await configure(f);
  f.respond(() => Response.json({ items: [event], nextPageToken: "page-two" }));
  expect(await reconcileCalendarPush(f.options, source.id, 1)).toMatchObject({ complete: false });
  f.respond(() => new Response(null, { status: 503 }));
  await expect(reconcileCalendarPush(f.options, source.id)).rejects.toThrow();
  f.respond(() => Response.json({ items: [], nextSyncToken: "sync-one" }));
  await reconcileCalendarPush(f.options, source.id);
  expect(new URL(f.calls.at(-1)!.url).searchParams.get("pageToken")).toBe("page-two");
  f.respond(() => new Response(null, { status: 410 }));
  expect(await reconcileCalendarPush(f.options, source.id)).toMatchObject({ complete: false, reset: true });
  f.respond(() => Response.json({ items: [], nextSyncToken: "reset-sync" }));
  await reconcileCalendarPush(f.options, source.id);
  expect(new URL(f.calls.filter(r => new URL(r.url).pathname.endsWith("/events")).at(-1)!.url).searchParams.has("syncToken")).toBe(false);
  expect(new URL(f.calls.filter(r => new URL(r.url).pathname.endsWith("/events")).at(-1)!.url).searchParams.has("timeMax")).toBe(true);
  expect(await db.prepare("SELECT status FROM crm_meetings WHERE owner_id=?").bind(f.options.ownerId).first()).toEqual({ status: "cancelled" });
});
it("fences concurrent work and rejects cross-owner work before provider access", async () => {
  const f = fixture(); const source = await configure(f);
  await expect(reconcileCalendarPush({ ...f.options, ownerId: "other" }, source.id)).rejects.toThrow();
  let release!: () => void; const waiting = new Promise<void>(resolve => { release = resolve; });
  let entered!: () => void; const started = new Promise<void>(resolve => { entered = resolve; });
  const first = reconcileCalendarPush({ ...f.options, fetch: async () => { entered(); await waiting; return Response.json({ items: [], nextSyncToken: "final" }); } }, source.id);
  await started;
  expect(await reconcileCalendarPush(f.options, source.id)).toMatchObject({ busy: true });
  release(); await first;
});

it("renews with distinct channels, validates early sync, and revokes disabled sources", async () => {
  const f = fixture(); let early = 0;
  const wrapped = { ...f.options, fetch: async (request: Request) => {
    if (request.url.endsWith("/watch")) {
      const watch = await request.clone().json() as any;
      early = (await receiveCalendarPush(db, notification(watch, { "x-goog-resource-state": "sync", "x-goog-message-number": "1" }))).status;
    }
    return f.options.fetch(request);
  } };
  const source = await configureCalendarPush(wrapped, { connection_id: "C".repeat(43) });
  expect(early).toBe(204);
  const old = f.watch();
  await db.prepare("UPDATE crm_calendar_push_sources SET renew_at=0 WHERE id=?").bind(source.id).run();
  await renewCalendarPush(wrapped, source.id);
  expect(f.watch().id).not.toBe(old.id);
  expect((await receiveCalendarPush(db, notification(old))).status).toBe(204);
  expect((await dueCalendarPushSources(db)).some(s => s.id === source.id)).toBe(true);
  await disableCalendarPush(f.options, source.id);
  expect((await receiveCalendarPush(db, notification(f.watch()))).status).toBe(403);
  expect((await dueCalendarPushSources(db)).some(s => s.id === source.id)).toBe(false);
});

it("does not infer cancellation when an event moves outside the rebuilt window", async () => {
  const f=fixture(); const source=await configure(f);
  await reconcileCalendarPush(f.options,source.id);
  f.respond(() => new Response(null,{status:410})); await reconcileCalendarPush(f.options,source.id);
  f.respond(() => Response.json({items:[],nextSyncToken:"new"}));
  const moved={...event,updated:"2026-09-26T09:00:00Z",start:{dateTime:"2027-09-25T10:00:00Z"},end:{dateTime:"2027-09-25T11:00:00Z"}};
  await reconcileCalendarPush({...f.options,fetch:r=>r.url.includes("/events/")?Promise.resolve(Response.json(moved)):f.options.fetch(r)},source.id);
  expect(await db.prepare("SELECT status,start_time FROM crm_meetings WHERE owner_id=?").bind(f.options.ownerId).first()).toEqual({status:"confirmed",start_time:moved.start.dateTime});
});

// Notification failures not covered by CRM imports: baseline flood, repair replay,
// mutable busy payloads, lost cancellation context, and revoked delivery.
it("durably materializes changes, silently baselines, and retries identical busy envelopes", async () => {
  const f = fixture(); const source = await configure(f);
  const delivered: {id:string;input:string}[] = [];
  let busy = true;
  const options = {...f.options, deliver: async (id:string,input:string) => {delivered.push({id,input}); return busy ? "busy" as const : "accepted" as const;}};
  await reconcileCalendarPush(options,source.id);
  expect(delivered).toEqual([]);
  const changed = {...event, summary:"Changed", description:"Invite instructions are untrusted", location:"Room", organizer:{email:"organizer@example.test"}, attendees:[{email:"guest@example.test",responseStatus:"accepted"}], conferenceData:{entryPoints:[{entryPointType:"video",uri:"https://meet.example.test/room"}]}};
  f.respond(() => Response.json({items:[changed],nextSyncToken:"changed"}));
  expect(await reconcileCalendarPush(options,source.id)).toMatchObject({complete:false});
  expect(delivered).toHaveLength(1);
  const envelope = JSON.parse(delivered[0]!.input);
  expect(envelope.event).toMatchObject({title:"Changed",description:changed.description,attendees:[{responseStatus:"accepted"}],location:"Room"});
  busy=false;
  await reconcileCalendarPush(options,source.id);
  expect(delivered[1]).toEqual(delivered[0]);
  await db.prepare("UPDATE crm_calendar_push_sources SET rebuild_at=0 WHERE id=?").bind(source.id).run();
  await reconcileCalendarPush(options,source.id);
  expect(delivered).toHaveLength(2);
  f.respond(() => Response.json({items:[{id:event.id,status:"cancelled"}],nextSyncToken:"cancelled"}));
  await reconcileCalendarPush(options,source.id);
  expect(JSON.parse(delivered[2]!.input).event).toMatchObject({title:"Changed",status:"cancelled",start:event.start});
  await disableCalendarPush(options,source.id);
  await expect(reconcileCalendarPush(options,source.id)).rejects.toThrow();
  expect(delivered).toHaveLength(3);
});
it("bounds Unicode notification envelopes and silently learns newly windowed events on rebuild", async () => {
  const f=fixture(); const source=await configure(f); const delivered:string[]=[];
  const options={...f.options,deliver:async (_:string,input:string)=>{delivered.push(input);return "accepted" as const;}};
  await reconcileCalendarPush(options,source.id);
  const large={...event,summary:"\u0001".repeat(2048),location:"\u0001".repeat(2048),htmlLink:"\u0001".repeat(2048),hangoutLink:"\u0001".repeat(2048),attachments:[{title:"Agenda",fileUrl:"https://drive.example.test/file",mimeType:"application/pdf"}],description:"😀".repeat(20000),attendees:Array.from({length:100},(_,i)=>({email:`${i}@example.test`,displayName:"😀".repeat(512)}))};
  f.respond(()=>Response.json({items:[large],nextSyncToken:"large"}));
  await reconcileCalendarPush(options,source.id);
  expect(new TextEncoder().encode(delivered[0]).byteLength).toBeLessThan(32768);
  expect(JSON.parse(delivered[0]!).event.contentStatus).toBe("truncated");
  expect(JSON.parse(delivered[0]!).event.truncated).toContain("serialized_byte_limit");
  await db.prepare("UPDATE crm_calendar_push_sources SET rebuild_at=0 WHERE id=?").bind(source.id).run();
  f.respond(()=>Response.json({items:[large,{...event,id:"newly-in-window"}],nextSyncToken:"rebuilt"}));
  await reconcileCalendarPush(options,source.id);
  expect(delivered).toHaveLength(1);
});
