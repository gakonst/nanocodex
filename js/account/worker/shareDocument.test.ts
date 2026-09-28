import assert from "node:assert/strict";
import test from "node:test";
import { routeLinkPreview } from "./linkPreview.ts";

const agentId = "00000000-0000-4000-8000-000000000001";
const document = "<!doctype html><html><head></head><body><main id=\"root\"></main></body></html>";
const env = { ASSETS: { async fetch(request: Request) {
  assert.equal(new URL(request.url).pathname, "/");
  return new Response(document, { headers: { "content-type": "text/html" } });
} } as Fetcher };

test("a copied share link navigates directly to the website app without disclosing its fragment", async () => {
  const url = new URL(`https://nanocodex.example/share/${agentId}#token=nsl_${"x".repeat(43)}`);
  const response = await routeLinkPreview(new Request(url, { headers: { accept: "text/html" } }), env, url);
  assert.equal(response?.status, 200);
  const html = await response?.text() ?? "";
  assert.match(html, /id="root"/);
  assert.ok(!html.includes("nsl_"));
  assert.ok(!response?.headers.get("location"));
});

test("an invalid share path does not serve the guest application", async () => {
  const url = new URL("https://nanocodex.example/share/not-a-thread");
  const response = await routeLinkPreview(new Request(url, { headers: { accept: "text/html" } }), env, url);
  assert.notEqual(response?.status, 200);
});
