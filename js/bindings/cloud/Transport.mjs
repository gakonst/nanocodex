import { HttpError, InvalidResponseError } from "./Errors.mjs";
import { Hex, PersonalMessage } from "ox";

export const DEFAULT_API_URL = "https://api.nanocodex.xyz";
export const MOCK_ACCOUNT_ADDRESS = "0x8ba1f109551bd432803012645ac136ddd64dba72";
export const MOCK_MACHINE_USD_ADDRESS = "0x20c0000000000000000000000000000000000001";

export function from(parameters) {
  if (!parameters || typeof parameters !== "object") {
    throw new TypeError("Transport.from requires parameters");
  }
  if (typeof parameters.setup !== "function") {
    throw new TypeError("Transport.from requires setup");
  }
  return Object.freeze({
    key: requiredString(parameters.key, "transport key"),
    name: requiredString(parameters.name, "transport name"),
    type: requiredString(parameters.type, "transport type"),
    setup: parameters.setup,
  });
}

export function http(url = DEFAULT_API_URL, options = {}) {
  const baseUrl = new URL(url).toString();
  const fetchFn = options.fetch ?? globalThis.fetch;
  const credentials = options.credentials ?? "include";
  if (typeof fetchFn !== "function") throw new TypeError("http transport requires fetch");
  return from({
    key: options.key ?? "http",
    name: options.name ?? "Nanocodex HTTP",
    type: "http",
    setup({ appId }) {
      return {
        baseUrl,
        fetch(input, init) {
          const headers = new Headers(
            init?.headers ?? (input instanceof Request ? input.headers : undefined),
          );
          headers.set("x-nanocodex-app-id", appId);
          return fetchFn(input, {
            ...init,
            headers,
            credentials: init?.credentials ?? credentials,
          });
        },
        async request(request) {
          const headers = new Headers(request.headers);
          headers.set("accept", "application/json");
          headers.set("x-nanocodex-app-id", appId);
          if (request.body !== undefined) headers.set("content-type", "application/json");
          const response = await fetchFn(new URL(request.path, baseUrl), {
            method: request.method ?? "GET",
            headers,
            credentials,
            body: request.body === undefined ? undefined : JSON.stringify(request.body),
            signal: request.signal,
          });
          const body = response.status === 204 ? undefined : await response.json().catch(() => undefined);
          if (!response.ok) {
            throw new HttpError(
              response.status,
              body?.error?.message ?? `Nanocodex request failed with ${response.status}`,
              { code: body?.error?.code },
            );
          }
          return body;
        },
      };
    },
  });
}

