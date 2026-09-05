import { authenticatePersistentAccount, type AccountAuthEnv } from "./account-auth";
import { bindAgentCredential } from "./credentials";
import {
  handleManagedEgress,
  isPrivateEgressHeader,
  isValidVaultId,
  isVaultPlaceholderHeader,
  VAULT_ID_HEADER,
} from "./managed-egress";

type BrowserEgressEnv = AccountAuthEnv & { NANOCODEX: Fetcher };

const THREAD_ID = /^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
const METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const ENVELOPE_FIELDS = new Set(["thread_id", "url", "method", "headers", "body"]);
const PRINCIPAL_HEADERS = new Set([
  "authorization",
  "cookie",
  "proxy-authorization",
  "x-nanocodex-subject",
]);

type EgressEnvelope = Readonly<{
  thread_id: string;
  url: string;
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}>;

/** One credential-free, destination-policy-owned egress capability for browser runtimes. */
export async function routeBrowserEgress(
  request: Request,
  env: BrowserEgressEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.pathname !== "/v1/egress") return undefined;
  if (request.method !== "POST" || url.search) return json({ error: "method_not_allowed" }, 405);
  if (request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, 403);
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "invalid_content_type" }, 415);
  }
  const authenticationHeaders = new Headers(request.headers);
  authenticationHeaders.delete("authorization");
  authenticationHeaders.delete("proxy-authorization");
  authenticationHeaders.delete("x-nanocodex-subject");
  const principal = await authenticatePersistentAccount(new Request(request.url, {
    method: request.method,
    headers: authenticationHeaders,
    signal: request.signal,
  }), env, url);
  if (!principal) return json({ error: "unauthorized" }, 401);

  const envelope = await readEnvelope(request);
  if (!envelope) return json({ error: "invalid_request" }, 400);
  const method = (envelope.method ?? "GET").toUpperCase();
  if (!METHODS.has(method)) return json({ error: "method_denied" }, 403);
  if ((method === "GET" || method === "HEAD") && envelope.body !== undefined) {
    return json({ error: "body_denied" }, 400);
  }
  const headers = decodeHeaders(envelope.headers);
  if (!headers) return json({ error: "invalid_headers" }, 400);

  let destination: URL;
  try { destination = new URL(envelope.url); } catch { return json({ error: "invalid_url" }, 400); }
  let target: Request;
  try {
    target = new Request(destination, {
      method,
      headers,
      ...(method === "GET" || method === "HEAD" || envelope.body === undefined
        ? {}
        : { body: envelope.body }),
      redirect: "manual",
      signal: request.signal,
    });
  } catch {
    return json({ error: "invalid_request" }, 400);
  }

  const subject = await browserEgressSubject(principal.userId, envelope.thread_id);
  try {
    await bindAgentCredential(env.NANOCODEX, subject, principal.userId);
  } catch {
    return json({ error: "credential_broker_unavailable" }, 503);
  }
  const response = await handleManagedEgress(target, env.NANOCODEX, subject);
  const responseHeaders = new Headers(response.headers);
  responseHeaders.set("cache-control", "no-store");
  responseHeaders.set("x-content-type-options", "nosniff");
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
}

export async function browserEgressSubject(userId: string, threadId: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`browser-egress-v1:${userId}:${threadId}`),
  );
  let binary = "";
  for (const byte of new Uint8Array(digest)) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function readEnvelope(request: Request): Promise<EgressEnvelope | undefined> {
  const encoded = await request.text();
  let value: unknown;
  try { value = JSON.parse(encoded); } catch { return undefined; }
  if (!isRecord(value) || Object.keys(value).some((key) => !ENVELOPE_FIELDS.has(key))
    || !THREAD_ID.test(value.thread_id as string)
    || typeof value.url !== "string"
    || (value.method !== undefined && typeof value.method !== "string")
    || (value.body !== undefined && typeof value.body !== "string")
    || (value.headers !== undefined && !isRecord(value.headers))) return undefined;
  return value as EgressEnvelope;
}

export function decodeHeaders(value: Record<string, string> | undefined): Headers | undefined {
  if (!value) return new Headers();
  const entries = Object.entries(value);
  const headers = new Headers();
  const names = new Set<string>();
  let vaultId: string | undefined;
  for (const [name, headerValue] of entries) {
    const lower = name.toLowerCase();
    if (names.has(lower) || typeof headerValue !== "string") return undefined;
    names.add(lower);
    if (lower === VAULT_ID_HEADER) {
      if (!isValidVaultId(headerValue)) return undefined;
      vaultId = headerValue;
    }
  }
  names.clear();
  try {
    for (const [name, headerValue] of entries) {
      const lower = name.toLowerCase();
      if (PRINCIPAL_HEADERS.has(lower)) {
        if (lower === "x-nanocodex-subject" || lower === "proxy-authorization" || lower === "cookie"
          || !vaultId || !isVaultPlaceholderHeader(lower, headerValue)) return undefined;
      } else if (isPrivateEgressHeader(lower)
        && (!vaultId || !isVaultPlaceholderHeader(lower, headerValue))) return undefined;
      names.add(lower);
      headers.append(name, headerValue);
    }
  } catch { return undefined; }
  return headers;
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
