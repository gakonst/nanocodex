import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

import { formatCliBrowserCookieSyncResource } from "../src/appToolPolicy.mts";

const execFileAsync = promisify(execFile);
const capability = "urn:nanocodex:browser-cookies:sync";
const appId = "nanocodex-chrome";
const appOrigin = `chrome-extension://${"a".repeat(32)}`;
const otherAppOrigin = `chrome-extension://${"b".repeat(32)}`;
const accountA = `0x${"1".repeat(40)}`;
const accountB = `0x${"2".repeat(40)}`;
const brokerA = "0f5f2ab8-2585-4d7c-9403-0de76f55ad18";
const brokerB = "1f5f2ab8-2585-4d7c-9403-0de76f55ad19";
const grantIdA = `0x${"3".repeat(64)}`;
const grantIdB = `0x${"4".repeat(64)}`;
const tokenA = "t".repeat(43);
const tokenB = "u".repeat(43);
const tokenCli = "v".repeat(43);
const jarId = "j".repeat(32);
const binding = {
  origin: "https://example.com",
  profile_id: "browser-profile-1",
  store_id: "0",
};
const cookie = {
  name: "session",
  value: "top-secret-cookie-value",
  domain: "example.com",
  path: "/",
  hostOnly: true,
  secure: true,
  httpOnly: true,
  sameSite: "lax",
  session: true,
  storeId: "0",
};

function grant(overrides = {}) {
  return {
    id: grantIdA,
    appId,
    appOrigin,
    accountAddress: accountA,
    brokerUserId: brokerA,
    agentId: "agent-cookie-test",
    permission: "agent.run",
    status: "active",
    expiresAt: Math.floor(Date.now() / 1000) + 3_600,
    capabilities: [capability],
    spentAtomics: "0",
    egressSubject: "s".repeat(43),
    ...overrides,
  };
}

function principal(record) {
  return {
    accountAddress: record.accountAddress,
    appId: record.appId,
    appOrigin: record.appOrigin,
    grantId: record.id,
  };
}

function publicRequest(pathname, init = {}, token = tokenA, origin = appOrigin, callerAppId = appId) {
  return new Request(`https://nanocodex-connect-api.gakonst.workers.dev${pathname}`, {
    ...init,
    headers: {
      authorization: `Bearer ${token}`,
      origin,
      "x-nanocodex-app-id": callerAppId,
      ...init.headers,
    },
  });
}

