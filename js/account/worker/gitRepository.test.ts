import assert from "node:assert/strict";
import { test } from "node:test";

import {
  GitRepository,
  MAX_REPOSITORY_PART_BYTES,
  isCommitPatchManifest,
  isRepositoryPublication,
  type RepositoryPublication,
} from "./gitRepository.ts";

const firstHash = "a".repeat(40);
const secondHash = "b".repeat(40);
const packHash = "c".repeat(40);

test("publication validation pins every mutable view to one generation", () => {
  assert.equal(isRepositoryPublication(publication(firstHash)), true);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    commitPatchParts: undefined,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    commitPatchParts: [{
      key: `generations/${secondHash}/commit-patches/0000.diff`,
      size: 1,
    }],
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    commitPatchParts: [
      {
        key: `generations/${firstHash}/commit-patches/0000.diff`,
        size: 1,
      },
      {
        key: `generations/${firstHash}/commit-patches/0001.diff`,
        size: 1,
      },
    ],
    commitPatchSize: 2,
  }), true);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    commitPatchParts: [
      { key: `generations/${firstHash}/commit-patches/0000.diff`, size: 1 },
      { key: `generations/${firstHash}/commit-patches/0002.diff`, size: 1 },
    ],
    commitPatchSize: 2,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    commitPatchParts: [{
      key: `generations/${firstHash}/commit-patches/0000.diff`,
      size: MAX_REPOSITORY_PART_BYTES + 1,
    }],
    commitPatchSize: MAX_REPOSITORY_PART_BYTES + 1,
  }), false);
  const current = publication(firstHash);
  const { packParts: _packParts, packSize: _packSize, ...legacy } = current;
  assert.equal(isRepositoryPublication({
    ...legacy,
    packKey: `generations/${firstHash}/repository.pack`,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [{ key: `generations/${secondHash}/packs/${packHash}/0000.pack`, size: 1 }],
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [{ key: `generations/${firstHash}/packs/${secondHash}/0000.pack`, size: 1 }],
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [],
    packSize: 0,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [{
      key: `generations/${firstHash}/packs/${packHash}/0000.pack`,
      size: MAX_REPOSITORY_PART_BYTES + 1,
    }],
    packSize: MAX_REPOSITORY_PART_BYTES + 1,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [
      { key: `generations/${firstHash}/packs/${packHash}/0000.pack`, size: 1 },
      { key: `generations/${firstHash}/packs/${packHash}/0001.pack`, size: 1 },
    ],
    packSize: 2,
  }), false);
  const uploadPartBytes = 4 * 1024 * 1024;
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [
      {
        key: `generations/${firstHash}/packs/${packHash}/0000.pack`,
        size: uploadPartBytes,
      },
      {
        key: `generations/${firstHash}/packs/${packHash}/0001.pack`,
        size: uploadPartBytes,
      },
      { key: `generations/${firstHash}/packs/${packHash}/0002.pack`, size: 1 },
    ],
    packSize: (uploadPartBytes * 2) + 1,
  }), true);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    packParts: [
      {
        key: `generations/${firstHash}/packs/${packHash}/0000.pack`,
        size: uploadPartBytes,
      },
      { key: `generations/${firstHash}/packs/${packHash}/0001.pack`, size: 1 },
      { key: `generations/${firstHash}/packs/${packHash}/0002.pack`, size: 1 },
    ],
    packSize: uploadPartBytes + 2,
  }), false);
  assert.equal(isRepositoryPublication({
    ...publication(firstHash),
    refs: [{ name: "refs/heads/../escape", oid: firstHash }],
  }), false);
});

test("commit patch manifests are immutable generation maps", () => {
  const current = publication(firstHash);
  const manifest = {
    version: 1,
    head: firstHash,
    parts: current.commitPatchParts,
    size: current.commitPatchSize,
  };
  assert.equal(isCommitPatchManifest(manifest, firstHash), true);
  assert.equal(isCommitPatchManifest(manifest, secondHash), false);
  assert.equal(isCommitPatchManifest({ ...manifest, size: 2 }, firstHash), false);
  assert.equal(isCommitPatchManifest({
    ...manifest,
    parts: [{ key: `generations/${secondHash}/commit-patches/0000.diff`, size: 1 }],
  }, firstHash), false);
  assert.equal(isCommitPatchManifest({
    ...manifest,
    parts: [
      { key: `generations/${firstHash}/commit-patches/0000.diff`, size: 1 },
      { key: `generations/${firstHash}/commit-patches/0001.diff`, size: 2 },
    ],
    size: 3,
  }, firstHash), true);
});

