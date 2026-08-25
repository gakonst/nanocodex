import {
  AgentSubjectDirectory,
  type BrokerEnv,
  UserCredentialBroker,
  type UserCredentialSnapshot,
} from "./broker";
import {
  UserConnectorBroker,
  type ConnectorBrokerEnv,
} from "./connector-broker";
import { canonicalConnectorPath } from "./connector-path";

export { AgentSubjectDirectory, UserCredentialBroker } from "./broker";
export { UserConnectorBroker } from "./connector-broker";

const SUBJECT_DIRECTORY_PREFIX = "agent-subject-v1:";
const READINESS_SUBJECT_DIRECTORY_NAME = "agent-subject-readiness-v1";
const SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const USER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const SUBJECT_HEADER = "x-nanocodex-subject";
const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const MODEL_STATUS_PATH = "/.well-known/nanocodex/model-status";
const BROKER_READINESS_PATH = "/.well-known/nanocodex/broker-readiness";
const MAX_CONTROL_BODY_BYTES = 16 * 1024;
const MAX_BROKER_RESPONSE_BYTES = 4 * 1024;
const MAX_MODEL_BODY_BYTES = 32 * 1024 * 1024;
const CODEX_ATTESTATION_UNAVAILABLE = '{"v":1,"s":1}';
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);

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
  CHATGPT_EGRESS?: DurableObjectNamespace;
  CODEX_RELAY_URL?: string;
  ALLOW_INSECURE_LOOPBACK_RELAY?: string;
  NANOCODEX_BROKER_PROBE_TOKEN?: string;
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

  const connector = connectorOperation(url);
  if (connector) return handleConnectorEgress(request, url, connector, env, started);
  if (url.search) return jsonError(403, "destination_denied");

  if (url.pathname.startsWith("/subjects/") || url.pathname.startsWith("/users/")) {
    return handleControl(request, url, env);
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

  try {
    const userId = await resolveSubject(env, subject);
    let credential = await resolveCredential(env, userId, false);
    if (operation.chatGptOnly && credential.kind !== "chatgpt") {
      return auditedError(409, "chatgpt_credential_required", request, url, operation.id, started);
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
        return auditedError(409, "chatgpt_credential_required", request, url, operation.id, started);
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
      return auditedError(502, "upstream_redirect_blocked", request, url, operation.id, started);
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
        { upstream_status: upstreamStatus },
      );
    }
    audit("allow", request, url, operation.id, started, { status: upstream.status, recovered });
    return sanitizeUpstreamResponse(upstream);
  } catch (error) {
    const problem = egressFailure(error);
    if (!(error instanceof EgressFailure)) {
      const detail = { name: error instanceof Error ? error.name : typeof error };
      diagnostics?.upstreamException(detail);
      console.error(JSON.stringify({ type: "egress.upstream_exception", ...detail }));
    }
    return auditedError(problem.status, problem.code, request, url, operation.id, started);
  }
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
  try {
    const userId = await resolveSubject(env, subject);
    const response = await connectorBroker(env, userId).fetch(request);
    audit(response.status >= 500 ? "error" : response.status >= 400 ? "deny" : "allow",
      request, url, connector.id, started, { status: response.status });
    return sanitizeUpstreamResponse(response);
  } catch (error) {
    const problem = egressFailure(error);
    return auditedError(problem.status, problem.code, request, url, connector.id, started);
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

  const connectorMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/connectors(?:\/(github|gmail|gdrive|x)(\/callback)?)?$/,
  );
  if (connectorMatch) {
    const userId = connectorMatch[1]!;
    const connector = connectorMatch[2];
    const callback = connectorMatch[3] === "/callback";
    const target = connector
      ? `https://connectors.internal/v1/${connector}${callback ? "/callback" : request.method === "POST" ? "/start" : ""}`
      : "https://connectors.internal/v1/status";
    if ((!connector && request.method !== "GET")
      || (connector && callback && request.method !== "POST")
      || (connector && !callback && request.method !== "POST" && request.method !== "DELETE")) {
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

  const userMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/credentials(?:\/(openai|chatgpt|chatgpt\/login|chatgpt\/login\/status|chatgpt\/local-claim))?$/,
  );
  if (!userMatch) return jsonError(404, "not_found");
  const userId = userMatch[1]!;
  const operation = userMatch[2];

  if (operation === "chatgpt/local-claim") {
    if (request.method !== "POST") return jsonError(405, "method_not_allowed");
    if (!localClaimEnabled(env)) return jsonError(404, "not_found");
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
    const { done } = await reader.read();
    return !done;
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
  if ((!publicRelay && !localRelay) || relay.username || relay.password || relay.pathname !== "/"
    || relay.search || relay.hash) {
    throw new EgressFailure(503, "invalid_codex_relay_url");
  }
  const target = new URL(operation.chatgpt);
  relay.pathname = target.pathname;
  relay.search = target.search;
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
function audit(
  action: "allow" | "deny" | "error",
  request: Request,
  url: URL,
  rule: string | undefined,
  started: number,
  detail: Record<string, unknown>,
): void {
  const connector = rule === "github" || rule === "gmail" || rule === "gdrive";
  console.log(JSON.stringify({
    type: "egress.request",
    action,
    rule,
    method: request.method,
    host: url.host,
    path: connector ? "/provider-api" : url.pathname,
    duration_ms: Date.now() - started,
    ...detail,
  }));
}
