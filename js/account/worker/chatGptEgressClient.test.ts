import assert from "node:assert/strict";
import test from "node:test";

import { fetchChatGpt, warmChatGptEgress } from "./chatGptEgressClient.ts";

const SESSION_ID = "a".repeat(43);

function egress() {
  const requests: Request[] = [];
  const stub = {
    async fetch(request: Request) {
      requests.push(request);
      return new Response(null, { status: 204 });
    },
  } as unknown as DurableObjectStub;
  const namespace = {
    idFromName(name: string) {
      assert.equal(name, `session-v2:${SESSION_ID}`);
      return {} as DurableObjectId;
    },
    get() {
      return stub;
    },
  } as unknown as DurableObjectNamespace;
  return { namespace, requests };
}

test("production ChatGPT traffic uses only the fixed internal Container binding", async () => {
  const { namespace, requests } = egress();
  const response = await fetchChatGpt(
    { ENVIRONMENT: "production", CHATGPT_EGRESS: namespace },
    "https://chatgpt.com/backend-api/codex/alpha/search?stream=1",
    {
      method: "POST",
      headers: { authorization: "Bearer secret", origin: "https://browser.example" },
      body: '{"query":"nanocodex"}',
    },
    SESSION_ID,
  );

  assert.equal(response.status, 204);
  assert.equal(requests.length, 1);
  assert.equal(requests[0]?.url, "https://chatgpt-egress.internal/backend-api/codex/alpha/search?stream=1");
  assert.equal(requests[0]?.headers.get("authorization"), "Bearer secret");
  assert.equal(await requests[0]?.text(), '{"query":"nanocodex"}');
});

test("production egress rejects every non-ChatGPT origin", async () => {
  const { namespace } = egress();
  await assert.rejects(
    fetchChatGpt(
      { ENVIRONMENT: "production", CHATGPT_EGRESS: namespace },
      "https://attacker.example/backend-api/codex/responses",
      undefined,
      SESSION_ID,
    ),
    /only accepts chatgpt.com/,
  );
});

test("warming targets only the private Container health route", async () => {
  const { namespace, requests } = egress();
  await warmChatGptEgress(
    { ENVIRONMENT: "production", CHATGPT_EGRESS: namespace },
    SESSION_ID,
  );
  assert.equal(requests[0]?.url, "https://chatgpt-egress.internal/health");
});
