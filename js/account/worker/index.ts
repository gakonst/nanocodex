import {
  CHATGPT_LOGIN_TTL_MS,
  CHATGPT_SESSION_TTL_MS,
  ChatGptSession,
  type ChatGptCredential,
  type ChatGptOperation,
} from "./subscriptionAuth.ts";
import {
  fetchChatGpt,
  type ChatGptEgressEnv,
  warmChatGptEgress,
} from "./chatGptEgressClient.ts";
import { CredentialVault, type CredentialVaultEnv, type EncryptedEnvelope } from "./credentialVault.ts";
import { EvalCoordinator, routeEvalMutation, type EvalStorageEnv } from "./evalCoordinator.ts";
import { routeEvalRead } from "./evalReadApi.ts";
import { handleGitRequest, type GitStorageEnv } from "./gitRoutes.ts";
import { GitRepository } from "./gitRepository.ts";
import { proxyDefaultMcp } from "./mcpProxy.ts";
import {
  handleThreadGitRequest,
  type ThreadGitStorageEnv,
} from "./threadRoutes.ts";
import { ThreadGitRepository } from "./threadRepository.ts";
import {
  apiKeyActorId,
  limitAgentOperation,
  limitLoginStart,
  limitSessionPoll,
  type PublicSecurityEnv,
} from "./publicSecurity.ts";
import { routeLinkPreview } from "./linkPreview.ts";
import { routeManaged } from "./managedProxy.ts";
import { routeAccountFunding, type AccountFundingProxyEnv } from "./accountFundingProxy.ts";
import {
  routeChiefOfStaff,
  type ChiefOfStaffProxyEnv,
} from "./chiefOfStaffProxy.ts";
import {
  routeConnectDialog,
  type ConnectDialogProxyEnv,
} from "./connectDialogProxy.ts";
import { routeLocalConnectApi, type LocalConnectApiEnv } from "./localConnectApi.ts";
import { routeConnectApi, type ConnectApiProxyEnv } from "./connectApiProxy.ts";
import {
  routeLocalConnectorCallbackReturn,
  type LocalConnectorCallbackRelayEnv,
} from "./localConnectorCallbackRelay.ts";
import {
  fetchManagedModel,
  fetchManagedRealtimeCall,
  managedModelAccess,
  managedModelActorId,
  managedModelStatus,
  openManagedResponsesWebSocket,
  openManagedRealtimeSideband,
  resetManagedSponsoredTrial,
  type ManagedModelAccess,
} from "./managedModel.ts";

export { ChatGptSession, EvalCoordinator, GitRepository, ThreadGitRepository };

const json = (body: unknown, init?: ResponseInit) =>
  Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init?.headers,
    },
  });

const RESPONSES_UPGRADE_URL = "https://api.openai.com/v1/responses";
const CHATGPT_API_BASE_URL = "https://chatgpt.com/backend-api/codex";
const RESPONSES_WEBSOCKETS_BETA = "responses_websockets=2026-02-06";
const WEB_SEARCH_URL = "https://api.openai.com/v1/alpha/search";
const IMAGE_GENERATION_URL = "https://api.openai.com/v1/images/generations";
const IMAGE_EDIT_URL = "https://api.openai.com/v1/images/edits";
const MODEL = "gpt-5.6-sol";
const IMAGE_MODEL = "gpt-image-2";
const CODEX_ORIGINATOR = "codex_cli_rs";
const CODEX_USER_AGENT = "codex_cli_rs/0.0.0";
const CODEX_ATTESTATION_UNAVAILABLE = '{"v":1,"s":1}';
const MAX_JSON_BODY_CHARS = 32 * 1024 * 1024;
const MAX_SEARCH_OUTPUT_CHARS = 1024 * 1024;
const MAX_WEB_OPERATION_ITEMS = 16;
const MAX_IMAGE_INPUT_CHARS = 8 * 1024 * 1024;
const MAX_IMAGE_INPUT_TOTAL_CHARS = 20 * 1024 * 1024;
const MAX_IMAGE_PROMPT_CHARS = 16 * 1024;
const MAX_API_KEY_CHARS = 1_024;
const MAX_REALTIME_SDP_CHARS = 1024 * 1024;
const MAX_REALTIME_CALL_BODY_CHARS = MAX_REALTIME_SDP_CHARS + 128 * 1024;
const REALTIME_SIDEBAND_URL = "https://api.openai.com/v1/live/";
const MAX_WEBSOCKET_MESSAGE_CHARS = 8 * 1024 * 1024;
const BYOK_SESSION_TTL_MS = 60 * 60 * 1_000;
const BYOK_COOKIE = "nanocodex_byok_v2";
const SECURE_BYOK_COOKIE = "__Secure-nanocodex_byok_v2";
const CHATGPT_COOKIE = "nanocodex_chatgpt_v2";
const SECURE_CHATGPT_COOKIE = "__Secure-nanocodex_chatgpt_v2";
const GIT_SHA_PATTERN = /^[0-9a-f]{40}$/;
const LOCAL_SPONSORED_TRIAL_RESET = typeof __NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__ !== "undefined"
  && __NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__;

type WorkerEnv = GitStorageEnv & ThreadGitStorageEnv & EvalStorageEnv & ChatGptEgressEnv
  & AccountFundingProxyEnv
  & ConnectDialogProxyEnv
  & LocalConnectApiEnv
  & ConnectApiProxyEnv
  & LocalConnectorCallbackRelayEnv
  & ChiefOfStaffProxyEnv
  & PublicSecurityEnv & CredentialVaultEnv & {
  ASSETS?: Fetcher;
  ENVIRONMENT: string;
  DEPLOYMENT_SHA?: string;
  CHATGPT_ISSUER?: string;
  BYOK_SESSIONS?: DurableObjectNamespace;
  CHATGPT_SESSIONS?: DurableObjectNamespace;
  EGRESS?: Fetcher;
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_PUBLIC_ORIGIN?: string;
};

type ApiKeyCredential = {
  kind: "api_key";
  apiKey: string;
  actorId: string;
  source: "user";
};
type SubscriptionCredential = ChatGptCredential & {
  actorId: string;
  sessionId: string;
  leaseId?: string;
  source: "subscription";
};
type Credential = ApiKeyCredential | SubscriptionCredential;
type StoredCredential = { apiKey: string; expiresAt: number };

