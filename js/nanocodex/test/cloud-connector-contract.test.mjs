import assert from "node:assert/strict";
import { test } from "node:test";

import {
  connectionFromWire,
  connectionMatchesRequest,
  grantFromWire,
  reconnectRequestFromConnection,
} from "../cloud/internal.mjs";

const A = "a".repeat(43);
const B = "b".repeat(43);

test("cloud grants preserve exact service capability connection selections", () => {
  const connection = connectionFromWire(wire({
    capabilities: [
      "nanocodex.agent",
      "gmail",
      "gdrive",
      "gcalendar",
      "gtasks",
      "gdocs",
      "gsheets",
      "gslides",
      "gcontacts",
      "slack",
      "spotify",
      "soundcloud",
      "link",
    ],
    connectorConnections: {
      gmail: [A, B],
      gdrive: [A],
      gcalendar: [A],
      gtasks: [A],
      gdocs: [A],
      gsheets: [A],
      gslides: [A],
      gcontacts: [A],
      slack: [B],
      spotify: [A],
      soundcloud: [B],
      link: [A],
    },
  }));

  assert.deepEqual(connection.grant.connectors, [
    "gmail",
    "gdrive",
    "gcalendar",
    "gtasks",
    "gdocs",
    "gsheets",
    "gslides",
    "gcontacts",
    "slack",
    "spotify",
    "soundcloud",
    "link",
  ]);
  assert.deepEqual(connection.grant.connectorConnections, {
    gmail: [A, B],
    gdrive: [A],
    gcalendar: [A],
    gtasks: [A],
    gdocs: [A],
    gsheets: [A],
    gslides: [A],
    gcontacts: [A],
    slack: [B],
    spotify: [A],
    soundcloud: [B],
    link: [A],
  });
  assert.equal(Object.isFrozen(connection.grant.connectorConnections), true);
  assert.equal(Object.isFrozen(connection.grant.connectorConnections.gmail), true);
  const retained = reconnectRequestFromConnection(connection);
  assert.deepEqual(retained.connectorConnections, connection.grant.connectorConnections);
  assert.equal(connectionMatchesRequest(connection, retained), true);
  assert.equal(connectionMatchesRequest(connection, {
    ...retained,
    connectorConnections: { ...retained.connectorConnections, slack: [A] },
  }), false);
});

test("cloud connection readers accept legacy grants and reject widened selections", () => {
  const legacy = connectionFromWire(wire({ capabilities: ["nanocodex.agent", "slack"] }));
  assert.equal(legacy.grant.connectorConnections, undefined);

  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { github: [A] },
  })), /ungranted connector capability/);
  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { slack: ["short"] },
  })), /opaque connection ID/);
  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { slack: [A, A] },
  })), /duplicate connections/);
});

test("standalone grant readers retain exact connector selections", () => {
  const source = wire({
    capabilities: ["nanocodex.agent", "gmail", "slack"],
    connectorConnections: { gmail: [A], slack: [B] },
  }).grant;
  const grant = grantFromWire(source);

  assert.deepEqual(grant.connectorConnections, { gmail: [A], slack: [B] });
  assert.equal(Object.isFrozen(grant.connectorConnections), true);
  assert.equal(Object.isFrozen(grant.connectorConnections.gmail), true);
  assert.equal(grantFromWire({ ...source, connector_connections: undefined }).connectorConnections, undefined);
  assert.throws(() => grantFromWire({
    ...source,
    connector_connections: { github: [A] },
  }), /ungranted connector capability/);
});

function wire({ capabilities, connectorConnections }) {
  return {
    account_address: `0x${"11".repeat(20)}`,
    agent_id: "agent_connector_contract",
    authorization_mode: "hosted",
    grant: {
      id: `0x${"22".repeat(32)}`,
      permission: "agent.run",
      status: "active",
      expires_at: Math.floor(Date.now() / 1_000) + 3_600,
      capabilities,
      mcp_connections: [],
      ...(connectorConnections === undefined ? {} : {
        connector_connections: connectorConnections,
      }),
    },
  };
}


