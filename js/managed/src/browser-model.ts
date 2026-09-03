import {
  authenticatePersistentAccount,
  forwardPrincipalAssertions,
  type AccountAuthEnv,
  type Principal,
} from "./account-auth";
import { bindAgentCredential, browserModelSubject } from "./credentials";

const MODEL_HOST = "nanocodex.internal";
const STATUS_HOST = "broker.internal";
const STATUS_PATH = "/.well-known/nanocodex/model-status";
const SPONSORED_TRIAL_RESET_PATH = "/.well-known/nanocodex/sponsored-trial-reset";
const MODEL_PATHS = new Set([
  "/v1/responses",
  "/v1/search",
  "/v1/images/generations",
  "/v1/images/edits",
  "/v1/realtime/calls",
  "/v1/realtime/sideband",
]);

type BrowserModelEnv = AccountAuthEnv & {
  NANOCODEX: Fetcher;
  NANOCODEX_SESSIONS: {
    idFromName(name: string): DurableObjectId;
    get(id: DurableObjectId): Fetcher;
  };
};

/**
 * Authenticates browser-owned model traffic inside the private managed Worker,
 * binds one opaque broker subject to the account during the readiness check,
 * self-heals a missing binding once, and forwards no account cookie beyond
 * this boundary.
 */
export async function routeBrowserModel(
  request: Request,
  env: BrowserModelEnv,
  url: URL,
): Promise<Response | undefined> {
  const status = url.protocol === "https:" && url.hostname === STATUS_HOST
    && !url.port && !url.search && !url.hash && url.pathname === STATUS_PATH
    && request.method === "GET";
  const resetSponsoredTrial = env.ENVIRONMENT?.trim().toLowerCase() === "development"
    && url.protocol === "https:" && url.hostname === STATUS_HOST
    && !url.port && !url.search && !url.hash && url.pathname === SPONSORED_TRIAL_RESET_PATH
    && request.method === "POST";
  const model = url.protocol === "https:" && url.hostname === MODEL_HOST
    && !url.port && !url.search && !url.hash && MODEL_PATHS.has(url.pathname);
  if (!status && !resetSponsoredTrial && !model) return undefined;

  const authenticationHeaders = new Headers(request.headers);
  authenticationHeaders.delete("authorization");
  authenticationHeaders.delete("x-nanocodex-subject");
  const principal = await authenticatePersistentAccount(
    new Request(request.url, {
      method: request.method,
      headers: authenticationHeaders,
      signal: request.signal,
    }),
    env,
    url,
  );
  if (!principal || principal.kind !== "account_session") {
    return Response.json({ error: "unauthorized" }, {
      status: 401,
      headers: { "cache-control": "no-store" },
    });
  }
  const realtimeSubject = model && url.pathname.startsWith("/v1/realtime/")
    ? await ownedRealtimeSubject(request, env, principal)
    : undefined;
  if (realtimeSubject instanceof Response) return realtimeSubject;
  const subject = realtimeSubject ?? await browserModelSubject(principal.userId);
  const headers = new Headers(request.headers);
  headers.delete("cookie");
  headers.delete("x-nanocodex-agent-id");
  headers.set("x-nanocodex-subject", subject);

  if (status || resetSponsoredTrial) {
    const bindingFailure = await bindBrowserModelSubject(env, subject, principal.userId);
    if (bindingFailure) return bindingFailure;
    return env.NANOCODEX.fetch(new Request(request, { headers }));
  }

  const retryRequest = request.clone();
  let response: Response;
  try {
    response = await env.NANOCODEX.fetch(new Request(request, { headers }));
  } catch (error) {
    await retryRequest.body?.cancel().catch(() => {});
    throw error;
  }
  if (!await agentSubjectUnavailable(response)) {
    await retryRequest.body?.cancel().catch(() => {});
    return response;
  }
  await response.body?.cancel().catch(() => {});

  const bindingFailure = await bindBrowserModelSubject(env, subject, principal.userId);
  if (bindingFailure) {
    await retryRequest.body?.cancel().catch(() => {});
    return bindingFailure;
  }
  return env.NANOCODEX.fetch(new Request(retryRequest, { headers }));
}

async function bindBrowserModelSubject(
  env: BrowserModelEnv,
  subject: string,
  userId: string,
): Promise<Response | undefined> {
  try {
    await bindAgentCredential(env.NANOCODEX, subject, userId);
    return undefined;
  } catch {
    return Response.json({ error: "credential_broker_unavailable" }, {
      status: 503,
      headers: { "cache-control": "no-store" },
    });
  }
}

async function agentSubjectUnavailable(response: Response): Promise<boolean> {
  if (response.status !== 403) return false;
  try {
    const body = await response.clone().json<{ error?: unknown }>();
    return body.error === "agent_subject_unavailable";
  } catch {
    return false;
  }
}

async function ownedRealtimeSubject(
  request: Request,
  env: BrowserModelEnv,
  principal: Principal,
): Promise<string | Response | undefined> {
  const agentId = request.headers.get("x-nanocodex-agent-id");
  if (agentId === null) return undefined;
  const identities = [
    request.headers.get("x-session-id"),
    request.headers.get("session-id"),
    request.headers.get("thread-id"),
  ];
  const voiceSessionId = identities[0];
  if (!UUID.test(agentId)
    || !voiceSessionId
    || !UUID_V7.test(voiceSessionId)
    || identities.some((identity) => identity !== voiceSessionId)) {
    return Response.json({ error: "not_found" }, { status: 404 });
  }
  const durableId = env.NANOCODEX_SESSIONS.idFromName(agentId);
  const ownershipHeaders = new Headers();
  forwardPrincipalAssertions(ownershipHeaders, principal);
  const owned = await env.NANOCODEX_SESSIONS.get(durableId).fetch("https://session.internal/state", {
    headers: ownershipHeaders,
  });
  await owned.body?.cancel();
  if (!owned.ok) return Response.json({ error: "not_found" }, { status: 404 });
  return durableId.toString();
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
