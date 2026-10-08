import type { Principal } from "./account-auth";
import type { AccountHostedTools } from "./account-hosted-tools";
import { boundedJSON } from "./hand-hosts";
import { requireSameOriginMutation } from "./same-origin-mutation";

const uuid = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
export const handShareDocumentPath = new RegExp(`^/hand-share/(${uuid})$`);
export const handShareAPIPath = new RegExp(`^/v1/account/hand-shares(?:/(redeem|${uuid}))?$`);
const headers = { "cache-control": "no-store", "referrer-policy": "no-referrer", "x-content-type-options": "nosniff" };
const reply = (value: unknown, status = 200) => Response.json(value, { status, headers });

/** Sharing always requires a direct authenticated account and its own permissions. */
export async function routeHandSharing(request: Request, principal: Principal | null | undefined,
  namespace: DurableObjectNamespace<AccountHostedTools>): Promise<Response> {
  const url = new URL(request.url);
  if (!principal) return reply({ error: "unauthorized" }, 401);
  if (!["account_session", "api_key"].includes(principal.kind) || principal.connectGrant
    || !principal.capabilities.includes("tools:use")
    || !principal.capabilities.includes(request.method === "GET" ? "agents:read" : "agents:write")) {
    return reply({ error: "forbidden" }, 403);
  }
  if (url.search) return reply({ error: "invalid_request" }, 400);
  const match = handShareAPIPath.exec(url.pathname);
  if (!match) return reply({ error: "not_found" }, 404);
  const action = match[1];
  const allowed = action === "redeem" ? ["POST"] : action ? ["DELETE"] : ["GET", "POST"];
  if (!allowed.includes(request.method)) return reply({ error: "method_not_allowed" }, 405);
  if (request.method !== "GET") {
    const failure = requireSameOriginMutation(request, url, principal);
    if (failure) return failure;
  }
  const account = namespace.getByName(principal.userId);
  if (request.method === "GET") return reply({ data: await account.listHandShares(principal.userId) });
  if (request.method === "DELETE") {
    const revoked = await account.revokeHandShare(principal.userId, action!);
    return reply(revoked ? { revoked: true } : { error: "not_found" }, revoked ? 200 : 404);
  }
  let body: unknown;
  try { body = await boundedJSON(request); } catch { return reply({ error: "invalid_request" }, 400); }
  if (!body || typeof body !== "object" || Array.isArray(body)) return reply({ error: "invalid_request" }, 400);
  const input = body as Record<string, unknown>;
  if (action === "redeem") {
    if (Object.keys(input).length !== 1 || typeof input.url !== "string") return reply({ error: "invalid_request" }, 400);
    let link: URL;
    try { link = new URL(input.url); } catch { return reply({ error: "invalid_link" }, 400); }
    const owner = handShareDocumentPath.exec(link.pathname)?.[1];
    const token = /^#token=(nhs_[A-Za-z0-9_-]{43})$/.exec(link.hash)?.[1];
    if (link.origin !== url.origin || link.username || link.password || link.search || !owner || !token) {
      return reply({ error: "invalid_link" }, 400);
    }
    const result = await account.redeemHandShare(principal.userId, owner, token);
    return reply(result, "error" in result ? result.error === "share_limit" ? 409 : 404 : 200);
  }
  if (Object.keys(input).length !== 1 || typeof input.machine_id !== "string"
    || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(input.machine_id)) return reply({ error: "invalid_request" }, 400);
  const result = await account.createHandShare(principal.userId, input.machine_id);
  if ("error" in result) return reply(result, ["limit_reached", "share_limit"].includes(result.error ?? "") ? 409 : 404);
  const { token, ...metadata } = result;
  return reply({ ...metadata, url: `${url.origin}/hand-share/${principal.userId}#token=${token}` }, 201);
}

/** The token stays in the fragment until the recipient explicitly redeems it. */
export function handShareDocument(request: Request): Response {
  if (request.method !== "GET" && request.method !== "HEAD") return reply({ error: "method_not_allowed" }, 405);
  const nonce = crypto.randomUUID();
  return new Response(request.method === "HEAD" ? null : `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Add a shared Hand · Nanocodex</title><body><main><h1>Add a shared Hand</h1><p>This link gives your signed-in Nanocodex account access to a computer. Its owner can revoke access at any time.</p><p><a href="/" target="_blank" rel="noopener noreferrer">Sign in to Nanocodex</a>, then return here.</p><button id="redeem">Add Hand to my account</button><p id="status" role="status"></p></main><script nonce="${nonce}">
const button = document.getElementById('redeem'), status = document.getElementById('status');
button.addEventListener('click', async () => {
  button.disabled = true; status.textContent = 'Adding Hand…';
  try {
    const response = await fetch('/v1/account/hand-shares/redeem', {method:'POST', credentials:'same-origin', headers:{'content-type':'application/json'}, body:JSON.stringify({url:location.href})});
    const value = await response.json();
    if (!response.ok) { status.textContent = response.status === 401 ? 'Sign in first, then try again.' : response.status === 403 ? 'Your account does not have permission to add this Hand.' : 'This link is invalid or has been revoked.'; button.disabled = false; return; }
    history.replaceState(null, '', location.pathname); status.textContent = 'Hand added. Open Nanocodex to use it. Machine: ' + value.machine_id;
  } catch { status.textContent = 'Could not confirm the result. You can safely try adding this link again.'; button.disabled = false; }
});</script></body></html>`, { headers: { ...headers, "content-type": "text/html; charset=utf-8",
    "x-frame-options": "DENY", "content-security-policy": `default-src 'none'; script-src 'nonce-${nonce}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'` } });
}
