import {
  sanitizeCliWalletResult,
  sanitizeWalletResult,
} from "./connectPolicy.mjs";

type JsonResponse = Readonly<{ ok: boolean; json(): Promise<unknown> }>;
type FetchJson = (input: string, init: Readonly<{
  body: string;
  credentials: "include";
  headers: Readonly<{ "content-type": "application/json" }>;
  method: "POST";
}>) => Promise<JsonResponse>;

export type ManagedWalletConnect = Readonly<{
  address?: `0x${string}`;
  authToken?: string;
  result: ReturnType<typeof sanitizeWalletResult> | ReturnType<typeof sanitizeCliWalletResult>;
}>;

/**
 * Calls the authenticated, same-origin account Wallet Worker. The Worker is
 * deliberately the only production path for an SMS account: private key
 * material is rejected before the signed public wallet result is returned.
 */
export async function requestManagedWalletConnect(
  request: unknown,
  cli: boolean,
  fetchJson: FetchJson = fetch,
): Promise<ManagedWalletConnect> {
  const response = await fetchJson("/v1/wallet/connect", postBody({ request }));
  const body = await response.json().catch(() => undefined);
  if (!response.ok) throw new Error(workerError(body, "Unable to connect this account."));
  rejectPrivateKeys(body);
  const result = responseResult(body);
  const sanitized = cli ? sanitizeCliWalletResult(result) : sanitizeWalletResult(result);
  return {
    ...(canonicalAddress(body, result) ? { address: canonicalAddress(body, result) } : {}),
    ...(authToken(body, result) ? { authToken: authToken(body, result) } : {}),
    result: sanitized,
  };
}

export async function requestManagedWalletRevocation(
  request: unknown,
  authenticatedAddress: `0x${string}`,
  fetchJson: FetchJson = fetch,
): Promise<void> {
  const requestedAddress = revocationAddress(request);
  if (!requestedAddress || requestedAddress.toLowerCase() !== authenticatedAddress.toLowerCase()) {
    throw new Error("Sign in to the account that owns this access key before revoking it.");
  }
  const response = await fetchJson("/v1/wallet/revoke-access-key", postBody({ request }));
  const body = await response.json().catch(() => undefined);
  if (!response.ok) throw new Error(workerError(body, "Unable to revoke this account key."));
  rejectPrivateKeys(body);
}

function revocationAddress(request: unknown): string | undefined {
  if (!isRecord(request) || request.method !== "wallet_revokeAccessKey"
    || !Array.isArray(request.params) || request.params.length !== 1
    || !isRecord(request.params[0])) return undefined;
  const address = request.params[0].address;
  return typeof address === "string" && /^0x[0-9a-fA-F]{40}$/.test(address)
    ? address
    : undefined;
}

function postBody(value: unknown) {
  return {
    body: JSON.stringify(value),
    credentials: "include" as const,
    headers: { "content-type": "application/json" } as const,
    method: "POST" as const,
  };
}

function responseResult(body: unknown): unknown {
  return isRecord(body) && Object.hasOwn(body, "result") ? body.result : body;
}

function canonicalAddress(body: unknown, result: unknown): `0x${string}` | undefined {
  const value = isRecord(body) && typeof body.address === "string"
    ? body.address
    : isRecord(body) && typeof body.account_address === "string"
      ? body.account_address
      : firstAccountAddress(result);
  return typeof value === "string" && /^0x[0-9a-fA-F]{40}$/.test(value)
    ? value as `0x${string}`
    : undefined;
}

function firstAccountAddress(result: unknown): unknown {
  return isRecord(result) && Array.isArray(result.accounts) && isRecord(result.accounts[0])
    ? result.accounts[0].address
    : undefined;
}

function authToken(body: unknown, result: unknown): string | undefined {
  const topLevel = isRecord(body) && typeof body.token === "string" ? body.token : undefined;
  const account = isRecord(result) && Array.isArray(result.accounts) && isRecord(result.accounts[0])
    ? result.accounts[0]
    : undefined;
  const nested = account && isRecord(account.capabilities) && isRecord(account.capabilities.auth)
    && typeof account.capabilities.auth.token === "string"
    ? account.capabilities.auth.token
    : undefined;
  const value = topLevel ?? nested;
  return value && value.length <= 4_096 ? value : undefined;
}

function rejectPrivateKeys(value: unknown): void {
  if (Array.isArray(value)) {
    value.forEach(rejectPrivateKeys);
    return;
  }
  if (!isRecord(value)) return;
  for (const [key, nested] of Object.entries(value)) {
    if (key.replaceAll(/[-_]/g, "").toLowerCase().includes("privatekey")) {
      throw new Error("The account Worker returned private key material.");
    }
    rejectPrivateKeys(nested);
  }
}

function workerError(body: unknown, fallback: string) {
  return isRecord(body) && isRecord(body.error) && typeof body.error.message === "string"
    ? body.error.message
    : fallback;
}

function isRecord(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
