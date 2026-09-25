import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest } from "../src/crm";
import { crmMeetingRequest, importCalendarEvents, type CrmMeetingOperation } from "../src/crm-meetings";

// Failure scenarios defined before implementation: replayed and concurrent
// imports, recurring-ID collisions, stale provider snapshots resurrecting a
// cancelled meeting, profile edits lost during sync, exact-email ambiguity,
// self/resource guesses, research mistaken for user meeting notes, premature
// reminders, cross-owner links, note replay/move attacks, and cursor scope or
// date coercion. Exercise production migrations and real workerd D1 throughout.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const owner = () => `account-${crypto.randomUUID()}`;
const call = (account: string, operation: CrmMeetingOperation, input: unknown, createId = crypto.randomUUID()): Promise<any> =>
  crmMeetingRequest(db, account, operation, input, createId);
const person = async (account: string, input: Record<string, unknown>): Promise<any> =>
  crmRequest(db, account, "save", { kind: "person", ...input }, crypto.randomUUID());
const event = (id = "event-1", patch: Record<string, unknown> = {}) => ({
  id, summary: "Product planning", description: "Agenda, not meeting notes", location: "Room 2",
  htmlLink: "https://calendar.google.com/calendar/event?eid=synthetic", status: "confirmed", eventType: "default",
  updated: "2024-05-01T00:00:00Z", start: { dateTime: "2024-05-01T10:00:00-04:00" }, end: { dateTime: "2024-05-01T11:00:00-04:00" },
  organizer: { email: "self@example.test", self: true, displayName: "Calendar owner" },
  attendees: [{ email: "self@example.test", self: true, responseStatus: "accepted" },
    { email: "Ada@Example.Test", displayName: "Calendar Ada", responseStatus: "accepted" },
    { email: "room@example.test", resource: true, responseStatus: "accepted" }],
  ...patch,
});
const sync = (account: string, events: unknown[], extra: { connection_id?: string; calendar_id?: string } = {}): Promise<any> =>
  importCalendarEvents(db, account, { connection_id: "google-1", calendar_id: "primary", events, ...extra }, Date.parse("2024-06-01T00:00:00Z"));
const invalid = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "invalid_input" });
const missing = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "not_found" });

