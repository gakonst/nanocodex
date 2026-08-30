import { describe, expect, it, vi } from "vitest";

import { accountInfo, withInitialAccountInfo } from "../src/account-info";

describe("account info", () => {
  it("reports authenticated connector names and display labels only", async () => {
    const fetch = vi.fn(async () => Response.json({
      connectors: {
        github: { connected: true, account_id: "secret-account", label: "Nano Cat (nanocat)" },
        gmail: { connected: false },
        gdrive: { connected: true, access_token: "secret-token" },
        x: { connected: true, account_id: "secret-x-account", label: "Nano Cat (@nanocat)" },
        whoop: { connected: true, account_id: "secret-whoop-account", label: "Nano Athlete" },
      },
    }));

    const info = await accountInfo({ fetch }, "user/with spaces", true);

    expect(info).toEqual({
      status: "ready",
      authenticated: ["github", "gdrive", "x", "whoop"],
      accounts: { github: "Nano Cat (nanocat)", x: "Nano Cat (@nanocat)", whoop: "Nano Athlete" },
      identity: {},
      stablecoins: [],
      authorizations: [],
    });
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
    );
    expect(JSON.stringify(info)).not.toMatch(/secret-account|secret-token|secret-x-account|secret-whoop-account/);
  });

  it("fails closed when status is unavailable or malformed", async () => {
    expect(await accountInfo({
      fetch: async () => Response.json({ error: "down" }, { status: 503 }),
    }, "user", true)).toEqual({
      status: "unavailable", authenticated: [], accounts: {}, identity: {}, stablecoins: [], authorizations: [],
    });
    expect(await accountInfo({
      fetch: async () => Response.json({ connectors: null }),
    }, "user", true)).toEqual({
      status: "unavailable", authenticated: [], accounts: {}, identity: {}, stablecoins: [], authorizations: [],
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

  it("does not query account connectors for shared rooms", async () => {
    const fetch = vi.fn(async () => Response.json({}));
    expect(await accountInfo({ fetch }, "owner", false)).toEqual({
      status: "disabled",
      authenticated: [],
      accounts: {},
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
