import { describe, expect, it, vi } from "vitest";
// @ts-expect-error ToolRouter is a shared JavaScript runtime module.
import { ToolRouter, providerSource } from "nanocodex-tools/runtime/tool-router";
import { connectorToolsProvider, CONNECTOR_TOOL_CATALOG } from "../src/connector-tools";
import { CONNECTOR_CAPABILITY_IDS, CONNECTOR_PROVIDER_IDS, CONNECTOR_PROVIDER_CATALOG } from "../src/connector-status";
import { accountInfo, projectAccountInfo } from "../src/account-info";
import { exactConnectorAccess, handleManagedEgress } from "../src/managed-egress";

const ID = "a".repeat(43);
const OTHER = "b".repeat(43);
const context = { sessionId: "session", callId: "call", parentCallId: "", model: "test", signal: new AbortController().signal };

function setup() {
  let allowed = true;
  const fetch = vi.fn(async (_request: Request) => Response.json({ items: [{ name: "Playlist" }] }));
  const provider = connectorToolsProvider({
    available: () => allowed,
    fetch: (request, _context, capability) => handleManagedEgress(request, { fetch } as unknown as Fetcher,
      "s".repeat(43), (actual, selected) => actual === capability && allowed ? exactConnectorAccess([ID], selected) : false),
  });
  const router = new ToolRouter([providerSource("account-connectors", provider)]);
  return { provider, router, fetch, revoke: () => { allowed = false; } };
}

