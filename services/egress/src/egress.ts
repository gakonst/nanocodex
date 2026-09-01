import {
  AgentSubjectDirectory,
  type BrokerEnv,
  UserCredentialBroker,
  type UserCredentialSnapshot,
  validChatGptCredentialImport,
} from "./broker";
import {
  UserConnectorBroker,
  type ConnectorBrokerEnv,
} from "./connector-broker";
import { canonicalConnectorPath } from "./connector-path";
import {
  McpConnectionDirectory,
  validMcpConnectionMaterialization,
} from "./mcp-connection-owner";
import {
  BrokeredSshError,
  executeBrokeredSsh,
  type BrokeredSshIdentity,
  validateBrokeredSshRequest,
  validateSshIdentity,
  validSshIdentityReference,
} from "./ssh";

export { AgentSubjectDirectory, UserCredentialBroker } from "./broker";
export { UserConnectorBroker } from "./connector-broker";
export { McpConnectionDirectory } from "./mcp-connection-owner";

const SUBJECT_DIRECTORY_PREFIX = "agent-subject-v1:";
const READINESS_SUBJECT_DIRECTORY_NAME = "agent-subject-readiness-v1";
const SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const USER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const SUBJECT_HEADER = "x-nanocodex-subject";
const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const MODEL_STATUS_PATH = "/.well-known/nanocodex/model-status";
const BROKER_READINESS_PATH = "/.well-known/nanocodex/broker-readiness";
const MAX_CONTROL_BODY_BYTES = 16 * 1024;
const MAX_CHATGPT_IMPORT_BODY_BYTES = 64 * 1024;
const MAX_BROKER_RESPONSE_BYTES = 4 * 1024;
const MAX_MODEL_BODY_BYTES = 32 * 1024 * 1024;
const MAX_SSH_BODY_BYTES = 72 * 1024;
const CODEX_ATTESTATION_UNAVAILABLE = '{"v":1,"s":1}';
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const RELAY_CAPABILITY_PATH = /^\/v1\/[A-Za-z0-9_-]{43,}$/;
const RELAY_HTTP_ROUTES: Readonly<Record<ModelOperation["id"], string | undefined>> = {
  responses: undefined,
  search: "codex-web-search",
  "image-generation": "codex-image-generation",
  "image-edit": "codex-image-edit",
  "realtime-call": undefined,
  "realtime-sideband": undefined,
};

type ConnectorOperation = Readonly<{
  id: "github" | "gmail" | "gdrive" | "x";
  origin: `https://${string}`;
  paths: readonly RegExp[];
}>;

