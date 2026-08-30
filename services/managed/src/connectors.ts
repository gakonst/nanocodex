import {
  authenticatePersistentAccount,
  requireSameOriginMutation,
  type AccountAuthEnv,
} from "./account-auth";
import {
  localConnectorAuthorization,
  wrapLocalConnectorAuthorizationState,
} from "../../../web/localConnectorCallback";
import { canonicalRemoteMcpTarget } from "../../mcp-policy/mcpTarget.mjs";

type ConnectorEnv = AccountAuthEnv & {
  NANOCODEX: Fetcher;
  NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY?: string;
};
type ConnectorId = "github" | "gmail" | "gdrive" | "x" | "whoop";
type McpConnectionStatus =
  | "authorization_required"
  | "connected"
  | "reauthorization_required"
  | "disabled"
  | "revoked";
type McpConnection = Readonly<{
  id: string;
  name: string;
  status: McpConnectionStatus;
}>;

const CONNECTOR = /^(github|gmail|gdrive|x|whoop)$/;
const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const MCP_CONNECTION_NAME = /^[^\u0000-\u001f\u007f]{1,256}$/u;
const MCP_CONNECTION_STATUSES = new Set<McpConnectionStatus>([
  "authorization_required",
  "connected",
  "reauthorization_required",
  "disabled",
  "revoked",
]);
const MAX_MCP_CONNECTIONS = 64;
const MAX_MCP_CREATE_BODY_BYTES = 4_096;
const CALLBACK_SUFFIX = "/callback";
const CONNECTOR_ERROR_CODES = new Set([
  "authorization_code_missing",
  "connector_broker_failed",
  "connector_identity_failed",
  "connector_identity_response_invalid",
  "connector_not_configured",
  "connector_provider_unavailable",
  "connector_token_exchange_failed",
  "connector_token_response_invalid",
  "invalid_oauth_state",
  "invalid_request",
]);

