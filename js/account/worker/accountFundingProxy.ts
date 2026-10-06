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
 * account's active payment wallet. Account cookies never reach Connect API.
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
  const isOrder = url.pathname === ORDER_PATH || ORDER_STATUS_PATH.test(url.pathname);
  const account = isOrder ? await persistentAccount(request, env, url) : undefined;
  if (isOrder && !account) return json({ error: "authentication_required" }, 401);
  let expectedCents: number | undefined;
  if (url.pathname === ORDER_PATH && request.method === "POST") {
    if (request.headers.get("origin") !== url.origin) {
      return json({ error: "origin_denied" }, 403);
    }
    if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
      return json({ error: "invalid_request" }, 415);
    }
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ error: "invalid_request" }, 400);
    }
    if (!isRecord(body) || !Number.isSafeInteger(body.usd_amount_cents)
      || (body.usd_amount_cents as number) < 500 || (body.usd_amount_cents as number) > 10_000
      || typeof body.order_token !== "string" || !body.order_token || body.order_token.length > 255
      || body.payment_mode !== "hosted_checkout") return json({ error: "invalid_request" }, 400);
    // A wallet switch must never redirect a checkout the user prepared for another address.
    if (typeof body.wallet_address !== "string" || body.wallet_address.toLowerCase() !== account!.address) {
      return json({ error: "wallet_changed" }, 409);
    }
    const key = request.headers.get("idempotency-key");
    if (!key || key.length > 180) return json({ error: "invalid_request" }, 400);
    expectedCents = body.usd_amount_cents as number;
    // Keep retries stable and prevent another account from colliding with this key.
    headers.set("idempotency-key", `nanocodex:${account!.address}:${key}`);
    headers.set("content-type", "application/json");
    upstreamRequest = new Request(url, {
      method: "POST",
      headers,
      body: JSON.stringify({
        order_token: body.order_token,
        payment_mode: body.payment_mode,
        usd_amount_cents: body.usd_amount_cents,
        wallet_address: account!.address,
      }),
    });
  } else {
    upstreamRequest = new Request(url, { method: request.method, headers });
  }

  try {
    const response = await env.NANOCODEX_CONNECT_API.fetch(upstreamRequest);
    if (!response.ok || !isOrder) return response;
    const value: unknown = await response.json();
    const order = isRecord(value) && isRecord(value.order) ? value.order : undefined;
    if (!order || typeof order.id !== "string" || !/^ord_[0-9a-f]{32}$/.test(order.id)
      || typeof order.status !== "string" || !["requires_payment", "processing", "issuing", "complete", "failed"].includes(order.status)
      || typeof order.wallet_address !== "string"
      || order.wallet_address.toLowerCase() !== account!.address
      || !Number.isSafeInteger(order.usd_amount_cents)
      || (order.usd_amount_cents as number) < 500 || (order.usd_amount_cents as number) > 10_000
      || order.mach_amount_atomics !== (order.usd_amount_cents as number) * 10_000
      || (expectedCents !== undefined && order.usd_amount_cents !== expectedCents)
      || (request.method === "GET" && order.id !== url.pathname.split("/").at(-1))
      || (order.status === "complete" && (order.issuance_status !== "fulfilled"
        || typeof order.issuance_transaction_hash !== "string"
        || !/^0x[0-9a-f]{64}$/i.test(order.issuance_transaction_hash)))) {
      return json({ error: request.method === "GET" ? "order_not_found" : "invalid_funding_order" }, request.method === "GET" ? 404 : 502);
    }
    if (request.method === "POST") {
      const payment = isRecord(value) && isRecord(value.payment) ? value.payment : undefined;
      if (!payment || payment.provider !== "stripe" || payment.mode !== "hosted_checkout"
        || typeof payment.checkout_url !== "string" || !isCheckoutUrl(payment.checkout_url)) {
        return json({ error: "invalid_funding_order" }, 502);
      }
    }
    return json(value, response.status);
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
    || !ADDRESS.test(body.user.address) || /^0x0{40}$/i.test(body.user.address)) return undefined;
  // The sign-in address remains the original account identity after linking.
  // Resolve the selected payment wallet with the same owner session.
  try {
    const walletResponse = await env.NANOCODEX_BACKEND.fetch(new Request(new URL("/v1/wallet", url), { method: "GET", headers }));
    if (!walletResponse.ok) { await walletResponse.body?.cancel(); return undefined; }
    const wallet: unknown = await walletResponse.json();
    if (!isRecord(wallet) || typeof wallet.address !== "string" || !ADDRESS.test(wallet.address)
      || /^0x0{40}$/i.test(wallet.address)) return undefined;
    return { address: wallet.address.toLowerCase() };
  } catch { return undefined; }
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

function isCheckoutUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.origin === "https://checkout.stripe.com" && !url.username && !url.password;
  } catch { return false; }
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