export default {
  async fetch(
    request: Request,
    env: WorkerEnv,
    context?: ExecutionContext,
  ): Promise<Response> {
    const url = new URL(request.url);
    const insecure = enforceHttps(request, env, url);
    if (insecure) return insecure;
    const connectorCallbackReturn = await routeLocalConnectorCallbackReturn(request, env, url);
    if (connectorCallbackReturn != null) return connectorCallbackReturn;
    const accountFunding = await routeAccountFunding(request, env, url);
    if (accountFunding != null) return accountFunding;
    const localConnectApi = await routeLocalConnectApi(request, env, url);
    if (localConnectApi != null) return localConnectApi;
    const connectApi = await routeConnectApi(request, env, url);
    if (connectApi != null) return connectApi;
    const connectDialog = await routeConnectDialog(request, env, url);
    if (connectDialog != null) return connectDialog;
    const chiefOfStaff = await routeChiefOfStaff(request, env, url);
    if (chiefOfStaff != null) return chiefOfStaff;
    const managed = await routeManaged(request, env, url);
    if (managed != null) return managed;
    const evalMutation = await routeEvalMutation(request, env, url);
    if (evalMutation != null) return evalMutation;
    const evalRead = await routeEvalRead(request, env, url, context);
    if (evalRead != null) return evalRead;
    const gitResponse = await handleGitRequest(request, env, url, context);
    if (gitResponse != null) return gitResponse;
    const threadGitResponse = await handleThreadGitRequest(request, env, url, context);
    if (threadGitResponse != null) return threadGitResponse;
    const mcpResponse = await proxyDefaultMcp(
      request,
      url,
      sameOrigin(request, url, env),
      env.NANOCODEX_BACKEND,
    );
    if (mcpResponse != null) return mcpResponse;

    if (url.pathname === "/api/health" && request.method === "GET") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) {
        const model = await managedModelStatus(managed);
        return json({
          agent_configured: model.ready,
          credential_source: model.source === "sponsored"
            ? "sponsored"
            : model.ready ? "brokered" : null,
          ...(model.source === "sponsored"
            ? { free_prompts_remaining: model.freePromptsRemaining }
            : {}),
          voice_enabled: model.voiceEnabled,
          deployment_sha: GIT_SHA_PATTERN.test(env.DEPLOYMENT_SHA ?? "")
            ? env.DEPLOYMENT_SHA
            : null,
          interactive_auth: false,
          service: "nanocodex",
          runtime: "cloudflare-workers",
          status: "ok",
        });
      }
      const resolved = await resolveCredential(request, env, "health");
      if (resolved instanceof Response) return resolved;
      const credential = resolved;
      if (credential?.kind === "chatgpt" && context) {
        context.waitUntil(warmChatGptEgress(env, credential.sessionId));
      }
      return json({
        agent_configured: Boolean(credential),
        credential_source: credential?.source ?? null,
        voice_enabled: credential?.kind === "chatgpt",
        deployment_sha: GIT_SHA_PATTERN.test(env.DEPLOYMENT_SHA ?? "")
          ? env.DEPLOYMENT_SHA
          : null,
        service: "nanocodex",
        runtime: "cloudflare-workers",
        status: "ok",
      });
    }

    if (LOCAL_SPONSORED_TRIAL_RESET && url.pathname === "/api/dev/sponsored-trial/reset") {
      const environment = env.ENVIRONMENT?.trim().toLowerCase();
      if (environment !== "development" && environment !== "local") {
        return json({ error: "not_found" }, { status: 404 });
      }
      if (request.method !== "POST") {
        return json({ error: "method_not_allowed" }, { status: 405 });
      }
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (!managed) return json({ error: "managed model access unavailable" }, { status: 503 });
      const reset = await resetManagedSponsoredTrial(managed);
      if (!reset.ok) {
        const status = reset.status >= 400 && reset.status < 500 ? reset.status : 503;
        await reset.body?.cancel();
        return json({ error: "sponsored_trial_reset_failed" }, { status });
      }
      await reset.body?.cancel();
      return json({ free_prompts_remaining: 3 });
    }

    if (url.pathname === "/api/auth/chatgpt" && request.method === "POST") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) return interactiveAuthDisabled();
      return startChatGptSession(request, env, url);
    }

    if (url.pathname === "/api/auth/chatgpt" && request.method === "GET") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) return interactiveAuthDisabled();
      return chatGptSessionStatus(request, env, context);
    }

    if (url.pathname === "/api/auth/chatgpt" && request.method === "DELETE") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) return interactiveAuthDisabled();
      return clearChatGptSession(request, env, url);
    }

    if (url.pathname === "/api/auth/openai" && request.method === "PUT") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) return interactiveAuthDisabled();
      return createByokSession(request, env, url);
    }

    if (url.pathname === "/api/auth/openai" && request.method === "DELETE") {
      const managed = managedAccess(request, env);
      if (managed instanceof Response) return managed;
      if (managed) return interactiveAuthDisabled();
      return clearByokSession(request, env, url);
    }

    if (url.pathname === "/api/responses") {
      return upgradeResponsesWebSocket(request, env, url, context);
    }

    if (url.pathname === "/api/realtime/sideband") {
      return upgradeRealtimeSideband(request, env, url);
    }

    if (url.pathname === "/api/realtime/calls" && request.method === "POST") {
      return createRealtimeCall(request, env, url);
    }

    if (url.pathname === "/api/tools/web-search" && request.method === "POST") {
      return proxyWebSearch(request, env, url);
    }

    if (url.pathname === "/api/tools/image-generation" && request.method === "POST") {
      return proxyImageGeneration(request, env, url);
    }

    const linkPreview = await routeLinkPreview(request, env, url);
    if (linkPreview != null) return linkPreview;

    return json({ error: "not_found" }, { status: 404 });
  },
};

