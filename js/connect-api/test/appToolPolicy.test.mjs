import assert from "node:assert/strict";
import test from "node:test";

import {
  APP_TOOL_CATALOG_RESOURCE_PREFIX,
  BROWSER_COOKIE_SYNC_RESOURCE,
  CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX,
  appToolCatalogDigestFromResources,
  formatCliBrowserCookieSyncResource,
  isAllowedAppToolCatalogResource,
  isChromeExtensionGrantResources,
  parseCliBrowserCookieSyncResource,
} from "../src/appToolPolicy.mts";

test("signed app tool catalogs contain one exact lowercase SHA-256 digest", () => {
  const digest = "a".repeat(64);
  const resource = `${APP_TOOL_CATALOG_RESOURCE_PREFIX}${digest}`;
  assert.equal(isAllowedAppToolCatalogResource(resource), true);
  assert.equal(appToolCatalogDigestFromResources([resource]), `0x${digest}`);
  assert.equal(appToolCatalogDigestFromResources(["urn:nanocodex:agent:run"]), undefined);
  assert.equal(isAllowedAppToolCatalogResource(`${APP_TOOL_CATALOG_RESOURCE_PREFIX}${"A".repeat(64)}`), false);
  assert.throws(() => appToolCatalogDigestFromResources([resource, resource]));
  assert.throws(() => appToolCatalogDigestFromResources([
    `${APP_TOOL_CATALOG_RESOURCE_PREFIX}${"x".repeat(64)}`,
  ]));
});

test("CLI browser cookie resources encode one canonical HTTPS or loopback origin", () => {
  const origin = "https://accounts.example.com:8443";
  const resource = formatCliBrowserCookieSyncResource(origin);
  assert.equal(
    resource,
    `${CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX}${encodeURIComponent(origin)}`,
  );
  assert.equal(parseCliBrowserCookieSyncResource(resource), origin);
  assert.equal(
    parseCliBrowserCookieSyncResource(formatCliBrowserCookieSyncResource("http://127.0.0.1:3000")),
    "http://127.0.0.1:3000",
  );
  for (const invalid of [
    "https://accounts.example.com/",
    "https://ACCOUNTS.example.com",
    "https://accounts.example.com:443",
    "http://accounts.example.com",
    "file:///tmp/cookies",
  ]) {
    assert.throws(() => formatCliBrowserCookieSyncResource(invalid), /canonical origin/);
  }
  assert.equal(parseCliBrowserCookieSyncResource(
    `${CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX}${encodeURIComponent("https://accounts.example.com/")}`,
  ), undefined);
  assert.equal(parseCliBrowserCookieSyncResource(
    `${CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX}%not-encoded`,
  ), undefined);
});

test("Chrome grants are self-contained ChatGPT-only hosted requests from any exact extension origin", () => {
  const origin = `chrome-extension://${"a".repeat(32)}`;
  const resources = [
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:nanocodex-chrome",
    `urn:nanocodex:origin:${encodeURIComponent(origin)}`,
    "urn:nanocodex:connectors:chatgpt",
    BROWSER_COOKIE_SYNC_RESOURCE,
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
    "urn:nanocodex:authorization:hosted",
    `${APP_TOOL_CATALOG_RESOURCE_PREFIX}${"b".repeat(64)}`,
    "urn:nanocodex:agent:conversation:0f5f2ab8-2585-4d7c-9403-0de76f55ad18",
  ];
  assert.equal(isChromeExtensionGrantResources(resources, "nanocodex-chrome", origin), true);
  for (const extra of [
    "urn:nanocodex:memory:read",
    "urn:nanocodex:mpp:machusd:spend",
    "urn:nanocodex:connector:github",
    `urn:nanocodex:mcp:${"m".repeat(43)}`,
  ]) {
    assert.equal(
      isChromeExtensionGrantResources([...resources, extra], "nanocodex-chrome", origin),
      false,
      extra,
    );
  }
  assert.equal(isChromeExtensionGrantResources(
    resources.filter((resource) => resource !== "urn:nanocodex:authorization:hosted"),
    "nanocodex-chrome",
    origin,
  ), false);
  assert.equal(isChromeExtensionGrantResources(
    resources.filter((resource) => resource !== BROWSER_COOKIE_SYNC_RESOURCE),
    "nanocodex-chrome",
    origin,
  ), false);
  assert.equal(isChromeExtensionGrantResources(
    [...resources, "urn:nanocodex:browser-cookies:*"],
    "nanocodex-chrome",
    origin,
  ), false);
  assert.equal(isChromeExtensionGrantResources(
    resources,
    "nanocodex-chrome",
    `chrome-extension://${"q".repeat(32)}`,
  ), false);
});
