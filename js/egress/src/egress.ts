import { WorkerEntrypoint } from "cloudflare:workers";
import { HISTORY_NOTES_PATHS, historyNotesEligible, historyNotesHeaders } from "./history-notes";
import {
  AgentSubjectDirectory,
  type BrokerEnv,
  UserCredentialBroker,
  type UserCredentialSnapshot,
  type VaultEntry,
  type VaultKind,
  validChatGptCredentialImport,
  validateMaterializedVaultEntry,
  validateVaultEntryPayload,
} from "./broker";
import {
  BROWSER_COOKIE_JAR_ID,
  MAX_BROWSER_COOKIE_JAR_BODY_BYTES,
  validateBrowserCookieJarBinding,
  validateBrowserCookieJarDelete,
  validateBrowserCookieJarUpsert,
} from "./browser-cookie-jar";
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
const EPHEMERAL_BROWSER_MODEL_SUBJECT = /^[A-Za-z0-9_-]{43}$/;
const SPONSORED_PROMPT_ID = /^[A-Za-z][A-Za-z0-9_-]{1,127}$/;
const SPONSORED_RESPONSE_ID = /^[A-Za-z][A-Za-z0-9._:-]{1,199}$/;
const SPONSORED_CALL_ID = /^[A-Za-z][A-Za-z0-9._:-]{1,199}$/;
const MAX_SPONSORED_CALL_IDS = 64;
const MAX_SPONSORED_RESPONSES_FRAME_CHARS = 16 * 1024 * 1024;
const USER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const CHIEF_USER_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SUBJECT_HEADER = "x-nanocodex-subject";
const CREDENTIAL_PROVENANCE_HEADER = "x-nanocodex-credential-provenance";
const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const MODEL_STATUS_PATH = "/.well-known/nanocodex/model-status";
const CONTEXT_CAPABILITY_PATH = "/.well-known/nanocodex/context-management";
const SPONSORED_TRIAL_RESET_PATH = "/.well-known/nanocodex/sponsored-trial-reset";
const BROKER_READINESS_PATH = "/.well-known/nanocodex/broker-readiness";
const MAX_CONTROL_BODY_BYTES = 16 * 1024;
const MAX_CHATGPT_IMPORT_BODY_BYTES = 64 * 1024;
const MAX_VAULT_BODY_BYTES = 12 * 1024;
const MAX_BROKER_RESPONSE_BYTES = 4 * 1024;
const MAX_MODEL_BODY_BYTES = 32 * 1024 * 1024;
const MAX_SSH_BODY_BYTES = 72 * 1024;
const MAX_VAULT_EGRESS_ENVELOPE_BYTES = 96 * 1024;
const MAX_VAULT_EGRESS_TARGET_BYTES = 8 * 1024;
const MAX_VAULT_EGRESS_HEADERS = 64;
const MAX_VAULT_EGRESS_HEADER_NAME_BYTES = 128;
const MAX_VAULT_EGRESS_HEADER_VALUE_BYTES = 4 * 1024;
const MAX_VAULT_EGRESS_HEADER_BYTES = 32 * 1024;
const MAX_VAULT_EGRESS_REQUEST_BODY_BYTES = 64 * 1024;
const CODEX_ATTESTATION_UNAVAILABLE = '{"v":1,"s":1}';
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const VAULT_EGRESS_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const VAULT_ENTRY_ID = /^[A-Za-z0-9_-]{22,64}$/;
const VAULT_PLACEHOLDER = /\{\{NANOCODEX_VAULT_([A-Z_]+)\}\}/g;
const VAULT_PLACEHOLDER_MARKER = "NANOCODEX_VAULT_";
const VAULT_PRIVATE_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i;
const VAULT_FORBIDDEN_HEADERS = new Set([
  "connection", "content-length", "cookie", "expect", "host", "origin", "proxy-authorization", "proxy-connection", "referer",
  "te", "trailer", "transfer-encoding", "upgrade", "via",
]);
const PRIVATE_HOST_SUFFIXES = [
  ".internal", ".invalid", ".local", ".localhost", ".test", ".home.arpa",
];
const VAULT_PROVIDER_HOSTS = new Set([
  "api.github.com", "api.openai.com", "api.x.com", "chatgpt.com",
  "calendar.googleapis.com", "docs.googleapis.com", "gmail.googleapis.com",
  "people.googleapis.com", "sheets.googleapis.com", "slack.com",
  "slides.googleapis.com", "tasks.googleapis.com", "www.googleapis.com",
]);
const RELAY_CAPABILITY_PATH = /^\/v1\/[A-Za-z0-9_-]{43,}$/;
const RELAY_HTTP_ROUTES: Readonly<Record<ModelOperation["id"], string | undefined>> = {
  responses: undefined,
  search: "codex-web-search",
  "image-generation": "codex-image-generation",
  "image-edit": "codex-image-edit",
  "realtime-call": undefined,
  "realtime-sideband": undefined,
  "history-notes": undefined,
};

type ConnectorOperation = Readonly<{
  id: "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks" | "gdocs"
    | "gsheets" | "gslides" | "gcontacts" | "slack" | "x";
  origin: `https://${string}`;
  paths: readonly RegExp[];
}>;

type VaultPlaceholder = "API_KEY" | "USERNAME" | "PASSWORD" | "BASIC" | "CARD_NUMBER"
  | "EXPIRY_MONTH" | "EXPIRY_YEAR" | "CVV" | "BILLING_ZIP";