function enforceHttps(request: Request, env: WorkerEnv, url: URL): Response | null {
  if (
    url.protocol === "https:"
    || (env.ENVIRONMENT !== "production" && env.ENVIRONMENT !== "preview")
  ) return null;

  const headers = new Headers({
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  if (request.method === "GET" || request.method === "HEAD") {
    const secure = new URL(url);
    secure.protocol = "https:";
    headers.set("location", secure.href);
    return new Response(null, { headers, status: 308 });
  }
  headers.set("content-type", "text/plain; charset=utf-8");
  return new Response("HTTPS required", { headers, status: 426 });
}

function managedAccess(
  request: Request,
  env: WorkerEnv,
): ManagedModelAccess | Response | undefined {
  try {
    return managedModelAccess(request, env);
  } catch {
    return json({ error: "managed model access is misconfigured" }, { status: 503 });
  }
}

function interactiveAuthDisabled(): Response {
  return json({ error: "interactive authentication is disabled for managed model access" }, {
    status: 409,
  });
}

function isManagedAccess(value: Credential | ManagedModelAccess): value is ManagedModelAccess {
  return "binding" in value;
}

async function proxyWebSearch(request: Request, env: WorkerEnv, url: URL): Promise<Response> {
  const access = await validateToolRequest(request, env, url, "search");
  if (access instanceof Response) return access;
  const decoded = await readJsonBody(request);
  if (decoded instanceof Response) return decoded;
  const sessionId = typeof decoded.session_id === "string" ? decoded.session_id : "";
  if (!/^[A-Za-z0-9._:-]{1,200}$/.test(sessionId)) return json({ error: "invalid session" }, { status: 400 });
  const commands = asObject(decoded.commands);
  if (!commands || !hasWebOperation(commands)) {
    return json({ error: "web__run requires at least one operation" }, { status: 400 });
  }
  const queries = Array.isArray(commands.search_query) ? commands.search_query.length : 0;
  if (queries > 4) return json({ error: "web__run accepts at most 4 search queries" }, { status: 400 });
  if (queries === 4 && !["medium", "long"].includes(String(commands.response_length))) {
    return json({ error: "four search queries require medium or long response_length" }, { status: 400 });
  }
  if (webOperationItemCount(commands) > MAX_WEB_OPERATION_ITEMS) {
    return json({ error: "web__run accepts at most 16 operation items per request" }, { status: 400 });
  }
  const actorId = isManagedAccess(access)
    ? managedModelActorId(request, access)
    : access.actorId;
  const limited = await limitAgentOperation(env, actorId, "search");
  if (limited) return limited;
  const upstreamBody = JSON.stringify({
    id: sessionId,
    model: MODEL,
    commands,
    settings: { allowed_callers: ["direct"], external_web_access: true },
    max_output_tokens: 10_000,
  });
  const upstream = isManagedAccess(access)
    ? await fetchManagedModel(access, "search", upstreamBody)
    : await fetchOpenAi(
        access,
        env,
        access.kind === "chatgpt"
          ? `${chatGptApiBaseUrl(env)}/alpha/search`
          : WEB_SEARCH_URL,
        {
          method: "POST",
          headers: openAiHeaders(access),
          body: upstreamBody,
        },
      );
  const body = await upstream.text();
  if (body.length > MAX_SEARCH_OUTPUT_CHARS) {
    return json({ error: "web search response exceeded 1 MiB" }, { status: 502 });
  }
  if (!upstream.ok) return upstreamError("web search", upstream.status, body);
  let payload: unknown;
  try { payload = JSON.parse(body); } catch { return json({ error: "web search returned invalid JSON" }, { status: 502 }); }
  const output = asObject(payload)?.output;
  if (typeof output !== "string") return json({ error: "web search response omitted output" }, { status: 502 });
  return json({ output });
}

async function proxyImageGeneration(request: Request, env: WorkerEnv, url: URL): Promise<Response> {
  const access = await validateToolRequest(request, env, url, "image");
  if (access instanceof Response) return access;
  const decoded = await readJsonBody(request);
  if (decoded instanceof Response) return decoded;
  const prompt = typeof decoded.prompt === "string" ? decoded.prompt.trim() : "";
  if (!prompt) return json({ error: "image prompt must not be empty" }, { status: 400 });
  if (prompt.length > MAX_IMAGE_PROMPT_CHARS) {
    return json({ error: "image prompt exceeded 16 KiB" }, { status: 400 });
  }
  const images = Array.isArray(decoded.images)
    ? decoded.images.filter((image): image is string => typeof image === "string")
    : [];
  if (images.length > 5 || images.some((image) => !image.startsWith("data:image/"))) {
    return json({ error: "image edits require at most five data-image inputs" }, { status: 400 });
  }
  if (
    images.some((image) => image.length > MAX_IMAGE_INPUT_CHARS)
    || images.reduce((total, image) => total + image.length, 0) > MAX_IMAGE_INPUT_TOTAL_CHARS
  ) {
    return json({ error: "image edit inputs exceeded the 8 MiB each / 20 MiB total limit" }, { status: 413 });
  }
  const actorId = isManagedAccess(access)
    ? managedModelActorId(request, access)
    : access.actorId;
  const limited = await limitAgentOperation(env, actorId, "image");
  if (limited) return limited;
  const body = JSON.stringify({
    ...(images.length ? { images: images.map((image_url) => ({ image_url })) } : {}),
    prompt,
    background: "auto",
    model: IMAGE_MODEL,
    quality: "auto",
    size: "auto",
  });
  const upstream = isManagedAccess(access)
    ? await fetchManagedModel(
        access,
        images.length ? "image_edit" : "image_generation",
        body,
      )
    : await fetchOpenAi(
        access,
        env,
        access.kind === "chatgpt"
          ? `${chatGptApiBaseUrl(env)}/images/${images.length ? "edits" : "generations"}`
          : images.length ? IMAGE_EDIT_URL : IMAGE_GENERATION_URL,
        {
          method: "POST",
          headers: openAiHeaders(access),
          body,
        },
      );
  const payload = await upstream.json().catch(() => undefined) as {
    data?: Array<{ b64_json?: unknown }>;
    error?: { message?: unknown };
  } | undefined;
  if (!upstream.ok) {
    const message = typeof payload?.error?.message === "string" ? payload.error.message : `HTTP ${upstream.status}`;
    return json({ error: `image generation failed: ${message}` }, { status: 502 });
  }
  const encoded = payload?.data?.[0]?.b64_json;
  if (typeof encoded !== "string" || !encoded) {
    return json({ error: "image generation returned no image" }, { status: 502 });
  }
  return json({ image_url: `data:image/png;base64,${encoded}` });
}

async function createRealtimeCall(request: Request, env: WorkerEnv, url: URL): Promise<Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected JSON" }, { status: 415 });
  }
  const decoded = await readJsonBody(request);
  if (decoded instanceof Response) return decoded;
  const identity = realtimeIdentity(decoded);
  const managedAgentId = managedRealtimeAgentId(decoded.managed_agent_id, identity);
  const callBody = typeof decoded.call_body === "string" ? decoded.call_body : "";
  if (!callBody || callBody.length > MAX_REALTIME_CALL_BODY_CHARS) {
    return json({ error: "Realtime call body exceeded its bound" }, { status: 400 });
  }
  if (!identity) {
    return json({ error: "invalid session" }, { status: 400 });
  }
  if (decoded.managed_agent_id !== undefined && !managedAgentId) {
    return json({ error: "invalid managed agent" }, { status: 400 });
  }
  const body = callBody;
  const managed = managedAccess(request, env);
  if (managed instanceof Response) return managed;
  let upstream: Response;
  if (managed) {
    const limited = await limitAgentOperation(
      env,
      managedModelActorId(request, managed),
      "socket",
    );
    if (limited) return limited;
    upstream = await fetchManagedRealtimeCall(managed, identity, body, managedAgentId);
  } else {
    const resolved = await resolveSubscriptionCredential(request, env, "health");
    if (resolved instanceof Response) return resolved;
    let credential = resolved;
    if (!credential) {
      return json({ error: "voice requires an authenticated ChatGPT subscription" }, { status: 503 });
    }
    const limited = await limitAgentOperation(env, credential.actorId, "socket");
    if (limited) return limited;
    upstream = await openRealtimeCall(credential, env, identity, body);
    if (upstream.status === 401) {
      await upstream.body?.cancel();
      const recovered = await recoverSubscriptionCredential(request, env, credential);
      if (recovered) {
        credential = recovered;
        upstream = await openRealtimeCall(credential, env, identity, body);
      }
    }
  }
  if (!upstream.ok) {
    return upstreamError(
      "Realtime call",
      upstream.status,
      await readBoundedResponse(upstream, 4_096),
    );
  }
  const location = upstream.headers.get("location");
  if (!location || location.length > 2_048) {
    await upstream.body?.cancel();
    return json({ error: "Realtime call response omitted its Location" }, { status: 502 });
  }
  const answer = await readRealtimeAnswer(upstream);
  if (answer instanceof Response) return answer;
  return new Response(answer, {
    headers: {
      "cache-control": "no-store",
      "content-type": "application/sdp",
      "x-nanocodex-realtime-location": location,
    },
  });
}

