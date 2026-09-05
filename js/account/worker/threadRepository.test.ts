import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ThreadGitRepository,
  isThreadRepository,
  type ThreadPack,
  type ThreadPackMetadata,
  type ThreadRepository,
} from "./threadRepository.ts";

const zero = "0".repeat(40);
const headA = "a".repeat(40);
const headB = "b".repeat(40);
const headC = "c".repeat(40);
const ref = "refs/heads/nanocodex";

test("thread repository state accepts only one nanocodex ref and a valid pack chain", () => {
  assert.equal(isThreadRepository(repository()), true);
  assert.equal(isThreadRepository({ ...repository(), branch: "main" }), false);
  assert.equal(isThreadRepository({ ...repository(), refs: [{ name: ref, oid: headB }] }), false);
  assert.equal(isThreadRepository({ ...repository(), packs: [] }), false);
  assert.equal(isThreadRepository({
    ...repository(),
    packs: [{ ...pack("a", zero, headA), key: "../escape.pack" }],
  }), false);
  assert.equal(isThreadRepository({
    ...repository(),
    packs: [pack("a", zero, headA), pack("a", headA, headB)],
  }), false);
  assert.equal(isThreadRepository({
    ...repository(),
    head: headB,
    refs: [{ name: ref, oid: headB }],
    packs: [pack("a", zero, headA), pack("b", headC, headB)],
  }), false);
});

test("receive leases CAS the old OID and append immutable packs", async () => {
  const { durable } = memoryRepository();

  const first = await begin(durable, zero, headA);
  assert.equal(first.status, 200);
  const busy = await begin(durable, zero, headA);
  assert.equal(busy.status, 409);
  assert.deepEqual(await busy.json(), { error: "receive_busy" });
  const firstToken = await leaseToken(first);
  assert.equal((await finalize(durable, firstToken, packMetadata("a"))).status, 200);

  const firstState = await current(durable);
  assert.equal(firstState.head, headA);
  assert.deepEqual(firstState.refs, [{ name: ref, oid: headA }]);
  assert.deepEqual(firstState.packs, [pack("a", zero, headA)]);

  const second = await begin(durable, headA, headB);
  const secondToken = await leaseToken(second);
  assert.equal((await finalize(durable, secondToken, packMetadata("b"))).status, 200);
  const secondState = await current(durable);
  assert.equal(secondState.head, headB);
  assert.deepEqual(secondState.packs, [
    pack("a", zero, headA),
    pack("b", headA, headB),
  ]);

  const stale = await begin(durable, headA, headC);
  assert.equal(stale.status, 409);
  assert.deepEqual(await stale.json(), { error: "stale_receive", currentHead: headB });
  assert.deepEqual(await current(durable), secondState);
});

test("finalize rejects malformed metadata and abort is token scoped", async () => {
  const { durable } = memoryRepository();
  const started = await begin(durable, zero, headA);
  const token = await leaseToken(started);

  const malformed = await finalize(durable, token, { ...packMetadata("a"), objectCount: 0 });
  assert.equal(malformed.status, 400);
  const wrongAbort = await durable.fetch(new Request("https://repository.test/receive/abort", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: "wrong" }),
  }));
  assert.equal(wrongAbort.status, 200);
  assert.equal((await begin(durable, zero, headB)).status, 409);

  await durable.fetch(new Request("https://repository.test/receive/abort", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
  }));
  assert.equal((await begin(durable, zero, headB)).status, 200);
});

test("finalize owns transition metadata from the receive lease", async () => {
  const { durable } = memoryRepository();
  const token = await leaseToken(await begin(durable, zero, headA));
  const callerPack = {
    ...packMetadata("a"),
    oldOid: headC,
    newOid: headC,
  };
  const response = await finalize(durable, token, callerPack);
  assert.equal(response.status, 200);
  assert.deepEqual((await current(durable)).packs, [pack("a", zero, headA)]);
});

function memoryRepository(): { durable: ThreadGitRepository } {
  const values = new Map<string, unknown>();
  const state = {
    storage: {
      get: async <T>(key: string) => structuredClone(values.get(key)) as T | undefined,
      put: async (key: string, value: unknown) => { values.set(key, structuredClone(value)); },
      delete: async (key: string) => values.delete(key),
    },
    blockConcurrencyWhile: async <T>(callback: () => Promise<T>) => callback(),
  } as unknown as DurableObjectState;
  return { durable: new ThreadGitRepository(state) };
}

function begin(durable: ThreadGitRepository, oldOid: string, newOid: string): Promise<Response> {
  return durable.fetch(new Request("https://repository.test/receive/begin", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ oldOid, newOid, ref }),
  }));
}

function finalize(
  durable: ThreadGitRepository,
  token: string,
  nextPack: ThreadPackMetadata,
): Promise<Response> {
  return durable.fetch(new Request("https://repository.test/receive/finalize", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, pack: nextPack }),
  }));
}

async function leaseToken(response: Response): Promise<string> {
  assert.equal(response.status, 200);
  return ((await response.json()) as { lease: { token: string } }).lease.token;
}

async function current(durable: ThreadGitRepository): Promise<ThreadRepository> {
  const response = await durable.fetch(new Request("https://repository.test/thread"));
  assert.equal(response.status, 200);
  return response.json() as Promise<ThreadRepository>;
}

function packMetadata(name: string): ThreadPackMetadata {
  return {
    key: `thread-repositories/thread-12345678-1234-4123-8123-123456789abc/${name}.pack`,
    hash: name.charCodeAt(0).toString(16).padStart(2, "0").repeat(20),
    size: 123,
    objectCount: 3,
  };
}

function pack(name: string, oldOid: string, newOid: string): ThreadPack {
  return { ...packMetadata(name), oldOid, newOid };
}

function repository(): ThreadRepository {
  return {
    version: 1,
    branch: "nanocodex",
    head: headA,
    refs: [{ name: ref, oid: headA }],
    packs: [pack("a", zero, headA)],
    updatedAt: "2026-08-18T00:00:00.000Z",
  };
}
