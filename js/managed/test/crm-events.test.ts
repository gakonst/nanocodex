import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest } from "../src/crm";
import { importCalendarEvents } from "../src/crm-meetings";
import { crmInteractionRequest } from "../src/crm-events";
import { crmTimelineRequest } from "../src/crm-timeline";

// Define runtime failure scenarios before implementation: cross-owner/person/filter
// cursor replay; equal-time cross-kind ties; duplicated invitations; deleted mail
// notes; invalid person kinds and bounds. Use shipped migrations and real D1.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const account = () => `events-${crypto.randomUUID()}`;
const person = (owner: string, id = "person") => crmRequest(db, owner, "save", { kind: "person", name: "Synthetic Guest", email: `${id}@example.test` }, id);
const timeline = (owner: string, input: Record<string, unknown> = {}): Promise<any> => crmTimelineRequest(db, owner, { person_id: "person", ...input });
const invalid = (p: Promise<unknown>) => expect(p).rejects.toMatchObject({ code: "invalid_input" });
const missing = (p: Promise<unknown>) => expect(p).rejects.toMatchObject({ code: "not_found" });
const at = "2026-01-02T12:00:00Z", ms = Date.parse(at);
async function native(owner: string) {
  await importCalendarEvents(db, owner, { connection_id: "google", calendar_id: "primary", events: [{ id: "invite", summary: "Synthetic invitation", start: { dateTime: at }, end: { dateTime: "2026-01-02T13:00:00Z" }, attendees: [{ email: "person@example.test", responseStatus: "accepted" }] }] });
  const meeting = await db.prepare("SELECT id FROM crm_meetings WHERE owner_id=?").bind(owner).first<{ id: string }>();
  await db.prepare("INSERT INTO crm_meeting_attendees(owner_id,meeting_id,ordinal,person_id,response_status) VALUES (?,?,99,'person','accepted')").bind(owner, meeting!.id).run();
  await db.prepare("INSERT INTO crm_notes(owner_id,id,record_id,body,source_url,created_at,updated_at) VALUES (?,'mail','person','Synthetic mail','https://mail.google.com/',?,?)").bind(owner, ms, ms).run();
  await db.prepare("INSERT INTO crm_email_imports(owner_id,connection_id,message_id,record_id,note_id,imported_at) VALUES (?,'google','message','person','mail',?)").bind(owner, ms).run();
}

