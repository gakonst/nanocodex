import assert from "node:assert/strict";
import test from "node:test";
import { createElevenLabsManager } from "../index.mjs";

test("ElevenLabs management paginates and sends credentials only in authenticated PUT", async () => {
  const calls = [];
  const manager = createElevenLabsManager({ fetch: async (url, init) => {
    calls.push({ url, init });
    return Response.json(init.method === "PUT" ? { configured: true } : url.includes("next_page_token")
      ? { voices: [{ voice_id: "two", name: "Second" }], has_more: false }
      : { voices: [{ voice_id: "one", name: "First" }], has_more: true, next_page_token: "page 2" });
  }});
  assert.deepEqual((await manager.listVoices()).map(v => v.voiceId), ["one", "two"]);
  assert.match(calls[1].url, /next_page_token=page%202/);
  await manager.saveApiKey("synthetic-test-key");
  assert.equal(calls[2].init.credentials, "include");
  assert.equal(calls[2].init.method, "PUT");
  assert.deepEqual(JSON.parse(calls[2].init.body), { api_key: "synthetic-test-key" });
  assert.ok(calls.every(c => !c.url.includes("synthetic-test-key")));
});

test("cloning requires explicit consent and preserves verification requirement", async () => {
  let calls = 0;
  const manager = createElevenLabsManager({ fetch: async (_url, init) => {
    calls++;
    assert.equal(init.body.get("consent"), "true");
    assert.equal(init.body.getAll("files").length, 1);
    return Response.json({ voice_id: "clone", requires_verification: true });
  }});
  await assert.rejects(manager.cloneVoice({ name: "Sample", files: [], consent: false }), /consent/);
  assert.equal(calls, 0);
  const voice = await manager.cloneVoice({ name: "Sample", files: [new File(["audio"], "sample.wav", { type: "audio/wav" })], consent: true });
  assert.equal(voice.requiresVerification, true);
});

test("provider response bodies cannot leak into management errors", async () => {
  const manager = createElevenLabsManager({ fetch: async () => new Response("private provider diagnostic", { status: 401 }) });
  await assert.rejects(manager.listVoices(), error => !error.message.includes("private") && error.message.includes("401"));
});
