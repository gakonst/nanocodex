import { describe, expect, it } from "vitest";
import {
  localMcpAuthorization,
  verifyLocalMcpOAuthRelayState,
} from "nanocodex-vite/oauth-relay";

import {
  connectorCompletionPage,
  connectorMobileCompletion,
  connectorResultReturnTo,
  mcpMobileCompletion,
  mcpCallbackCompletionPage,
  publicMcpStartResponse,
} from "../src/connectors";

const connectionId = "m".repeat(43);
const state = "s".repeat(43);
const requestUrl = new URL(
  `https://nanocodex.example/v1/connectors/mcp-connections/${connectionId}/callback`,
);

describe("managed MCP OAuth popup completion", () => {
  it("accepts a public MCP connection without manufacturing an OAuth URL", async () => {
    const response = await publicMcpStartResponse(Response.json({
      mcp_connections: [{ id: connectionId, name: "Mercator", status: "connected" }],
    }), connectionId, undefined, "");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      mcp_connection: { id: connectionId, name: "Mercator", status: "connected" },
    });
  });

  it("retains the broker state while the Vite relay wraps local callback routing", async () => {
    const relayKey = "local-relay-test-key-with-32-bytes";
    const local = localMcpAuthorization(
      "https://nanocodex.localhost",
      connectionId,
      "managed",
    );
    const response = await publicMcpStartResponse(Response.json({
      authorization_url: `https://mcp.example/authorize?state=${state}`,
      mcp_connections: [{ id: connectionId, name: "Example", status: "authorization_required" }],
    }), connectionId, local, relayKey);
    const body = await response.json() as Record<string, unknown>;
    const wrappedState = new URL(String(body.authorization_url)).searchParams.get("state");

    expect(body.callback_state).toBe(state);
    expect(wrappedState).not.toBe(state);
    expect(await verifyLocalMcpOAuthRelayState(wrappedState, connectionId, relayKey)).toMatchObject({
      c: connectionId,
      f: "managed",
      o: "https://nanocodex.localhost",
      s: state,
    });
  });

  it("publishes one secret-free correlated success and closes the popup", async () => {
    const response = mcpCallbackCompletionPage(
      requestUrl,
      "/agent?thread=example",
      connectionId,
      state,
      "connected",
    );
    const html = await response.text();

    expect(response.status).toBe(200);
    expect(response.headers.get("cross-origin-opener-policy")).toBe("unsafe-none");
    expect(html).toContain(`nanocodex:oauth-completion:${state}`);
    expect(html).toContain(`nanocodex-oauth-completion-${state}`);
    expect(html).toContain(`\"connector\":\"mcp:${connectionId}\"`);
    expect(html).toContain(`\"state\":\"${state}\"`);
    expect(html).toContain("window.opener?.postMessage");
    expect(html).toContain("window.close();");
    expect(html).not.toMatch(/access_token|refresh_token|client_secret/);
  });

  it.each(["cancelled", "failed"] as const)(
    "keeps the popup open and makes %s visible",
    async (result) => {
      const response = mcpCallbackCompletionPage(
        requestUrl,
        "/agent",
        connectionId,
        state,
        result,
      );
      const html = await response.text();

      expect(response.status).toBe(result === "failed" ? 502 : 200);
      expect(html).toContain(result === "cancelled"
        ? "The MCP authorization was cancelled"
        : "The MCP provider could not complete authorization");
      expect(html).toContain(`\"result\":\"error\"`);
      expect(html).not.toContain("window.close();");
    },
  );
});

describe("native connector OAuth completion", () => {
  it("correlates the HTTPS callback without projecting OAuth material", async () => {
    const returnTo = connectorResultReturnTo(
      "/v1/connectors/mobile-complete?attempt=6f3eec23-8a1a-4de4-b498-1689a2829ca0",
      new URL("https://nanocodex.example/v1/connectors/google/callback?code=private&state=private"),
      "google",
      "connected",
    );
    const response = connectorCompletionPage(requestUrl, "google", "connected", returnTo);
    const html = await response.text();

    expect(returnTo).toBe(
      "/v1/connectors/mobile-complete?attempt=6f3eec23-8a1a-4de4-b498-1689a2829ca0&connector=google&connector_result=connected",
    );
    expect(html).toContain("https://nanocodex.example/v1/connectors/mobile-complete?");
    expect(html).not.toMatch(/code=private|state=private|access_token|refresh_token/);
  });

  it("keeps callback destinations on the managed origin", () => {
    expect(connectorResultReturnTo(
      "https://evil.example/steal",
      new URL("https://nanocodex.example/v1/connectors/google/callback"),
      "google",
      "failed",
    )).toBe("/");
  });

  it("bridges a validated, secret-free result to the native app scheme", () => {
    const attempt = "6f3eec23-8a1a-4de4-b498-1689a2829ca0";
    const response = connectorMobileCompletion(new URL(
      `https://nanocodex.example/v1/connectors/mobile-complete?attempt=${attempt}&connector=google&connector_result=connected`,
    ));

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe(
      `nanocodex://connectors/complete?attempt=${attempt}&connector=google&connector_result=connected`,
    );
    expect(response.headers.get("cache-control")).toBe("no-store");
  });

  it("rejects extra OAuth material and malformed native results", () => {
    expect(connectorMobileCompletion(new URL(
      "https://nanocodex.example/v1/connectors/mobile-complete?attempt=bad&connector=google&connector_result=connected",
    )).status).toBe(400);
    expect(connectorMobileCompletion(new URL(
      "https://nanocodex.example/v1/connectors/mobile-complete?attempt=6f3eec23-8a1a-4de4-b498-1689a2829ca0&connector=google&connector_result=connected&code=private",
    )).status).toBe(400);
  });
});

describe("native MCP OAuth completion", () => {
  it("bridges a validated, secret-free MCP result to the native app scheme", () => {
    const attempt = "6f3eec23-8a1a-4de4-b498-1689a2829ca0";
    const response = mcpMobileCompletion(new URL(
      `https://nanocodex.example/v1/connectors/mcp-mobile-complete?attempt=${attempt}&mcp_connection=${connectionId}&mcp_result=connected`,
    ));

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe(
      `nanocodex://connectors/mcp-complete?attempt=${attempt}&mcp_connection=${connectionId}&mcp_result=connected`,
    );
    expect(response.headers.get("cache-control")).toBe("no-store");
  });

  it("rejects extra OAuth material and malformed MCP results", () => {
    expect(mcpMobileCompletion(new URL(
      `https://nanocodex.example/v1/connectors/mcp-mobile-complete?attempt=bad&mcp_connection=${connectionId}&mcp_result=connected`,
    )).status).toBe(400);
    expect(mcpMobileCompletion(new URL(
      `https://nanocodex.example/v1/connectors/mcp-mobile-complete?attempt=6f3eec23-8a1a-4de4-b498-1689a2829ca0&mcp_connection=${connectionId}&mcp_result=connected&code=private`,
    )).status).toBe(400);
  });
});
