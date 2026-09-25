/** Import format shared with js/egress. Tokens remain in UserCredentials, never in Agent. */
export interface ChatGptCredentialImport {
  access_token: string;
  refresh_token: string;
  account_id: string;
  expires_at: number; // Unix milliseconds
  fedramp: boolean;
}

export const REFRESH_EARLY_MS = 5 * 60_000;
function claims(token: string): Record<string, unknown> | null {
  if (!/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)) return null;
  try {
    const payload = token.split(".")[1].replaceAll("-", "+").replaceAll("_", "/");
    const decoded: unknown = JSON.parse(atob(payload));
    return decoded !== null && typeof decoded === "object" && !Array.isArray(decoded)
      ? decoded as Record<string, unknown> : null;
  } catch { return null; }
}

function authClaims(payload: Record<string, unknown> | null): { accountId?: string; fedramp?: boolean } {
  const auth = payload?.["https://api.openai.com/auth"];
  if (auth === null || typeof auth !== "object" || Array.isArray(auth)) return {};
  const data = auth as Record<string, unknown>;
  return {
    ...(typeof data.chatgpt_account_id === "string" ? { accountId: data.chatgpt_account_id } : {}),
    ...(typeof data.chatgpt_account_is_fedramp === "boolean" ? { fedramp: data.chatgpt_account_is_fedramp } : {}),
  };
}

function bounded(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && !!value && value.trim() === value
    && !/[\u0000-\u001f\u007f]/.test(value)
    && new TextEncoder().encode(value).byteLength <= maxBytes;
}

export function validChatGptImport(value: unknown, now = Date.now()): value is ChatGptCredentialImport {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const data = value as Record<string, unknown>;
  if (Object.keys(data).length !== 5 || Object.keys(data).some(key => ![
    "access_token", "refresh_token", "account_id", "expires_at", "fedramp",
  ].includes(key))) return false;
  if (!bounded(data.access_token, 16_384) || !bounded(data.refresh_token, 16_384)
    || !bounded(data.account_id, 256) || typeof data.fedramp !== "boolean"
    || typeof data.expires_at !== "number" || !Number.isSafeInteger(data.expires_at)
    || data.expires_at <= now + REFRESH_EARLY_MS) return false;
  const payload = claims(data.access_token);
  const auth = authClaims(payload);
  return payload !== null && Number.isSafeInteger(payload.exp)
    && (payload.exp as number) * 1_000 === data.expires_at
    && (auth.accountId === undefined || auth.accountId === data.account_id)
    && (auth.fedramp === undefined || auth.fedramp === data.fedramp);
}