describe("connected service discovery and requests", () => {
  it("sends Link approval requests once through the selected account and refuses delegated endpoints", async () => {
    const { router, fetch } = setup();
    await router.execute("link_request", { method: "POST", path: "/spend_requests/lsrq_123/request_approval", connection_id: ID }, context);
    const request = fetch.mock.calls[0]![0];
    expect(request.url).toBe("https://api.link.com/spend_requests/lsrq_123/request_approval");
    expect(request.method).toBe("POST");
    expect(request.headers.get("x-nanocodex-connector-connection")).toBe(ID);
    expect(await router.execute("link_request", { method: "POST", path: "/spend_requests/create_delegated" }, context)).toMatchObject({ status: 403 });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("advertises every supported provider in the native connector catalog", () => {
    expect(CONNECTOR_PROVIDER_CATALOG.map(provider => provider.id)).toEqual(CONNECTOR_PROVIDER_IDS);
  });
  it("makes every connector discoverable with tool_search and calls Spotify through authenticated egress", async () => {
    const { router, fetch } = setup();
    for (const capability of CONNECTOR_CAPABILITY_IDS) {
      const result = await router.execute("tool_search", { query: `${capability} request`, limit: 20 }, context);
      expect(JSON.stringify(result)).toContain(`${capability}_request`);
    }
    const result = await router.execute("spotify_request", { path: "/v1/me/playlists?limit=5", connection_id: ID }, context);
    expect(result).toMatchObject({ ok: true, status: 200, data: { items: [{ name: "Playlist" }] } });
    const request = fetch.mock.calls[0]![0];
    expect(request.url).toBe("https://api.spotify.com/v1/me/playlists?limit=5");
    expect(request.headers.get("x-nanocodex-connector-connection")).toBe(ID);
    expect(request.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    expect(request.redirect).toBe("manual");
  });

  it("keeps writes intact and denies unapproved account selectors before broker dispatch", async () => {
    const { router, fetch } = setup();
    const body = { name: "New playlist", public: false };
    await router.execute("spotify_request", { method: "POST", path: "/v1/me/playlists", body }, context);
    const request = fetch.mock.calls[0]![0];
    expect(request.method).toBe("POST");
    expect(await request.json()).toEqual(body);
    expect(request.headers.get("x-nanocodex-connector-connection")).toBe(ID);
    expect(await router.execute("spotify_request", { path: "/v1/me", connection_id: OTHER }, context)).toMatchObject({ status: 403 });
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("blocks destination escapes, credentials, OAuth paths and cross-capability Google requests", async () => {
    const { router, fetch } = setup();
    for (const input of [
      { path: "https://evil.example/v1/me" }, { path: "//evil.example/v1/me" },
      { path: "/\\evil.example/v1/me" }, { path: "/v1/me#fragment" },
      { path: "/v1/me", headers: { authorization: "bad" } },
      { path: "/v1/me", connection_id: "bad" }, { method: "GET", path: "/v1/me", body: {} },
    ]) await expect(router.execute("spotify_request", input, context)).rejects.toThrow();
    expect(await router.execute("spotify_request", { path: "/api/token" }, context)).toMatchObject({ status: 403 });
    expect(await router.execute("gdrive_request", { path: "/calendar/v3/users/me/calendarList" }, context)).toMatchObject({ status: 403 });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("routes every documented example through its own connector boundary", async () => {
    const { router, fetch } = setup();
    for (const [id, spec] of Object.entries(CONNECTOR_TOOL_CATALOG)) {
      const result = await router.execute(`${id}_request`, { path: spec.example }, context);
      expect(result).toMatchObject({ status: 200 });
      expect(fetch.mock.calls.at(-1)![0].url).toBe(spec.origin + spec.example);
    }
    await router.execute("spotify_request", { path: "/v1/me/player/recently-played?limit=1" }, context);
    expect(fetch.mock.calls.at(-1)![0].url).toBe("https://api.spotify.com/v1/me/player/recently-played?limit=1");
    for (const [id, path] of [["gdocs", "/v1/documents"], ["gsheets", "/v4/spreadsheets"], ["gslides", "/v1/presentations"]]) {
      expect(await router.execute(`${id}_request`, { method: "POST", path, body: {} }, context)).toMatchObject({ status: 200 });
    }
  });

  it("rejects unsupported provider verbs and Contacts writes before dispatch", async () => {
    const { router, fetch } = setup();
    for (const [id, method] of [["spotify", "PATCH"], ["soundcloud", "PATCH"], ["slack", "DELETE"], ["gdocs", "PUT"], ["gslides", "PATCH"], ["gsheets", "DELETE"], ["gdrive", "PUT"], ["gcontacts", "POST"]]) {
      await expect(router.execute(`${id}_request`, { method, path: "/", body: {} }, context)).rejects.toThrow("Invalid method");
    }
    expect(fetch).not.toHaveBeenCalled();
  });

  it("reports Slack HTTP 200 API failures and empty Spotify playback responses correctly", async () => {
    const fetch = vi.fn(async () => Response.json({ ok: false, error: "missing_scope" }));
    const provider = connectorToolsProvider({ available: () => true, fetch });
    expect(await provider.resolve("slack_request")!.handler({ path: "/api/conversations.list" }, context))
      .toMatchObject({ ok: false, status: 200, data: { error: "missing_scope" } });
    fetch.mockImplementationOnce(async () => new Response(null, { status: 204 }));
    expect(await provider.resolve("spotify_request")!.handler({ path: "/v1/me/player" }, context))
      .toEqual({ ok: true, status: 204, data: null });
  });

  it("removes revoked tools from discovery and rejects retained handlers", async () => {
    const { provider, revoke, fetch } = setup();
    const handler = provider.resolve("spotify_request")!;
    revoke();
    expect(provider.definitions()).toEqual([]);
    await expect(handler.handler({ path: "/v1/me" }, context)).rejects.toThrow("unavailable");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("reports rate limits without replaying writes and bounds response reads", async () => {
    const fetch = vi.fn(async () => new Response("limited", { status: 429, headers: { "retry-after": "3" } }));
    const provider = connectorToolsProvider({ available: () => true, fetch });
    const handler = provider.resolve("spotify_request")!;
    expect(await handler.handler({ method: "POST", path: "/v1/me/playlists", body: { name: "x" } }, context)).toMatchObject({ status: 429, retry_after: "3" });
    expect(fetch).toHaveBeenCalledTimes(1);
    fetch.mockImplementationOnce(async () => new Response("x".repeat(512 * 1024 + 1)));
    expect(await handler.handler({ path: "/v1/me/playlists" }, context)).toMatchObject({ response_too_large: true });
  });

  it("Account Info advertises only connected and authorized service tools", async () => {
    const fetch = async (input: RequestInfo | URL) => Response.json(String(input).endsWith("/connectors") ? { connectors: {
      spotify: { connected: true, connections: [{ id: ID, label: "listener", account_id: "listener", capabilities: ["spotify"] }] },
      soundcloud: { connected: false },
    } } : {});
    const info = await accountInfo({ fetch }, "user", { enabled: true });
    expect(info.connectorTools.spotify).toMatchObject({ tool: "spotify_request" });
    expect(info.connectorTools.soundcloud).toBeUndefined();
    expect(projectAccountInfo(info, [], {}).connectorTools).toEqual({});
    expect(projectAccountInfo(info, ["spotify"], { spotify: [OTHER] }).connectorTools).toEqual({});
    expect((await accountInfo({ fetch }, "user", { enabled: false })).connectorTools).toEqual({});
  });
});
