import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { test } from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const account = "AC" + "1".repeat(32), token = "synthetic-phone-provider-secret";
const output = new URL(`../../../output/phone-provider/${Date.now()}/`, import.meta.url);
test("private phone provider restricts operations and validates Twilio signatures over real service transport", async () => {
  const compiled = await build({ stdin: { contents: `export {PhoneProvider} from './src/phone-provider.ts';export default {fetch(){return new Response(null,{status:404})}}`,
    resolveDir: fileURLToPath(new URL("..", import.meta.url)) }, bundle: true, write: false, format: "esm", platform: "node", external: ["cloudflare:*", "node:*"], target: "es2022" });
  const dispatched = [], trace = [];
  const mf = new Miniflare({ port: 0, workers: [
    { name: "gateway", modules: true, script: `export default {fetch(r,e){const u=new URL(r.url);return e.PROVIDER.fetch(new Request('https://phone-provider.internal'+u.pathname,r))}}`,
      compatibilityDate: "2026-07-30", serviceBindings: { PROVIDER: { name: "provider", entrypoint: "PhoneProvider" } } },
    { name: "provider", modules: [{ type: "ESModule", path: "provider.mjs", contents: compiled.outputFiles[0].text }],
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"], bindings: { TWILIO_ACCOUNT_SID: account, TWILIO_AUTH_TOKEN: token },
      outboundService: async request => {
        const url = new URL(request.url);
        dispatched.push({ origin:url.origin,path:url.pathname,method:request.method,credential_match:request.headers.get("authorization") === "Basic " + Buffer.from(account + ":" + token).toString("base64") });
        assert.ok(["https://api.twilio.com", "https://pricing.twilio.com"].includes(url.origin));
        assert.equal(request.headers.get("authorization"), "Basic " + Buffer.from(account + ":" + token).toString("base64"));
        return Response.json({ sid: "PN" + "2".repeat(32), phone_number: "+12025550123" }, { status: request.method === "POST" ? 201 : 200 });
      } },
  ] });
  try {
    const base = await mf.ready;
    async function call(path, body, expected = 200) {
      const response = await fetch(new URL(path, base), { method: body ? "POST" : "GET", ...(body ? { headers: { "content-type": "application/json" }, body: JSON.stringify(body) } : {}) });
      const raw = await response.text(); assert.ok(!raw.includes(token));
      const value = JSON.parse(raw); trace.push({ path, expected, observed: response.status, result: value });
      assert.equal(response.status, expected, raw); return value;
    }
    assert.equal((await call("/status")).configured, true);
    const input = { path: "/2010-04-01/Accounts/{account}/IncomingPhoneNumbers.json", method: "POST", body: "PhoneNumber=%2B12025550123" };
    await call("/request", input, 201);
    await call("/request", { ...input, path: "/2010-04-01/Accounts/AC" + "3".repeat(32) + "/IncomingPhoneNumbers.json" }, 403);
    await call("/request", { ...input, path: "//attacker.example/exfiltrate" }, 403);
    await call("/request", { ...input, path: "/2010-04-01/Accounts/{account}/Messages.json" }, 403);
    await call("/request", { ...input, path: "/2010-04-01/Accounts/{account}/Calls.json" }, 403);
    assert.equal(dispatched.length, 1);
    const url = "https://account.example/v1/services/phone/webhook", fields = new URLSearchParams({ AccountSid: account, From: "+12025550124", To: "+12025550123", Body: "Synthetic code 123456" });
    const signature = createHmac("sha1", token).update(url + [...fields.keys()].sort().map(key => key + fields.get(key)).join("")).digest("base64");
    assert.deepEqual(await call("/verify", { url, body: fields.toString(), signature }), { valid: true });
    assert.deepEqual(await call("/verify", { url: url + "?tampered=1", body: fields.toString(), signature }), { valid: false });
    assert.deepEqual(await call("/verify", { url, body: fields.toString() + "x", signature }), { valid: false });
    const provider = await mf.getWorker("provider");
    assert.equal((await provider.fetch("https://phone-provider.internal/status")).status, 404, "only named service binding exposes provider");
  } finally {
    await mf.dispose(); await mkdir(output, { recursive: true });
    await writeFile(new URL("trace.json", output), JSON.stringify({ trace, dispatched }, null, 2) + "\n");
  }
});
