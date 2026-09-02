import {
  authenticatePersistentAccount,
  requireSameOriginMutation,
  type AccountAuthEnv,
} from "./account-auth";
import { browserEgressSubject } from "./browser-egress";
import { bindAgentCredential } from "./credentials";
import {
  localConnectorAuthorization,
  wrapLocalConnectorAuthorizationState,
} from "nanocodex-vite/oauth-relay";
import { canonicalRemoteMcpTarget } from "../../mcp-target.mjs";
import {
  connectorConnectionId,
  connectorProviderId,
  type ConnectorProviderId,
} from "./connector-status";

type ConnectorEnv = AccountAuthEnv & {
  NANOCODEX: Fetcher;
  NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY?: string;
};
type ConnectorRouteId = ConnectorProviderId | "gmail" | "gdrive";
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
const MCP_PROXY_METHODS = new Set(["DELETE", "GET", "POST"]);
const MCP_PROXY_REQUEST_HEADERS = [
  "accept",
  "content-type",
  "last-event-id",
  "mcp-protocol-version",
  "mcp-session-id",
] as const;
const MCP_PROXY_RESPONSE_HEADERS = [
  "content-type",
  "mcp-session-id",
  "retry-after",
] as const;
const CONNECTOR_ERROR_CODES = new Set([
  "authorization_code_missing",
  "connector_broker_failed",
  "connector_account_mismatch",
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
    /^\/v1\/connectors\/mcp-connections\/([^/]+)(?:\/(start|callback|proxy))?$/,
  );
  if (mcpMatch) {
    const connectionId = mcpConnectionId(mcpMatch[1]);
    if (!connectionId) return json({ error: "not_found" }, 404);
    const operation = mcpMatch[2];
    if (operation === "proxy") {
      if (!MCP_PROXY_METHODS.has(request.method)) {
        return json({ error: "method_not_allowed" }, 405);
      }
      const threadId = url.searchParams.get("thread_id");
      if ([...url.searchParams].length !== 1 || !threadId || !UUID.test(threadId)) {
        return json({ error: "invalid_thread_id" }, 400);
      }
      const principal = await authenticatePersistentAccount(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, 401);
      if (!sameOriginMcpRequest(request, url)) {
        return json({ error: "forbidden_origin" }, 403);
      }
      const subject = await browserEgressSubject(principal.userId, threadId);
      try {
        await bindAgentCredential(env.NANOCODEX, subject, principal.userId);
      } catch {
        return json({ error: "credential_broker_unavailable" }, 503);
      }
      const headers = new Headers();
      for (const name of MCP_PROXY_REQUEST_HEADERS) {
        const value = request.headers.get(name);
        if (value !== null) headers.set(name, value);
      }
      headers.set("x-nanocodex-subject", subject);
      const response = await env.NANOCODEX.fetch(new Request(
        `https://mcp.internal/v1/connections/${connectionId}`,
        {
          method: request.method,
          headers,
          ...(request.method === "GET" || request.method === "HEAD" || request.body === null
            ? {}
            : { body: request.body }),
          redirect: "manual",
          signal: request.signal,
        },
      ));
      const responseHeaders = new Headers();
      for (const name of MCP_PROXY_RESPONSE_HEADERS) {
        const value = response.headers.get(name);
        if (value !== null) responseHeaders.set(name, value);
      }
      responseHeaders.set("cache-control", "no-store");
      responseHeaders.set("x-content-type-options", "nosniff");
      return new Response(response.body, {
        status: response.status,
        statusText: response.statusText,
        headers: responseHeaders,
      });
    }
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

  const match = url.pathname.match(
    /^\/v1\/connectors\/([^/]+)(?:\/(callback)|\/connections\/([^/]+))?$/,
  );
  if (!match) return undefined;
  const routeConnector = connectorRouteId(match[1]);
  const provider = connectorProviderId(routeConnector);
  if (!routeConnector || !provider) return json({ error: "not_found" }, 404);
  const callback = match[2] === "callback";
  const connectionId = match[3] === undefined ? undefined : connectorConnectionId(match[3]);
  if ((match[3] !== undefined && !connectionId)
    || (callback && request.method !== "GET")
    || (connectionId && request.method !== "DELETE")
    || (!callback && !connectionId && request.method !== "POST")) {
    return json({ error: "method_not_allowed" }, 405);
  }

  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, 401);
  if (!callback) {
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
  }

  const target = `https://broker.internal/users/${encodeURIComponent(principal.userId)}/connectors/${provider}${callback ? "/callback" : connectionId ? `/connections/${connectionId}` : ""}`;
  if (callback) return finishCallback(await env.NANOCODEX.fetch(target, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      code: url.searchParams.get("code"),
      state: url.searchParams.get("state"),
      error: url.searchParams.get("error"),
      error_description: url.searchParams.get("error_description"),
    }),
  }), url, routeConnector);

  if (url.search) return json({ error: "invalid_request" }, 400);
  if (connectionId) return env.NANOCODEX.fetch(target, { method: "DELETE" });

  const returnTo = await decodeReturnTo(request, url);
  if (!returnTo) return json({ error: "invalid_return_to" }, 400);
  const local = localConnectorAuthorization(url.origin, routeConnector, "managed");
  const response = await env.NANOCODEX.fetch(target, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uri: local?.redirectUri ?? `${url.origin}/v1/connectors/${routeConnector}/callback`,
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
  connector: ConnectorRouteId,
): Promise<Response> {
  const value: unknown = await response.json().catch(() => undefined);
  if (!response.ok) {
    console.warn({
      type: "connector.callback_failed",
      connector,
      status: response.status,
      error_code: connectorErrorCode(value),
    });
  }
  if (!isRecord(value) || typeof value.return_to !== "string") {
    return connectorCompletionPage(requestUrl, connector, "failed");
  }
  const returnTo = safeReturnTo(value.return_to, requestUrl);
  return connectorCompletionPage(
    requestUrl,
    connector,
    response.ok ? value.connected === true ? "connected" : "cancelled" : "failed",
    returnTo,
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
  connector: ConnectorRouteId,
  result: "connected" | "cancelled" | "failed",
  returnTo?: string,
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
  const fallback = returnTo === undefined
    ? ""
    : `else{window.location.replace(${JSON.stringify(new URL(returnTo, requestUrl.origin).href)})}`;
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Nanocodex connector</title></head><body><p>Connection flow complete. This window can be closed.</p><script>if(window.opener){window.opener.postMessage(${completion},${JSON.stringify(requestUrl.origin)});window.close()}${fallback}</script></body></html>`;
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

function connectorRouteId(value: string | undefined): ConnectorRouteId | undefined {
  return value === "gmail" || value === "gdrive"
    ? value
    : connectorProviderId(value);
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

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function sameOriginMcpRequest(request: Request, url: URL): boolean {
  if (request.headers.get("x-nanocodex-request") === "1"
    && request.headers.get("sec-fetch-site") === "same-origin") return true;
  for (const name of ["origin", "referer"] as const) {
    const value = request.headers.get(name);
    if (!value) continue;
    try {
      if (new URL(value).origin === url.origin) return true;
    } catch { return false; }
  }
  return false;
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