type VaultEgressEnvelope = Readonly<{
  vaultId: string;
  url: URL;
  method: string;
  headers: ReadonlyMap<string, string>;
  body?: string;
  placeholders: ReadonlySet<VaultPlaceholder>;
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
    id: "gcalendar",
    origin: "https://www.googleapis.com",
    paths: [/^\/calendar\/v3(?:\/|$)/],
  },
  {
    id: "gcalendar",
    origin: "https://calendar.googleapis.com",
    paths: [/^\/calendar\/v3(?:\/|$)/],
  },
  {
    id: "gtasks",
    origin: "https://tasks.googleapis.com",
    paths: [/^\/tasks\/v1(?:\/|$)/],
  },
  {
    id: "gdocs",
    origin: "https://docs.googleapis.com",
    paths: [/^\/v1\/documents(?:\/|$)/],
  },
  {
    id: "gsheets",
    origin: "https://sheets.googleapis.com",
    paths: [/^\/v4\/spreadsheets(?:\/|$)/],
  },
  {
    id: "gslides",
    origin: "https://slides.googleapis.com",
    paths: [/^\/v1\/presentations(?:\/|$)/],
  },
  {
    id: "gcontacts",
    origin: "https://people.googleapis.com",
    paths: [/^\/v1\/(?:people|contactGroups|otherContacts)(?:\/|:|$)/],
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
  {
    id: "slack",
    origin: "https://slack.com",
    paths: [/^\/api\/(?!auth\.revoke$)[A-Za-z0-9._-]+$/],
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

export class ChiefOfStaffEgress extends WorkerEntrypoint<EgressEnv> {
  async ensureCredential(userIdValue: unknown): Promise<void> {
    if (typeof userIdValue !== "string" || !CHIEF_USER_ID.test(userIdValue)) {
      throw new Error("invalid_chief_user");
    }
    const response = await userBroker(this.env, userIdValue).fetch(
      "https://credentials.internal/v1/chief-of-staff/openai-key",
      { method: "PUT" },
    );
    if (!response.ok) {
      await response.body?.cancel();
      throw new Error("chief_credential_unavailable");
    }
    await response.body?.cancel();
  }
}

type ModelOperation = Readonly<{
  id: "responses" | "search" | "image-generation" | "image-edit"
    | "realtime-call" | "realtime-sideband" | "history-notes";
  method: "GET" | "POST";
  path: `/v1/${string}`;
  websocket: boolean;
  openai: `https://${string}`;
  chatgpt: `https://${string}`;
  chatGptOnly?: true;
  directChatGpt?: true;
}>;

const OPERATIONS: readonly ModelOperation[] = [
  ...HISTORY_NOTES_PATHS.map((path): ModelOperation => ({
    id: "history-notes", method: "POST", path: `/v1/${path}`, websocket: false,
    openai: `https://api.openai.com/v1/${path}`,
    chatgpt: `https://chatgpt.com/backend-api/codex/${path}`,
    chatGptOnly: true,
  })),
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
  ctx?: Pick<ExecutionContext, "waitUntil">,
  upstreamFetch: typeof fetch = fetch,
  diagnostics?: Readonly<{ upstreamException(error: Readonly<{ name: string }>): void }>,
): Promise<Response> {
  const started = Date.now();
  let url: URL;
  try { url = new URL(request.url); } catch { return jsonError(400, "invalid_url"); }
  if (url.username || url.password || url.hash) return jsonError(403, "destination_denied");

  if (url.protocol === "https:" && url.hostname === "vault-egress.internal" && !url.port
    && url.pathname === "/v1/request" && !url.search) {
    return handleVaultEgress(request, url, env, started, upstreamFetch);
  }

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
  if (url.pathname === CONTEXT_CAPABILITY_PATH && url.origin === "https://nanocodex.internal") {
    return handleContextCapability(request, env);
  }
  if (url.pathname === SPONSORED_TRIAL_RESET_PATH) {
    return handleSponsoredTrialReset(request, env);
  }

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
    const sponsoredDemo = EPHEMERAL_BROWSER_MODEL_SUBJECT.test(subject)
      && operation.id === "responses";
    let credential = await resolveCredential(env, userId, false, undefined, sponsoredDemo);
    if (operation.id === "history-notes" && !historyNotesEligible(credential)) {
      return auditedError(409, "experimental_context_unavailable", request, url, operation.id, started);
    }
    if (operation.chatGptOnly && credential.kind !== "chatgpt") {
      return auditedError(409, "chatgpt_credential_required", request, url, operation.id, started, {
        user_id: userId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
    }
    let sponsoredConnectionId = credential.source === "sponsored" && operation.id === "responses"
      ? await acquireSponsoredConnection(env, userId)
      : undefined;
    try {
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
        credential = await resolveCredential(
          env,
          userId,
          true,
          credential.revision,
          sponsoredDemo,
        );
        if (operation.id === "history-notes" && !historyNotesEligible(credential)) {
          return auditedError(409, "experimental_context_unavailable", request, url, operation.id, started);
        }
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
        model_source: credential.source,
        user_id: userId,
        deployment_sha: env.DEPLOYMENT_SHA,
      });
      if (credential.source === "sponsored" && operation.id === "responses") {
        if (!sponsoredConnectionId) {
          throw new EgressFailure(503, "sponsored_connection_lease_unavailable");
        }
        const response = sponsoredResponsesWebSocket(
          upstream,
          env,
          userId,
          sponsoredConnectionId,
          ctx,
        );
        sponsoredConnectionId = undefined;
        return response;
      }
      return sanitizeUpstreamResponse(upstream);
    } finally {
      if (sponsoredConnectionId) {
        await updateSponsoredConnection(env, userId, sponsoredConnectionId, "release")
          .catch(logSponsoredConnectionReleaseFailure);
      }
    }
  } catch (error) {
    const problem = egressFailure(error);
    if (!(error instanceof EgressFailure)) {
      const detail = { name: error instanceof Error ? error.name : typeof error };
      diagnostics?.upstreamException(detail);
      console.error({ type: "egress.upstream_exception", error_kind: detail.name });
    }
    return auditedError(problem.status, problem.code, request, url, operation.id, started,
      {
        ...(userId === undefined ? {} : { user_id: userId }),
        deployment_sha: env.DEPLOYMENT_SHA,
      });
  }
}

async function handleVaultEgress(
  request: Request,
  url: URL,
  env: EgressEnv,
  started: number,
  upstreamFetch: typeof fetch,
): Promise<Response> {
  if (request.method !== "POST") {
    return auditedError(403, "method_denied", request, url, "vault", started);
  }
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)) {
    return auditedError(403, "agent_subject_required", request, url, "vault", started);
  }
  if (!isJsonContentType(request.headers.get("content-type"))
    || request.headers.has("authorization") || request.headers.has("cookie")
    || request.headers.has("proxy-authorization")) {
    return auditedError(403, "required_header_mismatch", request, url, "vault", started);
  }

  let envelope: VaultEgressEnvelope;
  try {
    const value: unknown = JSON.parse(
      await readBoundedText(request, MAX_VAULT_EGRESS_ENVELOPE_BYTES),
    );
    envelope = validateVaultEgressEnvelope(value);
  } catch (error) {
    const problem = error instanceof EgressFailure
      ? error
      : new EgressFailure(400, "invalid_vault_request");
    return auditedError(problem.status, problem.code, request, url, "vault", started);
  }

  try {
    const userId = await resolveSubject(env, subject);
    const entry = await resolveVaultEntry(env, userId, envelope.vaultId);
    const replacements = vaultReplacements(entry, envelope.placeholders);
    const headers = new Headers();
    let injectedHeaderBytes = 0;
    for (const [name, template] of envelope.headers) {
      const value = substituteVaultTemplate(template, replacements);
      injectedHeaderBytes += new TextEncoder().encode(name).byteLength
        + new TextEncoder().encode(value).byteLength;
      if (new TextEncoder().encode(value).byteLength > 16 * 1024
        || injectedHeaderBytes > 64 * 1024) {
        throw new EgressFailure(413, "vault_request_too_large");
      }
      headers.set(name, value);
    }
    const body = envelope.body === undefined
      ? undefined
      : substituteVaultTemplate(envelope.body, replacements);
    if (body !== undefined
      && new TextEncoder().encode(body).byteLength > MAX_VAULT_EGRESS_REQUEST_BODY_BYTES) {
      throw new EgressFailure(413, "vault_request_too_large");
    }
    const upstream = await upstreamFetch(new Request(envelope.url, {
      method: envelope.method,
      headers,
      ...(body === undefined ? {} : { body }),
      redirect: "manual",
      signal: request.signal,
    }));
    const status = upstream.status;
    const ok = upstream.ok;
    await cancelResponseBody(upstream);
    audit("allow", request, url, "vault", started, {
      status,
      deployment_sha: env.DEPLOYMENT_SHA,
    });
    return json({ status, ok }, 200);
  } catch (error) {
    const problem = egressFailure(error);
    return auditedError(problem.status, problem.code, request, url, "vault", started, {
      deployment_sha: env.DEPLOYMENT_SHA,
    });
  }
}

