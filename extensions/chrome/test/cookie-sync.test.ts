import assert from "node:assert/strict";
import test from "node:test";
import {
  assertCookieJarFence,
  cookieRemovalDetails,
  cookieSetDetails,
  createCookieJar,
  validateCookieJar,
} from "../lib/cookie-sync.ts";
import { createAuthenticatedCookieSyncTransport } from "../lib/cookie-sync-client.ts";
const fence = {
  origin: "https://app.example",
  profile_id: "4d82df92-c273-410b-9dd3-f2e97377990c",
  store_id: "0",
};

const persistentCookie = {
  name: "session",
  value: "secret-cookie-value",
  domain: ".app.example",
  path: "/account",
  hostOnly: false,
  secure: true,
  httpOnly: true,
  sameSite: "no_restriction",
  session: false,
  expirationDate: 2_000_000_000,
  storeId: "0",
  partitionKey: {
    topLevelSite: "https://top.example",
    hasCrossSiteAncestor: true,
  },
} as const;

test("preserves the complete supported Chrome cookie shape and exact ownership fence", () => {
  const jar = createCookieJar(fence, [persistentCookie, {
    name: "host",
    value: "value",
    domain: "app.example",
    path: "/",
    hostOnly: true,
    secure: true,
    httpOnly: false,
    sameSite: "lax",
    session: true,
    storeId: "0",
  }]);
  assert.deepEqual(jar, {
    schema_version: 1,
    ...fence,
    revision: 0,
    cookies: [persistentCookie, {
      name: "host",
      value: "value",
      domain: "app.example",
      path: "/",
      hostOnly: true,
      secure: true,
      httpOnly: false,
      sameSite: "lax",
      session: true,
      storeId: "0",
    }],
  });
  assert.doesNotThrow(() => assertCookieJarFence(jar, fence));
  assert.throws(() => assertCookieJarFence(jar, { ...fence, origin: "https://other.example" }), /exact site/);
  assert.throws(() => assertCookieJarFence(jar, { ...fence, profile_id: "other-profile" }), /browser profile/);
  assert.throws(() => assertCookieJarFence(jar, { ...fence, store_id: "1" }), /browser profile/);
});

test("builds lossless set and partition-aware remove details", () => {
  assert.deepEqual(cookieSetDetails(persistentCookie, fence.origin), {
    url: "https://app.example/account",
    name: persistentCookie.name,
    value: persistentCookie.value,
    domain: persistentCookie.domain,
    path: persistentCookie.path,
    secure: true,
    httpOnly: true,
    sameSite: "no_restriction",
    expirationDate: persistentCookie.expirationDate,
    storeId: "0",
    partitionKey: persistentCookie.partitionKey,
  });
  assert.deepEqual(cookieRemovalDetails(persistentCookie, fence.origin), {
    url: "https://app.example/account",
    name: persistentCookie.name,
    storeId: "0",
    partitionKey: persistentCookie.partitionKey,
  });
  const hostOnly = createCookieJar(fence, [{
    ...persistentCookie,
    domain: "app.example",
    hostOnly: true,
    session: true,
    expirationDate: undefined,
    partitionKey: undefined,
  }]).cookies[0]!;
  const details = cookieSetDetails(hostOnly, fence.origin);
  assert.equal("domain" in details, false);
  assert.equal("expirationDate" in details, false);
  assert.equal("partitionKey" in details, false);
});

test("rejects cross-origin, cross-store, inconsistent session, and unsupported partition data", () => {
  assert.throws(() => createCookieJar(fence, [{ ...persistentCookie, domain: ".other.example" }]), /exact jar origin/);
  assert.throws(() => createCookieJar(fence, [{ ...persistentCookie, storeId: "1" }]), /another browser store/);
  assert.throws(() => createCookieJar(fence, [{ ...persistentCookie, session: true }]), /Session cookie/);
  assert.throws(() => createCookieJar(fence, [{
    ...persistentCookie,
    partitionKey: { topLevelSite: "https://top.example/path" },
  }]), /exact HTTP\(S\) origin/);
  assert.throws(() => validateCookieJar({
    schema_version: 1,
    ...fence,
    revision: 0,
    cookies: [{
      ...persistentCookie,
      partitionKey: { topLevelSite: "https://top.example", futureField: true },
    }],
  }), /unsupported fields/);
});

test("maps cookie sync only through the injected authenticated Connect fetch", async () => {
  const calls: Array<{ input: string; init: RequestInit | undefined }> = [];
  const jar = createCookieJar(fence, [persistentCookie]);
  const transport = createAuthenticatedCookieSyncTransport(async (input, init) => {
    calls.push({ input, init });
    if (input.endsWith("/materialize")) {
      return Response.json({ id: "0123456789abcdefghijklmnop", updated_at: 1_788_307_200_000, ...jar });
    }
    return Response.json({
      id: "0123456789abcdefghijklmnop",
      ...fence,
      revision: 1,
      cookie_count: 1,
      updated_at: 1_788_307_200_000,
    });
  });
  const id = "0123456789abcdefghijklmnop";
  const metadata = await transport.replace(id, jar);
  assert.equal(metadata.revision, 1);
  assert.deepEqual(await transport.materialize(id, fence), jar);
  assert.equal(calls[0]?.input, `/v1/browser-cookie-jars/${id}`);
  assert.equal(calls[0]?.init?.method, "PUT");
  assert.deepEqual(JSON.parse(String(calls[0]?.init?.body)), jar);
  assert.equal(calls[1]?.input, `/v1/browser-cookie-jars/${id}/materialize`);
  assert.equal(calls[1]?.init?.method, "POST");
  assert.deepEqual(JSON.parse(String(calls[1]?.init?.body)), fence);
  assert.equal(new Headers(calls[0]?.init?.headers).has("authorization"), false);
  assert.equal(calls[0]?.init?.cache, "no-store");
  assert.equal(calls[0]?.init?.redirect, "error");
});
