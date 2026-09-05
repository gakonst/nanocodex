export type AccountFundingProxyEnv = {
  ENVIRONMENT?: string;
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_CONNECT_API?: Fetcher;
};

const ADDRESS = /^0x[0-9a-f]{40}$/i;
const ORDER_PATH = "/v1/machine-usd/orders";
const ORDER_STATUS_PATH = /^\/v1\/machine-usd\/orders\/[A-Za-z0-9_-]+$/;

/**
 * Projects the public onramp while binding order creation to the persistent
 * account's Worker-owned wallet. Account cookies never reach Connect API.
 */
export async function routeAccountFunding(
  request: Request,
  env: AccountFundingProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (!isAccountFundingPath(url.pathname)) return undefined;
  if (!env.NANOCODEX_CONNECT_API) return unavailable();

  const headers = upstreamHeaders(request, env, url);
  let upstreamRequest: Request;
  if (url.pathname === ORDER_PATH && request.method === "POST") {
    const account = await persistentAccount(request, env, url);
    if (!account) return json({ error: "authentication_required" }, 401);
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ error: "invalid_request" }, 400);
    }
    if (!isRecord(body)) return json({ error: "invalid_request" }, 400);
    headers.set("content-type", "application/json");
    upstreamRequest = new Request(url, {
      method: "POST",
      headers,
      body: JSON.stringify({
        order_token: body.order_token,
        payment_mode: body.payment_mode,
        usd_amount_cents: body.usd_amount_cents,
        wallet_address: account.address,
      }),
    });
  } else {
    upstreamRequest = new Request(url, { method: request.method, headers });
  }

  try {
    return await env.NANOCODEX_CONNECT_API.fetch(upstreamRequest);
  } catch {
    return unavailable();
  }
}

export function isAccountFundingPath(pathname: string): boolean {
  return pathname === "/v1/machine-usd/config"
    || pathname === ORDER_PATH
    || ORDER_STATUS_PATH.test(pathname);
}

async function persistentAccount(
  request: Request,
  env: AccountFundingProxyEnv,
  url: URL,
): Promise<{ address: string } | undefined> {
  if (!env.NANOCODEX_BACKEND) return undefined;
  const headers = new Headers({ accept: "application/json" });
  const cookie = request.headers.get("cookie");
  if (cookie) headers.set("cookie", cookie);
  let response: Response;
  try {
    response = await env.NANOCODEX_BACKEND.fetch(new Request(new URL("/v1/me", url), {
      method: "GET",
      headers,
    }));
  } catch {
    return undefined;
  }
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const body: unknown = await response.json().catch(() => undefined);
  if (!isRecord(body)
    || body.authentication !== "account_session"
    || !isRecord(body.user)
    || body.user.persistent !== true
    || typeof body.user.address !== "string"
    || !ADDRESS.test(body.user.address)) return undefined;
  return { address: body.user.address.toLowerCase() };
}

function upstreamHeaders(request: Request, env: AccountFundingProxyEnv, url: URL): Headers {
  const headers = new Headers({
    accept: "application/json",
    origin: url.origin,
  });
  for (const name of ["authorization", "idempotency-key"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  if (env.ENVIRONMENT === "development") {
    headers.set("x-nanocodex-local-origin", url.origin);
  }
  return headers;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function unavailable(): Response {
  return json({ error: "machine_usd_unavailable" }, 503);
}

function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
