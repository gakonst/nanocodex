import {
  authenticate,
  authenticatePersistentAccount,
  requireSameOriginMutation,
  type AccountAuthEnv,
} from "./account-auth";
import { fetchResponseWithDeadline } from "./deadline";

type CredentialEnv = AccountAuthEnv & { NANOCODEX: Fetcher };

const DEFAULT_OWNERSHIP_IO_TIMEOUT_MS = 10_000;
const CREDENTIAL_BIND_ATTEMPTS = 3;
const CREDENTIAL_BIND_RETRY_MS = 25;
const MAX_VAULT_BODY_BYTES = 12 * 1024;

type VaultKind = "login" | "api_key" | "card" | "address" | "phone";

const ROUTES = new Map<string, ReadonlySet<string>>([
  ["/v1/credentials", new Set(["GET"])],
  ["/v1/credentials/openai", new Set(["PUT", "DELETE"])],
  ["/v1/credentials/chatgpt", new Set(["DELETE"])],
  ["/v1/credentials/chatgpt/login", new Set(["GET", "POST"])],
  ["/v1/credentials/local-claim", new Set(["POST"])],
]);

export async function routeCredentialRequest(
  request: Request,
  env: CredentialEnv,
  url: URL,
): Promise<Response | undefined> {
  const sshIdentity = url.pathname.match(/^\/v1\/credentials\/ssh\/([A-Za-z0-9][A-Za-z0-9._-]{0,63})$/)?.[1];
  const vaultMatch = url.pathname.match(
    /^\/v1\/credentials\/vault\/(login|api_key|card|address|phone)(?:\/([A-Za-z0-9_-]{22,64}))?$/,
  );
  const vaultKind = vaultMatch?.[1] as VaultKind | undefined;
  const vaultId = vaultMatch?.[2];
  const methods = ROUTES.get(url.pathname)
    ?? (sshIdentity ? new Set(["PUT", "DELETE"]) : undefined)
    ?? (vaultKind ? new Set(vaultId ? ["DELETE"] : ["POST"]) : undefined);
  if (!methods) return undefined;
  if (!methods.has(request.method)) return json({ error: "method_not_allowed" }, 405);
  if (url.search) return json({ error: "invalid_request" }, 400);

  // Metadata reads are safe for an ephemeral browser identity, but mutations
  // must be tied to a passkey-backed account so a user-supplied provider secret
  // cannot outlive the anonymous session that submitted it.
  const principal = request.method === "GET"
    ? await authenticate(request, env, url)
    : await authenticatePersistentAccount(request, env, url);
  if (!principal || principal.kind !== "account_session") {
    return json({ error: "unauthorized" }, 401);
  }
  if (request.method === "PUT" && (url.pathname === "/v1/credentials/openai" || sshIdentity)
    && !request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "invalid_content_type" }, 415);
  }
  if (request.method === "POST" && vaultKind
    && !isJsonContentType(request.headers.get("content-type"))) {
    return json({ error: "invalid_content_type" }, 415);
  }
  if (request.method !== "GET") {
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
  }

  let vaultBody: string | undefined;
  if (request.method === "POST" && vaultKind) {
    let value: unknown;
    try {
      value = JSON.parse(await readBoundedText(request, MAX_VAULT_BODY_BYTES));
    } catch (error) {
      return error instanceof BodyTooLarge
        ? json({ error: "body_too_large" }, 413)
        : json({ error: "invalid_vault_entry" }, 400);
    }
    const validated = validateVaultPayload(value, vaultKind);
    if (!validated) return json({ error: "invalid_vault_entry" }, 400);
    vaultBody = JSON.stringify(validated);
  }

  const suffix = url.pathname.slice("/v1/credentials".length);
  const polling = suffix === "/chatgpt/login" && request.method === "GET";
  const brokerSuffix = suffix === "/local-claim"
    ? "/chatgpt/local-claim"
    : polling ? "/chatgpt/login/status" : suffix;
  const target = `https://broker.internal/users/${encodeURIComponent(principal.userId)}/credentials${brokerSuffix}`;
  const response = await env.NANOCODEX.fetch(target, {
    method: polling ? "POST" : request.method,
    ...(vaultBody !== undefined ? {
      headers: { "content-type": "application/json" },
      body: vaultBody,
    } : request.body === null ? {} : {
      headers: { "content-type": request.headers.get("content-type") ?? "" },
      body: request.body,
    }),
  });
  if (suffix !== "" || request.method !== "GET" || !response.ok) return response;
  try {
    await bindAgentCredential(
      env.NANOCODEX,
      await browserModelSubject(principal.userId),
      principal.userId,
    );
    return response;
  } catch {
    await response.body?.cancel().catch(() => {});
    return json({ error: "credential_broker_unavailable" }, 503);
  }
}