describe("calendar meetings on real D1", () => {
  it("imports actual Google metadata, links exact normalized emails and preserves manual profiles and meeting notes on replay and updates", async () => {
    const account = owner();
    const { record: ada } = await person(account, { name: "Ada's chosen name", email: " ADA@example.test ", title: "Architect", phone: "555-0100", tags: ["manual"] });
    const imported = await sync(account, [event()]);
    expect(imported).toMatchObject({ imported: 1, skipped: 0, people_created: 0, unresolved: [] });
    const { meetings } = await call(account, "list", { needs_notes: true });
    expect(meetings).toHaveLength(1);
    expect(meetings[0]).toMatchObject({ title: "Product planning", description: "Agenda, not meeting notes", location: "Room 2", htmlLink: event().htmlLink,
      start: "2024-05-01T10:00:00-04:00", end: "2024-05-01T11:00:00-04:00", all_day: false, status: "confirmed", needs_notes: true,
      organizer: { email: "self@example.test", self: true }, source_updated: "2024-05-01T00:00:00Z" });
    expect(meetings[0]).not.toHaveProperty("owner_id");
    const detail = await call(account, "get", { id: meetings[0].id });
    expect(detail.attendees).toEqual([expect.objectContaining({ email: "ada@example.test", name: "Calendar Ada", person_id: ada.id, response_status: "accepted" })]);
    expect(detail.notes).toEqual([]);
    const { note } = await call(account, "note", { meeting_id: meetings[0].id, body: "Agreed to deliver a prototype" }, "stable-note");
    expect(await sync(account, [event()])).toMatchObject({ imported: 0, skipped: 1, people_created: 0 });
    await sync(account, [event("event-1", { updated: "2024-05-02T00:00:00Z", summary: "Updated provider title" })]);
    const next = await call(account, "get", { id: meetings[0].id });
    expect(next.meeting).toMatchObject({ title: "Updated provider title", needs_notes: false });
    expect(next.notes).toEqual([note]);
    expect((await crmRequest(db, account, "get", { id: ada.id }, "unused") as any).record).toMatchObject({ name: ada.name, email: ada.email, phone: "555-0100", title: "Architect", tags: ["manual"], company_id: null });
  });

  it("deduplicates concurrent imports and recurring instances per connection/calendar without name merges or domain-derived companies", async () => {
    const account = owner();
    await person(account, { name: "Calendar Ada", email: "someone-else@example.test" });
    const copy = event("series_20240501T140000Z", { recurringEventId: "series", originalStartTime: { dateTime: "2024-05-01T14:00:00Z" } });
    const outcomes = await Promise.all([sync(account, [copy]), sync(account, [copy])]);
    expect(outcomes.reduce((sum, item) => sum + item.imported, 0)).toBe(1);
    expect(outcomes.reduce((sum, item) => sum + item.people_created, 0)).toBe(1);
    await sync(account, [event("series_20240502T140000Z", { recurringEventId: "series" })]);
    await sync(account, [copy], { calendar_id: "another@example.test" });
    await sync(account, [copy], { connection_id: "google-2" });
    expect((await call(account, "list", {})).meetings).toHaveLength(4);
    const records = (await crmRequest(db, account, "search", {}, "unused") as any).records;
    expect(records).toHaveLength(2);
    expect(records.every((row: any) => row.kind === "person" && row.company_id === null)).toBe(true);
    const newPerson = records.find((row: any) => row.email === "ada@example.test");
    await crmRequest(db, account, "save", { id: newPerson.id, name: "Manually renamed", website: "https://ada.example" }, "unused");
    await sync(account, [event("third-meeting")]);
    expect((await crmRequest(db, account, "get", { id: newPerson.id }, "unused") as any).record.name).toBe("Manually renamed");
  });

  it("keeps duplicate-email and missing-email invitees unresolved and does not link another owner or a company", async () => {
    const account = owner(), other = owner();
    const { record: foreign } = await person(other, { name: "Foreign Ada", email: "ada@example.test" });
    await person(account, { name: "First Ada", email: "ada@example.test" });
    await person(account, { name: "Second Ada", email: " ADA@EXAMPLE.TEST " });
    await crmRequest(db, account, "save", { kind: "company", name: "A company", email: "company@example.test" }, "company");
    const result = await sync(account, [event("ambiguous", { attendees: [
      { email: "ada@example.test", displayName: "First Ada" }, { displayName: "Nameless address" },
      { email: "company@example.test", displayName: "Human Company Contact" },
    ] })]);
    expect(result.people_created).toBe(1);
    expect(result.unresolved).toEqual(expect.arrayContaining([
      expect.objectContaining({ event_id: "ambiguous", email: "ada@example.test", reason: "ambiguous_email" }),
      expect.objectContaining({ event_id: "ambiguous", email: null, reason: "missing_email" }),
    ]));
    const meeting = (await call(account, "list", {})).meetings[0];
    const detail = await call(account, "get", { id: meeting.id });
    expect(detail.attendees.find((row: any) => row.email === "ada@example.test").person_id).toBeNull();
    expect(detail.attendees.every((row: any) => row.person_id !== foreign.id)).toBe(true);
    expect((await call(account, "list", { q: "nameless address", needs_notes: true })).meetings).toHaveLength(1);
  });

  it("excludes self/resource/nondefault/cancelled/declined new events and never invents attendance or timezone offsets", async () => {
    const account = owner();
    const result = await sync(account, [
      event("cancelled", { status: "cancelled" }), event("declined", { attendees: [{ email: "self@example.test", self: true, responseStatus: "declined" }, { email: "ada@example.test" }] }),
      event("focus", { eventType: "focusTime" }), event("room-only", { attendees: [{ email: "room@example.test", resource: true }] }),
      event("self-only", { attendees: [{ email: "self@example.test", self: true }] }),
      event("invalid-offset", { start: { dateTime: "2024-05-01T10:00:00", timeZone: "America/New_York" } }),
      event("invalid-date", { start: { date: "2024-02-31" }, end: { date: "2024-03-02" } }),
    ]);
    expect(result).toMatchObject({ imported: 0, skipped: 7, people_created: 0 });
    expect((await call(account, "list", {})).meetings).toEqual([]);
    await sync(account, [event("all-day", { start: { date: "2024-05-01" }, end: { date: "2024-05-02" }, attendees: [{ email: "ada@example.test", responseStatus: "needsAction" }] })]);
    expect((await call(account, "list", { needs_notes: true })).meetings).toEqual([]);
    const allDay = (await call(account, "list", {})).meetings[0];
    expect(allDay).toMatchObject({ start: "2024-05-01", end: "2024-05-02", all_day: true, needs_notes: false });
    const detail = await call(account, "get", { id: allDay.id });
    expect(detail.attendees[0].response_status).toBe("needsAction");
    expect(detail.attendees[0]).not.toHaveProperty("attended");
  });

  it("updates existing cancellation/decline state, preserves user notes/skips and rejects stale or unversioned resurrection", async () => {
    const account = owner();
    await sync(account, [event("cancel-me"), event("decline-me"), event("tombstone")]);
    const all = (await call(account, "list", {})).meetings;
    const cancelId = all.find((row: any) => row.event_id === "cancel-me").id;
    await call(account, "note", { meeting_id: cancelId, body: "Recorded outcome" }, "cancel-note");
    await call(account, "skip", { id: cancelId, reason: "No further notes" });
    await sync(account, [{ id: "cancel-me", status: "cancelled", updated: "2024-05-03T00:00:00Z" },
      event("decline-me", { updated: "2024-05-03T00:00:00Z", attendees: [{ self: true, email: "self@example.test", responseStatus: "declined" }, { email: "ada@example.test" }] }),
      { id: "tombstone", status: "cancelled" }]);
    expect((await call(account, "list", { needs_notes: true })).meetings).toEqual([]);
    const result = await sync(account, [event("cancel-me"), event("decline-me"), event("tombstone", { updated: "2024-05-31T00:00:00Z" })]);
    expect(result).toMatchObject({ imported: 0, skipped: 3 });
    const detail = await call(account, "get", { id: cancelId });
    expect(detail.meeting).toMatchObject({ status: "cancelled", skipped: true, skip_reason: "No further notes", title: "Product planning" });
    expect(detail.notes).toHaveLength(1);
    await sync(account, [event("decline-me", { updated: "2024-05-04T00:00:00Z" })]);
    expect((await call(account, "list", { needs_notes: true })).meetings.map((row: any) => row.event_id)).toEqual(["decline-me"]);
  });

  it("asks only about ended confirmed human meetings lacking actual meeting notes, with reversible skip and replay-safe note edits", async () => {
    const account = owner();
    const future = new Date(Date.now() + 86400000).toISOString();
    await sync(account, [event("past"), event("future", { start: { dateTime: future }, end: { dateTime: new Date(Date.now() + 90000000).toISOString() } }),
      event("tentative", { status: "tentative" }), event("unknown-end", { endTimeUnspecified: true }),
      event("all-declined", { attendees: [{ email: "ada@example.test", responseStatus: "declined" }] })]);
    const past = (await call(account, "list", { needs_notes: true })).meetings;
    expect(past.map((row: any) => row.event_id)).toEqual(["past"]);
    const detail = await call(account, "get", { id: past[0].id });
    await crmRequest(db, account, "save_note", { record_id: detail.attendees[0].person_id, body: "Researched biography: Ada designs systems", source_url: "https://example.test/bio" }, "research-bio");
    expect((await call(account, "list", { needs_notes: true })).meetings).toHaveLength(1);
    await call(account, "skip", { id: past[0].id, reason: "Social invitation" });
    expect((await call(account, "list", { needs_notes: true })).meetings).toEqual([]);
    await call(account, "skip", { id: past[0].id, skipped: false });
    const created = await call(account, "note", { meeting_id: past[0].id, body: "We chose the blue prototype" }, "same-note");
    expect(await call(account, "note", { meeting_id: past[0].id, body: "Replay must preserve the original" }, "same-note")).toEqual(created);
    expect((await call(account, "list", { needs_notes: true })).meetings).toEqual([]);
    const edited = await call(account, "note", { id: created.note.id, meeting_id: past[0].id, body: "Edited user notes" });
    expect(edited.note).toMatchObject({ body: "Edited user notes", created_at: created.note.created_at });
    expect((await call(account, "get", { id: past[0].id })).notes).toEqual([edited.note]);
  });

  it("enforces owner-safe notes and attendee links, immutable note parentage, and person deletion without deleting calendar metadata", async () => {
    const alice = owner(), bob = owner();
    await sync(alice, [event("private"), event("another")]);
    const meetings = (await call(alice, "list", {})).meetings;
    const detail = await call(alice, "get", { id: meetings[0].id });
    const personId = detail.attendees[0].person_id;
    const { note } = await call(alice, "note", { meeting_id: meetings[0].id, body: "Private meeting" });
    expect((await call(bob, "list", { q: "Ada" })).meetings).toEqual([]);
    await missing(call(bob, "get", { id: meetings[0].id }));
    await missing(call(bob, "skip", { id: meetings[0].id }));
    await missing(call(bob, "note", { meeting_id: meetings[0].id, body: "Bad" }));
    await missing(call(bob, "note", { id: note.id, meeting_id: meetings[0].id, body: "Bad" }));
    await invalid(call(alice, "note", { id: note.id, meeting_id: meetings[1].id, body: "Move" }));
    await invalid(call(alice, "note", { meeting_id: meetings[1].id, body: "Replay to another meeting" }, note.id));
    const { record: bobPerson } = await person(bob, { name: "Bob contact", email: "bob@example.test" });
    await expect(db.prepare("UPDATE crm_meeting_attendees SET person_id = ? WHERE owner_id = ? AND meeting_id = ?").bind(bobPerson.id, alice, meetings[0].id).run()).rejects.toThrow();
    await expect(db.prepare("INSERT INTO crm_meeting_notes (owner_id,id,meeting_id,body,created_at,updated_at) VALUES (?,?,?,?,?,?)").bind(bob, "forged", meetings[0].id, "bad", 1, 1).run()).rejects.toThrow();
    await crmRequest(db, alice, "delete", { id: personId }, "unused");
    const afterDelete = await call(alice, "get", { id: meetings[0].id });
    expect(afterDelete.attendees[0]).toMatchObject({ person_id: null, email: "ada@example.test" });
    expect(afterDelete.notes).toEqual([note]);
  });

  it("searches title and person names/emails literally, intersects filters and paginates with scoped cursors and strict absolute dates", async () => {
    const account = owner();
    await sync(account, [event("c"), event("a"), event("b"), event("percent", { summary: "100%_ready" })]);
    const first = await call(account, "list", { limit: 2 });
    const last = await call(account, "list", { limit: 2, cursor: first.next_cursor });
    expect(new Set([...first.meetings, ...last.meetings].map((row: any) => row.id)).size).toBe(4);
    expect(last.next_cursor).toBeNull();
    await invalid(call(owner(), "list", { cursor: first.next_cursor }));
    await invalid(call(account, "list", { needs_notes: true, cursor: first.next_cursor }));
    const detail = await call(account, "get", { id: first.meetings[0].id });
    const personId = detail.attendees[0].person_id;
    await crmRequest(db, account, "save", { id: personId, name: "Ada's preferred name" }, "unused");
    for (const q of ["Product", "ada@", "preferred"]) {
      const expected = q === "Product" ? 3 : 4;
      expect((await call(account, "list", { q, person_id: personId, needs_notes: true, from: "2024-05-01T00:00:00Z", to: "2024-05-02T00:00:00Z" })).meetings).toHaveLength(expected);
    }
    expect((await call(account, "list", { q: "%_" })).meetings).toHaveLength(1);
    expect((await call(account, "list", { q: "' OR 1=1 --" })).meetings).toEqual([]);
    expect((await call(account, "list", { from: "2024-05-02" })).meetings).toEqual([]);
    for (const input of [{ limit: 0 }, { limit: 101 }, { limit: "2" }, { needs_notes: "true" }, { from: "2024-05-01T00:00:00" }, { from: "2024-02-31" }, { from: "2024-05-02", to: "2024-05-01" }, { owner_id: "forged" }, { cursor: "e30" }]) await invalid(call(account, "list", input));
    await invalid(call(account, "note", { meeting_id: first.meetings[0].id, body: "   " }));
    await invalid(call(account, "skip", { id: first.meetings[0].id, skipped: "true" }));
    await invalid(call(account, "get", { id: first.meetings[0].id, owner_id: "forged" }));
  });
  it("validates the whole page before writes and preserves omitted attendee links on newer snapshots", async () => {
    const account = owner();
    await invalid(sync(account, [event("valid-before-malformed"), { status: "confirmed" }]));
    expect((await call(account, "list", {})).meetings).toEqual([]);
    await invalid(sync(account, Array.from({ length: 101 }, (_, n) => event(`too-many-${n}`))));
    await invalid(sync(account, [event("malformed-guests", { attendees: "not an array" })]));
    expect((await call(account, "list", {})).meetings).toEqual([]);
    await sync(account, [event("omitted")]);
    const meeting = (await call(account, "list", {})).meetings[0];
    const before = await call(account, "get", { id: meeting.id });
    await sync(account, [event("omitted", { updated: "2024-05-03T00:00:00Z", attendeesOmitted: true, attendees: [], summary: "Updated sparse attendees" })]);
    const after = await call(account, "get", { id: meeting.id });
    expect(after.attendees).toEqual(before.attendees);
    expect(after.meeting).toMatchObject({ title: "Updated sparse attendees", needs_notes: true, attendees_complete: false });
    await sync(account, [event("omitted", { updated: "2024-05-03T00:00:00Z", attendees: [{ email: "replacement@example.test", displayName: "Replacement guest" }] })]);
    const repaired = await call(account, "get", { id: meeting.id });
    expect(repaired.meeting.attendees_complete).toBe(true);
    expect(repaired.attendees).toEqual([expect.objectContaining({ email: "replacement@example.test" })]);
  });

  it("accepts a new event without updated, skips its replay, and fences concurrent stale cancellation updates", async () => {
    const account = owner();
    const unversioned = event("unversioned");
    delete (unversioned as { updated?: string }).updated;
    expect(await sync(account, [unversioned])).toMatchObject({ imported: 1 });
    expect(await sync(account, [unversioned])).toMatchObject({ imported: 0, skipped: 1 });
    await sync(account, [event("race")]);
    await Promise.all([
      sync(account, [{ id: "race", status: "cancelled", updated: "2024-05-05T00:00:00Z" }]),
      sync(account, [event("race", { updated: "2024-05-04T00:00:00Z" })]),
    ]);
    expect((await call(account, "list", {})).meetings.find((m: any) => m.event_id === "race").status).toBe("cancelled");
  });

  it("retains unseen cancellations against stale imports and imports external organizers omitted from attendees", async () => {
    const account = owner();
    await sync(account, [{ id: "cancelled-before-first-import", status: "cancelled", updated: "2024-05-05T00:00:00Z" }]);
    await sync(account, [event("cancelled-before-first-import")]);
    expect((await call(account, "list", {})).meetings).toEqual([]);
    await sync(account, [event("cancelled-before-first-import", { updated: "2024-05-06T00:00:00Z" })]);
    expect((await call(account, "list", { needs_notes: true })).meetings).toHaveLength(1);
    await sync(account, [event("organizer-only", {
      organizer: { email: "host@example.test", displayName: "External host" },
      attendees: [{ email: "self@example.test", self: true, responseStatus: "accepted" }],
    })]);
    const meeting = (await call(account, "list", { q: "External host" })).meetings[0];
    expect((await call(account, "get", { id: meeting.id })).attendees).toEqual([
      expect.objectContaining({ email: "host@example.test", name: "External host", person_id: expect.any(String) }),
    ]);
  });

  it("reports bounded large invitations while importing other valid events on their page", async () => {
    const account = owner();
    const result = await sync(account, [event("large-event", { attendees: Array.from({ length: 201 }, (_, i) => ({ email: `guest-${i}@example.test` })) }), event("ordinary-event")]);
    expect(result).toMatchObject({ imported: 1, skipped: 1, limited_events: 1 });
    expect((await call(account, "list", {})).meetings.map((m: any) => m.event_id)).toEqual(["ordinary-event"]);
  });

});
