import assert from "node:assert/strict";
import test from "node:test";
import worker from "./index.ts";

type Env = Parameters<typeof worker.fetch>[1];
const token = `nci_live_${"a".repeat(43)}_${"b".repeat(43)}`;
const ambient = {
  cookie: `nanocodex_account=s_${"a".repeat(43)}; nanocodex_chatgpt_v2=${"b".repeat(43)}; nanocodex_byok_v2=${"c".repeat(43)}`,
  "x-nanocodex-access": "synthetic-account-access",
  "x-nanocodex-owner-id": "11111111-1111-4111-8111-111111111111",
  origin: "https://nanocodex.example",
};
const inaccessible = new Proxy({} as Env, {
  get(_target, property) { assert.fail(`inference credential touched environment ${String(property)}`); },
});

const routes: [string, string][] = [
  ["GET", "/api/health"], ["POST", "/api/auth/chatgpt"], ["DELETE", "/api/auth/chatgpt"],
  ["PUT", "/api/auth/openai"], ["GET", "/api/responses"],
  ["GET", "/api/realtime/sideband"], ["POST", "/api/realtime/calls"],
  ["POST", "/api/tools/web-search"], ["POST", "/api/tools/image-generation"],
  ["GET", "/api/voice/elevenlabs/voices"], ["POST", "/api/voice/elevenlabs/speech"],
  ["POST", "/v1/machine-usd/orders"], ["GET", "/v1/machine-usd/config"],
  ["GET", "/api/connect/health"], ["POST", "/v1/connect/auth/start"],
  ["GET", "/v1/connectors/github?client=onboarding"], ["GET", "/v1/connections"],
  ["GET", "/connect-dialog"], ["GET", "/api/chief-of-staff/status"],
  ["GET", "/api/chief-of-staff/slack/install"], ["GET", "/v1/me"],
  ["GET", "/v1/account/hands"], ["POST", "/v1/agents"],
  ["GET", "/v1/inference-other"], ["GET", "/unknown"],
  ["POST", "/v1/responses/"], ["GET", "/v1/responses/response-id"],
  ["POST", "/v1/responses-other"], ["GET", "/v1/models/"],
  ["GET", "/v1/models/model-id"], ["GET", "/v1/models-other"],
  ["POST", "/v1/chat/completions"], ["POST", "/v1/sessions"],
];

for (const [method, path] of routes) {
  test(`inference bearer cannot reach ${method} ${path} with ambient account authority`, async () => {
    const request = new Request("https://nanocodex.example" + path, {
      method, headers: { ...ambient, authorization: `Bearer ${token}`, upgrade: "websocket" },
    });
    const response = await worker.fetch(request, inaccessible);
    assert.equal(response.status, 403);
    assert.deepEqual(await response.json(), { error: "inference_key_scope" });
    assert.equal(response.headers.get("cache-control"), "no-store");
  });
}

test("malformed and combined inference credentials cannot fall back to ambient authority", async () => {
  for (const authorization of [token, `bearer ${token}`, `Basic bad, Bearer ${token}`, `Bearer ${token}, Basic bad`, "Bearer nci_broken"]) {
    const response = await worker.fetch(new Request("http://nanocodex.example/api/health", {
      headers: { ...ambient, authorization },
    }), inaccessible);
    assert.equal(response.status, 403, authorization);
    assert.deepEqual(await response.json(), { error: "inference_key_scope" });
  }
});

test("inference namespace and exact Responses aliases reach dedicated authentication without adding authority", async () => {
  for (const [method, path] of [
    ["GET", "/v1/inference"], ["GET", "/v1/inference/models"], ["POST", "/v1/inference/sessions"],
    ["POST", "/v1/inference/responses"], ["GET", "/v1/inference/keys"],
    ["GET", "/v1/models"], ["POST", "/v1/responses"],
  ]) {
    const request = new Request("https://nanocodex.example" + path, {
      method, headers: { ...ambient, authorization: `Bearer ${token}`, "content-type": "application/json" },
      ...(method === "POST" ? { body: JSON.stringify({ model: "auto", input: "Synthetic input" }) } : {}),
    });
    let forwarded = 0;
    const response = await worker.fetch(request, {
      NANOCODEX_BACKEND: { fetch: async (candidate: Request) => {
        forwarded++;
        assert.equal(candidate, request);
        assert.equal(candidate.headers.get("authorization"), `Bearer ${token}`);
        assert.equal(candidate.headers.get("x-nanocodex-access"), ambient["x-nanocodex-access"]);
        if (method === "POST") assert.deepEqual(await candidate.json(), { model: "auto", input: "Synthetic input" });
        return Response.json({ error: "synthetic_dedicated_authenticator" }, { status: 401 });
      } } as unknown as Fetcher,
    } as Env);
    assert.equal(response.status, 401);
    assert.equal(forwarded, 1);
    assert.deepEqual(await response.json(), { error: "synthetic_dedicated_authenticator" });
  }
});

test("ordinary account authorization still reaches existing managed authentication", async () => {
  for (const authorization of [undefined, "Bearer ncx_live_synthetic"]) {
    let forwarded = 0;
    const request = new Request("https://nanocodex.example/v1/me", {
      headers: { ...ambient, ...(authorization ? { authorization } : {}) },
    });
    const response = await worker.fetch(request, {
      NANOCODEX_BACKEND: { fetch: async (candidate: Request) => {
        forwarded++;
        assert.equal(candidate, request);
        return Response.json({ synthetic: "existing_account_authentication" });
      } } as unknown as Fetcher,
    } as Env);
    assert.equal(response.status, 200);
    assert.equal(forwarded, 1);
  }
});
