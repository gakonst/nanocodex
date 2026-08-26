import assert from "node:assert/strict";
import test from "node:test";

import { createLocalDevelopmentCredentialResource } from "../src/localDevelopmentCredential.ts";

test("localhost credential claim is single-flight for one browser identity", async () => {
  let calls = 0;
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => { release = resolve; });
  const resource = createLocalDevelopmentCredentialResource(async (_input, init) => {
    calls += 1;
    assert.equal(init?.method, "POST");
    assert.equal(init?.credentials, "same-origin");
    await blocked;
    return new Response(null, { status: 204 });
  }, "localhost");

  const first = resource.ensure("user-a");
  const second = resource.ensure("user-a");
  assert.equal(calls, 1);
  release();
  assert.equal(await first, true);
  assert.equal(await second, true);
  assert.equal(await resource.ensure("user-a"), true);
  assert.equal(calls, 1);
});

test("a changed browser identity receives its own localhost credential claim", async () => {
  const users: string[] = [];
  const resource = createLocalDevelopmentCredentialResource(async () => {
    users.push("claim");
    return new Response(null, { status: 204 });
  }, "127.0.0.1");

  assert.equal(await resource.ensure("user-a"), true);
  assert.equal(await resource.ensure("user-b"), true);
  assert.deepEqual(users, ["claim", "claim"]);
});

test("the canonical browser hostname receives the local credential claim", async () => {
  let calls = 0;
  const resource = createLocalDevelopmentCredentialResource(async () => {
    calls += 1;
    return new Response(null, { status: 204 });
  }, "nanocodex.localhost");

  assert.equal(await resource.ensure("user-a"), true);
  assert.equal(calls, 1);
});

test("an instance-scoped browser hostname receives the local credential claim", async () => {
  let calls = 0;
  const resource = createLocalDevelopmentCredentialResource(async () => {
    calls += 1;
    return new Response(null, { status: 204 });
  }, "passkey-a.nanocodex.localhost");

  assert.equal(await resource.ensure("user-a"), true);
  assert.equal(calls, 1);
});

test("a rejected localhost claim blocks startup and remains retryable", async () => {
  let calls = 0;
  const resource = createLocalDevelopmentCredentialResource(async () => {
    calls += 1;
    return new Response(null, { status: calls === 1 ? 503 : 204 });
  }, "localhost");

  await assert.rejects(resource.ensure("user-a"), /HTTP 503/);
  assert.equal(await resource.ensure("user-a"), true);
  assert.equal(calls, 2);
});

test("production hosts never expose the localhost credential claim", async () => {
  let calls = 0;
  const resource = createLocalDevelopmentCredentialResource(async () => {
    calls += 1;
    return new Response(null, { status: 204 });
  }, "nanocodex.gakonst.workers.dev");

  assert.equal(await resource.ensure("user-a"), false);
  assert.equal(calls, 0);
});