describe("CRM event timeline in real D1", () => {
  it("returns native invitations once and joins only surviving email notes", async () => {
    const owner = account(); await person(owner); await native(owner);
    const page = await timeline(owner);
    expect(page.entries.map((e: any) => e.kind).sort()).toEqual(["calendar_meeting", "email"]);
    expect(page.entries.find((e: any) => e.kind === "calendar_meeting")).toMatchObject({ participation_status: "invited", occurred_at: new Date(ms).toISOString() });
    expect(page.entries.find((e: any) => e.kind === "email")).toMatchObject({ id: "mail", body: "Synthetic mail", origin: "source" });
    expect(page.next_cursor).toBeNull();
    // An imported response is retained without converting acceptance to attendance.
    expect(page.entries.find((e: any) => e.kind === "calendar_meeting")).toMatchObject({ response_status: "accepted", self_declined: false });
    await crmRequest(db, owner, "delete_note", { id: "mail" }, "unused");
    expect((await timeline(owner)).entries.map((e: any) => e.kind)).toEqual(["calendar_meeting"]);
    expect(await db.prepare("SELECT note_id FROM crm_email_imports WHERE owner_id=?").bind(owner).first()).toEqual({ note_id: "mail" });
  });

  it("scopes keyset cursors to account, person and time filter and continues after anchor deletion", async () => {
    const owner = account(), other = account();
    await person(owner); await person(owner, "second"); await person(other); await native(owner);
    const first = await timeline(owner, { limit: 1 });
    expect(first.entries).toHaveLength(1); expect(first.next_cursor).toEqual(expect.any(String));
    await invalid(timeline(other, { limit: 1, cursor: first.next_cursor }));
    await invalid(timeline(owner, { person_id: "second", cursor: first.next_cursor }));
    await invalid(timeline(owner, { from: at, cursor: first.next_cursor }));
    const all = await timeline(owner);
    if (first.entries[0].kind === "email") await crmRequest(db, owner, "delete_note", { id: "mail" }, "unused");
    else await db.prepare("DELETE FROM crm_meetings WHERE owner_id=?").bind(owner).run();
    const next = await timeline(owner, { cursor: first.next_cursor, limit: 100 });
    expect(next.entries).toEqual(all.entries.slice(1)); expect(next.next_cursor).toBeNull();
    expect((await timeline(owner, { to: at })).entries).toEqual([]);
  });

  it("merges all four kinds at the same instant without skipping ties and retains observation provenance", async () => {
    const owner = account(); await person(owner); await native(owner);
    await db.prepare("INSERT INTO crm_events(owner_id,id,title,start_at,start_ms,origin,sources,created_at,updated_at) VALUES (?,'same','Synthetic conference',?,?,'user','[]',1,1)").bind(owner, at, ms).run();
    await db.prepare("INSERT INTO crm_event_participation(owner_id,id,event_id,record_id,person_id,status,role,origin,sources,created_at,updated_at) VALUES (?,'same','same','person','person','expected','attendee','source','[{\"kind\":\"web\",\"reference\":\"https://example.test/roster\"}]',1,1)").bind(owner).run();
    await db.prepare("INSERT INTO crm_interactions(owner_id,id,person_id,event_id,occurred_at,occurred_ms,body,origin,sources,created_at,updated_at) VALUES (?,'same','person','same',?,?,'Met briefly','user','[]',1,1)").bind(owner, at, ms).run();
    await db.prepare("INSERT INTO crm_interaction_participants(owner_id,interaction_id,record_id,role) VALUES (?,'same','person','attendee')").bind(owner).run();
    const entries: any[] = []; let cursor: string | null = null;
    do {
      const page = await timeline(owner, { limit: 1, ...(cursor ? { cursor } : {}) });
      entries.push(...page.entries); cursor = page.next_cursor;
      expect(entries.length).toBeLessThanOrEqual(4);
    } while (cursor);
    expect(entries.map(e => e.kind).sort()).toEqual(["calendar_meeting", "email", "event_participation", "interaction"]);
    expect(new Set(entries.map(e => `${e.kind}:${e.id}`)).size).toBe(4);
    expect(entries.find(e => e.kind === "event_participation")).toMatchObject({ participation_status: "expected", origin: "source", sources: [{ kind: "web", reference: "https://example.test/roster" }] });
    expect(entries.find(e => e.kind === "interaction")).toMatchObject({ body: "Met briefly", origin: "user" });
    expect(entries).toEqual((await timeline(owner, { from: at, to: "2026-01-03T00:00:00Z" })).entries);
    await db.prepare("DELETE FROM crm_events WHERE owner_id=? AND id='same'").bind(owner).run();
    expect((await timeline(owner)).entries).toHaveLength(3);
    expect((await timeline(owner)).entries.find((e: any) => e.kind === "interaction")).toMatchObject({ body: "Met briefly", event_id: null });
  });

  it("includes authored record and meeting notes without duplicating imported email notes", async () => {
    const owner = account(); await person(owner); await native(owner);
    await crmRequest(db, owner, "save_note", { record_id: "person", body: "User observation" }, "manual");
    const meeting = await db.prepare("SELECT id FROM crm_meetings WHERE owner_id=?").bind(owner).first<{ id: string }>();
    await db.prepare("INSERT INTO crm_meeting_notes(owner_id,id,meeting_id,body,created_at,updated_at) VALUES (?,'manual',?,'User meeting notes',?,?)").bind(owner, meeting!.id, ms, ms).run();
    const entries = (await timeline(owner)).entries;
    expect(entries.map((e: any) => e.kind).sort()).toEqual(["calendar_meeting", "email", "meeting_note", "note"]);
    expect(entries.find((e: any) => e.kind === "note")).toMatchObject({ body: "User observation", timestamp_basis: "created_at" });
    expect(entries.find((e: any) => e.kind === "meeting_note")).toMatchObject({ body: "User meeting notes", origin: "user" });
    const first = await timeline(owner, { limit: 1 });
    const rest = await timeline(owner, { cursor: first.next_cursor });
    expect([...first.entries, ...rest.entries]).toEqual(entries);
  });

  it("shows one date-only proposal in both participants timelines with roles", async () => {
    const owner = account(); await person(owner); await person(owner, "second");
    await crmInteractionRequest(db, owner, "save", { type: "proposal", summary: "Synthetic proposal", body: "Discuss a joint project", occurred_at: "2026-01-02", participants: [{ record_id: "person", role: "proposer" }, { record_id: "second", role: "recipient" }], origin: "user" }, "proposal");
    const one = (await timeline(owner)).entries, two = (await timeline(owner, { person_id: "second" })).entries;
    expect(one).toHaveLength(1); expect(two).toHaveLength(1);
    for (const entries of [one, two]) expect(entries[0]).toMatchObject({ id: "proposal", kind: "interaction", type: "proposal", summary: "Synthetic proposal", occurred_at: "2026-01-02", precision: "date", participants: [{ record_id: "person", role: "proposer" }, { record_id: "second", role: "recipient" }] });
    expect((await db.prepare("SELECT id FROM crm_interactions WHERE owner_id=?").bind(owner).all()).results).toEqual([{ id: "proposal" }]);
    expect((await crmTimelineRequest(db, owner, {})).entries).toHaveLength(1);
  });

  it("sorts imported email by received time and labels legacy import-time fallback", async () => {
    const owner = account(); await person(owner); await native(owner);
    expect((await timeline(owner)).entries.find((e: any) => e.kind === "email")).toMatchObject({ timestamp_basis: "imported_at" });
    await db.prepare("UPDATE crm_email_imports SET received_ms=? WHERE owner_id=?").bind(ms - 3600000, owner).run();
    const email = (await timeline(owner)).entries.find((e: any) => e.kind === "email");
    expect(email).toMatchObject({ occurred_at: new Date(ms - 3600000).toISOString(), timestamp_basis: "received_at" });
    expect((await timeline(owner, { from: at })).entries.map((e: any) => e.kind)).toEqual(["calendar_meeting"]);
  });

  it("filters global timelines by CRM event and binds cursors to event scope", async () => {
    const owner = account(); await person(owner); await native(owner);
    await db.prepare("INSERT INTO crm_events(owner_id,id,title,start_at,start_ms,origin,sources,created_at,updated_at) VALUES (?,'event-filter','Synthetic conference',?,?,'user','[]',1,1)").bind(owner, at, ms).run();
    await crmInteractionRequest(db, owner, "save", { event_id: "event-filter", body: "Discussion", occurred_at: at, participants: [{ record_id: "person" }], origin: "user" }, "linked");
    await crmInteractionRequest(db, owner, "save", { event_id: "event-filter", body: "Follow-up", occurred_at: at, participants: [{ record_id: "person" }], origin: "user" }, "linked-two");
    const global = await crmTimelineRequest(db, owner, {});
    expect(global.entries).toHaveLength(4);
    expect(global.entries.find(e => e.kind === "email")).toMatchObject({ record_id: "person" });
    const first = await crmTimelineRequest(db, owner, { event_id: "event-filter", limit: 1 });
    expect(first.entries[0].kind).toBe("interaction"); expect(first.next_cursor).toEqual(expect.any(String));
    await invalid(crmTimelineRequest(db, owner, { cursor: first.next_cursor }));
    await invalid(crmTimelineRequest(db, owner, { person_id: "person", event_id: "event-filter", cursor: first.next_cursor }));
    const last = await crmTimelineRequest(db, owner, { event_id: "event-filter", cursor: first.next_cursor });
    expect(last.entries).toHaveLength(1); expect(last.next_cursor).toBeNull();
    expect(last.entries[0].id).not.toBe(first.entries[0].id);
    await missing(crmTimelineRequest(db, account(), { event_id: "event-filter" }));
  });

  it("retains declined invitation responses without implying attendance", async () => {
    const owner = account(); await person(owner); await native(owner);
    await db.prepare("UPDATE crm_meeting_attendees SET response_status='declined' WHERE owner_id=?").bind(owner).run();
    await db.prepare("UPDATE crm_meetings SET self_declined=1 WHERE owner_id=?").bind(owner).run();
    const meeting = (await timeline(owner)).entries.find((e: any) => e.kind === "calendar_meeting");
    expect(meeting).toMatchObject({ participation_status: "declined", response_status: "declined", self_declined: true, attendance_status: "unknown" });
    await db.prepare("UPDATE crm_meeting_attendees SET response_status='accepted' WHERE owner_id=? AND ordinal=99").bind(owner).run();
    expect((await timeline(owner)).entries.find((e: any) => e.kind === "calendar_meeting")).toMatchObject({ response_status: null, attendance_status: "unknown" });
  });

  it("requires an owned person and rejects malformed bounds and cursors", async () => {
    const owner = account(); await person(owner);
    await crmRequest(db, owner, "save", { kind: "company", name: "Synthetic Works" }, "company");
    await missing(timeline(account())); await missing(timeline(owner, { person_id: "company" }));
    for (const input of [{ limit: 0 }, { limit: 101 }, { limit: "2" }, { cursor: "garbage" }, { cursor: "a".repeat(4097) }, { from: "2026-02-30T00:00:00Z" }, { from: at, to: at }, { to: "tomorrow" }, { extra: true }]) await invalid(timeline(owner, input));
    await invalid(crmTimelineRequest(db, "", { person_id: "person" }));
  });
});