export async function browserModelSubject(userId: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`browser-model-v1:${userId}`),
  );
  let binary = "";
  for (const byte of new Uint8Array(digest)) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export async function bindAgentCredential(
  binding: Fetcher,
  subject: string,
  userId: string,
  timeoutMs = DEFAULT_OWNERSHIP_IO_TIMEOUT_MS,
): Promise<void> {
  let failure: unknown;
  for (let attempt = 0; attempt < CREDENTIAL_BIND_ATTEMPTS; attempt += 1) {
    try {
      await fetchResponseWithDeadline(
        binding,
        `https://broker.internal/subjects/${subject}`,
        {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ user_id: userId }),
        },
        timeoutMs,
        "credential subject binding",
        (response) => {
          if (!response.ok) {
            const error = new Error(
              `credential subject binding failed with HTTP ${response.status}`,
            );
            throw Object.assign(error, {
              code: response.status === 408 || response.status === 429 || response.status >= 500
                ? "retryable"
                : "definitive",
            });
          }
        },
        { retryable: true },
      );
      return;
    } catch (error) {
      failure = error;
      if (errorCode(error) === "definitive" || attempt === CREDENTIAL_BIND_ATTEMPTS - 1) {
        throw error;
      }
      const baseDelay = CREDENTIAL_BIND_RETRY_MS * (2 ** attempt);
      await scheduler.wait(baseDelay + Math.floor(Math.random() * baseDelay));
    }
  }
  throw failure;
}

export async function unbindAgentCredential(
  binding: Fetcher,
  subject: string,
  userId: string,
  timeoutMs = DEFAULT_OWNERSHIP_IO_TIMEOUT_MS,
): Promise<void> {
  await fetchResponseWithDeadline(
    binding,
    `https://broker.internal/subjects/${subject}`,
    {
      method: "DELETE",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ user_id: userId }),
    },
    timeoutMs,
    "credential subject unbinding",
    (response) => {
      if (!response.ok && response.status !== 404) {
        throw new Error(`credential subject unbinding failed with HTTP ${response.status}`);
      }
    },
    { retryable: true },
  );
}

function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error
    && typeof error.code === "string"
    ? error.code
    : undefined;
}

