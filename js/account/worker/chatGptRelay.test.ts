import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { startRelay } from "../container/relay.mjs";

test("production ChatGPT relay forwards pinned history/notes routes and protocol headers", async () => {
  const requests: Array<{ path: string; headers: Record<string, unknown>; body: string }> = [];
  const upstream = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += String(chunk);
    requests.push({ path: request.url!, headers: request.headers, body });
    response.writeHead(200, { "content-type": "application/json", "set-cookie": "provider-private" });
    response.end('{"encrypted_output":"opaque"}');
  });
  await new Promise<void>((resolve) => upstream.listen(0, "127.0.0.1", resolve));
  const address = upstream.address();
  assert.ok(address && typeof address === "object");
  const relay = startRelay({ host: "127.0.0.1", port: 0, upstreamOrigin: `http://127.0.0.1:${address.port}` });
  await new Promise<void>((resolve) => relay.once("listening", resolve));
  const relayAddress = relay.address();
  assert.ok(relayAddress && typeof relayAddress === "object");
  const origin = `http://127.0.0.1:${relayAddress.port}`;
  try {
    for (const [namespace, actions] of [
      ["history", ["list_windows", "list_items", "read_item", "search_contents"]],
      ["notes", ["list_files_by_prefix", "read_file", "search_contents", "write_file", "append_to_file", "thread_hint"]],
    ] as const) for (const action of actions) {
      const path = `/backend-api/codex/alpha/${namespace}/v2/${action}`;
      const result = await fetch(`${origin}${path}`, {
        method: "POST", headers: {
          authorization: "Bearer test-token", "chatgpt-account-id": "account", "content-type": "application/json",
          "x-openai-encrypted-tool-arguments": "true",
          "x-openai-tool-output-truncation-policy": '{"mode":"tokens","limit":10000}',
          "x-nanocodex-subject": "private-subject",
        }, body: '{"context":{"session_id":"session","current_agent_name":"/root"}}',
      });
      assert.deepEqual(await result.json(), { encrypted_output: "opaque" });
      assert.equal(result.headers.get("set-cookie"), null);
      const seen = requests.at(-1)!;
      assert.equal(seen.path, path);
      assert.equal(seen.headers.authorization, "Bearer test-token");
      assert.equal(seen.headers["x-openai-encrypted-tool-arguments"], "true");
      assert.equal(seen.headers["x-openai-tool-output-truncation-policy"], '{"mode":"tokens","limit":10000}');
      assert.equal(seen.headers["x-nanocodex-subject"], undefined);
    }
    assert.equal((await fetch(`${origin}/backend-api/codex/alpha/notes/v2/delete_all`, { method: "POST" })).status, 404);
    assert.equal((await fetch(`${origin}/backend-api/codex/alpha/notes/v2/read_file`, { method: "POST" })).status, 401);
    assert.equal(requests.length, 10);
  } finally {
    relay.closeAllConnections();
    upstream.closeAllConnections();
    await Promise.all([
      new Promise<void>((resolve) => relay.close(() => resolve())),
      new Promise<void>((resolve) => upstream.close(() => resolve())),
    ]);
  }
});
