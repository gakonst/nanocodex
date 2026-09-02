import assert from "node:assert/strict";
import test from "node:test";

import {
  applyConnectorConnectionSelector,
  completeConnectorConnectionSnapshot,
  ConnectorPolicyFailure,
  connectorCapabilities,
  connectorCapabilityForUrl,
  connectorConnectionSnapshot,
  connectorProvider,
  connectorRequestTarget,
  intersectConnectorConnectionSnapshot,
  isConnectorConnectionSnapshot,
  publicConnectorStatus,
  resolveConnectorConnection,
} from "../src/connectorPolicy.mts";

const alpha = "a".repeat(43);
const bravo = "b".repeat(43);
const charlie = "c".repeat(43);

test("capabilities stay provider-neutral while Google shares one OAuth control provider", () => {
  assert.deepEqual(connectorCapabilities, [
    "github", "gmail", "gdrive", "gcalendar", "gtasks", "gdocs",
    "gsheets", "gslides", "gcontacts", "slack", "x", "chatgpt",
  ]);
  for (const capability of [
    "gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts",
  ]) assert.equal(connectorProvider(capability), "google");
  assert.equal(connectorProvider("slack"), "slack");
  assert.equal(connectorProvider("google"), undefined);
});

test("status projection exposes bounded identities and strips provider secrets", () => {
  const status = publicConnectorStatus({
    connected: true,
    access_token: "must-not-surface",
    connections: [{
      id: alpha,
      label: "  Primary workspace  ",
      account_id: "  U123  ",
      capabilities: ["slack"],
      refresh_token: "must-not-surface",
    }],
  });
  assert.deepEqual(status, {
    connected: true,
    connections: [{
      id: alpha,
      label: "Primary workspace",
      account_id: "U123",
      capabilities: ["slack"],
    }],
    label: "Primary workspace",
    account_id: "U123",
  });
  assert.equal(JSON.stringify(status).includes("token"), false);
  assert.equal(Object.isFrozen(status), true);
  assert.equal(Object.isFrozen(status.connections), true);
});

test("mixed-version status readers retain legacy top-level metadata", () => {
  assert.deepEqual(publicConnectorStatus({
    connected: true,
    label: "  legacy@example.com ",
    account_id: " subject-1 ",
  }), {
    connected: true,
    connections: [],
    label: "legacy@example.com",
    account_id: "subject-1",
  });
  assert.deepEqual(publicConnectorStatus({
    connected: true,
    connection_id: alpha,
    label: " first rollout ",
    account_id: "account",
  }).connections, [{ id: alpha, label: "first rollout", account_id: "account" }]);
});

test("invalid and duplicate broker identities fail closed", () => {
  assert.throws(
    () => publicConnectorStatus({ connected: true, connections: [{ id: "workspace", label: "bad" }] }),
    (error) => error instanceof ConnectorPolicyFailure && error.code === "connector_broker_invalid",
  );
  assert.throws(
    () => publicConnectorStatus({
      connected: true,
      connections: [{ id: alpha, label: "one" }, { id: alpha, label: "two" }],
    }),
    (error) => error instanceof ConnectorPolicyFailure && error.status === 502,
  );
});

test("approval snapshots are immutable and grant intersection cannot broaden them", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [{ id: alpha, label: "A" }, { id: bravo, label: "B" }],
  });
  const approval = connectorConnectionSnapshot(statuses);
  assert.deepEqual(approval, { gmail: [alpha, bravo] });
  assert.equal(Object.isFrozen(approval), true);
  assert.equal(Object.isFrozen(approval.gmail), true);

  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [{ id: bravo, label: "B" }, { id: charlie, label: "connected later" }],
  });
  assert.deepEqual(intersectConnectorConnectionSnapshot(approval, statuses, ["gmail"]), {
    gmail: [bravo],
  });
  const grant = intersectConnectorConnectionSnapshot(approval, statuses, ["gmail"]);
  assert.throws(
    () => resolveConnectorConnection(grant, "gmail", charlie),
    (error) => error.code === "connector_connection_not_granted",
  );
  assert.equal(isConnectorConnectionSnapshot({ gmail: [alpha], google: [bravo] }), false);
});

test("fresh-account exchange binds only requested post-approval identities", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [{ id: alpha, label: "Approved Gmail" }],
  });
  statuses.slack = publicConnectorStatus({
    connected: true,
    connections: [{ id: bravo, label: "Unrequested Slack" }],
  });

  const completed = completeConnectorConnectionSnapshot({}, [], statuses, ["gmail"]);
  assert.deepEqual(completed, {
    connectorConnections: { gmail: [alpha] },
    legacyConnectorCapabilities: [],
  });
  assert.equal(resolveConnectorConnection(completed.connectorConnections, "gmail"), alpha);
  assert.equal(Object.hasOwn(completed.connectorConnections, "slack"), false);
});

test("exchange keeps pre-approved identities exact and rejects pre-rollout approvals", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [
      { id: alpha, label: "Approved" },
      { id: bravo, label: "Connected later" },
    ],
  });

  assert.deepEqual(
    completeConnectorConnectionSnapshot({ gmail: [alpha] }, ["gmail"], statuses, ["gmail"]),
    { connectorConnections: { gmail: [alpha] }, legacyConnectorCapabilities: [] },
  );
  assert.throws(
    () => completeConnectorConnectionSnapshot(undefined, undefined, statuses, ["gmail"]),
    (error) => error.code === "connector_approval_snapshot_required" && error.status === 409,
  );
});

