import assert from "node:assert/strict";
import test from "node:test";
import { isManagedRoutePath, routeManaged } from "./managedProxy.ts";

test("private one-off account operations are not public proxy routes", async () => {
  for (const operation of ["conversation-project-migration", "private-maintenance"]) {
    for (const suffix of ["", "-20000101", "/", "/other"]) {
      const path = `/v1/account/${operation}${suffix}`;
      assert.equal(isManagedRoutePath(path), false);
      for (const method of ["GET", "POST"]) {
        const request = new Request(`https://example.com${path}`, { method });
        const response = await routeManaged(request, {
          NANOCODEX_BACKEND: { fetch: async () => assert.fail("private route forwarded") } as unknown as Fetcher,
        }, new URL(request.url));
        assert.equal(response, undefined);
      }
    }
  }
});