function validateVaultEgressEnvelope(value: unknown): VaultEgressEnvelope {
  if (!isRecord(value)) throw new EgressFailure(400, "invalid_vault_request");
  const hasBody = Object.prototype.hasOwnProperty.call(value, "body");
  const expected = ["vault_id", "url", "method", "headers", ...(hasBody ? ["body"] : [])];
  const keys = Object.keys(value);
  if (keys.length !== expected.length || keys.some((key) => !expected.includes(key))
    || typeof value.vault_id !== "string" || !VAULT_ENTRY_ID.test(value.vault_id)
    || typeof value.url !== "string"
    || new TextEncoder().encode(value.url).byteLength > MAX_VAULT_EGRESS_TARGET_BYTES
    || typeof value.method !== "string" || !VAULT_EGRESS_METHODS.has(value.method)
    || !isRecord(value.headers)
    || (hasBody && typeof value.body !== "string")) {
    throw new EgressFailure(400, "invalid_vault_request");
  }
  if ((value.method === "GET" || value.method === "HEAD") && hasBody) {
    throw new EgressFailure(400, "invalid_vault_request");
  }
  let target: URL;
  try { target = vaultEgressTarget(new URL(value.url)); }
  catch { throw new EgressFailure(403, "vault_destination_denied"); }

  const entries = Object.entries(value.headers);
  if (entries.length > MAX_VAULT_EGRESS_HEADERS) {
    throw new EgressFailure(413, "vault_request_too_large");
  }
  const headers = new Map<string, string>();
  const seen = new Set<string>();
  const placeholders = new Set<VaultPlaceholder>();
  let headerBytes = 0;
  for (const [name, headerValue] of entries) {
    if (typeof headerValue !== "string") {
      throw new EgressFailure(400, "invalid_vault_request");
    }
    const lower = name.toLowerCase();
    const nameBytes = new TextEncoder().encode(name).byteLength;
    const valueBytes = new TextEncoder().encode(headerValue).byteLength;
    headerBytes += nameBytes + valueBytes;
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)
      || nameBytes > MAX_VAULT_EGRESS_HEADER_NAME_BYTES
      || valueBytes > MAX_VAULT_EGRESS_HEADER_VALUE_BYTES
      || /[\0\r\n]/.test(headerValue)
      || headerBytes > MAX_VAULT_EGRESS_HEADER_BYTES || seen.has(lower)
      || VAULT_FORBIDDEN_HEADERS.has(lower) || lower.startsWith("cf-")
      || lower.startsWith("forwarded") || lower.startsWith("sec-")
      || lower.startsWith("x-forwarded-") || lower.startsWith("x-nanocodex-")) {
      throw new EgressFailure(403, "vault_header_denied");
    }
    const found = vaultTemplatePlaceholders(headerValue);
    if (VAULT_PRIVATE_HEADER.test(name) && !validVaultPrivateHeader(lower, headerValue)) {
      throw new EgressFailure(403, "vault_raw_credential_denied");
    }
    for (const placeholder of found) placeholders.add(placeholder);
    seen.add(lower);
    headers.set(name, headerValue);
  }
  let body: string | undefined;
  if (hasBody) {
    body = value.body as string;
    if (new TextEncoder().encode(body).byteLength > MAX_VAULT_EGRESS_REQUEST_BODY_BYTES) {
      throw new EgressFailure(413, "vault_request_too_large");
    }
    for (const placeholder of vaultTemplatePlaceholders(body)) placeholders.add(placeholder);
  }
  if (![...placeholders].some((placeholder) => placeholder !== "USERNAME")) {
    throw new EgressFailure(400, "vault_secret_placeholder_required");
  }
  return {
    vaultId: value.vault_id,
    url: target,
    method: value.method,
    headers,
    ...(body === undefined ? {} : { body }),
    placeholders,
  };
}

function validVaultPrivateHeader(name: string, value: string): boolean {
  if (name === "authorization") {
    return value === "Basic {{NANOCODEX_VAULT_BASIC}}"
      || value === "Bearer {{NANOCODEX_VAULT_API_KEY}}"
      || value === "Bearer {{NANOCODEX_VAULT_PASSWORD}}";
  }
  return /^\{\{NANOCODEX_VAULT_(?:PASSWORD|API_KEY|BASIC|CARD_NUMBER|EXPIRY_MONTH|EXPIRY_YEAR|CVV|BILLING_ZIP)\}\}$/.test(value);
}

function vaultTemplatePlaceholders(template: string): Set<VaultPlaceholder> {
  const placeholders = new Set<VaultPlaceholder>();
  const supported = new Set<VaultPlaceholder>([
    "API_KEY", "USERNAME", "PASSWORD", "BASIC", "CARD_NUMBER", "EXPIRY_MONTH", "EXPIRY_YEAR",
    "CVV", "BILLING_ZIP",
  ]);
  for (const match of template.matchAll(VAULT_PLACEHOLDER)) {
    if (!supported.has(match[1] as VaultPlaceholder)) {
      throw new EgressFailure(400, "invalid_vault_placeholder");
    }
    placeholders.add(match[1] as VaultPlaceholder);
  }
  if (template.replace(VAULT_PLACEHOLDER, "").includes(VAULT_PLACEHOLDER_MARKER)) {
    throw new EgressFailure(400, "invalid_vault_placeholder");
  }
  return placeholders;
}

function vaultEgressTarget(url: URL): URL {
  if ((url.protocol !== "http:" && url.protocol !== "https:") || url.username || url.password
    || url.hash) throw new Error("invalid target");
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  if (!hostname || hostname === "localhost" || PRIVATE_HOST_SUFFIXES.some((suffix) => (
    hostname === suffix.slice(1) || hostname.endsWith(suffix)
  )) || deniedVaultIpLiteral(hostname) || VAULT_PROVIDER_HOSTS.has(hostname)) {
    throw new Error("denied target");
  }
  return url;
}

function deniedVaultIpLiteral(hostname: string): boolean {
  const ipv4 = hostname.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (ipv4) {
    const octets = ipv4.slice(1).map(Number);
    if (octets.some((octet) => octet > 255)) return true;
    const [a, b] = octets;
    return a === 0 || a === 10 || a === 127 || a! >= 224
      || (a === 100 && b! >= 64 && b! <= 127)
      || (a === 169 && b === 254) || (a === 172 && b! >= 16 && b! <= 31)
      || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19));
  }
  if (!hostname.includes(":")) return false;
  const normalized = hostname.replace(/^\[/, "").replace(/\]$/, "").toLowerCase();
  return normalized === "::" || normalized === "::1" || normalized.startsWith("fc")
    || normalized.startsWith("fd") || normalized.startsWith("fe") || normalized.startsWith("ff")
    || normalized.startsWith("::ffff:");
}

async function resolveVaultEntry(
  env: EgressEnv,
  userId: string,
  vaultId: string,
): Promise<VaultEntry> {
  const response = await userBroker(env, userId).fetch(
    `https://credentials.internal/v1/vault-entry/${vaultId}`,
    { method: "POST" },
  );
  if (!response.ok) {
    await readBoundedText(response, MAX_BROKER_RESPONSE_BYTES);
    throw new EgressFailure(
      response.status === 404 ? 409 : 503,
      response.status === 404 ? "vault_entry_unavailable" : "vault_broker_unavailable",
    );
  }
  let value: unknown;
  try { value = JSON.parse(await readBoundedText(response, MAX_VAULT_BODY_BYTES)); }
  catch { throw new EgressFailure(503, "invalid_vault_entry_response"); }
  const entry = validateMaterializedVaultEntry(vaultId, value);
  if (!entry) throw new EgressFailure(503, "invalid_vault_entry_response");
  return entry;
}

