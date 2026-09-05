import assert from "node:assert/strict";
import test from "node:test";

import {
  browserCookieBrokerPath,
  projectBrowserCookieBrokerError,
  projectBrowserCookieJarList,
  projectBrowserCookieJarMaterialization,
  projectBrowserCookieJarMetadata,
  projectBrowserCookieJarNames,
} from "../src/browserCookieEgress.mjs";

const jarId = "j".repeat(32);
const binding = {
  origin: "https://example.com",
  profile_id: "browser-profile-1",
  store_id: "0",
};
const metadata = {
  id: jarId,
  ...binding,
  revision: 2,
  cookie_count: 1,
  updated_at: 123,
};

test("browser cookie broker paths isolate the derived user and private route variants", () => {
  assert.equal(
    browserCookieBrokerPath("profile:user/one"),
    "/users/profile%3Auser%2Fone/credentials/browser-cookie-jars",
  );
  assert.equal(
    browserCookieBrokerPath("profile:user/one", jarId),
    `/users/profile%3Auser%2Fone/credentials/browser-cookie-jars/${jarId}`,
  );
  assert.equal(
    browserCookieBrokerPath("profile:user/one", jarId, true),
    `/users/profile%3Auser%2Fone/credentials/browser-cookie-jars/${jarId}/materialize`,
  );
  assert.equal(
    browserCookieBrokerPath("profile:user/one", jarId, "names"),
    `/users/profile%3Auser%2Fone/credentials/browser-cookie-jars/${jarId}/names`,
  );
  assert.throws(() => browserCookieBrokerPath("profile:user/one", "short"));
  assert.throws(() => browserCookieBrokerPath("profile:user/one", jarId, "unknown"));
});

test("metadata projection filters to the exact tab/profile/store and cannot project cookie secrets", () => {
  const other = { ...metadata, id: "k".repeat(32), origin: "https://other.example" };
  const projected = projectBrowserCookieJarList({ browser_cookie_jars: [metadata, other] }, binding);
  assert.deepEqual(projected, { browser_cookie_jars: [metadata] });
  assert.doesNotMatch(JSON.stringify(projected), /cookie-name|cookie-value/);
  assert.throws(() => projectBrowserCookieJarMetadata({
    ...metadata,
    name: "cookie-name",
    value: "cookie-value",
  }));
});

test("materialization is the only response projection that retains cookie names and values", () => {
  const materialized = projectBrowserCookieJarMaterialization({
    schema_version: 1,
    id: jarId,
    ...binding,
    revision: 2,
    updated_at: 123,
    cookies: [{
      name: "session",
      value: "top-secret",
      domain: "example.com",
      path: "/",
      hostOnly: true,
      secure: true,
      httpOnly: true,
      sameSite: "lax",
      session: true,
      storeId: "0",
    }],
  }, jarId, binding);
  assert.equal(materialized.cookies[0].name, "session");
  assert.equal(materialized.cookies[0].value, "top-secret");
  assert.throws(() => projectBrowserCookieJarMaterialization({
    ...materialized,
    profile_id: "another-profile",
  }, jarId, binding));
  assert.throws(() => projectBrowserCookieJarMaterialization({
    ...materialized,
    cookies: [{ ...materialized.cookies[0], unexpected_secret: "must-not-project" }],
  }, jarId, binding));
});

test("names projection accepts only a sorted unique names-only response for the exact binding", () => {
  const names = {
    ...metadata,
    cookie_count: 3,
    cookie_names: ["persistent", "session"],
  };
  assert.deepEqual(projectBrowserCookieJarNames(names, jarId, binding), names);
  assert.doesNotMatch(JSON.stringify(names), /top-secret/);
  assert.throws(() => projectBrowserCookieJarNames({
    ...names,
    origin: "https://other.example",
  }, jarId, binding));
  assert.throws(() => projectBrowserCookieJarNames({
    ...names,
    cookie_names: ["session", "persistent"],
  }, jarId, binding));
  assert.throws(() => projectBrowserCookieJarNames({
    ...names,
    cookie_names: ["persistent", "persistent"],
  }, jarId, binding));
  assert.throws(() => projectBrowserCookieJarNames({
    ...names,
    cookies: [{ name: "session", value: "top-secret" }],
  }, jarId, binding));
});

test("broker failures pass only a bounded public code", () => {
  assert.deepEqual(projectBrowserCookieBrokerError({
    error: "browser_cookie_jar_revision_conflict",
    current_revision: 7,
    message: "top-secret",
  }), { error: "browser_cookie_jar_revision_conflict" });
  assert.throws(() => projectBrowserCookieBrokerError({ error: "unknown", value: "top-secret" }));
});
