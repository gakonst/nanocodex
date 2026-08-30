import assert from "node:assert/strict";
import test from "node:test";

import {
  authenticateEmbedProject,
  embedPrincipalId,
  identitySessionToken,
  parseEmbedProjects,
  parseEmbedSessionBody,
  sha256Base64Url,
} from "../src/embedIdentity.mjs";

const secret = "project-secret-that-is-long-enough-123";
const app = {
  appId: "acme",
  appOrigin: "https://app.acme.test",
};

async function projects() {
  return JSON.stringify([{
    app_id: app.appId,
    app_origin: app.appOrigin,
    secret_sha256: await sha256Base64Url(secret),
  }]);
}

test("embed project configuration authenticates an exact app, origin, and secret", async () => {
  const encoded = await projects();
  assert.deepEqual(parseEmbedProjects(encoded), [{
    ...app,
    secretSha256: await sha256Base64Url(secret),
  }]);
  assert.deepEqual(await authenticateEmbedProject(encoded, { ...app, secret }), {
    ...app,
    secretSha256: await sha256Base64Url(secret),
  });
  assert.equal(await authenticateEmbedProject(encoded, { ...app, secret: `${secret}x` }), undefined);
  assert.equal(await authenticateEmbedProject(encoded, {
    ...app,
    appOrigin: "https://other.acme.test",
    secret,
  }), undefined);
});

test("embed session request is strict, short lived, and opaque", () => {
  assert.deepEqual(parseEmbedSessionBody({
    app_origin: app.appOrigin,
    subject: "better-auth-user-id",
    organization: "tenant-7",
    expires_in: 60,
  }), {
    appOrigin: app.appOrigin,
    subject: "better-auth-user-id",
    organization: "tenant-7",
    expiresIn: 60,
  });
  assert.throws(() => parseEmbedSessionBody({
    app_origin: app.appOrigin,
    subject: "user",
    provider_access_token: "must-never-cross-this-boundary",
  }), /invalid/);
  assert.throws(() => parseEmbedSessionBody({
    app_origin: app.appOrigin,
    subject: "user",
    expires_in: 301,
  }), /expiry/);
});

test("Connect accepts exactly one well-formed embedded session resource", () => {
  const token = "s".repeat(43);
  assert.equal(identitySessionToken(["documents", `urn:nanocodex:identity-session:${token}`]), token);
  assert.equal(identitySessionToken(["documents"]), undefined);
  assert.throws(() => identitySessionToken([
    `urn:nanocodex:identity-session:${token}`,
    `urn:nanocodex:identity-session:${"t".repeat(43)}`,
  ]), /invalid/);
});

test("external principal IDs are stable within an app and unlinkable across apps", async () => {
  const base = {
    appId: app.appId,
    appOrigin: app.appOrigin,
    issuer: `urn:nanocodex:app:${app.appId}`,
    subject: "privy-user-id",
  };
  assert.equal(await embedPrincipalId(base), await embedPrincipalId(base));
  assert.notEqual(await embedPrincipalId(base), await embedPrincipalId({
    ...base,
    appId: "other-app",
    issuer: "urn:nanocodex:app:other-app",
  }));
});

test("embed project configuration rejects duplicates and non-HTTPS origins", async () => {
  const entry = {
    app_id: app.appId,
    app_origin: app.appOrigin,
    secret_sha256: await sha256Base64Url(secret),
  };
  assert.throws(() => parseEmbedProjects(JSON.stringify([entry, entry])), /duplicate/);
  assert.throws(() => parseEmbedProjects(JSON.stringify([{ ...entry, app_origin: "http://app.acme.test" }])), /origin/);
  assert.doesNotThrow(() => parseEmbedProjects(JSON.stringify([{
    ...entry,
    app_origin: "http://localhost:5190",
  }])));
});
