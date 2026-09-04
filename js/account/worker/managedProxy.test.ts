import assert from "node:assert/strict";
import test from "node:test";

import { isManagedRoutePath, routeManaged } from "./managedProxy.ts";

test("the account Worker exposes only the exact managed wallet routes", () => {
  for (const path of [
    "/v1/wallet",
    "/v1/wallet/balance",
    "/v1/wallet/connect",
    "/v1/wallet/revoke-access-key",
  ]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }

  for (const path of [
    "/v1/wallet/",
    "/v1/wallet/export",
    "/v1/wallet/connect/extra",
    "/v1/wallet/revoke-access-key/extra",
  ]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
});

test("the account Worker projects opaque sandbox preview capabilities", () => {
  assert.equal(isManagedRoutePath("/sandbox-preview/capability/"), true);
  assert.equal(isManagedRoutePath("/sandbox-preview/capability/assets/app.js"), true);
  assert.equal(isManagedRoutePath("/sandbox-preview/"), false);
});

test("the removed model capabilities route is not projected", () => {
  assert.equal(isManagedRoutePath("/v1/model-capabilities"), false);
});

test("the account hand WebSocket stays on the managed service boundary", async () => {
  assert.equal(isManagedRoutePath("/v1/account/tool-host"), true);
  const request = new Request("https://nanocodex.localhost/v1/account/tool-host", {
    headers: { upgrade: "websocket" },
  });
  let forwarded: Request | undefined;
  const response = await routeManaged(request, {
    NANOCODEX_BACKEND: {
      fetch(candidate: Request) {
        forwarded = candidate;
        return Promise.resolve(new Response(null, { status: 204 }));
      },
      connect() { throw new Error("unused"); },
    },
  }, new URL(request.url));

  assert.equal(response?.status, 204);
  assert.equal(forwarded, request);
});

test("VM host WebSockets stay on their exact managed service boundaries", () => {
  for (const path of [
    "/v1/account/vm-host",
    "/v1/agents/agent-1/vm-host",
    "/v1/system/vm-host",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host`,
  ]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }

  for (const path of [
    "/v1/account/vm-host/",
    "/v1/system/vm-host/",
    "/v1/system/vm-host/extra",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host/extra`,
  ]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
});
