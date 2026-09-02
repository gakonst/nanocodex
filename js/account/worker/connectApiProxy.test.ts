import assert from "node:assert/strict";
import test from "node:test";

import { isConnectApiRequest } from "./connectApiProxy.ts";

const state = `connect.${"s".repeat(43)}`;

test("production callback routing accepts unified Google and Slack providers", () => {
  for (const provider of ["github", "google", "gmail", "gdrive", "slack", "x"]) {
    const url = new URL(`https://nanocodex.test/v1/connectors/${provider}/callback?state=${state}`);
    assert.equal(isConnectApiRequest(new Request(url), url.pathname), true, provider);
  }
});

test("production callback routing rejects capability-only and unscoped callbacks", () => {
  for (const provider of ["gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts"]) {
    const url = new URL(`https://nanocodex.test/v1/connectors/${provider}/callback?state=${state}`);
    assert.equal(isConnectApiRequest(new Request(url), url.pathname), false, provider);
  }
  const unscoped = new URL("https://nanocodex.test/v1/connectors/google/callback?state=broker-state-only");
  assert.equal(isConnectApiRequest(new Request(unscoped), unscoped.pathname), false);
});
