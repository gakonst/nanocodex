import assert from "node:assert/strict";
import test from "node:test";
import { routeLinkPreview } from "./linkPreview.ts";
import { agentIdFromPath, legacyRedirectPath, pathForAgent, pathForSurface, surfaceFromUrl } from "../src/navigation.ts";

const agentId = "00000000-0000-4000-8000-000000000001";
const document = "<!doctype html><html><head></head><body><main id=\"root\"></main></body></html>";
const env = { ASSETS: { async fetch() {
  return new Response(document, { headers: { "content-type": "text/html" } });
} } as unknown as Fetcher };

function navigate(path: string) {
  const url = new URL(`https://nanocodex.example${path}`);
  return routeLinkPreview(new Request(url, { headers: { accept: "text/html" } }), env, url);
}

test("canonical home, account and agents paths serve the app shell", async () => {
  for (const path of ["/", "/account", "/account/vault", "/account/wallet", "/account/access", "/agents", `/agents/${agentId}`]) {
    const response = await navigate(path);
    assert.equal(response?.status, 200, path);
    assert.match(await response!.text(), /id="root"/, path);
  }
});

test("canonical surfaces resolve to the expected app", () => {
  const surface = (path: string) => surfaceFromUrl(new URL(`https://nanocodex.example${path}`));
  assert.equal(surface("/"), "home");
  assert.equal(surface("/account"), "connect");
  assert.equal(surface("/account/vault"), "connect");
  assert.equal(surface("/agents"), "agent");
  assert.equal(surface(`/agents/${agentId}`), "agent");
  assert.equal(surface("/agent?demo=attached-tools"), "tools");
  assert.equal(pathForSurface("home"), "/");
  assert.equal(pathForSurface("connect"), "/account");
  assert.equal(pathForSurface("agent"), "/agents");
  assert.equal(pathForAgent(agentId), `/agents/${agentId}`);
  assert.equal(agentIdFromPath(`/agents/${agentId}`), agentId);
  assert.equal(agentIdFromPath(`/agent/${agentId}`), agentId);
});

test("legacy paths redirect to canonical routes preserving query", async () => {
  const cases: Array<[string, string]> = [
    ["/agent", "/agents"],
    [`/agent/${agentId}`, `/agents/${agentId}`],
    ["/connect", "/account"],
    ["/connect?connect=github", "/account?connect=github"],
    ["/connect?connector_result=ok", "/account?connector_result=ok"],
    ["/connect/vault", "/account/vault"],
    ["/connect/wallet", "/account/wallet"],
    ["/connect/access", "/account/access"],
  ];
  for (const [from, to] of cases) {
    const response = await navigate(from);
    assert.equal(response?.status, 302, from);
    assert.equal(response?.headers.get("location"), to, from);
    assert.equal(response?.headers.get("cache-control"), "no-store");
  }
  assert.equal(legacyRedirectPath(new URL("https://x/connect/vault?a=1#h")), "/account/vault?a=1#h");
});

test("device auth, hosted services and the tools demo keep their URLs", async () => {
  for (const path of ["/connect?user_code=ABCD-EFGH", "/connect/device", "/vault", "/services/phone", "/agent?demo=attached-tools"]) {
    assert.equal(legacyRedirectPath(new URL(`https://x${path}`)), null, path);
    const response = await navigate(path);
    assert.equal(response?.status, 200, path);
  }
});
