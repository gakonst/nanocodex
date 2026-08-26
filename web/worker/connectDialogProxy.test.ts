import assert from "node:assert/strict";
import test from "node:test";

import { isConnectDialogPath, routeConnectDialog } from "./connectDialogProxy.ts";
import worker from "./index.ts";

test("matches only the canonical Connect dialog route tree", () => {
  assert.equal(isConnectDialogPath("/connect-dialog"), true);
  assert.equal(isConnectDialogPath("/connect-dialog/"), true);
  assert.equal(isConnectDialogPath("/connect-dialog/assets/app.js"), true);
  assert.equal(isConnectDialogPath("/connect-dialogue"), false);
  assert.equal(isConnectDialogPath("/connect-dialog-old/index.html"), false);
});

test("the main Worker dispatches the canonical route to the dialog binding", async () => {
  const forwarded: Request[] = [];
  const response = await worker.fetch(new Request(
    "https://nanocodex.test/connect-dialog/assets/dialog.js?version=1",
  ), {
    ENVIRONMENT: "production",
    NANOCODEX_CONNECT_DIALOG: {
      async fetch(request: Request) {
        forwarded.push(request);
        return new Response("asset", { headers: { "content-type": "text/javascript" } });
      },
    },
  } as never);

  assert.equal(response.status, 200);
  assert.equal(await response.text(), "asset");
  assert.equal(forwarded[0]?.url, "https://nanocodex.test/assets/dialog.js?version=1");
});

test("strips the public prefix without exposing canonical credentials", async () => {
  const forwarded: Request[] = [];
  const env = {
    NANOCODEX_CONNECT_DIALOG: {
      async fetch(request: Request) {
        forwarded.push(request);
        return new Response("dialog", {
          headers: {
            "content-type": "text/html",
            "set-cookie": "should-not-reach-the-canonical-origin=1",
          },
        });
      },
    },
  };
  const request = new Request("https://nanocodex.test/connect-dialog/assets/app.js?v=7", {
    headers: {
      authorization: "Bearer private",
      cookie: "nanocodex_account=session",
      "if-none-match": '"asset"',
    },
  });
  const response = await routeConnectDialog(request, env as never, new URL(request.url));

  assert.equal(response?.status, 200);
  assert.equal(await response?.text(), "dialog");
  assert.equal(response?.headers.get("set-cookie"), null);
  assert.equal(response?.headers.get("x-content-type-options"), "nosniff");
  assert.equal(
    response?.headers.get("content-security-policy"),
    "frame-ancestors 'self' http://nanocodex.localhost:* http://*.nanocodex.localhost:* https://nanocodex-connect-playground.gakonst.workers.dev chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle",
  );
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0]?.url, "https://nanocodex.test/assets/app.js?v=7");
  assert.equal(forwarded[0]?.headers.get("authorization"), null);
  assert.equal(forwarded[0]?.headers.get("cookie"), null);
  assert.equal(forwarded[0]?.headers.get("if-none-match"), '"asset"');

  const root = new Request("https://nanocodex.test/connect-dialog?request=kept");
  await routeConnectDialog(root, env as never, new URL(root.url));
  assert.equal(forwarded[1]?.url, "https://nanocodex.test/?request=kept");
});

test("rebases same-origin redirects and rejects unexpected external redirects", async () => {
  const request = new Request("https://nanocodex.test/connect-dialog/nested");
  const redirected = await routeConnectDialog(request, {
    NANOCODEX_CONNECT_DIALOG: {
      fetch: async () => new Response(null, {
        headers: { location: "/login?from=dialog" },
        status: 302,
      }),
    } as never,
  }, new URL(request.url));
  assert.equal(
    redirected?.headers.get("location"),
    "https://nanocodex.test/connect-dialog/login?from=dialog",
  );

  const external = await routeConnectDialog(request, {
    NANOCODEX_CONNECT_DIALOG: {
      fetch: async () => new Response(null, {
        headers: { location: "https://unexpected.test/" },
        status: 302,
      }),
    } as never,
  }, new URL(request.url));
  assert.equal(external?.status, 502);
  assert.deepEqual(await external?.json(), { error: "connect_dialog_invalid_redirect" });
});

test("rejects mutations and returns hardened unavailable responses", async () => {
  const request = new Request("https://nanocodex.test/connect-dialog", { method: "POST" });
  const rejected = await routeConnectDialog(request, {}, new URL(request.url));
  assert.equal(rejected?.status, 405);
  assert.equal(rejected?.headers.get("allow"), "GET, HEAD");
  assert.equal(rejected?.headers.get("cache-control"), "no-store");

  const get = new Request(request.url);
  const missing = await routeConnectDialog(get, {}, new URL(get.url));
  assert.equal(missing?.status, 503);
  assert.deepEqual(await missing?.json(), { error: "connect_dialog_unavailable" });

  const originalError = console.error;
  console.error = () => {};
  try {
    const failed = await routeConnectDialog(get, {
      NANOCODEX_CONNECT_DIALOG: {
        fetch: async () => { throw new Error("offline"); },
      } as never,
    }, new URL(get.url));
    assert.equal(failed?.status, 503);
  } finally {
    console.error = originalError;
  }
});
