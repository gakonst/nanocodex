import { describe, expect, it, vi } from "vitest";

import { manageAccountConnectors } from "../src/account-connectors-tool";

const A = "a".repeat(43);
const B = "b".repeat(43);
const base = {
  userId: "user/with spaces",
  sessionId: "77777777-7777-4777-8777-777777777777",
  publicOrigin: "https://nanocodex.example",
  canManage: () => true,
  allowedConnectors: () => undefined,
};

describe("managed account connector tool", () => {
  it("lists provider-neutral connection metadata without broker credentials", async () => {
    const fetch = vi.fn(async () => Response.json(canonicalStatuses()));
    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
    }, { operation: "list" });

    expect(result).toMatchObject({
      connectors: {
        github: {
          connected: true,
          account: "octocat",
          connections: [{ id: A, label: "octocat", accountId: "github-1", capabilities: ["github"] }],
        },
        gmail: {
          connected: true,
          connections: [
            { id: A, label: "work@example.com", accountId: "google-1", capabilities: ["gmail", "gdrive"] },
            { id: B, label: "home@example.com", accountId: "google-2", capabilities: ["gmail"] },
          ],
        },
        gdrive: {
          connected: true,
          account: "work@example.com",
          connections: [
            { id: A, label: "work@example.com", accountId: "google-1", capabilities: ["gmail", "gdrive"] },
          ],
        },
        slack: { connected: true, account: "Acme (U123)", connections: [{ id: B, label: "Acme (U123)", accountId: "T123:U123", capabilities: ["slack"] }] },
      },
      supported: [
        { id: "github", name: "GitHub", capabilities: ["github"] },
        { id: "google", name: "Google Workspace", capabilities: ["gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts"] },
        { id: "slack", name: "Slack", capabilities: ["slack"] },
        { id: "x", name: "X", capabilities: ["x"] },
      ],
    });
    expect(JSON.stringify(result)).not.toMatch(/access_token|secret/);
    expect(fetch).toHaveBeenCalledWith(
      "https://broker.internal/users/user%2Fwith%20spaces/connectors",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
  });

  it("filters both capabilities and exact connection IDs for a delegated grant", async () => {
    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch: async () => Response.json(canonicalStatuses()) } as unknown as Fetcher,
      allowedConnectors: () => ["gmail", "slack"],
      allowedConnectorConnections: () => ({ gmail: [B], slack: [] }),
    }, { operation: "list" });

    expect(result).toMatchObject({
      connectors: {
        github: { connected: false, connections: [] },
        gmail: { connected: true, account: "home@example.com", connections: [{ id: B }] },
        gdrive: { connected: false, connections: [] },
        slack: { connected: false, connections: [] },
      },
    });
    expect(JSON.stringify(result)).not.toMatch(/work@example.com|Acme/);
  });

  it("keeps legacy singleton readers without granting them a selector", async () => {
    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch: async () => Response.json({ connectors: {
        github: { connected: true, label: "legacy-octocat", account_id: "old-id" },
      } }) } as unknown as Fetcher,
      allowedConnectors: () => ["github"],
    }, { operation: "list" });

    expect(result).toMatchObject({ connectors: {
      github: { connected: true, account: "legacy-octocat" },
    } });
    expect((result as any).connectors.github).not.toHaveProperty("connections");
  });

  it("normalizes legacy Google controls onto one provider authorization", async () => {
    const authorizationUrl = providerAuthorizationUrl("google");
    const fetch = vi.fn<(
      input: RequestInfo | URL,
      init?: RequestInit,
    ) => Promise<Response>>(async () => Response.json({ authorization_url: authorizationUrl }));
    const result = await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
    }, {
      operation: "connect",
      connector: "gmail",
      account_hint: " Reader@Example.COM ",
    });

    expect(result).toMatchObject({
      ok: true,
      status: "authorization_required",
      connector: "google",
      account: "reader@example.com",
      authorization_url: authorizationUrl,
    });
    const [url, init] = fetch.mock.calls[0]!;
    expect(url).toBe("https://broker.internal/users/user%2Fwith%20spaces/connectors/google");
    expect(JSON.parse(String(init?.body))).toEqual({
      redirect_uri: "https://nanocodex.example/v1/connectors/google/callback",
      return_to: "/agent/77777777-7777-4777-8777-777777777777",
      account_hint: "reader@example.com",
    });
  });

  it("accepts Slack authorization without claiming PKCE fields", async () => {
    const authorizationUrl = providerAuthorizationUrl("slack");
    expect(await manageAccountConnectors({
      ...base,
      broker: { fetch: async () => Response.json({ authorization_url: authorizationUrl }) } as unknown as Fetcher,
    }, { operation: "connect", connector: "slack" })).toMatchObject({
      ok: true,
      connector: "slack",
      authorization_url: authorizationUrl,
    });
  });

  it("revokes one exact connection and resolves a legacy omitted ID only when unambiguous", async () => {
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => (
      init?.method === "DELETE" ? new Response(null, { status: 204 }) : Response.json(canonicalStatuses())
    ));
    const options = { ...base, broker: { fetch } as unknown as Fetcher };
    expect(await manageAccountConnectors(options, {
      operation: "disconnect",
      connector: "slack",
      connection_id: B,
    })).toEqual({
      ok: true,
      status: "disconnected",
      connector: "slack",
      connection_id: B,
    });
    expect(fetch.mock.calls[0]![0]).toBe(
      `https://broker.internal/users/user%2Fwith%20spaces/connectors/slack/connections/${B}`,
    );

    fetch.mockClear();
    expect(await manageAccountConnectors(options, {
      operation: "disconnect",
      connector: "google",
    })).toMatchObject({ ok: false, status: "conflict" });
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("does not let a delegated app grant mutate account connections", async () => {
    const fetch = vi.fn();
    expect(await manageAccountConnectors({
      ...base,
      broker: { fetch } as unknown as Fetcher,
      canManage: () => false,
    }, { operation: "disconnect", connector: "github", connection_id: A })).toEqual({
      ok: false,
      status: "forbidden",
      message: "This delegated app grant cannot change account-level connectors.",
    });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("rejects credentials and unexpected fields from provider URLs", async () => {
    for (const authorization_url of [
      `${providerAuthorizationUrl("google")}&client_secret=broker-leak`,
      `${providerAuthorizationUrl("google")}&state=duplicate`,
      providerAuthorizationUrl("google").replace("enable_granular_consent=true", "enable_granular_consent=false"),
      wrongCallbackUrl(),
    ]) {
      const result = await manageAccountConnectors({
        ...base,
        broker: { fetch: async () => Response.json({ authorization_url }) } as unknown as Fetcher,
      }, { operation: "connect", connector: "google" });
      expect(result).toMatchObject({ ok: false, status: "unavailable" });
      expect(JSON.stringify(result)).not.toContain("broker-leak");
    }
  });
});

function canonicalStatuses() {
  const googleWork = { id: A, label: " work@example.com ", account_id: "google-1", capabilities: ["gmail", "gdrive"], access_token: "secret" };
  return { connectors: {
    github: { connected: true, connections: [{ id: A, label: "octocat", account_id: "github-1", capabilities: ["github"], access_token: "secret" }] },
    gmail: { connected: true, connections: [googleWork, { id: B, label: "home@example.com", account_id: "google-2", capabilities: ["gmail"] }] },
    gdrive: { connected: true, connections: [googleWork] },
    slack: { connected: true, connections: [{ id: B, label: "Acme (U123)", account_id: "T123:U123", capabilities: ["slack"], token: "secret" }] },
    x: { connected: false, connections: [] },
  } };
}

function wrongCallbackUrl(): string {
  const url = new URL(providerAuthorizationUrl("google"));
  url.searchParams.set(
    "redirect_uri",
    "https://nanocodex.example/v1/connectors/github/callback",
  );
  return url.href;
}

function providerAuthorizationUrl(provider: "github" | "google" | "slack" | "x"): string {
  const url = new URL(provider === "github"
    ? "https://github.com/login/oauth/authorize"
    : provider === "x"
      ? "https://x.com/i/oauth2/authorize"
      : provider === "slack"
        ? "https://slack.com/oauth/v2/authorize"
        : "https://accounts.google.com/o/oauth2/v2/auth");
  const query: Record<string, string> = {
    client_id: "client-id",
    redirect_uri: `https://nanocodex.example/v1/connectors/${provider}/callback`,
    state: "opaque-state",
    ...(provider === "slack" ? { user_scope: "channels:read,chat:write" } : {
      response_type: "code",
      scope: "openid email",
      code_challenge: "A".repeat(43),
      code_challenge_method: "S256",
      ...(provider === "google" ? { enable_granular_consent: "true" } : {}),
    }),
  };
  url.search = new URLSearchParams(query).toString();
  return url.href;
}
