import {
  authenticate,
  forwardPrincipalAssertions,
  requireSameOriginMutation,
  type AccountAuthEnv,
} from "./account-auth";
import { bindAgentCredential } from "./credentials";
import { readSessionCredentialSubject, validateSessionCredentialSubject } from "./session-credential-ownership";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";

const AGENT_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[78][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const VOICE_SESSION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const CALL_ID = /^(?:rtc_[A-Za-z0-9._:-]{1,196}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i;
const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const REALTIME_MODEL = "gpt-live-1-codex";
const REALTIME_VOICES = new Set([
  "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove",
]);

type ManagedRealtimeTransportEnv = AccountAuthEnv & {
  NANOCODEX: Fetcher;
  NANOCODEX_REALTIME?: Fetcher & {
    createCall?(body: string, headers: Record<string, string>): Promise<{
      status: number; headers: Record<string, string>; body: string;
    }>;
  };
  NANOCODEX_SESSIONS: {
    get(id: DurableObjectId): Fetcher & {
      resolveCredentialSubject?(assertions: Record<string, string>, traceId?: string): Promise<unknown>;
    };
    idFromName(name: string): DurableObjectId;
  };
};

/** Public, credential-free media transport for one account-owned managed Agent. */
export async function routeManagedRealtimeTransport(
  request: Request,
  env: ManagedRealtimeTransportEnv,
  url: URL,
  ownershipTimeoutMs: number,
): Promise<Response | undefined> {
  const match = url.pathname.match(/^\/v1\/agents\/([^/]+)\/realtime\/(calls|sideband)$/);
  if (!match) return undefined;
  const agentId = match[1]!;
  const resource = match[2]!;
  if (!AGENT_ID.test(agentId)) return json({ error: "not_found" }, 404);

  const expectedMethod = resource === "calls" ? "POST" : "GET";
  if (request.method !== expectedMethod) return json({ error: "method_not_allowed" }, 405);
  const began = performance.now();
  const principal = await authenticate(request, env, url);
  const authenticated = performance.now();
  if (!principal) return json({ error: "unauthorized" }, 401);
  if (resource === "calls") {
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
  } else if (principal.kind === "account_session"
    && request.headers.get("origin") !== url.origin) {
    return json({ error: "forbidden_origin" }, 403);
  }

  let callBody: string | undefined;
  let callId: string | undefined;
  let voiceSessionId: string | undefined;
  if (resource === "calls") {
    const validated = await validatedCallBody(request, url);
    if (validated instanceof Response) return validated;
    callBody = validated;
    voiceSessionId = request.headers.get("x-nanocodex-voice-session-id") ?? undefined;
  } else {
    const validated = validatedSideband(request, url);
    if (validated instanceof Response) return validated;
    ({ callId, voiceSessionId } = validated);
  }
  const validatedAt = performance.now();
  const durableId = env.NANOCODEX_SESSIONS.idFromName(agentId);
  const ownershipHeaders = new Headers();
  forwardPrincipalAssertions(ownershipHeaders, principal);
  let owned: Awaited<ReturnType<typeof readSessionCredentialSubject>>;
  try {
    const stub = env.NANOCODEX_SESSIONS.get(durableId);
    owned = typeof stub.resolveCredentialSubject === "function"
      ? await withHardDeadline("managed Realtime ownership assertion", ownershipTimeoutMs,
        async () => validateSessionCredentialSubject(
          await stub.resolveCredentialSubject!(Object.fromEntries(ownershipHeaders), voiceSessionId), durableId.toString(),
        ))
      : await fetchResponseWithDeadline(
        stub,
        "https://session.internal/credential-subject",
        { headers: ownershipHeaders },
        ownershipTimeoutMs,
        "managed Realtime ownership assertion",
        (response) => readSessionCredentialSubject(response, durableId.toString()),
      );
  } catch {
    return json({ error: "agent_ownership_unavailable" }, 503);
  }
  const authorized = performance.now();
  if (!owned) return json({ error: "not_found" }, 404);
  const { subject, direct } = owned;
  if (!voiceSessionId || !VOICE_SESSION_ID.test(voiceSessionId)) {
    return json({ error: "invalid_voice_session" }, 400);
  }

  try {
    // Private call egress uses the Session's just-verified owner for either
    // retained strategy. Legacy sidebands and older deployments still repair
    // their directory mapping before the generic broker resolves it.
    if (!direct && (resource !== "calls" || !env.NANOCODEX_REALTIME)) {
      await bindAgentCredential(env.NANOCODEX, subject, principal.userId, ownershipTimeoutMs);
    }
  } catch {
    return json({ error: "credential_broker_unavailable" }, 503);
  }

  if (resource === "calls") {
    const response = await realtimeCall(callBody!, env, agentId, voiceSessionId, subject, voiceRelayRegion(request), principal.userId, owned.accountId);
    response.headers.append("server-timing", [
      `voice_auth;dur=${(authenticated - began).toFixed(1)}`,
      `voice_validate;dur=${(validatedAt - authenticated).toFixed(1)}`,
      `voice_ownership;dur=${(authorized - validatedAt).toFixed(1)}`,
      `voice_egress;dur=${(performance.now() - authorized).toFixed(1)}`,
    ].join(", "));
    return response;
  }
  return realtimeSideband(callId!, env, agentId, voiceSessionId, subject, owned.accountId);
}

async function validatedCallBody(request: Request, url: URL): Promise<string | Response> {
  if (url.search) return json({ error: "invalid_request" }, 400);
  if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase()
    !== "application/json") {
    return json({ error: "invalid_content_type" }, 415);
  }
  const body = await request.text();
  let decoded: unknown;
  try { decoded = JSON.parse(body); }
  catch { return json({ error: "invalid_request" }, 400); }
  if (!isRecord(decoded)
    || !exactKeys(decoded, ["sdp", "session"])
    || typeof decoded.sdp !== "string"
    || !decoded.sdp.trim()
    || !validRealtimeSession(decoded.session)) {
    return json({ error: "invalid_request" }, 400);
  }
  return body;
}

async function realtimeCall(
  body: string,
  env: ManagedRealtimeTransportEnv,
  agentId: string,
  voiceSessionId: string,
  subject: string,
  region: string | undefined,
  verifiedOwner: string | undefined,
  accountId?: string,
): Promise<Response> {
  const headers = internalHeaders(agentId, voiceSessionId, subject, false);
  if (accountId) headers.set("x-nanocodex-chatgpt-account-id", accountId);
  if (region) headers.set("x-nanocodex-voice-region", region);
  const binding = verifiedOwner && env.NANOCODEX_REALTIME ? env.NANOCODEX_REALTIME : env.NANOCODEX;
  if (binding === env.NANOCODEX_REALTIME) headers.set("x-nanocodex-realtime-owner", verifiedOwner!);
  const privateBinding = env.NANOCODEX_REALTIME;
  let response: Response;
  if (binding === privateBinding && typeof privateBinding?.createCall === "function") {
    const reply = await privateBinding.createCall(body, Object.fromEntries(headers));
    response = new Response(reply.body, { status: reply.status, headers: reply.headers });
  } else {
    response = await binding.fetch(new Request("https://nanocodex.internal/v1/realtime/calls", {
      method: "POST", headers, body,
    }));
  }
  const responseHeaders = sanitizedHeaders(response.headers);
  const location = responseHeaders.get("location");
  if (location) {
    responseHeaders.set("x-nanocodex-realtime-location", location);
    responseHeaders.delete("location");
  }
  responseHeaders.set("cache-control", "no-store");
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
}

function validatedSideband(
  request: Request,
  url: URL,
): { callId: string; voiceSessionId?: string } | Response {
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  const keys = [...url.searchParams.keys()];
  const callIds = url.searchParams.getAll("call_id");
  const voiceSessionIds = url.searchParams.getAll("voice_session_id");
  if (keys.some((key) => key !== "call_id" && key !== "voice_session_id")
    || callIds.length !== 1
    || voiceSessionIds.length > 1
    || !CALL_ID.test(callIds[0]!)) {
    return json({ error: "invalid_request" }, 400);
  }
  return {
    callId: callIds[0]!,
    ...(voiceSessionIds[0] === undefined ? {} : { voiceSessionId: voiceSessionIds[0] }),
  };
}

function realtimeSideband(
  callId: string,
  env: ManagedRealtimeTransportEnv,
  agentId: string,
  voiceSessionId: string,
  subject: string,
  accountId?: string,
): Promise<Response> {
  const headers = internalHeaders(agentId, voiceSessionId, subject, true, callId);
  if (accountId) headers.set("x-nanocodex-chatgpt-account-id", accountId);
  // Return the binding response itself: reconstructing a 101 Response severs
  // Cloudflare's upgraded WebSocket from its provider peer.
  return env.NANOCODEX.fetch(new Request(
    "https://nanocodex.internal/v1/realtime/sideband",
    { headers },
  ));
}

function internalHeaders(
  agentId: string,
  voiceSessionId: string,
  subject: string,
  websocket: boolean,
  callId?: string,
): Headers {
  const headers = new Headers({
    authorization: PROVIDER_PLACEHOLDER,
    "openai-alpha": "quicksilver=v2",
    "session-id": voiceSessionId,
    "thread-id": voiceSessionId,
    "user-agent": "nanocodex-managed/0.1.0",
    "x-nanocodex-agent-id": agentId,
    "x-nanocodex-subject": subject,
    "x-session-id": voiceSessionId,
  });
  if (websocket) {
    headers.set("upgrade", "websocket");
    headers.set("x-nanocodex-realtime-call-id", callId!);
  } else {
    headers.set("content-type", "application/json");
  }
  return headers;
}

function sanitizedHeaders(source: Headers): Headers {
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function validRealtimeSession(value: unknown): boolean {
  if (!isRecord(value)
    || !exactKeys(value, ["audio", "delegation", "instructions", "model"])
    || value.model !== REALTIME_MODEL
    || typeof value.instructions !== "string"
    || !value.instructions
    || !isRecord(value.delegation)
    || !(exactKeys(value.delegation, ["type"]) || (exactKeys(value.delegation, ["type", "ack_filler"])
      && typeof value.delegation.ack_filler === "boolean"))
    || value.delegation.type !== "client"
    || !isRecord(value.audio)
    || !exactKeys(value.audio, ["output"])
    || !isRecord(value.audio.output)
    || !exactKeys(value.audio.output, ["voice"])
    || typeof value.audio.output.voice !== "string"
    || !REALTIME_VOICES.has(value.audio.output.voice)) return false;
  return true;
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const keys = Object.keys(value).sort();
  return keys.length === expected.length
    && [...expected].sort().every((key, index) => key === keys[index]);
}


function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}

/** Derive placement only from Cloudflare metadata, never caller headers. */
export function voiceRelayRegion(request: Request): string | undefined {
  const cf = request.cf;
  const longitude = typeof cf?.longitude === "string" && cf.longitude.trim()
    ? Number(cf.longitude) : NaN;
  switch (cf?.continent) {
    case "NA": return Number.isFinite(longitude) ? (longitude < -100 ? "wnam" : "enam") : undefined;
    case "SA": return "sam";
    case "EU": return Number.isFinite(longitude) ? (longitude < 20 ? "weur" : "eeur") : undefined;
    case "AS": return "apac";
    case "OC": return "oc";
    default: return undefined;
  }
}
