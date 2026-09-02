import assert from "node:assert/strict";
import test from "node:test";

import {
  connectorAttemptedCapabilitiesConnected,
  connectorCapabilityIds,
  connectorConnectionHeader,
  connectorControlsForCapabilities,
  connectorConnectionsForCapabilities,
  connectorProviderFor,
  connectorProviderMatchesCapabilities,
  connectorStatusesFromWire,
  googleConnectorCapabilities,
} from "nanocodex-connect-ui/connectorPolicy.mjs";

const GOOGLE_ID = "g".repeat(43);
const SLACK_ID = "s".repeat(43);

test("Google remains one provider while every Workspace service stays an exact capability", () => {
  assert.deepEqual(googleConnectorCapabilities, [
    "gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts",
  ]);
  assert.equal(connectorCapabilityIds.includes("google"), false);
  assert.equal(connectorProviderFor("gcalendar"), "google");
  assert.equal(connectorProviderMatchesCapabilities("google", ["gmail", "gdocs"]), true);
  assert.equal(connectorProviderMatchesCapabilities("google", ["github", "slack"]), false);
});

test("provider-neutral statuses merge one identity across partial Google consent", () => {
  const statuses = connectorStatusesFromWire({
    gmail: {
      connected: true,
      connections: [{
        id: GOOGLE_ID,
        label: "  person@example.test  ",
        account_id: " google-person ",
        capabilities: ["gmail", "gdrive"],
      }],
    },
    gdrive: {
      connected: true,
      connections: [{
        id: GOOGLE_ID,
        label: "person@example.test",
        account_id: "google-person",
        capabilities: ["gmail", "gdrive"],
      }],
    },
    gcalendar: { connected: false, connections: [] },
    slack: {
      connected: true,
      connections: [{ id: SLACK_ID, label: "Acme Workspace · Ada" }],
    },
  });
  assert.deepEqual(connectorConnectionsForCapabilities(statuses, googleConnectorCapabilities), [{
    id: GOOGLE_ID,
    label: "person@example.test",
    account_id: "google-person",
    capabilities: ["gmail", "gdrive"],
  }]);
  assert.equal(statuses.gcalendar.connected, false);
  assert.equal(statuses.slack.connections[0].label, "Acme Workspace · Ada");

  const controls = connectorControlsForCapabilities(
    ["gmail", "gdrive", "gcalendar", "github"],
    statuses,
  );
  assert.equal(controls.filter(({ provider }) => provider === "google").length, 1);
  assert.deepEqual(controls[0], {
    provider: "google",
    capabilities: ["gmail", "gdrive", "gcalendar"],
    connectedCapabilities: ["gmail", "gdrive"],
    missingCapabilities: ["gcalendar"],
    connections: [{
      id: GOOGLE_ID,
      label: "person@example.test",
      account_id: "google-person",
      capabilities: ["gmail", "gdrive"],
    }],
    connected: false,
    partial: true,
    resolved: true,
  });
});

test("OAuth completion counts attempted missing capabilities, not pre-existing grants", () => {
  const statuses = {
    gmail: { connected: true, connections: [] },
    gdrive: { connected: false, connections: [] },
  };
  assert.equal(connectorAttemptedCapabilitiesConnected(["gdrive"], statuses), false);
  assert.equal(connectorAttemptedCapabilitiesConnected(["gmail"], statuses), true);
  assert.equal(connectorAttemptedCapabilitiesConnected(["gdrive"], {
    ...statuses,
    gdrive: { connected: true, connections: [] },
  }), true);
});

test("legacy singleton labels remain displayable without inventing opaque IDs", () => {
  const statuses = connectorStatusesFromWire({
    github: { connected: true, account_id: "octocat", label: "octocat" },
    gmail: { connected: true, account_id: "person@example.test", label: "person@example.test" },
    gdrive: { connected: false },
  });
  assert.deepEqual(statuses, {
    github: {
      connected: true,
      connections: [],
      account_id: "octocat",
      label: "octocat",
    },
    gmail: {
      connected: true,
      connections: [],
      account_id: "person@example.test",
      label: "person@example.test",
    },
    gdrive: { connected: false, connections: [] },
  });
  assert.deepEqual(connectorControlsForCapabilities(
    ["gmail", "gdrive", "gcalendar"],
    statuses,
  )[0], {
    provider: "google",
    capabilities: ["gmail", "gdrive", "gcalendar"],
    connectedCapabilities: ["gmail"],
    missingCapabilities: ["gdrive", "gcalendar"],
    connections: [],
    connected: false,
    partial: true,
    resolved: true,
  });
});

test("status projection rejects secrets, malformed identities, duplicates, and unknown capabilities", () => {
  for (const value of [
    { google: { connected: true, connections: [] } },
    { gmail: { connected: true, token: "secret", connections: [] } },
    { gmail: { connected: true, connections: [{ id: "short", label: "person@example.test" }] } },
    { gmail: { connected: true, connections: [{ id: GOOGLE_ID, label: " ".repeat(3) }] } },
    { gmail: { connected: true, connections: [{ id: GOOGLE_ID, label: "x".repeat(257) }] } },
    { gmail: { connected: true, connections: [
      { id: GOOGLE_ID, label: "one@example.test" },
      { id: GOOGLE_ID, label: "one@example.test" },
    ] } },
    { gmail: { connected: true, connections: [{
      id: GOOGLE_ID,
      label: "one@example.test",
      capabilities: ["google"],
    }] } },
    { slack: { connected: true, connections: [{
      id: SLACK_ID,
      label: "Acme Workspace · Ada",
      capabilities: ["gmail"],
    }] } },
    { gmail: { connected: true, connections: [{
      id: GOOGLE_ID,
      label: "one@example.test",
      capabilities: ["slack"],
    }] } },
  ]) assert.throws(() => connectorStatusesFromWire(value), /invalid connector statuses/);
});

test("the generic runtime selector has one exact public header name", () => {
  assert.equal(connectorConnectionHeader, "X-Nanocodex-Connector-Connection");
});
