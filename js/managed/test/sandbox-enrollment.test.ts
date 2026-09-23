import { env } from "cloudflare:test";
import { expect, it } from "vitest";
import type { AccountHostedTools } from "../src/account-hosted-tools";

function fixture() {
  const owner = crypto.randomUUID();
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
  const account = namespace.getByName(owner);
  const headers = { "x-nanocodex-owner-id": owner };
  const enroll = (sandbox: boolean, id = crypto.randomUUID()) => account.fetch(
    `https://account-tools.internal/${sandbox ? "sandbox-hand-hosts" : "hand-hosts"}/${id}`,
    { method: "PUT", headers, body: JSON.stringify({ name: "Enrollment fixture", ...(sandbox ? { machine_id: `cf:${id}` } : {}) }) },
  );
  return { account, headers, enroll };
}

it("enrolls retained sandbox publishers beyond the server quota without consuming server capacity", async () => {
  const f = fixture();
  for (let i = 0; i < 65; i++) expect((await f.enroll(true)).status).toBe(201);
  expect((await f.enroll(false)).status).toBe(201);
  const list = await f.account.fetch("https://account-tools.internal/hand-hosts", { headers: f.headers });
  expect((await list.json<{ data: unknown[] }>()).data).toHaveLength(66);
});

it("retains the 64-server cap while allowing sandbox startup and credential renewal at capacity", async () => {
  const f = fixture(), server = crypto.randomUUID();
  expect((await f.enroll(false, server)).status).toBe(201);
  for (let i = 1; i < 64; i++) expect((await f.enroll(false)).status).toBe(201);
  expect((await f.enroll(false)).status).toBe(429);
  expect((await f.enroll(false, server)).status).toBe(201);
  const sandbox = crypto.randomUUID();
  const first = await f.enroll(true, sandbox);
  expect(first.status).toBe(201);
  const before = await first.json<{ credential: string }>();
  const rotated = await f.enroll(true, sandbox);
  expect(rotated.status).toBe(201);
  const after = await rotated.json<{ credential: string }>();
  expect(after.credential).not.toBe(before.credential);
  expect((await f.enroll(false)).status).toBe(429);
  const ice = (credential: string) => f.account.fetch(`https://account-tools.internal/hand-hosts/${sandbox}/hands/ice`, {
    method: "POST", headers: { ...f.headers, authorization: `Bearer ${credential}` },
  });
  expect((await ice(before.credential)).status).toBe(401);
  expect((await ice(after.credential)).status).toBe(200);
});

it("does not accept a sandbox quota exemption from a server enrollment body", async () => {
  const f = fixture();
  const response = await f.account.fetch(`https://account-tools.internal/hand-hosts/${crypto.randomUUID()}`, {
    method: "PUT", headers: f.headers, body: JSON.stringify({ name: "Server", machine_id: "cf:forged" }),
  });
  expect(response.status).toBe(400);
});
