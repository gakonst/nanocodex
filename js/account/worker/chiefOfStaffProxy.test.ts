import assert from "node:assert/strict";
import test from "node:test";
import { routeChiefOfStaff } from "./chiefOfStaffProxy.ts";

test("the account route preserves browser identity and targets only readiness", async () => {
  let forwarded: Request | undefined;
  const request = new Request("https://nanocodex.example/api/chief-of-staff/status", {
    headers: { cookie: "session=opaque", origin: "https://nanocodex.example" },
  });
  const response = await routeChiefOfStaff(request, {
    CHIEF_OF_STAFF: {
      async fetch(inner) {
        forwarded = inner;
        return Response.json({ configured: false });
      },
    },
  }, new URL(request.url));

  assert.equal(response?.status, 200);
  assert.equal(new URL(forwarded!.url).pathname, "/v1/readiness");
  assert.equal(forwarded!.headers.get("cookie"), "session=opaque");
  assert.equal(forwarded!.headers.get("origin"), "https://nanocodex.example");
});

test("the account route fences methods and missing bindings", async () => {
  const endpoint = "https://nanocodex.example/api/chief-of-staff/status";
  const post = await routeChiefOfStaff(
    new Request(endpoint, { method: "POST" }),
    {},
    new URL(endpoint),
  );
  const unavailable = await routeChiefOfStaff(new Request(endpoint), {}, new URL(endpoint));

  assert.equal(post?.status, 405);
  assert.equal(unavailable?.status, 503);
});
