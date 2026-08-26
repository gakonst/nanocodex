import { Kv } from "accounts/server";

import {
  authenticatePersistentAccount,
  isUserId,
  type AccountAuthEnv,
} from "./account-auth";

const CONNECT_DIALOG_ORIGIN = "https://nanocodex.gakonst.workers.dev";
const CONNECT_APP_ID = "atlas-workspace";
const INTERNAL_ORIGIN = "https://nanocodex.internal";
const NANOCODEX_ORIGIN = "https://nanocodex.gakonst.workers.dev";
const AUTHORIZATION_TTL_SECONDS = 5 * 60;
const ADDRESS = /^0x[0-9a-fA-F]{40}$/;
const OPAQUE_TOKEN = /^[A-Za-z0-9_-]{43}$/;

type AuthorizationIntent = Readonly<{
  accountAddress: string;
  appId: string;
  returnOrigin: string;
  state: string;
  userId: string;
}>;

type AuthorizationCode = Readonly<{
  accountAddress: string;
  appId: string;
  state: string;
  userId: string;
}>;

type AccountLink = Readonly<{
  accountAddress: string;
  linkedAt: number;
  userId: string;
}>;

export async function routeAccountLinkRequest(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.origin === INTERNAL_ORIGIN) {
    if (url.pathname === "/connect/account-links/resolve") {
      return resolveAccountLink(request, env, url);
    }
    if (url.pathname === "/connect/account-links/exchange") {
      return exchangeAuthorizationCode(request, env);
    }
    return undefined;
  }

  if (url.pathname === "/v1/connect/account-link") {
    if (request.method === "GET") return accountLinkConfirmation(request, env, url);
    if (request.method === "POST") return authorizeAccountLink(request, env, url);
    return json({ error: "method_not_allowed" }, { status: 405 });
  }
  if (url.pathname === "/v1/connect/account-link/authorize") {
    if (request.method === "POST") return authorizeAccountLinkDirect(request, env, url);
    return json({ error: "method_not_allowed" }, { status: 405 });
  }

  const unlink = url.pathname.match(/^\/v1\/connect\/account-links\/(0x[0-9a-fA-F]{40})$/);
  if (!unlink) return undefined;
  if (request.method !== "DELETE") return json({ error: "method_not_allowed" }, { status: 405 });
  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, { status: 401 });
  const originFailure = requireNanocodexOrigin(request, url);
  if (originFailure) return originFailure;
  const accountAddress = normalizeAddress(unlink[1]!);
  const store = accountLinkStore(env);
  const link = await store.get<AccountLink>(linkKey(accountAddress));
  if (link?.userId === principal.userId) await store.delete(linkKey(accountAddress));
  return new Response(null, { status: 204, headers: noStoreHeaders() });
}

async function accountLinkConfirmation(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response> {
  const parameters = authorizationParameters(url.searchParams, expectedConnectDialogOrigin(url));
  if (!parameters) return accountLinkPage({ kind: "error", message: "This link request is invalid." }, 400);
  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) {
    return accountLinkPage({
      kind: "error",
      message: "Sign in to your Nanocodex profile in this browser, then try again.",
    }, 401);
  }
  const intent = randomToken();
  const store = accountLinkStore(env);
  if (!store.create || !await store.create(`intent:${intent}`, {
    ...parameters,
    userId: principal.userId,
  } satisfies AuthorizationIntent, { ttl: AUTHORIZATION_TTL_SECONDS })) {
    return accountLinkPage({ kind: "error", message: "This link request could not be reserved." }, 503);
  }
  return accountLinkPage({
    kind: "confirm",
    accountAddress: parameters.accountAddress,
    intent,
  });
}