function validRealtimeCallId(value: string): boolean {
  return /^rtc_[A-Za-z0-9._:-]{1,196}$/.test(value) || isUuid(value);
}

function isUuid(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}

function openRealtimeCall(
  credential: SubscriptionCredential,
  env: WorkerEnv,
  identity: RealtimeIdentity,
  body: string,
): Promise<Response> {
  const endpoint = `${chatGptApiBaseUrl(env)}/realtime/calls?intent=quicksilver&architecture=avas`;
  return fetchChatGpt(env, endpoint, {
    method: "POST",
    headers: {
      ...realtimeHeaders(credential, identity),
      "content-type": "application/json",
    },
    body,
  }, credential.sessionId);
}

async function upgradeRealtimeSideband(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  if (!sameOrigin(request, url, env)) return new Response("Forbidden", { status: 403 });
  const callId = url.searchParams.get("call_id") ?? "";
  const identity = realtimeIdentity({
    openai_alpha: url.searchParams.get("openai_alpha"),
    realtime_session_id: url.searchParams.get("realtime_session_id"),
    session_id: url.searchParams.get("session_id"),
    thread_id: url.searchParams.get("thread_id"),
  });
  const managedAgentId = managedRealtimeAgentId(url.searchParams.get("managed_agent_id"), identity);
  if (!validRealtimeCallId(callId) || !identity) {
    return new Response("Invalid Realtime session", { status: 400 });
  }
  if (url.searchParams.has("managed_agent_id") && !managedAgentId) {
    return new Response("Invalid managed agent", { status: 400 });
  }
  const managed = managedAccess(request, env);
  if (managed instanceof Response) return webSocketError(managed);
  let credential: SubscriptionCredential | undefined;
  let upstreamResponse: Response;
  try {
    if (managed) {
      const limited = await limitAgentOperation(
        env,
        managedModelActorId(request, managed),
        "socket",
      );
      if (limited) return webSocketError(limited);
      upstreamResponse = await openManagedRealtimeSidebandWithRetry(
        managed,
        callId,
        identity,
        managedAgentId,
      );
    } else {
      const leaseId = randomSessionId();
      const resolved = await resolveSubscriptionCredential(request, env, "socket", leaseId);
      if (resolved instanceof Response) return webSocketError(resolved);
      credential = resolved;
      if (!credential) {
        return new Response("Voice requires an authenticated ChatGPT subscription", { status: 503 });
      }
      const limited = await limitAgentOperation(env, credential.actorId, "socket");
      if (limited) {
        await releaseSubscriptionLease(env, credential);
        return webSocketError(limited);
      }
      upstreamResponse = await openRealtimeSidebandWithRetry(credential, callId, identity);
    }
  } catch (error) {
    if (credential) await releaseSubscriptionLease(env, credential);
    const detail = error instanceof Error ? error.message : String(error);
    return new Response(`Realtime sideband upgrade request failed: ${detail}`, { status: 502 });
  }
  if (credential && upstreamResponse.status === 401) {
    await upstreamResponse.body?.cancel();
    const recovered = await recoverSubscriptionCredential(request, env, credential);
    if (recovered) {
      credential = recovered;
      upstreamResponse = await openRealtimeSidebandWithRetry(credential, callId, identity);
    }
  }
  const upstream = upstreamResponse.webSocket;
  if (!upstream) {
    const detail = await upstreamResponseDetail(upstreamResponse);
    if (credential) await releaseSubscriptionLease(env, credential);
    return new Response(
      `Realtime sideband upgrade failed with HTTP ${upstreamResponse.status}: ${detail}`,
      { status: 502 },
    );
  }

  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  bridge(server, upstream, () => {
    if (credential) void releaseSubscriptionLease(env, credential);
  });
  return new Response(null, { status: 101, webSocket: client });
}

async function openManagedRealtimeSidebandWithRetry(
  access: ManagedModelAccess,
  callId: string,
  identity: RealtimeIdentity,
  agentId?: string,
): Promise<Response> {
  let response: Response | undefined;
  for (let attempt = 0; attempt < 4; attempt += 1) {
    response = await openManagedRealtimeSideband(access, callId, identity, agentId);
    if (response.webSocket) return response;
    if (attempt < 3) {
      await response.body?.cancel();
      await new Promise((resolve) => setTimeout(resolve, 100 * 2 ** attempt));
    }
  }
  return response!;
}

async function openRealtimeSidebandWithRetry(
  credential: SubscriptionCredential,
  callId: string,
  identity: RealtimeIdentity,
): Promise<Response> {
  let response: Response | undefined;
  for (let attempt = 0; attempt < 4; attempt += 1) {
    response = await openRealtimeSideband(credential, callId, identity);
    if (response.webSocket || response.status === 401) return response;
    if (attempt < 3) {
      await response.body?.cancel();
      await new Promise((resolve) => setTimeout(resolve, 100 * 2 ** attempt));
    }
  }
  return response!;
}

function openRealtimeSideband(
  credential: SubscriptionCredential,
  callId: string,
  identity: RealtimeIdentity,
): Promise<Response> {
  const endpoint = new URL(callId, REALTIME_SIDEBAND_URL);
  return fetch(endpoint, {
    headers: {
      Upgrade: "websocket",
      ...realtimeHeaders(credential, identity),
    },
  });
}

type RealtimeIdentity = Readonly<{
  openAiAlpha: "quicksilver=v2";
  realtimeSessionId: string;
  sessionId: string;
  threadId: string;
}>;

function realtimeIdentity(value: Record<string, unknown>): RealtimeIdentity | undefined {
  const openAiAlpha = value.openai_alpha;
  const realtimeSessionId = value.realtime_session_id;
  const sessionId = value.session_id;
  const threadId = value.thread_id;
  const valid = (id: unknown): id is string =>
    typeof id === "string" && /^[A-Za-z0-9._:-]{1,200}$/.test(id);
  if (openAiAlpha !== "quicksilver=v2"
    || !valid(realtimeSessionId) || !valid(sessionId) || !valid(threadId)) return undefined;
  return { openAiAlpha, realtimeSessionId, sessionId, threadId };
}

function managedRealtimeAgentId(
  value: unknown,
  identity: RealtimeIdentity | undefined,
): string | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "string" || !isUuid(value) || !identity) return undefined;
  const voiceSessionId = identity.realtimeSessionId;
  return UUID_V7.test(voiceSessionId)
    && identity.sessionId === voiceSessionId
    && identity.threadId === voiceSessionId
    ? value
    : undefined;
}