const CONNECTOR_OPERATIONS: readonly ConnectorOperation[] = [
  {
    id: "github",
    origin: "https://api.github.com",
    paths: [/^\//],
  },
  {
    id: "gmail",
    origin: "https://gmail.googleapis.com",
    paths: [/^\/gmail\/v1\/users\/me(?:\/|$)/],
  },
  {
    id: "gdrive",
    origin: "https://www.googleapis.com",
    paths: [/^\/drive\/v3(?:\/|$)/, /^\/upload\/drive\/v3(?:\/|$)/],
  },
  {
    id: "x",
    origin: "https://api.x.com",
    paths: [
      /^\/2\/tweets(?:\/|$)/,
      /^\/2\/users(?:\/|$)/,
      /^\/2\/lists(?:\/|$)/,
      /^\/2\/dm_(?:conversations|events)(?:\/|$)/,
      /^\/2\/media(?:\/|$)/,
    ],
  },
];

export interface EgressEnv extends BrokerEnv, ConnectorBrokerEnv {
  USER_CREDENTIALS: DurableObjectNamespace<UserCredentialBroker>;
  USER_CONNECTORS: DurableObjectNamespace<UserConnectorBroker>;
  AGENT_SUBJECTS: DurableObjectNamespace<AgentSubjectDirectory>;
  MCP_CONNECTIONS: DurableObjectNamespace<McpConnectionDirectory>;
  CHATGPT_EGRESS?: DurableObjectNamespace;
  CODEX_RELAY_URL?: string;
  ALLOW_INSECURE_LOOPBACK_RELAY?: string;
  NANOCODEX_BROKER_PROBE_TOKEN?: string;
  DEPLOYMENT_SHA?: string;
}

type ModelOperation = Readonly<{
  id: "responses" | "search" | "image-generation" | "image-edit"
    | "realtime-call" | "realtime-sideband";
  method: "GET" | "POST";
  path: `/v1/${string}`;
  websocket: boolean;
  openai: `https://${string}`;
  chatgpt: `https://${string}`;
  chatGptOnly?: true;
  directChatGpt?: true;
}>;

const OPERATIONS: readonly ModelOperation[] = [
  {
    id: "responses",
    method: "GET",
    path: "/v1/responses",
    websocket: true,
    openai: "https://api.openai.com/v1/responses",
    chatgpt: "https://chatgpt.com/backend-api/codex/responses",
  },
  {
    id: "search",
    method: "POST",
    path: "/v1/search",
    websocket: false,
    openai: "https://api.openai.com/v1/alpha/search",
    chatgpt: "https://chatgpt.com/backend-api/codex/alpha/search",
  },
  {
    id: "image-generation",
    method: "POST",
    path: "/v1/images/generations",
    websocket: false,
    openai: "https://api.openai.com/v1/images/generations",
    chatgpt: "https://chatgpt.com/backend-api/codex/images/generations",
  },
  {
    id: "image-edit",
    method: "POST",
    path: "/v1/images/edits",
    websocket: false,
    openai: "https://api.openai.com/v1/images/edits",
    chatgpt: "https://chatgpt.com/backend-api/codex/images/edits",
  },
  {
    id: "realtime-call",
    method: "POST",
    path: "/v1/realtime/calls",
    websocket: false,
    openai: "https://api.openai.com/v1/realtime/calls?intent=quicksilver&architecture=avas",
    chatgpt: "https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
    chatGptOnly: true,
  },
  {
    id: "realtime-sideband",
    method: "GET",
    path: "/v1/realtime/sideband",
    websocket: true,
    openai: "https://api.openai.com/v1/live/",
    chatgpt: "https://api.openai.com/v1/live/",
    chatGptOnly: true,
    directChatGpt: true,
  },
];

export default {
  fetch(request: Request, env: EgressEnv, ctx: ExecutionContext): Promise<Response> {
    return handleEgress(request, env, ctx);
  },
} satisfies ExportedHandler<EgressEnv>;

export async function handleEgress(
  request: Request,
  env: EgressEnv,
  _ctx?: Pick<ExecutionContext, "waitUntil">,
  upstreamFetch: typeof fetch = fetch,
  diagnostics?: Readonly<{ upstreamException(error: Readonly<{ name: string }>): void }>,
): Promise<Response> {
  const started = Date.now();
  let url: URL;
  try { url = new URL(request.url); } catch { return jsonError(400, "invalid_url"); }
  if (url.username || url.password || url.hash) return jsonError(403, "destination_denied");

  if (url.protocol === "https:" && url.hostname === "ssh.internal" && !url.port
    && url.pathname === "/v1/execute" && !url.search) {
    return handleSshEgress(request, url, env, started);
  }

  const mcpConnection = mcpConnectionId(url);
  if (mcpConnection) {
    return handleMcpEgress(request, url, mcpConnection, env, started);
  }
  const connector = connectorOperation(url);
  if (connector) return handleConnectorEgress(request, url, connector, env, started);
  if (url.search) return jsonError(403, "destination_denied");

  if (url.pathname.startsWith("/subjects/") || url.pathname.startsWith("/users/")) {
    const response = await handleControl(request, url, env);
    auditControl(request, url, response.status, started, env.DEPLOYMENT_SHA);
    return response;
  }
  if (url.pathname === BROKER_READINESS_PATH) return handleReadiness(request, env);
  if (url.pathname === MODEL_STATUS_PATH) return handleModelStatus(request, env);

  const operation = OPERATIONS.find((candidate) => (
    candidate.method === request.method && candidate.path === url.pathname
      && url.protocol === "https:" && url.hostname === "nanocodex.internal" && !url.port
  ));
  if (!operation) return auditedError(403, "destination_denied", request, url, undefined, started);
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) {
    return auditedError(403, "agent_subject_required", request, url, operation.id, started);
  }
  if (request.headers.get("authorization") !== PROVIDER_PLACEHOLDER) {
    return auditedError(403, "credential_placeholder_mismatch", request, url, operation.id, started);
  }
  if (request.headers.has("chatgpt-account-id") || request.headers.has("x-openai-fedramp")
    || request.headers.has("originator")) {
    return auditedError(403, "provider_header_forbidden", request, url, operation.id, started);
  }
  if (operation.websocket) {
    const responseHeadersValid = operation.id !== "responses"
      || request.headers.get("openai-beta")?.toLowerCase()
        === "responses_websockets=2026-02-06";
    const realtimeHeadersValid = operation.id !== "realtime-sideband"
      || validRealtimeCallId(request.headers.get("x-nanocodex-realtime-call-id"));
    if (request.headers.get("upgrade")?.toLowerCase() !== "websocket"
      || !responseHeadersValid || !realtimeHeadersValid) {
      return auditedError(403, "required_header_mismatch", request, url, operation.id, started);
    }
  } else if (request.headers.get("content-type")?.toLowerCase() !== "application/json") {
    return auditedError(403, "required_header_mismatch", request, url, operation.id, started);
  }

  let userId: string | undefined;
  try {
    userId = await resolveSubject(env, subject);
    let credential = await resolveCredential(env, userId, false);
    if (operation.chatGptOnly && credential.kind !== "chatgpt") {
      return auditedError(409, "chatgpt_credential_required", request, url, operation.id, started, {
        user_id: userId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    }
    const body = await replayableBody(request, operation);
    let upstream = await fetchUpstream(
      env,
      userId,
      credential,
      operation,
      buildUpstreamRequest(request, env, operation, credential, body),
      upstreamFetch,
    );
    let recovered = false;
    if (upstream.status === 401 && credential.kind === "chatgpt") {
      await cancelResponseBody(upstream);
      credential = await resolveCredential(env, userId, true, credential.revision);
      if (operation.chatGptOnly && credential.kind !== "chatgpt") {
        return auditedError(409, "chatgpt_credential_required", request, url, operation.id, started, {
          user_id: userId,
          deployment_sha: env.DEPLOYMENT_SHA,
        });
      }
      upstream = await fetchUpstream(
        env,
        userId,
        credential,
        operation,
        buildUpstreamRequest(request, env, operation, credential, body),
        upstreamFetch,
      );
      recovered = true;
    }
    if (REDIRECT_STATUS.has(upstream.status)) {
      await cancelResponseBody(upstream);
      return auditedError(502, "upstream_redirect_blocked", request, url, operation.id, started, {
        user_id: userId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    }
    if (upstream.status >= 400) {
      const upstreamStatus = upstream.status;
      await cancelResponseBody(upstream);
      return auditedError(
        upstreamStatus === 429 ? 503 : 502,
        "upstream_rejected",
        request,
        url,
        operation.id,
        started,
        {
          upstream_status: upstreamStatus,
          user_id: userId,
          deployment_sha: env.DEPLOYMENT_SHA,
        },
      );
    }
    audit("allow", request, url, operation.id, started, {
      status: upstream.status,
      recovered,
      user_id: userId,
      deployment_sha: env.DEPLOYMENT_SHA,
    });
    return sanitizeUpstreamResponse(upstream);
  } catch (error) {
    const problem = egressFailure(error);
    if (!(error instanceof EgressFailure)) {
      const detail = { name: error instanceof Error ? error.name : typeof error };
      diagnostics?.upstreamException(detail);
      console.error(JSON.stringify({ type: "egress.upstream_exception", ...detail }));
    }
    return auditedError(problem.status, problem.code, request, url, operation.id, started,
      {
        ...(userId === undefined ? {} : { user_id: userId }),
        deployment_sha: env.DEPLOYMENT_SHA,
      });
  }
}

async function handleSshEgress(
  request: Request,
  url: URL,
  env: EgressEnv,
  started: number,
): Promise<Response> {
  if (request.method !== "POST") return auditedError(403, "method_denied", request, url, "ssh", started);
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) {
    return auditedError(403, "agent_subject_required", request, url, "ssh", started);
  }
  if (request.headers.get("content-type")?.toLowerCase() !== "application/json"
    || request.headers.has("authorization") || request.headers.has("cookie")
    || request.headers.has("proxy-authorization")) {
    return auditedError(403, "required_header_mismatch", request, url, "ssh", started);
  }
  const parsed = validateBrokeredSshRequest(await readJson(request, MAX_SSH_BODY_BYTES));
  if (!parsed) return auditedError(400, "invalid_ssh_request", request, url, "ssh", started);
  let userId: string | undefined;
  try {
    userId = await resolveSubject(env, subject);
    const identity = await resolveSshIdentity(env, userId, parsed.identityReference);
    const result = await executeBrokeredSsh(identity, parsed, request.signal);
    audit("allow", request, url, "ssh", started, {
      status: 200,
      user_id: userId,
      deployment_sha: env.DEPLOYMENT_SHA,
    });
    return json({ stdout: result.stdout, stderr: result.stderr, exit_code: result.exitCode }, 200);
  } catch (error) {
    const problem = error instanceof BrokeredSshError
      ? new EgressFailure(error.status, error.code)
      : egressFailure(error);
    return auditedError(problem.status, problem.code, request, url, "ssh", started, {
      ...(userId === undefined ? {} : { user_id: userId }),
      deployment_sha: env.DEPLOYMENT_SHA,
    });
  }
}

async function handleMcpEgress(
  request: Request,
  url: URL,
  connectionId: string,
  env: EgressEnv,
  started: number,
): Promise<Response> {
  if (!CONNECTOR_METHODS.has(request.method)) {
    return auditedError(403, "method_denied", request, url, "mcp", started);
  }
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) {
    return auditedError(403, "agent_subject_required", request, url, "mcp", started);
  }
  if (request.headers.has("authorization") || request.headers.has("cookie")
    || request.headers.has("proxy-authorization")) {
    return auditedError(403, "caller_credential_forbidden", request, url, "mcp", started);
  }
  let userId: string | undefined;
  try {
    userId = await resolveSubject(env, subject);
    const owner = await resolveMcpConnectionOwner(env, connectionId);
    if (owner !== userId) {
      return auditedError(403, "mcp_connection_owner_mismatch", request, url, "mcp", started, {
        user_id: userId,
        mcp_connection_id: connectionId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    }
    const headers = new Headers();
    for (const name of [
      "accept",
      "content-type",
      "mcp-protocol-version",
      "mcp-session-id",
      "last-event-id",
    ]) {
      const value = request.headers.get(name);
      if (value !== null) headers.set(name, value);
    }
    const response = await connectorBroker(env, userId).fetch(new Request(
      `https://mcp-connections.internal/v1/connections/${connectionId}/proxy`,
      {
        method: request.method,
        headers,
        ...(request.method === "GET" || request.method === "HEAD" || request.body === null
          ? {}
          : { body: request.body }),
      },
    ));
    audit(response.status >= 500 ? "error" : response.status >= 400 ? "deny" : "allow",
      request, url, "mcp", started, {
        status: response.status,
        user_id: userId,
        mcp_connection_id: connectionId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    return response;
  } catch (error) {
    const problem = egressFailure(error);
    return auditedError(problem.status, problem.code, request, url, "mcp", started, {
      ...(userId === undefined ? {} : { user_id: userId }),
      mcp_connection_id: connectionId,
      deployment_sha: env.DEPLOYMENT_SHA,
    });
  }
}

function mcpConnectionId(url: URL): string | undefined {
  if (url.protocol !== "https:" || url.hostname !== "mcp.internal" || url.port || url.search) {
    return undefined;
  }
  return url.pathname.match(/^\/v1\/connections\/([A-Za-z0-9_-]{43})$/)?.[1];
}

async function handleConnectorEgress(
  request: Request,
  url: URL,
  connector: ConnectorOperation,
  env: EgressEnv,
  started: number,
): Promise<Response> {
  if (!CONNECTOR_METHODS.has(request.method)) {
    return auditedError(403, "method_denied", request, url, connector.id, started);
  }
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) {
    return auditedError(403, "agent_subject_required", request, url, connector.id, started);
  }
  if (request.headers.get("authorization") !== PROVIDER_PLACEHOLDER) {
    return auditedError(403, "credential_placeholder_mismatch", request, url, connector.id, started);
  }
  let userId: string | undefined;
  try {
    userId = await resolveSubject(env, subject);
    const response = await connectorBroker(env, userId).fetch(request);
    audit(response.status >= 500 ? "error" : response.status >= 400 ? "deny" : "allow",
      request, url, connector.id, started, {
        status: response.status,
        user_id: userId,
        connector: connector.id,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    return sanitizeUpstreamResponse(response);
  } catch (error) {
    const problem = egressFailure(error);
    return auditedError(problem.status, problem.code, request, url, connector.id, started, {
      ...(userId === undefined ? {} : { user_id: userId }),
      connector: connector.id,
      deployment_sha: env.DEPLOYMENT_SHA,
    });
  }
}

function connectorOperation(url: URL): ConnectorOperation | undefined {
  if (url.href.length > 8_192) return undefined;
  return CONNECTOR_OPERATIONS.find((candidate) => candidate.origin === url.origin
    && canonicalConnectorPath(candidate.id, url.pathname)
    && candidate.paths.some((path) => path.test(url.pathname)));
}

function sanitizeUpstreamResponse(upstream: Response): Response {
  // An upgraded socket must be returned intact. Its peer is the explicitly
  // trusted provider/relay selected by the fixed rule, never caller input.
  if (upstream.webSocket) return upstream;
  const headers = new Headers(upstream.headers);
  for (const name of [
    "authorization",
    "chatgpt-account-id",
    "proxy-authenticate",
    "proxy-authorization",
    "set-cookie",
    "x-openai-fedramp",
  ]) headers.delete(name);
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers,
  });
}

async function handleControl(request: Request, url: URL, env: EgressEnv): Promise<Response> {
  const subjectMatch = url.pathname.match(/^\/subjects\/([A-Za-z0-9_-]{43,128})$/);
  if (subjectMatch) {
    if (request.method !== "PUT" && request.method !== "DELETE") {
      return jsonError(405, "method_not_allowed");
    }
    const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
    const userId = stringField(body, "user_id");
    if (!USER_ID.test(userId ?? "")) return jsonError(400, "invalid_request");
    return subjectDirectory(env, subjectMatch[1]!).fetch(
      `https://subjects.internal/v1/${request.method === "PUT" ? "bind" : "unbind"}`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ subject: subjectMatch[1], user_id: userId }),
      },
    );
  }

  const mcpMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/mcp-connections(?:\/([A-Za-z0-9_-]{43})(?:\/(start|callback))?)?$/,
  );
  if (mcpMatch) {
    const userId = mcpMatch[1]!;
    const connectionId = mcpMatch[2];
    const operation = mcpMatch[3];
    const allowed = (!connectionId && !operation && request.method === "GET")
      || (connectionId && !operation
        && (request.method === "GET" || request.method === "PUT" || request.method === "DELETE"))
      || (connectionId && (operation === "start" || operation === "callback")
        && request.method === "POST");
    if (!allowed) return jsonError(405, "method_not_allowed");
    let forwardedBody: BodyInit | null = request.body;
    if (connectionId && request.method === "PUT") {
      const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
      if (!validMcpConnectionMaterialization(body)) return jsonError(400, "invalid_request");
      const ownershipFailure = await bindMcpConnectionOwner(env, connectionId, userId);
      if (ownershipFailure) return ownershipFailure;
      forwardedBody = new TextEncoder().encode(JSON.stringify(body));
    } else if (connectionId) {
      const owner = await resolveMcpConnectionOwner(env, connectionId);
      if (owner === undefined) return jsonError(404, "mcp_connection_not_found");
      if (owner !== userId) return jsonError(403, "mcp_connection_owner_mismatch");
    }
    const target = connectionId
      ? `https://mcp-connections.internal/v1/connections/${connectionId}${operation ? `/${operation}` : ""}`
      : "https://mcp-connections.internal/v1/connections";
    return connectorBroker(env, userId).fetch(target, {
      method: request.method,
      ...(forwardedBody === null ? {} : {
        headers: { "content-type": request.headers.get("content-type") ?? "" },
        body: forwardedBody,
      }),
    });
  }

  const connectorMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/connectors(?:\/(github|gmail|gdrive|x)(\/callback)?(?:\/([A-Za-z0-9_-]{43}))?)?$/,
  );
  if (connectorMatch) {
    const userId = connectorMatch[1]!;
    const connector = connectorMatch[2];
    const callback = connectorMatch[3] === "/callback";
    const connectionId = connectorMatch[4];
    const target = connector
      ? `https://connectors.internal/v1/${connector}${callback
        ? "/callback"
        : connectionId ? `/connections/${connectionId}` : request.method === "POST" ? "/start" : ""}`
      : "https://connectors.internal/v1/status";
    if ((!connector && request.method !== "GET")
      || (connector && callback && request.method !== "POST")
      || (connectionId && (connector === "github" || connector === "x" || request.method !== "DELETE"))
      || (connector && !callback && !connectionId && request.method !== "POST" && request.method !== "DELETE")) {
      return jsonError(405, "method_not_allowed");
    }
    return connectorBroker(env, userId).fetch(target, {
      method: request.method,
      ...(request.body === null ? {} : {
        headers: { "content-type": request.headers.get("content-type") ?? "" },
        body: request.body,
      }),
    });
  }

  const sshIdentityMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/credentials\/ssh\/([A-Za-z0-9][A-Za-z0-9._-]{0,63})$/,
  );
  if (sshIdentityMatch) {
    if (request.method !== "PUT" && request.method !== "DELETE") {
      return jsonError(405, "method_not_allowed");
    }
    const userId = sshIdentityMatch[1]!;
    const reference = sshIdentityMatch[2]!;
    if (!validSshIdentityReference(reference)) return jsonError(400, "invalid_ssh_identity_reference");
    const target = `https://credentials.internal/v1/ssh-identities/${encodeURIComponent(reference)}`;
    if (request.method === "DELETE") return userBroker(env, userId).fetch(target, { method: "DELETE" });
    if (request.headers.get("content-type")?.toLowerCase() !== "application/json") {
      return jsonError(400, "invalid_ssh_identity");
    }
    const body = await readJson(request, MAX_SSH_BODY_BYTES);
    const identity = validateSshIdentity(body);
    if (!identity) return jsonError(400, "invalid_ssh_identity");
    return userBroker(env, userId).fetch(target, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        private_key: identity.privateKey,
        hostname: identity.hostname,
        port: identity.port,
        username: identity.username,
        host_key_sha256: identity.hostKeySha256,
      }),
    });
  }

  const userMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/credentials(?:\/(openai|chatgpt|chatgpt\/login|chatgpt\/login\/status|chatgpt\/local-claim))?$/,
  );
  if (!userMatch) return jsonError(404, "not_found");
  const userId = userMatch[1]!;
  const operation = userMatch[2];

  if (operation === "chatgpt/local-claim") {
    if (request.method !== "POST") return jsonError(405, "method_not_allowed");
    if (!localClaimEnabled(env)) return jsonError(404, "not_found");
    if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
    return userBroker(env, userId).fetch("https://credentials.internal/v1/chatgpt/local-claim", {
      method: "POST",
    });
  }

  if (!operation && request.method === "GET") {
    return userBroker(env, userId).fetch("https://credentials.internal/v1/status");
  }
  if (operation === "openai" && request.method === "PUT") {
    const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
    return userBroker(env, userId).fetch("https://credentials.internal/v1/openai-key", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ api_key: stringField(body, "api_key") }),
    });
  }
  if (operation === "openai" && request.method === "DELETE") {
    return userBroker(env, userId).fetch("https://credentials.internal/v1/openai-key", {
      method: "DELETE",
    });
  }
  if (operation === "chatgpt" && request.method === "PUT") {
    if (request.headers.get("content-type")?.toLowerCase() !== "application/json") {
      return jsonError(400, "invalid_chatgpt_credential");
    }
    let body: unknown;
    try {
      body = JSON.parse(await readBoundedText(request, MAX_CHATGPT_IMPORT_BODY_BYTES));
    } catch (error) {
      return error instanceof EgressFailure
        ? jsonError(error.status, error.code)
        : jsonError(400, "invalid_chatgpt_credential");
    }
    if (!validChatGptCredentialImport(body)) {
      return jsonError(400, "invalid_chatgpt_credential");
    }
    return userBroker(env, userId).fetch("https://credentials.internal/v1/chatgpt", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  }
  if (operation === "chatgpt/login" && request.method === "POST") {
    return userBroker(env, userId).fetch("https://credentials.internal/v1/chatgpt/login/start", {
      method: "POST",
    });
  }
  if (operation === "chatgpt/login/status" && request.method === "POST") {
    return userBroker(env, userId).fetch("https://credentials.internal/v1/chatgpt/login/status", {
      method: "POST",
    });
  }
  if (operation === "chatgpt" && request.method === "DELETE") {
    return userBroker(env, userId).fetch("https://credentials.internal/v1/chatgpt", {
      method: "DELETE",
    });
  }
  return jsonError(405, "method_not_allowed");
}