export function mock(options = {}) {
  const accountAddress = options.accountAddress ?? MOCK_ACCOUNT_ADDRESS;
  const machineUsdAddress = options.machineUsdAddress ?? MOCK_MACHINE_USD_ADDRESS;
  const appName = options.appName ?? "Acme Workspace";
  const grants = new Map();
  let sequence = 0;

  return from({
    key: options.key ?? "mock",
    name: options.name ?? "Nanocodex mock",
    type: "mock",
    setup({ appId }) {
      return {
        baseUrl: "memory://nanocodex",
        async request(request) {
          if (request.method === "POST" && request.path === "/v1/connections") {
            const authorization = request.body?.key_authorization;
            const keyId = authorization?.keyId ?? authorization?.address;
            if (typeof keyId !== "string") throw new InvalidResponseError("key authorization is missing keyId");
            const expiry = authorization.expiry ?? Math.floor(Date.now() / 1000) + 30 * 86_400;
            const connectors = requestedConnectors(request.body?.requested_connectors);
            const mcpConnections = requestedMcpConnections(request.body?.requested_mcp_connections);
            const grantId = mockHex(`${appId}:${accountAddress}:${keyId}`, 32);
            const wire = {
              grant_token: `mock-grant-${grantId.slice(2)}`,
              account_address: accountAddress,
              agent_id: `agent_${grantId.slice(2, 14)}`,
              session_id: `session_${grantId.slice(2, 14)}`,
              grant: {
                id: grantId,
                permission: request.body?.permission ?? "agent.run",
                status: "active",
                expires_at: expiry,
                capabilities: [
                  "nanocodex.agent",
                  "mercator.boost",
                  "mpp.machusd",
                  ...connectors.map((connector) => connector === "slack" ? "slack:TMOCK" : connector),
                  ...mcpConnections.map(({ id }) => `mcp:${id}`),
                ],
                mcp_connections: mcpConnections,
              },
              access_key: {
                address: keyId,
                chain_id: String(authorization.chainId ?? 4217),
                key_id: keyId,
                key_type: authorization.keyType ?? "p256",
                limits: authorization.limits ?? [{ token: machineUsdAddress, limit: "10000000", period: 86_400 }],
                scopes: authorization.scopes ?? [],
                witness: authorization.witness ?? mockHex(`${grantId}:witness`, 32),
                expiry,
                authorization: request.body?.signed_key_authorization ?? "0x01",
              },
              mpp: {
                token: machineUsdAddress,
                symbol: "MACHUSD",
                balance_status: "ready",
                settlement_token: "0x20C000000000000000000000b9537d11c60E8b50",
                settlement_symbol: "USDC.e",
                settlement_balance_atomics: "0",
                limit_atomics: "10000000",
                max_per_request_atomics: "250000",
                period: 86_400,
                balance_atomics: "0",
                spent_atomics: "0",
              },
            };
            grants.set(grantId, wire);
            return wire;
          }

          if (request.method === "GET" && request.path.match(/^\/v1\/grants\/0x[0-9a-f]+$/)) {
            return requiredGrant(grants, request.path.split("/")[3]);
          }

          if (request.method === "POST" && request.path === "/v1/connections/prepare") {
            const permission = request.body?.permission;
            if (typeof permission !== "string" || permission.length === 0) {
              throw new TypeError("connection permission must be a non-empty string");
            }
            const resources = request.body?.resources ?? [];
            if (!Array.isArray(resources) || resources.some((resource) => typeof resource !== "string" || resource.length === 0)) {
              throw new TypeError("connection resources must be non-empty strings");
            }
            const connectors = connectorResources(resources);
            const id = `req_${++sequence}`;
            const accessPolicy = request.body?.authorize_access_key;
            const keyId = accessPolicy?.address ?? mockHex(`${appId}:${accountAddress}:${permission}:key`, 20);
            const tokenLimit = accessPolicy?.limits?.find((limit) => limit.token === machineUsdAddress);
            const expiry = accessPolicy?.expiry ?? Math.floor(Date.now() / 1000) + 30 * 86_400;
            const dailyLimit = tokenLimit?.limit ?? "10000000";
            const period = tokenLimit?.period ?? 86_400;
            const appOrigin = options.appOrigin ?? "https://app.example";
            const appUrl = new URL(appOrigin);
            const nonce = mockHex(`${id}:siwe-nonce`, 8).slice(2);
            const issuedAt = new Date().toISOString();
            const authMessage = [
              `${appUrl.host} wants you to sign in with your Ethereum account:`,
              "0x0000000000000000000000000000000000000000",
              "",
              `${appName} wants to connect your Nanocodex agent.`,
              "",
              `URI: ${appUrl.origin}`,
              "Version: 1",
              "Chain ID: 4217",
              `Nonce: ${nonce}`,
              `Issued At: ${issuedAt}`,
              ...(resources.length > 0
                ? ["Resources:", ...resources.map((resource) => `- ${resource}`)]
                : []),
            ].join("\n");
            return {
              request_id: id,
              app: { id: appId, name: appName, origin: appOrigin },
              account_address: accountAddress,
              auth: {
                message: authMessage,
                resources,
              },
              permission: {
                id: permission,
                title: "Use your Nanocodex agent",
                description: "Run an app-owned Nanocodex agent with your approved capabilities.",
                connectors: [
                  { id: "agent", name: "Nanocodex", detail: "Instantiate the real Nanocodex runtime in this app" },
                  { id: "mercator", name: "BOOST with Mercator", detail: "Find and compose the right tools, paying per call through MPP" },
                  ...connectors.map((provider) => ({
                    id: provider,
                    name: connectorName(provider),
                    detail: `Use the connected ${connectorName(provider)} account through the grant`,
                  })),
                ],
              },
              access_key: {
                address: keyId,
                chain_id: accessPolicy?.chainId ?? "4217",
                key_id: keyId,
                public_key: accessPolicy?.publicKey ?? mockHex(`${id}:public`, 65),
                key_type: accessPolicy?.keyType ?? "p256",
                limits: accessPolicy?.limits ?? [{ token: machineUsdAddress, limit: dailyLimit, period }],
                scopes: accessPolicy?.scopes ?? [],
                witness: PersonalMessage.getSignPayload(Hex.fromString(authMessage)),
                expiry,
              },
              mpp: {
                token: machineUsdAddress,
                symbol: "MACHUSD",
                settlement_token: "0x20C000000000000000000000b9537d11c60E8b50",
                settlement_symbol: "USDC.e",
                settlement_balance_atomics: "0",
                limit_atomics: dailyLimit,
                period,
                max_per_request_atomics: "250000",
              },
            };
          }

          if (request.method === "POST" && request.path === "/v1/connections/complete") {
            const prepared = request.body?.prepared;
            const approval = request.body?.approval;
            if (!prepared || !approval || approval.approved !== true) {
              throw new InvalidResponseError("connection approval is malformed");
            }
            const approvedCapabilities = approval.capabilities;
            if (
              approvedCapabilities?.keyAuthorization?.witness !== prepared.access_key.witness
              || approvedCapabilities?.personalSign?.message !== prepared.auth.message
              || typeof approvedCapabilities?.personalSign?.keyAuthorization !== "string"
              || typeof approvedCapabilities?.signature !== "string"
            ) {
              throw new InvalidResponseError("connection approval is not bound to the SIWE challenge and key authorization");
            }
            const grantId = mockHex(`${prepared.app.id}:${prepared.account_address}:${prepared.access_key.key_id}`, 32);
            const wire = {
              grant_token: `mock-grant-${grantId.slice(2)}`,
              account_address: prepared.account_address,
              agent_id: `agent_${grantId.slice(2, 14)}`,
              session_id: `session_${grantId.slice(2, 14)}`,
              grant: {
                id: grantId,
                permission: prepared.permission.id,
                status: "active",
                expires_at: prepared.access_key.expiry,
                capabilities: prepared.permission.connectors.map(({ id }) => id),
              },
              access_key: prepared.access_key,
              mpp: {
                ...prepared.mpp,
                balance_atomics: "0",
                balance_status: "ready",
                spent_atomics: "0",
              },
            };
            grants.set(grantId, wire);
            return wire;
          }

          if (request.method === "POST" && request.path === "/v1/connections/disconnect") {
            return undefined;
          }

          if (request.method === "GET" && request.path.match(/^\/v1\/grants\/0x[0-9a-f]+\/mpp\/balance$/)) {
            const wire = requiredGrant(grants, request.path.split("/")[3]);
            wire.mpp = { ...wire.mpp, balance_status: "ready" };
            return wire;
          }

          if (request.method === "POST" && request.path.match(/^\/v1\/grants\/0x[0-9a-f]+\/revoke$/)) {
            const grantId = request.path.split("/")[3];
            const wire = requiredGrant(grants, grantId);
            wire.grant = { ...wire.grant, status: "revoked" };
            return wire.grant;
          }

          if (request.method === "GET" && request.path === "/v1/machine-usd/config") {
            return {
              chain_id: 4217,
              min_usd_amount_cents: 500,
              max_usd_amount_cents: 10_000,
              onramp_enabled: true,
              stripe_publishable_key: "pk_test_nanocodex",
              token_address: machineUsdAddress,
            };
          }

          if (request.method === "POST" && request.path.match(/^\/v1\/grants\/0x[0-9a-f]+\/fund$/)) {
            const grantId = request.path.split("/")[3];
            const wire = requiredActiveGrant(grants, grantId);
            const usdAmountCents = request.body?.usd_amount_cents;
            if (!Number.isSafeInteger(usdAmountCents) || usdAmountCents < 500 || usdAmountCents > 10_000) {
              throw new TypeError("machineUSD amount must be from 500 through 10000 cents");
            }
            const amount = BigInt(usdAmountCents) * 10_000n;
            wire.mpp = {
              ...wire.mpp,
              balance_atomics: String(BigInt(wire.mpp.balance_atomics) + amount),
            };
            return {
              order: {
                id: `ord_${++sequence}`,
                status: "complete",
                usd_amount_cents: usdAmountCents,
                machine_usd_amount_atomics: String(amount),
                issuance_transaction_hash: mockHex(`fund:${sequence}`, 32),
              },
              connection: wire,
            };
          }

          if (request.method === "POST" && request.path.match(/^\/v1\/grants\/0x[0-9a-f]+\/mpp\/charge$/)) {
            const grantId = request.path.split("/")[3];
            const wire = requiredActiveGrant(grants, grantId);
            const amount = BigInt(request.body?.amount_atomics ?? "-1");
            const max = BigInt(wire.mpp.max_per_request_atomics);
            const spent = BigInt(wire.mpp.spent_atomics);
            const limit = BigInt(wire.mpp.limit_atomics);
            const balance = BigInt(wire.mpp.balance_atomics);
            if (amount <= 0n) throw new TypeError("MPP amount must be positive");
            if (amount > max) throw new HttpError(403, "This payment exceeds the per-request permission.", { code: "mpp_request_limit_exceeded" });
            if (spent + amount > limit) throw new HttpError(403, "This payment exceeds the daily MPP permission.", { code: "mpp_period_limit_exceeded" });
            if (amount > balance) throw new HttpError(402, "Add machineUSD before paying for this capability.", { code: "machine_usd_required" });
            wire.mpp = {
              ...wire.mpp,
              balance_atomics: String(balance - amount),
              spent_atomics: String(spent + amount),
            };
            return {
              receipt: {
                id: `mpp_${++sequence}`,
                amount_atomics: String(amount),
                origin: request.body?.origin,
                transaction_hash: mockHex(`mpp:${sequence}`, 32),
              },
              connection: wire,
            };
          }

          throw new HttpError(404, `No mock route for ${request.method ?? "GET"} ${request.path}`, {
            code: "mock_route_not_found",
          });
        },
      };
    },
  });
}

