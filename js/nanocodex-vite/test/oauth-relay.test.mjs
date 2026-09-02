import assert from "node:assert/strict";
import { test } from "node:test";

import {
  LOCAL_OAUTH_RELAY_ORIGIN,
  localConnectorAuthorization,
  localMcpAuthorization,
  verifyLocalMcpOAuthRelayState,
  verifyLocalOAuthRelayState,
  wrapLocalConnectorAuthorizationState,
  wrapLocalMcpAuthorizationState,
} from "../oauth-relay.mjs";

const RELAY_KEY = "test-only-local-oauth-relay-key-000000000000";
const CONNECTION_ID = "a".repeat(43);
const TARGET_ORIGIN = "https://feature.nanocodex.localhost:1443";

test("local connector and MCP authorizations use the loopback relay", () => {
  assert.deepEqual(localConnectorAuthorization(TARGET_ORIGIN, "github", "connect"), {
    connector: "github",
    redirectUri: `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/github/callback`,
    targetOrigin: TARGET_ORIGIN,
    flow: "connect",
  });
  assert.deepEqual(localMcpAuthorization(TARGET_ORIGIN, CONNECTION_ID, "managed"), {
    connectionId: CONNECTION_ID,
    redirectUri: `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/mcp-connections/${CONNECTION_ID}/callback`,
    targetOrigin: TARGET_ORIGIN,
    flow: "managed",
  });
  assert.equal(localConnectorAuthorization(TARGET_ORIGIN, "invalid", "connect"), undefined);
  assert.equal(localConnectorAuthorization("https://example.com", "github", "connect"), undefined);
  assert.equal(localMcpAuthorization(TARGET_ORIGIN, "short", "connect"), undefined);
});

test("unified Google and Slack provider callbacks stay owned by the local relay", () => {
  for (const provider of ["google", "slack"]) {
    assert.deepEqual(localConnectorAuthorization(TARGET_ORIGIN, provider, "managed"), {
      connector: provider,
      redirectUri: `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/${provider}/callback`,
      targetOrigin: TARGET_ORIGIN,
      flow: "managed",
    });
  }
});

test("local connector authorization state is wrapped for the relay", async () => {
  const local = localConnectorAuthorization(TARGET_ORIGIN, "gdrive", "managed");
  assert(local);
  const authorizationUrl = new URL("https://provider.example/authorize?state=provider-state");
  const wrapped = await wrapLocalConnectorAuthorizationState(authorizationUrl, local, RELAY_KEY);
  assert.equal(wrapped, authorizationUrl);
  const envelope = await verifyLocalOAuthRelayState(
    wrapped.searchParams.get("state"),
    "gdrive",
    RELAY_KEY,
  );
  assert.equal(envelope?.p, "gdrive");
  assert.equal(envelope?.o, TARGET_ORIGIN);
  assert.equal(envelope?.f, "managed");
  assert.equal(envelope?.s, "provider-state");
  assert.equal(typeof envelope?.i, "number");
  assert.equal(typeof envelope?.e, "number");
  assert.equal(typeof envelope?.n, "string");
});

test("local MCP authorization state is wrapped for the relay", async () => {
  const local = localMcpAuthorization(TARGET_ORIGIN, CONNECTION_ID, "connect");
  assert(local);
  const authorizationUrl = new URL("https://provider.example/authorize?state=mcp-state");
  const wrapped = await wrapLocalMcpAuthorizationState(authorizationUrl, local, RELAY_KEY);
  assert.equal(wrapped, authorizationUrl);
  const envelope = await verifyLocalMcpOAuthRelayState(
    wrapped.searchParams.get("state"),
    CONNECTION_ID,
    RELAY_KEY,
  );
  assert.equal(envelope?.c, CONNECTION_ID);
  assert.equal(envelope?.o, TARGET_ORIGIN);
  assert.equal(envelope?.f, "connect");
  assert.equal(envelope?.s, "mcp-state");
});

test("authorization state wrappers reject missing and oversized provider state", async () => {
  const connector = localConnectorAuthorization(TARGET_ORIGIN, "github", "connect");
  const mcp = localMcpAuthorization(TARGET_ORIGIN, CONNECTION_ID, "connect");
  assert(connector && mcp);
  await assert.rejects(
    wrapLocalConnectorAuthorizationState(new URL("https://provider.example/authorize"), connector, RELAY_KEY),
    /invalid local connector authorization state/,
  );
  await assert.rejects(
    wrapLocalMcpAuthorizationState(
      new URL(`https://provider.example/authorize?state=${"x".repeat(513)}`),
      mcp,
      RELAY_KEY,
    ),
    /invalid local MCP authorization state/,
  );
});