async function handleReadiness(request: Request, env: EgressEnv): Promise<Response> {
  if (request.method !== "POST") return jsonError(404, "not_found");
  const token = env.NANOCODEX_BROKER_PROBE_TOKEN;
  if (!token || token.length < 32 || token.length > 512
    || request.headers.get("authorization") !== `Bearer ${token}`) {
    return jsonError(404, "not_found");
  }
  if (await hasRequestPayload(request)) return jsonError(404, "not_found");
  try {
    const [subjects, credentials] = await Promise.all([
      env.AGENT_SUBJECTS.getByName(READINESS_SUBJECT_DIRECTORY_NAME)
        .fetch("https://subjects.internal/v1/health"),
      userBroker(env, "broker-readiness-v1").fetch("https://credentials.internal/v1/health"),
    ]);
    if (!subjects.ok || !credentials.ok) {
      await Promise.all([
        cancelResponseBody(subjects),
        cancelResponseBody(credentials),
      ]);
      return jsonError(503, "broker_not_ready");
    }
    await Promise.all([
      cancelResponseBody(subjects),
      cancelResponseBody(credentials),
    ]);
    return json({ ready: true }, 200);
  } catch { return jsonError(503, "broker_not_ready"); }
}

async function hasRequestPayload(request: Request): Promise<boolean> {
  if (request.body === null) return false;
  const reader = request.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return false;
      if (value.byteLength > 0) return true;
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

async function handleModelStatus(request: Request, env: EgressEnv): Promise<Response> {
  if (request.method !== "GET" || request.body !== null) return jsonError(404, "not_found");
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) return jsonError(403, "agent_subject_required");
  try {
    const userId = await resolveSubject(env, subject);
    await resolveCredential(env, userId, false);
    return json({ ready: true }, 200);
  } catch { return jsonError(503, "broker_not_ready"); }
}

