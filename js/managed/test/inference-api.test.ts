import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type Env } from "../src/index";
import { routeInferenceApi, type InferenceApiEnv } from "../src/inference-api";

const token = `nci_live_${"a".repeat(43)}_${"b".repeat(43)}`;
const headers = { authorization: `Bearer ${token}`, cookie: "synthetic=owner",
  "x-nanocodex-request-principal": JSON.stringify({ kind: "api_key", user_id: "11111111-1111-4111-8111-111111111111" }),
  "x-nanocodex-owner-id": "11111111-1111-4111-8111-111111111111" };

describe("inference HTTP isolation boundary", () => {
  it.each(["/v1/agents", "/v1/agents/11111111-1111-4111-8111-111111111111/turns", "/v1/api-keys",
    "/v1/credentials", "/v1/connectors/github", "/v1/connectors", "/v1/account/hands", "/v1/account/tool-host",
    "/v1/phone/bridge/health", "/v1/history", "/v1/memory", "/v1/wallet", "/v1/egress", "/v1/me",
    "/sandbox-preview/synthetic/", "/v1/organization", "/v1/inference-other", "/auth/session"])("rejects %s before any account binding", async path => {
    const response = await worker.fetch(new Request("https://nanocodex.example" + path, { headers }), {} as Env, createExecutionContext());
    expect(response.status).toBe(403);
    expect(await response.json()).toMatchObject({ error: "inference_key_scope" });
  });
  it.each(["GET", "POST", "DELETE"])("inference keys cannot administer keys via %s even with caller-supplied ownership headers", async method => {
    const response = await routeInferenceApi(new Request("https://nanocodex.example/v1/inference/keys", { method, headers }),
      {} as InferenceApiEnv, new URL("https://nanocodex.example/v1/inference/keys"));
    expect(response?.status).toBe(403);
  });
  it("does not intercept existing account requests without an inference credential", async () => {
    const request = new Request("https://nanocodex.example/v1/agents", { headers: { authorization: "Bearer ncx_live_synthetic" } });
    expect(await routeInferenceApi(request, {} as InferenceApiEnv, new URL(request.url))).toBeUndefined();
  });
  it("fails closed when inference is disabled", async () => {
    const request = new Request("https://nanocodex.example/v1/inference/models", { headers });
    expect((await routeInferenceApi(request, {} as InferenceApiEnv, new URL(request.url)))?.status).toBe(503);
  });
});


