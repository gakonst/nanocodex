const GRANT_ID = /^0x[0-9a-fA-F]{64}$/;
const GRANT_TOKEN = /^[A-Za-z0-9_-]{43}$/;
const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const ADDRESS = /^0x[0-9a-fA-F]{40}$/;

export type ConnectConfiguration = Readonly<{
  apiUrl: string;
  appId: string;
  appOrigin: string;
  dialogUrl: string;
}>;

export type ConnectIdentity = Readonly<{
  accountAddress: `0x${string}`;
  accountId: string;
  expiresAt: number;
  grantId: `0x${string}`;
}>;

export class ConnectVerificationError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ConnectVerificationError";
    this.status = status;
  }
}

export function connectConfiguration(env: {
  NANOCODEX_CONNECT_API_URL?: string;
  NANOCODEX_CONNECT_APP_ID?: string;
  NANOCODEX_CONNECT_APP_ORIGIN?: string;
  NANOCODEX_CONNECT_DIALOG_URL?: string;
}): ConnectConfiguration | undefined {
  const appId = env.NANOCODEX_CONNECT_APP_ID?.trim();
  const appOrigin = exactPublicOrigin(env.NANOCODEX_CONNECT_APP_ORIGIN?.trim());
  const apiUrl = exactServiceUrl(env.NANOCODEX_CONNECT_API_URL?.trim());
  const dialogUrl = exactServiceUrl(env.NANOCODEX_CONNECT_DIALOG_URL?.trim());
  if (!appId || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(appId)
    || !appOrigin || !apiUrl || !dialogUrl) return undefined;
  return { apiUrl, appId, appOrigin, dialogUrl };
}

export function connectCredentials(request: Request): Readonly<{
  grantId: `0x${string}`;
  token: string;
}> | undefined {
  const token = request.headers.get("authorization")?.match(/^Bearer ([A-Za-z0-9_-]{43})$/i)?.[1];
  const grantId = request.headers.get("x-nanocodex-grant-id")?.trim();
  if (!token || !GRANT_TOKEN.test(token) || !grantId || !GRANT_ID.test(grantId)) return undefined;
  return { grantId: grantId.toLowerCase() as `0x${string}`, token };
}

export async function verifyConnectIdentity(
  credentials: Readonly<{ grantId: `0x${string}`; token: string }>,
  configuration: ConnectConfiguration,
  fetcher: typeof fetch = fetch,
): Promise<ConnectIdentity> {
  let response: Response;
  try {
    response = await fetcher(
      new URL(`/v1/grants/${credentials.grantId}`, configuration.apiUrl),
      {
        headers: {
          accept: "application/json",
          authorization: `Bearer ${credentials.token}`,
          origin: configuration.appOrigin,
          "x-nanocodex-app-id": configuration.appId,
        },
      },
    );
  } catch {
    throw new ConnectVerificationError(503, "connect_unavailable");
  }
  const value = await response.json().catch(() => undefined);
  if (!response.ok) {
    throw new ConnectVerificationError(
      response.status === 401 || response.status === 403 ? 401 : 503,
      response.status === 401 || response.status === 403
        ? "connect_session_invalid"
        : "connect_unavailable",
    );
  }
  return connectIdentityFromWire(value, credentials.grantId);
}

export function connectIdentityFromWire(value: unknown, expectedGrantId: string): ConnectIdentity {
  if (!isRecord(value) || typeof value.account_id !== "string" || !ACCOUNT_ID.test(value.account_id)
    || typeof value.account_address !== "string" || !ADDRESS.test(value.account_address)
    || value.authorization_mode !== "access_key" || !isRecord(value.grant)
    || typeof value.grant.id !== "string"
    || value.grant.id.toLowerCase() !== expectedGrantId.toLowerCase()
    || value.grant.status !== "active" || value.grant.permission !== "agent.run"
    || !Number.isSafeInteger(value.grant.expires_at)
    || Number(value.grant.expires_at) <= Math.floor(Date.now() / 1_000)
    || !Array.isArray(value.grant.capabilities)
    || !value.grant.capabilities.includes("nanocodex.agent")
    || !value.grant.capabilities.includes("mpp.mach")) {
    throw new ConnectVerificationError(401, "connect_session_invalid");
  }
  return {
    accountAddress: value.account_address.toLowerCase() as `0x${string}`,
    accountId: value.account_id,
    expiresAt: Number(value.grant.expires_at),
    grantId: value.grant.id.toLowerCase() as `0x${string}`,
  };
}

function exactServiceUrl(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    if (url.username || url.password || url.hash || !publicProtocol(url)) return undefined;
    return url.toString();
  } catch {
    return undefined;
  }
}

function exactPublicOrigin(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return url.origin === value && publicProtocol(url) && !url.username && !url.password
      ? value
      : undefined;
  } catch {
    return undefined;
  }
}

function publicProtocol(url: URL): boolean {
  const loopback = url.hostname === "localhost" || url.hostname.endsWith(".localhost")
    || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
  return url.protocol === "https:" || (url.protocol === "http:" && loopback);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
