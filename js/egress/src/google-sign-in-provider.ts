import { WorkerEntrypoint } from "cloudflare:workers";
import { googleOAuthClient, type ConnectorBrokerEnv } from "./connector-broker";

const ORIGIN = "https://google-sign-in.internal";
const CALLBACK = "/v1/connectors/google/callback";
const VERIFIER = /^[A-Za-z0-9._~-]{43,128}$/;

function json(value: unknown, status = 200) {
  return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
}
async function readJson(message: Request | Response, limit: number): Promise<Record<string, unknown>> {
  const reader = message.body?.getReader();
  if (!reader) throw new Error("missing body");
  let bytes = 0;
  let text = "";
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) break;
      bytes += part.value.byteLength;
      if (bytes > limit) { await reader.cancel(); throw new Error("body too large"); }
      text += decoder.decode(part.value, { stream: true });
    }
    const value: unknown = JSON.parse(text + decoder.decode());
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid JSON");
    return value as Record<string, unknown>;
  } finally { reader.releaseLock(); }
}
function validCallback(value: unknown, environment?: string): value is string {
  if (typeof value !== "string" || value.length > 2048) return false;
  try {
    const url = new URL(value);
    const local = ["local", "development", "test"].includes(environment?.trim().toLowerCase() ?? "");
    return !url.username && !url.password && !url.search && !url.hash && url.pathname === CALLBACK
      && (url.protocol === "https:" || (local && url.protocol === "http:"
        && (url.hostname === "localhost" || url.hostname.endsWith(".localhost") || url.hostname === "127.0.0.1")));
  } catch { return false; }
}

/** Private managed-auth binding only. The default/model egress does not route this API. */
export class GoogleSignInProvider extends WorkerEntrypoint<ConnectorBrokerEnv> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.origin !== ORIGIN || url.username || url.password || url.search || url.hash
      || !["/v1/client", "/v1/token"].includes(url.pathname)) return json({ error: "not_found" }, 404);
    if (request.method !== (url.pathname === "/v1/client" ? "GET" : "POST")) return json({ error: "method_not_allowed" }, 405);
    let client: { clientId: string; clientSecret: string };
    try { client = googleOAuthClient(this.env); }
    catch { return json({ error: "google_sign_in_unavailable" }, 503); }
    if (url.pathname === "/v1/client") return json({ client_id: client.clientId });
    if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() !== "application/json") {
      return json({ error: "invalid_google_request" }, 400);
    }
    let input: Record<string, unknown>;
    try { input = await readJson(request, 8192); }
    catch { return json({ error: "invalid_google_request" }, 400); }
    if (Object.keys(input).length !== 4 || Object.keys(input).some(key => !["client_id", "code", "code_verifier", "redirect_uri"].includes(key))
      || input.client_id !== client.clientId || typeof input.code !== "string" || !input.code || input.code.length > 4096
      || typeof input.code_verifier !== "string" || !VERIFIER.test(input.code_verifier)
      || !validCallback(input.redirect_uri, this.env.ENVIRONMENT)) return json({ error: "invalid_google_request" }, 400);
    try {
      const response = await fetch("https://oauth2.googleapis.com/token", {
        method: "POST", redirect: "manual", signal: AbortSignal.timeout(10_000),
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ client_id: client.clientId, client_secret: client.clientSecret,
          code: input.code, code_verifier: input.code_verifier, redirect_uri: input.redirect_uri,
          grant_type: "authorization_code" }),
      });
      if (!response.ok) { await response.body?.cancel(); throw new Error("provider failed"); }
      const tokens = await readJson(response, 128 * 1024);
      if (typeof tokens.id_token !== "string" || !tokens.id_token || tokens.id_token.length > 32_768) throw new Error("missing identity");
      // Managed auth verifies signature, issuer, audience, nonce and expiry.
      // Neither the OAuth secret nor Google access/refresh tokens cross this binding.
      return json({ id_token: tokens.id_token });
    } catch { return json({ error: "google_authorization_failed" }, 502); }
  }
}