const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function realtimeHeaders(
  credential: SubscriptionCredential,
  identity: RealtimeIdentity,
): Record<string, string> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${credential.accessToken}`,
    "ChatGPT-Account-ID": credential.accountId,
    "User-Agent": CODEX_USER_AGENT,
    "x-oai-attestation": CODEX_ATTESTATION_UNAVAILABLE,
    "openai-alpha": identity.openAiAlpha,
    "x-session-id": identity.realtimeSessionId,
    "session-id": identity.sessionId,
    "thread-id": identity.threadId,
  };
  if (credential.fedramp) headers["X-OpenAI-Fedramp"] = "true";
  return headers;
}

async function validateToolRequest(
  request: Request,
  env: WorkerEnv,
  url: URL,
  operation: Extract<ChatGptOperation, "search" | "image">,
): Promise<Credential | ManagedModelAccess | Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected JSON" }, { status: 415 });
  }
  const managed = managedAccess(request, env);
  if (managed instanceof Response) return managed;
  if (managed) return managed;
  const resolved = await resolveCredential(request, env, operation);
  if (resolved instanceof Response) return resolved;
  if (!resolved) return json({ error: "OpenAI credentials are not configured" }, { status: 503 });
  return resolved;
}

async function readJsonBody(request: Request): Promise<Record<string, unknown> | Response> {
  const body = await request.text();
  if (body.length > MAX_JSON_BODY_CHARS) return json({ error: "request body is too large" }, { status: 413 });
  try {
    const decoded = JSON.parse(body);
    return asObject(decoded) ?? json({ error: "expected a JSON object" }, { status: 400 });
  } catch {
    return json({ error: "invalid JSON" }, { status: 400 });
  }
}

function hasWebOperation(commands: Record<string, unknown>): boolean {
  return ["search_query", "image_query", "open", "click", "find", "finance", "weather", "sports", "time"]
    .some((key) => Array.isArray(commands[key]) && commands[key].length > 0);
}

function webOperationItemCount(commands: Record<string, unknown>): number {
  return ["search_query", "image_query", "open", "click", "find", "finance", "weather", "sports", "time"]
    .reduce((total, key) => total + (Array.isArray(commands[key]) ? commands[key].length : 0), 0);
}

function openAiHeaders(credential: Credential): Record<string, string> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${credential.kind === "chatgpt" ? credential.accessToken : credential.apiKey}`,
    "content-type": "application/json",
    "User-Agent": "nanocodex-web/0.1.0",
  };
  if (credential.kind === "chatgpt") {
    headers.originator = CODEX_ORIGINATOR;
    headers["User-Agent"] = CODEX_USER_AGENT;
    headers["ChatGPT-Account-ID"] = credential.accountId;
    if (credential.fedramp) headers["X-OpenAI-Fedramp"] = "true";
  }
  return headers;
}

function fetchOpenAi(
  credential: Credential,
  env: WorkerEnv,
  url: string,
  init: RequestInit,
): Promise<Response> {
  return credential.kind === "chatgpt"
    ? fetchChatGpt(env, url, init, credential.sessionId)
    : fetch(url, init);
}

function upstreamError(operation: string, status: number, body: string): Response {
  let message = body.trimStart().startsWith("<") ? `HTTP ${status}` : body.slice(0, 4_096);
  try {
    const parsed = asObject(JSON.parse(body));
    const error = asObject(parsed?.error);
    if (typeof error?.message === "string") message = error.message;
  } catch { /* Use the bounded response body. */ }
  return json({ error: `${operation} failed: ${message || `HTTP ${status}`}` }, { status: 502 });
}

function asObject(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

async function upgradeResponsesWebSocket(
  request: Request,
  env: WorkerEnv,
  url: URL,
  context?: ExecutionContext,
): Promise<Response> {
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  if (!sameOrigin(request, url, env)) {
    return new Response("Forbidden", { status: 403 });
  }
  const sessionId = url.searchParams.get("session_id");
  if (!sessionId || !/^[A-Za-z0-9._:-]{1,200}$/.test(sessionId)) {
    return new Response("Invalid session", { status: 400 });
  }
  const threadId = url.searchParams.get("thread_id") ?? sessionId;
  if (!/^[A-Za-z0-9._:-]{1,200}$/.test(threadId)) {
    return new Response("Invalid thread", { status: 400 });
  }
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  server.accept();
  const setup = setupResponsesWebSocket(request, env, sessionId, threadId, server, context);
  if (context) context.waitUntil(setup);
  else void setup;
  return new Response(null, { status: 101, webSocket: client });
}

async function setupResponsesWebSocket(
  request: Request,
  env: WorkerEnv,
  sessionId: string,
  threadId: string,
  downstream: WebSocket,
  context?: ExecutionContext,
): Promise<void> {
  let credential: Credential | undefined;
  let upstream: WebSocket | undefined;
  let upstreamAccepted = false;
  let bridged = false;
  let downstreamClosed = false;
  let leaseReleased = false;
  const onDownstreamClose = () => { downstreamClosed = true; };
  const releaseLease = async () => {
    if (!credential || leaseReleased) return;
    leaseReleased = true;
    await releaseSubscriptionLease(env, credential);
  };
  const removeSetupListeners = () => {
    downstream.removeEventListener("close", onDownstreamClose);
    downstream.removeEventListener("error", onDownstreamClose);
  };
  downstream.addEventListener("close", onDownstreamClose);
  downstream.addEventListener("error", onDownstreamClose);
  try {
    const managed = managedAccess(request, env);
    if (managed instanceof Response) {
      await rejectResponsesWebSocket(downstream, managed);
      return;
    }
    if (managed) {
      const limited = await limitAgentOperation(
        env,
        managedModelActorId(request, managed),
        "socket",
      );
      if (limited) {
        await rejectResponsesWebSocket(downstream, limited);
        return;
      }
      if (downstreamClosed) return;
      try {
        const opened = await openManagedResponsesWebSocket(managed, sessionId, threadId);
        upstream = opened.socket;
        upstreamAccepted = true;
      } catch (error) {
        await rejectResponsesWebSocket(downstream, managedBrokerError(error));
        return;
      }
    } else {
      const leaseId = randomSessionId();
      const resolved = await resolveCredential(request, env, "socket", leaseId);
      if (resolved instanceof Response) {
        await rejectResponsesWebSocket(downstream, resolved);
        return;
      }
      credential = resolved;
      if (!credential) {
        await rejectResponsesWebSocket(
          downstream,
          new Response("OpenAI credentials are not configured", { status: 503 }),
        );
        return;
      }
      if (downstreamClosed) return;
      const limited = await limitAgentOperation(env, credential.actorId, "socket");
      if (limited) {
        await rejectResponsesWebSocket(downstream, limited);
        return;
      }
      if (downstreamClosed) return;

      let upstreamResponse = await openResponsesWebSocket(
        env,
        credential,
        sessionId,
        threadId,
        chatGptApiBaseUrl(env),
      );
      if (credential.kind === "chatgpt" && upstreamResponse.status === 401) {
        const recovered = await recoverSubscriptionCredential(request, env, credential);
        if (recovered) {
          await upstreamResponse.body?.cancel();
          credential = recovered;
          upstreamResponse = await openResponsesWebSocket(
            env,
            credential,
            sessionId,
            threadId,
            chatGptApiBaseUrl(env),
          );
        }
      }
      upstream = upstreamResponse.webSocket ?? undefined;
      if (!upstream) {
        console.error("OpenAI WebSocket upgrade rejected", { status: upstreamResponse.status });
        await rejectResponsesWebSocket(downstream, upstreamResponse);
        return;
      }
    }
    upstream.binaryType = "arraybuffer";
    if (!upstreamAccepted) upstream.accept();
    if (downstreamClosed) return;
    downstream.send(JSON.stringify({ type: "nanocodex.proxy.ready" }));
    if (downstreamClosed) return;
    bridge(downstream, upstream, () => {
      const release = releaseLease();
      if (context) context.waitUntil(release);
      else void release;
    });
    bridged = true;
  } catch (error) {
    const name = error instanceof Error ? error.name : typeof error;
    console.error("OpenAI WebSocket setup failed", { name });
    if (!downstreamClosed) {
      await rejectResponsesWebSocket(
        downstream,
        new Response("OpenAI WebSocket setup failed", { status: 502 }),
      );
    }
  } finally {
    removeSetupListeners();
    if (!bridged) {
      try { upstream?.close(1000, "proxy setup ended"); } catch { /* Already closed. */ }
      await releaseLease();
    }
  }
}

function managedBrokerError(error: unknown): Response {
  const rejected = error as { body?: unknown; retryAfter?: unknown; status?: unknown };
  const status = Number.isInteger(rejected?.status)
    && Number(rejected.status) >= 400
    && Number(rejected.status) <= 599
    ? Number(rejected.status)
    : 502;
  const code = typeof rejected?.body === "string"
    && /^[a-z0-9_]{1,80}$/.test(rejected.body)
    ? rejected.body
    : "credential_broker_rejected";
  const retryAfter = Number(rejected?.retryAfter);
  return json(
    { error: code },
    {
      status,
      ...(Number.isFinite(retryAfter) && retryAfter >= 0
        ? { headers: { "retry-after": String(retryAfter) } }
        : {}),
    },
  );
}

async function rejectResponsesWebSocket(socket: WebSocket, response: Response): Promise<void> {
  const status = response.status;
  const error = await upstreamResponseDetail(response);
  const retryAfter = response.headers.get("retry-after");
  try {
    socket.send(JSON.stringify({
      type: "nanocodex.proxy.rejected",
      status,
      error,
      ...(retryAfter === null ? {} : { retryAfter }),
    }));
    socket.close(status === 429 ? 1013 : 1011, "connection rejected");
  } catch { /* The browser may have gone away while the upstream was opening. */ }
}

async function upstreamResponseDetail(response: Response): Promise<string> {
  const body = await readBoundedResponse(response, 4_096);
  try {
    const parsed = asObject(JSON.parse(body));
    const error = asObject(parsed?.error);
    if (typeof error?.message === "string") return error.message.slice(0, 1_024);
    if (typeof parsed?.error === "string") return parsed.error.slice(0, 1_024);
    if (typeof parsed?.detail === "string") return parsed.detail.slice(0, 1_024);
  } catch { /* Fall through to the bounded text classification. */ }
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (contentType.startsWith("text/plain") && !body.trimStart().startsWith("<")) {
    return body.slice(0, 1_024);
  }
  return `HTTP ${response.status}`;
}

async function readBoundedResponse(response: Response, limit: number): Promise<string> {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let body = "";
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) return body + decoder.decode();
    total += value.byteLength;
    if (total > limit) {
      const remaining = Math.max(0, limit - (total - value.byteLength));
      body += decoder.decode(value.subarray(0, remaining));
      await reader.cancel();
      return `${body}…`;
    }
    body += decoder.decode(value, { stream: true });
  }
}