it("real key issuance and public gateway sessions isolate two callers", async () => {
  const bindings = { ...env, NANOCODEX_INFERENCE_ENABLED: "true", AI: { run: async () => { throw Error("no inference expected"); } } } as unknown as InferenceApiEnv;
  const userId = crypto.randomUUID();
  bindings.NANOCODEX_ADMIN_USER_ID = userId;
  const principal = { kind: "api_key", userId, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "owner",
    subjectId: `user:${userId}`, credentialId: "synthetic-owner-key", authorizationEpoch: 1,
    capabilities: ["api_keys:read", "api_keys:write"] } as const;
  async function call(path: string, method: string, body?: unknown, token?: string) {
    const request = new Request("https://nanocodex.example/v1/inference" + path, { method,
      headers: { "content-type": "application/json", ...(token ? { authorization: "Bearer " + token } : {}) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    return (await routeInferenceApi(request, bindings, new URL(request.url), token ? undefined : principal))!;
  }
  const key1 = await (await call("/keys", "POST", { label: "synthetic one" })).json<{api_key: string; key: {id: string}}>();
  const key2 = await (await call("/keys", "POST", { label: "synthetic two" })).json<{api_key: string; key: {id: string}}>();
  const models = await call("/models", "GET", undefined, key1.api_key);
  expect(models.status).toBe(200);
  const catalog = await models.json<{data: Array<{provider: string}>}>();
  expect(catalog.data.length).toBe(3);
  expect(catalog.data.every(candidate => candidate.provider === "workers_ai")).toBe(true);
  // The catalog is deployment gated and inference-only even when every
  // standalone transport is configured. No subscription/account models leak.
  bindings.OPENROUTER_API_KEY = "synthetic-catalog-only";
  bindings.AI_GATEWAY_API_KEY = "synthetic-catalog-only";
  for (const gate of [undefined, "false", "true"] as const) {
    bindings.NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED = gate;
    const result = await call("/models", "GET", undefined, key1.api_key);
    expect(result.status).toBe(200);
    const expanded = await result.json<{ data: Array<{ id: string; provider: string }> }>();
    expect(expanded.data).toHaveLength(gate === "true" ? 45 : 33);
    expect(expanded.data.some(c => c.provider === "chatgpt")).toBe(false);
    const cloudflare = expanded.data.filter(c => c.provider === "cloudflare");
    expect(cloudflare).toHaveLength(gate === "true" ? 12 : 0);
    if (gate === "true") expect(cloudflare.map(c => c.id).sort()).toEqual(
      ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"].flatMap(model =>
        ["low", "medium", "high"].map(effort => `cloudflare:openai/${model}:${effort}`)).sort());
  }
  delete bindings.OPENROUTER_API_KEY;
  delete bindings.AI_GATEWAY_API_KEY;
  delete bindings.NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED;
  const created = await call("/sessions", "POST", {}, key1.api_key);
  expect(created.status).toBe(201);
  const session = await created.json<{id:string; key_id:string; route:null}>();
  expect(session.id).toMatch(/^[0-9a-f-]{36}$/);
  expect(session.key_id).toBe(key1.key.id);
  expect(session.route).toBeNull();
  expect((await call("/sessions/" + session.id, "GET", undefined, key1.api_key)).status).toBe(200);
  expect((await call("/sessions/" + session.id, "GET", undefined, key2.api_key)).status).toBe(404);
  expect((await call("/sessions/" + session.id, "DELETE", undefined, key2.api_key)).status).toBe(404);
  expect((await call("/responses", "POST", {session_id:session.id,input:"synthetic"},key2.api_key)).status).toBe(404);
  expect((await call("/sessions", "POST", {key_id:key2.key.id},key1.api_key)).status).toBe(400);
  expect((await call("/sessions/" + session.id, "DELETE", undefined, key1.api_key)).status).toBe(204);
  expect((await call("/keys/" + key1.key.id, "DELETE")).status).toBe(204);
  expect((await call("/models", "GET", undefined, key1.api_key)).status).toBe(401);
  expect((await call("/models", "GET", undefined, key2.api_key)).status).toBe(200);
  expect((await call("/keys/" + key2.key.id, "DELETE")).status).toBe(204);
});

it.each(["Basic nci_live_synthetic", "bearer NCI_LIVE_synthetic", "nci_live_synthetic", "Bearer synthetic, nci_live_synthetic"])("malformed inference credential cannot fall back to ambient account auth: %s", async authorization => {
  const request = new Request("https://nanocodex.example/v1/agents", {headers:{...headers, authorization}});
  const response=await worker.fetch(request, {} as Env, createExecutionContext());
  expect(response.status).toBe(403);
});

it("ordinary and anonymous account authority cannot mint shared-credit inference keys", async () => {
  const request = new Request("https://nanocodex.example/v1/inference/keys", {method:"POST",headers:{"content-type":"application/json"},body:"{}"});
  const userId = crypto.randomUUID();
  const principal = {kind:"account_session",userId,organizationId:crypto.randomUUID(),teamId:crypto.randomUUID(),role:"owner",subjectId:`user:${userId}`,credentialId:"synthetic",authorizationEpoch:1,capabilities:["api_keys:write"]} as const;
  const response=await routeInferenceApi(request,{NANOCODEX_ADMIN_USER_ID:crypto.randomUUID()} as InferenceApiEnv,new URL(request.url),principal);
  expect(response?.status).toBe(403);
  expect(await response?.json()).toEqual({error:"inference_key_admin_required"});
});


it("standard Responses aliases require only inference credentials and preserve the standard model catalog", async () => {
  const userId=crypto.randomUUID();
  const bindings={...env,NANOCODEX_INFERENCE_ENABLED:"true",NANOCODEX_ADMIN_USER_ID:userId,AI:{run:async()=>{throw Error("unexpected provider call");}}} as unknown as InferenceApiEnv;
  const principal={kind:"api_key",userId,organizationId:crypto.randomUUID(),teamId:crypto.randomUUID(),role:"owner",subjectId:`user:${userId}`,credentialId:"synthetic",authorizationEpoch:1,capabilities:["api_keys:read","api_keys:write"]} as const;
  const issuedRequest=new Request("https://nanocodex.example/v1/inference/keys",{method:"POST",headers:{"content-type":"application/json"},body:"{}"});
  const issued=await (await routeInferenceApi(issuedRequest,bindings,new URL(issuedRequest.url),principal))!.json<{api_key:string}>();
  const modelsRequest=new Request("https://nanocodex.example/v1/models",{headers:{authorization:`Bearer ${issued.api_key}`}});
  const models=await routeInferenceApi(modelsRequest,bindings,new URL(modelsRequest.url));
  expect(models?.status).toBe(200);
  expect(await models?.json()).toMatchObject({object:"list",data:expect.arrayContaining([expect.objectContaining({object:"model",owned_by:"workers_ai",created:0})])});
  const responseRequest=new Request("https://nanocodex.example/v1/responses",{method:"POST",headers:{authorization:`Bearer ${issued.api_key}`,"content-type":"application/json"},body:JSON.stringify({model:"auto",input:"hello",tools:[{type:"web_search"}]})});
  const response=await routeInferenceApi(responseRequest,bindings,new URL(responseRequest.url));
  expect(response?.status).toBe(400);
  expect(await response?.json()).toMatchObject({error:{code:"invalid_inference_request"}});
  const fullAccountRequest=new Request("https://nanocodex.example/v1/responses",{method:"POST",headers:{authorization:"Bearer ncx_live_synthetic"},body:"{}"});
  expect((await routeInferenceApi(fullAccountRequest,bindings,new URL(fullAccountRequest.url)))?.status).toBe(401);
});