function vaultReplacements(
  entry: VaultEntry,
  requested: ReadonlySet<VaultPlaceholder>,
): ReadonlyMap<VaultPlaceholder, string> {
  let replacements: Map<VaultPlaceholder, string>;
  if (entry.kind === "api_key") {
    replacements = new Map([["API_KEY", entry.api_key]]);
  } else if (entry.kind === "login") {
    replacements = new Map([
      ["USERNAME", entry.username],
      ["PASSWORD", entry.password],
      ["BASIC", base64Utf8(`${entry.username}:${entry.password}`)],
    ]);
  } else if (entry.kind === "card") {
    replacements = new Map([
      ["CARD_NUMBER", entry.card_number],
      ["EXPIRY_MONTH", entry.expiry_month],
      ["EXPIRY_YEAR", entry.expiry_year],
      ["CVV", entry.cvv],
      ["BILLING_ZIP", entry.billing_zip],
    ]);
  } else {
    throw new EgressFailure(403, "vault_entry_kind_mismatch");
  }
  if ([...requested].some((placeholder) => !replacements.has(placeholder))) {
    throw new EgressFailure(403, "vault_entry_kind_mismatch");
  }
  return replacements;
}

function substituteVaultTemplate(
  template: string,
  replacements: ReadonlyMap<VaultPlaceholder, string>,
): string {
  return template.replace(VAULT_PLACEHOLDER, (_match, name: string) => (
    replacements.get(name as VaultPlaceholder) ?? ""
  ));
}

function base64Utf8(value: string): string {
  let binary = "";
  for (const byte of new TextEncoder().encode(value)) binary += String.fromCharCode(byte);
  return btoa(binary);
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
  const headers = sanitizedUpstreamHeaders(upstream.headers);
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers,
  });
}

function sanitizedUpstreamHeaders(source: Headers): Headers {
  const headers = new Headers(source);
  for (const name of [
    "authorization",
    "chatgpt-account-id",
    "proxy-authenticate",
    "proxy-authorization",
    "set-cookie",
    "x-openai-fedramp",
  ]) headers.delete(name);
  return headers;
}

type SponsoredPromptStatus = Readonly<{
  limit: number;
  used: number;
  remaining: number;
}>;

async function acquireSponsoredConnection(env: EgressEnv, userId: string): Promise<string> {
  const connectionId = crypto.randomUUID();
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-connections",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action: "acquire", connection_id: connectionId }),
    },
  );
  if (response.status === 402) {
    await cancelResponseBody(response);
    throw new EgressFailure(402, "sponsored_prompt_limit_reached");
  }
  if (response.status === 429) {
    await cancelResponseBody(response);
    throw new EgressFailure(429, "sponsored_connection_limit_reached");
  }
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(503, "sponsored_connection_lease_unavailable");
  }
  let value: unknown;
  try { value = JSON.parse(await readBoundedText(response, 1_024)); }
  catch { throw new EgressFailure(503, "invalid_sponsored_connection_lease"); }
  if (!isRecord(value) || value.acquired !== true) {
    throw new EgressFailure(503, "invalid_sponsored_connection_lease");
  }
  return connectionId;
}

async function updateSponsoredConnection(
  env: EgressEnv,
  userId: string,
  connectionId: string,
  action: "heartbeat" | "release",
): Promise<boolean> {
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-connections",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action, connection_id: connectionId }),
    },
  );
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(503, "sponsored_connection_lease_unavailable");
  }
  let value: unknown;
  try { value = JSON.parse(await readBoundedText(response, 1_024)); }
  catch { throw new EgressFailure(503, "invalid_sponsored_connection_lease"); }
  if (!isRecord(value) || typeof value.updated !== "boolean") {
    throw new EgressFailure(503, "invalid_sponsored_connection_lease");
  }
  return value.updated;
}

function logSponsoredConnectionReleaseFailure(error: unknown): void {
  console.error({
    type: "sponsored_connection.release_failed",
    error_kind: error instanceof Error ? error.name : typeof error,
  });
}

async function sponsoredPromptStatus(
  env: EgressEnv,
  userId: string,
): Promise<SponsoredPromptStatus> {
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-prompts",
  );
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(503, "sponsored_prompt_status_unavailable");
  }
  return parseSponsoredPromptStatus(response);
}

async function reserveSponsoredPrompt(
  env: EgressEnv,
  userId: string,
  promptId: string,
  promptHash: string,
  cancelled: () => boolean = () => false,
): Promise<{
  allowed: boolean;
  attempt?: number;
  dispatch: boolean;
  status: SponsoredPromptStatus;
}> {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    if (cancelled()) throw new EgressFailure(409, "sponsored_prompt_socket_closed");
    const response = await userBroker(env, userId).fetch(
      "https://credentials.internal/v1/sponsored-prompts",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: promptId, prompt_hash: promptHash }),
      },
    );
    if (response.status !== 200 && response.status !== 402) {
      await cancelResponseBody(response);
      throw new EgressFailure(503, "sponsored_prompt_reservation_unavailable");
    }
    let value: unknown;
    try { value = JSON.parse(await readBoundedText(response, 1_024)); }
    catch { throw new EgressFailure(503, "invalid_sponsored_prompt_status"); }
    const pending = response.status === 200 && isRecord(value) && value.pending === true;
    if (pending && attempt < 3) {
      const retryAfterMs = isRecord(value) && Number.isSafeInteger(value.retry_after_ms)
        ? value.retry_after_ms as number
        : -1;
      if (retryAfterMs < 0 || retryAfterMs > 15_000) {
        throw new EgressFailure(503, "invalid_sponsored_prompt_retry_lease");
      }
      await new Promise((resolve) => setTimeout(resolve, retryAfterMs + 5));
      if (cancelled()) throw new EgressFailure(409, "sponsored_prompt_socket_closed");
      continue;
    }
    return {
      allowed: response.status === 200,
      ...(response.status === 200 && isRecord(value)
        && Number.isSafeInteger(value.attempt)
        && (value.attempt as number) >= 1
        && (value.attempt as number) <= 2
        ? { attempt: value.attempt as number }
        : {}),
      dispatch: response.status === 200 && isRecord(value) && value.dispatch === true,
      status: parseSponsoredPromptStatusValue(value),
    };
  }
  throw new EgressFailure(503, "sponsored_prompt_reservation_unavailable");
}

async function parseSponsoredPromptStatus(response: Response): Promise<SponsoredPromptStatus> {
  let value: unknown;
  try {
    value = JSON.parse(await readBoundedText(response, 1_024));
  } catch {
    throw new EgressFailure(503, "invalid_sponsored_prompt_status");
  }
  return parseSponsoredPromptStatusValue(value);
}

function parseSponsoredPromptStatusValue(value: unknown): SponsoredPromptStatus {
  if (!isRecord(value)
    || value.limit !== 3
    || !Number.isSafeInteger(value.used)
    || !Number.isSafeInteger(value.remaining)
    || (value.used as number) < 0
    || (value.remaining as number) < 0
    || (value.used as number) + (value.remaining as number) !== 3) {
    throw new EgressFailure(503, "invalid_sponsored_prompt_status");
  }
  return {
    limit: 3,
    used: value.used as number,
    remaining: value.remaining as number,
  };
}

async function grantSponsoredContinuation(
  env: EgressEnv,
  userId: string,
  promptId: string,
  attempt: number,
  responseId: string,
  callIds: readonly string[],
): Promise<void> {
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-prompts/continuation",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        action: "grant",
        prompt_id: promptId,
        attempt,
        response_id: responseId,
        call_ids: callIds,
      }),
    },
  );
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(503, "sponsored_continuation_grant_unavailable");
  }
  await cancelResponseBody(response);
}

