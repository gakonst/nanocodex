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

test("interactive screen routes retain their exact managed boundary", () => {
  for (const suffix of ["", "/screens", "/host", "/view", "/renew", "/ice"]) {
    assert.equal(isManagedRoutePath("/v1/account/hands" + suffix), true);
  }
  for (const suffix of ["/", "/host/extra", "/input", "/command", "-other"]) {
    assert.equal(isManagedRoutePath("/v1/account/hands" + suffix), false);
  }
});

test("VM host WebSockets stay on their exact managed service boundaries", () => {
  for (const path of [
    "/v1/account/vm-host",
    "/v1/agents/agent-1/vm-host",
    "/v1/system/vm-host",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host`,
    ...["host", "ice", "renew"].map(endpoint => `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/hands/${endpoint}`),
  ]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }

  for (const path of [
    "/v1/account/vm-host/",
    "/v1/system/vm-host/",
    "/v1/system/vm-host/extra",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host/extra`,
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/hands/view`,
  ]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
});


test("account Hand discovery uses only the exact managed route", async () => {
  assert.equal(isManagedRoutePath("/v1/account/hands"), true);
  assert.equal(isManagedRoutePath("/v1/account/hands/"), false);
  assert.equal(isManagedRoutePath("/v1/account/hands/other"), false);
  const request = new Request("https://nanocodex.localhost/v1/account/hands", { headers: { authorization: "Bearer test" } });
  let forwarded: Request | undefined;
  const response = await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch(candidate: Request) { forwarded = candidate; return Promise.resolve(Response.json({ data: [] })); },
    connect() { throw new Error("unused"); },
  } }, new URL(request.url));
  assert.equal(forwarded, request);
  assert.deepEqual(await response?.json(), { data: [] });
});


test("server Hand enrollment and scoped publishers retain their managed boundary", async () => {
  const owner = "11111111-1111-4111-8111-111111111111";
  const id = "22222222-2222-4222-8222-222222222222";
  const management = `/v1/account/hand-hosts/${id}`;
  const publisher = `/v1/hand-hosts/${owner}/${id}/hands`;
  for (const path of ["/v1/account/hand-hosts", management, ...["host", "ice", "renew"].map(endpoint => `${publisher}/${endpoint}`)]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }
  for (const path of ["/v1/account/hand-hosts/", `${management}/extra`, "/v1/account/hand-hosts/invalid", `${publisher}/view`, `${publisher}/host/extra`, `${publisher}/`, `/v1/hand-hosts/${owner}/invalid/hands/host`]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
  for (const request of [
    new Request(`https://nanocodex.localhost${management}`, { method: "PUT", headers: { authorization: "Bearer account-test", origin: "https://nanocodex.localhost", "content-type": "application/json" }, body: JSON.stringify({ name: "SSH fixture" }) }),
    new Request(`https://nanocodex.localhost${publisher}/host`, { headers: { authorization: "Bearer publisher-test", upgrade: "websocket" } }),
    new Request(`https://nanocodex.localhost${publisher}/ice`, { method: "POST", headers: { authorization: "Bearer publisher-test" } }),
  ]) {
    let forwarded: Request | undefined;
    const response = await routeManaged(request, { NANOCODEX_BACKEND: {
      fetch(candidate: Request) { forwarded = candidate; return Promise.resolve(new Response(null, { status: 204 })); },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(response?.status, 204);
    assert.equal(forwarded, request);
  }
});