async function authorizeAccountLink(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response> {
  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return accountLinkPage({ kind: "error", message: "Your Nanocodex session expired." }, 401);
  const originFailure = requireNanocodexOrigin(request, url);
  if (originFailure) return originFailure;
  const contentLength = Number(request.headers.get("content-length") ?? 0);
  if (!Number.isFinite(contentLength) || contentLength > 4_096) {
    return accountLinkPage({ kind: "error", message: "This link request is invalid." }, 413);
  }
  let intentToken: unknown;
  try {
    intentToken = (await request.formData()).get("intent");
  } catch {
    return accountLinkPage({ kind: "error", message: "This link request is invalid." }, 400);
  }
  if (typeof intentToken !== "string" || !OPAQUE_TOKEN.test(intentToken)) {
    return accountLinkPage({ kind: "error", message: "This link request is invalid." }, 400);
  }
  const store = accountLinkStore(env);
  if (!store.take) return accountLinkPage({ kind: "error", message: "One-time authorization is unavailable." }, 503);
  const intent = await store.take<AuthorizationIntent>(`intent:${intentToken}`);
  if (!validIntent(intent) || intent.userId !== principal.userId) {
    return accountLinkPage({ kind: "error", message: "This link request expired or was already used." }, 400);
  }
  const code = await issueAuthorizationCode(store, intent, intent.userId);
  if (!code) return accountLinkPage({ kind: "error", message: "This authorization could not be completed." }, 503);
  return accountLinkPage({
    kind: "complete",
    code,
    returnOrigin: intent.returnOrigin,
    state: intent.state,
  });
}

async function authorizeAccountLinkDirect(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response> {
  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, { status: 401 });
  const originFailure = requireNanocodexOrigin(request, url);
  if (originFailure) return originFailure;
  const parameters = authorizationParameters(url.searchParams, expectedConnectDialogOrigin(url));
  if (!parameters) return json({ error: "invalid_account_link" }, { status: 400 });
  const code = await issueAuthorizationCode(accountLinkStore(env), parameters, principal.userId);
  return code
    ? json({ code, state: parameters.state })
    : json({ error: "authorization_unavailable" }, { status: 503 });
}

async function issueAuthorizationCode(
  store: Kv.Kv,
  parameters: Omit<AuthorizationIntent, "returnOrigin" | "userId">,
  userId: string,
): Promise<string | undefined> {
  const code = randomToken();
  if (!store.create || !await store.create(`code:${await sha256(code)}`, {
    accountAddress: parameters.accountAddress,
    appId: parameters.appId,
    state: parameters.state,
    userId,
  } satisfies AuthorizationCode, { ttl: AUTHORIZATION_TTL_SECONDS })) return undefined;
  return code;
}

async function resolveAccountLink(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response> {
  if (request.method !== "GET") return json({ error: "method_not_allowed" }, { status: 405 });
  const accountAddress = parseAddress(url.searchParams.get("account_address"));
  if (!accountAddress) return json({ error: "invalid_account_address" }, { status: 400 });
  const link = await accountLinkStore(env).get<AccountLink>(linkKey(accountAddress));
  if (!validLink(link) || link.accountAddress !== accountAddress) {
    return json({ error: "not_found" }, { status: 404 });
  }
  return json({ linked: true, user_id: link.userId });
}

async function exchangeAuthorizationCode(
  request: Request,
  env: AccountAuthEnv,
): Promise<Response> {
  if (request.method !== "POST") return json({ error: "method_not_allowed" }, { status: 405 });
  const encoded = await request.text();
  if (encoded.length > 4_096) return json({ error: "request_too_large" }, { status: 413 });
  let body: Record<string, unknown>;
  try {
    const value = JSON.parse(encoded) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
    body = value as Record<string, unknown>;
  } catch {
    return json({ error: "invalid_json" }, { status: 400 });
  }
  const code = typeof body.code === "string" && OPAQUE_TOKEN.test(body.code) ? body.code : undefined;
  const state = typeof body.state === "string" && OPAQUE_TOKEN.test(body.state) ? body.state : undefined;
  const appId = body.app_id === CONNECT_APP_ID ? CONNECT_APP_ID : undefined;
  const accountAddress = parseAddress(body.account_address);
  if (!code || !state || !appId || !accountAddress) {
    return json({ error: "invalid_exchange" }, { status: 400 });
  }
  const store = accountLinkStore(env);
  if (!store.take) return json({ error: "one_time_exchange_unavailable" }, { status: 503 });
  const codeKey = `code:${await sha256(code)}`;
  const candidate = await store.get<AuthorizationCode>(codeKey);
  if (!validCode(candidate)
    || candidate.accountAddress !== accountAddress
    || candidate.appId !== appId
    || candidate.state !== state) {
    return json({ error: "invalid_authorization_code" }, { status: 403 });
  }
  const current = await store.get<AccountLink>(linkKey(accountAddress));
  if (validLink(current) && current.userId !== candidate.userId) {
    return json({ error: "account_already_linked" }, { status: 409 });
  }
  const authorization = await store.take<AuthorizationCode>(codeKey);
  if (!validCode(authorization)
    || authorization.accountAddress !== candidate.accountAddress
    || authorization.appId !== candidate.appId
    || authorization.state !== candidate.state
    || authorization.userId !== candidate.userId) {
    return json({ error: "invalid_authorization_code" }, { status: 403 });
  }
  const link: AccountLink = {
    accountAddress,
    linkedAt: Date.now(),
    userId: authorization.userId,
  };
  if (!validLink(current)) {
    if (!store.create || !await store.create(linkKey(accountAddress), link)) {
      const winner = await store.get<AccountLink>(linkKey(accountAddress));
      if (!validLink(winner) || winner.userId !== link.userId) {
        return json({ error: "account_already_linked" }, { status: 409 });
      }
    }
  }
  return json({ linked: true, user_id: link.userId });
}

function authorizationParameters(
  parameters: URLSearchParams,
  expectedReturnOrigin: string,
): Omit<AuthorizationIntent, "userId"> | undefined {
  const accountAddress = parseAddress(parameters.get("account_address"));
  const state = parameters.get("state");
  const appId = parameters.get("app_id");
  const returnOrigin = parameters.get("return_origin");
  if (!accountAddress || !state || !OPAQUE_TOKEN.test(state)
    || appId !== CONNECT_APP_ID || returnOrigin !== expectedReturnOrigin) return undefined;
  return { accountAddress, appId, returnOrigin, state };
}

function accountLinkStore(env: AccountAuthEnv): Kv.Kv {
  return Kv.durableObject(env.NANOCODEX_AUTH as unknown as Kv.durableObject.Namespace, {
    name: "connect-account-links",
  });
}

function linkKey(accountAddress: string): string {
  return `account:${accountAddress}`;
}

function parseAddress(value: unknown): string | undefined {
  return typeof value === "string" && ADDRESS.test(value) ? normalizeAddress(value) : undefined;
}

function normalizeAddress(value: string): string {
  return `0x${value.slice(2).toLowerCase()}`;
}

function validIntent(value: unknown): value is AuthorizationIntent {
  return validCode(value)
    && isAllowedConnectDialogOrigin((value as Partial<AuthorizationIntent>).returnOrigin);
}

function validCode(value: unknown): value is AuthorizationCode {
  if (!value || typeof value !== "object") return false;
  const record = value as Partial<AuthorizationCode>;
  return parseAddress(record.accountAddress) === record.accountAddress
    && record.appId === CONNECT_APP_ID
    && typeof record.state === "string"
    && OPAQUE_TOKEN.test(record.state)
    && isUserId(record.userId);
}

function validLink(value: unknown): value is AccountLink {
  if (!value || typeof value !== "object") return false;
  const record = value as Partial<AccountLink>;
  return parseAddress(record.accountAddress) === record.accountAddress
    && Number.isSafeInteger(record.linkedAt)
    && isUserId(record.userId);
}

type AccountLinkPage =
  | Readonly<{ kind: "confirm"; accountAddress: string; intent: string }>
  | Readonly<{ kind: "complete"; code: string; returnOrigin: string; state: string }>
  | Readonly<{ kind: "error"; message: string }>;

function accountLinkPage(page: AccountLinkPage, status = 200): Response {
  const nonce = randomToken();
  const body = page.kind === "confirm"
    ? `<p class="eyebrow">Nanocodex Connect</p><h1>Use this profile</h1><p class="copy">Share your connected services with Atlas Workspace under the signed Connect grant.</p><div class="account"><span>Tempo account</span><strong>${shortAddress(page.accountAddress)}</strong></div><form method="post"><input type="hidden" name="intent" value="${page.intent}"><button type="submit">Use Nanocodex profile</button></form>`
    : page.kind === "complete"
      ? `<p class="eyebrow">Nanocodex Connect</p><h1>Profile linked</h1><p class="copy">Your existing connectors are ready in the Connect approval.</p><script nonce="${nonce}">window.opener?.postMessage(${JSON.stringify({ type: "nanocodex:account-link", code: page.code, state: page.state })},${JSON.stringify(page.returnOrigin)});window.close();</script>`
      : `<p class="eyebrow">Nanocodex Connect</p><h1>Could not link profile</h1><p class="copy">${escapeHtml(page.message)}</p>`;
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Nanocodex Connect</title><style nonce="${nonce}">:root{color-scheme:dark;font-family:"Berkeley Mono",ui-monospace,SFMono-Regular,Menlo,monospace;background:#0d0d0d;color:#fff}*{box-sizing:border-box}body{display:grid;min-height:100vh;place-items:center;margin:0;padding:20px;background:radial-gradient(circle at 50% 0,rgba(115,216,155,.08),transparent 38%),#0d0d0d}.card{width:min(390px,100%);border:1px solid rgba(255,255,255,.18);padding:28px;background:#141414;box-shadow:0 28px 80px rgba(0,0,0,.48)}.eyebrow{margin:0 0 12px;color:#73d89b;font-size:10px;text-transform:uppercase;letter-spacing:.08em}h1{margin:0;font-size:22px;font-weight:500;letter-spacing:-.05em}.copy{margin:9px 0 22px;color:rgba(255,255,255,.58);font-size:11px;line-height:1.6}.account{display:flex;justify-content:space-between;gap:20px;margin-bottom:18px;padding:12px 0;border-block:1px solid rgba(255,255,255,.1);font-size:10px}.account span{color:rgba(255,255,255,.45)}.account strong{font-weight:500}button{width:100%;min-height:44px;border:1px solid #fff;border-radius:0;background:#fff;color:#000;font:inherit;font-size:10px;font-weight:600;cursor:pointer}button:hover{background:#ddd}button:focus-visible{outline:1px solid #73d89b;outline-offset:3px}</style></head><body><main class="card">${body}</main></body></html>`;
  return new Response(html, {
    status,
    headers: {
      ...noStoreHeaders(),
      "content-security-policy": `default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}'`,
      "content-type": "text/html; charset=utf-8",
      // Chromium serializes the Origin of a same-origin form POST as `null`
      // under `no-referrer`, which makes the explicit mutation check fail.
      // This page has no cross-origin resources or links, so `same-origin`
      // keeps the approval query private while preserving the public origin.
      "referrer-policy": "same-origin",
      "x-frame-options": "DENY",
    },
  });
}

function shortAddress(value: string): string {
  return `${value.slice(0, 8)}…${value.slice(-6)}`;
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    "\"": "&quot;",
    "'": "&#39;",
  })[character]!);
}

