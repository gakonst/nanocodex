import { Buffer } from "node:buffer";
import { applyD1Migrations, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { crmRequest } from "../src/crm";
import { crmIdentityRequest, type CrmIdentityOperation } from "../src/crm-identities";

// Behavioral scenarios and failure modes defined before implementation: canonical
// retries must retain manual provenance; distinct emails and ambiguous aliases
// must not merge people; provider lookalikes and partial usernames must reject;
// account/parent cursor replay, orphan rows, immutable IDs, and malformed bounded
// input must fail. Exercise the shipped migrations against real D1, not mocks.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
const owner = () => `identity-account-${crypto.randomUUID()}`;
const call = (account: string, operation: CrmIdentityOperation, input: unknown, id = crypto.randomUUID()): Promise<any> => crmIdentityRequest(db, account, operation, input, id);
const create = (account: string, id: string) => crmRequest(db, account, "save", { kind: "person", name: "Synthetic Same Name" }, id);
const save = (record_id: string, kind = "email", value = "guest@example.test") => ({ record_id, kind, value, origin: "user" });
const invalid = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "invalid_input" });
const missing = (promise: Promise<unknown>) => expect(promise).rejects.toMatchObject({ code: "not_found" });

describe("multiple CRM identities in real D1", () => {
  it("keeps two emails and ambiguous aliases on separate people and preserves manual provenance on retries", async () => {
    const account = owner();
    await create(account, "one"); await create(account, "two");
    const first = await call(account, "save", save("one", "email", " Guest+work@Example.Test "), "manual");
    expect(first.identity).toMatchObject({ id: "manual", record_id: "one", kind: "email", normalized: "guest+work@example.test", origin: "user", source_ref: null });
    expect(first.identity).not.toHaveProperty("owner_id");
    expect(await call(account, "save", { ...save("one", "email", "guest+work@example.test"), origin: "source", source_ref: "synthetic-message-1" })).toEqual(first);
    await call(account, "save", save("one", "email", "guest@example.test"));
    await call(account, "save", save("two", "email", "guest+work@example.test"));
    await call(account, "save", save("one", "aka", " Synthetic   Guest "));
    await call(account, "save", save("two", "aka", "synthetic guest"));
    expect((await call(account, "list", { record_id: "one" })).identities).toHaveLength(3);
    expect((await call(account, "list", { record_id: "two" })).identities).toHaveLength(2);
    expect((await db.prepare("SELECT id FROM crm_records WHERE owner_id=?").bind(account).all()).results).toHaveLength(2);
  });

  it("canonicalizes recognized providers, exact handles, domain and case-sensitive website paths", async () => {
    const account = owner(); await create(account, "one");
    const cases = [
      ["github", "https://WWW.GitHub.com/Synthetic-Guest/?utm_source=test#bio", "@synthetic-guest", "synthetic-guest"],
      ["x", "https://twitter.com/Synthetic_Guest/?s=20", "@synthetic_guest", "synthetic_guest"],
      ["telegram", "https://t.me/Synthetic_Guest/", "@synthetic_guest", "synthetic_guest"],
      ["linkedin", "https://www.linkedin.com/in/Synthetic-Guest/?trk=test", "https://linkedin.com/in/synthetic-guest", "https://linkedin.com/in/synthetic-guest"],
      ["website", "https://www.Example.test/CasePath/?utm_source=test&ref=keep#about", "https://example.test/CasePath/?ref=keep", "https://example.test/CasePath/?ref=keep"],
      ["domain", "WWW.Example.TEST", "example.test", "example.test"],
    ];
    for (const [kind, value, again, normalized] of cases) {
      const first = await call(account, "save", save("one", kind, value));
      expect(first.identity.normalized).toBe(normalized);
      expect(await call(account, "save", save("one", kind, again))).toEqual(first);
    }
    const sourced = await call(account, "save", { ...save("one", "linkedin", "https://linkedin.com/company/synthetic-works/"), origin: "source", source_ref: "https://example.test/team" });
    expect(sourced.identity).toMatchObject({ normalized: "https://linkedin.com/company/synthetic-works", origin: "source", source_ref: "https://example.test/team" });
    await call(account, "save", save("one", "website", "https://example.test/casepath/?ref=keep"));
    expect((await call(account, "list", { record_id: "one" })).identities).toHaveLength(8);
  });

  it("paginates with bounded scoped cursors and rejects replay and forged anchors", async () => {
    const alice = owner(), bob = owner();
    for (const account of [alice, bob]) { await create(account, "one"); await create(account, "two"); }
    for (let i = 0; i < 22; i++) await call(alice, "save", save("one", "email", `guest${i}@example.test`), `alias-${String(i).padStart(2, "0")}`);
    await db.prepare("UPDATE crm_identities SET created_at=100 WHERE owner_id=?").bind(alice).run();
    const first = await call(alice, "list", { record_id: "one" });
    expect(first.identities).toHaveLength(20);
    const last = await call(alice, "list", { record_id: "one", cursor: first.next_cursor, limit: 100 });
    expect(last.identities.map((row: any) => row.id)).toEqual(["alias-20", "alias-21"]);
    expect(last.next_cursor).toBeNull();
    await invalid(call(bob, "list", { record_id: "one", cursor: first.next_cursor }));
    await invalid(call(alice, "list", { record_id: "two", cursor: first.next_cursor }));
    const forged = JSON.parse(Buffer.from(first.next_cursor, "base64url").toString());
    forged.id = "absent";
    await invalid(call(alice, "list", { record_id: "one", cursor: Buffer.from(JSON.stringify(forged)).toString("base64url") }));
  });

  it("isolates all operations, enforces same-owner parents and cascades only their identities", async () => {
    const alice = owner(), bob = owner();
    await create(alice, "one");
    await call(alice, "save", save("one"), "shared-alias");
    await missing(call(bob, "list", { record_id: "one" }));
    await missing(call(bob, "save", save("one")));
    await missing(call(bob, "delete", { id: "shared-alias" }));
    await expect(db.prepare("INSERT INTO crm_identities(owner_id,id,record_id,kind,value,normalized,origin,created_at,updated_at) VALUES (?,?,?,'email','a@example.test','a@example.test','user',1,1)").bind(bob, "orphan", "one").run()).rejects.toThrow();
    await create(bob, "one"); await call(bob, "save", save("one"), "shared-alias");
    await create(alice, "two");
    await expect(db.prepare("UPDATE crm_identities SET record_id='two' WHERE owner_id=?").bind(alice).run()).rejects.toThrow();
    await expect(db.prepare("UPDATE crm_identities SET id='replacement' WHERE owner_id=?").bind(alice).run()).rejects.toThrow();
    await crmRequest(db, alice, "delete", { id: "one" }, "unused");
    expect((await db.prepare("SELECT id FROM crm_identities WHERE owner_id=?").bind(alice).all()).results).toEqual([]);
    expect((await call(bob, "list", { record_id: "one" })).identities).toHaveLength(1);
    expect(await call(bob, "delete", { id: "shared-alias" })).toEqual({ deleted: true });
    await missing(call(bob, "delete", { id: "shared-alias" }));
  });

  it("rejects invalid identity and provenance inputs without writing", async () => {
    const account = owner(); await create(account, "one");
    const malformed = [
      save("one", "email", "two@@example.test"), save("one", "email", "guest name@example.test"),
      save("one", "github", "https://github.com.evil.test/guest"), save("one", "github", "https://github.com/guest/repo"),
      save("one", "x", "https://x.com/guest/status/123"), save("one", "telegram", "https://t.me/+invite"),
      save("one", "linkedin", "https://linkedin.com/feed"), save("one", "x", "guest*"),
      save("one", "website", "https://user:pass@example.test"), save("one", "website", "javascript:alert(1)"),
      save("one", "domain", "https://example.test"), save("one", "domain", "example.test/path"),
      save("one", "aka", " "), save("one", "aka", "a\u0000b"), save("one", "unknown", "guest"),
      { ...save("one"), value: "x".repeat(2049) }, { ...save("one"), origin: "source" },
      { ...save("one"), origin: "source", source_ref: "https://user:pass@example.test" },
      { ...save("one"), source_ref: "javascript:alert(1)" }, { ...save("one"), source_ref: "x".repeat(2049) },
      { ...save("one"), origin: "manual" }, { ...save("one"), id: "edit" },
      { ...save("one"), owner_id: "forged" }, { ...save("one"), value: null },
    ];
    for (const input of malformed) await invalid(call(account, "save", input));
    for (const input of [null, [], {}, { record_id: "one", limit: 0 }, { record_id: "one", limit: 101 }, { record_id: "one", limit: "1" }, { record_id: "one", cursor: "x".repeat(4097) }, { record_id: "one", cursor: "bm90LWpzb24" }, { record_id: "one", extra: true }]) await invalid(call(account, "list", input));
    await invalid(call(account, "delete", { id: "bad id" }));
    await invalid(call(account, "delete", { id: "valid", record_id: "one" }));
    await invalid(call("", "list", { record_id: "one" }));
    await invalid(call(account, "unknown" as CrmIdentityOperation, {}));
    await missing(call(account, "save", save("missing")));
    expect((await call(account, "list", { record_id: "one" })).identities).toEqual([]);
  });
});