function buildUpstreamRequest(
  original: Request,
  env: EgressEnv,
  operation: ModelOperation,
  credential: UserCredentialSnapshot,
  body: Uint8Array | null,
): Request {
  const headers = new Headers();
  const realtime = operation.id === "realtime-call" || operation.id === "realtime-sideband";
  const allowed = operation.id === "responses"
    ? ["openai-beta", "session-id", "thread-id", "upgrade", "user-agent",
        "x-client-request-id", "x-codex-turn-state",
        "x-openai-internal-codex-responses-lite", "x-responsesapi-include-timing-metrics"]
    : operation.id === "realtime-sideband"
      ? ["openai-alpha", "session-id", "thread-id", "upgrade", "x-session-id"]
      : ["content-type", "user-agent"];
  for (const name of allowed) {
    const value = original.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  if (realtime) {
    const realtimeSessionId = original.headers.get("x-session-id");
    const sessionId = original.headers.get("session-id");
    const threadId = original.headers.get("thread-id");
    const validId = (value: string | null): value is string =>
      value !== null && /^[A-Za-z0-9._:-]{1,200}$/.test(value);
    if (original.headers.get("openai-alpha") !== "quicksilver=v2"
      || !validId(realtimeSessionId) || !validId(sessionId) || !validId(threadId)) {
      throw new EgressFailure(400, "invalid_realtime_session");
    }
    headers.set("openai-alpha", "quicksilver=v2");
    headers.set("x-oai-attestation", CODEX_ATTESTATION_UNAVAILABLE);
    headers.set("x-session-id", realtimeSessionId);
    headers.set("session-id", sessionId);
    headers.set("thread-id", threadId);
    headers.set("user-agent", "codex_cli_rs/0.0.0");
  }
  headers.set("authorization", `Bearer ${credential.secret}`);
  if (credential.kind === "chatgpt") {
    if (!credential.accountId) throw new EgressFailure(503, "credential_field_unavailable");
    headers.set("chatgpt-account-id", credential.accountId);
    if (credential.fedramp) headers.set("x-openai-fedramp", "true");
    if (!operation.websocket && !realtime) headers.set("originator", "codex_cli_rs");
  }
  const target = upstreamUrl(env, operation, credential.kind);
  if (operation.id === "realtime-sideband") {
    const callId = original.headers.get("x-nanocodex-realtime-call-id");
    if (!validRealtimeCallId(callId)) throw new EgressFailure(400, "invalid_realtime_call");
    target.pathname += callId;
  }
  return new Request(target, {
    method: original.method,
    headers,
    body,
    cache: "no-store",
    redirect: "manual",
  });
}

function upstreamUrl(
  env: EgressEnv,
  operation: ModelOperation,
  kind: UserCredentialSnapshot["kind"],
): URL {
  if (kind === "openai") return new URL(operation.openai);
  const configured = env.CODEX_RELAY_URL?.trim();
  if (!configured || operation.directChatGpt) return new URL(operation.chatgpt);
  let relay: URL;
  try { relay = new URL(configured); } catch { throw new EgressFailure(503, "invalid_codex_relay_url"); }
  const publicRelay = relay.protocol === "https:" && !relay.port;
  const localRelay = env.ALLOW_INSECURE_LOOPBACK_RELAY === "true"
    && relay.protocol === "http:" && relay.hostname === "127.0.0.1" && Boolean(relay.port);
  const capabilityRelay = RELAY_CAPABILITY_PATH.test(relay.pathname);
  if ((!publicRelay && !localRelay) || relay.username || relay.password
    || (relay.pathname !== "/" && !capabilityRelay) || relay.search || relay.hash) {
    throw new EgressFailure(503, "invalid_codex_relay_url");
  }
  if (!capabilityRelay) {
    const target = new URL(operation.chatgpt);
    relay.pathname = target.pathname;
    relay.search = target.search;
  } else if (!operation.websocket) {
    const httpRoute = RELAY_HTTP_ROUTES[operation.id];
    if (!httpRoute) throw new EgressFailure(503, "invalid_codex_relay_url");
    relay.pathname = `${relay.pathname}/http/${httpRoute}`;
  }
  return relay;
}

async function fetchUpstream(
  env: EgressEnv,
  userId: string,
  credential: UserCredentialSnapshot,
  operation: ModelOperation,
  request: Request,
  upstreamFetch: typeof fetch,
): Promise<Response> {
  if (credential.kind !== "chatgpt" || env.CODEX_RELAY_URL || operation.directChatGpt) {
    return upstreamFetch(request);
  }
  if (env.CHATGPT_EGRESS) {
    const target = new URL(request.url);
    const internal = new URL(`${target.pathname}${target.search}`, "https://chatgpt-egress.internal");
    const id = env.CHATGPT_EGRESS.idFromName(`user-v1:${userId}`);
    return env.CHATGPT_EGRESS.get(id).fetch(new Request(internal, {
      method: request.method,
      headers: request.headers,
      body: request.body,
      redirect: "manual",
    }));
  }
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  if (environment === "production" || environment === "preview") {
    throw new EgressFailure(503, "chatgpt_relay_unavailable");
  }
  return upstreamFetch(request);
}

function validRealtimeCallId(value: string | null): value is string {
  return value !== null && (
    /^rtc_[A-Za-z0-9._:-]{1,196}$/.test(value)
    || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
  );
}

async function resolveSubject(env: EgressEnv, subject: string): Promise<string> {
  const response = await subjectDirectory(env, subject).fetch("https://subjects.internal/v1/resolve", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ subject }),
  });
  if (!response.ok) {
    await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
    throw new EgressFailure(response.status === 404 ? 403 : 503, "agent_subject_unavailable");
  }
  return subjectUser(response);
}

