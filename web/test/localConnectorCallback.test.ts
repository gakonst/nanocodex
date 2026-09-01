import assert from "node:assert/strict";
import test from "node:test";

import {
  localConnectorAuthorization,
  localConnectorCallbackReturn,
  localMcpAuthorization,
  wrapLocalConnectorAuthorizationState,
  wrapLocalMcpAuthorizationState,
} from "../localConnectorCallback.ts";
import {
  LOCAL_OAUTH_RELAY_ORIGIN,
  isLocalNanocodexOrigin,
  localOAuthRelayCallbackRedirect,
} from "../localOAuthRelayEnvelope.mjs";

const RELAY_KEY = "test-local-oauth-relay-key-that-is-distinct";
const NOW = 1_800_000_000_000;
const MCP_CONNECTION_ID = "mcp_abcdefghijklmnopqrstuvwxyz0123456789ABC";

test("portable worktrees use one fixed loopback callback and return to their own origin", async () => {
  const target = "http://feature-a.nanocodex.localhost:20735";
  const local = localConnectorAuthorization(target, "github", "connect");
  assert.deepEqual(local, {
    connector: "github",
    redirectUri: "http://127.0.0.1:47891/v1/connectors/github/callback",
    targetOrigin: target,
    flow: "connect",
  });

  const authorization = new URL(
    "https://github.com/login/oauth/authorize?state=broker-state&redirect_uri=http%3A%2F%2F127.0.0.1%3A47891%2Fv1%2Fconnectors%2Fgithub%2Fcallback",
  );
  await wrapLocalConnectorAuthorizationState(authorization, local!, RELAY_KEY);
  assert.notEqual(authorization.searchParams.get("state"), "broker-state");

  const callback = new URL(local!.redirectUri);
  callback.searchParams.set("code", "provider-code");
  callback.searchParams.set("scope", "ignored-provider-field");
  callback.searchParams.set("state", authorization.searchParams.get("state")!);
  const destination = await localOAuthRelayCallbackRedirect(callback, RELAY_KEY);
  assert.equal(
    destination?.href,
    `${target}/v1/connect/auth/connector-callback/github?code=provider-code&state=broker-state`,
  );
  assert.deepEqual(localConnectorCallbackReturn(destination!), {
    callbackUrl: new URL(`${target}/v1/connectors/github/callback?code=provider-code&state=broker-state`),
    flow: "connect",
  });
});

test("managed callbacks return only to the signed fixed provider path", async () => {
  const target = "http://nanocodex.localhost:5173";
  const local = localConnectorAuthorization(target, "gmail", "managed")!;
  const authorization = new URL("https://accounts.google.com/o/oauth2/v2/auth?state=inner");
  await wrapLocalConnectorAuthorizationState(authorization, local, RELAY_KEY);
  const callback = new URL(local.redirectUri);
  callback.searchParams.set("error", "access_denied");
  callback.searchParams.set("error_description", "cancelled");
  callback.searchParams.set("state", authorization.searchParams.get("state")!);
  assert.equal(
    (await localOAuthRelayCallbackRedirect(callback, RELAY_KEY))?.href,
    `${target}/v1/connectors/gmail/callback?error=access_denied&error_description=cancelled&state=inner`,
  );
});

test("Slack callbacks use the same isolated local relay", async () => {
  const target = "http://feature-a.nanocodex.localhost:20735";
  const local = localConnectorAuthorization(target, "slack", "connect")!;
  assert.equal(local.redirectUri, `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/slack/callback`);
  const authorization = new URL("https://slack.com/oauth/v2/authorize?state=inner");
  await wrapLocalConnectorAuthorizationState(authorization, local, RELAY_KEY);
  const callback = new URL(local.redirectUri);
  callback.searchParams.set("code", "provider-code");
  callback.searchParams.set("state", authorization.searchParams.get("state")!);
  assert.equal(
    (await localOAuthRelayCallbackRedirect(callback, RELAY_KEY))?.href,
    `${target}/v1/connect/auth/connector-callback/slack?code=provider-code&state=inner`,
  );
});

test("generic MCP callbacks bind the opaque connection and return to its fixed backend path", async () => {
  const target = "http://feature-a.nanocodex.localhost:20735";
  const local = localMcpAuthorization(target, MCP_CONNECTION_ID, "connect");
  assert.deepEqual(local, {
    connectionId: MCP_CONNECTION_ID,
    redirectUri: `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/mcp-connections/${MCP_CONNECTION_ID}/callback`,
    targetOrigin: target,
    flow: "connect",
  });
  const authorization = new URL("https://auth.example/authorize?state=private-broker-state");
  await wrapLocalMcpAuthorizationState(authorization, local!, RELAY_KEY);
  const callback = new URL(local!.redirectUri);
  callback.searchParams.set("code", "provider-code");
  callback.searchParams.set("state", authorization.searchParams.get("state")!);
  const destination = await localOAuthRelayCallbackRedirect(callback, RELAY_KEY);
  assert.equal(destination?.href,
    `${target}/v1/connect/auth/mcp-connection-callback/${MCP_CONNECTION_ID}?code=provider-code&state=private-broker-state`);
  assert.deepEqual(localConnectorCallbackReturn(destination!), {
    callbackUrl: new URL(`${target}/v1/mcp-connections/${MCP_CONNECTION_ID}/callback?code=provider-code&state=private-broker-state`),
    flow: "connect",
  });

  const wrongConnection = new URL(
    `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/mcp-connections/${"x".repeat(43)}/callback`,
  );
  wrongConnection.searchParams.set("state", authorization.searchParams.get("state")!);
  assert.equal(await localOAuthRelayCallbackRedirect(wrongConnection, RELAY_KEY), undefined);
});

