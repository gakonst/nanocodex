import { describe, expect, it, vi } from "vitest";

import { manageAccountConnectors } from "../src/account-connectors-tool";

const base = {
  userId: "user/with spaces",
  sessionId: "77777777-7777-4777-8777-777777777777",
  publicOrigin: "https://nanocodex.example",
  canManage: () => true,
  allowedConnectors: () => undefined,
};

describe("managed account connector tool", () => {
  it("lists every built-in connector without projecting broker credentials", async () => {
    const fetch = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>(async () => Response.json({
      connectors: {
        github: { connected: true, account_id: "private-id", label: "octocat", access_token: "secret" },
        gmail: { connected: false },
        gdrive: { connected: true, label: "reader@example.com" },
        x: { connected: false },
      },
    }));

    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
    }, { operation: "list" });

    expect(result).toEqual({
      connectors: {
        github: { connected: true, account: "octocat" },
        gmail: { connected: false },
        gdrive: { connected: true, account: "reader@example.com" },
        x: { connected: false },
      },
      supported: [
        { id: "github", name: "GitHub" },
        { id: "gmail", name: "Gmail" },
        { id: "gdrive", name: "Google Drive" },
        { id: "x", name: "X" },
      ],
    });
    expect(JSON.stringify(result)).not.toMatch(/private-id|secret/);
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
  });

  it("projects connector status through a delegated grant", async () => {
    const fetch = vi.fn(async () => Response.json({
      connectors: {
        github: { connected: true, label: "allowed" },
        gmail: { connected: true, label: "private@example.com" },
        gdrive: { connected: false },
        x: { connected: false },
      },
    }));

    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
      allowedConnectors: () => ["github"],
    }, { operation: "list" });

    expect(result).toMatchObject({
      connectors: {
        github: { connected: true, account: "allowed" },
        gmail: { connected: false },
      },
    });
    expect(JSON.stringify(result)).not.toContain("private@example.com");
  });

  it("starts Gmail authorization for the exact requested email", async () => {
    const authorizationUrl = providerAuthorizationUrl("gmail");
    const fetch = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>(async () => Response.json({
      authorization_url: authorizationUrl,
    }));

    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
    }, {
      operation: "connect",
      connector: "gmail",
      account_hint: " Reader@Example.COM ",
    });

    expect(result).toEqual({
      ok: true,
      status: "authorization_required",
      connector: "gmail",
      name: "Gmail",
      account: "reader@example.com",
      authorization_url: authorizationUrl,
      expires_in_seconds: 600,
      message: "Authorize Gmail to finish connecting it.",
    });
    expect(fetch).toHaveBeenCalledTimes(1);
    const [url, init] = fetch.mock.calls[0]!;
    expect(url).toBe("https://broker.internal/users/user%2Fwith%20spaces/connectors/gmail");
    expect(init).toMatchObject({ method: "POST" });
    expect(JSON.parse(String(init?.body))).toEqual({
      redirect_uri: "https://nanocodex.example/v1/connectors/gmail/callback",
      return_to: "/agent?thread=77777777-7777-4777-8777-777777777777",
      account_hint: "reader@example.com",
    });
  });

  it("does not let a delegated app grant mutate account connections", async () => {
    const fetch = vi.fn();
    expect(await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
      canManage: () => false,
    }, { operation: "disconnect", connector: "github" })).toEqual({
      ok: false,
      status: "forbidden",
      message: "This delegated app grant cannot change account-level connectors.",
    });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("rejects account hints outside Google connectors and unsafe broker URLs", async () => {
    const fetch = vi.fn(async () => Response.json({
      authorization_url: "https://attacker.example/oauth",
    }));
    const options = { ...base, broker: { fetch } as unknown as Fetcher };

    await expect(manageAccountConnectors(options, {
      operation: "connect",
      connector: "github",
      account_hint: "reader@example.com",
    })).rejects.toThrow("supported only for Gmail and Google Drive");
    expect(await manageAccountConnectors(options, {
      operation: "connect",
      connector: "github",
    })).toMatchObject({ ok: false, status: "unavailable" });
  });

  it("rejects credentials or unexpected fields from an otherwise allowed provider URL", async () => {
    for (const authorization_url of [
      `${providerAuthorizationUrl("gmail")}&client_secret=broker-leak`,
      `${providerAuthorizationUrl("gmail")}&code_verifier=broker-leak`,
      `${providerAuthorizationUrl("gmail")}&state=duplicate`,
    ]) {
      const fetch = vi.fn(async () => Response.json({ authorization_url }));
      const result = await manageAccountConnectors({
        ...base,
        broker: { fetch } as unknown as Fetcher,
      }, { operation: "connect", connector: "gmail" });
      expect(result).toMatchObject({ ok: false, status: "unavailable" });
      expect(JSON.stringify(result)).not.toContain("broker-leak");
    }
  });

  it.each(["github", "gmail", "gdrive", "x"] as const)(
    "accepts the fixed %s provider authorization boundary",
    async (connector) => {
      const authorizationUrl = providerAuthorizationUrl(connector);
      const result = await manageAccountConnectors({
        ...base,
        broker: ({
          fetch: async () => Response.json({ authorization_url: authorizationUrl }),
        } as unknown as Fetcher),
      }, { operation: "connect", connector });
      expect(result).toMatchObject({
        ok: true,
        status: "authorization_required",
        connector,
        authorization_url: authorizationUrl,
      });
    },
  );

  it("rejects a valid authorization URL for the wrong requested connector", async () => {
    const authorization_url = providerAuthorizationUrl("gmail")
      .replace("/gmail/callback", "/github/callback");
    const result = await manageAccountConnectors({
      ...base,
      broker: ({ fetch: async () => Response.json({ authorization_url }) } as unknown as Fetcher),
    }, { operation: "connect", connector: "github" });
    expect(result).toMatchObject({ ok: false, status: "unavailable" });
    expect(JSON.stringify(result)).not.toContain("authorization_url");
  });
});

function providerAuthorizationUrl(connector: "github" | "gmail" | "gdrive" | "x"): string {
  const url = new URL(connector === "github"
    ? "https://github.com/login/oauth/authorize"
    : connector === "x"
      ? "https://x.com/i/oauth2/authorize"
      : "https://accounts.google.com/o/oauth2/v2/auth");
  const query = {
    client_id: "google-client-id",
    redirect_uri: `https://nanocodex.example/v1/connectors/${connector}/callback`,
    response_type: "code",
    scope: "openid email",
    state: "opaque-state",
    code_challenge: "A".repeat(43),
    code_challenge_method: "S256",
  };
  url.search = new URLSearchParams(query).toString();
  return url.href;
}
