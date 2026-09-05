import assert from "node:assert/strict";
import test from "node:test";

import { AppVisibilityPermissions } from "../dist/AppVisibilityPermissions.js";
import {
  appVisibilityPermissions,
  formatCliBrowserCookieSyncResource,
  parseConnectPolicy,
  parseCliBrowserCookieSyncResource,
} from "../dist/connectPolicy.mjs";

const resource = "urn:nanocodex:browser-cookies:sync";
const detail = "Allow the Chrome extension to upload and restore cookie values for the current site. Restored cookies can grant access to your signed-in account. Cookie values are encrypted in your account Vault and are never shown to the model.";
const cliOrigin = "https://accounts.example.com";
const cliResource = formatCliBrowserCookieSyncResource(cliOrigin);
const cliDetail = `Allow the Nanocodex CLI to upload and restore cookie values only for ${cliOrigin}. Restored cookies can grant access to your signed-in account. Cookie values are encrypted in your account Vault and are never shown to the model.`;

test("the exact signed browser cookie sync resource is accepted and projected separately", () => {
  assert.deepEqual(parseConnectPolicy([resource]), { chatGptCredentialImport: false });
  assert.deepEqual(appVisibilityPermissions([resource]), [{
    resource,
    label: "Browser cookie sync",
    detail,
  }]);
});

test("CLI browser cookie consent is canonical, exact-origin, and distinct from Chrome", () => {
  assert.equal(parseCliBrowserCookieSyncResource(cliResource), cliOrigin);
  assert.deepEqual(parseConnectPolicy([cliResource]), { chatGptCredentialImport: false });
  assert.deepEqual(appVisibilityPermissions([cliResource]), [{
    resource: cliResource,
    label: "Browser cookie sync",
    detail: cliDetail,
  }]);
  for (const invalid of [
    "https://accounts.example.com/",
    "https://ACCOUNTS.example.com",
    "http://accounts.example.com",
  ]) {
    assert.throws(() => formatCliBrowserCookieSyncResource(invalid), /canonical origin/);
  }
  assert.throws(
    () => parseConnectPolicy([resource, cliResource]),
    /signed browser cookie sync resource is invalid/,
  );
});

test("browser cookie sync resource variants fail closed", () => {
  for (const invalid of [
    "urn:nanocodex:browser-cookies",
    "urn:nanocodex:browser-cookies:",
    "urn:nanocodex:browser-cookies:read",
    "urn:nanocodex:browser-cookies:sync:all-sites",
    "urn:nanocodex:browser-cookies-sync",
  ]) {
    assert.throws(
      () => parseConnectPolicy([invalid]),
      /signed browser cookie sync resource is invalid/,
    );
    assert.deepEqual(appVisibilityPermissions([invalid]), []);
  }
  assert.throws(
    () => parseConnectPolicy([resource, resource]),
    /signed browser cookie sync resource is invalid/,
  );
});

test("browser cookie sync approval renders the complete human-readable warning", () => {
  const rendered = AppVisibilityPermissions({
    permissions: appVisibilityPermissions([resource]),
  });
  assert.equal(rendered.length, 1);
  assert.equal(rendered[0].props.role, "listitem");
  const row = rendered[0].props.children;
  assert.equal(row[0].props.children, "✓");
  assert.equal(row[1].props.children[0].props.children, "Browser cookie sync");
  assert.equal(row[1].props.children[1].props.children, detail);

  const cliRendered = AppVisibilityPermissions({
    permissions: appVisibilityPermissions([cliResource]),
  });
  assert.equal(cliRendered[0].props.children[1].props.children[1].props.children, cliDetail);
});
