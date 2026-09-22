import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { MemoryScope } from "../src/memory-scope";

const bindings = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> };
const headers = {
  "x-nanocodex-organization-id": "organization-a", "x-nanocodex-team-id": "team-a",
  "x-nanocodex-subject-id": "user:alice", "x-nanocodex-memory-mutation": "1",
  "x-nanocodex-memory-initialize": "1", "content-type": "application/json",
};
it("returns 404 for all retired memory routes", async () => {
  const stub = bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID());
  for (const [method, path] of [
    ["GET", "/memories"], ["POST", "/memory"], ["POST", "/personalization"],
    ...["list", "read", "search", "add_ad_hoc_note", "files", "file"].map(op => ["POST", `/extension-memories/${op}`]),
  ]) {
    expect((await stub.fetch(`https://memory.internal${path}`, {
      method, headers, ...(method === "POST" ? { body: "{}" } : {}),
    })).status).toBe(404);
  }
});
it("initializes canonical memory without allowing another organization to reclaim the scope", async () => {
  const stub = bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID());
  const get = (organization: string) => stub.fetch("https://memory.internal/markdown-memory/get", {
    method: "POST", headers: { ...headers, "x-nanocodex-organization-id": organization }, body: JSON.stringify({ path: "MEMORY.md" }),
  });
  expect((await get("organization-a")).status).toBe(200);
  expect((await get("organization-b")).status).toBe(404);
  expect((await get("organization-a")).status).toBe(200);
});
it("drops retired storage idempotently on initialization while preserving canonical memory and history", async () => {
  const stub = bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID());
  const call = (path: string, body: unknown) => stub.fetch(`https://memory.internal${path}`, {
    method: "POST", headers, body: JSON.stringify(body),
  });
  expect((await call("/markdown-memory/write", { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "canonical jade" })).status).toBe(200);
  const thread = "01900000-0000-7000-8000-000000000001";
  expect((await call("/project", { thread_id: thread, turn_id: "turn-a", cursor: "1", title: "Retained conversation", input: "history jade", final_message: "history answer", created_at: 1 })).status).toBe(204);
  await runInDurableObject(stub, async (_object, ctx) => {
    const tables = ["durable_memories", "durable_memories_legacy", "durable_memory_content_chunks", "memory_scan_receipts", "extension_memory_files", "extension_memory_file_chunks", "prepared_personalization", "personalization_subscribers"];
    for (const table of tables) ctx.storage.sql.exec(`CREATE TABLE ${table} (content TEXT); INSERT INTO ${table} VALUES ('retired canary')`);
    ctx.storage.sql.exec(`CREATE TRIGGER personalization_memory_delete AFTER DELETE ON durable_memories BEGIN INSERT INTO prepared_personalization VALUES ('dirty'); END;
      CREATE TRIGGER durable_memories_ad AFTER DELETE ON durable_memories BEGIN DELETE FROM durable_memory_content_chunks; END;`);
    for (let restart = 0; restart < 2; restart++) {
      new MemoryScope(ctx, {});
      const names = ctx.storage.sql.exec<{ name: string }>("SELECT name FROM sqlite_master").toArray().map(row => row.name);
      for (const table of tables) expect(names).not.toContain(table);
      expect(names).not.toContain("personalization_memory_delete");
      expect(names).not.toContain("durable_memories_ad");
      expect(ctx.storage.sql.exec("SELECT * FROM memory_threads").toArray()).toHaveLength(1);
      expect(ctx.storage.sql.exec("SELECT * FROM markdown_memory_documents").toArray()).toHaveLength(1);
    }
  });
  expect(await (await call("/markdown-memory/get", { path: "MEMORY.md" })).json()).toMatchObject({ content: "canonical jade", revision: 1 });
  expect(await (await call("/read", { session_id: thread })).json()).toMatchObject({ turns: [{ assistant: "history answer" }] });
  expect(await (await call("/markdown-memory/search", { query: "canonical jade" })).text()).toContain("canonical jade");
});
