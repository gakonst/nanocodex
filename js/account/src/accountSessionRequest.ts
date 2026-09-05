export type AuthenticatedAccount = Readonly<{
  address?: `0x${string}` | undefined;
  id: string;
  persistent: boolean;
}>;

const USER_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export class ReauthenticationRequiredError extends Error {
  constructor() {
    super("Your session expired. Sign in by SMS to restore your account.");
    this.name = "ReauthenticationRequiredError";
  }
}

export function getCurrentUser(fetcher: typeof fetch = fetch): Promise<AuthenticatedAccount | null> {
  return readCurrentUser(fetcher, true);
}

async function readCurrentUser(
  fetcher: typeof fetch,
  recoverInvalidSession: boolean,
): Promise<AuthenticatedAccount | null> {
  const response = await fetcher("/v1/me", {
    cache: "no-store",
    credentials: "same-origin",
    headers: { accept: "application/json" },
  });
  if (response.status === 401) {
    const body: unknown = await response.json().catch(() => undefined);
    if (isRecord(body) && body.error === "reauthentication_required") {
      throw new ReauthenticationRequiredError();
    }
    if (isRecord(body) && body.error === "invalid_session") {
      if (recoverInvalidSession) return readCurrentUser(fetcher, false);
      throw new Error("Couldn’t renew your browser session. Reload and try again.");
    }
    return null;
  }
  if (!response.ok) throw await responseFailure(response, "Account service unavailable.");
  const body: unknown = await response.json();
  if (!isRecord(body) || !isRecord(body.user)) throw new Error("Invalid account response.");
  const { address, id, persistent } = body.user;
  if (
    typeof id !== "string"
    || !USER_ID.test(id)
    || (address !== undefined && (typeof address !== "string" || !/^0x[0-9a-f]{40}$/.test(address)))
    || typeof persistent !== "boolean"
  ) throw new Error("Invalid account response.");
  return { ...(address ? { address: address as `0x${string}` } : {}), id, persistent };
}

export async function responseFailure(response: Response, fallback: string): Promise<Error> {
  const body: unknown = await response.json().catch(() => undefined);
  const reason = isRecord(body) && typeof body.error === "string"
    ? body.error.replaceAll("_", " ")
    : fallback;
  return new Error(reason);
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