async function bindMcpConnectionOwner(
  env: EgressEnv,
  connectionId: string,
  userId: string,
): Promise<Response | undefined> {
  const response = await env.MCP_CONNECTIONS.getByName(connectionId).fetch(
    "https://mcp-directory.internal/v1/bind",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: connectionId, user_id: userId }),
    },
  );
  if (response.ok) {
    await cancelResponseBody(response);
    return undefined;
  }
  await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
  return response.status === 409
    ? jsonError(409, "mcp_connection_owner_mismatch")
    : jsonError(503, "mcp_connection_directory_unavailable");
}

async function resolveMcpConnectionOwner(
  env: EgressEnv,
  connectionId: string,
): Promise<string | undefined> {
  const response = await env.MCP_CONNECTIONS.getByName(connectionId).fetch(
    "https://mcp-directory.internal/v1/resolve",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: connectionId }),
    },
  );
  if (response.status === 404) {
    await cancelResponseBody(response);
    return undefined;
  }
  if (!response.ok) {
    await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
    throw new EgressFailure(503, "mcp_connection_directory_unavailable");
  }
  return subjectUser(response);
}

async function subjectUser(response: Response): Promise<string> {
  const value = await response.json<Record<string, unknown>>();
  const userId = stringField(value, "user_id");
  if (!USER_ID.test(userId ?? "")) throw new EgressFailure(503, "invalid_subject_response");
  return userId!;
}