export async function routeConnectorRequest(
  request: Request,
  env: ConnectorEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.pathname === "/v1/connectors/mcp-connections") {
    if ((request.method !== "GET" && request.method !== "POST") || url.search) {
      return json({ error: "method_not_allowed" }, 405);
    }
    const principal = await authenticatePersistentAccount(request, env, url);
    if (!principal) return json({ error: "unauthorized" }, 401);
    if (request.method === "POST") {
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      const target = await decodeMcpTarget(request);
      if (!target) return json({ error: "invalid_request" }, 400);
      let materialization: Readonly<{ endpoint: string; name: string }>;
      try { materialization = canonicalRemoteMcpTarget(target); } catch {
        return json({ error: "invalid_mcp_target" }, 400);
      }
      const id = newMcpConnectionId();
      const response = await env.NANOCODEX.fetch(
        `https://broker.internal/users/${encodeURIComponent(principal.userId)}/mcp-connections/${id}`,
        {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(materialization),
        },
      );
      const connection = await publicMcpConnectionResponse(response, id);
      return connection
        ? json({ mcp_connection: connection }, 201)
        : json({ error: "mcp_broker_failed" }, 502);
    }
    return publicMcpConnectionList(await env.NANOCODEX.fetch(
      `https://broker.internal/users/${encodeURIComponent(principal.userId)}/mcp-connections`,
    ));
  }

  const mcpMatch = url.pathname.match(
    /^\/v1\/connectors\/mcp-connections\/([^/]+)(?:\/(start|callback))?$/,
  );
  if (mcpMatch) {
    const connectionId = mcpConnectionId(mcpMatch[1]);
    if (!connectionId) return json({ error: "not_found" }, 404);
    const operation = mcpMatch[2];
    if ((!operation && request.method !== "DELETE")
      || (operation === "start" && request.method !== "POST")
      || (operation === "callback" && request.method !== "GET")) {
      return json({ error: "method_not_allowed" }, 405);
    }
    const principal = await authenticatePersistentAccount(request, env, url);
    if (!principal) return json({ error: "unauthorized" }, 401);
    if (operation !== "callback") {
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
    }
    if (operation !== "callback" && url.search) return json({ error: "invalid_request" }, 400);
    const target = `https://broker.internal/users/${encodeURIComponent(principal.userId)}/mcp-connections/${connectionId}${operation ? `/${operation}` : ""}`;
    if (operation === "start") {
      const start = await mcpStartRequest(request, url, connectionId);
      if (!start) return json({ error: "invalid_return_to" }, 400);
      const response = await env.NANOCODEX.fetch(target, start);
      return publicMcpStartResponse(response, connectionId);
    }
    const response = await env.NANOCODEX.fetch(
      target,
      operation === "callback" ? mcpCallbackRequest(url) : { method: "DELETE" },
    );
    if (operation === "callback") return finishMcpCallback(response, url, connectionId);
    await response.body?.cancel();
    if (!response.ok) return json({ error: "mcp_broker_failed" }, 502);
    return new Response(null, {
      status: 204,
      headers: { "cache-control": "no-store" },
    });
  }

  if (url.pathname === "/v1/connectors") {
    if (request.method !== "GET" || url.search) return json({ error: "method_not_allowed" }, 405);
    const principal = await authenticatePersistentAccount(request, env, url);
    if (!principal) return json({ error: "unauthorized" }, 401);
    return env.NANOCODEX.fetch(
      `https://broker.internal/users/${encodeURIComponent(principal.userId)}/connectors`,
    );
  }

  const match = url.pathname.match(/^\/v1\/connectors\/([^/]+)(\/callback)?$/);
  if (!match) return undefined;
  const connector = connectorId(match[1]);
  if (!connector) return json({ error: "not_found" }, 404);
  const callback = match[2] === CALLBACK_SUFFIX;
  if ((!callback && request.method !== "POST" && request.method !== "DELETE")
    || (callback && request.method !== "GET")) {
    return json({ error: "method_not_allowed" }, 405);
  }

  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, 401);
  if (!callback) {
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
  }

  const target = `https://broker.internal/users/${encodeURIComponent(principal.userId)}/connectors/${connector}${callback ? "/callback" : ""}`;
  if (callback) return finishCallback(await env.NANOCODEX.fetch(target, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      code: url.searchParams.get("code"),
      state: url.searchParams.get("state"),
      error: url.searchParams.get("error"),
      error_description: url.searchParams.get("error_description"),
    }),
  }), url, connector);

  if (url.search) return json({ error: "invalid_request" }, 400);
  if (request.method === "DELETE") return env.NANOCODEX.fetch(target, { method: "DELETE" });

  const returnTo = await decodeReturnTo(request, url);
  if (!returnTo) return json({ error: "invalid_return_to" }, 400);
  const local = localConnectorAuthorization(url.origin, connector, "managed");
  const response = await env.NANOCODEX.fetch(target, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uri: local?.redirectUri ?? `${url.origin}/v1/connectors/${connector}/callback`,
      return_to: returnTo,
    }),
  });
  if (!local || !response.ok) return response;
  const value: unknown = await response.json().catch(() => undefined);
  if (!isRecord(value) || typeof value.authorization_url !== "string") {
    return json({ error: "connector_broker_failed" }, 502);
  }
  let authorizationUrl: URL;
  try { authorizationUrl = new URL(value.authorization_url); } catch {
    return json({ error: "connector_broker_failed" }, 502);
  }
  try {
    await wrapLocalConnectorAuthorizationState(
      authorizationUrl,
      local,
      env.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY ?? "",
    );
  } catch {
    return json({ error: "connector_broker_failed" }, 502);
  }
  return json({ ...value, authorization_url: authorizationUrl.href }, 200);
}

