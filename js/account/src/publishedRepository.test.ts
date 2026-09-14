import assert from "node:assert/strict";
import test from "node:test";
import { appQueryClient } from "./queryClient.ts";
import { loadPublishedRepositorySnapshot, publishedSnapshotQueryOptions } from "./publishedRepository.ts";

test("repository caches respect requested generations, refresh stale metadata, and share immutable files", async (t) => {
  t.after(() => appQueryClient.clear());
  const first = "a".repeat(40);
  const second = "b".repeat(40);
  const file = { path: "README.md", objectId: "c".repeat(40), mode: "100644", size: 4, contentUrl: "/api/repository/file/c" };
  let metadataReads = 0;
  let fileReads = 0;
  t.mock.method(globalThis, "fetch", async (input: string) => {
    const url = new URL(input, "https://example.test");
    if (url.pathname === file.contentUrl) {
      fileReads++;
      return new Response("text");
    }
    metadataReads++;
    const head = url.searchParams.get("generation")!;
    return Response.json({
      repository: { head, branch: "main", indexedCommits: 1, commitPageSize: 32 },
      generatedAt: "2026-09-07T00:00:00Z", tree: [file],
    }, { headers: { "x-repository-generation": head } });
  });
  const [a, duplicate] = await Promise.all([
    loadPublishedRepositorySnapshot(fetch, false, first),
    loadPublishedRepositorySnapshot(fetch, false, first),
  ]);
  assert.equal(a, duplicate);
  const b = await loadPublishedRepositorySnapshot(fetch, false, second);
  assert.equal(b.repository.head, second);
  assert.equal(metadataReads, 2);
  assert.deepEqual(await Promise.all([a.readFile(file), b.readFile(file)]), ["text", "text"]);
  assert.equal(fileReads, 1);
  const options = publishedSnapshotQueryOptions(first);
  appQueryClient.setQueryData(options.queryKey, a, { updatedAt: Date.now() - 5 * 60_000 - 1 });
  await loadPublishedRepositorySnapshot(fetch, false, first);
  assert.equal(metadataReads, 3);
  await b.readFile(file);
  assert.equal(fileReads, 1);
});