async function readRealtimeAnswer(response: Response): Promise<string | Response> {
  if (!response.body) return json({ error: "Realtime call returned an empty SDP answer" }, { status: 502 });
  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  const chunks: string[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        chunks.push(decoder.decode());
        break;
      }
      total += value.byteLength;
      if (total > MAX_REALTIME_SDP_CHARS) {
        await reader.cancel();
        return json({ error: "Realtime answer exceeded 1 MiB" }, { status: 502 });
      }
      chunks.push(decoder.decode(value, { stream: true }));
    }
  } catch {
    await reader.cancel().catch(() => {});
    return json({ error: "Realtime call returned invalid UTF-8 SDP" }, { status: 502 });
  }
  const answer = chunks.join("");
  return answer.trim().length > 0
    ? answer
    : json({ error: "Realtime call returned an empty SDP answer" }, { status: 502 });
}

function openResponsesWebSocket(
  env: WorkerEnv,
  credential: Credential,
  sessionId: string,
  threadId: string,
  chatGptBaseUrl: string,
): Promise<Response> {
  const headers: Record<string, string> = {
    Upgrade: "websocket",
    Authorization: `Bearer ${credential.kind === "chatgpt" ? credential.accessToken : credential.apiKey}`,
    "OpenAI-Beta": RESPONSES_WEBSOCKETS_BETA,
    "x-openai-internal-codex-responses-lite": "true",
    "session-id": sessionId,
    "thread-id": threadId,
    "x-client-request-id": threadId,
    "x-responsesapi-include-timing-metrics": "true",
    originator: CODEX_ORIGINATOR,
    "User-Agent": CODEX_USER_AGENT,
  };
  if (credential.kind === "chatgpt") {
    headers["ChatGPT-Account-ID"] = credential.accountId;
    if (credential.fedramp) headers["X-OpenAI-Fedramp"] = "true";
  }
  return credential.kind === "chatgpt"
    ? fetchChatGpt(env, `${chatGptBaseUrl}/responses`, { headers }, credential.sessionId)
    : fetch(RESPONSES_UPGRADE_URL, { headers });
}

function chatGptApiBaseUrl(_env: WorkerEnv): string {
  return CHATGPT_API_BASE_URL;
}

async function startChatGptSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  if (!env.CHATGPT_SESSIONS) {
    return json({ error: "ChatGPT subscription login is not configured" }, { status: 503 });
  }
  const limited = await limitLoginStart(request, env);
  if (limited) return limited;
  await deleteChatGptSession(request, env);
  const sessionId = randomSessionId();
  const response = await chatGptStub(env, sessionId).fetch("https://chatgpt.internal/start", {
    method: "POST",
  });
  return new Response(response.body, {
    status: response.status,
    headers: response.ok
      ? responseHeaders(response, {
          "set-cookie": chatGptSessionCookie(sessionId, url, CHATGPT_LOGIN_TTL_MS),
        })
      : responseHeaders(response),
  });
}

async function chatGptSessionStatus(
  request: Request,
  env: WorkerEnv,
  context?: ExecutionContext,
): Promise<Response> {
  if (!env.CHATGPT_SESSIONS) {
    return json({ error: "ChatGPT subscription login is not configured" }, { status: 503 });
  }
  const sessionId = chatGptSessionIdFromRequest(request);
  if (!sessionId) return json({ state: "signed_out" });
  const limited = await limitSessionPoll(env, sessionId);
  if (limited) return limited;
  const response = await chatGptStub(env, sessionId).fetch("https://chatgpt.internal/status");
  const body = await response.text();
  const state = response.ok ? parseState(body) : undefined;
  const extra: Record<string, string> = {};
  if (state === "authenticated") {
    extra["set-cookie"] = chatGptSessionCookie(sessionId, new URL(request.url), CHATGPT_SESSION_TTL_MS);
    if (context) context.waitUntil(warmChatGptEgress(env, sessionId));
  }
  return new Response(body, {
    status: response.status,
    headers: responseHeaders(response, extra),
  });
}

