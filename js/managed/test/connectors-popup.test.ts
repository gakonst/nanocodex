import { describe, expect, it } from "vitest";
import {
  localMcpAuthorization,
  verifyLocalMcpOAuthRelayState,
} from "nanocodex-vite/oauth-relay";

import { mcpCallbackCompletionPage, publicMcpStartResponse } from "../src/connectors";

const connectionId = "m".repeat(43);
const state = "s".repeat(43);
const requestUrl = new URL(
  `https://nanocodex.example/v1/connectors/mcp-connections/${connectionId}/callback`,
);

describe("managed MCP OAuth popup completion", () => {
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
