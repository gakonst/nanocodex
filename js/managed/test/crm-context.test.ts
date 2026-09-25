import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest } from "../src/crm";
import { crmFactRequest, crmRelationshipRequest, type CrmContextOperation } from "../src/crm-context";

// Scenarios defined before implementation: research must coexist with human
// facts, stable create retries cannot replace content, explicit edits preserve
// identity/provenance, scoped keysets survive edits and reject query/account
// reuse, invalid/oversized JSON and provenance never write, relationships prove
// endpoint ownership/kinds, reverse rosters work, and parent deletion cascades
// only within the authenticated owner. All exercise actual shipped D1 migrations.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const owner = () => `context-${crypto.randomUUID()}`;
const fact = (account: string, op: CrmContextOperation, input: unknown, id = crypto.randomUUID()): Promise<any> => crmFactRequest(db, account, op, input, id);
const rel = (account: string, op: CrmContextOperation, input: unknown, id = crypto.randomUUID()): Promise<any> => crmRelationshipRequest(db, account, op, input, id);
const record = (account: string, id: string, kind = "person") => crmRequest(db, account, "save", { kind, name: `Synthetic ${id}` }, id);
const invalid = (result: Promise<unknown>) => expect(result).rejects.toMatchObject({ code: "invalid_input" });
const missing = (result: Promise<unknown>) => expect(result).rejects.toMatchObject({ code: "not_found" });
const source = [{ kind: "web", reference: "https://example.test/profile" }];
const human = { record_id: "person", predicate: "bio.expertise", value: ["distributed systems", "日本語"], origin: "user" };
const inferred = { ...human, value: "databases", origin: "inferred", sources: source, confidence: "medium", rationale: "The cited profile describes database research." };
const employment = { from_id: "person", to_id: "company", type: "works_at", role: "Engineer", origin: "user" };

