/** Public Pub/Sub boundary. OAuth credentials never enter this receiver. */
export interface GmailPushIngressEnv {
  GMAIL_PUSH_AUDIENCE?: string;
  GMAIL_PUSH_SERVICE_ACCOUNT?: string;
  GMAIL_PUSH_SUBSCRIPTION?: string;
  GMAIL_PUSH_OWNER_ID?: string;
  GMAIL_PUSH_CONNECTION_ID?: string;
  GMAIL_PUSH_MAILBOXES?: DurableObjectNamespace;
}
type GoogleKey = JsonWebKey & { kid?: string; alg?: string; use?: string };
let cachedKeys: { keys: GoogleKey[]; until: number } | undefined;
const JWKS = "https://www.googleapis.com/oauth2/v3/certs";
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
export function gmailMailboxName(userId: string, connectionId: string): string {
  return JSON.stringify([userId, connectionId]);
}
function decode(value: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("invalid base64url");
  return Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));
}
async function keys(fetchKeys: typeof fetch): Promise<GoogleKey[]> {
  if (fetchKeys === fetch && cachedKeys && cachedKeys.until > Date.now()) return cachedKeys.keys;
  const response = await fetchKeys(JWKS, { redirect: "error", signal: AbortSignal.timeout(10_000) });
  if (!response.ok) throw new Error("Google keys unavailable");
  const body = await response.json() as { keys?: GoogleKey[] };
  if (!Array.isArray(body.keys) || body.keys.length > 20) throw new Error("invalid Google keys");
  if (fetchKeys === fetch) cachedKeys = { keys: body.keys, until: Date.now() + 300_000 };
  return body.keys;
}
export async function verifyGooglePushToken(token: string, env: GmailPushIngressEnv, fetchKeys: typeof fetch = fetch): Promise<boolean> {
  try {
    if (!env.GMAIL_PUSH_AUDIENCE || !env.GMAIL_PUSH_SERVICE_ACCOUNT || token.length > 8192) return false;
    const parts = token.split(".");
    if (parts.length !== 3) return false;
    const header = JSON.parse(new TextDecoder().decode(decode(parts[0]!)));
    const claims = JSON.parse(new TextDecoder().decode(decode(parts[1]!)));
    const now = Math.floor(Date.now() / 1000);
    if (header.alg !== "RS256" || typeof header.kid !== "string"
      || !["accounts.google.com", "https://accounts.google.com"].includes(claims.iss)
      || claims.aud !== env.GMAIL_PUSH_AUDIENCE || claims.email !== env.GMAIL_PUSH_SERVICE_ACCOUNT
      || claims.email_verified !== true || typeof claims.sub !== "string" || !claims.sub
      || typeof claims.exp !== "number" || !Number.isFinite(claims.exp) || claims.exp <= now
      || typeof claims.iat !== "number" || !Number.isFinite(claims.iat) || claims.iat > now + 60
      || claims.exp - claims.iat > 7200) return false;
    const jwk = (await keys(fetchKeys)).find((key) => key.kid === header.kid && key.kty === "RSA" && (!key.alg || key.alg === "RS256") && (!key.use || key.use === "sig"));
    if (!jwk) return false; // Google rotates keys; retry after the short cache TTL.
    const key = await crypto.subtle.importKey("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
    return await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, decode(parts[2]!), new TextEncoder().encode(`${parts[0]}.${parts[1]}`));
  } catch { return false; }
}
export async function handleGmailPush(request: Request, env: GmailPushIngressEnv): Promise<Response> {
  if (!env.GMAIL_PUSH_AUDIENCE || !env.GMAIL_PUSH_SERVICE_ACCOUNT || !env.GMAIL_PUSH_SUBSCRIPTION || !env.GMAIL_PUSH_OWNER_ID || !env.GMAIL_PUSH_CONNECTION_ID) return new Response(null, { status: 503 });
  if (request.method !== "POST") return new Response(null, { status: 405 });
  const bearer = /^Bearer ([A-Za-z0-9_.-]+)$/.exec(request.headers.get("authorization") ?? "");
  if (!bearer || !await verifyGooglePushToken(bearer[1]!, env)) return new Response(null, { status: 401 });
  const match = /^\/v1\/gmail-push\/([^/]+)\/([^/]+)$/.exec(new URL(request.url).pathname);
  if (!match || !ID.test(match[1]!) || !ID.test(match[2]!)) return new Response(null, { status: 404 });
  if (match[1] !== env.GMAIL_PUSH_OWNER_ID || match[2] !== env.GMAIL_PUSH_CONNECTION_ID) return new Response(null, { status: 404 });
  if (!env.GMAIL_PUSH_MAILBOXES) return new Response(null, { status: 503 });
  try {
    // Stream-bound even when Content-Length is absent or false.
    const reader = request.body?.getReader();
    if (!reader) return new Response(null, { status: 400 });
    let text = "", bytes = 0;
    const decoder = new TextDecoder();
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > 16384) { await reader.cancel(); return new Response(null, { status: 413 }); }
      text += decoder.decode(chunk.value, { stream: true });
    }
    text += decoder.decode();
    const envelope = JSON.parse(text);
    if (typeof envelope.subscription !== "string" || !/^projects\/[^/]+\/subscriptions\/[^/]+$/.test(envelope.subscription)
      || envelope.subscription !== env.GMAIL_PUSH_SUBSCRIPTION
      || typeof envelope.message?.messageId !== "string" || typeof envelope.message?.data !== "string"
      || envelope.message.data.length > 8192) return new Response(null, { status: 400 });
    const data = JSON.parse(atob(envelope.message.data.replaceAll("-", "+").replaceAll("_", "/")));
    if (typeof data.emailAddress !== "string" || data.emailAddress.length > 320
      || typeof data.historyId !== "string" || !/^[0-9]{1,30}$/.test(data.historyId)) return new Response(null, { status: 400 });
    return await env.GMAIL_PUSH_MAILBOXES.getByName(gmailMailboxName(match[1]!, match[2]!)).fetch("https://gmail-push.internal/notify", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(data),
    });
  } catch { return new Response(null, { status: 503 }); }
}
