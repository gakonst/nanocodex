import assert from "node:assert/strict";
import test from "node:test";
import { QueryObserver } from "@tanstack/react-query";
import { accountResourceKey, accountResourceOptions, refreshAccountResource } from "./accountQueries.ts";
import { createAppQueryClient, clearOtherAccountQueries, retryQuery, sessionQueryKey } from "./queryClient.ts";
import { sessionQueryOptions } from "./sessionQueries.ts";

const accountA = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const accountB = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const path = "/v1/credentials";

test("account and vault observers share a request and reuse fresh data on remount", async (t) => {
  const client = testQueryClient();
  t.after(() => client.clear());
  let requests = 0;
  t.mock.method(globalThis, "fetch", async (_url: string, init: RequestInit) => {
    requests++;
    assert.equal(init.cache, "no-store");
    assert.equal(init.credentials, "same-origin");
    assert.ok(init.signal);
    return Response.json({ openai: { connected: true }, vault: [{ id: "entry" }] });
  });
  const options = accountResourceOptions(accountA, path);
  const credentials = new QueryObserver(client, { ...options, select: (value) => (value as { openai: unknown }).openai });
  const vault = new QueryObserver(client, { ...options, select: (value) => (value as { vault: unknown }).vault });
  const unsubscribeCredentials = credentials.subscribe(() => {});
  const unsubscribeVault = vault.subscribe(() => {});
  await client.fetchQuery(options);
  assert.deepEqual(credentials.getCurrentResult().data, { connected: true });
  assert.deepEqual(vault.getCurrentResult().data, [{ id: "entry" }]);
  unsubscribeCredentials();
  unsubscribeVault();
  await client.fetchQuery(options);
  assert.equal(requests, 1);
});

test("mutation refresh cancels old reads and updates every observer of the resource", async (t) => {
  const client = testQueryClient();
  t.after(() => client.clear());
  const queryKey = accountResourceKey(accountA, path);
  client.setQueryData(queryKey, { version: 1 });
  let oldSignal: AbortSignal | undefined;
  let resolveOld!: (response: Response) => void;
  let requests = 0;
  t.mock.method(globalThis, "fetch", async (_url: string, init: RequestInit) => {
    requests++;
    if (requests === 1) {
      oldSignal = init.signal as AbortSignal;
      return new Promise<Response>((resolve) => { resolveOld = resolve; });
    }
    return Response.json({ version: 2 });
  });
  const observer = new QueryObserver(client, accountResourceOptions(accountA, path));
  const unsubscribe = observer.subscribe(() => {});
  t.after(unsubscribe);
  const oldRequest = client.fetchQuery({ ...accountResourceOptions(accountA, path), staleTime: 0 }).catch(() => undefined);
  await refreshAccountResource(client, accountA, path);
  assert.equal(oldSignal?.aborted, true);
  resolveOld(Response.json({ version: 0 }));
  await oldRequest;
  assert.deepEqual(observer.getCurrentResult().data, { version: 2 });
  assert.equal(requests, 2);
});

test("switching accounts cancels private reads and prevents late mutations from restoring them", async (t) => {
  const client = testQueryClient();
  t.after(() => client.clear());
  let signal: AbortSignal | undefined;
  let resolve!: (response: Response) => void;
  t.mock.method(globalThis, "fetch", async (_url: string, init: RequestInit) => {
    signal = init.signal as AbortSignal;
    return new Promise<Response>((done) => { resolve = done; });
  });
  client.setQueryData(["evals", "overview"], { public: true });
  client.setQueryData(accountResourceKey(accountB, path), { owner: accountB });
  const pending = client.fetchQuery(accountResourceOptions(accountA, path)).catch(() => undefined);
  clearOtherAccountQueries(client, accountB);
  assert.equal(signal?.aborted, true);
  resolve(Response.json({ owner: accountA }));
  await pending;
  await refreshAccountResource(client, accountA, path);
  assert.equal(client.getQueryData(accountResourceKey(accountA, path)), undefined);
  assert.deepEqual(client.getQueryData(accountResourceKey(accountB, path)), { owner: accountB });
  assert.deepEqual(client.getQueryData(["evals", "overview"]), { public: true });
});

test("expired and changed sessions discard private data while retaining public caches", async (t) => {
  const client = testQueryClient();
  t.after(() => client.clear());
  let expired = false;
  t.mock.method(globalThis, "fetch", async () => expired
    ? Response.json({ error: "reauthentication_required" }, { status: 401 })
    : Response.json({ user: { id: accountB, persistent: true } }));
  client.setQueryData(sessionQueryKey, { account: { id: accountA, persistent: true }, reauthenticationRequired: false });
  client.setQueryData(accountResourceKey(accountA, path), { owner: accountA });
  await client.fetchQuery({ ...sessionQueryOptions(client), staleTime: 0 });
  assert.equal(client.getQueryData(accountResourceKey(accountA, path)), undefined);
  client.setQueryData(accountResourceKey(accountB, path), { owner: accountB });
  expired = true;
  const session = await client.fetchQuery({ ...sessionQueryOptions(client), staleTime: 0 });
  assert.deepEqual(session, { account: null, reauthenticationRequired: true });
  assert.equal(client.getQueryData(accountResourceKey(accountB, path)), undefined);
});

test("failed reads can retry on the next visit; permanent HTTP errors are not retried", async (t) => {
  const client = testQueryClient();
  t.after(() => client.clear());
  let requests = 0;
  t.mock.method(globalThis, "fetch", async () => ++requests === 1
    ? Response.json({ error: "forbidden" }, { status: 403 })
    : Response.json({ ready: true }));
  await assert.rejects(client.fetchQuery(accountResourceOptions(accountA, path)), /forbidden/);
  assert.equal(requests, 1);
  assert.deepEqual(await client.fetchQuery(accountResourceOptions(accountA, path)), { ready: true });
  assert.equal(retryQuery(0, Object.assign(new Error(), { status: 401 })), false);
  assert.equal(retryQuery(0, Object.assign(new Error(), { status: 429 })), true);
  assert.equal(retryQuery(2, new Error("offline")), false);
});

test("inactive queries expire after the configured retention window", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const client = createAppQueryClient();
  t.after(() => client.clear());
  await client.fetchQuery({ queryKey: ["unused"], queryFn: async () => "cached" });
  t.mock.timers.tick(10 * 60_000 - 1);
  assert.equal(client.getQueryData(["unused"]), "cached");
  t.mock.timers.tick(2);
  assert.equal(client.getQueryData(["unused"]), undefined);
});

function testQueryClient() {
  const client = createAppQueryClient();
  // Cancelled mock transports may finish after clear(); avoid real GC timers
  // keeping Node alive after a test. Browser retention is checked separately.
  client.setDefaultOptions({ ...client.getDefaultOptions(), queries: { ...client.getDefaultOptions().queries, gcTime: Infinity } });
  return client;
}