test("publication uses compare-and-swap so stale mirrors cannot win", async () => {
  const values = new Map<string, unknown>();
  const state = {
    storage: {
      get: async <T>(key: string) => values.get(key) as T | undefined,
      put: async (key: string, value: unknown) => { values.set(key, value); },
    },
    blockConcurrencyWhile: async <T>(callback: () => Promise<T>) => callback(),
  } as unknown as DurableObjectState;
  const repository = new GitRepository(state);

  const first = await repository.fetch(publishRequest(null, publication(firstHash)));
  assert.equal(first.status, 200);
  const stale = await repository.fetch(publishRequest(null, publication(secondHash)));
  assert.equal(stale.status, 409);
  assert.deepEqual(await stale.json(), {
    error: "publication_conflict",
    currentHead: firstHash,
  });
  const second = await repository.fetch(publishRequest(firstHash, publication(secondHash)));
  assert.equal(second.status, 200);
  const current = await repository.fetch(new Request("https://repository.test/publication"));
  assert.equal((await current.json() as RepositoryPublication).head, secondHash);
});

test("publication repair atomically replaces only invalid stored state", async () => {
  const oldPrefix = `generations/${firstHash}/`;
  const values = new Map<string, unknown>([[
    "publication",
    {
      version: 1,
      head: firstHash,
      branch: "master",
      refs: [{ name: "refs/heads/master", oid: firstHash }],
      snapshotKey: `${oldPrefix}repository.json`,
      commitsKey: `${oldPrefix}commits.json`,
      inventoryKey: `${oldPrefix}inventory.json`,
      packKey: `${oldPrefix}repository.pack`,
      objectManifestKey: `${oldPrefix}objects.json`,
      packHash,
      publishedAt: "2026-08-17T00:00:00.000Z",
    },
  ]]);
  const state = {
    storage: {
      get: async <T>(key: string) => values.get(key) as T | undefined,
      put: async (key: string, value: unknown) => { values.set(key, value); },
    },
    blockConcurrencyWhile: async <T>(callback: () => Promise<T>) => callback(),
  } as unknown as DurableObjectState;
  const repository = new GitRepository(state);

  const normal = await repository.fetch(publishRequest(null, publication(secondHash)));
  assert.equal(normal.status, 409);
  assert.deepEqual(await normal.json(), { error: "publication_invalid" });

  const repaired = await repository.fetch(
    publishRequest(null, publication(secondHash), true),
  );
  assert.equal(repaired.status, 200);
  assert.equal((values.get("publication") as RepositoryPublication).head, secondHash);

  const clobber = await repository.fetch(
    publishRequest(null, publication(firstHash), true),
  );
  assert.equal(clobber.status, 409);
  assert.deepEqual(await clobber.json(), {
    error: "publication_repair_conflict",
    currentHead: secondHash,
  });
});

function publication(head: string): RepositoryPublication {
  const prefix = `generations/${head}/`;
  return {
    version: 1,
    head,
    branch: "master",
    refs: [{ name: "refs/heads/master", oid: head }],
    snapshotKey: `${prefix}repository.json`,
    commitsKey: `${prefix}commits.json`,
    commitPatchParts: [{ key: `${prefix}commit-patches/0000.diff`, size: 1 }],
    commitPatchSize: 1,
    inventoryKey: `${prefix}inventory.json`,
    packParts: [{ key: `${prefix}packs/${packHash}/0000.pack`, size: 1 }],
    packSize: 1,
    objectManifestKey: `${prefix}objects.json`,
    packHash,
    publishedAt: "2026-08-17T00:00:00.000Z",
  };
}

function publishRequest(
  expectedHead: string | null,
  value: RepositoryPublication,
  replaceInvalid = false,
): Request {
  return new Request("https://repository.test/publication", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      expectedHead,
      publication: value,
      ...(replaceInvalid ? { replaceInvalid: true } : {}),
    }),
  });
}
