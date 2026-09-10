import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { MemoryScope } from "../src/memory-scope";
import { initializeMemoryContent, memoryIdentityDigest, readMemoryContent } from "../src/durable-memory-storage";
import { parseMemoryResult } from "../src/durable-memory";

const binding = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
const headers = {
  "x-nanocodex-organization-id": "organization", "x-nanocodex-team-id": "team",
  "x-nanocodex-subject-id": "subject", "x-nanocodex-memory-mutation": "1",
};
async function operate(memory: MemoryScope, body: unknown): Promise<Response> {
  return memory.fetch(new Request("https://memory.internal/memory", { method: "POST", headers, body: JSON.stringify(body) }));
}
async function scan(memory: MemoryScope, query: string) {
  const response = await operate(memory, { operation: "scan", query });
  expect(response.status).toBe(200);
  return response.json() as Promise<{ candidates: { key: { id: number; version: number } }[] }>;
}

describe("durable memory content storage", () => {
  it("stores beyond both old scope quotas and the old per-record cap, with exact read/replace/delete", async () => {
    await runInDurableObject(binding.getByName(crypto.randomUUID()), async (memory, state) => {
      await memory.fetch(new Request("https://memory.internal/initialize", { method: "PUT", headers }));
      state.storage.transactionSync(() => {
        for (let index = 0; index < 600; index++) {
          const content = `Record ${index}: ${"steady local ballast ".repeat(30)}`;
          state.storage.sql.exec(`INSERT INTO durable_memories
            (version, owner_team_id, content_json, identity_digest, created_at_ms, updated_at_ms)
            VALUES (1, 'team', ?, ?, 1, 1)`, JSON.stringify(content), memoryIdentityDigest(content));
        }
      });
      await scan(memory, "copper lighthouse");
      const content = "copper lighthouse ".repeat(130_000) + "😀 conclusion tail";
      const inserted = await operate(memory, { operation: "put", content });
      expect(inserted.status).toBe(200);
      const result = await inserted.json() as { memory: { key: { id: number; version: number }; content: string } };
      expect(result.memory.content).toBe(content);
      expect(() => parseMemoryResult({ operation: "put", ...result }, "put")).not.toThrow();
      const key = result.memory.key;
      expect(state.storage.sql.exec<{ content_json: string; identity_digest: string }>(
        "SELECT content_json, identity_digest FROM durable_memories WHERE id = ?", key.id,
      ).one()).toEqual({ content_json: expect.stringMatching(/^chunks:/), identity_digest: expect.stringMatching(/^[0-9a-f]{64}$/) });
      const found = await scan(memory, "copper lighthouse");
      expect(found.candidates[0]?.key).toEqual(key);
      const read = await operate(memory, { operation: "read", keys: [key] });
      expect(await read.json()).toMatchObject({ memories: [{ content, key }] });
      // The same logical conclusion remains duplicate despite case/whitespace.
      await scan(memory, "copper lighthouse");
      expect((await operate(memory, { operation: "put", content: `  ${content.toUpperCase()}  ` })).status).toBe(409);
      const listed = await memory.fetch(new Request("https://memory.internal/memories", { headers }));
      expect(listed.status).toBe(200);
      const listing = await listed.json() as { memories: { key: { id: number }; content: string }[] };
      expect(listing.memories).toHaveLength(601);
      expect(listing.memories.at(-1)?.content).toBe(content);
      expect(new Set(listing.memories.map((row) => row.key.id)).size).toBe(601);
      await scan(memory, "copper lighthouse");
      const replaced = await operate(memory, { operation: "put", replace: key, content: "small replacement" });
      expect(replaced.status).toBe(200);
      expect(await replaced.json()).toMatchObject({ replaced: true, memory: { key: { id: key.id, version: 2 }, content: "small replacement" } });
      expect(state.storage.sql.exec<{ n: number }>(
        "SELECT COUNT(*) AS n FROM durable_memory_content_chunks WHERE turn_id = ?", String(key.id),
      ).one().n).toBe(0);
      await scan(memory, "replacement");
      expect((await operate(memory, { operation: "put", replace: { id: key.id, version: 2 }, content })).status).toBe(200);
      expect((await operate(memory, { operation: "delete", key: { id: key.id, version: 3 } })).status).toBe(200);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM durable_memory_content_chunks").one().n).toBe(0);
    });
  }, 30_000);

  it("migrates old content and normalized identity without confusing literal chunk markers", async () => {
    await runInDurableObject(binding.getByName(crypto.randomUUID()), async (_memory, state) => {
      state.storage.sql.exec(`ALTER TABLE durable_memories RENAME COLUMN content_json TO content;
        ALTER TABLE durable_memories RENAME COLUMN identity_digest TO identity;`);
      state.storage.sql.exec(`INSERT INTO durable_memories
        (version, owner_team_id, content, identity, created_at_ms, updated_at_ms, scan_count, use_count)
        VALUES (7, 'team', 'chunks:3', 'chunks:3', 1, 2, 4, 5)`);
      const digestLiteral = memoryIdentityDigest("chunks:3");
      state.storage.sql.exec(`INSERT INTO durable_memories
        (version, owner_team_id, content, identity, created_at_ms, updated_at_ms)
        VALUES (1, 'team', ?, ?, 3, 3)`, digestLiteral, digestLiteral);
      state.storage.sql.exec("UPDATE sqlite_sequence SET seq = 100 WHERE name = 'durable_memories'");
      initializeMemoryContent(state.storage);
      const row = state.storage.sql.exec<{ id: number; content_json: string; identity_digest: string; version: number; use_count: number }>(
        "SELECT * FROM durable_memories WHERE id = 1",
      ).one();
      expect(readMemoryContent(state.storage, row)).toBe("chunks:3");
      expect(row).toMatchObject({ version: 7, use_count: 5, identity_digest: memoryIdentityDigest("CHUNKS:3") });
      initializeMemoryContent(state.storage);
      expect(readMemoryContent(state.storage, row)).toBe("chunks:3");
      expect(state.storage.sql.exec<{ seq: number }>("SELECT seq FROM sqlite_sequence WHERE name = 'durable_memories'").one().seq).toBe(100);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM durable_memories").one().n).toBe(2);
    });
  });
});
