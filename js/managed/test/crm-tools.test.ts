import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, expect, it } from "vitest";
import { crmTools, type CrmAuthorization } from "../src/crm-tools";

// Failure cases: grants or absent/revoked authority must not access private data;
// reads must not imply writes; model arguments cannot select another account;
// canceled calls cannot write; a replayed create cannot duplicate a contact.
const db = (env as unknown as { NANOCODEX_CRM: D1Database }).NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, (env as unknown as { CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] }).CRM_MIGRATIONS); });
const context = (callId = crypto.randomUUID()) => ({ callId, parentCallId: "", sessionId: "crm-session", model: "test", signal: new AbortController().signal });
const full = { capabilities: ["agents:read", "agents:write", "tools:use"] };

it("runs a chat-tool collection journey and shares records only within the account", async () => {
  const ownerId = crypto.randomUUID();
  let authorization: CrmAuthorization | undefined = full;
  const tools = crmTools({ db, ownerId, authorization: () => authorization });
  const run = (name: string, input: unknown, ctx = context()) => tools.find(t => t.name === name)!.handler(input, ctx) as Promise<any>;
  const createContext = context();
  const companyResult = await run("crm_save", { kind: "company", name: "Example Labs", tags: ["research"] }, createContext);
  expect(await run("crm_save", { kind: "company", name: "Example Labs", tags: ["research"] }, createContext)).toEqual(companyResult);
  const company = companyResult.record;
  const { record: person } = await run("crm_save", { kind: "person", name: "Alex Sample", company_id: company.id });
  const { note } = await run("crm_save_note", { record_id: person.id, body: "Discussed database performance", source_url: "https://example.com/meeting" });
  const freshTools = crmTools({ db, ownerId, authorization: () => full });
  const found = await freshTools.find(t => t.name === "crm_search")!.handler({ q: "database" }, context()) as any;
  expect(found.records.map((r: any) => r.id)).toEqual([person.id]);
  const stranger = crmTools({ db, ownerId: crypto.randomUUID(), authorization: () => full });
  expect(await stranger.find(t => t.name === "crm_search")!.handler({}, context())).toMatchObject({ records: [] });
  await expect(stranger.find(t => t.name === "crm_get")!.handler({ id: person.id }, context())).rejects.toThrow();

  authorization = { capabilities: ["agents:read", "tools:use"] };
  expect(await run("crm_search", {})).toMatchObject({ records: expect.any(Array) });
  for (const name of ["crm_save", "crm_save_note", "crm_delete", "crm_delete_note"]) {
    await expect(run(name, { id: person.id })).rejects.toThrow(/authoriz|forbidden|requires/i);
  }
  for (const denied of [undefined, { capabilities: ["agents:read", "agents:write"] }, { ...full, connectGrant: {} }]) {
    authorization = denied;
    for (const tool of tools) await expect(tool.handler({}, context())).rejects.toThrow(/authoriz|forbidden|requires/i);
  }
  authorization = full;
  await expect(run("crm_search", { owner_id: "someone-else" })).rejects.toThrow();
  const controller = new AbortController(); controller.abort();
  await expect(run("crm_save", { kind: "person", name: "Canceled" }, { ...context(), signal: controller.signal })).rejects.toThrow();
  expect((await run("crm_search", { q: "Canceled" })).records).toEqual([]);
  await run("crm_delete_note", { id: note.id });
  await run("crm_delete", { id: person.id });
  await run("crm_delete", { id: company.id });
  expect(await run("crm_search", {})).toMatchObject({ records: [] });
});

it("does not register CRM tools without storage", () => {
  expect(crmTools({ ownerId: "fixture-owner", authorization: () => full })).toEqual([]);
});