test("app connector requests use the Connect session without an agent turn", async () => {
  const { Client, Dialog, Transport, Actions } = await import("../cloud/index.mjs");
  const seen = [];
  const client = Client.create({ appId: "playlist-app", appOrigin: "https://app.example.com",
    dialog: Dialog.memory(), session: false,
    transport: Transport.http("https://api.nanocodex.xyz", { fetch: async (input, init) => {
      seen.push(new Request(input, init));
      return Response.json({ items: [] }, { status: 200 });
    } }),
  });
  client._setSessionToken("grant-session");
  const signal = new AbortController().signal;
  const result = await client.connector.request({ connector: "spotify", path: "/v1/me/playlists?limit=1", connectionId: A, signal });
  assert.equal(result.status, 200);
  assert.deepEqual(await result.json(), { items: [] });
  assert.equal(new URL(seen[0].url).pathname, "/v1/connectors/spotify/request");
  assert.equal(seen[0].headers.get("authorization"), "Bearer grant-session");
  assert.equal(seen[0].headers.get("x-nanocodex-app-id"), "playlist-app");
  assert.equal(seen[0].headers.get("origin"), "https://app.example.com");
  assert.deepEqual(await seen[0].json(), { method: "GET", path: "/v1/me/playlists?limit=1", connection_id: A });
  await Actions.connector.request(client, { connector: "soundcloud", path: "/playlists/fixture", method: "PUT", body: { title: "Renamed" } });
  assert.deepEqual(await seen[1].json(), { method: "PUT", path: "/playlists/fixture", body: { title: "Renamed" } });
  await assert.rejects(client.connector.request({ connector: "chatgpt", path: "/" }), /Unknown connector/);
  await assert.rejects(client.connector.request({ connector: "spotify", path: "//evil.example" }), /provider-relative/);
  assert.equal(seen.length, 2);
});


test("plural and provider-scoped connector APIs keep the current grant and exact account", async () => {
  const { Client, Dialog, Transport, Actions } = await import("../cloud/index.mjs");
  const seen = [];
  const client = Client.create({ appId: "playlist-app", appOrigin: "https://app.example.com",
    dialog: Dialog.memory(), session: false,
    transport: Transport.http("https://api.nanocodex.xyz", { fetch: async (input, init) => {
      seen.push(new Request(input, init));
      return Response.json({ error: "rate limited" }, { status: 429, headers: { "retry-after": "5" } });
    } }),
  });
  const services = ["github", "gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets",
    "gslides", "gcontacts", "slack", "x", "spotify", "soundcloud", "link"];
  assert.deepEqual(Object.keys(client.connectors).filter(key => key !== "request"), services);
  for (const service of services) {
    client._setSessionToken(`grant-${service}`);
    const response = await client.connectors[service].request({
      path: "/fixture", method: "POST", body: { name: "Updated" }, connectionId: A,
      // JS callers cannot accidentally override a provider-scoped client.
      connector: "other-provider",
    });
    assert.equal(response.status, 429);
    assert.equal(response.headers.get("retry-after"), "5");
    const request = seen.at(-1);
    assert.equal(new URL(request.url).pathname, `/v1/connectors/${service}/request`);
    assert.equal(request.headers.get("authorization"), `Bearer grant-${service}`);
    assert.deepEqual(await request.json(), { path: "/fixture", method: "POST", body: { name: "Updated" }, connection_id: A });
  }
  await client.connectors.request({ connector: "spotify", path: "/v1/me/playlists" });
  await Actions.connectors.request(client, { connector: "soundcloud", path: "/me/playlists" });
  assert.equal(new URL(seen.at(-2).url).pathname, "/v1/connectors/spotify/request");
  assert.equal(new URL(seen.at(-1).url).pathname, "/v1/connectors/soundcloud/request");
  assert.equal(seen.length, services.length + 2, "write failures are not automatically retried");
});