function requiredGrant(grants, grantId) {
  const grant = grants.get(grantId);
  if (!grant) throw new HttpError(404, "Nanocodex grant not found", { code: "grant_not_found" });
  return grant;
}

function requestedConnectors(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new TypeError("requested_connectors must be an array");
  const supported = ["github", "gmail", "gdrive", "x", "slack", "chatgpt"];
  if (value.some((provider) => !supported.includes(provider))) {
    throw new TypeError("requested_connectors contains an unsupported provider");
  }
  return supported.filter((provider) => value.includes(provider));
}

function requestedMcpConnections(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 16
    || value.some((id) => typeof id !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(id))
    || new Set(value).size !== value.length) {
    throw new TypeError("requested_mcp_connections must contain exact hosted connection IDs");
  }
  return value.map((id) => ({ id, name: "MCP connection" }));
}

function connectorResources(resources) {
  return requestedConnectors(resources
    .filter((resource) => resource.startsWith("urn:nanocodex:connector:"))
    .map((resource) => resource.slice("urn:nanocodex:connector:".length)));
}

function connectorName(provider) {
  return ({ github: "GitHub", gmail: "Gmail", gdrive: "Google Drive", x: "X", slack: "Slack", chatgpt: "ChatGPT" })[provider];
}

function requiredActiveGrant(grants, grantId) {
  const grant = requiredGrant(grants, grantId);
  if (grant.grant.status !== "active") {
    throw new HttpError(403, "This Nanocodex grant has been revoked.", { code: "grant_revoked" });
  }
  return grant;
}

function mockHex(seed, bytes) {
  let state = 2166136261;
  let output = "";
  for (let index = 0; output.length < bytes * 2; index += 1) {
    const code = seed.charCodeAt(index % seed.length) + index;
    state ^= code;
    state = Math.imul(state, 16777619) >>> 0;
    output += state.toString(16).padStart(8, "0");
  }
  return `0x${output.slice(0, bytes * 2)}`;
}

function requiredString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${label} must be a non-empty string`);
  }
  return value;
}