async function resolveCredential(
  env: EgressEnv,
  userId: string,
  recover: boolean,
  revision?: number,
): Promise<UserCredentialSnapshot> {
  const response = await userBroker(env, userId).fetch("https://credentials.internal/v1/credential", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ recover, ...(revision === undefined ? {} : { revision }) }),
  });
  if (!response.ok) {
    await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
    throw new EgressFailure(response.status === 404 ? 409 : 503, "user_credential_unavailable");
  }
  const value = await response.json<UserCredentialSnapshot>();
  if ((value.kind !== "openai" && value.kind !== "chatgpt") || !value.secret
    || !Number.isSafeInteger(value.revision)) {
    throw new EgressFailure(503, "invalid_credential_response");
  }
  return value;
}

async function resolveSshIdentity(
  env: EgressEnv,
  userId: string,
  reference: string,
): Promise<BrokeredSshIdentity> {
  const response = await userBroker(env, userId).fetch(
    `https://credentials.internal/v1/ssh-identities/${encodeURIComponent(reference)}`,
    { method: "POST" },
  );
  if (!response.ok) {
    await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
    throw new EgressFailure(
      response.status === 404 ? 409 : 503,
      response.status === 404 ? "ssh_identity_unavailable" : "ssh_identity_broker_unavailable",
    );
  }
  const identity = validateSshIdentity(await response.json<unknown>());
  if (!identity) throw new EgressFailure(503, "invalid_ssh_identity_response");
  return identity;
}