async function consumeSponsoredContinuation(
  env: EgressEnv,
  userId: string,
  callIds: readonly string[],
  options?: Readonly<{
    responseId?: string;
    promptId?: string;
    promptHash?: string;
  }>,
): Promise<Readonly<{ promptId: string; attempt: number }> | undefined> {
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-prompts/continuation",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        action: "consume",
        ...(options?.responseId ? { response_id: options.responseId } : {}),
        ...(options?.promptId ? { prompt_id: options.promptId } : {}),
        ...(options?.promptHash ? { prompt_hash: options.promptHash } : {}),
        call_ids: callIds,
      }),
    },
  );
  if (response.status === 409) {
    await cancelResponseBody(response);
    return undefined;
  }
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(409, "sponsored_continuation_unavailable");
  }
  let value: unknown;
  try { value = JSON.parse(await readBoundedText(response, 1_024)); }
  catch { throw new EgressFailure(503, "invalid_sponsored_continuation"); }
  const promptId = stringField(value, "prompt_id");
  const attempt = isRecord(value) && Number.isSafeInteger(value.attempt)
    ? value.attempt as number
    : undefined;
  if (!promptId || !SPONSORED_PROMPT_ID.test(promptId)
    || !attempt || attempt > 2) {
    throw new EgressFailure(503, "invalid_sponsored_continuation");
  }
  return { promptId, attempt };
}

async function setSponsoredPromptLifecycle(
  env: EgressEnv,
  userId: string,
  promptId: string,
  attempt: number,
  action: "terminal" | "interrupted" | "heartbeat",
): Promise<"updated" | "settled"> {
  const response = await userBroker(env, userId).fetch(
    "https://credentials.internal/v1/sponsored-prompts/lifecycle",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action, prompt_id: promptId, attempt }),
    },
  );
  if (response.status !== 200) {
    await cancelResponseBody(response);
    throw new EgressFailure(503, "sponsored_prompt_lifecycle_unavailable");
  }
  let value: unknown;
  try { value = JSON.parse(await readBoundedText(response, 1_024)); }
  catch { throw new EgressFailure(503, "invalid_sponsored_prompt_lifecycle"); }
  if (isRecord(value) && value.updated === true) return "updated";
  if (action === "heartbeat" && isRecord(value)
    && value.updated === false && value.settled === true) return "settled";
  throw new EgressFailure(409, "sponsored_prompt_attempt_stale");
}

async function publishSponsoredInterruption(
  env: EgressEnv,
  userId: string,
  promptId: string,
  attempt: number,
): Promise<void> {
  let failure: unknown;
  for (let retry = 0; retry < 3; retry += 1) {
    try {
      await setSponsoredPromptLifecycle(env, userId, promptId, attempt, "interrupted");
      return;
    } catch (error) {
      failure = error;
    }
  }
  throw failure;
}

function sponsoredResponsesWebSocket(
  upstreamResponse: Response,
  env: EgressEnv,
  userId: string,
  connectionId: string,
  ctx?: Pick<ExecutionContext, "waitUntil">,
): Response {
  const upstream = upstreamResponse.webSocket;
  if (!upstream) throw new EgressFailure(502, "sponsored_responses_upgrade_missing");
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  let closed = false;
  let activePromptId: string | undefined;
  let activeAttempt: number | undefined;
  let generationInFlight = false;
  let clientTail = Promise.resolve();
  let upstreamTail = Promise.resolve();
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  let heartbeatRunning = false;
  let connectionHeartbeat: ReturnType<typeof setInterval> | undefined;
  let connectionHeartbeatRunning = false;
  let connectionReleased = false;

  const retain = (promise: Promise<void>) => {
    if (ctx) ctx.waitUntil(promise);
    else void promise;
  };

  const interruptActiveGeneration = () => {
    if (!generationInFlight || !activePromptId || !activeAttempt) return;
    generationInFlight = false;
    const publication = publishSponsoredInterruption(
      env,
      userId,
      activePromptId,
      activeAttempt,
    ).catch((error) => {
      console.error({
        type: "sponsored_prompt.interruption_failed",
        error_kind: error instanceof Error ? error.name : typeof error,
      });
    });
    retain(publication);
  };

  const releaseConnection = () => {
    if (connectionReleased) return;
    connectionReleased = true;
    retain(updateSponsoredConnection(env, userId, connectionId, "release")
      .then(() => undefined)
      .catch(logSponsoredConnectionReleaseFailure));
  };

  const close = (code: number, reason: string, retryActive = false) => {
    if (closed) return;
    if (retryActive) interruptActiveGeneration();
    closed = true;
    if (heartbeat !== undefined) clearInterval(heartbeat);
    if (connectionHeartbeat !== undefined) clearInterval(connectionHeartbeat);
    releaseConnection();
    closeSponsoredSocket(server, code, reason);
    closeSponsoredSocket(upstream, code, reason);
  };

  const heartbeatMs = env.ENVIRONMENT?.trim().toLowerCase() === "test" ? 75 : 5_000;
  heartbeat = setInterval(() => {
    if (closed || !generationInFlight || !activePromptId || !activeAttempt || heartbeatRunning) return;
    heartbeatRunning = true;
    const renewal = setSponsoredPromptLifecycle(
      env,
      userId,
      activePromptId,
      activeAttempt,
      "heartbeat",
    ).then(() => undefined)
      .catch(() => close(1011, "sponsored prompt heartbeat failed", true))
      .finally(() => { heartbeatRunning = false; });
    retain(renewal);
  }, heartbeatMs);
  connectionHeartbeat = setInterval(() => {
    if (closed || connectionHeartbeatRunning) return;
    connectionHeartbeatRunning = true;
    const renewal = updateSponsoredConnection(env, userId, connectionId, "heartbeat")
      .then((updated) => {
        if (!updated) close(1011, "sponsored connection lease lost", true);
      })
      .catch(() => close(1011, "sponsored connection heartbeat failed", true))
      .finally(() => { connectionHeartbeatRunning = false; });
    retain(renewal);
  }, heartbeatMs);

  server.addEventListener("message", (event) => {
    clientTail = clientTail.then(async () => {
      if (closed) return;
      if (typeof event.data !== "string"
        || event.data.length > MAX_SPONSORED_RESPONSES_FRAME_CHARS) {
        close(4002, "invalid sponsored prompt frame");
        return;
      }
      const frame = sponsoredResponsesFrame(event.data);
      if (!frame.valid) {
        close(4002, "invalid sponsored prompt frame");
        return;
      }
      if (frame.generation) {
        if (generationInFlight) {
          close(4002, "sponsored generation already in flight");
          return;
        }
        let continuationClaim: Readonly<{ promptId: string; attempt: number }> | undefined;
        if (frame.continuation
          && (frame.continuation.responseId || (frame.promptId && frame.prompt))) {
          const promptHash = frame.prompt ? await sponsoredPromptHash(frame.prompt) : undefined;
          continuationClaim = await consumeSponsoredContinuation(
            env,
            userId,
            frame.continuation.callIds,
            {
              ...(frame.continuation.responseId
                ? { responseId: frame.continuation.responseId }
                : {}),
              ...(frame.promptId ? { promptId: frame.promptId } : {}),
              ...(promptHash ? { promptHash } : {}),
            },
          );
        }
        if (continuationClaim) {
          activePromptId = continuationClaim.promptId;
          activeAttempt = continuationClaim.attempt;
        } else if (frame.promptId) {
          const promptHash = await sponsoredPromptHash(frame.prompt);
          const reservation = await reserveSponsoredPrompt(
            env,
            userId,
            frame.promptId,
            promptHash,
            () => closed
              || server.readyState !== WebSocket.OPEN
              || upstream.readyState !== WebSocket.OPEN,
          );
          if (!reservation.allowed) {
            sendSponsoredPromptError(server);
            close(4003, "three free prompts used");
            return;
          }
          if (!reservation.dispatch) {
            sendSponsoredProtocolError(
              server,
              "sponsored_prompt_replay",
              "This free prompt was already dispatched.",
            );
            close(4002, "sponsored prompt already dispatched");
            return;
          }
          if (!reservation.attempt) {
            close(1011, "sponsored prompt attempt unavailable");
            return;
          }
          activePromptId = frame.promptId;
          activeAttempt = reservation.attempt;
        } else {
          close(4002, "sponsored prompt identity required");
          return;
        }
        generationInFlight = true;
      }
      if (upstream.readyState === WebSocket.OPEN) upstream.send(frame.encoded);
    }).catch(() => close(1011, "sponsored prompt check failed"));
  });
  upstream.addEventListener("message", (event) => {
    upstreamTail = upstreamTail.then(async () => {
      if (closed) return;
      if (typeof event.data === "string") {
        const providerFrame = sponsoredProviderFrame(event.data);
        if (providerFrame.grant) {
          if (!activePromptId || !activeAttempt) {
            throw new EgressFailure(503, "sponsored_prompt_identity_unavailable");
          }
          await grantSponsoredContinuation(
            env,
            userId,
            activePromptId,
            activeAttempt,
            providerFrame.grant.responseId,
            providerFrame.grant.callIds,
          );
        } else if (providerFrame.terminal && activePromptId && activeAttempt) {
          await setSponsoredPromptLifecycle(
            env,
            userId,
            activePromptId,
            activeAttempt,
            "terminal",
          );
        }
        if (providerFrame.terminal) generationInFlight = false;
      }
      if (server.readyState === WebSocket.OPEN) server.send(event.data);
    }).catch(() => close(1011, "sponsored continuation check failed"));
  });
  server.addEventListener("close", (event) => {
    close(event.code, event.reason || "client closed", true);
  });
  server.addEventListener("error", () => close(1011, "client WebSocket failed", true));
  upstream.addEventListener("close", (event) => {
    close(event.code, event.reason || "provider closed", true);
  });
  upstream.addEventListener("error", () => close(1011, "provider WebSocket failed", true));

  return new Response(null, {
    status: 101,
    headers: sanitizedUpstreamHeaders(upstreamResponse.headers),
    webSocket: client,
  });
}

