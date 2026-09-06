import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { createChatGptSubscriptionPlugin } from "../chatgpt-subscription.mjs";

test("Vite history/notes authenticates locally and rejects foreign origins and routes", async () => {
  let plan = "plus";
  const calls = [];
  const middlewares = [];
  const server = createServer((request, response) => {
    let index = 0;
    const next = () => middlewares[index++]?.(request, response, next) ?? undefined;
    next();
  });
  const plugin = createChatGptSubscriptionPlugin({ statusPath: false }, {
    readAuth: async () => ({
      accessToken: `e30.${Buffer.from(JSON.stringify({ "https://api.openai.com/auth": { chatgpt_plan_type: plan } })).toString("base64url")}.private-signature`,
      accountId: "private-account", expiresAt: Date.now() + 3_600_000, fedramp: true,
    }),
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      return Response.json({ encrypted_output: "opaque-result" });
    },
  });
  await plugin.configureServer({
    httpServer: server, middlewares: { use(fn) { middlewares.push(fn); } },
    config: { logger: { info() {}, warn() {} } },
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const post = (body, headers = {}) => fetch(`${origin}/api/responses/context`, {
    method: "POST", headers: { origin, "content-type": "application/json", ...headers }, body: JSON.stringify(body),
  });
  try {
    assert.deepEqual(await (await post({})).json(), { enabled: true });
    const request = {
      path: "alpha/notes/v2/write_file", threadId: "thread",
      body: { path: "progress", content: "checkpoint", context: { session_id: "session", current_agent_name: "/root" } },
      budget: { mode: "tokens", limit: 200_000 },
    };
    const response = await post(request);
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { encrypted_output: "opaque-result" });
    assert.equal(calls[0].url, "https://chatgpt.com/backend-api/codex/alpha/notes/v2/write_file");
    assert.match(calls[0].init.headers.get("authorization"), /private-signature/);
    assert.equal(calls[0].init.headers.get("chatgpt-account-id"), "private-account");
    assert.equal(calls[0].init.headers.get("x-openai-encrypted-tool-arguments"), "true");
    assert.equal(calls[0].init.headers.get("x-openai-fedramp"), "true");
    assert.deepEqual(JSON.parse(calls[0].init.headers.get("x-openai-tool-output-truncation-policy")), request.budget);
    assert.equal((await post(request, { origin: "https://evil.example" })).status, 403);
    assert.equal((await post(request, { authorization: "Bearer injected" })).status, 403);
    assert.equal((await post({ ...request, path: "alpha/notes/v2/delete_all" })).status, 400);
    plan = "business";
    assert.deepEqual(await (await post({})).json(), { enabled: false });
    assert.equal((await post(request)).status, 409);
    assert.equal(calls.length, 1);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});
