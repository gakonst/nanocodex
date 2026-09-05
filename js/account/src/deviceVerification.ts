export const productionConnectApiOrigin = "https://nanocodex-connect-api.gakonst.workers.dev";
export const productionNanocodexOrigin = "https://nanocodex.gakonst.workers.dev";

const cliApp = Object.freeze({
  id: "nanocodex-cli",
  name: "Nanocodex CLI",
  origin: "https://cli.nanocodex.xyz",
});

export type PendingDeviceAuthorization = Readonly<{
  apiOrigin: string;
  request: Readonly<{
    id: string | number;
    jsonrpc: "2.0";
    method: "wallet_connect";
    params?: unknown;
  }>;
  userCode: string;
}>;

export function deviceApiOrigin(value: string | null, pageOrigin: string): string {
  const candidate = value ?? productionConnectApiOrigin;
  if (candidate === productionConnectApiOrigin) return candidate;
  if (candidate === pageOrigin && isLocalDevelopmentOrigin(candidate)) return candidate;
  throw new Error("The device authorization API origin is invalid.");
}

function isLoopbackOrigin(value: string): boolean {
  try {
    const url = new URL(value);
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        url.hostname === "localhost"
        || url.hostname === "127.0.0.1"
        || url.hostname === "[::1]"
      );
  } catch {
    return false;
  }
}

function isLocalDevelopmentOrigin(value: string): boolean {
  if (isLoopbackOrigin(value)) return true;
  try {
    const url = new URL(value);
    if (url.origin !== value) return false;
    const hostname = url.hostname.toLowerCase();
    return (url.protocol === "http:" || url.protocol === "https:")
      && (hostname === "nanocodex.localhost"
        || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(hostname));
  } catch {
    return false;
  }
}

export function deviceUserCode(value: string | null): string {
  if (!value) throw new Error("The device confirmation code is invalid.");
  const normalized = value.replace(/[\s-]/g, "").toUpperCase();
  if (!/^[A-Z]{8}$/.test(normalized)) {
    throw new Error("The device confirmation code is invalid.");
  }
  return normalized;
}

export async function loadPendingDeviceAuthorization(
  url: URL,
  signal?: AbortSignal,
): Promise<PendingDeviceAuthorization> {
  const userCode = deviceUserCode(singleParameter(url, "user_code"));
  const apiOrigin = deviceApiOrigin(singleParameter(url, "api_origin"), url.origin);
  const response = await fetch(
    `${apiOrigin}/v1/device/verify?user_code=${encodeURIComponent(userCode)}`,
    {
      cache: "no-store",
      headers: { accept: "application/json" },
      signal,
    },
  );
  const body = await response.json().catch(() => undefined) as unknown;
  if (!response.ok || !isPendingResponse(body, userCode)) {
    throw new Error(deviceApiError(body, "The device authorization is unavailable or expired."));
  }
  return Object.freeze({ apiOrigin, request: body.request, userCode });
}

export async function settleDeviceAuthorization(
  pending: PendingDeviceAuthorization,
  action: "approve" | "deny",
  result?: unknown,
): Promise<void> {
  const response = await fetch(`${pending.apiOrigin}/v1/device/verify`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      action,
      user_code: pending.userCode,
      ...(action === "approve"
        ? { results: [{ id: pending.request.id, result }] }
        : {}),
    }),
  });
  const body = await response.json().catch(() => undefined) as unknown;
  if (!response.ok) {
    throw new Error(deviceApiError(body, "The device authorization could not be completed."));
  }
}

function isPendingResponse(value: unknown, userCode: string): value is Readonly<{
  app: typeof cliApp;
  request: PendingDeviceAuthorization["request"];
  user_code: string;
}> {
  if (!isRecord(value) || value.user_code !== userCode
    || !isRecord(value.app) || !isRecord(value.request)) return false;
  return value.app.id === cliApp.id
    && value.app.name === cliApp.name
    && value.app.origin === cliApp.origin
    && Object.keys(value.app).every((key) => key === "id" || key === "name" || key === "origin")
    && value.request.jsonrpc === "2.0"
    && (typeof value.request.id === "string" || typeof value.request.id === "number")
    && value.request.method === "wallet_connect";
}

function deviceApiError(value: unknown, fallback: string): string {
  if (!isRecord(value)) return fallback;
  if (typeof value.error_description === "string") return value.error_description;
  if (typeof value.error === "string") return value.error;
  return fallback;
}

function singleParameter(url: URL, name: string): string | null {
  const values = url.searchParams.getAll(name);
  return values.length === 1 ? values[0]! : null;
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
