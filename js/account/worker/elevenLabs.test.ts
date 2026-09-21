import assert from "node:assert/strict";
import { test } from "node:test";
import { ElevenLabsAccount, routeElevenLabs } from "./elevenLabs.ts";
import { CredentialVault } from "./credentialVault.ts";

const env = { ENVIRONMENT: "production", SESSION_CREDENTIAL_KEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" };
function fixture(provider: (request: Request) => Promise<Response>, id = "synthetic-account", config: Parameters<typeof routeElevenLabs>[1] = env, data = new Map<string, unknown>()) {
  const storage = { get: async (key: string) => data.get(key), put: async (key: string, value: unknown) => { data.set(key, value); }, delete: async (key: string) => data.delete(key) };
  const object = new ElevenLabsAccount({ id: { toString: () => id }, storage } as unknown as DurableObjectState, config, provider);
  const call = (path = "/", method = "GET", body?: unknown) => object.fetch(new Request(`https://elevenlabs.internal${path}`, { method, ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }) }));
  return { data, object, call };
}

test("public route admits persistent browser/native owners, rejects grants, and scopes to account identity", async () => {
  const seen: string[] = [];
  let account: unknown = { authentication: "account_session", user: { persistent: true, id: "synthetic-user" } };
  const routeEnv = { NANOCODEX_BACKEND: { fetch: async (request: Request) => { assert.equal(request.headers.get("cookie"), "session=fixture"); return Response.json(account); } }, ELEVENLABS_ACCOUNTS: { idFromName: (id: string) => { seen.push(id); return id; }, get: () => ({ fetch: async (request: Request) => { assert.equal(request.url, "https://elevenlabs.internal/voices?next_page_token=page2"); assert.equal(request.headers.get("cookie"), null); return Response.json({ voices: [] }); } }) } } as unknown as Parameters<typeof routeElevenLabs>[1];
  const url = new URL("https://app.test/api/voice/elevenlabs/voices?next_page_token=page2");
  const request = () => new Request(url, { headers: { cookie: "session=fixture" } });
  assert.equal((await routeElevenLabs(request(), routeEnv, url))?.status, 200);
  assert.deepEqual(seen, ["synthetic-user"]);
  account = { authentication: "api_key", user: { persistent: true, id: "synthetic-user" } };
  assert.equal((await routeElevenLabs(request(), routeEnv, url))?.status, 200);
  assert.deepEqual(seen, ["synthetic-user", "synthetic-user"]);
  for (const value of [null, { authentication: "connect_grant", user: { persistent: true, id: "synthetic-user" } }, { authentication: "api_key", user: { persistent: false, id: "synthetic-user" } }, { authentication: "account_session", user: { persistent: false, id: "synthetic-user" } }]) {
    account = value;
    assert.equal((await routeElevenLabs(request(), routeEnv, url))?.status, 401);
  }
  assert.equal(seen.length, 2);
  assert.equal((await routeElevenLabs(new Request(url, { headers: { origin: "https://other.test" } }), routeEnv, url))?.status, 403);
});

test("keys are encrypted, account-bound, replace only after validation, and deletable", async () => {
  let valid = true;
  const f = fixture(async request => { assert.equal(request.headers.get("xi-api-key"), valid ? "fixture-secret" : "bad-secret"); return new Response("provider-private-detail", { status: valid ? 200 : 401 }); });
  assert.deepEqual(await (await f.call()).json(), { configured: false });
  assert.equal((await f.call("/", "PUT", { api_key: "fixture-secret" })).status, 200);
  const stored = f.data.get("credential");
  assert.doesNotMatch(JSON.stringify(stored), /fixture-secret/);
  await assert.rejects(new CredentialVault(env, "elevenlabs/another-account").open(stored), /authentication/);
  valid = false;
  const rejected = await f.call("/", "PUT", { api_key: "bad-secret" });
  assert.equal(rejected.status, 403);
  assert.doesNotMatch(await rejected.text(), /bad-secret|provider-private-detail/);
  assert.equal(f.data.get("credential"), stored);
  assert.deepEqual(await (await f.call("/", "DELETE")).json(), { configured: false });
  assert.equal((await f.call("/voices")).status, 409);
});

test("catalog paginates and sanitizes provider metadata", async () => {
  const f = fixture(async request => {
    if (request.url.includes("page_size=1") && !request.url.includes("page_size=100")) return Response.json({});
    assert.equal(new URL(request.url).searchParams.get("next_page_token"), "a&b");
    return Response.json({ voices: [{ voice_id: "voice_1", name: "Demo", preview_url: "javascript:alert(1)", secret: "private" }, { voice_id: "../invalid" }], has_more: true, next_page_token: "page3" });
  });
  await f.call("/", "PUT", { api_key: "fixture-secret" });
  assert.deepEqual(await (await f.call("/voices?next_page_token=a%26b")).json(), { voices: [{ voice_id: "voice_1", name: "Demo", category: null, preview_url: null }], has_more: true, next_page_token: "page3" });
});

