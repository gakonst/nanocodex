import type { AccountHostedTools, HandDeviceAccountRequest } from "./account-hosted-tools";
import type { Principal } from "./account-auth";
import { isUserId } from "./account-auth";
import { HAND_MACHINE_HEADER, HAND_RUNTIME_HEADER } from "./hand-directory";
import { isHandDeviceAuthorization, parseHandDeviceCredential, validHandDeviceId, type HandDeviceResult } from "./hand-devices";

const OWNER_ASSERTION = "x-nanocodex-owner-id";
export const HAND_DEVICE_ORIGIN_HEADER = "x-nanocodex-hand-device-origin";
const noStore = { "cache-control": "no-store" };
/** Route marker lets clients distinguish this API's errors from an older server's missing route. */
const marked = { ...noStore, "x-nanocodex-hand-devices": "v1" };
const reply = (body: unknown, status: number, headers: Record<string, string> = noStore) => Response.json(body, { status, headers });
const HAND_HOST_PUBLISHER = /^\/v1\/hand-hosts\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/hands\/(?:host|ice|renew|device)$/;
const ACCOUNT_ROUTE = /^\/v1\/account\/hand-devices(?:\/(challenges|policy|[0-9a-f-]{36}))?$/;
const POSSESSION_ROUTE = /^\/v1\/hand-devices\/([0-9a-f-]{36})\/([0-9a-f-]{36})\/(challenges|credentials|rotate|ssh-host-keys)$/;

export type HandDeviceRouteDependencies = Readonly<{
  tools: DurableObjectNamespace<AccountHostedTools>;
  authenticate(): Promise<Principal | undefined>;
  /** Live account authorization for credential issuance; undefined when the account cannot publish. */
  resolveAccount(ownerId: string, deviceId: string): Promise<Principal | undefined>;
  /** Device-credential VM factory attachment for the owner, bound to the authenticating device. */
  vmHost(ownerId: string, device: Readonly<{ device_id: string; machine_id: string; key_version: number }>): Promise<Response>;
}>;

async function body(request: Request, optional: boolean): Promise<unknown> {
  const reader = request.body?.getReader();
  if (!reader) { if (optional) return undefined; throw new Error("body required"); }
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 2048) throw new Error("body too large");
      chunks.push(value);
    }
  } finally { await reader.cancel(); reader.releaseLock(); }
  if (size === 0) { if (optional) return undefined; throw new Error("body required"); }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
}

const result = (value: HandDeviceResult) => reply(value.body, value.status, marked);

/**
 * Hand device routes. Device credentials (ncxhd1) are accepted only by Hand
 * publisher endpoints; every other route rejects them before account
 * authentication runs. Server grants (ncxhg1) are accepted only by the
 * HandHosts device enrollment endpoint, routed by the existing publisher path.
 */
