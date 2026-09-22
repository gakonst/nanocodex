import assert from "node:assert/strict";
import test from "node:test";
import { isManagedRoutePath, routeManaged } from "./managedProxy.ts";

test("admin requests preserve backend authentication and deny expanded route paths", async () => {
  assert.equal(isManagedRoutePath("/v1/account/admin"), true);
  for (const path of ["/v1/account/admin/", "/v1/account/admin/users", "/v1/account/admin-other"]) {
    assert.equal(isManagedRoutePath(path), false);
  }
  const request = new Request("https://example.com/v1/account/admin");
  const response = await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch: async (forwarded: Request) => {
      assert.equal(forwarded, request);
      return Response.json({ error: "forbidden" }, { status: 403, headers: { "cache-control": "no-store" } });
    },
  } as Fetcher }, new URL(request.url));
  assert.equal(response?.status, 403);
  assert.equal(response?.headers.get("cache-control"), "no-store");
});