export function sponsoredResponsesFrame(encoded: string): Readonly<{
  valid: boolean;
  generation: boolean;
  encoded: string;
  promptId?: string;
  prompt?: Readonly<Record<string, unknown>>;
  continuation?: Readonly<{ responseId?: string; callIds: readonly string[] }>;
}> {
  let value: unknown;
  try { value = JSON.parse(encoded); } catch {
    return { valid: false, generation: false, encoded };
  }
  if (!isRecord(value)) return { valid: false, generation: false, encoded };
  if (value.type !== "response.create") {
    return { valid: true, generation: false, encoded };
  }
  const canonical = canonicalSponsoredResponseCreate(value);
  if (value.generate === false) {
    return { valid: true, generation: false, encoded: JSON.stringify(canonical) };
  }
  const input = Array.isArray(value.input) ? value.input : [];
  const continuation = sponsoredContinuation(value, input);
  for (let index = input.length - 1; index >= 0; index -= 1) {
    const item = input[index];
    if (!isRecord(item) || item.type !== "message" || item.role !== "user") continue;
    return typeof item.id === "string" && SPONSORED_PROMPT_ID.test(item.id)
      ? {
        valid: true,
        generation: true,
        encoded: JSON.stringify(canonical),
        promptId: item.id,
        prompt: item,
        ...(continuation ? { continuation } : {}),
      }
      : {
        valid: true,
        generation: true,
        encoded: JSON.stringify(canonical),
        ...(continuation ? { continuation } : {}),
      };
  }
  return {
    valid: true,
    generation: true,
    encoded: JSON.stringify(canonical),
    ...(continuation ? { continuation } : {}),
  };
}

function canonicalSponsoredResponseCreate(
  value: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const reasoning = isRecord(value.reasoning) ? value.reasoning : {};
  const canonical: Record<string, unknown> = {
    ...value,
    model: "gpt-5.6-luna",
    reasoning: { ...reasoning, effort: "none", mode: "standard" },
  };
  delete canonical.service_tier;
  return canonical;
}

function sponsoredContinuation(
  value: Readonly<Record<string, unknown>>,
  input: readonly unknown[],
): Readonly<{ responseId?: string; callIds: readonly string[] }> | undefined {
  const responseId = typeof value.previous_response_id === "string"
    && SPONSORED_RESPONSE_ID.test(value.previous_response_id)
    ? value.previous_response_id
    : undefined;
  const callIds: string[] = [];
  for (const item of input) {
    if (!isRecord(item) || typeof item.type !== "string"
      || !/^(?:(?:custom_tool|function|computer)_call_output|tool_search_output)$/.test(item.type)) {
      continue;
    }
    if (item.call_id === undefined && item.type === "tool_search_output") continue;
    if (typeof item.call_id !== "string" || !SPONSORED_CALL_ID.test(item.call_id)) {
      return undefined;
    }
    callIds.push(item.call_id);
  }
  const unique = [...new Set(callIds)].sort();
  return unique.length > 0 && unique.length === callIds.length
    && unique.length <= MAX_SPONSORED_CALL_IDS
    ? { ...(responseId ? { responseId } : {}), callIds: unique }
    : undefined;
}

function sponsoredProviderFrame(encoded: string): Readonly<{
  terminal: boolean;
  grant?: Readonly<{ responseId: string; callIds: readonly string[] }>;
}> {
  let value: unknown;
  try { value = JSON.parse(encoded); } catch { return { terminal: false }; }
  if (!isRecord(value)) return { terminal: false };
  const terminal = value.type === "response.completed"
    || value.type === "response.failed"
    || value.type === "response.incomplete"
    || value.type === "response.cancelled";
  if (value.type !== "response.completed" || !isRecord(value.response)
    || typeof value.response.id !== "string"
    || !SPONSORED_RESPONSE_ID.test(value.response.id)
    || !Array.isArray(value.response.output)) return { terminal };
  const callIds = value.response.output.flatMap((item) => isRecord(item)
    && (item.type === "custom_tool_call"
      || item.type === "function_call"
      || item.type === "computer_call"
      || item.type === "tool_search_call")
    && typeof item.call_id === "string"
    && SPONSORED_CALL_ID.test(item.call_id)
    ? [item.call_id]
    : []);
  const unique = [...new Set(callIds)].sort();
  return unique.length > 0 && unique.length <= MAX_SPONSORED_CALL_IDS
    ? { terminal, grant: { responseId: value.response.id, callIds: unique } }
    : { terminal };
}

async function sponsoredPromptHash(prompt: Readonly<Record<string, unknown>> | undefined) {
  if (!prompt) throw new EgressFailure(400, "sponsored_prompt_identity_required");
  const bytes = new Uint8Array(await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(JSON.stringify(prompt)),
  ));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function sendSponsoredPromptError(socket: WebSocket): void {
  sendSponsoredProtocolError(
    socket,
    "sponsored_prompt_limit_reached",
    "Your three free prompts are used. Connect ChatGPT to continue.",
    "usage_limit_error",
  );
}