describe("account-private structured CRM context in real D1", () => {
  it("keeps human facts distinct, retries creates safely, and edits only explicit immutable identities", async () => {
    const account = owner();
    await record(account, "person");
    await record(account, "other");
    const manual = await fact(account, "save", human, "human");
    expect(await fact(account, "save", { ...human, value: "retry cannot replace" }, "human")).toEqual(manual);
    const research = await fact(account, "save", inferred, "research");
    expect(research.fact).toMatchObject({ origin: "inferred", confidence: "medium", rationale: inferred.rationale, state: "current" });
    expect((await fact(account, "list", { record_id: "person", predicate: "bio.expertise" })).facts).toEqual([manual.fact, research.fact]);
    const edited = await fact(account, "save", { id: "research", state: "superseded", value: { specialty: "storage", years: 4 } });
    expect(edited.fact.created_at).toBe(research.fact.created_at);
    expect(edited.fact.value).toEqual({ specialty: "storage", years: 4 });
    for (const patch of [{ origin: "inferred", sources: source, confidence: "high", rationale: "Wrong identity" }, { record_id: "other" }]) {
      await invalid(fact(account, "save", { id: "human", ...patch }));
    }
    expect((await fact(account, "list", { record_id: "person" })).facts[0]).toEqual(manual.fact);
    await missing(fact(account, "save", { id: "absent", value: 1 }));
    expect(await fact(account, "delete", { id: "research" })).toEqual({ deleted: true });
    await missing(fact(account, "delete", { id: "research" }));
    await expect(db.prepare("UPDATE crm_facts SET origin = 'source', sources = ? WHERE owner_id = ? AND id = 'human'").bind(JSON.stringify(source), account).run()).rejects.toThrow();
  });

  it("isolates owners, scopes stable keysets, and cascades facts plus both relationship endpoints", async () => {
    const account = owner(), other = owner();
    for (const a of [account, other]) {
      await record(a, "person"); await record(a, "company", "company");
      for (const id of ["a", "b", "c"]) await fact(a, "save", human, id);
      await rel(a, "save", employment, "job");
    }
    await db.prepare("UPDATE crm_facts SET created_at = 1 WHERE owner_id = ?").bind(account).run();
    const first = await fact(account, "list", { record_id: "person", limit: 1 });
    expect(first.facts.map((f: any) => f.id)).toEqual(["a"]);
    await fact(account, "save", { id: "a", value: "edited" });
    const rest = await fact(account, "list", { record_id: "person", limit: 100, cursor: first.next_cursor });
    expect(rest.facts.map((f: any) => f.id)).toEqual(["b", "c"]);
    expect(rest.next_cursor).toBeNull();
    await invalid(fact(other, "list", { record_id: "person", cursor: first.next_cursor }));
    await invalid(fact(account, "list", { predicate: "bio.location", cursor: first.next_cursor }));
    await invalid(rel(account, "list", { cursor: first.next_cursor }));
    const isolated = owner();
    expect((await fact(isolated, "list", {})).facts).toEqual([]);
    await missing(fact(isolated, "save", { id: "a", value: "foreign" }));
    await missing(fact(isolated, "delete", { id: "a" }));
    await invalid(fact(isolated, "save", human));
    await invalid(rel(isolated, "save", employment));
    await missing(rel(isolated, "save", { id: "job", description: "foreign" }));
    await missing(rel(isolated, "delete", { id: "job" }));
    await crmRequest(db, account, "delete", { id: "company" }, "unused");
    expect((await rel(account, "list", {})).relationships).toEqual([]);
    await record(account, "peer");
    await rel(account, "save", { from_id: "person", to_id: "peer", type: "knows", origin: "user" });
    await crmRequest(db, account, "delete", { id: "person" }, "unused");
    expect((await fact(account, "list", {})).facts).toEqual([]);
    expect((await rel(account, "list", {})).relationships).toEqual([]);
    expect((await fact(other, "list", {})).facts).toHaveLength(3);
    expect((await rel(other, "list", {})).relationships).toHaveLength(1);
  });

  it("lists reverse company rosters and validates kinds, immutable links, provenance, and dates", async () => {
    const account = owner();
    await record(account, "person"); await record(account, "peer"); await record(account, "company", "company");
    const job = await rel(account, "save", { ...employment, effective_from: "2024-02-29", effective_to: "2026-09-25" }, "job");
    expect(await rel(account, "save", { ...employment, role: "Retry" }, "job")).toEqual(job);
    await rel(account, "save", { ...employment, from_id: "peer" }, "peer-job");
    const first = await rel(account, "list", { record_id: "company", type: "works_at", limit: 1 });
    expect(first.relationships[0].id).toBe("job");
    await rel(account, "save", { id: "job", role: "Lead", description: "Team lead" });
    expect((await rel(account, "list", { record_id: "company", type: "works_at", cursor: first.next_cursor })).relationships.map((r: any) => r.id)).toEqual(["peer-job"]);
    await invalid(rel(account, "list", { record_id: "person", type: "works_at", cursor: first.next_cursor }));
    for (const patch of [{ from_id: "peer" }, { to_id: "peer" }, { type: "worked_at" }, { origin: "source", sources: source }]) await invalid(rel(account, "save", { id: "job", ...patch }));
    for (const patch of [{ to_id: "person" }, { from_id: "company" }, { type: "knows" }, { type: "other" }, { effective_from: "2025-02-29" }, { effective_from: "2027-01-01", effective_to: "2026-01-01" }, { origin: "inferred", sources: source, confidence: "high" }]) await invalid(rel(account, "save", { ...employment, ...patch }));
    await invalid(rel(account, "save", { from_id: "person", to_id: "peer", type: "knows", role: "Friend", origin: "user" }));
    const link = await rel(account, "save", { from_id: "person", to_id: "peer", type: "referred", origin: "inferred", sources: [{ kind: "email", reference: "synthetic-message" }], confidence: "low", rationale: "The introduction suggests a referral." }, "referral");
    expect(link.relationship.origin).toBe("inferred");
    await invalid(rel(account, "save", { id: "referral", confidence: null }));
    await invalid(rel(account, "save", { id: "job", effective_to: "2023-01-01" }));
    expect(await rel(account, "delete", { id: "referral" })).toEqual({ deleted: true });
    await expect(db.prepare("UPDATE crm_relationships SET to_id = 'peer' WHERE owner_id = ? AND id = 'job'").bind(account).run()).rejects.toThrow();
  });

  it("rejects invalid provenance, malformed Unicode/JSON, byte overflow, dates, and unknown fields without mutation", async () => {
    const account = owner(); await record(account, "person");
    const malformed: unknown[] = [undefined, NaN, Infinity, 1n, () => 1, new Date(), { nested: undefined }, "\ud800", { "\udfff": 1 }, "😀".repeat(4096)];
    const cycle: any = {}; cycle.self = cycle; malformed.push(cycle);
    for (const value of malformed) await invalid(fact(account, "save", { ...human, value }));
    for (const patch of [
      { predicate: "location" }, { predicate: "bio..location" }, { predicate: "bio." + "x".repeat(128) },
      { origin: "source" }, { origin: "inferred", sources: source }, { ...inferred, rationale: " " },
      { sources: [{ kind: "web", reference: "https://user:secret@example.test" }] },
      { sources: [{ kind: "web", reference: "file:///secret" }] }, { sources: [{ kind: "other", reference: "x" }] },
      { sources: [{ kind: "document", reference: "doc", owner_id: "extra" }] },
      { sources: [{ kind: "user", reference: "\ud800" }] }, { confidence: "certain" },
      { effective_from: "2026-02-30" }, { effective_from: "2026-1-01" }, { state: "pending" }, { owner_id: "forged" },
    ]) await invalid(fact(account, "save", { ...human, ...patch }));
    for (const request of [fact, rel]) {
      for (const input of [null, [], { limit: 0 }, { limit: 101 }, { cursor: "x" }, { extra: true }]) await invalid(request(account, "list", input));
      await invalid(request(account, "delete", { id: "x", extra: true }));
      await invalid(request("", "list", {}));
    }
    expect((await fact(account, "list", {})).facts).toEqual([]);
    const accepted = await fact(account, "save", { ...human, value: "😀".repeat(4095), sources: [{ kind: "document", reference: "synthetic-document", detail: "Read profile" }] }, "unicode");
    expect(accepted.fact.value).toHaveLength(8190);
    await fact(account, "save", { ...human, value: null, origin: "source", sources: [{ kind: "calendar", reference: "synthetic-event" }] });
    await expect(db.prepare("UPDATE crm_facts SET value_json = ? WHERE owner_id = ? AND id = 'unicode'").bind(JSON.stringify("😀".repeat(4096)), account).run()).rejects.toThrow();
  });
});
