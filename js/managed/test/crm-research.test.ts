import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest } from "../src/crm";
import { crmResearchRequest, type CrmResearchOperation } from "../src/crm-research";

// Failure modes defined before implementation: account leakage through any
// operation or cursor, missing imported people, pagination gaps as work is saved,
// stale facts never refreshing, repeated ambiguous-identity retries, malformed
// provenance, orphan profiles, and research overwriting manual fields or being
// mistaken for user meeting notes. Exercise the shipped migrations in real D1.
// Reference syntax validation cannot establish that a model fetched a source;
// tool/cron instructions must require actual reads and prohibit invented IDs.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const owner = () => `research-account-${crypto.randomUUID()}`;
const call = (account: string, operation: CrmResearchOperation, input: unknown): Promise<any> => crmResearchRequest(db, account, operation, input);
const create = (account: string, input: Record<string, unknown>, id = crypto.randomUUID()): Promise<any> => crmRequest(db, account, "save", input, id);
const invalid = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "invalid_input" });
const missing = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "not_found" });
const complete = (recordId: string) => ({ record_id: recordId, summary: "Synthetic Guest designs laboratory tools.", company: "Synthetic Works", title: "Designer", website: "https://synthetic.example/team", sources: [{ kind: "web", reference: "https://synthetic.example/team", detail: "Team biography lists the role." }], status: "complete" });

// One shared journey proves Calendar imports become research work while research
// stays separate from meeting notes. No actual calendar or account data is used.
const event = {
  id: "synthetic-research-event", summary: "Synthetic planning meeting", status: "confirmed",
  start: { dateTime: "2025-01-01T10:00:00Z" }, end: { dateTime: "2025-01-01T11:00:00Z" },
  attendees: [{ email: "synthetic-guest@example.test", displayName: "Synthetic Guest", responseStatus: "accepted" }],
};