async function replayableBody(request: Request, operation: ModelOperation): Promise<Uint8Array | null> {
  if (operation.websocket) return null;
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const size = Number(declared);
    if (!/^(?:0|[1-9][0-9]*)$/.test(declared) || !Number.isSafeInteger(size)) {
      throw new EgressFailure(400, "invalid_content_length");
    }
    if (size > MAX_MODEL_BODY_BYTES) throw new EgressFailure(413, "request_body_too_large");
  }
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > MAX_MODEL_BODY_BYTES) {
        await reader.cancel();
        throw new EgressFailure(413, "request_body_too_large");
      }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
  return body;
}

function subjectDirectory(
  env: EgressEnv,
  subject: string,
): DurableObjectStub<AgentSubjectDirectory> {
  return env.AGENT_SUBJECTS.getByName(`${SUBJECT_DIRECTORY_PREFIX}${subject}`);
}
function userBroker(env: EgressEnv, userId: string): DurableObjectStub<UserCredentialBroker> {
  return env.USER_CREDENTIALS.getByName(userId);
}
function connectorBroker(env: EgressEnv, userId: string): DurableObjectStub<UserConnectorBroker> {
  return env.USER_CONNECTORS.getByName(userId);
}
async function cancelResponseBody(response: Response): Promise<void> {
  try { await response.body?.cancel(); } catch { /* Response disposal is best-effort. */ }
}
function localClaimEnabled(env: EgressEnv): boolean {
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  return env.ALLOW_LOCAL_CREDENTIAL_CLAIM === "true"
    && (environment === "development" || environment === "local" || environment === "test");
}
async function readJson(request: Request, limit: number): Promise<Record<string, unknown> | undefined> {
  try {
    const value: unknown = JSON.parse(await readBoundedText(request, limit));
    return isRecord(value) ? value : undefined;
  } catch { return undefined; }
}
async function readBoundedText(message: Request | Response, limit: number): Promise<string> {
  if (!message.body) return "";
  const reader = message.body.getReader();
  const decoder = new TextDecoder();
  let bytes = 0;
  let text = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return text + decoder.decode();
      bytes += value.byteLength;
      if (bytes > limit) { await reader.cancel(); throw new EgressFailure(413, "body_too_large"); }
      text += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}
