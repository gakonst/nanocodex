import assert from "node:assert/strict";
import test from "node:test";

import { requestingAccountId, type ManagedBackend } from "../src/managed.ts";

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
