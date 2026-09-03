import assert from "node:assert/strict";
import test from "node:test";

import { isManagedRoutePath } from "./managedProxy.ts";

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
