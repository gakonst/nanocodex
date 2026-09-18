import assert from "node:assert/strict";
import test from "node:test";
import { isManagedRoutePath, routeManaged } from "./managedProxy.ts";
test("one-time migration forwards only the exact route with authentication intact", async () => {
  const path = "/v1/account/conversation-project-migration-20260918";
  assert.equal(isManagedRoutePath(path), true);
  for (const suffix of ["/", "/other", "-other"]) assert.equal(isManagedRoutePath(path + suffix), false);
  const request = new Request("https://example.com" + path, {method:"POST",headers:{authorization:"Bearer test"},body:"{}"});
  const response = await routeManaged(request,{NANOCODEX_BACKEND:{fetch:async (forwarded:Request)=> {
    assert.equal(forwarded,request);
    return Response.json({error:"forbidden"},{status:403});
  }} as Fetcher},new URL(request.url));
  assert.equal(response?.status,403);
});