function validateVaultPayload(
  value: unknown,
  kind: VaultKind,
): Record<string, string> | undefined {
  if (!isRecord(value)) return undefined;
  const hasAddressLine2 = Object.prototype.hasOwnProperty.call(value, "address_line_2");
  const expected = vaultKeys(kind, hasAddressLine2);
  const keys = Object.keys(value);
  if (keys.length !== expected.length || keys.some((key) => !expected.includes(key))) {
    return undefined;
  }
  const name = boundedText(value.name, 120);
  if (!name) return undefined;
  if (kind === "api_key") {
    const apiKey = boundedSecret(value.api_key, 8_192);
    return apiKey ? { name, api_key: apiKey } : undefined;
  }
  if (kind === "login") {
    const username = boundedText(value.username, 512);
    const password = boundedSecret(value.password, 8_192);
    return username && password ? { name, username, password } : undefined;
  }
  if (kind === "card") {
    const cardNumber = vaultCardNumber(value.card_number);
    const expiryMonth = typeof value.expiry_month === "string"
      && /^(?:0?[1-9]|1[0-2])$/.test(value.expiry_month) ? value.expiry_month : undefined;
    const expiryYear = typeof value.expiry_year === "string"
      && /^[0-9]{4}$/.test(value.expiry_year) ? value.expiry_year : undefined;
    const cvv = typeof value.cvv === "string" && /^[0-9]{3,4}$/.test(value.cvv)
      ? value.cvv : undefined;
    const billingZip = boundedText(value.billing_zip, 32);
    return cardNumber && expiryMonth && expiryYear && cvv && billingZip
      ? {
          name,
          card_number: cardNumber,
          expiry_month: expiryMonth,
          expiry_year: expiryYear,
          cvv,
          billing_zip: billingZip,
        }
      : undefined;
  }
  if (kind === "address") {
    const addressLine1 = boundedText(value.address_line_1, 256);
    const addressLine2 = value.address_line_2 === undefined
      ? undefined : boundedText(value.address_line_2, 256);
    const city = boundedText(value.city, 120);
    const state = boundedText(value.state, 120);
    const zip = boundedText(value.zip, 32);
    const country = boundedText(value.country, 120);
    if (!addressLine1 || (value.address_line_2 !== undefined && !addressLine2)
      || !city || !state || !zip || !country) return undefined;
    return {
      name,
      address_line_1: addressLine1,
      ...(addressLine2 ? { address_line_2: addressLine2 } : {}),
      city,
      state,
      zip,
      country,
    };
  }
  const phoneNumber = boundedText(value.phone_number, 64);
  return phoneNumber ? { name, phone_number: phoneNumber } : undefined;
}

function vaultKeys(kind: VaultKind, hasAddressLine2: boolean): readonly string[] {
  switch (kind) {
    case "api_key": return ["name", "api_key"];
    case "login": return ["name", "username", "password"];
    case "card": return [
      "name", "card_number", "expiry_month", "expiry_year", "cvv", "billing_zip",
    ];
    case "address": return [
      "name", "address_line_1",
      ...(hasAddressLine2 ? ["address_line_2"] : []),
      "city", "state", "zip", "country",
    ];
    case "phone": return ["name", "phone_number"];
  }
}

function boundedText(value: unknown, maxBytes: number): string | undefined {
  return typeof value === "string" && value.length > 0 && value.trim() === value
    && !/[\u0000-\u001f\u007f]/.test(value)
    && new TextEncoder().encode(value).byteLength <= maxBytes ? value : undefined;
}

function boundedSecret(value: unknown, maxBytes: number): string | undefined {
  return typeof value === "string" && value.length > 0 && !value.includes("\0")
    && new TextEncoder().encode(value).byteLength <= maxBytes ? value : undefined;
}

function vaultCardNumber(value: unknown): string | undefined {
  const cardNumber = boundedText(value, 23);
  if (!cardNumber || !/^[0-9][0-9 -]*[0-9]$/.test(cardNumber)) return undefined;
  return /^[0-9]{12,19}$/.test(cardNumber.replaceAll(" ", "").replaceAll("-", ""))
    ? cardNumber : undefined;
}

async function readBoundedText(request: Request, limit: number): Promise<string> {
  if (!request.body) return "";
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let bytes = 0;
  let text = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return text + decoder.decode();
      bytes += value.byteLength;
      if (bytes > limit) {
        await reader.cancel();
        throw new BodyTooLarge();
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}

function isJsonContentType(value: string | null): boolean {
  return value?.split(";", 1)[0]?.trim().toLowerCase() === "application/json";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

class BodyTooLarge extends Error {}
