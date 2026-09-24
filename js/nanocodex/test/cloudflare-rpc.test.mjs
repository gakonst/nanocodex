import assert from "node:assert/strict";
import { test } from "node:test";
import { consumeRpcData } from "nanocodex/cloudflare/rpc";

test("RPC data is detached and its result owner is disposed exactly once", () => {
  let disposals = 0;
  const result = { status: 200, catalog: { connectors: {}, mcp_connections: [] },
    [Symbol.dispose]() { assert.equal(this, result); disposals++; } };
  const data = consumeRpcData(result);
  assert.equal(disposals, 1);
  assert.equal(data[Symbol.dispose], undefined);
  assert.notEqual(data.catalog, result.catalog);
  result.catalog.mcp_connections.push({ id: "synthetic" });
  assert.deepEqual(data.catalog.mcp_connections, []);
});

test("failed cloning still disposes the result without silently dropping capabilities", () => {
  let disposals = 0;
  assert.throws(() => consumeRpcData({ callback() {}, [Symbol.dispose]() { disposals++; } }),
    { name: "DataCloneError" });
  assert.equal(disposals, 1);
});
