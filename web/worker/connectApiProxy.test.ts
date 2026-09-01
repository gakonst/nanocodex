import assert from "node:assert/strict";
import test from "node:test";

import { isConnectApiRequest, routeConnectApi } from "./connectApiProxy.ts";

test("local Connect routing owns device, auth, and authenticated onboarding routes", () => {
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/device/register"), "/v1/device/register"), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/connect/auth/challenge"), "/v1/connect/auth/challenge"), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/hosted-authorizations", {
    method: "POST",
  }), "/v1/hosted-authorizations"), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/connectors", {
    headers: {
      authorization: "Bearer connect-session",
      "x-nanocodex-connect-client": "onboarding",
    },
  }), "/v1/connectors"), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/connectors/chatgpt", {
    method: "POST",
    headers: {
      authorization: "Bearer connect-session",
      "x-nanocodex-connect-client": "device",
    },
  }), "/v1/connectors/chatgpt"), true);
  const mcpId = "a".repeat(43);
  assert.equal(isConnectApiRequest(new Request(`http://nanocodex.localhost:5173/v1/mcp-connections/${mcpId}`, {
    method: "POST",
    headers: {
      authorization: "Bearer connect-session",
      "x-nanocodex-connect-client": "device",
    },
  }), `/v1/mcp-connections/${mcpId}`), true);
  assert.equal(isConnectApiRequest(
    new Request(`http://nanocodex.localhost:5173/v1/mcp-connections/${mcpId}/callback`),
    `/v1/mcp-connections/${mcpId}/callback`,
  ), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/connectors", {
    headers: { authorization: "Bearer connect-session" },
  }), "/v1/connectors"), false);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/connectors"), "/v1/connectors"), false);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/history/sessions/search", {
    headers: { "x-nanocodex-app-id": "nanocodex-cli" },
  }), "/v1/history/sessions/search"), true);
  assert.equal(isConnectApiRequest(new Request("http://nanocodex.localhost:5173/v1/history/sessions/search"), "/v1/history/sessions/search"), false);
  assert.equal(isConnectApiRequest(
    new Request("http://nanocodex.localhost:5173/v1/connectors/github/callback"),
    "/v1/connectors/github/callback",
  ), false);
  assert.equal(isConnectApiRequest(
    new Request(`http://nanocodex.localhost:5173/v1/connectors/github/callback?state=connect.${"a".repeat(43)}`),
    "/v1/connectors/github/callback",
  ), true);
  assert.equal(isConnectApiRequest(
    new Request(`http://nanocodex.localhost:5173/v1/connectors/slack/callback?state=connect.${"a".repeat(43)}`),
    "/v1/connectors/slack/callback",
  ), true);
});

test("local Connect proxy preserves the canonical HTTPS request origin", async () => {
  let forwarded: Request | undefined;
  const response = await routeConnectApi(
    new Request("http://nanocodex.localhost:5173/v1/device/verify?user_code=ABCDWXYZ", {
      headers: { origin: "http://nanocodex.localhost:5173" },
    }),
    {
      ENVIRONMENT: "development",
      NANOCODEX_CONNECT_API: {
        async fetch(request) {
          forwarded = request;
          return Response.json({ ok: true });
        },
      },
    },
    new URL("http://nanocodex.localhost:5173/v1/device/verify?user_code=ABCDWXYZ"),
  );
  assert.equal(response?.status, 200);
  assert.equal(forwarded?.url, "http://nanocodex.localhost:5173/v1/device/verify?user_code=ABCDWXYZ");
  assert.equal(forwarded?.headers.get("origin"), "http://nanocodex.localhost:5173");
});

test("local Connect readiness maps to the auxiliary Worker health route", async () => {
  let pathname = "";
  const response = await routeConnectApi(
    new Request("http://nanocodex.localhost:5173/api/connect/health"),
    { ENVIRONMENT: "development", NANOCODEX_CONNECT_API: { async fetch(request) {
      pathname = new URL(request.url).pathname;
      return Response.json({ mode: "live", status: "ok" });
    } } },
    new URL("http://nanocodex.localhost:5173/api/connect/health"),
  );
  assert.equal(pathname, "/healthz");
  assert.deepEqual(await response?.json(), { mode: "live", status: "ok" });
});

test("production projects Connect through the canonical Nanocodex origin", async () => {
  let called = false;
  const response = await routeConnectApi(
    new Request("https://nanocodex.example/v1/device/register"),
    {
      ENVIRONMENT: "production",
      NANOCODEX_CONNECT_API: { async fetch() {
        called = true;
        return new Response();
      } },
    },
    new URL("https://nanocodex.example/v1/device/register"),
  );
  assert.equal(response?.status, 200);
  assert.equal(called, true);
});