function sendSponsoredProtocolError(
  socket: WebSocket,
  code: string,
  message: string,
  type = "invalid_request_error",
): void {
  if (socket.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify({
    type: "error",
    error: {
      type,
      code,
      message,
      param: null,
    },
  }));
}

function closeSponsoredSocket(socket: WebSocket, code: number, reason: string): void {
  if (socket.readyState !== WebSocket.CONNECTING && socket.readyState !== WebSocket.OPEN) return;
  const safeCode = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1011;
  socket.close(safeCode, reason.slice(0, 120));
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

  const walletMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/wallet(?:\/(balance|connect|revoke-access-key))?$/,
  );
  if (walletMatch) {
    const userId = walletMatch[1]!;
    const operation = walletMatch[2];
    const target = operation
      ? `https://credentials.internal/v1/wallet/${operation}`
      : "https://credentials.internal/v1/wallet";
    if (!operation && request.method === "GET") {
      return userBroker(env, userId).fetch(target, { method: "GET" });
    }
    if (!operation && request.method === "PUT") {
      if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
      return userBroker(env, userId).fetch(target, { method: "PUT" });
    }
    if (operation === "balance" && request.method === "GET") {
      return userBroker(env, userId).fetch(target, { method: "GET" });
    }
    if (operation && request.method === "POST") {
      if (!isJsonContentType(request.headers.get("content-type"))) {
        return jsonError(415, "invalid_content_type");
      }
      return userBroker(env, userId).fetch(target, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: request.body,
      });
    }
    return jsonError(405, "method_not_allowed");
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

  const browserCookieJarMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/credentials\/browser-cookie-jars(?:\/([A-Za-z0-9_-]{22,64})(?:\/(materialize|names))?)?$/,
  );
  if (browserCookieJarMatch) {
    const userId = browserCookieJarMatch[1]!;
    const id = browserCookieJarMatch[2];
    const projection = browserCookieJarMatch[3];
    if (!id) {
      if (request.method !== "GET") return jsonError(405, "method_not_allowed");
      if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
      return userBroker(env, userId).fetch("https://credentials.internal/v1/browser-cookie-jars");
    }
    if (!BROWSER_COOKIE_JAR_ID.test(id)) {
      return jsonError(400, "invalid_browser_cookie_jar_id");
    }
    const target = `https://credentials.internal/v1/browser-cookie-jars/${id}${
      projection ? `/${projection}` : ""
    }`;
    if (request.method === "PUT" && !projection) {
      if (!isJsonContentType(request.headers.get("content-type"))) {
        return jsonError(415, "invalid_content_type");
      }
      let value: unknown;
      try { value = JSON.parse(await readBoundedText(request, MAX_BROWSER_COOKIE_JAR_BODY_BYTES)); }
      catch (error) {
        return error instanceof EgressFailure
          ? jsonError(error.status, error.code)
          : jsonError(400, "invalid_browser_cookie_jar");
      }
      const upsert = validateBrowserCookieJarUpsert(value);
      if (!upsert) return jsonError(400, "invalid_browser_cookie_jar");
      return userBroker(env, userId).fetch(target, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          schema_version: 1,
          origin: upsert.origin,
          profile_id: upsert.profileId,
          store_id: upsert.storeId,
          revision: upsert.revision,
          cookies: upsert.cookies,
        }),
      });
    }
    if (request.method === "POST" && projection) {
      if (!isJsonContentType(request.headers.get("content-type"))) {
        return jsonError(415, "invalid_content_type");
      }
      const value = await readJson(request, 8 * 1024);
      const binding = validateBrowserCookieJarBinding(value);
      if (!binding) return jsonError(400, "invalid_browser_cookie_jar_binding");
      return userBroker(env, userId).fetch(target, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          origin: binding.origin,
          profile_id: binding.profileId,
          store_id: binding.storeId,
        }),
      });
    }
    if (request.method === "DELETE" && !projection) {
      if (!isJsonContentType(request.headers.get("content-type"))) {
        return jsonError(415, "invalid_content_type");
      }
      const value = await readJson(request, 8 * 1024);
      const deletion = validateBrowserCookieJarDelete(value);
      if (!deletion) return jsonError(400, "invalid_browser_cookie_jar_delete");
      return userBroker(env, userId).fetch(target, {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          origin: deletion.origin,
          profile_id: deletion.profileId,
          store_id: deletion.storeId,
          revision: deletion.revision,
        }),
      });
    }
    return jsonError(405, "method_not_allowed");
  }

  const connectorMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/connectors(?:\/(github|google|gmail|gdrive|slack|x)(?:\/(callback)|\/connections\/([A-Za-z0-9_-]{43}))?)?$/,
  );
  if (connectorMatch) {
    const userId = connectorMatch[1]!;
    const connector = connectorMatch[2];
    const callback = connectorMatch[3] === "callback";
    const connectionId = connectorMatch[4];
    const target = connector
      ? `https://connectors.internal/v1/${connector}${callback
        ? "/callback"
        : connectionId ? `/connections/${connectionId}` : request.method === "POST" ? "/start" : ""}`
      : "https://connectors.internal/v1/status";
    if ((!connector && request.method !== "GET")
      || (connector && callback && request.method !== "POST")
      || (connectionId && request.method !== "DELETE")
      || (connector && !callback && !connectionId
        && request.method !== "POST" && request.method !== "DELETE")) {
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

  const vaultMatch = url.pathname.match(
    /^\/users\/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\/credentials\/vault\/(login|api_key|card|address|phone)(?:\/([A-Za-z0-9_-]{22,64}))?$/,
  );
  if (vaultMatch) {
    const userId = vaultMatch[1]!;
    const kind = vaultMatch[2] as VaultKind;
    const id = vaultMatch[3];
    const target = `https://credentials.internal/v1/vault/${kind}${id ? `/${id}` : ""}`;
    if (request.method === "DELETE" && id) {
      return userBroker(env, userId).fetch(target, { method: "DELETE" });
    }
    if (request.method !== "POST" || id) return jsonError(405, "method_not_allowed");
    if (!isJsonContentType(request.headers.get("content-type"))) {
      return jsonError(415, "invalid_content_type");
    }
    let body: unknown;
    try {
      body = JSON.parse(await readBoundedText(request, MAX_VAULT_BODY_BYTES));
    } catch (error) {
      return error instanceof EgressFailure
        ? jsonError(error.status, error.code)
        : jsonError(400, "invalid_vault_entry");
    }
    const validated = validateVaultEntryPayload(body, kind);
    if (!validated) return jsonError(400, "invalid_vault_entry");
    const forwarded = Object.fromEntries(
      Object.entries(validated).filter(([key]) => key !== "kind"),
    );
    return userBroker(env, userId).fetch(target, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(forwarded),
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
      headers: { [CREDENTIAL_PROVENANCE_HEADER]: "user" },
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
    const credential = await resolveCredential(
      env,
      userId,
      false,
      undefined,
      EPHEMERAL_BROWSER_MODEL_SUBJECT.test(subject),
    );
    const sponsoredPrompts = credential.source === "sponsored"
      ? await sponsoredPromptStatus(env, userId)
      : undefined;
    return json({
      ready: true,
      active: credential.kind,
      source: credential.source,
      ...(sponsoredPrompts
        ? { free_prompts_remaining: sponsoredPrompts.remaining }
        : {}),
    }, 200);
  } catch { return jsonError(503, "broker_not_ready"); }
}

async function handleContextCapability(request: Request, env: EgressEnv): Promise<Response> {
  if (request.method !== "GET" || request.body !== null) return jsonError(404, "not_found");
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !SUBJECT.test(subject)
    || request.headers.get("authorization") !== PROVIDER_PLACEHOLDER) {
    return jsonError(403, "agent_subject_required");
  }
  try {
    const userId = await resolveSubject(env, subject);
    const credential = await resolveCredential(env, userId, false);
    return json({ enabled: historyNotesEligible(credential) }, 200);
  } catch { return json({ enabled: false }, 200); }
}

async function handleSponsoredTrialReset(request: Request, env: EgressEnv): Promise<Response> {
  if (!localSponsoredTrialResetEnabled(env)
    || request.method !== "POST") {
    return jsonError(404, "not_found");
  }
  const subject = request.headers.get(SUBJECT_HEADER);
  if (!subject || !EPHEMERAL_BROWSER_MODEL_SUBJECT.test(subject)) {
    return jsonError(403, "agent_subject_required");
  }
  try {
    const userId = await resolveSubject(env, subject);
    const credential = await resolveCredential(env, userId, false, undefined, true);
    if (credential.source !== "sponsored") return jsonError(409, "sponsored_trial_unavailable");
    const reset = await userBroker(env, userId).fetch(
      "https://credentials.internal/v1/sponsored-prompts/reset",
      { method: "POST" },
    );
    if (!reset.ok) {
      await cancelResponseBody(reset);
      return jsonError(503, "sponsored_trial_reset_failed");
    }
    const value = await reset.json<Record<string, unknown>>();
    if (value.remaining !== 3 || value.used !== 0 || value.limit !== 3) {
      return jsonError(503, "sponsored_trial_reset_failed");
    }
    return json({ free_prompts_remaining: 3 }, 200);
  } catch {
    return jsonError(503, "sponsored_trial_reset_failed");
  }
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
  if (operation.id === "history-notes") {
    try {
      for (const [name, value] of historyNotesHeaders(original, body)) headers.set(name, value);
    } catch { throw new EgressFailure(400, "invalid_history_notes_request"); }
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
    const httpRoute = operation.id === "history-notes"
      ? `codex-${operation.path.slice("/v1/alpha/".length).replace("/v2/", "-").replaceAll("_", "-")}`
      : RELAY_HTTP_ROUTES[operation.id];
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
  allowSponsored = false,
): Promise<ResolvedModelCredential> {
  try {
    const credential = await resolveUserCredential(env, userId, recover, revision);
    if (!isLegacyLocalBootstrapCredential(env, userId, credential)) {
      return { ...credential, source: "user" };
    }
  } catch (error) {
    if (!(error instanceof EgressFailure) || error.status !== 409) throw error;
  }
  if (!allowSponsored) throw new EgressFailure(409, "user_credential_unavailable");
  return { ...await resolveSponsoredChatGptCredential(env, recover, revision), source: "sponsored" };
}

type ResolvedModelCredential = UserCredentialSnapshot & Readonly<{
  source: "sponsored" | "user";
}>;

async function resolveSponsoredChatGptCredential(
  env: EgressEnv,
  recover: boolean,
  revision?: number,
): Promise<UserCredentialSnapshot> {
  const sponsorUserId = env.NANOCODEX_SPONSORED_CHATGPT_USER_ID?.trim();
  if (!sponsorUserId || !USER_ID.test(sponsorUserId)) {
    throw new EgressFailure(409, "sponsored_chatgpt_unavailable");
  }
  if (localClaimEnabled(env)) {
    const claimed = await userBroker(env, sponsorUserId).fetch(
      "https://credentials.internal/v1/chatgpt/local-claim",
      {
        method: "POST",
        headers: { [CREDENTIAL_PROVENANCE_HEADER]: "sponsor" },
      },
    );
    if (!claimed.ok) {
      await cancelResponseBody(claimed);
      throw new EgressFailure(409, "sponsored_chatgpt_unavailable");
    }
    await cancelResponseBody(claimed);
  }
  const credential = await resolveUserCredential(env, sponsorUserId, recover, revision);
  if (credential.kind !== "chatgpt") {
    throw new EgressFailure(409, "sponsored_chatgpt_unavailable");
  }
  return credential;
}

export function isLegacyLocalBootstrapCredential(
  env: Pick<EgressEnv,
    "ALLOW_LOCAL_CREDENTIAL_CLAIM" | "ENVIRONMENT" | "LOCAL_CHATGPT_BOOTSTRAP"
    | "NANOCODEX_SPONSORED_CHATGPT_USER_ID">,
  userId: string,
  credential: UserCredentialSnapshot,
): boolean {
  if (!localClaimEnabled(env) || credential.kind !== "chatgpt" || credential.provenance
    || userId === env.NANOCODEX_SPONSORED_CHATGPT_USER_ID?.trim()) {
    return false;
  }
  const raw = env.LOCAL_CHATGPT_BOOTSTRAP?.trim();
  if (!raw) return false;
  try {
    const bootstrap = JSON.parse(raw) as unknown;
    if (!isRecord(bootstrap)) return false;
    const accountId = stringField(bootstrap, "account_id");
    return Boolean(accountId && credential.accountId === accountId);
  } catch {
    return false;
  }
}

async function resolveUserCredential(
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
function localClaimEnabled(
  env: Pick<EgressEnv, "ALLOW_LOCAL_CREDENTIAL_CLAIM" | "ENVIRONMENT">,
): boolean {
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  return env.ALLOW_LOCAL_CREDENTIAL_CLAIM === "true"
    && (environment === "development" || environment === "local" || environment === "test");
}
function localSponsoredTrialResetEnabled(
  env: Pick<EgressEnv,
    "ALLOW_LOCAL_CREDENTIAL_CLAIM" | "ENVIRONMENT" | "NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET">,
): boolean {
  return localClaimEnabled(env)
    && env.NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET === "true";
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
function isJsonContentType(value: string | null): boolean {
  return value?.split(";", 1)[0]?.trim().toLowerCase() === "application/json";
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
    ? tail?.match(/^(github|google|gmail|gdrive|slack|x)/)?.[1]
    : undefined;
  const log = status >= 500 ? console.error : status >= 400 ? console.warn : console.info;
  log({
    type: "egress.control",
    action: status >= 500 ? "error" : status >= 400 ? "deny" : "allow",
    method: request.method,
    operation: subject ? "subject" : user?.[2] ?? "unknown",
    status,
    duration_ms: Date.now() - started,
    ...(deploymentSha === undefined ? {} : { deployment_sha: deploymentSha }),
    ...(connector === undefined ? {} : { connector }),
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
    || rule === "gcalendar" || rule === "gtasks" || rule === "gdocs"
    || rule === "gsheets" || rule === "gslides" || rule === "gcontacts"
    || rule === "slack" || rule === "x" || rule === "mcp";
  const log = action === "error" ? console.error : action === "deny" ? console.warn : console.info;
  const safeDetail = {
    ...(typeof detail.code === "string" ? { code: detail.code } : {}),
    ...(typeof detail.status === "number" ? { status: detail.status } : {}),
    ...(typeof detail.recovered === "boolean" ? { recovered: detail.recovered } : {}),
    ...(typeof detail.connector === "string" ? { connector: detail.connector } : {}),
    ...(typeof detail.deployment_sha === "string" ? { deployment_sha: detail.deployment_sha } : {}),
  };
  log({
    type: "egress.request",
    action,
    rule,
    method: request.method,
    host: url.host,
    path: connector ? "/provider-api" : url.pathname,
    duration_ms: Date.now() - started,
    ...safeDetail,
  });
}
