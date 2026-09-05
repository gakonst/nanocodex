import {
  authenticate,
  forwardPrincipalAssertions,
  requireSameOriginMutation,
  type AccountAuthEnv,
} from "./account-auth";
import { bindAgentCredential } from "./credentials";
import { fetchResponseWithDeadline } from "./deadline";

const AGENT_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[78][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const VOICE_SESSION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const CALL_ID = /^(?:rtc_[A-Za-z0-9._:-]{1,196}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i;
const MAX_CALL_BODY_BYTES = 64 * 1024;
const MAX_INSTRUCTIONS_BYTES = 32 * 1024;
const MAX_SDP_BYTES = 32 * 1024;
const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const REALTIME_MODEL = "gpt-live-1-codex";
const REALTIME_VOICES = new Set([
  "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove",
]);

type ManagedRealtimeTransportEnv = AccountAuthEnv & {
  NANOCODEX: Fetcher;
  NANOCODEX_SESSIONS: {
    get(id: DurableObjectId): Fetcher;
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
  const principal = await authenticate(request, env, url);
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
  const durableId = env.NANOCODEX_SESSIONS.idFromName(agentId);
  const subject = durableId.toString();
  const ownershipHeaders = new Headers();
  forwardPrincipalAssertions(ownershipHeaders, principal);
  let owned: boolean;
  try {
    owned = await fetchResponseWithDeadline(
      env.NANOCODEX_SESSIONS.get(durableId),
      "https://session.internal/state",
      { headers: ownershipHeaders },
      ownershipTimeoutMs,
      "managed Realtime ownership assertion",
      (response) => response.ok,
    );
  } catch {
    return json({ error: "agent_ownership_unavailable" }, 503);
  }
  if (!owned) return json({ error: "not_found" }, 404);
  if (!voiceSessionId || !VOICE_SESSION_ID.test(voiceSessionId)) {
    return json({ error: "invalid_voice_session" }, 400);
  }

  try {
    // Creation installs this mapping. Rebinding here also repairs broker state
    // that was lost independently without exposing either account credential.
    await bindAgentCredential(env.NANOCODEX, subject, principal.userId, ownershipTimeoutMs);
  } catch {
    return json({ error: "credential_broker_unavailable" }, 503);
  }

  if (resource === "calls") return realtimeCall(callBody!, env, agentId, voiceSessionId, subject);
  return realtimeSideband(callId!, env, agentId, voiceSessionId, subject);
}

async function validatedCallBody(request: Request, url: URL): Promise<string | Response> {
  if (url.search) return json({ error: "invalid_request" }, 400);
  if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase()
    !== "application/json") {
    return json({ error: "invalid_content_type" }, 415);
  }
  let body: string;
  try { body = await readBoundedText(request, MAX_CALL_BODY_BYTES); }
  catch { return json({ error: "request_too_large" }, 413); }
  let decoded: unknown;
  try { decoded = JSON.parse(body); }
  catch { return json({ error: "invalid_request" }, 400); }
  if (!isRecord(decoded)
    || !exactKeys(decoded, ["sdp", "session"])
    || typeof decoded.sdp !== "string"
    || !decoded.sdp.trim()
    || encodedBytes(decoded.sdp) > MAX_SDP_BYTES
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
): Promise<Response> {
  const response = await env.NANOCODEX.fetch(new Request(
    "https://nanocodex.internal/v1/realtime/calls",
    {
      method: "POST",
      headers: internalHeaders(agentId, voiceSessionId, subject, false),
      body,
    },
  ));
  const headers = sanitizedHeaders(response.headers);
  const location = headers.get("location");
  if (location) {
    headers.set("x-nanocodex-realtime-location", location);
    headers.delete("location");
  }
  headers.set("cache-control", "no-store");
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
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
): Promise<Response> {
  // Return the binding response itself: reconstructing a 101 Response severs
  // Cloudflare's upgraded WebSocket from its provider peer.
  return env.NANOCODEX.fetch(new Request(
    "https://nanocodex.internal/v1/realtime/sideband",
    { headers: internalHeaders(agentId, voiceSessionId, subject, true, callId) },
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

async function readBoundedText(request: Request, limit: number): Promise<string> {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) throw new Error("request too large");
  if (!request.body) return "";
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let total = 0;
  let text = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return text + decoder.decode();
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      throw new Error("request too large");
    }
    text += decoder.decode(value, { stream: true });
  }
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

function validRealtimeSession(value: unknown): boolean {
  if (!isRecord(value)
    || !exactKeys(value, ["audio", "delegation", "instructions", "model"])
    || value.model !== REALTIME_MODEL
    || typeof value.instructions !== "string"
    || !value.instructions
    || encodedBytes(value.instructions) > MAX_INSTRUCTIONS_BYTES
    || !isRecord(value.delegation)
    || !exactKeys(value.delegation, ["type"])
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

function encodedBytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}
