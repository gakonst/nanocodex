import { WorkerEntrypoint } from "cloudflare:workers";
import { verifyTwilioWebhookSignature, type TwilioVoiceEnv } from "./twilio-voice";

/** Private service-binding capability; never dispatched by the public managed router. */
export class PhoneProvider extends WorkerEntrypoint<TwilioVoiceEnv> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.origin !== "https://phone-provider.internal" || url.search || url.hash) return result({ error: "not_found" }, 404);
    const credentials = authentication(this.env);
    if (url.pathname === "/status" && request.method === "GET") {
      return result({ configured: Boolean(credentials && this.env.TWILIO_AUTH_TOKEN),
        ...(credentials ? { account_sid: credentials.account } : {}),
        auth_token_available: Boolean(this.env.TWILIO_AUTH_TOKEN) });
    }
    if (request.method !== "POST" || !["/request", "/verify"].includes(url.pathname)
      || request.headers.get("content-type")?.split(";")[0].toLowerCase() !== "application/json") return result({ error: "invalid_request" }, 400);
    let value: Record<string, unknown>;
    try {
      const decoded: unknown = JSON.parse(await boundedBody(request.body, 96 * 1024));
      if (!decoded || typeof decoded !== "object" || Array.isArray(decoded)) throw new Error();
      value = decoded as Record<string, unknown>;
    } catch { return result({ error: "invalid_request" }, 400); }
    if (url.pathname === "/verify") {
      if (Object.keys(value).some(key => !["url", "signature", "body"].includes(key))
        || typeof value.url !== "string" || typeof value.signature !== "string" || typeof value.body !== "string") return result({ valid: false }, 400);
      return result({ valid: await verifyTwilioWebhookSignature(this.env, value.url, new URLSearchParams(value.body), value.signature) });
    }
    if (!credentials) return result({ error: "phone_provider_unavailable" }, 503);
    if (Object.keys(value).some(key => !["path", "method", "body"].includes(key)) || typeof value.path !== "string"
      || typeof value.method !== "string" || !["GET", "POST", "DELETE"].includes(value.method)
      || (value.body !== undefined && typeof value.body !== "string")
      || (value.method !== "POST" && value.body !== undefined)) return result({ error: "invalid_request" }, 400);
    const path = value.path.replace("{account}", credentials.account);
    const target = providerTarget(path, value.method, credentials.account);
    if (!target) return result({ error: "provider_operation_denied" }, 403);
    try {
      // A timeout is an unresolved operation, never an automatic retry.
      const response = await fetch(target, { method: value.method, redirect: "manual", signal: AbortSignal.timeout(15_000),
        headers: { authorization: credentials.authorization, ...(value.body === undefined ? {} : { "content-type": "application/x-www-form-urlencoded" }) },
        ...(typeof value.body === "string" ? { body: value.body } : {}),
      });
      if (response.status === 204) return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
      const data: unknown = JSON.parse(await boundedBody(response.body, 512 * 1024));
      if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error();
      if (!response.ok) {
        const code = (data as Record<string, unknown>).code;
        return result({ error: "phone_provider_rejected", ...(Number.isSafeInteger(code) ? { code } : {}) }, response.status);
      }
      return result(data, response.status);
    } catch { return result({ error: "phone_provider_outcome_unknown" }, 502); }
  }
}

function authentication(env: TwilioVoiceEnv): { account: string; authorization: string } | undefined {
  const account = env.TWILIO_ACCOUNT_SID;
  if (!account || !/^AC[0-9a-f]{32}$/i.test(account)) return;
  const useKey = env.TWILIO_API_KEY_SID !== undefined || env.TWILIO_API_KEY_SECRET !== undefined;
  const user = useKey ? env.TWILIO_API_KEY_SID : account;
  const password = useKey ? env.TWILIO_API_KEY_SECRET : env.TWILIO_AUTH_TOKEN;
  if (!user || (useKey && !/^SK[0-9a-f]{32}$/i.test(user)) || !password || password.length > 256 || !/^[\x21-\x7e]+$/.test(password)) return;
  return { account, authorization: `Basic ${btoa(user + ":" + password)}` };
}

function providerTarget(path: string, method: string, account: string): string | undefined {
  if (path.length > 8192 || /[\\#\u0000-\u0020\u007f]/.test(path) || !path.startsWith("/") || path.startsWith("//")) return;
  const pricing = /^\/(?:v2\/PhoneNumbers\/[A-Z]{2}|v1\/(?:PhoneNumbers|Messaging)\/Countries\/[A-Z]{2})$/.test(path) && method === "GET";
  if (pricing) return "https://pricing.twilio.com" + path;
  const prefix = `/2010-04-01/Accounts/${account}`;
  if (!path.startsWith(prefix + "/")) return;
  const tail = path.slice(prefix.length);
  const [pathname, query] = tail.split("?");
  if (tail.split("?").length > 2) return;
  const available = /^\/AvailablePhoneNumbers\/[A-Z]{2}\/(?:Local|Mobile)\.json$/.test(pathname) && method === "GET";
  const collection = pathname === "/IncomingPhoneNumbers.json" && (method === "GET" || method === "POST");
  const item = /^\/IncomingPhoneNumbers\/PN[0-9a-f]{32}\.json$/i.test(pathname) && (method === "GET" || method === "DELETE");
  if (!available && !collection && !item) return;
  if (query !== undefined) {
    if (method !== "GET" || item) return;
    const values = new URLSearchParams(query);
    const allowed = available ? ["AreaCode", "Contains", "SmsEnabled", "SMS", "PageSize", "ExcludeAllAddressRequired"] : ["PhoneNumber", "FriendlyName", "PageSize", "Page"];
    for (const key of values.keys()) if (!allowed.includes(key) || values.getAll(key).length !== 1) return;
  }
  return "https://api.twilio.com" + path;
}

async function boundedBody(body: ReadableStream<Uint8Array> | null, limit: number): Promise<string> {
  if (!body) return "";
  const reader = body.getReader(), decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  let text = "", bytes = 0;
  try {
    for (;;) {
      const part = await reader.read();
      if (part.done) return text + decoder.decode();
      bytes += part.value.byteLength;
      if (bytes > limit) { await reader.cancel(); throw new Error("body_limit"); }
      text += decoder.decode(part.value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}
function result(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
}
