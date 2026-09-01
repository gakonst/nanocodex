import { describe, expect, it, vi } from "vitest";

import { accountInfo, withInitialAccountInfo } from "../src/account-info";

describe("account info", () => {
  it("reports authenticated connector names and display labels only", async () => {
    const fetch = vi.fn(async () => Response.json({
      connectors: {
        github: { connected: true, account_id: "secret-account", label: "Nano Cat (nanocat)" },
        gmail: { connected: true, connections: [
          { id: "a".repeat(43), account_id: "google-one", label: "one@example.test" },
          { id: "b".repeat(43), account_id: "google-two", label: "two@example.test" },
        ] },
        gdrive: { connected: true, access_token: "secret-token" },
        x: { connected: true, account_id: "secret-x-account", label: "Nano Cat (@nanocat)" },
      },
    }));

    const info = await accountInfo({ fetch }, "user/with spaces", true);

    expect(info).toEqual({
      status: "ready",
      authenticated: ["github", "gmail", "gdrive", "x"],
      accounts: { github: "Nano Cat (nanocat)", x: "Nano Cat (@nanocat)" },
      connectorAccounts: { gmail: [
        { id: "a".repeat(43), label: "one@example.test" },
        { id: "b".repeat(43), label: "two@example.test" },
      ] },
      identity: {},
      stablecoins: [],
      authorizations: [],
    });
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
    );
    expect(JSON.stringify(info)).not.toMatch(/secret-account|secret-token|secret-x-account/);
  });

  it("fails closed when status is unavailable or malformed", async () => {
    expect(await accountInfo({
      fetch: async () => Response.json({ error: "down" }, { status: 503 }),
    }, "user", true)).toEqual({
      status: "unavailable", authenticated: [], accounts: {}, connectorAccounts: {}, identity: {}, stablecoins: [], authorizations: [],
    });
    expect(await accountInfo({
      fetch: async () => Response.json({ connectors: null }),
    }, "user", true)).toEqual({
      status: "unavailable", authenticated: [], accounts: {}, connectorAccounts: {}, identity: {}, stablecoins: [], authorizations: [],
    });
  });

  it("omits authenticated connector identities outside a Connect grant projection", async () => {
    const info = await accountInfo({
      fetch: async () => Response.json({
        connectors: {
          github: { connected: true, label: "Allowed GitHub" },
          gmail: { connected: true, label: "Private Gmail" },
          gdrive: { connected: true, label: "Private Drive" },
        },
      }),
    }, "user", true, ["github"]);

    expect(info.authenticated).toEqual(["github"]);
    expect(info.accounts).toEqual({ github: "Allowed GitHub" });
    expect(JSON.stringify(info)).not.toMatch(/Private Gmail|Private Drive|gmail|gdrive/);
  });

  it("projects only Google accounts pinned into the Connect grant", async () => {
    const first = "a".repeat(43);
    const second = "b".repeat(43);
    const info = await accountInfo({
      fetch: async () => Response.json({ connectors: {
        gmail: { connected: true, connections: [
          { id: first, account_id: "google-one", label: "one@example.test" },
          { id: second, account_id: "google-two", label: "two@example.test" },
        ] },
      } }),
    }, "user", true, ["gmail"], { gmail: [second] });

    expect(info.authenticated).toEqual(["gmail"]);
    expect(info.accounts).toEqual({ gmail: "two@example.test" });
    expect(info.connectorAccounts).toEqual({ gmail: [{ id: second, label: "two@example.test" }] });
    expect(JSON.stringify(info)).not.toContain("one@example.test");
  });

  it("does not query account connectors for shared rooms", async () => {
    const fetch = vi.fn(async () => Response.json({}));
    expect(await accountInfo({ fetch }, "owner", false)).toEqual({
      status: "disabled",
      authenticated: [],
      accounts: {},
      connectorAccounts: {},
      identity: {},
      stablecoins: [],
      authorizations: [],
    });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("prepends a resolved snapshot to the first model prompt", () => {
    const info = {
      status: "ready" as const,
      authenticated: ["github" as const],
      accounts: { github: "Nano Cat (nanocat)" },
      connectorAccounts: {},
      identity: {},
      stablecoins: [] as const,
      authorizations: [] as const,
    };

    expect(withInitialAccountInfo("inspect my repositories", info)).toEqual([
      {
        type: "text",
        text: [
          "The managed runtime already resolved the following non-secret accountInfo snapshot for this agent. Use it as the current connected-account context. Do not call accountInfo again unless the task requires state refreshed after this first prompt.",
          `<account_info>\n${JSON.stringify(info)}\n</account_info>`,
        ].join("\n\n"),
      },
      { type: "text", text: "inspect my repositories" },
    ]);
  });
});
