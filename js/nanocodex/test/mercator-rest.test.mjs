import assert from "node:assert/strict";
import { test } from "node:test";

import { mercatorRestTool } from "../cloud/mercator.mjs";

const connection = {
  mpp: { maxPerRequest: 250_000n },
};

test("Mercator REST handoffs execute through bounded MPP fetch", async () => {
  const calls = new Set();
  const requests = [];
  const tool = mercatorRestTool({
    connection,
    calls,
    relay: "https://connect.example/v1/mercator/jobs",
    async fetch(...args) {
      requests.push(args);
      return new Response(JSON.stringify({ id: "job_1", status: "pending" }), {
        status: 201,
        headers: { "payment-receipt": "receipt_1" },
      });
    },
  });
  const body = { idempotencyKey: "quote-1", plan: { steps: [] } };
  const result = await tool.handler({
    url: "https://mercator.tempo.xyz/v1/jobs",
    method: "POST",
    body,
    maxSpend: "0.005",
  });

  assert.deepEqual(result, {
    status: 201,
    paymentReceipt: "receipt_1",
    result: { id: "job_1", status: "pending" },
  });
  assert.deepEqual([...calls], ["run_rest_request"]);
  assert.equal(requests.length, 1);
  assert.equal(String(requests[0][0]), "https://connect.example/v1/mercator/jobs");
  assert.equal(requests[0][1].method, "POST");
  assert.deepEqual(JSON.parse(requests[0][1].body), body);
  assert.deepEqual(requests[0][2], { intent: "charge", maxAmount: 5_000n });
});

test("Mercator REST handoffs cannot escape their origin or Connect spend limit", async () => {
  const tool = mercatorRestTool({
    connection,
    calls: new Set(),
    fetch: async () => new Response(),
    relay: "https://connect.example/v1/mercator/jobs",
  });
  const request = {
    url: "https://mercator.tempo.xyz/v1/jobs",
    method: "POST",
    body: {},
    maxSpend: "0.005",
  };

  await assert.rejects(
    tool.handler({ ...request, url: "https://example.com/v1/jobs" }),
    /must target https:\/\/mercator\.tempo\.xyz\/v1\/jobs/,
  );
  await assert.rejects(
    tool.handler({ ...request, maxSpend: "0.250001" }),
    /per-request limit/,
  );
  await assert.rejects(
    tool.handler({ ...request, maxSpend: "0.0000001" }),
    /at most 6 decimal places/,
  );
});