test("speech streams exact PCM and MP3 bytes and rejects invalid input before provider call", async () => {
  const calls: Request[] = [];
  const f = fixture(async request => { calls.push(request); return new Response(new Uint8Array([0, 255, 1, 128])); });
  await f.call("/", "PUT", { api_key: "fixture-secret" });
  for (const output_format of ["pcm_24000", "mp3_44100_128"]) {
    const response = await f.call("/speech", "POST", { voice_id: "voice_1", text: "Hello", output_format });
    assert.equal(response.headers.get("content-type"), output_format === "pcm_24000" ? "audio/pcm;rate=24000;channels=1" : "audio/mpeg");
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), new Uint8Array([0, 255, 1, 128]));
    const sent = calls.at(-1)!;
    assert.equal(new URL(sent.url).searchParams.get("output_format"), output_format);
    assert.deepEqual(await sent.json(), { text: "Hello", model_id: "eleven_flash_v2_5" });
  }
  for (const body of [{ voice_id: "../bad", text: "Hello" }, { voice_id: "voice_1", text: " " }, { voice_id: "voice_1", text: "Hello", output_format: "wav" }]) assert.equal((await f.call("/speech", "POST", body)).status, 400);
  assert.equal(calls.length, 3);
});

test("clone requires consent, strips filenames, forwards samples transiently, and preserves verification", async () => {
  let cloneCalls = 0;
  const f = fixture(async request => {
    if (request.method !== "POST") return Response.json({});
    cloneCalls++;
    const sent = await request.formData();
    assert.equal(sent.get("name"), "My voice");
    assert.equal(sent.get("consent"), null);
    const file = sent.get("files") as File;
    assert.equal(file.name, "sample-1.wav");
    assert.equal(await file.text(), "synthetic audio");
    return Response.json({ voice_id: "clone_1", requires_verification: true, private: "secret" });
  });
  await f.call("/", "PUT", { api_key: "fixture-secret" });
  const clone = (consent: string) => {
    const form = new FormData(); form.set("name", " My voice "); form.set("consent", consent); form.append("files", new Blob(["synthetic audio"], { type: "audio/wav" }), "private-name.wav");
    return f.object.fetch(new Request("https://elevenlabs.internal/voices", { method: "POST", body: form }));
  };
  assert.equal((await clone("false")).status, 400);
  assert.equal(cloneCalls, 0);
  const response = await clone("true");
  assert.equal(response.status, 201);
  assert.deepEqual(await response.json(), { voice_id: "clone_1", requires_verification: true });
  assert.deepEqual([...f.data.keys()], ["credential"]);
});

test("oversized requests are rejected before provider requests", async () => {
  const f = fixture(async () => { throw new Error("unexpected provider call"); });
  const response = await f.object.fetch(new Request("https://elevenlabs.internal/", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ api_key: "x".repeat(5000) }) }));
  assert.equal(response.status, 413);
});

const fallbackEnv = {
  ...env,
  ELEVENLABS_API_KEY: "synthetic-deployment-key",
  ELEVENLABS_ACCOUNT_ID: "synthetic-owner",
  ELEVENLABS_ACCOUNTS: { idFromName: (id: string) => ({ toString: () => `object-${id}` }) } as unknown as DurableObjectNamespace,
};