test("the relay rejects tampering, expiry, provider confusion, and unsafe targets", async () => {
  for (const origin of [
    "https://nanocodex.example",
    "http://attacker.example:20735",
    "http://nested.feature.nanocodex.localhost:20735",
    "http://feature.nanocodex.localhost:47891",
    "http://user@feature.nanocodex.localhost:20735",
    "http://feature.nanocodex.localhost:20735/path",
  ]) {
    assert.equal(isLocalNanocodexOrigin(origin), false, origin);
    assert.equal(localConnectorAuthorization(origin, "github", "managed"), undefined);
    assert.equal(localMcpAuthorization(origin, MCP_CONNECTION_ID, "connect"), undefined);
  }

  const local = localConnectorAuthorization(
    "http://feature-a.nanocodex.localhost:20735",
    "github",
    "managed",
  )!;
  const authorization = new URL("https://github.com/login/oauth/authorize?state=broker-state");
  await wrapLocalConnectorAuthorizationState(authorization, local, RELAY_KEY);
  const state = authorization.searchParams.get("state")!;

  for (const callback of [
    new URL(`${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/gmail/callback?state=${state}`),
    new URL(`${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/github/callback?state=${state.slice(0, -1)}A`),
    new URL(`http://127.0.0.1:47892/v1/connectors/github/callback?state=${state}`),
    new URL(`${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/github/callback?state=${state}#fragment`),
  ]) {
    assert.equal(await localOAuthRelayCallbackRedirect(callback, RELAY_KEY), undefined);
  }

  const deterministic = new URL("https://github.com/login/oauth/authorize?state=broker-state");
  const originalNow = Date.now;
  Date.now = () => NOW;
  try { await wrapLocalConnectorAuthorizationState(deterministic, local, RELAY_KEY); } finally {
    Date.now = originalNow;
  }
  const expired = new URL(local.redirectUri);
  expired.searchParams.set("state", deterministic.searchParams.get("state")!);
  assert.equal(
    await localOAuthRelayCallbackRedirect(expired, RELAY_KEY, { now: NOW + 601_000 }),
    undefined,
  );
});

test("the fixed local return path dispatches to only the Connect backend", async () => {
  const requests: string[] = [];
  const env = {
    ENVIRONMENT: "development",
    NANOCODEX_BACKEND: { async fetch(request: Request) {
      requests.push(`managed:${request.url}`);
      return new Response("managed");
    } },
    NANOCODEX_CONNECT_API: { async fetch(request: Request) {
      requests.push(`connect:${request.url}`);
      return new Response("connect");
    } },
  };
  const { routeLocalConnectorCallbackReturn } = await import("../worker/localConnectorCallbackRelay.ts");
  const request = new Request(
    "http://feature-a.nanocodex.localhost:20735/v1/connect/auth/connector-callback/github?code=code&state=state",
    { headers: { cookie: "nanocodex_account=session" } },
  );
  const response = await routeLocalConnectorCallbackReturn(request, env, new URL(request.url));
  assert.equal(await response?.text(), "connect");
  assert.deepEqual(requests, [
    "connect:http://feature-a.nanocodex.localhost:20735/v1/connectors/github/callback?code=code&state=state",
  ]);
});

test("the fixed local MCP return path dispatches only its bound connection callback", async () => {
  const requests: string[] = [];
  const env = {
    ENVIRONMENT: "development",
    NANOCODEX_CONNECT_API: { async fetch(request: Request) {
      requests.push(request.url);
      return new Response("connect");
    } },
  };
  const { routeLocalConnectorCallbackReturn } = await import("../worker/localConnectorCallbackRelay.ts");
  const request = new Request(
    `http://feature-a.nanocodex.localhost:20735/v1/connect/auth/mcp-connection-callback/${MCP_CONNECTION_ID}?code=code&state=state`,
  );
  const response = await routeLocalConnectorCallbackReturn(request, env, new URL(request.url));
  assert.equal(await response?.text(), "connect");
  assert.deepEqual(requests, [
    `http://feature-a.nanocodex.localhost:20735/v1/mcp-connections/${MCP_CONNECTION_ID}/callback?code=code&state=state`,
  ]);
});