// Calendar background work must populate profiles without claiming meeting notes.
// A later user note must clear exactly that meeting, and current authority must
// be rechecked after provider I/O before importing any personal data.
it("imports a meeting, stores sourced research separately, and accepts the user's meeting note", async () => {
  const ownerId = crypto.randomUUID(), connectionId = "c".repeat(43);
  let authorization: CrmAuthorization | undefined = full;
  const ended = new Date(Date.now() - 3_600_000).toISOString();
  const event = { id: "meeting-one", status: "confirmed", summary: "Jamie / Owner", updated: ended,
    start: { dateTime: new Date(Date.now() - 7_200_000).toISOString() }, end: { dateTime: ended },
    organizer: { email: "owner@example.test", self: true },
    attendees: [{ email: "owner@example.test", self: true, responseStatus: "accepted" }, { email: "jamie@example.test", displayName: "Jamie Example", responseStatus: "accepted" }] };
  const tools = crmTools({ db, ownerId, authorization: () => authorization,
    calendarFetch: async request => {
      expect(request.method).toBe("GET");
      expect(request.headers.get("x-nanocodex-connector-connection")).toBe(connectionId);
      return Response.json({ items: [event] });
    } });
  const run = (name: string, input: unknown) => tools.find(t => t.name === name)!.handler(input, context()) as Promise<any>;
  expect(await run("crm_sync", { connection_id: connectionId })).toMatchObject({ complete: true });
  const pending = await run("crm_meetings", { operation: "list", needs_notes: true });
  expect(pending.meetings).toHaveLength(1);
  const person = (await run("crm_research", { operation: "queue" })).records[0];
  await run("crm_research", { operation: "save", record_id: person.id, status: "complete", summary: "Jamie works on databases.", company: "Example Labs", title: "Engineer",
    sources: [{ kind: "web", reference: "https://example.test/team/jamie" }, { kind: "calendar", reference: "meeting-one" }] });
  expect((await run("crm_get", { id: person.id })).research).toMatchObject({ status: "complete", company: "Example Labs" });
  expect((await run("crm_search", { q: "Example Labs", kind: "person" })).records.map((r: any) => r.id)).toEqual([person.id]);
  const stranger = crmTools({ db, ownerId: crypto.randomUUID(), authorization: () => full });
  expect(await stranger.find(t => t.name === "crm_search")!.handler({ q: "Example Labs" }, context())).toMatchObject({ records: [] });
  expect((await run("crm_meetings", { operation: "list", needs_notes: true })).meetings).toHaveLength(1);
  await run("crm_meetings", { operation: "note", meeting_id: pending.meetings[0].id, body: "We discussed faster imports; I will send the benchmark." });
  expect((await run("crm_meetings", { operation: "list", needs_notes: true })).meetings).toEqual([]);
  const next = crmTools({ db, ownerId, authorization: () => authorization,
    calendarFetch: async () => { authorization = undefined; return Response.json({ items: [{ ...event, id: "forbidden-meeting" }] }); } });
  await expect(next.find(t => t.name === "crm_sync")!.handler({ connection_id: connectionId }, context())).rejects.toThrow(/authoriz|requires/);
  authorization = full;
  expect((await run("crm_meetings", { operation: "list" })).meetings).toHaveLength(1);
});

// Alternate email identities must join the existing person, while dated role and
// expertise evidence remain independently searchable without changing manual facts.
it("recalls identities, dated employment and sourced facts from a person's profile", async () => {
  const ownerId = crypto.randomUUID();
  const ended = new Date(Date.now() - 3_600_000).toISOString();
  const tools = crmTools({ db, ownerId, authorization: () => full, calendarFetch: async () => Response.json({ items: [{
    id: "alternate-address-meeting", updated: ended, status: "confirmed", summary: "Research discussion",
    start: { dateTime: new Date(Date.now() - 7_200_000).toISOString() }, end: { dateTime: ended },
    organizer: { self: true, email: "owner@example.test" },
    attendees: [{ email: "secondary@example.test", displayName: "Alternate name" }],
  }] }) });
  const run = (name: string, input: unknown) => tools.find(t => t.name === name)!.handler(input, context()) as Promise<any>;
  const person = (await run("crm_save", { kind: "person", name: "Chosen name", email: "primary@example.test" })).record;
  const company = (await run("crm_save", { kind: "company", name: "Orbital Research" })).record;
  await run("crm_identity", { operation: "save", record_id: person.id, kind: "email", value: "Secondary@Example.Test", origin: "user" });
  await run("crm_facts", { operation: "save", record_id: person.id, predicate: "bio.expertise", value: ["compilers", "distributed systems"], origin: "source", sources: [{ kind: "web", reference: "https://example.test/team" }] });
  await run("crm_relationships", { operation: "save", from_id: person.id, to_id: company.id, type: "worked_at", role: "Compiler engineer", effective_from: "2020-01-01", effective_to: "2024-01-01", origin: "user", sources: [] });
  await run("crm_sync", { connection_id: "c".repeat(43) });
  const saved = await run("crm_get", { id: person.id });
  expect(saved.record).toMatchObject({ name: "Chosen name", email: "primary@example.test", company_id: null });
  expect(saved.identities).toEqual([expect.objectContaining({ kind: "email", normalized: "secondary@example.test" })]);
  expect(saved.facts).toEqual([expect.objectContaining({ predicate: "bio.expertise", value: ["compilers", "distributed systems"] })]);
  expect(saved.relationships).toEqual([expect.objectContaining({ type: "worked_at", role: "Compiler engineer", effective_to: "2024-01-01" })]);
  expect((await run("crm_search", { kind: "person" })).records).toHaveLength(1);
  for (const q of ["secondary@example.test", "compilers", "Orbital Research"]) {
    expect((await run("crm_search", { q, kind: "person" })).records.map((r: any) => r.id)).toEqual([person.id]);
  }
  const meetings = (await run("crm_meetings", { operation: "list", person_id: person.id, needs_notes: true })).meetings;
  expect(meetings).toHaveLength(1);
  expect((await run("crm_meetings", { operation: "get", id: meetings[0].id })).attendees[0].person_id).toBe(person.id);
});