async function clearChatGptSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  await deleteChatGptSession(request, env);
  return json({ state: "signed_out" }, {
    headers: { "set-cookie": clearChatGptSessionCookie(url) },
  });
}

function responseHeaders(response: Response, extra?: Record<string, string>): Headers {
  const headers = new Headers({
    "cache-control": "no-store",
    "content-type": response.headers.get("content-type") ?? "application/json",
    "x-content-type-options": "nosniff",
  });
  for (const [name, value] of Object.entries(extra ?? {})) headers.set(name, value);
  return headers;
}

function parseState(body: string): string | undefined {
  try {
    const value = asObject(JSON.parse(body));
    return typeof value?.state === "string" ? value.state : undefined;
  } catch {
    return undefined;
  }
}

function webSocketError(response: Response): Response {
  return response;
}

async function createByokSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  if (!env.BYOK_SESSIONS) return json({ error: "BYOK sessions are not configured" }, { status: 503 });
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected JSON" }, { status: 415 });
  }
  const body = await request.text();
  if (body.length > 4_096) return json({ error: "request body is too large" }, { status: 413 });
  let apiKey: unknown;
  try {
    apiKey = asObject(JSON.parse(body))?.api_key;
  } catch {
    return json({ error: "invalid JSON" }, { status: 400 });
  }
  const normalizedApiKey = typeof apiKey === "string" ? apiKey.trim() : "";
  if (!normalizedApiKey || normalizedApiKey.length > MAX_API_KEY_CHARS) {
    return json({ error: "api_key must be a non-empty string of at most 1024 characters" }, { status: 400 });
  }

  const sessionId = randomSessionId();
  const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
  const stored = await stub.fetch("https://byok.internal/credential", {
    method: "PUT",
    body: normalizedApiKey,
  });
  if (!stored.ok) return json({ error: "failed to create BYOK session" }, { status: 503 });
  await deleteSession(request, env);
  return json(
    { agent_configured: true, credential_source: "user", expires_in: BYOK_SESSION_TTL_MS / 1_000 },
    { headers: { "set-cookie": sessionCookie(sessionId, url) } },
  );
}

async function clearByokSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url, env)) return json({ error: "forbidden" }, { status: 403 });
  await deleteSession(request, env);
  return json(
    { agent_configured: false, credential_source: null },
    { headers: { "set-cookie": clearSessionCookie(url) } },
  );
}

async function resolveCredential(
  request: Request,
  env: WorkerEnv,
  operation: ChatGptOperation,
  leaseId?: string,
): Promise<Credential | Response | undefined> {
  const subscription = await resolveSubscriptionCredential(request, env, operation, leaseId);
  if (subscription) return subscription;
  const sessionId = sessionIdFromRequest(request);
  if (sessionId) {
    if (!env.BYOK_SESSIONS) {
      return json({ error: "BYOK session storage is unavailable" }, { status: 503 });
    }
    try {
      const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
      const response = await stub.fetch("https://byok.internal/credential");
      if (response.ok) {
        const apiKey = await response.text();
        if (apiKey) {
          return {
            kind: "api_key",
            apiKey,
            actorId: await apiKeyActorId(apiKey),
            source: "user",
          };
        }
        return json({ error: "BYOK session credential is unavailable" }, { status: 503 });
      }
      if (response.status !== 404 && response.status !== 401) {
        await response.body?.cancel();
        return json({ error: "BYOK session lookup failed" }, { status: 503 });
      }
      await response.body?.cancel();
    } catch {
      return json({ error: "BYOK session lookup failed" }, { status: 503 });
    }
  }
  return undefined;
}

async function resolveSubscriptionCredential(
  request: Request,
  env: WorkerEnv,
  operation: ChatGptOperation,
  leaseId?: string,
): Promise<SubscriptionCredential | Response | undefined> {
  const sessionId = chatGptSessionIdFromRequest(request);
  if (!sessionId) return undefined;
  if (!env.CHATGPT_SESSIONS) {
    return json({ error: "ChatGPT session storage is unavailable" }, { status: 503 });
  }
  try {
    const response = await chatGptStub(env, sessionId).fetch("https://chatgpt.internal/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation, ...(leaseId ? { leaseId } : {}) }),
    });
    if (!response.ok) {
      if (response.status === 429) {
        return new Response(response.body, {
          status: 429,
          headers: {
            "cache-control": "no-store",
            "content-type": "application/json",
            "retry-after": response.headers.get("retry-after") ?? "60",
          },
        });
      }
      if (response.status !== 404 && response.status !== 401) {
        await response.body?.cancel();
        return json({ error: "ChatGPT session lookup failed" }, { status: 503 });
      }
      await response.body?.cancel();
      return undefined;
    }
    const credential = await response.json<ChatGptCredential>();
    if (!isChatGptCredential(credential)) {
      return json({ error: "ChatGPT session credential is invalid" }, { status: 503 });
    }
    return {
      ...credential,
      actorId: `chatgpt:${credential.accountId}`,
      sessionId,
      ...(leaseId ? { leaseId } : {}),
      source: "subscription",
    };
  } catch {
    return json({ error: "ChatGPT session lookup failed" }, { status: 503 });
  }
}

async function recoverSubscriptionCredential(
  request: Request,
  env: WorkerEnv,
  previous: SubscriptionCredential,
): Promise<SubscriptionCredential | undefined> {
  const sessionId = chatGptSessionIdFromRequest(request);
  if (!sessionId || !env.CHATGPT_SESSIONS) return undefined;
  try {
    const response = await chatGptStub(env, sessionId).fetch("https://chatgpt.internal/recover", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ revision: previous.revision }),
    });
    if (!response.ok) {
      await response.body?.cancel();
      return undefined;
    }
    const credential = await response.json<ChatGptCredential>();
    return isChatGptCredential(credential)
      ? {
          ...credential,
          actorId: previous.actorId,
          sessionId: previous.sessionId,
          ...(previous.leaseId ? { leaseId: previous.leaseId } : {}),
          source: "subscription",
        }
      : undefined;
  } catch {
    return undefined;
  }
}

async function releaseSubscriptionLease(
  env: WorkerEnv,
  credential: Credential,
): Promise<void> {
  if (credential.kind !== "chatgpt" || !credential.leaseId || !env.CHATGPT_SESSIONS) return;
  try {
    const response = await chatGptStub(env, credential.sessionId).fetch(
      "https://chatgpt.internal/lease",
      {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ leaseId: credential.leaseId }),
      },
    );
    await response.body?.cancel();
  } catch { /* The lease expires automatically if cleanup cannot be delivered. */ }
}

