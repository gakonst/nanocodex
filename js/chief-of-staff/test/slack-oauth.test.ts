import assert from "node:assert/strict";
import test from "node:test";
import {
  slackAuthorizationUrl,
  verifySlackInstallState,
} from "../src/slack-oauth.ts";

const accountId = "00000000-0000-4000-8000-000000000001";
const stateSecret = Buffer.alloc(32, 11).toString("base64url");

test("Slack install state rejects tampering, expiration, and the wrong signing key", async () => {
  const now = 1_800_000_000_000;
  const authorization = await slackAuthorizationUrl({
    accountId,
    clientId: "123.456",
    redirectUri: "https://chief.example/v1/slack/callback",
    stateSecret,
    now,
  });
  const state = authorization.searchParams.get("state")!;

  assert.equal(await verifySlackInstallState(`${state}x`, stateSecret, now), undefined);
  assert.equal(
    await verifySlackInstallState(state, Buffer.alloc(32, 12).toString("base64url"), now),
    undefined,
  );
  assert.equal(await verifySlackInstallState(state, stateSecret, now + 10 * 60 * 1_000 + 1), undefined);
});
