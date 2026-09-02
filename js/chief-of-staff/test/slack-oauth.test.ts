import assert from "node:assert/strict";
import test from "node:test";
import {
  SLACK_BOT_SCOPES,
  slackAuthorizationUrl,
  verifySlackInstallState,
} from "../src/slack-oauth.ts";

const accountId = "00000000-0000-4000-8000-000000000001";
const stateSecret = Buffer.alloc(32, 11).toString("base64url");

test("one-click Slack authorization requests only bot scopes and binds the callback", async () => {
  const now = 1_800_000_000_000;
  const authorization = await slackAuthorizationUrl({
    accountId,
    clientId: "123.456",
    redirectUri: "https://chief.example/v1/slack/callback",
    stateSecret,
    now,
  });

  assert.equal(authorization.origin, "https://slack.com");
  assert.equal(authorization.pathname, "/oauth/v2/authorize");
  assert.equal(authorization.searchParams.get("client_id"), "123.456");
  assert.equal(authorization.searchParams.get("scope"), SLACK_BOT_SCOPES.join(","));
  assert.equal(
    authorization.searchParams.get("redirect_uri"),
    "https://chief.example/v1/slack/callback",
  );
  assert.equal(
    (await verifySlackInstallState(authorization.searchParams.get("state")!, stateSecret, now))?.accountId,
    accountId,
  );
});

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