test("authenticated browser cookie routes preserve exact account/app/origin/profile fences", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-connect-browser-cookies-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  const wrangler = new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url);
  await execFileAsync(process.execPath, [
    wrangler.pathname, "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir,
  ], { cwd: new URL("..", import.meta.url) });
  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;

  const grantA = grant();
  const grantB = grant({ id: grantIdB, accountAddress: accountB, brokerUserId: brokerB });
  const cliAppId = "nanocodex-cli";
  const cliAppOrigin = "https://cli.nanocodex.xyz";
  const cliCapability = formatCliBrowserCookieSyncResource(binding.origin);
  const cliGrant = grant({
    id: `0x${"5".repeat(64)}`,
    appId: cliAppId,
    appOrigin: cliAppOrigin,
    capabilities: [cliCapability],
  });
  const resolved = new Map([
    [tokenA, { grant: grantA, principal: principal(grantA) }],
    [tokenB, { grant: grantB, principal: principal(grantB) }],
    [tokenCli, { grant: cliGrant, principal: principal(cliGrant) }],
  ]);
  let privateHandler = async () => Response.json({ error: "unexpected" }, { status: 500 });
  const privateRequests = [];
  const env = {
    CONNECT_STATE: {
      idFromName: (name) => name,
      get: () => ({
        async fetch(input) {
          const token = new URL(input instanceof Request ? input.url : input).searchParams.get("token");
          return Response.json(resolved.get(token));
        },
      }),
    },
    EGRESS: {
      async fetch(request) {
        privateRequests.push(request.clone());
        return privateHandler(request);
      },
    },
  };
  const context = { waitUntil() {} };
  const metadata = {
    id: jarId,
    ...binding,
    revision: 2,
    cookie_count: 1,
    updated_at: 123,
  };

  privateHandler = async () => Response.json({
    browser_cookie_jars: [
      metadata,
      { ...metadata, id: "k".repeat(32), origin: "https://other.example" },
      { ...metadata, id: "m".repeat(32), profile_id: "other-profile" },
    ],
  });
  const query = new URLSearchParams(binding);
  const listed = await worker.fetch(publicRequest(`/v1/browser-cookie-jars?${query}`), env, context);
  assert.equal(listed.status, 200);
  assert.equal(listed.headers.get("cache-control"), "no-store");
  assert.equal(listed.headers.get("access-control-allow-origin"), appOrigin);
  assert.deepEqual(await listed.json(), { browser_cookie_jars: [metadata] });
  assert.equal(privateRequests.at(-1).url,
    `https://broker.internal/users/${brokerA}/credentials/browser-cookie-jars`);

  await worker.fetch(publicRequest(`/v1/browser-cookie-jars?${query}`, {}, tokenB), env, context);
  assert.equal(privateRequests.at(-1).url,
    `https://broker.internal/users/${brokerB}/credentials/browser-cookie-jars`);

  const beforeDenied = privateRequests.length;
  const wildcard = await worker.fetch(publicRequest(
    "/v1/browser-cookie-jars?origin=https%3A%2F%2F*.example.com&profile_id=browser-profile-1&store_id=0",
  ), env, context);
  assert.equal(wildcard.status, 400);
  assert.equal(wildcard.headers.get("cache-control"), "no-store");
  assert.equal(privateRequests.length, beforeDenied);

  const wrongApp = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars?${query}`,
    {},
    tokenA,
    otherAppOrigin,
  ), env, context);
  assert.equal(wrongApp.status, 401);
  assert.equal(privateRequests.length, beforeDenied);

  resolved.set(tokenA, { grant: grant({ status: "revoked" }), principal: principal(grantA) });
  const revoked = await worker.fetch(publicRequest(`/v1/browser-cookie-jars?${query}`), env, context);
  assert.equal(revoked.status, 403);
  assert.equal(privateRequests.length, beforeDenied);

  const withoutCapability = grant({ capabilities: [] });
  resolved.set(tokenA, { grant: withoutCapability, principal: principal(withoutCapability) });
  const ungranted = await worker.fetch(publicRequest(`/v1/browser-cookie-jars?${query}`), env, context);
  assert.equal(ungranted.status, 403);
  assert.equal(privateRequests.length, beforeDenied);
  resolved.set(tokenA, { grant: grantA, principal: principal(grantA) });

  privateHandler = async (request) => {
    assert.equal(request.method, "PUT");
    assert.deepEqual(await request.json(), {
      schema_version: 1,
      ...binding,
      revision: 1,
      cookies: [cookie],
    });
    return Response.json(metadata);
  };
  const upserted = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ schema_version: 1, ...binding, revision: 1, cookies: [cookie] }),
  }), env, context);
  assert.equal(upserted.status, 200);
  assert.equal(upserted.headers.get("cache-control"), "no-store");
  assert.deepEqual(await upserted.json(), metadata);
  assert.doesNotMatch(JSON.stringify(metadata), /top-secret-cookie-value/);
  assert.equal(privateRequests.at(-1).url,
    `https://broker.internal/users/${brokerA}/credentials/browser-cookie-jars/${jarId}`);

  privateHandler = async () => Response.json({
    error: "browser_cookie_jar_revision_conflict",
    current_revision: 2,
    value: "must-not-project",
  }, { status: 409 });
  const stale = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ schema_version: 1, ...binding, revision: 1, cookies: [cookie] }),
  }), env, context);
  assert.equal(stale.status, 409);
  assert.equal(stale.headers.get("cache-control"), "no-store");
  assert.deepEqual(await stale.json(), { error: "browser_cookie_jar_revision_conflict" });

  const beforeMismatch = privateRequests.length;
  const profileMismatch = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      schema_version: 1,
      ...binding,
      revision: 2,
      cookies: [{ ...cookie, storeId: "1" }],
    }),
  }), env, context);
  assert.equal(profileMismatch.status, 400);
  assert.equal(privateRequests.length, beforeMismatch);
  const incognito = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      schema_version: 1,
      ...binding,
      revision: 2,
      cookies: [cookie],
      incognito: true,
    }),
  }), env, context);
  assert.equal(incognito.status, 400);
  assert.equal(privateRequests.length, beforeMismatch);
  const callerAuthority = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "DELETE",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...binding, revision: 2, account_id: accountB, user_id: brokerB }),
  }), env, context);
  assert.equal(callerAuthority.status, 400);
  assert.equal(privateRequests.length, beforeMismatch);

  privateHandler = async () => Response.json({
    schema_version: 1,
    id: jarId,
    ...binding,
    revision: 2,
    updated_at: 123,
    cookies: [cookie],
  });
  const materialized = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars/${jarId}/materialize`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(binding),
    },
  ), env, context);
  assert.equal(materialized.status, 200);
  assert.equal(materialized.headers.get("cache-control"), "no-store");
  assert.equal((await materialized.json()).cookies[0].value, "top-secret-cookie-value");

  const namesProjection = {
    id: jarId,
    ...binding,
    revision: 2,
    updated_at: 123,
    cookie_count: 2,
    cookie_names: ["persistent", "session"],
  };
  privateHandler = async (request) => {
    assert.equal(request.method, "POST");
    assert.deepEqual(await request.json(), binding);
    return Response.json(namesProjection);
  };
  const namesOnly = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars/${jarId}/names`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(binding),
    },
  ), env, context);
  assert.equal(namesOnly.status, 200);
  assert.equal(namesOnly.headers.get("cache-control"), "no-store");
  const namesText = await namesOnly.text();
  assert.deepEqual(JSON.parse(namesText), namesProjection);
  assert.doesNotMatch(namesText, /top-secret-cookie-value|"cookies"|"value"|"domain"|"path"/);
  assert.equal(privateRequests.at(-1).url,
    `https://broker.internal/users/${brokerA}/credentials/browser-cookie-jars/${jarId}/names`);

  privateHandler = async () => Response.json({
    ...namesProjection,
    cookie_value: "top-secret-cookie-value",
  });
  const malformedNames = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars/${jarId}/names`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(binding),
    },
  ), env, context);
  assert.equal(malformedNames.status, 502);
  assert.equal(malformedNames.headers.get("cache-control"), "no-store");
  assert.doesNotMatch(await malformedNames.text(), /top-secret-cookie-value/);

  privateHandler = async () => Response.json({
    ...namesProjection,
    profile_id: "other-profile",
  });
  const wrongNamesBinding = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars/${jarId}/names`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(binding),
    },
  ), env, context);
  assert.equal(wrongNamesBinding.status, 502);

  const namesWithoutCapability = grant({ capabilities: [] });
  resolved.set(tokenA, {
    grant: namesWithoutCapability,
    principal: principal(namesWithoutCapability),
  });
  const beforeNamesDenied = privateRequests.length;
  const namesUngranted = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars/${jarId}/names`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(binding),
    },
  ), env, context);
  assert.equal(namesUngranted.status, 403);
  assert.equal(privateRequests.length, beforeNamesDenied);
  resolved.set(tokenA, { grant: grantA, principal: principal(grantA) });

  privateHandler = async (request) => {
    assert.equal(request.method, "DELETE");
    assert.deepEqual(await request.json(), { ...binding, revision: 2 });
    return new Response(null, { status: 204 });
  };
  const deleted = await worker.fetch(publicRequest(`/v1/browser-cookie-jars/${jarId}`, {
    method: "DELETE",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...binding, revision: 2 }),
  }), env, context);
  assert.equal(deleted.status, 204);
  assert.equal(deleted.headers.get("cache-control"), "no-store");

  privateHandler = async () => Response.json({ browser_cookie_jars: [metadata] });
  const cliListed = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars?${query}`,
    {},
    tokenCli,
    cliAppOrigin,
    cliAppId,
  ), env, context);
  assert.equal(cliListed.status, 200);
  assert.deepEqual(await cliListed.json(), { browser_cookie_jars: [metadata] });
  assert.equal(privateRequests.at(-1).url,
    `https://broker.internal/users/${brokerA}/credentials/browser-cookie-jars`);

  const beforeCliDenied = privateRequests.length;
  const otherBinding = new URLSearchParams({ ...binding, origin: "https://other.example" });
  const wrongCliOrigin = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars?${otherBinding}`,
    {},
    tokenCli,
    cliAppOrigin,
    cliAppId,
  ), env, context);
  assert.equal(wrongCliOrigin.status, 403);
  assert.deepEqual(await wrongCliOrigin.json(), {
    error: {
      code: "browser_cookie_sync_origin_denied",
      message: "The requested browser cookie origin was not approved for this CLI grant.",
    },
  });
  assert.equal(privateRequests.length, beforeCliDenied);

  const genericCliGrant = { ...cliGrant, capabilities: [capability] };
  resolved.set(tokenCli, { grant: genericCliGrant, principal: principal(genericCliGrant) });
  const genericCli = await worker.fetch(publicRequest(
    `/v1/browser-cookie-jars?${query}`,
    {},
    tokenCli,
    cliAppOrigin,
    cliAppId,
  ), env, context);
  assert.equal(genericCli.status, 403);
  assert.equal(privateRequests.length, beforeCliDenied);

  const exactChromeGrant = grant({ capabilities: [cliCapability] });
  resolved.set(tokenA, { grant: exactChromeGrant, principal: principal(exactChromeGrant) });
  const exactChrome = await worker.fetch(publicRequest(`/v1/browser-cookie-jars?${query}`), env, context);
  assert.equal(exactChrome.status, 403);
  assert.equal(privateRequests.length, beforeCliDenied);
});
