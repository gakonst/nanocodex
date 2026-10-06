import type { NamedTool, ToolContext } from "nanocodex";
import type { Principal } from "./account-auth";

const MAX_REQUEST_BYTES = 96 * 1024;
const MAX_RESPONSE_BYTES = 4096;
const METHODS = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];
const ERRORS = new Set([
  "invalid_vault_request", "invalid_vault_placeholder", "vault_secret_placeholder_required",
  "vault_entry_kind_mismatch", "vault_entry_unavailable", "vault_broker_unavailable",
  "invalid_vault_entry_response", "vault_destination_denied", "vault_header_denied",
  "vault_raw_credential_denied", "vault_request_too_large", "invalid_vault_signing",
  "vault_signing_failed", "vault_signing_key_invalid", "vault_signing_kind_mismatch", "invalid_vault_signing_placeholder",
  "agent_subject_required", "subject_unavailable", "subject_deleted", "method_denied",
]);

function json(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
}

async function boundedText(body: ReadableStream<Uint8Array> | null, limit: number): Promise<string> {
  if (!body) return "";
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  let text = "", size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) { await reader.cancel(); throw new Error("limit"); }
      text += decoder.decode(value, { stream: true });
    }
    return text + decoder.decode();
  } finally { reader.releaseLock(); }
}

function envelope(input: unknown): string {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error("invalid_vault_request");
  const value = input as Record<string, unknown>;
  if (Object.keys(value).some(key => !["vault_id", "url", "method", "headers", "body", "body_encoding", "signing"].includes(key))
    || typeof value.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(value.vault_id)
    || typeof value.url !== "string" || !METHODS.includes(String(value.method ?? "GET"))) throw new Error("invalid_vault_request");
  // The broker owns destination, template, signing and Vault-kind validation.
  // Only references and public templates cross this boundary, never credentials.
  const encoded = JSON.stringify({ ...value, method: value.method ?? "GET", headers: value.headers ?? {} });
  if (new TextEncoder().encode(encoded).byteLength > MAX_REQUEST_BYTES) throw new Error("vault_request_too_large");
  return encoded;
}

/** Fixed response projection: never trust an upstream error body to be safe output. */
export async function vaultRequest(
  binding: Fetcher,
  identity: { subject: string } | { owner: string },
  input: unknown,
  signal?: AbortSignal,
): Promise<Response> {
  let body: string;
  try { body = envelope(input); } catch (error) {
    return json({ error: error instanceof Error && error.message === "vault_request_too_large" ? error.message : "invalid_vault_request" }, 400);
  }
  const endpoint = "subject" in identity ? "https://vault-egress.internal/v1/request"
    : `https://vault-egress.internal/v1/users/${encodeURIComponent(identity.owner)}/request`;
  signal?.throwIfAborted();
  try {
    const result = await binding.fetch(new Request(endpoint, {
      method: "POST", headers: { "content-type": "application/json", ...("subject" in identity ? { "x-nanocodex-subject": identity.subject } : {}) },
      body, redirect: "manual", signal,
    }));
    const value = JSON.parse(await boundedText(result.body, MAX_RESPONSE_BYTES));
    if (result.ok && value && typeof value === "object" && Number.isInteger(value.status)
      && value.status >= 100 && value.status <= 599 && value.ok === (value.status >= 200 && value.status < 300)) {
      return json({ status: value.status, ok: value.ok });
    }
    if (!result.ok && value && typeof value.error === "string" && ERRORS.has(value.error)) {
      return json({ error: value.error }, result.status);
    }
  } catch { /* Dispatch may have reached the destination. Never retry here. */ }
  return json({ error: "vault_request_outcome_unknown" }, 502);
}

/** Direct account API used by the native CLI/SDK; never inherits a Connect grant. */
export async function routeVaultRequest(request: Request, binding: Fetcher, principal: Principal | null | undefined): Promise<Response> {
  if (!principal) return json({ error: "unauthorized" }, 401);
  if ((principal.kind !== "api_key" && principal.kind !== "account_session") || principal.connectGrant !== undefined
    || !principal.capabilities.includes("agents:write") || !principal.capabilities.includes("tools:use")) return json({ error: "forbidden" }, 403);
  const url = new URL(request.url);
  if (request.method !== "POST" || url.search) return json({ error: "method_not_allowed" }, 405);
  if (principal.kind !== "api_key" && request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, 403);
  if ((request.headers.get("content-type") ?? "").toLowerCase().split(";")[0].trim() !== "application/json") return json({ error: "invalid_vault_request" }, 400);
  let input: unknown;
  try { input = JSON.parse(await boundedText(request.body, MAX_REQUEST_BYTES)); }
  catch { return json({ error: "invalid_vault_request" }, 400); }
  return vaultRequest(binding, { owner: principal.userId }, input, request.signal);
}

export function createVaultRequestTool(binding: Fetcher, subject: () => string, authorize: (context: ToolContext) => void): NamedTool {
  return {
    name: "vault_request",
    description: "Make an authorized HTTP request using a saved Vault item. Supply only its opaque vault_id and public templates. The broker replaces {{NANOCODEX_VAULT_API_KEY}}, USERNAME, PASSWORD, BASIC, CARD_NUMBER, EXPIRY_MONTH, EXPIRY_YEAR, CVV (if saved), BILLING_ZIP, ADDRESS_LINE_1, ADDRESS_LINE_2, CITY, STATE, ZIP, COUNTRY and PHONE_NUMBER placeholders at the final fetch. For a TOTP item, {{NANOCODEX_VAULT_TOTP}} inserts the current code only inside the broker at the saved exact HTTPS origin; seeds and codes are never returned. body_encoding=json or form safely escapes substituted values. Optional signing computes HMAC or PKCS8 signatures/JWTs inside the broker and substitutes {{NANOCODEX_VAULT_SIGNATURE}} or {{NANOCODEX_VAULT_JWT}}; keys and tokens never enter model code, native process arguments or output. Returns destination status and ok only, never response bodies or cookies. Does not authenticate arbitrary native CLIs or implement multi-step login protocols. No automatic retries: outcome_unknown may have executed. Use only the item and destination authorized by the user; no new password intake is needed for a saved item.",
    parameters: {
      type: "object", additionalProperties: false, required: ["vault_id", "url"],
      properties: {
        vault_id: { type: "string", description: "Exact saved Vault item ID." },
        url: { type: "string", description: "Exact public HTTPS destination authorized for this use." },
        method: { type: "string", enum: METHODS },
        headers: { type: "object", additionalProperties: { type: "string" } },
        body: { type: "string", description: "Public body template containing placeholders, never secret values." },
        body_encoding: { type: "string", enum: ["raw", "json", "form"] },
        signing: {
          type: "object", additionalProperties: false, required: ["algorithm"],
          properties: {
            algorithm: { type: "string", enum: ["HMAC-SHA256", "HMAC-SHA512", "RS256", "ES256", "EdDSA"] },
            message: { type: "string", description: "Public UTF-8 message to sign; mutually exclusive with jwt." },
            jwt: { type: "object", additionalProperties: false, required: ["header", "payload"], properties: { header: { type: "object", additionalProperties: true }, payload: { type: "object", additionalProperties: true } } },
            encoding: { type: "string", enum: ["hex", "base64", "base64url"] },
            key_encoding: { type: "string", enum: ["utf8", "base64", "hex", "pkcs8"] },
          },
        },
      },
    },
    handler: async (input, context) => {
      authorize(context);
      const response = await vaultRequest(binding, { subject: subject() }, input, context.signal);
      return response.json();
    },
  };
}