function isChatGptCredential(value: unknown): value is ChatGptCredential {
  const credential = asObject(value);
  return credential?.kind === "chatgpt"
    && typeof credential.accessToken === "string"
    && credential.accessToken.length > 0
    && typeof credential.accountId === "string"
    && credential.accountId.length > 0
    && typeof credential.fedramp === "boolean"
    && typeof credential.revision === "string"
    && /^(0|[1-9][0-9]*)$/.test(credential.revision);
}

async function deleteSession(request: Request, env: WorkerEnv): Promise<void> {
  const sessionId = sessionIdFromRequest(request);
  if (!sessionId || !env.BYOK_SESSIONS) return;
  const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
  await stub.fetch("https://byok.internal/credential", { method: "DELETE" });
}

function sessionIdFromRequest(request: Request): string | undefined {
  return cookieSessionId(request, [SECURE_BYOK_COOKIE, BYOK_COOKIE]);
}

function chatGptSessionIdFromRequest(request: Request): string | undefined {
  return cookieSessionId(request, [SECURE_CHATGPT_COOKIE, CHATGPT_COOKIE]);
}

function cookieSessionId(request: Request, cookieNames: readonly string[]): string | undefined {
  const cookie = request.headers.get("cookie");
  if (!cookie) return undefined;
  for (const part of cookie.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (!cookieNames.includes(name ?? "")) continue;
    const value = rest.join("=");
    if (/^[A-Za-z0-9_-]{43}$/.test(value)) return value;
  }
  return undefined;
}

function randomSessionId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function sessionCookie(sessionId: string, url: URL): string {
  const secure = url.protocol === "https:";
  const name = secure ? SECURE_BYOK_COOKIE : BYOK_COOKIE;
  return `${name}=${sessionId}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=${BYOK_SESSION_TTL_MS / 1_000}${secure ? "; Secure" : ""}`;
}

function clearSessionCookie(url: URL): string {
  const secure = url.protocol === "https:";
  const name = secure ? SECURE_BYOK_COOKIE : BYOK_COOKIE;
  return `${name}=; Path=/api; HttpOnly; SameSite=Strict; Max-Age=0${secure ? "; Secure" : ""}`;
}

function chatGptSessionCookie(sessionId: string, url: URL, ttlMs: number): string {
  const secure = url.protocol === "https:";
  const name = secure ? SECURE_CHATGPT_COOKIE : CHATGPT_COOKIE;
  return `${name}=${sessionId}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=${ttlMs / 1_000}${secure ? "; Secure" : ""}`;
}

function clearChatGptSessionCookie(url: URL): string {
  const secure = url.protocol === "https:";
  const name = secure ? SECURE_CHATGPT_COOKIE : CHATGPT_COOKIE;
  return `${name}=; Path=/api; HttpOnly; SameSite=Strict; Max-Age=0${secure ? "; Secure" : ""}`;
}

function chatGptStub(env: WorkerEnv, sessionId: string): DurableObjectStub {
  if (!env.CHATGPT_SESSIONS) throw new Error("ChatGPT subscription login is not configured");
  return env.CHATGPT_SESSIONS.get(env.CHATGPT_SESSIONS.idFromName(sessionId));
}

async function deleteChatGptSession(request: Request, env: WorkerEnv): Promise<void> {
  const sessionId = chatGptSessionIdFromRequest(request);
  if (!sessionId || !env.CHATGPT_SESSIONS) return;
  await chatGptStub(env, sessionId).fetch("https://chatgpt.internal/session", { method: "DELETE" });
}

function sameOrigin(request: Request, url: URL, env: WorkerEnv): boolean {
  if (
    request.headers.get("x-nanocodex-request") === "1" &&
    request.headers.get("sec-fetch-site") === "same-origin"
  ) return true;
  const origin = request.headers.get("Origin");
  if (origin) return matchesRequestOrigin(origin, url);
  const referer = request.headers.get("Referer");
  return referer !== null
    && matchesRequestOrigin(referer, url);
}

function matchesRequestOrigin(value: string, url: URL): boolean {
  try {
    const source = new URL(value);
    return source.origin === url.origin;
  } catch {
    return false;
  }
}

function bridge(
  left: WebSocket,
  right: WebSocket,
  onClose: () => void,
): void {
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    onClose();
  };
  forward(left, right, close);
  forward(right, left, close);
}

function forward(
  source: WebSocket,
  destination: WebSocket,
  onClose: () => void,
): void {
  source.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      closeSocket(source, 1003, "text frames required");
      closeSocket(destination, 1003, "text frames required");
      return;
    }
    if (event.data.length > MAX_WEBSOCKET_MESSAGE_CHARS) {
      closeSocket(source, 1009, "message too large");
      closeSocket(destination, 1009, "message too large");
      return;
    }
    if (destination.readyState === WebSocket.OPEN) destination.send(event.data);
  });
  source.addEventListener("close", (event) => {
    onClose();
    closeSocket(destination, event.code, event.reason || "peer closed");
  });
  source.addEventListener("error", () => {
    onClose();
    closeSocket(destination, 1011, "peer WebSocket failed");
  });
}

function closeSocket(socket: WebSocket, code: number, reason: string): void {
  if (socket.readyState !== WebSocket.CONNECTING && socket.readyState !== WebSocket.OPEN) return;
  const safeCode = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1011;
  socket.close(safeCode, reason.slice(0, 120));
}

export class ByokSession {
  readonly #state: DurableObjectState;
  readonly #vault: CredentialVault;

  constructor(state: DurableObjectState, env: CredentialVaultEnv) {
    this.#state = state;
    this.#vault = new CredentialVault(env, `byok/${state.id?.toString() ?? "test"}`);
  }

  async fetch(request: Request): Promise<Response> {
    if (request.method === "PUT") {
      const apiKey = await request.text();
      if (!apiKey || apiKey.length > MAX_API_KEY_CHARS) return new Response(null, { status: 400 });
      const credential: StoredCredential = {
        apiKey,
        expiresAt: Date.now() + BYOK_SESSION_TTL_MS,
      };
      await this.#state.storage.put("credential", await this.#vault.seal(credential));
      await this.#state.storage.setAlarm(credential.expiresAt);
      return new Response(null, { status: 204 });
    }
    if (request.method === "DELETE") {
      await this.#state.storage.deleteAll();
      return new Response(null, { status: 204 });
    }
    const envelope = await this.#state.storage.get<EncryptedEnvelope>("credential");
    const opened = envelope ? await this.#vault.open<StoredCredential>(envelope) : undefined;
    const credential = opened?.value;
    if (!credential || credential.expiresAt <= Date.now()) {
      if (envelope) await this.#state.storage.deleteAll();
      return new Response(null, { status: 404 });
    }
    if (opened.reseal) {
      await this.#state.storage.put("credential", await this.#vault.seal(credential));
    }
    return new Response(credential.apiKey, {
      headers: { "cache-control": "no-store", "content-type": "text/plain" },
    });
  }

  async alarm(): Promise<void> {
    await this.#state.storage.deleteAll();
  }
}