async function finishCallback(
  response: Response,
  requestUrl: URL,
  connector: ConnectorId,
): Promise<Response> {
  const value: unknown = await response.json().catch(() => undefined);
  if (!response.ok) {
    console.warn("connector callback failed", {
      connector,
      status: response.status,
      error: connectorErrorCode(value),
    });
  }
  if (!isRecord(value) || typeof value.return_to !== "string") {
    return connectorCompletionPage(requestUrl, connector, "failed");
  }
  return connectorCompletionPage(
    requestUrl,
    connector,
    response.ok ? value.connected === true ? "connected" : "cancelled" : "failed",
  );
}

function connectorErrorCode(value: unknown): string {
  const code = isRecord(value) && typeof value.error === "string" ? value.error : undefined;
  return code && CONNECTOR_ERROR_CODES.has(code) ? code : "invalid_response";
}

async function decodeReturnTo(request: Request, url: URL): Promise<string | undefined> {
  let value: unknown;
  try { value = await request.json(); } catch { return undefined; }
  if (!isRecord(value) || typeof value.return_to !== "string") return undefined;
  return safeReturnTo(value.return_to, url);
}

function safeReturnTo(value: string, requestUrl: URL): string | undefined {
  if (!value.startsWith("/") || value.startsWith("//") || value.length > 2_048) return undefined;
  const resolved = new URL(value, requestUrl.origin);
  return resolved.origin === requestUrl.origin ? `${resolved.pathname}${resolved.search}` : undefined;
}

