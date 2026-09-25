import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest, type CrmOperation } from "../src/crm";

// Failure modes at the persistence boundary: foreign-owner reads or mutations,
// forged links, lost fields in concurrent patches, partial deletion, duplicate
// replayed creates, LIKE wildcard expansion, cursor gaps/scope confusion, and
// coercion or passthrough of malformed/unknown input. These scenarios exercise
// the shipped migration and real workerd D1, with no database mock.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const owner = () => `account-${crypto.randomUUID()}`;
const call = (account: string, operation: CrmOperation, input: unknown, createId = crypto.randomUUID()): Promise<any> =>
  crmRequest(db, account, operation, input, createId);
const create = (account: string, input: Record<string, unknown>, id = crypto.randomUUID()) => call(account, "save", input, id);
const invalid = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "invalid_input" });
const missing = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "not_found" });

describe("private account CRM in D1", () => {
  it("persists a company/person/note journey and applies sparse edits and null clears", async () => {
    const account = owner();
    const { record: company } = await create(account, { kind: "company", name: "Juniper Works", website: "https://juniper.example" });
    const { record: person } = await create(account, { kind: "person", name: "Ada Reed", email: "ada@example.test", phone: "+1 202 555 0100", title: "Designer", company_id: company.id, tags: ["customer", "design"] });
    expect(person).toMatchObject({ kind: "person", company_id: company.id, tags: ["customer", "design"], website: null });
    expect(person).not.toHaveProperty("owner_id");
    expect(person.created_at).toEqual(expect.any(Number));
    const { note } = await call(account, "save_note", { record_id: person.id, body: "Met at the design conference", source_url: "https://events.example/meeting" });
    expect(note).not.toHaveProperty("owner_id");
    const updated = await call(account, "save", { id: person.id, name: "Ada Quinn", email: null });
    expect(updated.record).toMatchObject({ name: "Ada Quinn", email: null, phone: person.phone, title: "Designer", company_id: company.id, tags: person.tags, created_at: person.created_at });
    expect(updated.record.updated_at).toBeGreaterThanOrEqual(person.updated_at);
    const renamedNote = await call(account, "save_note", { id: note.id, body: "Discussed a workshop" });
    expect(renamedNote.note.source_url).toBe(note.source_url);
    await call(account, "save_note", { id: note.id, source_url: null });
    const detail = await call(account, "get", { id: person.id });
    expect(detail.record).toEqual(updated.record);
    expect(detail.notes).toEqual([expect.objectContaining({ id: note.id, body: "Discussed a workshop", source_url: null })]);
    expect(detail.next_cursor).toBeNull();
    await call(account, "save", { id: person.id, company_id: null, tags: [] });
    expect((await call(account, "get", { id: person.id })).record).toMatchObject({ company_id: null, tags: [] });
  });

  it("searches literal substrings in record fields, tags, notes and linked company names with intersecting filters", async () => {
    const account = owner();
    const { record: company } = await create(account, { kind: "company", name: "Juniper Works" });
    const { record: person } = await create(account, { kind: "person", name: "Ada Reed", email: "inbox@example.test", phone: "555-0102", website: "https://profile.example", title: "Cartographer", company_id: company.id, tags: ["design", "100%_ready\\set"] });
    await call(account, "save_note", { record_id: person.id, body: "Discussed spectrometer procurement", source_url: "https://source.example/meeting" });
    await create(account, { kind: "person", name: "Bryn Moss", tags: ["designers"] });
    for (const q of ["aDa", "inbox@", "555-0102", "profile.example", "cartograph", "design", "spectrometer", "source.example", "juniper", "%_", "\\set"]) {
      const result = await call(account, "search", { q, kind: "person", tag: "design", company_id: company.id });
      expect(result.records.map((r: any) => r.id), q).toEqual([person.id]);
    }
    expect((await call(account, "search", { q: "' OR 1=1 --" })).records).toEqual([]);
    expect((await call(account, "search", { q: "100xZready" })).records).toEqual([]);
    expect((await call(account, "search", { kind: "company" })).records.map((r: any) => r.id)).toEqual([company.id]);
  });

  it("paginates records and notes stably, including tied timestamps, and scopes cursors", async () => {
    const account = owner();
    const ids = ["record-c", "record-a", "record-b", "record-d", "record-e"];
    for (const id of ids) await create(account, { kind: "person", name: id }, id);
    // Real D1 fixture creates a timestamp tie that wall-clock timing cannot reliably produce.
    await db.prepare("UPDATE crm_records SET created_at = ? WHERE owner_id = ?").bind(1234, account).run();
    let cursor: string | null = null;
    const seen: string[] = [];
    let firstCursor = "";
    do {
      const page: any = await call(account, "search", { limit: 2, ...(cursor ? { cursor } : {}) });
      expect(page.records.length).toBeLessThanOrEqual(2);
      seen.push(...page.records.map((r: any) => r.id));
      cursor = page.next_cursor;
      if (!firstCursor && cursor) firstCursor = cursor;
    } while (cursor);
    expect(seen).toEqual([...ids].sort());
    await invalid(call(owner(), "search", { cursor: firstCursor }));
    await invalid(call(account, "search", { q: "changed", cursor: firstCursor }));
    for (const id of ["note-c", "note-a", "note-b"]) await call(account, "save_note", { record_id: ids[0], body: id }, id);
    await db.prepare("UPDATE crm_notes SET created_at = ? WHERE owner_id = ?").bind(2345, account).run();
    const first = await call(account, "get", { id: ids[0], notes_limit: 2 });
    expect(first.notes.map((n: any) => n.id)).toEqual(["note-a", "note-b"]);
    const last = await call(account, "get", { id: ids[0], notes_limit: 2, notes_cursor: first.next_cursor });
    expect(last.notes.map((n: any) => n.id)).toEqual(["note-c"]);
    expect(last.next_cursor).toBeNull();
    await invalid(call(account, "get", { id: ids[1], notes_cursor: first.next_cursor }));
    await invalid(call(account, "get", { id: ids[0], notes_cursor: firstCursor }));
  });

  it("bounds default and maximum pages for records and notes", async () => {
    const account = owner();
    for (let i = 0; i < 21; i++) await create(account, { kind: "person", name: `Person ${i}` }, `person-${i}`);
    const search = await call(account, "search", {});
    expect(search.records).toHaveLength(20);
    expect(search.next_cursor).toEqual(expect.any(String));
    for (let i = 0; i < 21; i++) await call(account, "save_note", { record_id: "person-0", body: `Note ${i}` }, `note-${i}`);
    const detail = await call(account, "get", { id: "person-0" });
    expect(detail.notes).toHaveLength(20);
    expect(detail.next_cursor).toEqual(expect.any(String));
    expect((await call(account, "search", { limit: 100 })).records).toHaveLength(21);
    expect((await call(account, "get", { id: "person-0", notes_limit: 100 })).notes).toHaveLength(21);
  });

  it("isolates every operation and blocks cross-owner or wrong-kind links in both API and schema", async () => {
    const alice = owner(), bob = owner();
    const { record: company } = await create(alice, { kind: "company", name: "Private Research" });
    const { record: person } = await create(alice, { kind: "person", name: "Private Person", company_id: company.id });
    const { note } = await call(alice, "save_note", { record_id: person.id, body: "Confidential brief" });
    expect((await call(bob, "search", {})).records).toEqual([]);
    expect((await call(bob, "search", { q: "confidential", company_id: company.id })).records).toEqual([]);
    await missing(call(bob, "get", { id: person.id }));
    await missing(call(bob, "save", { id: person.id, name: "Stolen" }));
    await missing(call(bob, "delete", { id: company.id }));
    await missing(call(bob, "save_note", { id: note.id, body: "Stolen" }));
    await missing(call(bob, "delete_note", { id: note.id }));
    await invalid(create(bob, { kind: "person", name: "Bad link", company_id: company.id }));
    const { record: bobPerson } = await create(bob, { kind: "person", name: "Bob's contact" });
    await invalid(call(bob, "save", { id: bobPerson.id, company_id: company.id }));
    await invalid(call(bob, "save_note", { record_id: person.id, body: "Foreign note" }));
    await invalid(create(alice, { kind: "person", name: "Wrong kind", company_id: person.id }));
    await invalid(create(alice, { kind: "company", name: "Company with parent", company_id: company.id }));
    await expect(db.prepare("UPDATE crm_records SET company_id = ? WHERE owner_id = ? AND id = ?").bind(company.id, bob, bobPerson.id).run()).rejects.toThrow();
    await expect(db.prepare("INSERT INTO crm_notes (owner_id,id,record_id,body,created_at,updated_at) VALUES (?,?,?,?,?,?)").bind(bob, "forged-note", person.id, "No", 1, 1).run()).rejects.toThrow();
    await expect(db.prepare("UPDATE crm_records SET company_id = ? WHERE owner_id = ? AND id = ?").bind(person.id, alice, person.id).run()).rejects.toThrow();
    // IDs can coincide across owners without collisions or inferred access.
    const { record: bobCopy } = await create(bob, { kind: "company", name: "Public Studio" }, company.id);
    expect(bobCopy.name).toBe("Public Studio");
    expect((await call(alice, "get", { id: person.id })).notes).toEqual([note]);
    expect((await call(alice, "get", { id: company.id })).record.name).toBe("Private Research");
    await call(bob, "delete", { id: company.id });
    expect((await call(alice, "get", { id: person.id })).record.company_id).toBe(company.id);
  });

  it("makes creates replay-safe and keeps explicit update IDs from becoming creates", async () => {
    const account = owner();
    const results = await Promise.all([
      create(account, { kind: "person", name: "Original" }, "replayed-record"),
      create(account, { kind: "person", name: "Original" }, "replayed-record"),
    ]);
    expect(results[0]).toEqual(results[1]);
    expect(await create(account, { kind: "company", name: "Replay must not patch" }, "replayed-record")).toEqual(results[0]);
    const originalNote = await call(account, "save_note", { record_id: "replayed-record", body: "Original note" }, "replayed-note");
    expect(await call(account, "save_note", { record_id: "replayed-record", body: "Ignored replay" }, "replayed-note")).toEqual(originalNote);
    expect((await call(account, "search", {})).records).toHaveLength(1);
    expect((await call(account, "get", { id: "replayed-record" })).notes).toHaveLength(1);
    await missing(call(account, "save", { id: "absent", kind: "person", name: "No upsert" }));
    await missing(call(account, "save_note", { id: "absent", record_id: "replayed-record", body: "No upsert" }));
    await invalid(call(account, "save", { id: "replayed-record", kind: "company" }));
  });

  it("preserves unrelated fields in concurrent record and note patches", async () => {
    const account = owner();
    const { record } = await create(account, { kind: "person", name: "Ada", phone: "original", tags: ["kept"] });
    await Promise.all([
      call(account, "save", { id: record.id, email: "new@example.test" }),
      call(account, "save", { id: record.id, title: "Principal" }),
    ]);
    expect((await call(account, "get", { id: record.id })).record).toMatchObject({ phone: "original", email: "new@example.test", title: "Principal", tags: ["kept"] });
    const { note } = await call(account, "save_note", { record_id: record.id, body: "Original" });
    await Promise.all([
      call(account, "save_note", { id: note.id, body: "Edited" }),
      call(account, "save_note", { id: note.id, source_url: "https://source.example" }),
    ]);
    expect((await call(account, "get", { id: record.id })).notes[0]).toMatchObject({ body: "Edited", source_url: "https://source.example" });
    const { record: other } = await create(account, { kind: "person", name: "Other" });
    await invalid(call(account, "save_note", { id: note.id, record_id: other.id }));
  });

  it("deletes notes, cascades record notes and unlinks people on company deletion atomically", async () => {
    const account = owner();
    const { record: company } = await create(account, { kind: "company", name: "Closing Company" });
    const { record: person } = await create(account, { kind: "person", name: "Ada", company_id: company.id });
    const { note } = await call(account, "save_note", { record_id: company.id, body: "Company memo" });
    const { note: personNote } = await call(account, "save_note", { record_id: person.id, body: "Person memo" });
    expect(await call(account, "delete_note", { id: personNote.id })).toEqual({ id: personNote.id, deleted: true });
    await missing(call(account, "delete_note", { id: personNote.id }));
    expect(await call(account, "delete", { id: company.id })).toEqual({ id: company.id, deleted: true });
    expect((await call(account, "get", { id: person.id })).record.company_id).toBeNull();
    await missing(call(account, "get", { id: company.id }));
    await missing(call(account, "save_note", { id: note.id, body: "Orphan" }));
    expect(await db.prepare("SELECT id FROM crm_notes WHERE owner_id = ? AND record_id = ?").bind(account, company.id).all()).toMatchObject({ results: [] });
    await call(account, "delete", { id: person.id });
    expect((await call(account, "search", {})).records).toEqual([]);
  });

  it("keeps link/delete races and failed mutations free of partial changes", async () => {
    const account = owner();
    const { record: company } = await create(account, { kind: "company", name: "Company" });
    const { record: person } = await create(account, { kind: "person", name: "Preserved", title: "Kept" });
    await invalid(call(account, "save", { id: person.id, name: "Must roll back", company_id: "missing-company" }));
    expect((await call(account, "get", { id: person.id })).record.name).toBe("Preserved");
    const outcomes = await Promise.allSettled([
      call(account, "save", { id: person.id, company_id: company.id }),
      call(account, "delete", { id: company.id }),
      call(account, "save_note", { record_id: company.id, body: "Concurrent" }),
    ]);
    expect(outcomes[1].status).toBe("fulfilled");
    expect((await call(account, "get", { id: person.id })).record).toMatchObject({ company_id: null, title: "Kept" });
    expect(await db.prepare("SELECT id FROM crm_notes WHERE owner_id = ? AND record_id = ?").bind(account, company.id).all()).toMatchObject({ results: [] });
  });

  it("rejects malformed input, unknown fields, forged tenancy and invalid cursors before writing", async () => {
    const account = owner();
    const { record } = await create(account, { kind: "person", name: "Unchanged" });
    const { note } = await call(account, "save_note", { record_id: record.id, body: "Unchanged" });
    const malformed: [CrmOperation, unknown][] = [
      ["save", null], ["save", []], ["save", { kind: "person" }], ["save", { name: "Missing kind" }],
      ["save", { kind: "other", name: "Bad" }], ["save", { kind: "person", name: "  " }],
      ["save", { kind: "person", name: "x".repeat(513) }], ["save", { id: "" }], ["save", { id: "a b", name: "Bad" }],
      ["save", { id: record.id, email: 123 }], ["save", { id: record.id, tags: "tag" }],
      ["save", { id: record.id, tags: ["ok", 3] }], ["save", { id: record.id, tags: null }],
      ["save", { id: record.id, website: "javascript:alert(1)" }], ["save", { id: record.id, name: null }],
      ["save", { id: record.id, name: "Bad\u0000value" }],
      ["search", { limit: 0 }], ["search", { limit: 101 }], ["search", { limit: 1.5 }], ["search", { limit: "2" }],
      ["search", { q: 2 }], ["search", { kind: "other" }], ["search", { cursor: "not-a-cursor" }],
      ["search", { cursor: "a".repeat(4097) }],
      ["get", { id: record.id, notes_limit: 101 }], ["get", { id: record.id, notes_cursor: "e30" }],
      ["get", { id: record.id, notes_limit: null }],
      ["save_note", { record_id: record.id, body: "" }], ["save_note", { body: "No record" }],
      ["save_note", { record_id: record.id, body: "x".repeat(20001) }],
      ["save_note", { id: note.id, source_url: "file:///private" }],
      ["delete", { id: null }], ["delete_note", { id: 2 }],
    ];
    for (const [operation, input] of malformed) await invalid(call(account, operation, input));
    for (const [operation, input] of [
      ["search", {}], ["get", { id: record.id }], ["save", { id: record.id, name: "Bad" }],
      ["delete", { id: record.id }], ["save_note", { id: note.id, body: "Bad" }], ["delete_note", { id: note.id }],
    ] as [CrmOperation, Record<string, unknown>][]) {
      await invalid(call(account, operation, { ...input, owner_id: "forged" }));
      await invalid(call(account, operation, { ...input, sql: "DROP TABLE crm_records" }));
    }
    await invalid(call(account, "unknown" as CrmOperation, {}));
    await invalid(call("", "search", {}));
    await invalid(create(account, { kind: "person", name: "Bad host ID" }, ""));
    expect((await call(account, "get", { id: record.id })).record.name).toBe("Unchanged");
    expect((await call(account, "get", { id: record.id })).notes[0].body).toBe("Unchanged");
  });
});
