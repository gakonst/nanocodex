import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { UserConnectorBroker } from "../src/connector-broker";
import type { EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const LINEAR_ENDPOINT = "https://mcp.linear.app/mcp";

describe("generic remote MCP connection owner", () => {
  it("materializes one immutable endpoint and denies cross-owner use", async () => {
    const id = connectionId("M");
    const created = await control(`/users/mcp-owner/mcp-connections/${id}`, "PUT", {
      endpoint: LINEAR_ENDPOINT,
      name: "Linear",
      scopes: ["read"],
    });
    expect(created.status).toBe(200);
    expect(await created.json()).toEqual({
      mcp_connections: [{ id, name: "Linear", status: "authorization_required" }],
    });

    const repeated = await control(`/users/mcp-owner/mcp-connections/${id}`, "PUT", {
      endpoint: LINEAR_ENDPOINT,
      name: "Ignored replacement name",
      scopes: ["write"],
    });
    expect(await repeated.json()).toEqual({
      mcp_connections: [{ id, name: "Linear", status: "authorization_required" }],
    });
    const substituted = await control(`/users/mcp-owner/mcp-connections/${id}`, "PUT", {
      endpoint: "https://mcp-fixture.nanocodex.dev/mcp",
      name: "Substitution",
    });
    expect(substituted.status).toBe(409);
    expect(await substituted.json()).toEqual({ error: "mcp_connection_substitution_denied" });
    expect(JSON.stringify(await (await SELF.fetch(
      "https://broker.test/users/mcp-owner/mcp-connections",
    )).json())).not.toContain(LINEAR_ENDPOINT);

    const otherStatus = await SELF.fetch(
      `https://broker.test/users/mcp-other/mcp-connections/${id}`,
    );
    expect(otherStatus.status).toBe(403);
    expect(await otherStatus.json()).toEqual({ error: "mcp_connection_owner_mismatch" });
    const otherMaterialization = await control(
      `/users/mcp-other/mcp-connections/${id}`,
      "PUT",
      { endpoint: LINEAR_ENDPOINT, name: "Cross-owner copy" },
    );
    expect(otherMaterialization.status).toBe(409);
    expect(await otherMaterialization.json()).toEqual({
      error: "mcp_connection_owner_mismatch",
    });
    const otherSubject = subject("cross-owner");
    await bindSubject(otherSubject, "mcp-other");
    const denied = await SELF.fetch(mcpRequest(id, otherSubject));
    expect(denied.status).toBe(403);
    expect(await denied.json()).toEqual({ error: "mcp_connection_owner_mismatch" });

    const unsafe = await control(
      `/users/mcp-owner/mcp-connections/${connectionId("U")}`,
      "PUT",
      { endpoint: "https://127.0.0.1/mcp", name: "Unsafe" },
    );
    expect(unsafe.status).toBe(400);
    const reserved = await control(
      `/users/mcp-owner/mcp-connections/${connectionId("I")}`,
      "PUT",
      { endpoint: "https://mcp.invalid/mcp", name: "Reserved" },
    );
    expect(reserved.status).toBe(400);
  });

  it("discovers protected-resource and authorization metadata, registers, and uses PKCE S256", async () => {
    const user = "mcp-oauth";
    const id = connectionId("O");
    await materialize(user, id, LINEAR_ENDPOINT, "Linear");
    const started = await start(user, id);
    expect(started.response.status).toBe(200);
    expect(started.body.mcp_connections).toEqual([
      { id, name: "Linear", status: "authorization_required" },
    ]);
    const authorization = new URL(started.body.authorization_url);
    expect(authorization.origin).toBe("https://mcp-auth.nanocodex.dev");
    expect(authorization.searchParams.get("client_id")).toBe("mcp-dynamic-client");
    expect(authorization.searchParams.get("resource")).toBe(LINEAR_ENDPOINT);
    expect(authorization.searchParams.get("scope")).toBe("read");
    expect(authorization.searchParams.get("code_challenge_method")).toBe("S256");
    expect(authorization.searchParams.get("code_challenge")).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(authorization.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(JSON.stringify(started.body)).not.toMatch(
      /mcp-dynamic-secret|mcp-access-token|mcp-refresh-token/,
    );

    const wrongState = await control(
      `/users/${user}/mcp-connections/${id}/callback`,
      "POST",
      { code: "connected", state: connectionId("W") },
    );
    expect(wrongState.status).toBe(400);
    expect(await wrongState.json()).toEqual({ error: "invalid_oauth_state" });

    const callback = await control(
      `/users/${user}/mcp-connections/${id}/callback`,
      "POST",
      { code: "connected", state: authorization.searchParams.get("state") },
    );
    expect(callback.status).toBe(200);
    expect(await callback.json()).toEqual({
      mcp_connections: [{ id, name: "Linear", status: "connected" }],
      return_to: "/connections",
    });
    const replay = await control(
      `/users/${user}/mcp-connections/${id}/callback`,
      "POST",
      { code: "connected", state: authorization.searchParams.get("state") },
    );
    expect(replay.status).toBe(400);
    expect(await replay.json()).toEqual({ error: "invalid_oauth_state" });

    const standardId = connectionId("S");
    const standardEndpoint = "https://mcp-standard.nanocodex.dev/mcp";
    await materialize("mcp-standard-discovery", standardId, standardEndpoint, "Standard MCP", ["read"]);
    const standard = await start("mcp-standard-discovery", standardId);
    expect(standard.response.status).toBe(200);
    const standardAuthorization = new URL(standard.body.authorization_url);
    expect(standardAuthorization.searchParams.get("resource")).toBe(standardEndpoint);
    expect(standardAuthorization.searchParams.get("code_challenge_method")).toBe("S256");
  });

  it("encrypts discovery, DCR, PKCE, and token state without exposing secrets", async () => {
    const user = "mcp-encrypted";
    const id = connectionId("E");
    await materialize(user, id, LINEAR_ENDPOINT, "Encrypted Linear");
    const started = await start(user, id);
    const state = new URL(started.body.authorization_url).searchParams.get("state")!;
    expect((await control(`/users/${user}/mcp-connections/${id}/callback`, "POST", {
      code: "connected",
      state,
    })).status).toBe(200);

    const stub = workerEnv.USER_CONNECTORS.getByName(user);
    await runInDurableObject(stub, async (_instance: UserConnectorBroker, durableState) => {
      const row = await durableState.storage.get("mcp-connection-state");
      const encoded = JSON.stringify(row);
      expect(encoded).toContain("ciphertext");
      for (const forbidden of [
        LINEAR_ENDPOINT,
        "mcp-dynamic-secret",
        "mcp-access-token",
        "mcp-refresh-token",
        state,
      ]) expect(encoded).not.toContain(forbidden);
    });
  });

  it("streams MCP requests and responses while preserving only MCP headers", async () => {
    const user = "mcp-proxy";
    const id = connectionId("P");
    const ownerSubject = subject("proxy");
    await materialize(user, id, "https://mcp-fixture.nanocodex.dev/mcp", "Example MCP", ["read"]);
    await authorize(user, id, "connected");
    await bindSubject(ownerSubject, user);

    const encoder = new TextEncoder();
    const response = await SELF.fetch(new Request(`https://mcp.internal/v1/connections/${id}`, {
      method: "POST",
      headers: {
        accept: "application/json, text/event-stream",
        "content-type": "application/json",
        "last-event-id": "event-7",
        "mcp-protocol-version": "2025-06-18",
        "mcp-session-id": "caller-session",
        "x-nanocodex-subject": ownerSubject,
        "x-should-not-forward": "caller-private",
      },
      body: new ReadableStream({
        start(controller) {
          controller.enqueue(encoder.encode('{"jsonrpc":"2.0",'));
          controller.enqueue(encoder.encode('"method":"tools/list"}'));
          controller.close();
        },
      }),
    }));
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toContain("application/json");
    expect(response.headers.get("mcp-session-id")).toBe("upstream-session");
    expect(response.headers.get("retry-after")).toBe("3");
    expect(response.headers.get("x-should-not-forward")).toBeNull();
    const body = await response.json<Record<string, unknown>>();
    expect(body).toEqual({
      authorized: "connected",
      method: "POST",
      accept: "application/json, text/event-stream",
      content_type: "application/json",
      protocol_version: "2025-06-18",
      session_id: "caller-session",
      last_event_id: "event-7",
      caller_header: null,
      body: '{"jsonrpc":"2.0","method":"tools/list"}',
    });
    expect(JSON.stringify(body)).not.toMatch(/mcp-access-token|mcp-refresh-token/);

    const callerCredential = await SELF.fetch(mcpRequest(id, ownerSubject, {
      authorization: "Bearer browser-or-provider-token",
    }));
    expect(callerCredential.status).toBe(403);
    expect(await callerCredential.json()).toEqual({ error: "caller_credential_forbidden" });

    const reflectedHeader = await SELF.fetch(mcpRequest(id, ownerSubject, {
      "last-event-id": "reflect-header",
    }));
    expect(reflectedHeader.status).toBe(502);
    expect(await reflectedHeader.json()).toEqual({ error: "credential_projection_blocked" });

    await expect(SELF.fetch(mcpRequest(id, ownerSubject, {
      "last-event-id": "reflect-body",
    })).then((reflectedBody) => reflectedBody.text())).rejects.toThrow(
      "credential_projection_blocked",
    );
    const reflectedStatus = await SELF.fetch(mcpRequest(id, ownerSubject, {
      "last-event-id": "reflect-status",
    }));
    expect(reflectedStatus.status).toBe(299);
    expect(reflectedStatus.statusText).not.toContain("mcp-access-token");
  });

  it("refreshes once after an upstream 401 and replays the bounded request body", async () => {
    const user = "mcp-refresh";
    const id = connectionId("R");
    const ownerSubject = subject("refresh");
    await materialize(user, id, "https://mcp-fixture.nanocodex.dev/mcp", "Refresh MCP", ["read"]);
    await authorize(user, id, "refresh-once");
    await bindSubject(ownerSubject, user);

    const first = await SELF.fetch(mcpRequest(id, ownerSubject, {
      method: "POST",
      contentType: "application/json",
      body: '{"jsonrpc":"2.0","id":1}',
    }));
    expect(first.status).toBe(200);
    expect(await first.json()).toMatchObject({
      authorized: "refreshed",
      method: "POST",
      body: '{"jsonrpc":"2.0","id":1}',
    });
    const second = await SELF.fetch(mcpRequest(id, ownerSubject));
    expect(second.status).toBe(200);
    expect(await second.json()).toMatchObject({ authorized: "refreshed" });
    const projectionUser = "mcp-refresh-projection";
    const projectionId = connectionId("T");
    const projectionSubject = subject("refresh-projection");
    await materialize(
      projectionUser,
      projectionId,
      "https://mcp-fixture.nanocodex.dev/mcp",
      "Refresh projection MCP",
      ["read"],
    );
    await authorize(projectionUser, projectionId, "refresh-once");
    await bindSubject(projectionSubject, projectionUser);
    await expect(SELF.fetch(mcpRequest(projectionId, projectionSubject, {
      method: "POST",
      contentType: "application/json",
      body: '{"jsonrpc":"2.0","id":2}',
      "last-event-id": "reflect-old-body",
    })).then((reflectedBody) => reflectedBody.text())).rejects.toThrow(
      "credential_projection_blocked",
    );
  });

  it("revokes and retains an immutable tombstone", async () => {
    const user = "mcp-revoke";
    const id = connectionId("V");
    const ownerSubject = subject("revoke");
    await materialize(user, id, "https://mcp-fixture.nanocodex.dev/mcp", "Revoked MCP", ["read"]);
    await authorize(user, id, "connected");
    await bindSubject(ownerSubject, user);

    const revoked = await SELF.fetch(
      `https://broker.test/users/${user}/mcp-connections/${id}`,
      { method: "DELETE" },
    );
    expect(revoked.status).toBe(200);
    expect(await revoked.json()).toEqual({
      mcp_connections: [{ id, name: "Revoked MCP", status: "revoked" }],
    });
    const denied = await SELF.fetch(mcpRequest(id, ownerSubject));
    expect(denied.status).toBe(409);
    expect(await denied.json()).toEqual({ error: "connection_revoked" });

    const repeated = await control(`/users/${user}/mcp-connections/${id}`, "PUT", {
      endpoint: "https://mcp-fixture.nanocodex.dev/mcp",
      name: "Attempted resurrection",
    });
    expect(await repeated.json()).toEqual({
      mcp_connections: [{ id, name: "Revoked MCP", status: "revoked" }],
    });
    const substituted = await control(`/users/${user}/mcp-connections/${id}`, "PUT", {
      endpoint: LINEAR_ENDPOINT,
      name: "Attempted substitution",
    });
    expect(substituted.status).toBe(409);
  });
});

async function materialize(
  user: string,
  id: string,
  endpoint: string,
  name: string,
  scopes?: string[],
): Promise<void> {
  const response = await control(`/users/${user}/mcp-connections/${id}`, "PUT", {
    endpoint,
    name,
    ...(scopes ? { scopes } : {}),
  });
  expect(response.status).toBe(200);
}

async function start(user: string, id: string): Promise<{
  response: Response;
  body: { mcp_connections: unknown[]; authorization_url: string };
}> {
  const response = await control(`/users/${user}/mcp-connections/${id}/start`, "POST", {
    redirect_uri: "https://connect-fixture.nanocodex.dev/v1/mcp/callback",
    return_to: "/connections",
  });
  return {
    response,
    body: await response.json<{ mcp_connections: unknown[]; authorization_url: string }>(),
  };
}

async function authorize(user: string, id: string, code: string): Promise<void> {
  const started = await start(user, id);
  expect(started.response.status).toBe(200);
  const state = new URL(started.body.authorization_url).searchParams.get("state");
  const completed = await control(`/users/${user}/mcp-connections/${id}/callback`, "POST", {
    code,
    state,
  });
  expect(completed.status).toBe(200);
}

async function bindSubject(value: string, user: string): Promise<void> {
  const response = await control(`/subjects/${value}`, "PUT", { user_id: user });
  expect(response.status).toBe(200);
}

function mcpRequest(
  id: string,
  ownerSubject: string,
  options: Readonly<{
    authorization?: string;
    body?: string;
    contentType?: string;
    method?: string;
    "last-event-id"?: string;
  }> = {},
): Request {
  const headers = new Headers({ "x-nanocodex-subject": ownerSubject });
  if (options.authorization) headers.set("authorization", options.authorization);
  if (options.contentType) headers.set("content-type", options.contentType);
  if (options["last-event-id"]) headers.set("last-event-id", options["last-event-id"]);
  return new Request(`https://mcp.internal/v1/connections/${id}`, {
    method: options.method ?? "GET",
    headers,
    ...(options.body === undefined ? {} : { body: options.body }),
  });
}

function connectionId(character: string): string {
  return character.repeat(43);
}

function subject(seed: string): string {
  const encoded = btoa(seed).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  return `${encoded}${"S".repeat(43)}`.slice(0, 43);
}

function control(path: string, method: string, body?: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.test${path}`, {
    method,
    ...(body === undefined ? {} : {
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  });
}
