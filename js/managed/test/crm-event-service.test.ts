import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, expect, it } from "vitest";
import { importCalendarEvents } from "../src/crm-meetings";
import { crmRequest } from "../src/crm";
import { crmEventRequest, crmParticipationRequest, crmInteractionRequest } from "../src/crm-events";
// Scenarios defined before implementation: owner isolation, immutable endpoints,
// invalid provenance/dates, roster pagination, mismatched links; replay regression.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const at = "2026-01-02T12:00:00Z";
const event = (owner: string, input: any, create = "event"): Promise<any> => crmEventRequest(db, owner, "save", input, create);
const participation = (owner: string, input: any, create = "participation"): Promise<any> => crmParticipationRequest(db, owner, "save", input, create);
const interaction = (owner: string, input: any, create = "interaction"): Promise<any> => crmInteractionRequest(db, owner, "save", input, create);
const invalid = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "invalid_input" });
async function setup() {
 const owner = crypto.randomUUID();
 for (const id of ["person", "other"]) await crmRequest(db, owner, "save", { kind: "person", name: id }, id);
 await crmRequest(db, owner, "save", { kind: "company", name: "Synthetic Company" }, "company");
 await event(owner, { title: "Synthetic Event", start_at: at, origin: "user" });
 return owner;
}
it("keeps organizer role separate from attendance and paginates an owned roster", async () => {
 const owner = await setup();
 await participation(owner, { event_id: "event", record_id: "company", role: "organizer", origin: "user" }, "organizer");
 await participation(owner, { event_id: "event", record_id: "person", status: "invited", origin: "user" });
 const first: any = await crmEventRequest(db, owner, "get", { id: "event", roster_limit: 1 }, "unused");
 expect(first.participation).toHaveLength(1); expect(first.next_cursor).toEqual(expect.any(String));
 const next: any = await crmEventRequest(db, owner, "get", { id: "event", roster_cursor: first.next_cursor }, "unused");
 expect(next.participation).toHaveLength(1);
 const rows = [...first.participation, ...next.participation];
 expect(rows.find(p => p.record_id === "company")).toMatchObject({ role: "organizer", status: "unknown", person_id: null });
 expect(rows.find(p => p.person_id === "person").status).toBe("invited");
 await expect(crmEventRequest(db, crypto.randomUUID(), "get", { id: "event" }, "unused")).rejects.toMatchObject({ code: "not_found" });
 await invalid(participation(owner, { id: "participation", record_id: "other" }));
 await invalid(participation(owner, { id: "organizer", role: "attendee" }));
});
it("validates provenance and absolute timestamps and preserves stable create replays", async () => {
 const owner = await setup();
 const replay = await event(owner, { title: "Replacement", start_at: at, origin: "user" });
 expect(replay.event.title).toBe("Synthetic Event");
 for (const extra of [{ start_at: "2026-02-30T00:00:00Z" }, { origin: "inferred" }, { origin: "source", sources: [{ kind: "web", reference: "opaque" }] }]) await invalid(event(owner, { title: "Bad", start_at: at, origin: "user", ...extra }, crypto.randomUUID()));
 const p = { event_id: "event", record_id: "person", status: "invited", origin: "user" };
 await participation(owner, p); expect((await participation(owner, { ...p, status: "attended" })).participation.status).toBe("invited");
 const i = { person_id: "person", event_id: "event", occurred_at: at, body: "First observation", origin: "user" };
 await interaction(owner, i); expect((await interaction(owner, { ...i, body: "Replacement" })).interaction.body).toBe("First observation");
 await invalid(interaction(owner, { id: "interaction", person_id: "other" }));
 await invalid(interaction(owner, { ...i, connection_id: "google" }, "bad-email"));
 await interaction(owner, { ...i, person_id: "other" }, "independent-person");
 await crmEventRequest(db, owner, "delete", { id: "event" }, "unused");
 const saved: any = await crmInteractionRequest(db, owner, "get", { id: "interaction" }, "unused");
 expect(saved.interaction).toMatchObject({ body: "First observation", event_id: null });
});

