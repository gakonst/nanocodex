import { describe, expect, it, vi } from "vitest";

import { connectorToolMetadata } from "../src/connector-tools";
import { accountInfo, projectAccountInfo } from "../src/account-info";
import { X_API } from "nanocodex-tools/x";

const A = "a".repeat(43);
const B = "b".repeat(43);
const LOGIN_ID = "l".repeat(22);
const CARD_ID = "c".repeat(22);
const ADDRESS_ID = "a".repeat(22);
const PHONE_ID = "p".repeat(22);

describe("managed account info", () => {
  it("preserves offline hand status through account discovery and projection", async () => {
    const machines = [{
      id: "user:desktop-vm", name: "Desktop VM", kind: "user" as const,
      mount: "/desktop-vm", workspace: "/desktop-vm", capabilities: ["shell"], online: false,
    }];
    const info = await accountInfo({ fetch: async () => Response.json(statuses()) }, "user", {
      enabled: true, machines,
    });
    expect(projectAccountInfo(info, [], {}).machines).toEqual(machines);
    expect(JSON.stringify(projectAccountInfo(info, [], {}))).toContain('"online":false');
  });

  it("discovers native public APIs without granting X connector access", async () => {
    for (const enabled of [true, false]) {
      const info = await accountInfo({ fetch: async () => new Response(null, { status: 503 }) },
        "user", { enabled, apis: [X_API], allowedConnectors: [] });
      expect(info.apis).toEqual([X_API]);
      expect(info.authenticated).toEqual([]);
      expect(projectAccountInfo(info, [], {}).apis).toEqual([X_API]);
    }
  });

  it("forwards cancellation to every broker request and preserves its reason", async () => {
    const controller = new AbortController();
    const reason = new Error("turn cancelled");
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      expect(init?.signal).toBe(controller.signal);
      if (String(input).endsWith("/connectors")) return Promise.resolve(Response.json(statuses()));
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal?.reason), { once: true });
      });
    });

    const pending = accountInfo(
      { fetch },
      "user",
      { enabled: true, signal: controller.signal },
    );
    controller.abort(reason);

    await expect(pending).rejects.toBe(reason);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("projects provider-neutral Google and Slack connection identities", async () => {
    const fetch = vi.fn(async () => Response.json(statuses()));
    const machines = [{
      id: "sandbox",
      name: "Agent sandbox",
      kind: "sandbox" as const,
      provider: "cloudflare",
      mount: "/sandbox",
      workspace: "/sandbox",
      capabilities: ["filesystem", "native-linux"],
    }];
    const info = await accountInfo(
      { fetch },
      "user/with spaces",
      { enabled: true, machines },
    );

    expect(info).toEqual({
      status: "ready",
      apis: [],
      authenticated: ["gmail", "gdrive", "slack"],
      connectorTools: connectorToolMetadata(["gmail", "gdrive", "slack"]),
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
      machines,
      identity: {},
      stablecoins: [],
      authorizations: [],
      vault: [],
    });
    expect(info.machines[0]).toMatchObject({ mount: "/sandbox", workspace: "/sandbox" });
    expect(JSON.stringify(info)).not.toMatch(/access_token|secret/);
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
    );
  });

  it("filters exact grant connection IDs and withholds selectors from legacy grants", async () => {
    const binding = { fetch: async () => Response.json(statuses()) };
    const exact = await accountInfo(binding, "user", {
      allowedConnectors: ["gmail", "slack"],
      allowedConnections: { gmail: [B], slack: [] },
      enabled: true,
    });
    expect(exact.authenticated).toEqual(["gmail"]);
    expect(exact.accounts).toEqual({ gmail: "home@example.com" });
    expect(exact.connectorAccounts).toEqual({
      gmail: [{ id: B, label: "home@example.com", accountId: "google-home", capabilities: ["gmail"] }],
    });

    const legacyGrant = await accountInfo(binding, "user", {
      allowedConnectors: ["gmail"],
      enabled: true,
    });
    expect(legacyGrant.authenticated).toEqual(["gmail"]);
    expect(legacyGrant.connectorAccounts).toEqual({});
  });

  it("preserves legacy singleton status and upgrades retained snapshots with the new field", async () => {
    const legacy = await accountInfo({
      fetch: async () => Response.json({ connectors: {
        github: { connected: true, label: "octocat", account_id: "legacy-id" },
      } }),
    }, "user", { enabled: true });
    expect(legacy.authenticated).toEqual(["github"]);
    expect(legacy.accounts).toEqual({ github: "octocat" });
    expect(legacy.connectorAccounts).toEqual({});

    const retained = { ...legacy } as any;
    delete retained.connectorAccounts;
    delete retained.machines;
    expect(projectAccountInfo(retained)).toMatchObject({
      connectorAccounts: {},
      connectorTools: connectorToolMetadata(["github"]),
      machines: [],
    });
  });

  it("fails closed on malformed connection metadata", async () => {
    const unavailable = await accountInfo({
      fetch: async () => Response.json({ connectors: {
        github: { connected: true, connections: [{ id: "not-opaque", label: "bad" }] },
      } }),
    }, "user", { enabled: true });
    expect(unavailable).toMatchObject({ status: "unavailable", connectorAccounts: {} });

  });

  it("preserves available hands when connector status is unavailable", async () => {
    const machines = [{
      id: "sandbox",
      name: "Agent sandbox",
      kind: "sandbox" as const,
      provider: "cloudflare",
      mount: "/sandbox",
      workspace: "/sandbox",
      capabilities: ["native-linux"],
    }];
    const info = await accountInfo(
      { fetch: async () => new Response(null, { status: 503 }) },
      "user",
      { enabled: true, machines },
    );
    expect(info).toMatchObject({ status: "unavailable", machines });
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

describe("managed accountInfo vault projection", () => {

  it("preserves API-key and login metadata in live and retained account snapshots", async () => {
    const vault = [
      { id: "k".repeat(43), kind: "api_key", name: "Example API", created_at: 1 },
      { id: LOGIN_ID, kind: "login", name: "Example login", created_at: 2,
        username: "person", browser_origin: "https://example.com" },
    ];
    const result = await accountInfo({
      fetch: async input => Response.json(
        String(input).endsWith("/connectors") ? { connectors: {} } : { vault },
      ),
    }, "user", { enabled: true });
    expect(result.vault).toEqual(vault);
    expect(projectAccountInfo(result).vault).toEqual(vault);
    expect(projectAccountInfo(result, []).vault).toEqual(vault);
  });

  it.each(["api_key", "value", "secret", "password"])(
    "rejects unexpected %s fields on API-key metadata",
    async field => {
      const result = await accountInfo({
        fetch: async input => Response.json(
          String(input).endsWith("/connectors") ? { connectors: {} } : {
            vault: [{ id: "k".repeat(43), kind: "api_key", name: "Example API",
              created_at: 1, [field]: "synthetic-secret" }],
          },
        ),
      }, "user", { enabled: true });
      expect(result.vault).toEqual([]);
      expect(JSON.stringify(result)).not.toContain("synthetic-secret");
    },
  );

  it("preserves approved login origins and rejects malformed origin metadata", async () => {
    for (const browser_origin of ["https://www.amazon.com", "http://www.amazon.com", "https://www.amazon.com/path"]) {
      const entry = { id: LOGIN_ID, kind: "login", name: "Amazon", created_at: 1, username: "person", browser_origin };
      const info = await accountInfo({ fetch: async input => Response.json(String(input).endsWith("/connectors") ? { connectors: {} } : { vault: [entry] }) }, "user", { enabled: true });
      expect(info.vault).toEqual(browser_origin === "https://www.amazon.com" ? [entry] : []);
    }
  });

  it("projects exact safe metadata for every Vault kind and preserves connector filtering", async () => {
    const fetch = vi.fn(async (input: RequestInfo | URL) => Response.json(
      String(input).endsWith("/connectors") ? {
        connectors: {
          github: { connected: true, label: "octocat", access_token: "secret" },
          gmail: { connected: true, label: "private@example.com" },
        },
      } : { vault: [
        { id: LOGIN_ID, kind: "login", name: "Example", created_at: 1, username: "octocat" },
        { id: CARD_ID, kind: "card", name: "Work card", created_at: 2, last4: "4242" },
        {
          id: ADDRESS_ID,
          kind: "address",
          name: "Office",
          created_at: 3,
          address_line_1: "1 Main Street",
          address_line_2: "Suite 2",
          city: "Athens",
          state: "Attica",
          zip: "10557",
          country: "GR",
        },
        { id: PHONE_ID, kind: "phone", name: "Mobile", created_at: 4, phone_number: "+301234567890" },
      ] },
    ));

    const result = await accountInfo({ fetch }, "user/id", {
      allowedConnectors: ["github"],
      enabled: true,
    });

    expect(result).toEqual({
      status: "ready",
      apis: [],
      authenticated: ["github"],
      accounts: { github: "octocat" },
      connectorAccounts: {},
      connectorTools: connectorToolMetadata(["github"]),
      machines: [],
      identity: {},
      stablecoins: [],
      authorizations: [],
      vault: [
        { id: LOGIN_ID, kind: "login", name: "Example", created_at: 1, username: "octocat" },
        { id: CARD_ID, kind: "card", name: "Work card", created_at: 2, last4: "4242" },
        {
          id: ADDRESS_ID,
          kind: "address",
          name: "Office",
          created_at: 3,
          address_line_1: "1 Main Street",
          address_line_2: "Suite 2",
          city: "Athens",
          state: "Attica",
          zip: "10557",
          country: "GR",
        },
        { id: PHONE_ID, kind: "phone", name: "Mobile", created_at: 4, phone_number: "+301234567890" },
      ],
    });
    expect(JSON.stringify(result)).not.toMatch(/access_token|secret|private@example\.com/);
    expect(projectAccountInfo(result, [])).toMatchObject({ authenticated: [], accounts: {}, vault: result.vault });
    expect(fetch.mock.calls.map(([input]) => String(input))).toEqual([
      "https://broker.internal/users/user%2Fid/connectors",
      "https://broker.internal/users/user%2Fid/credentials/vault",
    ]);
  });

  it.each([
    undefined,
    {},
    [{ id: LOGIN_ID, kind: "login", name: "Example", created_at: 1 }],
    [{ id: LOGIN_ID, kind: "login", name: "Example", created_at: 1, username: "octocat", password: "secret" }],
    [
      { id: PHONE_ID, kind: "phone", name: "Mobile", created_at: 1, phone_number: "+301234567890" },
      { id: CARD_ID, kind: "card", name: "Work", created_at: "2", last4: "4242" },
    ],
  ])("fails the entire Vault projection closed for %j", async (vault) => {
    const result = await accountInfo({
      fetch: async (input) => Response.json(
        String(input).endsWith("/connectors") ? { connectors: {} } : { vault },
      ),
    }, "user", { enabled: true });

    expect(result.status).toBe("ready");
    expect(result.vault).toEqual([]);
    expect(JSON.stringify(result)).not.toContain("secret");
  });

  it("rejects Vault metadata outside broker-compatible bounds", async () => {
    const malformedVaults = [
      [{ id: "short", kind: "login", name: "Example", created_at: 1, username: "octocat" }],
      [{ id: LOGIN_ID, kind: "login", name: " Example", created_at: 1, username: "octocat" }],
      [{ id: LOGIN_ID, kind: "login", name: "Example", created_at: 1, username: "octo\ncat" }],
      [{ id: CARD_ID, kind: "card", name: "Card", created_at: 1, last4: "123" }],
      [{
        id: ADDRESS_ID,
        kind: "address",
        name: "Office",
        created_at: 1,
        address_line_1: "a".repeat(257),
        city: "Athens",
        state: "Attica",
        zip: "10557",
        country: "GR",
      }],
      Array.from({ length: 101 }, (_, index) => ({
        id: index.toString().padStart(22, "p"),
        kind: "phone",
        name: "Mobile",
        created_at: index,
        phone_number: "+301234567890",
      })),
    ];
    for (const vault of malformedVaults) {
      const result = await accountInfo({
        fetch: async (input) => Response.json(
          String(input).endsWith("/connectors") ? { connectors: {} } : { vault },
        ),
      }, "user", { enabled: true });

      expect(result.vault).toEqual([]);
    }
  });

  it("includes an empty required Vault field in disabled and unavailable results", async () => {
    await expect(accountInfo(
      { fetch: vi.fn() },
      "user",
      { enabled: false },
    )).resolves.toMatchObject({ vault: [] });
    await expect(accountInfo({
      fetch: async () => new Response(null, { status: 503 }),
    }, "user", { enabled: true })).resolves.toMatchObject({ status: "unavailable", vault: [] });
  });

  it("keeps connector information ready when only credential metadata is unavailable", async () => {
    const result = await accountInfo({
      fetch: async (input) => String(input).endsWith("/connectors")
        ? Response.json({ connectors: { github: { connected: true, label: "octocat" } } })
        : new Response(null, { status: 503 }),
    }, "user", { enabled: true });

    expect(result).toMatchObject({
      status: "ready",
      apis: [],
      authenticated: ["github"],
      accounts: { github: "octocat" },
      vault: [],
    });
  });

  it("normalizes a retained legacy snapshot without Vault metadata", () => {
    const legacy = {
      status: "ready",
      apis: [],
      authenticated: ["github"],
      accounts: { github: "octocat" },
      identity: {},
      stablecoins: [],
      authorizations: [],
    } as unknown as Parameters<typeof projectAccountInfo>[0];

    expect(projectAccountInfo(legacy)).toEqual({
      ...legacy,
      connectorAccounts: {},
      connectorTools: connectorToolMetadata(["github"]),
      machines: [],
      vault: [],
    });
  });
});

describe("computer VM placement discovery", () => {
  it("exposes only the exact provider from an online computer and fences stale metadata", async () => {
    const { projectHandProviders } = await import("../src/account-info");
    const base = { id: "mac", name: "gak-9", kind: "user" as const, workspace: "/mac", mount: "/mac", online: true,
      capabilities: ["native", "vm_factory:mac-1234"] };
    expect(projectHandProviders([base])[0]).toMatchObject({ vm_provider: "mac-1234" });
    for (const machine of [
      { ...base, online: false, vm_provider: "stale" },
      { ...base, capabilities: ["vm_factory:cf_sandbox"] },
      { ...base, capabilities: ["vm_factory:a", "vm_factory:b"] },
    ]) expect(projectHandProviders([machine])[0]).not.toHaveProperty("vm_provider");
  });
});