test("legacy singleton status gets an explicit selector-less grant mode", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    label: "legacy@example.com",
    account_id: "legacy-subject",
  });

  assert.deepEqual(
    completeConnectorConnectionSnapshot({}, ["gmail"], statuses, ["gmail"]),
    { connectorConnections: undefined, legacyConnectorCapabilities: ["gmail"] },
  );
  assert.throws(
    () => completeConnectorConnectionSnapshot(undefined, undefined, statuses, ["gmail"]),
    (error) => error.code === "connector_approval_snapshot_required",
  );
  assert.equal(resolveConnectorConnection(undefined, "gmail"), undefined);
  assert.throws(
    () => resolveConnectorConnection(undefined, "gmail", alpha),
    (error) => error.code === "connector_connection_not_granted",
  );
});

test("new grants reject mixed exact and legacy identity modes", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [{ id: alpha, label: "Exact" }],
  });
  statuses.slack = publicConnectorStatus({
    connected: true,
    label: "Legacy",
    account_id: "legacy-team",
  });
  assert.throws(
    () => completeConnectorConnectionSnapshot(
      { gmail: [alpha] },
      ["gmail", "slack"],
      statuses,
      ["gmail", "slack"],
    ),
    (error) => error.code === "connector_identity_modes_mixed",
  );
});

test("late binding requires an explicit disconnected-at-approval record", () => {
  const statuses = Object.fromEntries(connectorCapabilities.map((capability) => [
    capability,
    { connected: false, connections: [] },
  ]));
  statuses.gmail = publicConnectorStatus({
    connected: true,
    connections: [{ id: alpha, label: "Live" }],
  });
  assert.throws(
    () => completeConnectorConnectionSnapshot({}, undefined, statuses, ["gmail"]),
    (error) => error.code === "connector_approval_snapshot_incomplete",
  );
});

test("selector routing auto-selects one and requires an explicit granted identity for many", () => {
  assert.equal(resolveConnectorConnection({ gmail: [alpha] }, "gmail"), alpha);
  assert.equal(resolveConnectorConnection({ gmail: [alpha, bravo] }, "gmail", bravo), bravo);
  assert.throws(
    () => resolveConnectorConnection({ gmail: [alpha, bravo] }, "gmail"),
    (error) => error.code === "connector_connection_required" && error.status === 409,
  );
  assert.throws(
    () => resolveConnectorConnection({ gmail: [alpha] }, "gmail", bravo),
    (error) => error.code === "connector_connection_not_granted" && error.status === 403,
  );
  assert.throws(
    () => resolveConnectorConnection({ gmail: [alpha] }, "gmail", "not-an-id"),
    (error) => error.code === "connector_connection_invalid" && error.status === 400,
  );
  assert.throws(
    () => resolveConnectorConnection({ gmail: [alpha, bravo] }, "gmail", alpha, bravo),
    (error) => error.code === "connector_connection_invalid",
  );
});

test("legacy grants remain selector-less and cannot opt into later identities", () => {
  assert.equal(resolveConnectorConnection(undefined, "slack"), undefined);
  assert.throws(
    () => resolveConnectorConnection(undefined, "slack", alpha),
    (error) => error.code === "connector_connection_not_granted" && error.status === 403,
  );
});

test("routing forwards only the generic selector header after grant validation", () => {
  const headers = new Headers({ accept: "application/json" });
  applyConnectorConnectionSelector(headers, { slack: [alpha] }, "slack", alpha);
  assert.deepEqual([...headers], [
    ["accept", "application/json"],
    ["x-nanocodex-connector-connection", alpha],
  ]);
  assert.equal(headers.has("x-nanocodex-connector-instance"), false);
});

test("provider URL routing covers unified Google capabilities and Slack narrowly", () => {
  const routes = [
    ["gmail", "https://gmail.googleapis.com/gmail/v1/users/me/messages"],
    ["gdrive", "https://www.googleapis.com/drive/v3/files"],
    ["gcalendar", "https://www.googleapis.com/calendar/v3/calendars/primary/events"],
    ["gtasks", "https://tasks.googleapis.com/tasks/v1/lists"],
    ["gdocs", "https://docs.googleapis.com/v1/documents/doc-1"],
    ["gsheets", "https://sheets.googleapis.com/v4/spreadsheets/sheet-1"],
    ["gslides", "https://slides.googleapis.com/v1/presentations/deck-1"],
    ["gcontacts", "https://people.googleapis.com/v1/people/me/connections"],
    ["gcontacts", "https://people.googleapis.com/v1/people:searchContacts"],
    ["gcontacts", "https://people.googleapis.com/v1/contactGroups:batchGet"],
    ["gcontacts", "https://people.googleapis.com/v1/otherContacts:search"],
    ["slack", "https://slack.com/api/conversations.list"],
  ];
  for (const [capability, href] of routes) {
    const url = new URL(href);
    assert.equal(connectorCapabilityForUrl(url), capability);
    assert.equal(connectorRequestTarget(capability, `${url.pathname}${url.search}`).href, href);
  }
  assert.equal(
    connectorCapabilityForUrl(new URL("https://www.googleapis.com/oauth2/v3/userinfo")),
    undefined,
  );
  assert.throws(
    () => connectorRequestTarget("slack", "/api/../admin"),
    (error) => error.code === "connector_destination_denied",
  );
  assert.throws(
    () => connectorRequestTarget("gdocs", "/v1/documents/doc-1?access_token=secret"),
    (error) => error.code === "connector_destination_denied",
  );
  assert.throws(
    () => connectorRequestTarget("gcontacts", "/v1/peopleAdmin:searchContacts"),
    (error) => error.code === "connector_destination_denied",
  );
});
