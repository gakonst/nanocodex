import { describe, expect, it, vi } from "vitest";

import { accountInfo, projectAccountInfo, withInitialAccountInfo } from "../src/account-info";

const A = "a".repeat(43);
const B = "b".repeat(43);

describe("managed account info", () => {
  it("projects provider-neutral Google and Slack connection identities", async () => {
    const fetch = vi.fn(async () => Response.json(statuses()));
    const info = await accountInfo({ fetch }, "user/with spaces", true);

    expect(info).toEqual({
      status: "ready",
      authenticated: ["gmail", "gdrive", "slack"],
      accounts: { gdrive: "work@example.com", slack: "Acme (U123)" },
      connectorAccounts: {
        gmail: [
          { id: A, label: "work@example.com", accountId: "google-work", capabilities: ["gmail", "gdrive"] },
          { id: B, label: "home@example.com", accountId: "google-home", capabilities: ["gmail"] },
        ],
        gdrive: [
          { id: A, label: "work@example.com", accountId: "google-work", capabilities: ["gmail", "gdrive"] },
        ],
        slack: [{ id: B, label: "Acme (U123)", accountId: "T123:U123", capabilities: ["slack"] }],
      },
      identity: {},
      stablecoins: [],
      authorizations: [],
    });
    expect(JSON.stringify(info)).not.toMatch(/access_token|secret/);
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
    );
  });

  it("filters exact grant connection IDs and withholds selectors from legacy grants", async () => {
    const binding = { fetch: async () => Response.json(statuses()) };
    const exact = await accountInfo(binding, "user", true, ["gmail", "slack"], {
      gmail: [B],
      slack: [],
    });
    expect(exact.authenticated).toEqual(["gmail"]);
    expect(exact.accounts).toEqual({ gmail: "home@example.com" });
    expect(exact.connectorAccounts).toEqual({
      gmail: [{ id: B, label: "home@example.com", accountId: "google-home", capabilities: ["gmail"] }],
    });

    const legacyGrant = await accountInfo(binding, "user", true, ["gmail"]);
    expect(legacyGrant.authenticated).toEqual(["gmail"]);
    expect(legacyGrant.connectorAccounts).toEqual({});
  });

  it("preserves legacy singleton status and upgrades retained snapshots with the new field", async () => {
    const legacy = await accountInfo({
      fetch: async () => Response.json({ connectors: {
        github: { connected: true, label: "octocat", account_id: "legacy-id" },
      } }),
    }, "user", true);
    expect(legacy.authenticated).toEqual(["github"]);
    expect(legacy.accounts).toEqual({ github: "octocat" });
    expect(legacy.connectorAccounts).toEqual({});

    const retained = { ...legacy } as any;
    delete retained.connectorAccounts;
    expect(projectAccountInfo(retained).connectorAccounts).toEqual({});
  });

  it("fails closed on malformed connection metadata and documents the generic selector", async () => {
    const unavailable = await accountInfo({
      fetch: async () => Response.json({ connectors: {
        github: { connected: true, connections: [{ id: "not-opaque", label: "bad" }] },
      } }),
    }, "user", true);
    expect(unavailable).toMatchObject({ status: "unavailable", connectorAccounts: {} });

    const prompt = withInitialAccountInfo("Use my calendar", unavailable);
    expect(JSON.stringify(prompt)).toContain("X-Nanocodex-Connector-Connection");
    expect(JSON.stringify(prompt)).toContain("Never invent a connection id");
  });
});

function statuses() {
  const work = {
    id: A,
    label: " work@example.com ",
    account_id: "google-work",
    capabilities: ["gmail", "gdrive"],
    access_token: "secret",
  };
  return { connectors: {
    gmail: { connected: true, connections: [work, {
      id: B,
      label: "home@example.com",
      account_id: "google-home",
      capabilities: ["gmail"],
    }] },
    gdrive: { connected: true, connections: [work] },
    slack: { connected: true, connections: [{
      id: B,
      label: "Acme (U123)",
      account_id: "T123:U123",
      capabilities: ["slack"],
    }] },
  } };
}