describe("private research profiles and refresh queue in D1", () => {
  it("queues Calendar-created people and keeps research out of meeting-note status", async () => {
    const { importCalendarEvents, crmMeetingRequest } = await import("../src/crm-meetings");
    const account = owner();
    await importCalendarEvents(db, account, { connection_id: "synthetic-google", calendar_id: "primary", events: [event] });
    const queue = await call(account, "queue", {});
    expect(queue.records).toHaveLength(1);
    expect(queue.records[0]).toMatchObject({ kind: "person", email: "synthetic-guest@example.test", research: null });
    await call(account, "save", complete(queue.records[0].id));
    const listed: any = await crmMeetingRequest(db, account, "list", {}, "synthetic-unused-id");
    expect(listed.meetings).toHaveLength(1);
    expect(listed.meetings[0].needs_notes).toBe(true);
    expect((await crmRequest(db, account, "get", { id: queue.records[0].id }, "unused") as any).notes).toEqual([]);
  });

  it("paginates missing people without gaps as earlier pages receive research", async () => {
    const account = owner();
    await create(account, { kind: "company", name: "Synthetic Works" }, "company-only");
    const ids = Array.from({ length: 23 }, (_, i) => `person-${String(i).padStart(2, "0")}`);
    for (const id of [...ids].reverse()) await create(account, { kind: "person", name: `Synthetic ${id}`, tags: ["synthetic"] }, id);
    await db.prepare("UPDATE crm_records SET created_at = 1234 WHERE owner_id = ?").bind(account).run();
    const first = await call(account, "queue", {});
    expect(first.records).toHaveLength(20);
    expect(first.records.map((r: any) => r.id)).toEqual(ids.slice(0, 20));
    expect(first.records[0]).toMatchObject({ tags: ["synthetic"], research: null });
    expect(first.records[0]).not.toHaveProperty("owner_id");
    expect(first.next_cursor).toEqual(expect.any(String));
    for (const record of first.records) await call(account, "save", complete(record.id));
    const next = await call(account, "queue", { cursor: first.next_cursor, limit: 100 });
    expect(next.records.map((r: any) => r.id)).toEqual(ids.slice(20));
    expect(next.next_cursor).toBeNull();
    expect((await call(account, "queue", { limit: 100 })).records.map((r: any) => r.id)).toEqual(ids.slice(20));
  });

  it("persists updated facts and typed sources without mutating manual record fields", async () => {
    const account = owner();
    const { record } = await create(account, { kind: "person", name: "Synthetic Guest", title: "Manual title", website: "https://manual.example", tags: ["keep"] });
    expect(await call(account, "get", { record_id: record.id })).toEqual({ research: null });
    const first = await call(account, "save", complete(record.id));
    expect(first.research).toMatchObject(complete(record.id));
    expect(first.research.checked_at).toEqual(expect.any(Number));
    expect(first.research).not.toHaveProperty("owner_id");
    expect(await call(account, "get", { record_id: record.id })).toEqual(first);
    const sources = [
      { kind: "email", reference: "synthetic-message-001", detail: "Recent signature names the company." },
      { kind: "email", reference: "https://mail.google.com/mail/u/0/#all/synthetic-thread-002" },
      { kind: "calendar", reference: "synthetic-calendar-event-003" },
      { kind: "calendar", reference: "https://calendar.google.com/calendar/event?eid=c3ludGhldGlj" },
    ];
    const updated = await call(account, "save", { record_id: record.id, summary: "Synthetic Guest changed roles.", title: "Research title", website: null, sources, status: "complete" });
    expect(updated.research).toMatchObject({ company: null, title: "Research title", website: null, sources });
    expect(updated.research.checked_at).toBeGreaterThanOrEqual(first.research.checked_at);
    expect(await call(account, "get", { record_id: record.id })).toEqual(updated);
    const manual: any = await crmRequest(db, account, "get", { id: record.id }, "unused");
    expect(manual.record).toEqual(record);
    expect(manual.notes).toEqual([]);
    expect((await call(account, "queue", {})).records).toEqual([]);
  });

  it("projects completed research onto empty person fields without rewriting manual data", async () => {
    const { crmRelationshipRequest } = await import("../src/crm-context");
    const account = owner();
    const company = (await create(account, { kind: "company", name: "Synthetic Works" }, "employer")).record;
    const person = (await create(account, { kind: "person", name: "Synthetic Guest" }, "guest")).record;
    const evidence = [{ kind: "web", reference: "https://synthetic.example/team" }];
    await call(account, "save", complete(person.id));
    let viewed: any = await crmRequest(db, account, "get", { id: person.id }, "unused");
    expect(viewed.record).toMatchObject({ title: "Designer", website: "https://synthetic.example/team", company_id: null,
      field_origins: { title: "research", website: "research" } });
    await crmRelationshipRequest(db, account, "save", {
      from_id: person.id, to_id: company.id, type: "works_at", role: "Designer", origin: "source", sources: evidence, confidence: "high",
    }, "employment");
    viewed = await crmRequest(db, account, "get", { id: person.id }, "unused");
    expect(viewed.record).toMatchObject({ company_id: company.id, field_origins: { company_id: "relationship" } });
    expect((await crmRequest(db, account, "search", { company_id: company.id }, "unused") as any).records.map((r: any) => r.id)).toEqual([person.id]);
    expect((await db.prepare("SELECT title,website,company_id FROM crm_records WHERE owner_id=? AND id=?")
      .bind(account, person.id).first())!).toMatchObject({ title: null, website: null, company_id: null });
    const saved: any = await crmRequest(db, account, "save", { id: person.id, title: "User title", website: "https://manual.example", company_id: company.id }, "unused");
    expect(saved.record).toMatchObject({ title: "User title", website: "https://manual.example", company_id: company.id });
    expect(saved.record.field_origins).toBeUndefined();
    await call(account, "save", { ...complete(person.id), title: "Changed research", website: "https://synthetic.example/new" });
    expect((await crmRequest(db, account, "get", { id: person.id }, "unused") as any).record).toMatchObject({ title: "User title", website: "https://manual.example" });
    await crmRequest(db, account, "save", { id: person.id, title: null, website: null, company_id: null }, "unused");
    await call(account, "save", { record_id: person.id, summary: "Identity uncertain", company: "Synthetic Works", title: "Guess",
      website: "https://synthetic.example/guess", sources: evidence, status: "needs_review" });
    expect((await crmRequest(db, account, "get", { id: person.id }, "unused") as any).record)
      .toMatchObject({ title: null, website: null, company_id: null });
  });

  it("refreshes only complete profiles older than 30 days and retains review reasons until explicit correction", async () => {
    const account = owner();
    for (const id of ["stale", "recent", "review"]) await create(account, { kind: "person", name: `Synthetic ${id}` }, id);
    await call(account, "save", complete("stale"));
    await call(account, "save", complete("recent"));
    const rationale = "Identity is ambiguous: two unrelated people share this name; no verified affiliation found.";
    await call(account, "save", { record_id: "review", summary: rationale, sources: [], status: "needs_review" });
    const thirtyDays = 30 * 24 * 60 * 60 * 1000;
    await db.prepare("UPDATE crm_research SET checked_at = ? WHERE owner_id = ? AND record_id IN ('stale','review')").bind(Date.now() - thirtyDays - 60_000, account).run();
    await db.prepare("UPDATE crm_research SET checked_at = ? WHERE owner_id = ? AND record_id = 'recent'").bind(Date.now() - thirtyDays + 60_000, account).run();
    const queued = await call(account, "queue", {});
    expect(queued.records.map((r: any) => r.id)).toEqual(["stale"]);
    expect(queued.records[0].research).toMatchObject({ record_id: "stale", status: "complete", sources: complete("stale").sources });
    expect((await call(account, "get", { record_id: "review" })).research).toMatchObject({ summary: rationale, status: "needs_review", sources: [] });
    await call(account, "save", complete("stale"));
    await call(account, "save", complete("review"));
    expect((await call(account, "queue", {})).records).toEqual([]);
    expect((await call(account, "get", { record_id: "review" })).research.status).toBe("complete");
  });

  it("scopes every operation and cursor and enforces parent ownership in the migration", async () => {
    const alice = owner(), bob = owner();
    for (const id of ["shared", "alice-only", "alice-extra"]) await create(alice, { kind: "person", name: `Synthetic Alice ${id}` }, id);
    const page = await call(alice, "queue", { limit: 1 });
    await invalid(call(bob, "queue", { cursor: page.next_cursor }));
    expect((await call(bob, "queue", {})).records).toEqual([]);
    await missing(call(bob, "get", { record_id: "alice-only" }));
    await missing(call(bob, "save", complete("alice-only")));
    await expect(db.prepare("INSERT INTO crm_research (owner_id,record_id,summary,sources,status,checked_at) VALUES (?,?,?,?,?,?)").bind(bob, "alice-only", "Synthetic orphan", "[]", "needs_review", 1).run()).rejects.toThrow();
    await create(bob, { kind: "person", name: "Synthetic Bob" }, "shared");
    await call(alice, "save", complete("shared"));
    await call(bob, "save", { ...complete("shared"), summary: "Bob's separate research." });
    expect((await call(alice, "get", { record_id: "shared" })).research.summary).toBe(complete("shared").summary);
    expect((await call(bob, "get", { record_id: "shared" })).research.summary).toBe("Bob's separate research.");
    await crmRequest(db, alice, "delete", { id: "shared" }, "unused");
    expect(await db.prepare("SELECT record_id FROM crm_research WHERE owner_id = ? AND record_id = ?").bind(alice, "shared").all()).toMatchObject({ results: [] });
    expect((await call(bob, "get", { record_id: "shared" })).research.summary).toBe("Bob's separate research.");
  });

  it("rejects malformed provenance, unbounded input and nonexistent records without partial writes", async () => {
    const account = owner();
    await create(account, { kind: "person", name: "Synthetic unchanged" }, "unchanged");
    const saved = await call(account, "save", complete("unchanged"));
    const invalidSources: unknown[] = [null, {}, [], [{ kind: "other", reference: "synthetic" }], [{ kind: "web", reference: "javascript:alert(1)" }],
      [{ kind: "web", reference: "file:///secret" }], [{ kind: "web", reference: "https://user:pass@example.test" }],
      [{ kind: "web", reference: "not-a-url" }], [{ kind: "web", reference: "https://example.test", unknown: "extra" }],
      [{ kind: "email", reference: "https://example.test/message" }], [{ kind: "email", reference: "https://mail.google.com.evil.test/mail/#all/id" }],
      [{ kind: "email", reference: "not a message id" }], [{ kind: "email", reference: "https://mail.google.com/mail/u/0/" }],
      [{ kind: "calendar", reference: "https://example.test/event" }], [{ kind: "calendar", reference: "https://calendar.google.com/" }],
      [{ kind: "calendar", reference: "" }], [{ kind: "web", reference: "https://example.test", detail: "x".repeat(2001) }],
      [{ kind: "web", reference: "https://example.test", detail: null }], Array.from({ length: 51 }, () => ({ kind: "web", reference: "https://example.test" }))];
    for (const sources of invalidSources) await invalid(call(account, "save", { ...complete("unchanged"), sources }));
    const malformed: [CrmResearchOperation, unknown][] = [
      ["queue", null], ["queue", []], ["queue", { limit: 0 }], ["queue", { limit: 101 }], ["queue", { limit: 1.5 }], ["queue", { limit: "2" }],
      ["queue", { cursor: "not-a-cursor" }], ["queue", { cursor: "x".repeat(4097) }], ["get", { record_id: "a b" }], ["get", {}],
      ["save", { ...complete("unchanged"), summary: " " }], ["save", { ...complete("unchanged"), summary: "x".repeat(20001) }],
      ["save", { ...complete("unchanged"), summary: "bad\u0000summary" }], ["save", { ...complete("unchanged"), status: "pending" }],
      ["save", { ...complete("unchanged"), company: 3 }], ["save", { ...complete("unchanged"), title: "x".repeat(513) }],
      ["save", { ...complete("unchanged"), website: "javascript:alert(1)" }], ["save", { record_id: "unchanged", summary: "Missing status and sources" }],
    ];
    for (const [operation, input] of malformed) await invalid(call(account, operation, input));
    for (const [operation, input] of [["queue", {}], ["get", { record_id: "unchanged" }], ["save", complete("unchanged")]] as [CrmResearchOperation, Record<string, unknown>][]) {
      await invalid(call(account, operation, { ...input, owner_id: "forged" }));
      await invalid(call(account, operation, { ...input, sql: "DROP TABLE crm_records" }));
    }
    await invalid(call("", "queue", {}));
    await invalid(call(account, "unknown" as CrmResearchOperation, {}));
    await missing(call(account, "get", { record_id: "nonexistent" }));
    await missing(call(account, "save", complete("nonexistent")));
    expect(await call(account, "get", { record_id: "unchanged" })).toEqual(saved);
    await expect(db.prepare("UPDATE crm_research SET sources = '[]' WHERE owner_id = ? AND record_id = ?").bind(account, "unchanged").run()).rejects.toThrow();
    await expect(db.prepare("UPDATE crm_research SET sources = 'not-json' WHERE owner_id = ? AND record_id = ?").bind(account, "unchanged").run()).rejects.toThrow();
  });
});