function stringField(value: unknown, key: string): string | undefined {
  return isRecord(value) && typeof value[key] === "string" && value[key].trim()
    ? value[key] as string : undefined;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function json(body: unknown, status: number): Response {
  return Response.json(body, { status, headers: { "cache-control": "no-store", pragma: "no-cache" } });
}
function jsonError(status: number, error: string): Response { return json({ error }, status); }

class EgressFailure extends Error {
  constructor(readonly status: number, readonly code: string) { super(code); }
}
function egressFailure(error: unknown): EgressFailure {
  return error instanceof EgressFailure ? error : new EgressFailure(502, "upstream_failed");
}
function auditedError(
  status: number,
  code: string,
  request: Request,
  url: URL,
  rule: string | undefined,
  started: number,
  detail: Record<string, unknown> = {},
): Response {
  audit(status >= 500 ? "error" : "deny", request, url, rule, started, {
    ...detail,
    code,
    status,
  });
  return jsonError(status, code);
}

function auditControl(
  request: Request,
  url: URL,
  status: number,
  started: number,
  deploymentSha: string | undefined,
): void {
  const user = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/(mcp-connections|connectors|credentials)(?:\/(.*))?$/,
  );
  const subject = url.pathname.startsWith("/subjects/");
  const tail = user?.[3];
  const connector = user?.[2] === "connectors"
    ? tail?.match(/^(github|gmail|gdrive|x)/)?.[1]
    : undefined;
  const mcpConnectionId = user?.[2] === "mcp-connections"
    ? tail?.match(/^([A-Za-z0-9_-]{43})/)?.[1]
    : undefined;
  console.log({
    type: "egress.control",
    action: status >= 500 ? "error" : status >= 400 ? "deny" : "allow",
    method: request.method,
    operation: subject ? "subject" : user?.[2] ?? "unknown",
    status,
    duration_ms: Date.now() - started,
    ...(deploymentSha === undefined ? {} : { deployment_sha: deploymentSha }),
    ...(user === null ? {} : { user_id: user[1] }),
    ...(connector === undefined ? {} : { connector }),
    ...(mcpConnectionId === undefined ? {} : { mcp_connection_id: mcpConnectionId }),
  });
}

function audit(
  action: "allow" | "deny" | "error",
  request: Request,
  url: URL,
  rule: string | undefined,
  started: number,
  detail: Record<string, unknown>,
): void {
  const connector = rule === "github" || rule === "gmail" || rule === "gdrive"
    || rule === "x" || rule === "mcp";
  console.log({
    type: "egress.request",
    action,
    rule,
    method: request.method,
    host: url.host,
    path: connector ? "/provider-api" : url.pathname,
    duration_ms: Date.now() - started,
    ...detail,
  });
}