test("deployment credential is available only to its configured owner object", async () => {
  let calls = 0;
  const provider = async (request: Request) => {
    calls++;
    assert.equal(request.headers.get("xi-api-key"), "synthetic-deployment-key");
    return Response.json({ voices: [] });
  };
  const owner = fixture(provider, "object-synthetic-owner", fallbackEnv);
  assert.deepEqual(await (await owner.call()).json(), { configured: true });
  const voices = await owner.call("/voices");
  assert.equal(voices.status, 200);
  assert.doesNotMatch(await voices.text(), /synthetic-deployment-key/);
  assert.equal(owner.data.size, 0);
  for (const [id, config] of [
    ["object-other-account", fallbackEnv],
    ["object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_ACCOUNT_ID: undefined }],
    ["object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_ACCOUNTS: undefined }],
    ["object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_API_KEY: undefined }],
    ["object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_API_KEY: "  " }],
    ["object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_API_KEY: "invalid\nkey" }],
    ["object-synthetic-owner", env],
  ] as const) {
    const f = fixture(provider, id, config);
    assert.deepEqual(await (await f.call()).json(), { configured: false });
    assert.equal((await f.call("/voices")).status, 409);
  }
  assert.equal(calls, 1);
});

test("stored credential wins; disconnect survives restart and secret rotation until explicit reconnect", async () => {
  const keys: Array<string | null> = [];
  const provider = async (request: Request) => {
    keys.push(request.headers.get("xi-api-key"));
    return Response.json({ voices: [] });
  };
  const f = fixture(provider, "object-synthetic-owner", fallbackEnv);
  await f.call("/", "PUT", { api_key: "synthetic-own-key" });
  await f.call("/voices");
  assert.deepEqual(keys, ["synthetic-own-key", "synthetic-own-key"]);
  assert.deepEqual(await (await f.call("/", "DELETE")).json(), { configured: false });
  const restarted = fixture(provider, "object-synthetic-owner", { ...fallbackEnv, ELEVENLABS_API_KEY: "rotated-key" }, f.data);
  assert.deepEqual(await (await restarted.call()).json(), { configured: false });
  assert.equal((await restarted.call("/voices")).status, 409);
  assert.equal(keys.length, 2);
  const rejected = fixture(async () => new Response(null, { status: 401 }), "object-synthetic-owner", fallbackEnv, f.data);
  assert.equal((await rejected.call("/", "PUT", { api_key: "bad-key" })).status, 403);
  assert.deepEqual(await (await rejected.call()).json(), { configured: false });
  assert.equal((await restarted.call("/", "PUT", { api_key: "reconnected-key" })).status, 200);
  assert.equal(f.data.has("fallbackDisabled"), false);
  await restarted.call("/voices");
  assert.equal(keys.at(-1), "reconnected-key");
});

test("forged account headers cannot select the deployment owner or reach the object", async () => {
  let calls = 0;
  const other = fixture(async () => { calls++; return Response.json({ voices: [] }); }, "object-synthetic-other", fallbackEnv);
  const routeEnv = {
    ...fallbackEnv,
    NANOCODEX_BACKEND: { fetch: async (request: Request) => {
      assert.equal(request.headers.get("x-account-id"), null);
      return Response.json({ authentication: "account_session", user: { persistent: true, id: "synthetic-other" } });
    } },
    ELEVENLABS_ACCOUNTS: {
      idFromName: (id: string) => { assert.equal(id, "synthetic-other"); return id; },
      get: () => ({ fetch: async (request: Request) => {
        assert.deepEqual([...request.headers], []);
        return other.object.fetch(request);
      } }),
    } as unknown as DurableObjectNamespace,
  };
  const url = new URL("https://app.test/api/voice/elevenlabs/voices?account_id=synthetic-owner");
  const response = await routeElevenLabs(new Request(url, { headers: {
    "x-account-id": "synthetic-owner", "x-elevenlabs-account-id": "synthetic-owner",
    "x-nanocodex-account-id": "synthetic-owner", "xi-api-key": "synthetic-deployment-key",
  } }), routeEnv, url);
  assert.equal(response?.status, 409);
  assert.equal(calls, 0);
});


test("default provider fetch preserves the Workers global receiver", async () => {
  const originalFetch = globalThis.fetch;
  let calls = 0;
  globalThis.fetch = async function (this: unknown, input: RequestInfo | URL) {
    assert.equal(this, undefined);
    assert.equal(new Request(input).headers.get("xi-api-key"), "fixture-secret");
    calls++;
    return Response.json({ voices: [] });
  };
  try {
    const object = new ElevenLabsAccount({ id: { toString: () => "fixture" }, storage: { get: async () => undefined } } as unknown as DurableObjectState, {
      ...env, ELEVENLABS_API_KEY: "fixture-secret", ELEVENLABS_ACCOUNT_ID: "fixture",
      ELEVENLABS_ACCOUNTS: { idFromName: () => "fixture" } as unknown as DurableObjectNamespace,
    });
    const response = await object.fetch(new Request("https://elevenlabs.internal/voices"));
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { voices: [], has_more: false, next_page_token: null });
    assert.equal(calls, 1);
  } finally { globalThis.fetch = originalFetch; }
});


test("provider redirects are not followed or exposed", async () => {
  const f = fixture(async request => {
    assert.equal(request.redirect, "manual");
    return new Response(null, { status: 302, headers: { location: "https://other.test/private" } });
  });
  const response = await f.call("/", "PUT", { api_key: "fixture-secret" });
  assert.equal(response.status, 502);
  assert.equal(response.headers.get("location"), null);
  assert.deepEqual(await response.json(), { error: "elevenlabs_unavailable" });
  assert.equal(f.data.has("credential"), false);
});
