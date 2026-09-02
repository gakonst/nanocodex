import assert from "node:assert/strict";
import test from "node:test";

import { localConnectorCallbackReturn } from "../localConnectorCallback.ts";

test("local callback routing recognizes unified Google and Slack providers", () => {
  for (const provider of ["google", "slack"]) {
    const returned = localConnectorCallbackReturn(new URL(
      `https://feature.nanocodex.localhost/v1/connect/auth/connector-callback/${provider}?code=code&state=state`,
    ));
    assert.equal(returned?.flow, "connect");
    assert.equal(
      returned?.callbackUrl.href,
      `https://feature.nanocodex.localhost/v1/connectors/${provider}/callback?code=code&state=state`,
    );
  }
});

test("local callback routing does not accept connector capability IDs as provider controls", () => {
  assert.equal(localConnectorCallbackReturn(new URL(
    "https://feature.nanocodex.localhost/v1/connect/auth/connector-callback/gcalendar?code=code&state=state",
  )), undefined);
});