export async function routeHandDevices(request: Request, url: URL, deps: HandDeviceRouteDependencies): Promise<Response | undefined> {
  const authorization = request.headers.get("authorization");
  if (isHandDeviceAuthorization(authorization) && !HAND_HOST_PUBLISHER.test(url.pathname)) {
    const hands = url.pathname.match(/^\/v1\/account\/hands\/(host|ice|renew)$/);
    const endpoint = url.pathname === "/v1/account/tool-host" ? "tool-host"
      : url.pathname === "/v1/account/vm-host" ? "vm-host" : hands ? "hands/" + hands[1] : undefined;
    const parsed = parseHandDeviceCredential(authorization);
    if (!endpoint || !parsed) return reply({ error: "unauthorized" }, 401);
    if (url.search) return reply({ error: "invalid_request" }, 400);
    if (endpoint === "vm-host") {
      if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const authorized = await deps.tools.getByName(parsed.ownerId).authorizeHandDevice(parsed.ownerId, authorization!);
      if (!authorized) return reply({ error: "unauthorized" }, 401);
      return deps.vmHost(parsed.ownerId, { device_id: authorized.device_id, machine_id: authorized.machine_id, key_version: authorized.key_version });
    }
    // A dedicated device scope: never forward account capability or principal assertions.
    const headers = new Headers();
    for (const name of ["authorization", "upgrade", "content-type", HAND_MACHINE_HEADER, HAND_RUNTIME_HEADER]) {
      const value = request.headers.get(name);
      if (value !== null) headers.set(name, value);
    }
    headers.set(OWNER_ASSERTION, parsed.ownerId);
    return deps.tools.getByName(parsed.ownerId).fetch("https://account-tools.internal/hand-device/" + endpoint,
      new Request(request, { headers }));
  }
  const account = url.pathname.match(ACCOUNT_ROUTE);
  if (account) {
    if (url.search) return reply({ error: "invalid_request" }, 400, marked);
    const sub = account[1];
    let operation: HandDeviceAccountRequest["operation"] | undefined;
    if (sub === undefined && request.method === "GET") operation = "list";
    else if (sub === undefined && request.method === "POST") operation = "enroll";
    else if (sub === "challenges" && request.method === "POST") operation = "challenge";
    else if (sub === "policy" && request.method === "PUT") operation = "policy";
    else if (sub !== undefined && validHandDeviceId(sub) && request.method === "DELETE") operation = "revoke";
    if (!operation) return reply({ error: "method_not_allowed" }, 405, marked);
    const write = operation !== "list";
    // Internal agent principals never enroll, revoke or relax policy: authenticate the request itself.
    const principal = await deps.authenticate();
    if (!principal) return reply({ error: "unauthorized" }, 401, marked);
    // Only a live signed-in account or its API key; never Connect grants or service principals.
    if (!["api_key", "account_session"].includes(principal.kind) || principal.connectGrant
      || !principal.capabilities.includes(write ? "agents:write" : "agents:read")
      || !principal.capabilities.includes("tools:use")) return reply({ error: "forbidden" }, 403, marked);
    if (write && principal.kind !== "api_key" && request.headers.get("origin") !== url.origin) {
      return reply({ error: "forbidden_origin" }, 403, marked);
    }
    let value: unknown;
    if (operation === "enroll" || operation === "challenge" || operation === "policy") {
      try { value = await body(request, operation === "challenge"); }
      catch { return reply({ error: "invalid_hand_device_request" }, 400, marked); }
    }
    return result(await deps.tools.getByName(principal.userId).handDeviceAccount(principal.userId, {
      operation, origin: url.origin, body: value, ...(operation === "revoke" ? { device_id: sub } : {}),
      enrolled_by: { kind: principal.kind, organization_id: principal.organizationId, team_id: principal.teamId,
        authorization_epoch: principal.authorizationEpoch },
      session_principal: principal.kind === "account_session",
    }));
  }
  const possession = url.pathname.match(POSSESSION_ROUTE);
  if (possession) {
    if (url.search) return reply({ error: "invalid_request" }, 400, marked);
    const [, ownerId, deviceId, operation] = possession as unknown as [string, string, string, "challenges" | "credentials" | "rotate" | "ssh-host-keys"];
    if (request.method !== (operation === "ssh-host-keys" ? "PUT" : "POST")) return reply({ error: "method_not_allowed" }, 405, marked);
    if (!isUserId(ownerId) || !validHandDeviceId(deviceId)) return reply({ error: "hand_reenroll_required" }, 401, marked);
    let value: unknown;
    try { value = await body(request, false); }
    catch { return reply({ error: "invalid_hand_device_request" }, 400, marked); }
    let account: { organization_id?: string; team_id?: string } = {};
    if (operation === "credentials") {
      // Credential issuance re-checks live account authorization.
      const principal = await deps.resolveAccount(ownerId, deviceId).catch(() => undefined);
      if (!principal || !principal.capabilities.includes("agents:write") || !principal.capabilities.includes("tools:use")) {
        return reply({ error: "hand_device_account_unauthorized" }, 403, marked);
      }
      account = { organization_id: principal.organizationId, team_id: principal.teamId };
    }
    return result(await deps.tools.getByName(ownerId).handDevicePossession(ownerId, deviceId, operation, value, url.origin, account));
  }
  return undefined;
}