it("stores a single shared interaction with roles and date precision and retains remaining participants", async () => {
 const owner = await setup();
 const input = { participants: [{ record_id: "person", role: "proposer" }, { record_id: "other", role: "recipient" }], type: "proposal", summary: "Proposed partnership", body: "Synthetic proposal", occurred_at: "2026-01-02", origin: "user" };
 const saved = await interaction(owner, input);
 expect((await interaction(owner, input)).interaction.id).toBe(saved.interaction.id);
 expect(saved.interaction).toMatchObject({ type: "proposal", precision: "date", occurred_at: "2026-01-02", participants: [{ record_id: "other", role: "recipient" }, { record_id: "person", role: "proposer" }] });
 for (const person_id of ["person", "other"]) {
  const page: any = await crmInteractionRequest(db, owner, "list", { person_id }, "unused");
  expect(page.interactions).toHaveLength(1); expect(page.interactions[0].id).toBe("interaction");
 }
 expect((await crmInteractionRequest(db, owner, "list", {}, "unused") as any).interactions).toHaveLength(1);
 await invalid(interaction(owner, { id: "interaction", participants: [{ record_id: "person" }] }));
 await crmRequest(db, owner, "delete", { id: "person" }, "unused");
 const remaining: any = await crmInteractionRequest(db, owner, "get", { id: "interaction" }, "unused");
 expect(remaining.interaction.participants).toEqual([{ record_id: "other", role: "recipient" }]);
});

it("checks native source membership and detaches deleted source links without losing manual text", async () => {
 const owner = await setup();
 await crmRequest(db, owner, "save", { id: "person", email: "guest@example.test" }, "unused");
 await importCalendarEvents(db, owner, { connection_id: "google", calendar_id: "primary", events: [{ id: "native", summary: "Synthetic", start: { dateTime: at }, end: { dateTime: "2026-01-02T13:00:00Z" }, attendees: [{ email: "guest@example.test" }] }] });
 const meeting = await db.prepare("SELECT id FROM crm_meetings WHERE owner_id=?").bind(owner).first<{id:string}>();
 await db.prepare("INSERT INTO crm_email_imports(owner_id,connection_id,message_id,record_id,note_id,imported_at) VALUES (?,'google','email','person','note',1)").bind(owner).run();
 const base = { person_id: "person", occurred_at: at, body: "Manual observation survives source deletion", origin: "user", meeting_id: meeting!.id, connection_id: "google", message_id: "email" };
 await invalid(interaction(owner, { ...base, person_id: "other" }, "wrong"));
 await expect(interaction(owner, { ...base, participants: [{record_id:"foreign"}] }, "foreign")).rejects.toMatchObject({ code: "invalid_input" });
 const otherOwner = await setup();
 await crmRequest(db, otherOwner, "save", { kind: "person", name: "Foreign" }, "foreign-person");
 await expect(interaction(owner, { ...base, person_id: undefined, participants: [{ record_id: "person" }, { record_id: "foreign-person" }] }, "rejected")).rejects.toMatchObject({ code: "not_found" });
 expect(await db.prepare("SELECT id FROM crm_interactions WHERE owner_id=? AND id='rejected'").bind(owner).first()).toBeNull();
 expect((await db.prepare("SELECT record_id FROM crm_interaction_participants WHERE owner_id=? AND interaction_id='rejected'").bind(owner).all()).results).toEqual([]);
 await interaction(owner, base);
 await db.prepare("DELETE FROM crm_meetings WHERE owner_id=?").bind(owner).run();
 await db.prepare("DELETE FROM crm_email_imports WHERE owner_id=?").bind(owner).run();
 const result: any = await crmInteractionRequest(db, owner, "get", { id: "interaction" }, "unused");
 expect(result.interaction).toMatchObject({ body: base.body, meeting_id: null, connection_id: null, message_id: null });
 await crmRequest(db, owner, "delete", { id: "person" }, "unused");
 expect((await crmInteractionRequest(db, owner, "get", { id: "interaction" }, "unused") as any).interaction).toMatchObject({ person_id: null, participants: [] });
});

// Provenance references may describe a user statement or local document. Removing
// the original participant must not make surviving shared history uneditable.
it("retains free-form provenance and permits editing history after a linked participant is deleted", async () => {
 const owner = await setup();
 await crmRequest(db, owner, "save", { id: "person", email: "guest@example.test" }, "unused");
 await importCalendarEvents(db, owner, { connection_id: "google", calendar_id: "primary", events: [{ id: "native", summary: "Synthetic", start: { dateTime: at }, end: { dateTime: "2026-01-02T13:00:00Z" }, attendees: [{ email: "guest@example.test" }] }] });
 const meeting = await db.prepare("SELECT id FROM crm_meetings WHERE owner_id=?").bind(owner).first<{id:string}>();
 const sources = [{ kind: "user", reference: "User statement in this conversation" }, { kind: "document", reference: "/documents/Synthetic notes.md" }];
 await interaction(owner, { participants: [{ record_id: "person" }, { record_id: "other" }], occurred_at: at, body: "Original", meeting_id: meeting!.id, origin: "user", sources });
 await crmRequest(db, owner, "delete", { id: "person" }, "unused");
 const saved = await interaction(owner, { id: "interaction", body: "Corrected observation" });
 expect(saved.interaction).toMatchObject({ body: "Corrected observation", sources, participants: [{ record_id: "other", role: null }] });
});
