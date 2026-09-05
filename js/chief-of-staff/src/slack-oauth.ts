const STATE_TTL_MS = 10 * 60 * 1_000;
const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const STATE_SECRET_BYTES = 32;

export const SLACK_BOT_SCOPES = Object.freeze([
  "assistant:write",
  "app_mentions:read",
  "chat:write",
  "channels:history",
  "groups:history",
  "im:history",
  "mpim:history",
  "users:read",
] as const);

type InstallState = Readonly<{
  accountId: string;
  expiresAt: number;
  nonce: string;
  version: 1;
}>;

export async function slackAuthorizationUrl(options: Readonly<{
  accountId: string;
  clientId: string;
  redirectUri: string;
  stateSecret: string;
  now?: number;
}>): Promise<URL> {
  if (!ACCOUNT_ID.test(options.accountId)) throw new Error("Slack install account is invalid");
  const redirect = new URL(options.redirectUri);
  if (redirect.protocol !== "https:") throw new Error("Slack redirect URI must use HTTPS");
  const state = await signInstallState({
    accountId: options.accountId,
    expiresAt: (options.now ?? Date.now()) + STATE_TTL_MS,
    nonce: crypto.randomUUID(),
    version: 1,
  }, options.stateSecret);
  const authorization = new URL("https://slack.com/oauth/v2/authorize");
  authorization.searchParams.set("client_id", options.clientId);
  authorization.searchParams.set("scope", SLACK_BOT_SCOPES.join(","));
  authorization.searchParams.set("redirect_uri", redirect.href);
  authorization.searchParams.set("state", state);
  return authorization;
}

export async function verifySlackInstallState(
  value: string,
  stateSecret: string,
  now = Date.now(),
): Promise<InstallState | undefined> {
  const [encoded, signature, extra] = value.split(".");
  if (!encoded || !signature || extra !== undefined) return undefined;
  const supplied = decodeBase64Url(signature);
  if (!supplied) return undefined;
  const key = await stateKey(stateSecret);
  if (!(await crypto.subtle.verify("HMAC", key, supplied, new TextEncoder().encode(encoded)))) {
    return undefined;
  }
  const bytes = decodeBase64Url(encoded);
  if (!bytes) return undefined;
  let state: unknown;
  try { state = JSON.parse(new TextDecoder().decode(bytes)); }
  catch { return undefined; }
  if (!isRecord(state)
    || state.version !== 1
    || typeof state.accountId !== "string"
    || !ACCOUNT_ID.test(state.accountId)
    || typeof state.expiresAt !== "number"
    || !Number.isSafeInteger(state.expiresAt)
    || state.expiresAt < now
    || state.expiresAt > now + STATE_TTL_MS
    || typeof state.nonce !== "string"
    || !/^[0-9a-f-]{36}$/.test(state.nonce)) return undefined;
  return state as InstallState;
}

async function signInstallState(state: InstallState, secret: string): Promise<string> {
  const encoded = encodeBase64Url(new TextEncoder().encode(JSON.stringify(state)));
  const signature = await crypto.subtle.sign(
    "HMAC",
    await stateKey(secret),
    new TextEncoder().encode(encoded),
  );
  return `${encoded}.${encodeBase64Url(new Uint8Array(signature))}`;
}

async function stateKey(secret: string): Promise<CryptoKey> {
  const bytes = decodeBase64Url(secret);
  if (!bytes || bytes.length !== STATE_SECRET_BYTES) {
    throw new Error("Slack OAuth state secret must be 32 base64url bytes");
  }
  return crypto.subtle.importKey("raw", bytes, { hash: "SHA-256", name: "HMAC" }, false, ["sign", "verify"]);
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value: string): Uint8Array | undefined {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) return undefined;
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    const binary = atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="));
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