// Runtime tool boundary: current authority gates every new read/write, revoked
// grants cannot use private CRM, account arguments cannot select another owner.
import { crmTools, type CrmAuthorization } from "../src/crm-tools";
it("enforces tool authority and returns the same timeline through crm_get", async () => {
  const owner = account(); await person(owner); await person(owner, "second");
  const full = { capabilities: ["agents:read", "agents:write", "tools:use"] };
  let authorization: CrmAuthorization | undefined = full;
  const tools = crmTools({ db, ownerId: owner, authorization: () => authorization });
  const ctx = () => ({ callId: crypto.randomUUID(), parentCallId: "", sessionId: "events-runtime", model: "test", signal: new AbortController().signal });
  const run = (name: string, input: unknown): Promise<any> => tools.find(t => t.name === name)!.handler(input, ctx()) as Promise<any>;
  const event = await run("crm_events", { operation: "save", title: "Synthetic conference", start_at: at, origin: "user" });
  await run("crm_event_participation", { operation: "save", event_id: event.event.id, record_id: "person", status: "expected", role: "attendee", origin: "user" });
  await run("crm_interactions", { operation: "save", participants: [{ record_id: "person", role: "proposer" }, { record_id: "second", role: "recipient" }], type: "proposal", occurred_at: "2026-01-02", body: "Synthetic proposal", origin: "user" });
  const page = await run("crm_timeline", { person_id: "person", limit: 1 });
  const record = await run("crm_get", { id: "person", timeline_limit: 1 });
  expect(record.timeline).toEqual(page.entries);
  expect(record.timeline_next_cursor).toBe(page.next_cursor);
  authorization = { capabilities: ["agents:read", "tools:use"] };
  expect((await run("crm_timeline", { person_id: "person" })).entries).toHaveLength(2);
  for (const name of ["crm_events", "crm_event_participation", "crm_interactions"]) await expect(run(name, { operation: "delete", id: "anything" })).rejects.toThrow(/authoriz|forbidden|requires/i);
  for (const denied of [undefined, { ...full, connectGrant: {} }]) {
    authorization = denied;
    for (const name of ["crm_events", "crm_event_participation", "crm_interactions", "crm_timeline"]) await expect(run(name, { operation: "list", person_id: "person" })).rejects.toThrow(/authoriz|forbidden|requires/i);
  }
  authorization = full;
  await expect(run("crm_timeline", { person_id: "person", owner_id: "forged" })).rejects.toThrow();
});