function randomToken(): string {
  const value = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function sha256(value: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function noStoreHeaders(): Record<string, string> {
  return {
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  };
}

function requireNanocodexOrigin(request: Request, url: URL): Response | undefined {
  const expected = isLocalDevelopmentOrigin(url.origin) || url.hostname.endsWith(".test")
    ? url.origin
    : NANOCODEX_ORIGIN;
  return request.headers.get("origin") === expected
    ? undefined
    : json({ error: "forbidden_origin" }, { status: 403 });
}

function expectedConnectDialogOrigin(url: URL): string {
  return isLocalDevelopmentOrigin(url.origin) ? url.origin : CONNECT_DIALOG_ORIGIN;
}

function isAllowedConnectDialogOrigin(value: unknown): value is string {
  return value === CONNECT_DIALOG_ORIGIN
    || (typeof value === "string" && isLocalDevelopmentOrigin(value));
}

function isLocalDevelopmentOrigin(value: string): boolean {
  return /^https?:\/\/(?:[a-z0-9-]+\.)*nanocodex\.localhost(?::\d+)?$/.test(value);
}

function json(body: unknown, init: ResponseInit = {}): Response {
  return Response.json(body, {
    ...init,
    headers: { ...noStoreHeaders(), ...init.headers },
  });
}