function connectorCompletionPage(
  requestUrl: URL,
  connector: ConnectorId,
  result: "connected" | "cancelled" | "failed",
): Response {
  const completion = JSON.stringify(result === "connected" ? {
    type: "nanocodex:connector-complete",
    connector,
    result: "success",
  } : {
    type: "nanocodex:connector-complete",
    connector,
    result: "error",
    error: result === "cancelled"
      ? "connector_authorization_cancelled"
      : "connector_authorization_failed",
    message: result === "cancelled"
      ? "The account authorization was cancelled. Connect again when you are ready."
      : "The account provider could not complete authorization. Try connecting again.",
  });
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Nanocodex connector</title></head><body><p>Connection flow complete. This window can be closed.</p><script>window.opener?.postMessage(${completion},${JSON.stringify(requestUrl.origin)});window.close();</script></body></html>`;
  return new Response(html, {
    headers: {
      "cache-control": "no-store",
      "content-security-policy": "default-src 'none'; script-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
      "content-type": "text/html; charset=utf-8",
      "cross-origin-opener-policy": "unsafe-none",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff",
    },
  });
}

function connectorId(value: string | undefined): ConnectorId | undefined {
  return value && CONNECTOR.test(value) ? value as ConnectorId : undefined;
}

function mcpConnectionId(value: string | undefined): string | undefined {
  return value && MCP_CONNECTION_ID.test(value) ? value : undefined;
}

async function publicMcpConnectionList(response: Response): Promise<Response> {
  if (!response.ok) {
    await response.body?.cancel();
    return json({ error: "mcp_broker_failed" }, 502);
  }
  const connections = publicMcpConnections(await response.json().catch(() => undefined));
  if (!connections) {
    return json({ error: "mcp_broker_invalid" }, 502);
  }
  return json({
    mcp_connections: connections.filter(({ status }) => status !== "revoked"),
  }, 200);
}

function publicMcpConnections(value: unknown): McpConnection[] | undefined {
  if (!isRecord(value) || !Array.isArray(value.mcp_connections)
    || value.mcp_connections.length > MAX_MCP_CONNECTIONS) return undefined;
  const seen = new Set<string>();
  const connections: McpConnection[] = [];
  for (const candidate of value.mcp_connections) {
    const connection = publicMcpConnection(candidate);
    if (!connection || seen.has(connection.id)) return undefined;
    seen.add(connection.id);
    connections.push(connection);
  }
  return connections;
}

async function publicMcpConnectionResponse(
  response: Response,
  id: string,
): Promise<McpConnection | undefined> {
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  return publicMcpConnections(await response.json().catch(() => undefined))?.find(
    (connection) => connection.id === id,
  );
}

async function mcpStartRequest(
  request: Request,
  url: URL,
  connectionId: string,
): Promise<RequestInit | undefined> {
  const returnTo = await decodeReturnTo(request, url);
  if (!returnTo) return undefined;
  return {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uri: `${url.origin}/v1/connectors/mcp-connections/${connectionId}/callback`,
      return_to: returnTo,
    }),
  };
}

function mcpCallbackRequest(url: URL): RequestInit {
  return {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      code: url.searchParams.get("code"),
      state: url.searchParams.get("state"),
      error: url.searchParams.get("error"),
      error_description: url.searchParams.get("error_description"),
    }),
  };
}

async function publicMcpStartResponse(response: Response, id: string): Promise<Response> {
  if (!response.ok) {
    await response.body?.cancel();
    return json({ error: "mcp_broker_failed" }, 502);
  }
  const value: unknown = await response.json().catch(() => undefined);
  const connection = publicMcpConnections(value)?.find((candidate) => candidate.id === id);
  const authorizationUrl = isRecord(value) && typeof value.authorization_url === "string"
    ? safeAuthorizationUrl(value.authorization_url)
    : undefined;
  return connection && authorizationUrl
    ? json({ mcp_connection: connection, authorization_url: authorizationUrl }, 200)
    : json({ error: "mcp_broker_invalid" }, 502);
}

async function finishMcpCallback(response: Response, url: URL, id: string): Promise<Response> {
  const value: unknown = await response.json().catch(() => undefined);
  const returnTo = isRecord(value) && typeof value.return_to === "string"
    ? safeReturnTo(value.return_to, url)
    : undefined;
  const connection = publicMcpConnections(value)?.find((candidate) => candidate.id === id);
  const result = response.ok && connection?.status === "connected"
    ? "connected"
    : url.searchParams.has("error") ? "cancelled" : "failed";
  return redirectMcpResult(url, returnTo ?? "/", id, result);
}

function safeAuthorizationUrl(value: string): string | undefined {
  if (value.length > 8_192) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.hash ? url.href : undefined;
  } catch { return undefined; }
}

function redirectMcpResult(
  requestUrl: URL,
  returnTo: string,
  id: string,
  result: "connected" | "cancelled" | "failed",
): Response {
  const destination = new URL(returnTo, requestUrl.origin);
  destination.searchParams.set("mcp_connection", id);
  destination.searchParams.set("mcp_result", result);
  return new Response(null, {
    status: 303,
    headers: {
      "cache-control": "no-store",
      location: destination.href,
      "referrer-policy": "no-referrer",
    },
  });
}

async function decodeMcpTarget(request: Request): Promise<string | undefined> {
  const contentLength = Number(request.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > MAX_MCP_CREATE_BODY_BYTES) return undefined;
  const text = await request.text();
  if (new TextEncoder().encode(text).byteLength > MAX_MCP_CREATE_BODY_BYTES) return undefined;
  let value: unknown;
  try { value = JSON.parse(text); } catch { return undefined; }
  return isRecord(value) && Object.keys(value).length === 1 && typeof value.target === "string"
    ? value.target
    : undefined;
}

function newMcpConnectionId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function publicMcpConnection(value: unknown): McpConnection | undefined {
  if (!isRecord(value)
    || !mcpConnectionId(typeof value.id === "string" ? value.id : undefined)
    || typeof value.name !== "string"
    || !MCP_CONNECTION_NAME.test(value.name)
    || value.name.trim().length === 0
    || typeof value.status !== "string"
    || !MCP_CONNECTION_STATUSES.has(value.status as McpConnectionStatus)) return undefined;
  return {
    id: value.id as string,
    name: value.name,
    status: value.status as McpConnectionStatus,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}
