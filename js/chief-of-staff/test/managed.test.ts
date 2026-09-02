import assert from "node:assert/strict";
import test from "node:test";

import {
  NanocodexManagedGateway,
  requestingAccountId,
  type ChiefOfStaffIdentity,
  type ManagedBackend,
} from "../src/managed.ts";

const identity: ChiefOfStaffIdentity = {
  provider: "whatsapp",
  subject: "15551234567",
  tenant: "123456789012345",
};

test("the managed gateway uses narrow RPC without bearer credentials or account IDs", async () => {
  const calls: unknown[][] = [];
  const backend: ManagedBackend = {
    async requestingAccountId() { return null; },
    async createAgent(...args) {
      calls.push(["create", ...args]);
      return "01991e48-76d1-7000-8000-000000000001";
    },
    async runTurn(...args) {
      calls.push(["turn", ...args]);
      return "done";
    },
  };
  const gateway = new NanocodexManagedGateway(backend, identity);
  const agentId = await gateway.createAgent("chief-session:key");
  const finalMessage = await gateway.runTurn(agentId, {
    id: "whatsapp-turn",
    idempotencyKey: "chief-turn:key",
    input: "hello",
  });

  assert.equal(finalMessage, "done");
  assert.deepEqual(calls, [
    ["create", identity, "chief-session:key"],
    ["turn", identity, agentId, {
      id: "whatsapp-turn",
      idempotencyKey: "chief-turn:key",
      input: "hello",
    }],
  ]);
  assert.equal(JSON.stringify(calls).includes("authorization"), false);
  assert.equal(JSON.stringify(calls).includes("accountId"), false);
});

test("Slack installer authentication forwards only browser authentication headers", async () => {
  let captured: Request | undefined;
  const backend: ManagedBackend = {
    async requestingAccountId(request) {
      captured = request;
      return "00000000-0000-4000-8000-000000000001";
    },
    async createAgent() { throw new Error("not used"); },
    async runTurn() { throw new Error("not used"); },
  };
  const accountId = await requestingAccountId(backend, new Request("https://chief.example/readiness", {
    headers: {
      authorization: "Bearer browser-session",
      cookie: "nanocodex_account=session",
      origin: "https://nanocodex.example",
      "x-forged-account-id": "victim",
    },
  }));

  assert.equal(accountId, "00000000-0000-4000-8000-000000000001");
  assert.equal(captured?.headers.get("cookie"), "nanocodex_account=session");
  assert.equal(captured?.headers.get("x-forged-account-id"), null);
});
