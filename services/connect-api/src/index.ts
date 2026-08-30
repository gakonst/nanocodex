import { Handler, Kv } from "accounts/server";
import { custom } from "viem";
import { KeyAuthorization } from "ox/tempo";

import {
  chatGptCredentialImportDigest,
  credentialImportDigestFromResources,
  parseChatGptCredentialImport,
} from "./chatGptCredentialImport.mjs";
import type { ChatGptCredentialImport } from "./chatGptCredentialImport.mjs";
import {
  cliApp,
  approvedCliAccessKeyMatches,
  parseCliRegisterBody,
  parseCliWalletRequest,
  sanitizeCliWalletResult,
  managedMemoryCapability,
  requestedConnectorsSatisfied,
} from "./devicePolicy.mjs";
import {
  connectAuthOrigin,
  deviceVerificationUrl,
  isLocalDevelopmentOrigin as isLocalDeviceOrigin,
} from "./deviceRedirect.mjs";
import {
  localConnectorAuthorization,
  localMcpAuthorization,
  wrapLocalConnectorAuthorizationState,
  wrapLocalMcpAuthorizationState,
} from "../../../web/localConnectorCallback";
import {
  scopedConnectConnectorState,
  unscopedConnectConnectorState,
} from "../../../web/connectConnectorCallback.mjs";
import {
  canonicalRemoteMcpTarget,
  isMcpConnectionId,
  validateMcpResources,
} from "./mcpPolicy.mjs";
import {
  managedGrantHeaders,
  type ManagedGrantAssertion,
} from "./managedGrant.mjs";
import {
  authenticateEmbedProject,
  embedPrincipalId,
  identitySessionToken,
  isEmbedIdentity,
  parseEmbedSessionBody,
  type EmbedIdentity,
} from "./embedIdentity.mjs";

type WorkerWebSocket = WebSocket & { accept(): void };
declare const WebSocketPair: {
  new(): { 0: WorkerWebSocket; 1: WorkerWebSocket };
};

export class ConnectNonceStorage extends Kv.NonceStorage {
  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname !== "/resolve-grant") return super.fetch(request);
    const token = url.searchParams.get("token");
    if (!token || !/^[A-Za-z0-9_-]{43}$/.test(token)) {
      return Response.json({ error: "invalid grant token" }, { status: 400 });
    }
    const principal = await this.activeValue(`grant-token:${token}`);
    const grantId = isRecord(principal) && typeof principal.grantId === "string"
      ? principal.grantId
      : undefined;
    const grant = grantId ? await this.activeValue(`grant:${grantId}`) : undefined;
    return Response.json({ principal, grant });
  }

  private async activeValue(key: string): Promise<unknown> {
    const entry = await this.state.storage.get<Kv.NonceStorage.Entry>(key);
    if (!entry) return undefined;
    if (entry.expiresAt !== undefined && Date.now() >= entry.expiresAt) {
      await this.state.storage.delete(key);
      return undefined;
    }
    return entry.value;
  }
}

const PLAYGROUND_ORIGIN = "https://nanocodex-connect-playground.gakonst.workers.dev";
const CHROME_EXTENSION_ORIGIN = "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle";
const CLI_APP_ID = cliApp.id;
const CLI_APP_ORIGIN = cliApp.origin;
const DIALOG_ORIGIN = "https://nanocodex.gakonst.workers.dev";
const API_ORIGIN = "https://nanocodex-connect-api.gakonst.workers.dev";
const NANOCODEX_ORIGIN = "https://nanocodex.gakonst.workers.dev";
const MACHINE_USD_ORIGIN = "https://machine-usd.porto.workers.dev";
const MERCATOR_ORIGIN = "https://mercator.tempoxyz.dev";
const TEMPO_RPC = "https://api.tempo.xyz/rpc/4217";
const MACHINE_USD = "0x20c0000000000000000000006637932dE5413804";
const USDC_E = "0x20C000000000000000000000b9537d11c60E8b50";
const MACHINE_USD_SWAPPER = "0xd588ED9Ae08643A450157Adaf61c3C0C1BBd0dbb";
const TIP20_CHANNEL_ESCROW = "0x4d50500000000000000000000000000000000000";
const MERCATOR_SETTLEMENT = "0xa295C42FBCC026a62304A7701f25B4c91799B0dA";
const MPP_LIMIT = 10_000_000n;
const MPP_PERIOD = 86_400;
const MPP_MAX_PER_REQUEST = 250_000n;
const CONNECTOR_IDS = ["github", "gmail", "gdrive", "x", "chatgpt"] as const;
const OAUTH_CONNECTOR_IDS = ["github", "gmail", "gdrive", "x"] as const;
const BASE_APPROVAL_RESOURCES = [
  "urn:nanocodex:agent:run",
  "urn:nanocodex:capability:mercator:boost",
  "urn:nanocodex:mpp:machusd:spend",
] as const;
const AGENT_VISIBILITY_RESOURCES = {
  "urn:nanocodex:agent:output:final": "agent.output.final",
  "urn:nanocodex:agent:output:actions": "agent.output.actions",
  "urn:nanocodex:agent:history:read": "agent.history.read",
  "urn:nanocodex:agent:trace:read": "agent.trace.read",
} as const;
const AGENT_VISIBILITY_RESOURCE_PREFIX = "urn:nanocodex:agent:visibility:";
const HOSTED_HISTORY_RESOURCE = "urn:nanocodex:history:read";
const HOSTED_MEMORY_READ_RESOURCE = "urn:nanocodex:memory:read";
const HOSTED_MEMORY_WRITE_RESOURCE = "urn:nanocodex:memory:write";
const HOSTED_AUTHORIZATION_RESOURCE = "urn:nanocodex:authorization:hosted";
const APP_RESOURCE_PREFIX = "urn:nanocodex:app:";
const APP_ORIGIN_RESOURCE_PREFIX = "urn:nanocodex:origin:";
const AGENT_VISIBILITY_NAMES = {
  reply: "agent.output.final",
  actions: "agent.output.actions",
  history: "agent.history.read",
  traces: "agent.trace.read",
} as const;
const CONNECTORS_RESOURCE_PREFIX = "urn:nanocodex:connectors:";
const PROVIDER_CREDENTIAL_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const CONNECTOR_STATE_TTL = 10 * 60;
const MCP_INTENT_TTL = 10 * 60;
const CONNECT_APPROVAL_TTL = 10 * 60;
const MODEL_TICKET_TTL = 60;
const REALTIME_TICKET_TTL = 60;
const TOOL_HOST_TICKET_TTL = 30;
const MODEL_PROTOCOL = "nanocodex-connect-v1";
const MODEL_TICKET_PROTOCOL_PREFIX = "nanocodex-ticket.";
const ACCOUNT_LINK_TTL = 5 * 60;
const REGISTERED_APP_ID = "atlas-workspace";
const CHROME_EXTENSION_APP_ID = "nanocodex-chrome";
const MAX_BROKER_BODY_BYTES = 16 * 1024;
const MAX_CONNECTOR_REQUEST_BODY_BYTES = 256 * 1024;
const MAX_CONNECTOR_RESPONSE_BODY_BYTES = 1024 * 1024;
const MAX_AGENT_TOOL_BODY_BYTES = 20 * 1024 * 1024;
const MAX_PUBLIC_EGRESS_BODY_BYTES = 256 * 1024;
const MAX_PUBLIC_EGRESS_RESPONSE_BYTES = 1024 * 1024;
const MAX_MANAGED_MEMORY_REQUEST_BYTES = 16 * 1024;
const MAX_MANAGED_MEMORY_RESPONSE_BYTES = 1024 * 1024;
const MAX_PINNED_RUNTIME_RESPONSE_BYTES = 32 * 1024 * 1024;
const MAX_ACCOUNT_AUTHORIZATIONS = 64;
const MAX_DEVICE_REGISTER_BYTES = 64 * 1024;
const MAX_CONNECTION_REQUEST_BYTES = 128 * 1024;
const MAX_EMBED_SESSION_REQUEST_BYTES = 4 * 1024;
const MAX_CHATGPT_IMPORT_BODY_BYTES = 64 * 1024;
const EGRESS_SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const CONNECTOR_REQUEST_HEADERS = new Set([
  "accept",
  "content-range",
  "content-type",
  "if-match",
  "if-modified-since",
  "if-none-match",
  "if-unmodified-since",
]);
const CONNECTOR_RESPONSE_HEADERS = new Set([
  "accept-ranges",
  "content-range",
  "content-type",
  "etag",
  "last-modified",
  "retry-after",
  "x-ratelimit-limit",
  "x-ratelimit-remaining",
  "x-ratelimit-reset",
  "x-ratelimit-resource",
]);
const FORBIDDEN_CONNECTOR_HEADERS = /^(?:authorization|cookie|forwarded|host|origin|proxy-|referer|set-cookie|x-forwarded-|x-nanocodex-subject$|x-real-ip$|cf-)/i;
const PRIVATE_EGRESS_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i;
const FORBIDDEN_EGRESS_HEADERS = new Set([
  "connection", "host", "origin", "proxy-connection", "referer", "te", "trailer",
  "transfer-encoding", "upgrade", "x-nanocodex-subject",
]);
const BLOCKED_EGRESS_RESPONSE_HEADERS = new Set([
  "clear-site-data", "connection", "content-encoding", "content-length", "keep-alive", "nel", "proxy-authenticate",
  "proxy-authorization", "refresh", "report-to", "set-cookie", "set-cookie2",
  "trailer", "transfer-encoding", "upgrade", "x-nanocodex-subject",
]);
const PRIVATE_HOST_SUFFIXES = [
  ".internal", ".invalid", ".local", ".localhost", ".test", ".home.arpa",
];
const PUBLIC_REDIRECTS = new Set([301, 302, 303, 307, 308]);

type ConnectorId = typeof CONNECTOR_IDS[number];
type OAuthConnectorId = typeof OAUTH_CONNECTOR_IDS[number];
type ConnectorStatus = Readonly<{
  connected: boolean;
  label?: string;
  account_id?: string;
}>;
type ConnectorState = Readonly<{
  accountAddress: `0x${string}`;
  brokerUserId: string;
  dialogOrigin: string;
  provider: OAuthConnectorId;
  returnTo?: string;
}>;
type McpConnectionStatus = "authorization_required" | "connected" | "reauthorization_required" | "disabled" | "revoked";
type McpConnection = Readonly<{
  id: string;
  name: string;
  status: McpConnectionStatus;
}>;
type McpIntent = Readonly<{
  appId: string;
  appOrigin: string;
  endpoint: string;
  endpointHash: string;
  expiresAt: number;
  id: string;
  name: string;
}>;
type McpConnectionState = Readonly<{
  accountAddress: `0x${string}`;
  brokerUserId: string;
  connectionId: string;
  dialogOrigin: string;
  returnTo?: string;
}>;
type AccountLinkState = Readonly<{
  accountAddress: `0x${string}`;
}>;
type PendingMcpAccountLink = Readonly<{
  appId: string;
  appOrigin: string;
  resources: readonly string[];
}>;
type AuthRequestContext = Readonly<{
  keyAuthorization?: `0x${string}`;
  message: string;
}>;
type ConnectApproval = Readonly<{
  accountAddress: `0x${string}`;
  appId: string;
  appOrigin: string;
  brokerUserId?: string;
  connectedConnectors?: readonly ConnectorId[];
  mcpConnections?: readonly McpConnection[];
  durableAgentId?: string;
  keyAuthorization?: `0x${string}`;
  profileLinked?: boolean;
  resources: readonly string[];
  authorization: "signed" | "hosted";
  externalPrincipalId?: string;
}>;
type Fetcher = Readonly<{
  fetch(request: Request): Promise<Response>;
}>;

type WorkerContext = Readonly<{
  waitUntil(promise: Promise<unknown>): void;
}>;

type ConnectLogContext = Readonly<{
  deployment_sha?: string;
  user_id?: string;
  account_id?: `0x${string}`;
  connector?: ConnectorId;
  mcp_connection_id?: string;
}>;

type Env = Readonly<{
  ACCOUNTS: Fetcher;
  CONNECT_STATE: Kv.durableObject.Namespace;
  EGRESS: Fetcher;
  NANOCODEX: Fetcher;
  NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY?: string;
  NANOCODEX_EMBED_PROJECTS?: string;
  DEPLOYMENT_SHA?: string;
}>;

type GrantRecord = Readonly<{
  id: `0x${string}`;
  appId: string;
  appOrigin: string;
  accountAddress: `0x${string}`;
  brokerUserId: string;
  agentId: string;
  permission: string;
  status: "active" | "revoked";
  expiresAt: number;
  capabilities: readonly string[];
  mcpConnections?: readonly Readonly<{ id: string; name: string }>[];
  accessKey?: Record<string, unknown>;
  balanceAtomics?: string;
  spentAtomics: string;
  egressSubject: string;
  settlementBalanceAtomics?: string;
  sharedEgressSubject?: boolean;
  externalPrincipalId?: string;
}>;
type EmbedIdentitySession = EmbedIdentity & Readonly<{ expiresAt: number }>;
type EmbedIdentityBinding = Readonly<{
  brokerUserId: string;
  principalId: string;
}>;
type HostedBrowserSession = Readonly<{
  accountAddress: `0x${string}`;
  expiresAt: number;
}>;
type GrantPrincipal = Readonly<{
  accountAddress: `0x${string}`;
  appId: string;
  appOrigin: string;
  grantId: `0x${string}`;
  externalPrincipalId?: string;
}>;
type ConnectAgentRecord = Readonly<{ agentId: string }>;
type ConnectIdentityRecord = Readonly<{
  accountAddress: `0x${string}`;
  brokerUserId: string;
}>;
type ConnectSubjectRecord = Readonly<{
  appId: string;
  brokerUserId: string;
  subject: string;
}>;
type AccessKeyRecord = Readonly<{
  accountAddress: `0x${string}`;
  appId: string;
  appOrigin: string;
  accessKey: Record<string, unknown>;
}>;
type CallerApp = Readonly<{ appId: string; origin: string }>;
type ModelTicket = Readonly<{
  grantId: `0x${string}`;
  sessionId: string;
  turnState?: string;
}>;
type RealtimeTicket = Readonly<{
  appId: string;
  appOrigin: string;
  agentId: string;
  callId: string;
  grantId: `0x${string}`;
  voiceSessionId: string;
}>;
type ToolHostTicket = Readonly<{
  appId: string;
  appOrigin: string;
  agentId: string;
  grantId: `0x${string}`;
  mcpFingerprint: `0x${string}`;
}>;

export default {
  async fetch(request: Request, env: Env, context: WorkerContext): Promise<Response> {
    if (request.method === "OPTIONS") return cors(new Response(null, { status: 204 }), request);
    let logContext: ConnectLogContext = {
      ...(env.DEPLOYMENT_SHA === undefined ? {} : { deployment_sha: env.DEPLOYMENT_SHA }),
    };
    try {
      const url = new URL(request.url);
      const store = Kv.durableObject(env.CONNECT_STATE);

      if (request.method === "POST" && url.pathname === "/v1/embed/sessions") {
        return cors(await createEmbedIdentitySession(request, env, store), request);
      }

      if (url.pathname.startsWith("/v1/device/")) {
        if (request.method === "POST" && url.pathname === "/v1/device/register") {
          try {
            parseCliRegisterBody(await boundedJson(
              request.clone(),
              MAX_DEVICE_REGISTER_BYTES,
              "device registration",
            ));
          } catch (cause) {
            if (cause instanceof ApiFailure) throw cause;
            throw new ApiFailure(400, "invalid_device_request", errorText(cause));
          }
        }
        if (request.method === "POST" && url.pathname === "/v1/device/verify") {
          requireDialogOrigin(request);
        }
        return cors(mutableResponse(await createDeviceCode(env, store).fetch(request)), request);
      }

      if (url.pathname.startsWith("/v1/connect/auth")) {
        requireDialogOrigin(request);
        const auth = createAuth(
          env,
          store,
          request,
          await authRequestContext(request, url),
        );
        return cors(await auth.fetch(request), request);
      }
      if (request.method === "POST" && url.pathname === "/v1/hosted-authorizations") {
        requireDialogOrigin(request);
        return cors(await createHostedAuthorization(request, env, store), request);
      }
      if (request.method === "POST" && url.pathname === "/v1/mcp-intents") {
        return cors(await createMcpIntent(request, store), request);
      }
      if (request.method === "GET" && url.pathname === "/healthz") {
        return cors(Response.json({ status: "ok", mode: "live" }), request);
      }
      if (request.method === "POST" && url.pathname === "/v1/client-diagnostics") {
        requirePlaygroundOrigin(request);
        const encoded = await request.text();
        if (encoded.length > 256) {
          return error(request, 413, "diagnostic_too_large", "Client diagnostics are limited to 256 bytes.");
        }
        const diagnostic = parseClientDiagnostic(encoded);
        console.log({
          type: "connect.client.diagnostic",
          outcome: "success",
          status: diagnostic.stage,
        });
        return cors(new Response(null, { status: 204 }), request);
      }
      const modelSocket = url.pathname.match(/^\/v1\/grants\/(0x[0-9a-fA-F]{64})\/model$/);
      if (modelSocket) {
        return await openGrantModelWebSocket(
          request,
          env,
          store,
          url,
          modelSocket[1] as `0x${string}`,
        );
      }
      const realtimeSocket = url.pathname.match(
        /^\/v1\/grants\/(0x[0-9a-fA-F]{64})\/agents\/([^/]+)\/realtime\/sideband$/,
      );
      if (realtimeSocket) {
        return await openGrantRealtimeWebSocket(
          request,
          env,
          store,
          url,
          realtimeSocket[1] as `0x${string}`,
          decodeURIComponent(realtimeSocket[2]!),
        );
      }
      const toolHostSocket = url.pathname.match(
        /^\/v1\/grants\/(0x[0-9a-fA-F]{64})\/agents\/([^/]+)\/tool-host$/,
      );
      if (toolHostSocket) {
        return await openGrantToolHostWebSocket(
          request,
          env,
          store,
          url,
          toolHostSocket[1] as `0x${string}`,
          decodeURIComponent(toolHostSocket[2]!),
        );
      }
      if (request.method === "GET" && url.pathname === "/v1/machine-usd/config") {
        const upstream = await fetch(`${MACHINE_USD_ORIGIN}/v1/config`, {
          headers: { accept: "application/json" },
        });
        return cors(new Response(upstream.body, {
          status: upstream.status,
          headers: { "content-type": upstream.headers.get("content-type") ?? "application/json" },
        }), request);
      }
      if (/^\/git\/thread-[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}\/(?:info\/refs|git-upload-pack|git-receive-pack)$/.test(url.pathname)) {
        requirePlaygroundOrigin(request);
        return cors(await proxyThreadGit(request, env, url), request);
      }
      if (request.method === "POST" && url.pathname === "/v1/mercator/jobs") {
        requirePlaygroundOrigin(request);
        const body = await request.text();
        if (new TextEncoder().encode(body).byteLength > 64 * 1024) {
          return error(request, 413, "mercator_request_too_large", "Mercator job requests are limited to 64 KiB.");
        }
        const headers = new Headers({
          accept: request.headers.get("accept") ?? "application/json",
          "content-type": "application/json",
        });
        for (const name of [
          "accept-payment",
          "authorization",
          "payment-signature",
          "payment-session",
          "payment-session-snapshot",
        ]) {
          const value = request.headers.get(name);
          if (value) headers.set(name, value);
        }
        const upstream = await fetch(`${MERCATOR_ORIGIN}/v1/jobs`, {
          method: "POST",
          headers,
          body,
        });
        return cors(proxyPayment(upstream), request);
      }
      if (request.method === "POST" && url.pathname === "/v1/machine-usd/orders") {
        requireOnrampOrigin(request);
        const upstream = await fetch(`${MACHINE_USD_ORIGIN}/v1/orders`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "idempotency-key": requiredHeader(request, "idempotency-key"),
          },
          body: await request.text(),
        });
        return cors(proxy(upstream), request);
      }
      const machineUsdOrder = url.pathname.match(/^\/v1\/machine-usd\/orders\/([^/]+)$/);
      if (request.method === "GET" && machineUsdOrder) {
        requireOnrampOrigin(request);
        const upstream = await fetch(
          `${MACHINE_USD_ORIGIN}/v1/orders/${encodeURIComponent(machineUsdOrder[1]!)}`,
          { headers: { authorization: requiredHeader(request, "authorization") } },
        );
        return cors(proxy(upstream), request);
      }

      const connectorCallback = url.pathname.match(/^\/v1\/connectors\/(github|gmail|gdrive|x)\/callback$/);
      if (connectorCallback) {
        if (request.method !== "GET") {
          return error(request, 405, "method_not_allowed", "Connector callbacks require GET.");
        }
        return cors(await completeConnectorCallback(
          env,
          store,
          url,
          connectorCallback[1] as OAuthConnectorId,
        ), request);
      }

      const mcpCallback = url.pathname.match(/^\/v1\/mcp-connections\/([A-Za-z0-9_-]{43})\/callback$/);
      if (mcpCallback) {
        if (request.method !== "GET") {
          return error(request, 405, "method_not_allowed", "MCP callbacks require GET.");
        }
        return cors(await completeMcpConnectionCallback(env, store, url, mcpCallback[1]!), request);
      }

      const accessKeyStatus = url.pathname.match(/^\/v1\/access-keys\/(0x[0-9a-fA-F]{40})\/(0x[0-9a-fA-F]{40})$/);
      if (request.method === "GET" && accessKeyStatus) {
        const app = requireCallerApp(request, url.searchParams.get("app_id"));
        const accountAddress = address(accessKeyStatus[1]);
        const keyId = address(accessKeyStatus[2]);
        const stored = await store.get<AccessKeyRecord>(accessKeyStorageKey(accountAddress, keyId));
        return cors(Response.json({
          registered: isAccessKeyRecord(stored)
            && stored.appId === app.appId
            && stored.appOrigin === app.origin
            && stored.accountAddress.toLowerCase() === accountAddress.toLowerCase(),
        }), request);
      }

      if (request.method === "POST" && url.pathname === "/v1/connections") {
        return cors(await createConnection(request, env, store, context), request);
      }

      const managedMemoryResponse = await handleManagedMemoryRoute(request, env, url);
      if (managedMemoryResponse) return cors(managedMemoryResponse, request);

      const grantResponse = await handleGrantRoute(request, env, store, url);
      if (grantResponse) return cors(grantResponse, request);

      const agentToolResponse = await handleAgentToolRoute(request, env, store, url);
      if (agentToolResponse) return cors(agentToolResponse, request);

      requireDialogOrigin(request);
      const hostedSession = await readHostedBrowserSession(store, request);
      let accountAddress: `0x${string}`;
      if (hostedSession) {
        accountAddress = hostedSession.accountAddress;
      } else {
        const session = await createAuth(env, store, request).getSession(request);
        if (!session) return error(request, 401, "not_authenticated", "Connect with a passkey first.");
        accountAddress = address(session.address);
      }

      if (url.pathname === "/v1/account-link") {
        if (request.method === "GET") {
          const identity = await brokerIdentity(env, accountAddress);
          return cors(Response.json({ linked: identity.linked }), request);
        }
        if (request.method === "POST") {
          return cors(await startAccountLink(request, store, accountAddress), request);
        }
        if (request.method === "PUT") {
          return cors(await completeAccountLink(request, env, store, accountAddress), request);
        }
        return error(request, 405, "method_not_allowed", "Unsupported account-link operation.");
      }

      if (request.method === "GET" && url.pathname === "/v1/connectors") {
        const identity = await brokerIdentity(env, accountAddress);
        logContext = { ...logContext, user_id: identity.userId, account_id: accountAddress };
        return cors(Response.json({
          ...(await connectorStatuses(env, identity.userId)),
          profile: { linked: identity.linked },
        }), request);
      }

      if (request.method === "GET" && url.pathname === "/v1/mcp-connections") {
        const identity = await brokerIdentity(env, accountAddress);
        logContext = { ...logContext, user_id: identity.userId, account_id: accountAddress };
        return cors(Response.json({ mcp_connections: await mcpConnectionStatuses(env, identity.userId) }), request);
      }

      const mcpConnectionRoute = url.pathname.match(/^\/v1\/mcp-connections\/([A-Za-z0-9_-]{43})$/);
      if (mcpConnectionRoute) {
        const identity = await brokerIdentity(env, accountAddress);
        const connectionId = mcpConnectionRoute[1]!;
        logContext = {
          ...logContext,
          user_id: identity.userId,
          account_id: accountAddress,
          mcp_connection_id: connectionId,
        };
        if (request.method === "POST") {
          const response = await startMcpConnection(
            env,
            store,
            request,
            accountAddress,
            identity.userId,
            connectionId,
          );
          console.info({
            type: "connect.mcp.start",
            outcome: "success",
            ...logContext,
            status: "accepted",
          });
          return cors(response, request);
        }
        if (request.method === "DELETE") {
          await disconnectMcpConnection(env, identity.userId, connectionId);
          console.info({
            type: "connect.mcp.disconnect",
            outcome: "success",
            ...logContext,
            status: "disconnected",
          });
          return cors(new Response(null, { status: 204 }), request);
        }
        return error(request, 405, "method_not_allowed", "Unsupported MCP connection operation.");
      }

      const connectorRoute = url.pathname.match(/^\/v1\/connectors\/(github|gmail|gdrive|x|chatgpt)$/);
      if (connectorRoute) {
        const connector = connectorRoute[1] as ConnectorId;
        const identity = await brokerIdentity(env, accountAddress);
        logContext = {
          ...logContext,
          user_id: identity.userId,
          account_id: accountAddress,
          connector,
        };
        if (request.method === "POST") {
          const response = await startConnector(env, store, request, accountAddress, identity.userId, connector);
          console.info({
            type: "connect.connector.start",
            outcome: "success",
            ...logContext,
            status: "accepted",
          });
          return cors(response, request);
        }
        if (request.method === "DELETE") {
          await disconnectConnector(env, identity.userId, connector);
          console.info({
            type: "connect.connector.disconnect",
            outcome: "success",
            ...logContext,
            status: "disconnected",
          });
          return cors(new Response(null, { status: 204 }), request);
        }
        if (request.method === "GET" && connector === "chatgpt") {
          return cors(await pollChatGpt(env, identity.userId), request);
        }
        return error(request, 405, "method_not_allowed", "Unsupported connector operation.");
      }

      return error(request, 404, "not_found", "Route not found.");
    } catch (cause) {
      const failure = cause instanceof ApiFailure
        ? cause
        : new ApiFailure(500, "internal_error", "Unexpected Connect API failure.");
      const event = {
        type: "connect.api.failure",
        outcome: "failure",
        ...logContext,
        status: failure.code,
      };
      if (cause instanceof ApiFailure) {
        if (logContext.user_id && logContext.account_id) console.warn(event);
      } else {
        console.error(event);
      }
      return error(request, failure.status, failure.code, failure.message);
    }
  },
};

async function handleManagedMemoryRoute(
  request: Request,
  env: Env,
  url: URL,
): Promise<Response | undefined> {
  const isSearchPath = url.pathname === "/v1/history/sessions/search";
  const readMatch = url.pathname.match(/^\/v1\/history\/sessions\/([^/]+)\/read$/);
  const isMemoryPath = url.pathname === "/v1/memory";
  if (!isSearchPath && !readMatch && !isMemoryPath) return undefined;
  if (request.method !== "POST") {
    throw new ApiFailure(405, "method_not_allowed", "Hosted history and memory requests require POST.");
  }
  const isSearch = isSearchPath;
  const isMemory = isMemoryPath;
  if (url.search) {
    throw new ApiFailure(400, "invalid_managed_request", "Hosted history and memory requests do not accept query parameters.");
  }

  const app = requireCallerApp(request);
  if (app.appId !== CLI_APP_ID || app.origin !== CLI_APP_ORIGIN) {
    throw new ApiFailure(403, "app_identity_mismatch", "Hosted history and memory are available only to the Nanocodex CLI.");
  }
  const { grant } = await authenticatedGrant(request, env.CONNECT_STATE);
  if (grant.appId !== CLI_APP_ID || grant.appOrigin !== CLI_APP_ORIGIN) {
    throw new ApiFailure(403, "app_identity_mismatch", "The Connect grant is not bound to the Nanocodex CLI.");
  }
  if (grant.status !== "active") {
    throw new ApiFailure(403, "grant_inactive", "The Connect grant is not active.");
  }
  remainingGrantTtl(grant);
  const body = await boundedJson(request, MAX_MANAGED_MEMORY_REQUEST_BYTES, "hosted history or memory");
  const requiredCapability = managedMemoryCapability(url.pathname, body.operation);
  if (!requiredCapability) {
    throw new ApiFailure(400, "invalid_memory_operation", "The hosted history or memory operation is invalid.");
  }
  if (!grant.capabilities.includes(requiredCapability)) {
    const code = requiredCapability === "history:read"
      ? "history_read_not_granted"
      : requiredCapability === "memory:write"
        ? "memory_write_not_granted"
        : "memory_read_not_granted";
    throw new ApiFailure(403, code, `This Connect grant does not include ${requiredCapability} access.`);
  }
  if (isMemory) {
    const operation = body.operation;
    if (operation !== "scan" && operation !== "read" && operation !== "put" && operation !== "delete") {
      throw new ApiFailure(400, "invalid_memory_operation", "The hosted memory operation is invalid.");
    }
  }

  const target = new URL(url.pathname, "https://nanocodex.internal");
  const headers = new Headers(managedGrantHeaders(managedGrantAssertion(grant)));
  headers.set("content-type", "application/json");
  const upstream = await env.ACCOUNTS.fetch(new Request(target, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
    redirect: "manual",
    signal: request.signal,
  }));
  return safeManagedJsonResponse(upstream);
}

async function safeManagedJsonResponse(upstream: Response): Promise<Response> {
  if (upstream.status >= 300 && upstream.status < 400) {
    await upstream.body?.cancel();
    throw new ApiFailure(502, "managed_upstream_redirect", "The hosted service returned an unexpected redirect.");
  }
  if (!(upstream.headers.get("content-type") ?? "").includes("application/json")) {
    await upstream.body?.cancel();
    throw new ApiFailure(502, "managed_response_invalid", "The hosted service returned a non-JSON response.");
  }
  const bytes = new Uint8Array(await boundedResponseBytes(upstream, MAX_MANAGED_MEMORY_RESPONSE_BYTES));
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new ApiFailure(502, "managed_response_invalid", "The hosted service returned invalid JSON.");
  }
  return Response.json(value, { status: upstream.status });
}

class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

function failureStatus(cause: unknown): string {
  return cause instanceof ApiFailure ? cause.code : "internal_error";
}

async function createMcpIntent(request: Request, store: Kv.Kv): Promise<Response> {
  const app = requireCallerApp(request);
  if (app.appId !== CLI_APP_ID || app.origin !== CLI_APP_ORIGIN) {
    throw new ApiFailure(403, "mcp_intent_denied", "Remote MCP preflight is reserved for the Nanocodex CLI.");
  }
  const body = await boundedJson(request, 4 * 1024, "remote MCP intent");
  let target: Readonly<{ endpoint: string; name: string }>;
  try {
    target = canonicalRemoteMcpTarget(body.target);
  } catch (cause) {
    throw new ApiFailure(400, "invalid_mcp_target", errorText(cause));
  }
  if (!store.create) {
    throw new ApiFailure(503, "mcp_intent_unavailable", "Atomic remote MCP preflight is unavailable.");
  }
  const now = Math.floor(Date.now() / 1_000);
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const id = randomSubject();
    const intent: McpIntent = {
      appId: app.appId,
      appOrigin: app.origin,
      endpoint: target.endpoint,
      endpointHash: await digestHex(target.endpoint),
      expiresAt: now + MCP_INTENT_TTL,
      id,
      name: target.name,
    };
    if (await store.create(`mcp-intent:${id}`, intent, { ttl: MCP_INTENT_TTL })) {
      return Response.json({ id, name: target.name }, { status: 201 });
    }
  }
  throw new ApiFailure(503, "mcp_intent_unavailable", "The remote MCP intent could not be reserved.");
}

async function materializeApprovedMcpConnections(
  env: Env,
  store: Kv.Kv,
  app: CallerApp,
  brokerUserId: string,
  resources: readonly string[],
): Promise<McpConnection[]> {
  let ids: readonly string[];
  try {
    ids = validateMcpResources(resources).requested;
  } catch (cause) {
    throw new ApiFailure(403, "invalid_mcp_resources", errorText(cause));
  }
  if (ids.length === 0) return [];
  if (ids.length > 16 || !store.create) {
    throw new ApiFailure(403, "invalid_mcp_resources", "A Connect approval may contain at most 16 remote MCP connections.");
  }
  for (const id of ids) {
    const intent = await store.get<McpIntent>(`mcp-intent:${id}`);
    let canonicalIntentEndpoint: string | undefined;
    try { canonicalIntentEndpoint = canonicalRemoteMcpTarget(intent?.endpoint).endpoint; } catch { /* fail below */ }
    if (!isMcpIntent(intent)
      || intent.appId !== app.appId
      || intent.appOrigin !== app.origin
      || intent.expiresAt <= Math.floor(Date.now() / 1_000)
      || canonicalIntentEndpoint !== intent.endpoint
      || intent.endpointHash !== await digestHex(intent.endpoint)) {
      throw new ApiFailure(403, "mcp_intent_unavailable", "A signed remote MCP intent is unavailable or expired.");
    }
    const ownerKey = `mcp-intent-owner:${id}`;
    if (!await store.create(ownerKey, brokerUserId, { ttl: MCP_INTENT_TTL })) {
      const owner = await store.get<unknown>(ownerKey);
      if (owner !== brokerUserId) {
        throw new ApiFailure(403, "mcp_intent_claimed", "A remote MCP intent is already bound to another account.");
      }
    }
    const response = await brokerFetch(env, `/users/${encodeURIComponent(brokerUserId)}/mcp-connections/${id}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ endpoint: intent.endpoint, name: intent.name }),
    });
    if (!response.ok) {
      await response.body?.cancel();
      throw new ApiFailure(502, "mcp_broker_failed", "The remote MCP broker could not materialize this connection.");
    }
    await response.body?.cancel();
  }
  const statuses = await mcpConnectionStatuses(env, brokerUserId);
  const byId = new Map(statuses.map((connection) => [connection.id, connection]));
  return ids.map((id) => {
    const connection = byId.get(id);
    if (!connection) {
      throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker omitted a materialized connection.");
    }
    return connection;
  });
}

function isMcpIntent(value: unknown): value is McpIntent {
  return isRecord(value)
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && isMcpConnectionId(value.id)
    && typeof value.name === "string" && value.name.length > 0 && value.name.length <= 256
    && typeof value.endpoint === "string" && value.endpoint.length <= 2_048
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.endpointHash))
    && Number.isSafeInteger(value.expiresAt);
}

async function createEmbedIdentitySession(
  request: Request,
  env: Env,
  store: Kv.Kv,
): Promise<Response> {
  if (request.headers.has("origin")) {
    throw new ApiFailure(
      403,
      "embed_session_server_required",
      "Embedded identity sessions must be minted by the application server.",
    );
  }
  const appId = request.headers.get("x-nanocodex-app-id");
  const secret = request.headers.get("authorization")?.match(/^Bearer ([^\s]{32,512})$/)?.[1];
  let body: ReturnType<typeof parseEmbedSessionBody>;
  try {
    body = parseEmbedSessionBody(await boundedJson(
      request,
      MAX_EMBED_SESSION_REQUEST_BYTES,
      "embedded identity session",
    ));
  } catch (cause) {
    if (cause instanceof ApiFailure) throw cause;
    throw new ApiFailure(400, "invalid_embed_session", errorText(cause));
  }
  const project = appId && secret
    ? await authenticateEmbedProject(env.NANOCODEX_EMBED_PROJECTS, {
        appId,
        appOrigin: body.appOrigin,
        secret,
      })
    : undefined;
  if (!project) {
    throw new ApiFailure(401, "invalid_embed_project", "The Nanocodex embed project is not authorized.");
  }
  if (!store.create) {
    throw new ApiFailure(503, "embed_session_unavailable", "Atomic embedded identity sessions are unavailable.");
  }
  const token = randomSubject();
  const expiresAt = Math.floor(Date.now() / 1_000) + body.expiresIn;
  const identity: EmbedIdentitySession = {
    appId: project.appId,
    appOrigin: project.appOrigin,
    issuer: `urn:nanocodex:app:${project.appId}`,
    subject: body.subject,
    ...(body.organization === undefined ? {} : { organization: body.organization }),
    expiresAt,
  };
  if (!await store.create(`embed-session:${token}`, identity, { ttl: body.expiresIn })) {
    throw new ApiFailure(503, "embed_session_conflict", "The embedded identity session could not be reserved.");
  }
  return Response.json({ token, expires_at: expiresAt }, {
    status: 201,
    headers: { "cache-control": "no-store" },
  });
}

async function createHostedAuthorization(
  request: Request,
  env: Env,
  store: Kv.Kv,
): Promise<Response> {
  const body = await boundedJson(request, 16 * 1024, "hosted authorization");
  const code = opaqueToken(body.code, "code");
  const accountAddress = address(body.account_address);
  const appId = requiredString(body.app_id, "app_id");
  const appOrigin = requiredString(body.app_origin, "app_origin");
  const resources = stringResources(body.resources);
  const app = approvedAppContext(resources);
  if (app.appId !== CLI_APP_ID || app.origin !== CLI_APP_ORIGIN
    || appId !== app.appId || appOrigin !== app.origin) {
    throw new ApiFailure(403, "app_identity_mismatch", "Hosted authorization is reserved for the Nanocodex CLI.");
  }
  if (!resources.includes(HOSTED_AUTHORIZATION_RESOURCE)
    || resources.includes("urn:nanocodex:mpp:machusd:spend")) {
    throw new ApiFailure(403, "hosted_authorization_denied", "Hosted authorization cannot request MPP or spending access.");
  }
  const approvedConnectorSet = approvedConnectors(resources);
  const connectors = CONNECTOR_IDS.filter((connector) => approvedConnectorSet.has(connector));
  const approvedMcpIds = approvedMcpConnectionIds(resources);
  requireApprovedCapabilities(resources, app.appId, connectors, approvedMcpIds);

  let exchanged: Response;
  try {
    exchanged = await env.ACCOUNTS.fetch(new Request(
      "https://nanocodex.internal/connect/hosted-authorizations/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: app.appId,
          app_origin: app.origin,
          code,
          resources,
        }),
      },
    ));
  } catch {
    throw new ApiFailure(502, "hosted_authorization_unavailable", "The Nanocodex account service is unavailable.");
  }
  if (!exchanged.ok) {
    await exchanged.body?.cancel();
    throw new ApiFailure(403, "hosted_authorization_rejected", "The hosted account authorization was rejected.");
  }
  const identity = await exchanged.json() as unknown;
  if (!isHostedAuthorizationIdentity(identity)
    || identity.account_address.toLowerCase() !== accountAddress.toLowerCase()
    || !sameResources(identity.resources, resources)) {
    throw new ApiFailure(502, "hosted_authorization_invalid", "The Nanocodex account service returned an invalid authorization.");
  }

  const [status, mcpConnections] = await Promise.all([
    connectorStatuses(env, identity.user_id),
    materializeApprovedMcpConnections(env, store, app, identity.user_id, resources),
  ]);
  const approvalId = randomSubject();
  const token = randomSubject();
  const expiresAt = Math.floor(Date.now() / 1000) + CONNECT_APPROVAL_TTL;
  const connectedConnectors = CONNECTOR_IDS.filter((connector) => status.connectors[connector].connected);
  if (!store.create) {
    throw new ApiFailure(503, "hosted_authorization_unavailable", "Atomic hosted authorization is unavailable.");
  }
  await store.set(`connect-approval:${approvalId}`, {
    accountAddress,
    appId: app.appId,
    appOrigin: app.origin,
    authorization: "hosted",
    brokerUserId: identity.user_id,
    connectedConnectors,
    mcpConnections,
    profileLinked: true,
    resources,
  } satisfies ConnectApproval, { ttl: CONNECT_APPROVAL_TTL });
  if (!await store.create(`hosted-browser-session:${token}`, {
    accountAddress,
    expiresAt,
  } satisfies HostedBrowserSession, { ttl: CONNECT_APPROVAL_TTL })) {
    await store.delete(`connect-approval:${approvalId}`);
    throw new ApiFailure(503, "hosted_authorization_unavailable", "The hosted browser session could not be reserved.");
  }
  return Response.json({
    account_address: accountAddress,
    approval_id: approvalId,
    connectors: status.connectors,
    mcp_connections: mcpConnections,
    profile: { linked: true },
    token,
  });
}

async function readHostedBrowserSession(
  store: Kv.Kv,
  request: Request,
): Promise<HostedBrowserSession | undefined> {
  const token = request.headers.get("authorization")?.match(/^Bearer ([A-Za-z0-9_-]{43})$/i)?.[1];
  if (!token) return undefined;
  const session = await store.get<HostedBrowserSession>(`hosted-browser-session:${token}`);
  return isHostedBrowserSession(session) && session.expiresAt > Math.floor(Date.now() / 1000)
    ? session
    : undefined;
}

function isHostedBrowserSession(value: unknown): value is HostedBrowserSession {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && Number.isSafeInteger(value.expiresAt);
}

function isHostedAuthorizationIdentity(value: unknown): value is {
  linked: true;
  user_id: string;
  account_address: `0x${string}`;
  resources: readonly string[];
} {
  return isRecord(value)
    && value.linked === true
    && isBrokerUserId(value.user_id)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.account_address))
    && Array.isArray(value.resources)
    && value.resources.every((resource) => typeof resource === "string");
}

function stringResources(value: unknown): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > 64
    || value.some((resource) => typeof resource !== "string" || resource.length === 0 || resource.length > 512)
    || new Set(value).size !== value.length) {
    throw new ApiFailure(400, "invalid_resources", "Hosted authorization resources are invalid.");
  }
  return [...value] as string[];
}

async function startAccountLink(
  request: Request,
  store: Kv.Kv,
  accountAddress: `0x${string}`,
): Promise<Response> {
  if (!store.create) {
    throw new ApiFailure(500, "account_link_unavailable", "Atomic account linking is unavailable.");
  }
  const state = randomSubject();
  if (!await store.create(`account-link-state:${state}`, {
    accountAddress,
  } satisfies AccountLinkState, { ttl: ACCOUNT_LINK_TTL })) {
    throw new ApiFailure(503, "account_link_conflict", "The account-link request could not be reserved.");
  }
  const dialogOrigin = localDevelopmentPublicOrigin(request) ?? connectDialogOrigin(request);
  const authorize = new URL("/v1/connect/account-link", dialogOrigin);
  authorize.searchParams.set("account_address", accountAddress);
  authorize.searchParams.set("app_id", REGISTERED_APP_ID);
  authorize.searchParams.set("return_origin", dialogOrigin);
  authorize.searchParams.set("state", state);
  return Response.json({ authorization_url: authorize.href, state });
}

async function completeAccountLink(
  request: Request,
  env: Env,
  store: Kv.Kv,
  accountAddress: `0x${string}`,
): Promise<Response> {
  const body = await json(request);
  const code = opaqueToken(body.code, "code");
  const state = opaqueToken(body.state, "state");
  if (!store.take) {
    throw new ApiFailure(500, "account_link_unavailable", "Atomic account linking is unavailable.");
  }
  const correlation = await store.take<AccountLinkState>(`account-link-state:${state}`);
  if (!isAccountLinkState(correlation)
    || correlation.accountAddress.toLowerCase() !== accountAddress.toLowerCase()) {
    throw new ApiFailure(403, "invalid_account_link_state", "The account-link request expired or was already used.");
  }
  let response: Response;
  try {
    response = await env.ACCOUNTS.fetch(new Request(
      "https://nanocodex.internal/connect/account-links/exchange",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          account_address: accountAddress,
          app_id: REGISTERED_APP_ID,
          code,
          state,
        }),
      },
    ));
  } catch {
    throw new ApiFailure(502, "account_link_unavailable", "The Nanocodex account service is unavailable.");
  }
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(403, "account_link_rejected", "The Nanocodex account authorization was rejected.");
  }
  const linked = await response.json() as unknown;
  if (!isBrokerIdentityResponse(linked)) {
    throw new ApiFailure(502, "account_link_invalid", "The Nanocodex account service returned an invalid identity.");
  }
  const pendingMcp = await store.get<PendingMcpAccountLink>(
    `pending-mcp-account-link:${accountAddress.toLowerCase()}`,
  );
  const mcpConnections = isPendingMcpAccountLink(pendingMcp)
    ? await materializeApprovedMcpConnections(
        env,
        store,
        validateCallerApp(pendingMcp.appId, pendingMcp.appOrigin),
        linked.user_id,
        pendingMcp.resources,
      )
    : [];
  if (isPendingMcpAccountLink(pendingMcp)) {
    await store.delete(`pending-mcp-account-link:${accountAddress.toLowerCase()}`);
  }
  return Response.json({
    linked: true,
    ...(await connectorStatuses(env, linked.user_id)),
    mcp_connections: mcpConnections,
  });
}

function isPendingMcpAccountLink(value: unknown): value is PendingMcpAccountLink {
  return isRecord(value)
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && Array.isArray(value.resources)
    && value.resources.length <= 64
    && value.resources.every((resource) => typeof resource === "string" && resource.length <= 512);
}

async function brokerIdentity(
  env: Env,
  accountAddress: `0x${string}`,
): Promise<{ linked: boolean; userId: string }> {
  let response: Response;
  try {
    const url = new URL("https://nanocodex.internal/connect/account-links/resolve");
    url.searchParams.set("account_address", accountAddress);
    response = await env.ACCOUNTS.fetch(new Request(url));
  } catch {
    throw new ApiFailure(502, "account_link_unavailable", "The Nanocodex account service is unavailable.");
  }
  if (response.status === 404) {
    await response.body?.cancel();
    return { linked: false, userId: accountAddress };
  }
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(502, "account_link_unavailable", "The Nanocodex account service rejected identity resolution.");
  }
  const body = await response.json() as unknown;
  if (!isBrokerIdentityResponse(body)) {
    throw new ApiFailure(502, "account_link_invalid", "The Nanocodex account service returned an invalid identity.");
  }
  return { linked: true, userId: body.user_id };
}

function isBrokerIdentityResponse(value: unknown): value is { linked: true; user_id: string } {
  return isRecord(value)
    && value.linked === true
    && typeof value.user_id === "string"
    && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.user_id);
}

function isAccountLinkState(value: unknown): value is AccountLinkState {
  return isRecord(value) && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress));
}

function opaqueToken(value: unknown, label: string): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value)) {
    throw new ApiFailure(400, "invalid_account_link", `${label} must be a 32-byte opaque token.`);
  }
  return value;
}

async function createConnection(
  request: Request,
  env: Env,
  store: Kv.Kv,
  context: WorkerContext,
): Promise<Response> {
  const startedAt = performance.now();
  const timings: Array<readonly [string, number]> = [];
  const mark = (name: string) => timings.push([name, performance.now()]);
  const body = await connectionRequestBody(request);
  const appId = requiredString(body.app_id, "app_id");
  const app = requireCallerApp(request, appId);
  const accountAddress = address(body.account_address);
  const approvalId = requiredString(body.approval_id, "approval_id");
  const permission = requiredString(body.permission, "permission");
  if (permission !== "agent.run") {
    throw new ApiFailure(403, "permission_not_supported", "This app may request only the agent.run permission.");
  }
  const requested = requestedConnectors(body.requested_connectors);
  const requestedMcpIds = requestedMcpConnections(body.requested_mcp_connections);
  const approval = await readConnectApproval(store, approvalId, accountAddress);
  mark("approval");
  if (approval.appId !== app.appId || approval.appOrigin !== app.origin) {
    throw new ApiFailure(403, "app_not_approved", "The signed approval is not bound to this app origin.");
  }
  requireApprovedCapabilities(approval.resources, appId, requested, requestedMcpIds);
  const agentCapabilities = approvedAgentCapabilities(approval.resources);
  const credentialImport = await approvedChatGptCredentialImport(
    body.chatgpt_credential_import,
    approval,
    app,
    requested,
  );

  const retainedIdentity = approval.profileLinked === true && isBrokerUserId(approval.brokerUserId)
    ? { linked: true, userId: approval.brokerUserId }
    : undefined;
  const identity = retainedIdentity ?? await brokerIdentity(env, accountAddress);
  mark("identity");
  if (!identity.linked) {
    throw new ApiFailure(403, "account_link_required", "Link this Tempo account to your Nanocodex profile before authorizing a durable agent.");
  }
  const externalPrincipalId = approval.externalPrincipalId
    ? await bindEmbedPrincipal(store, approval.externalPrincipalId, identity.userId)
    : undefined;
  if (externalPrincipalId) mark("external-identity");
  const credential = await connectionCredential(
    store,
    body,
    approval,
    app,
    accountAddress,
  );
  mark("access-key");
  const { accessKey, expiresAt, persist } = credential;
  const now = Math.floor(Date.now() / 1000);
  const grantTtl = expiresAt - now;
  if (grantTtl <= 0) {
    throw new ApiFailure(403, "access_key_expired", "The delegated access key has expired.");
  }
  if (credentialImport) {
    await importChatGptCredential(env, identity.userId, credentialImport);
    mark("credential-import");
  }
  // Provisioning a credential is not approval consumption. Recheck every live
  // connector and MCP immediately after broker success, then consume the
  // approval before creating any grant state.
  const liveConnectorStatuses = (await connectorStatuses(env, identity.userId)).connectors;
  const connectors = requested.filter((connector) => liveConnectorStatuses[connector].connected);
  requireRequestedConnectors(connectors, requested);
  if (credentialImport
    && liveConnectorStatuses.chatgpt.account_id !== credentialImport.account_id) {
    throw new ApiFailure(
      409,
      "chatgpt_credential_mismatch",
      "The retained ChatGPT credential does not match the approved import.",
    );
  }
  const mcpConnections = await connectedRequestedMcpConnections(env, identity.userId, requestedMcpIds);
  mark("live-capabilities");
  const consumedApproval = await takeConnectApproval(store, approvalId, accountAddress);
  if (JSON.stringify(consumedApproval) !== JSON.stringify(approval)) {
    throw new ApiFailure(403, "approval_unavailable", "The signed Connect approval changed before it was consumed.");
  }
  const appScope = await scopedAppId(app);
  const grantId = await digestHex(`grant:${randomSubject()}`);
  const grantCapabilities = [
    "nanocodex.agent",
    ...(externalPrincipalId ? ["identity.external"] : []),
    ...approvedHostedCapabilities(approval.resources),
    ...agentCapabilities,
    ...connectors,
    ...mcpConnections.map((connection) => `mcp:${connection.id}`),
  ];
  const grantAssertion: ManagedGrantAssertion = {
    brokerUserId: identity.userId,
    capabilities: grantCapabilities,
    connectors,
    grantId,
    mcpIds: mcpConnections.map(({ id }) => id),
  };
  const [durableAgentId, egressSubject] = await Promise.all([
    appId === CHROME_EXTENSION_APP_ID
      ? agentId(accountAddress)
      : isConnectAgentId(approval.durableAgentId)
        ? Promise.resolve(approval.durableAgentId)
        : connectManagedAgent(env, store, appScope, grantAssertion),
    connectEgressSubject(env, store, identity.userId, appScope),
  ]);
  mark("capabilities");
  const grantToken = randomSubject();
  const grant: GrantRecord = {
    id: grantId,
    appId,
    appOrigin: app.origin,
    accountAddress,
    brokerUserId: identity.userId,
    agentId: durableAgentId,
    permission,
    status: "active",
    expiresAt,
    capabilities: grantCapabilities,
    mcpConnections: mcpConnections.map(({ id, name }) => ({ id, name })),
    ...(accessKey ? { accessKey } : {}),
    spentAtomics: "0",
    egressSubject,
    sharedEgressSubject: true,
    ...(externalPrincipalId ? { externalPrincipalId } : {}),
  };

  try {
    const wireResult = connectionWire(grant, grantToken);
    mark("subject");

    if (!store.create) {
      throw new ApiFailure(500, "grant_token_unavailable", "The grant session could not be created.");
    }
    const writes = await Promise.allSettled([
      store.set(`grant:${grant.id}`, grant, { ttl: grantTtl }),
      store.create(`grant-token:${grantToken}`, {
        accountAddress,
        appId,
        appOrigin: app.origin,
        grantId: grant.id,
        ...(externalPrincipalId ? { externalPrincipalId } : {}),
      } satisfies GrantPrincipal, { ttl: grantTtl }),
      ...(persist && accessKey ? [store.set(accessKeyStorageKey(accountAddress, accessKey.key_id), {
        accountAddress,
        appId,
        appOrigin: app.origin,
        accessKey,
      } satisfies AccessKeyRecord, { ttl: grantTtl })] : []),
    ]);
    const writeFailure = writes.find((result) => result.status === "rejected");
    if (writeFailure?.status === "rejected") throw writeFailure.reason;
    if (writes[1]?.status !== "fulfilled" || writes[1].value !== true) {
      throw new ApiFailure(500, "grant_token_unavailable", "The grant session could not be created.");
    }
    mark("grant");
    console.info({
      type: "connect.grant.create",
      outcome: "success",
      user_id: grant.brokerUserId,
      account_id: grant.accountAddress,
      grant_id: grant.id,
      agent_id: grant.agentId,
      app_id: grant.appId,
      ...(env.DEPLOYMENT_SHA === undefined ? {} : { deployment_sha: env.DEPLOYMENT_SHA }),
      status: grant.status,
    });
    context.waitUntil(appendGrantIndex(store, grant.accountAddress, grant.id).catch(() => {
      console.error({
        type: "connect.grant.index",
        outcome: "failure",
        user_id: grant.brokerUserId,
        account_id: grant.accountAddress,
        grant_id: grant.id,
        agent_id: grant.agentId,
        app_id: grant.appId,
        ...(env.DEPLOYMENT_SHA === undefined ? {} : { deployment_sha: env.DEPLOYMENT_SHA }),
        status: "index_update_failed",
      });
    }));
    return Response.json(wireResult, {
      status: 201,
      headers: { "server-timing": serverTiming(startedAt, timings) },
    });
  } catch (cause) {
    const event = {
      type: "connect.grant.create",
      outcome: "failure",
      user_id: grant.brokerUserId,
      account_id: grant.accountAddress,
      grant_id: grant.id,
      agent_id: grant.agentId,
      app_id: grant.appId,
      ...(env.DEPLOYMENT_SHA === undefined ? {} : { deployment_sha: env.DEPLOYMENT_SHA }),
      status: failureStatus(cause),
    };
    if (cause instanceof ApiFailure) console.warn(event);
    else console.error(event);
    const cleanup: Promise<unknown>[] = [
      store.delete(`grant:${grant.id}`),
      store.delete(`grant-token:${grantToken}`),
    ];
    if (grant.sharedEgressSubject !== true) {
      cleanup.push(unbindSubject(env, grant.egressSubject, grant.brokerUserId));
    }
    if (persist && accessKey) cleanup.push(store.delete(accessKeyStorageKey(accountAddress, accessKey.key_id)));
    await Promise.allSettled(cleanup);
    throw cause;
  }
}

async function connectionRequestBody(request: Request): Promise<Record<string, unknown>> {
  const body = await boundedJson(request, MAX_CONNECTION_REQUEST_BYTES, "connection");
  const allowed = new Set([
    "account_address",
    "app_id",
    "approval_id",
    "authorization_mode",
    "chatgpt_credential_import",
    "key_authorization",
    "permission",
    "requested_connectors",
    "requested_mcp_connections",
    "reuse_access_key",
    "signed_key_authorization",
  ]);
  if (Object.keys(body).some((key) => !allowed.has(key))) {
    throw new ApiFailure(400, "invalid_connection_request", "The connection request contains an unknown field.");
  }
  return body;
}

async function approvedChatGptCredentialImport(
  value: unknown,
  approval: ConnectApproval,
  app: CallerApp,
  requested: readonly ConnectorId[],
): Promise<ChatGptCredentialImport | undefined> {
  let approvedDigest: string | undefined;
  try {
    approvedDigest = credentialImportDigestFromResources(approval.resources);
  } catch {
    throw new ApiFailure(403, "invalid_credential_import_resource", "The signed credential import resource is invalid.");
  }
  if ((value === undefined) !== (approvedDigest === undefined)) {
    throw new ApiFailure(
      403,
      "credential_import_mismatch",
      "The ChatGPT credential body and signed import resource must be provided together.",
    );
  }
  if (value === undefined || approvedDigest === undefined) return undefined;
  if (app.appId !== CLI_APP_ID || app.origin !== CLI_APP_ORIGIN
    || approval.appId !== CLI_APP_ID || approval.appOrigin !== CLI_APP_ORIGIN
    || !approvedConnectors(approval.resources).has("chatgpt")
    || !requested.includes("chatgpt")) {
    throw new ApiFailure(
      403,
      "credential_import_not_approved",
      "ChatGPT credential import is reserved for an exact Nanocodex CLI ChatGPT approval.",
    );
  }
  let credential: ChatGptCredentialImport;
  try {
    credential = parseChatGptCredentialImport(value);
  } catch {
    throw new ApiFailure(400, "invalid_chatgpt_credential", "The ChatGPT credential import is invalid.");
  }
  if (credential.expires_at <= Date.now()) {
    throw new ApiFailure(400, "invalid_chatgpt_credential", "The ChatGPT credential import is expired.");
  }
  if (await chatGptCredentialImportDigest(credential) !== approvedDigest) {
    throw new ApiFailure(
      403,
      "credential_import_mismatch",
      "The ChatGPT credential import does not match its signed commitment.",
    );
  }
  return credential;
}

async function importChatGptCredential(
  env: Env,
  brokerUserId: string,
  credential: ChatGptCredentialImport,
): Promise<void> {
  const encoded = JSON.stringify(credential);
  if (new TextEncoder().encode(encoded).byteLength > MAX_CHATGPT_IMPORT_BODY_BYTES) {
    throw new ApiFailure(413, "request_too_large", "The ChatGPT credential import is too large.");
  }
  let response: Response;
  try {
    response = await env.EGRESS.fetch(new Request(
      `https://nanocodex.internal/users/${encodeURIComponent(brokerUserId)}/credentials/chatgpt`,
      {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: encoded,
      },
    ));
  } catch {
    throw new ApiFailure(502, "chatgpt_credential_import_failed", "The credential broker is unavailable.");
  }
  if (response.status !== 204) {
    await response.body?.cancel();
    if (response.status === 409) {
      throw new ApiFailure(
        409,
        "chatgpt_credential_conflict",
        "A different live ChatGPT account is already connected.",
      );
    }
    throw new ApiFailure(502, "chatgpt_credential_import_failed", "The credential broker rejected the import.");
  }
  await response.body?.cancel();
}

async function connectionCredential(
  store: Kv.Kv,
  body: Record<string, unknown>,
  approval: ConnectApproval,
  app: CallerApp,
  accountAddress: `0x${string}`,
): Promise<{ accessKey?: Record<string, unknown>; expiresAt: number; persist: boolean }> {
  if (approval.authorization === "hosted") {
    if (body.authorization_mode !== "hosted"
      || body.key_authorization !== undefined
      || body.signed_key_authorization !== undefined
      || body.reuse_access_key !== undefined
      || approval.keyAuthorization !== undefined
      || !approval.resources.includes(HOSTED_AUTHORIZATION_RESOURCE)
      || approval.resources.includes("urn:nanocodex:mpp:machusd:spend")) {
      throw new ApiFailure(403, "hosted_authorization_denied", "This hosted grant cannot carry an access key or MPP authority.");
    }
    return {
      expiresAt: Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60,
      persist: false,
    };
  }
  if (body.authorization_mode !== undefined && body.authorization_mode !== "access_key") {
    throw new ApiFailure(400, "invalid_authorization_mode", "The connection authorization mode is invalid.");
  }
  const result = await connectionAccessKey(store, body, approval, app, accountAddress);
  return {
    ...result,
    expiresAt: safeInteger(result.accessKey.expiry, "access_key.expiry"),
  };
}

function serverTiming(startedAt: number, marks: readonly (readonly [string, number])[]): string {
  let previous = startedAt;
  return marks.map(([name, timestamp]) => {
    const duration = timestamp - previous;
    previous = timestamp;
    return `${name};dur=${duration.toFixed(1)}`;
  }).join(", ");
}

async function connectionAccessKey(
  store: Kv.Kv,
  body: Record<string, unknown>,
  approval: ConnectApproval,
  app: CallerApp,
  accountAddress: `0x${string}`,
): Promise<{ accessKey: Record<string, unknown>; persist: boolean }> {
  const hasNew = body.key_authorization !== undefined || body.signed_key_authorization !== undefined;
  const hasReuse = body.reuse_access_key !== undefined;
  if (hasNew === hasReuse) {
    throw new ApiFailure(400, "invalid_access_key", "Provide exactly one new or reusable access key.");
  }

  if (hasNew) {
    if (!isRecord(body.key_authorization)) {
      throw new ApiFailure(400, "invalid_access_key", "key_authorization must be an object.");
    }
    const serialized = hex(body.signed_key_authorization, "signed_key_authorization");
    if (!approval.keyAuthorization
      || approval.keyAuthorization.toLowerCase() !== serialized.toLowerCase()) {
      throw new ApiFailure(403, "access_key_not_approved", "The access key does not match the signed Connect approval.");
    }
    const accessKey = accessKeyWire(body.key_authorization, serialized, accountAddress);
    validateGrantAccessKey(accessKey, app.appId, approval.resources);
    return { accessKey, persist: true };
  }

  if (approval.keyAuthorization) {
    throw new ApiFailure(403, "access_key_not_approved", "A newly approved access key cannot be replaced with a reused key.");
  }
  if (!isRecord(body.reuse_access_key)) {
    throw new ApiFailure(400, "invalid_access_key", "reuse_access_key must be an object.");
  }
  if (Object.keys(body.reuse_access_key).some((key) => key !== "key_id" && key !== "expiry")) {
    throw new ApiFailure(400, "invalid_access_key", "reuse_access_key contains an unknown field.");
  }
  const keyId = address(body.reuse_access_key.key_id);
  const claimedExpiry = safeInteger(body.reuse_access_key.expiry, "reuse_access_key.expiry");
  const stored = await store.get<AccessKeyRecord>(accessKeyStorageKey(accountAddress, keyId));
  if (!isAccessKeyRecord(stored)
    || stored.appId !== app.appId
    || stored.appOrigin !== app.origin
    || stored.accountAddress.toLowerCase() !== accountAddress.toLowerCase()) {
    throw new ApiFailure(403, "access_key_unavailable", "The reusable access key is unavailable for this app and account.");
  }
  const storedKeyId = address(stored.accessKey.key_id);
  const storedExpiry = safeInteger(stored.accessKey.expiry, "stored access_key.expiry");
  if (storedKeyId.toLowerCase() !== keyId.toLowerCase() || storedExpiry !== claimedExpiry) {
    throw new ApiFailure(403, "access_key_unavailable", "The reusable access key does not match the requested key.");
  }
  const normalized = accessKeyWire(
    stored.accessKey,
    hex(stored.accessKey.authorization, "stored access_key.authorization"),
    accountAddress,
  );
  validateGrantAccessKey(normalized, app.appId, approval.resources);
  return { accessKey: normalized, persist: false };
}

function validateGrantAccessKey(
  accessKey: Record<string, unknown>,
  appId: string,
  resources: readonly string[],
): void {
  if (accessKey.chain_id !== "4217") {
    throw new ApiFailure(403, "invalid_access_key_chain", "The access key must be authorized for Tempo chain 4217.");
  }
  const expiry = safeInteger(accessKey.expiry, "access_key.expiry");
  if (expiry <= Math.floor(Date.now() / 1000)) {
    throw new ApiFailure(403, "access_key_expired", "The delegated access key has expired.");
  }
  hex(accessKey.authorization, "access_key.authorization");
  if (!Array.isArray(accessKey.limits) || !Array.isArray(accessKey.scopes)) {
    throw new ApiFailure(403, "invalid_access_key_policy", "The access key policy is incomplete.");
  }
  if (appId === CHROME_EXTENSION_APP_ID) {
    if (accessKey.limits.length !== 0 || accessKey.scopes.length !== 0) {
      throw new ApiFailure(403, "invalid_access_key_policy", "The Chrome extension access key cannot spend funds or call contracts.");
    }
    return;
  }
  if (appId === CLI_APP_ID && !resources.includes("urn:nanocodex:mpp:machusd:spend")) {
    if (accessKey.scopes.length !== 0
      || !hasZeroSpendPolicy(accessKey.limits)) {
      throw new ApiFailure(403, "invalid_access_key_policy", "A CLI key without MPP cannot spend funds or call contracts.");
    }
    return;
  }
  const limits = new Map<string, { limit: string; period?: number }>();
  for (const value of accessKey.limits) {
    if (!isRecord(value) || typeof value.limit !== "string") {
      throw new ApiFailure(403, "invalid_access_key_policy", "The access key spending limits are invalid.");
    }
    const token = address(value.token).toLowerCase();
    if (limits.has(token)) {
      throw new ApiFailure(403, "invalid_access_key_policy", "The access key spending limits contain duplicates.");
    }
    limits.set(token, {
      limit: value.limit,
      ...(Number.isSafeInteger(value.period) ? { period: value.period as number } : {}),
    });
  }
  if (limits.size !== 2
    || !matchesLimit(limits.get(MACHINE_USD.toLowerCase()))
    || !matchesLimit(limits.get(USDC_E.toLowerCase()))) {
    throw new ApiFailure(403, "invalid_access_key_policy", "The access key must contain the bounded Nanocodex MPP limits.");
  }

  const actualScopes = new Set(accessKey.scopes.map(scopeKey));
  const expectedScopes = new Set([
    scopeKey({ address: USDC_E, selector: "0xa9059cbb", recipients: [MERCATOR_SETTLEMENT] }),
    scopeKey({ address: USDC_E, selector: "0x95777d59", recipients: [MERCATOR_SETTLEMENT] }),
    scopeKey({ address: MACHINE_USD, selector: "0x095ea7b3", recipients: [MACHINE_USD_SWAPPER] }),
    scopeKey({ address: MACHINE_USD_SWAPPER, selector: "0x34189fed" }),
    scopeKey({ address: TIP20_CHANNEL_ESCROW, selector: "0xedc53b00" }),
    scopeKey({ address: TIP20_CHANNEL_ESCROW, selector: "0xdc48471e" }),
  ]);
  if (actualScopes.size !== expectedScopes.size
    || [...actualScopes].some((scope) => !expectedScopes.has(scope))) {
    throw new ApiFailure(403, "invalid_access_key_policy", "The access key must contain the bounded Nanocodex call scopes.");
  }
}

function matchesLimit(value: { limit: string; period?: number } | undefined): boolean {
  return value?.limit === MPP_LIMIT.toString() && value.period === MPP_PERIOD;
}

function hasZeroSpendPolicy(values: unknown[]): boolean {
  if (values.length !== 2) return false;
  const tokens = new Set(values.map((value) => {
    if (!isRecord(value) || value.limit !== "0" || value.period !== 0) return "";
    return address(value.token).toLowerCase();
  }));
  return tokens.size === 2
    && tokens.has(MACHINE_USD.toLowerCase())
    && tokens.has(USDC_E.toLowerCase());
}

function scopeKey(value: unknown): string {
  if (!isRecord(value) || typeof value.selector !== "string" || !/^0x[0-9a-fA-F]{8}$/.test(value.selector)) {
    throw new ApiFailure(403, "invalid_access_key_policy", "An access key call scope is invalid.");
  }
  const recipients = value.recipients === undefined
    ? []
    : Array.isArray(value.recipients)
      ? value.recipients.map(address).map((recipient) => recipient.toLowerCase()).sort()
      : (() => { throw new ApiFailure(403, "invalid_access_key_policy", "An access key recipient scope is invalid."); })();
  return `${address(value.address).toLowerCase()}:${value.selector.toLowerCase()}:${recipients.join(",")}`;
}

async function handleGrantRoute(
  request: Request,
  env: Env,
  store: Kv.Kv,
  url: URL,
): Promise<Response | undefined> {
  if (request.method === "POST" && url.pathname === "/v1/connections/disconnect") {
    const authenticated = await authenticatedGrant(request, env.CONNECT_STATE);
    await withGrantMutationLock(store, authenticated.grant.id, async () => {
      const current = await authenticatedGrant(request, env.CONNECT_STATE, authenticated.grant.id);
      await revokeGrant(env, store, current.grant, current.token);
    });
    return new Response(null, { status: 204 });
  }

  const grantRoute = url.pathname.match(/^\/v1\/grants\/(0x[0-9a-fA-F]{64})(?:\/(.*))?$/);
  if (!grantRoute) return undefined;
  const grantId = grantRoute[1] as `0x${string}`;
  const action = grantRoute[2];
  const { grant, token } = await authenticatedGrant(request, env.CONNECT_STATE, grantId);

  if (action === undefined && request.method === "GET") {
    return Response.json(connectionWire(grant, token));
  }
  const mcpAction = action?.match(/^mcp\/([A-Za-z0-9_-]{43})$/);
  if (mcpAction) {
    return grantMcpRequest(request, env, grant, mcpAction[1]!);
  }
  if (action === "mpp/balance" && request.method === "GET") {
    if (!grant.capabilities.includes("mpp.machusd") || !grant.accessKey) {
      throw new ApiFailure(403, "mpp_not_granted", "This connection has no MPP authority.");
    }
    const refreshed = await withGrantMutationLock(store, grant.id, async () => {
      const current = await authenticatedGrant(request, env.CONNECT_STATE, grantId);
      const [balance, settlementBalance] = await connectionBalances(current.grant.accountAddress);
      const updated = {
        ...current.grant,
        balanceAtomics: balance.toString(),
        settlementBalanceAtomics: settlementBalance.toString(),
      };
      await store.set(`grant:${updated.id}`, updated, { ttl: remainingGrantTtl(updated) });
      return connectionWire(updated, current.token);
    });
    return Response.json(refreshed);
  }
  if (action === "revoke" && request.method === "POST") {
    const revoked = await withGrantMutationLock(store, grant.id, async () => {
      const current = await authenticatedGrant(request, env.CONNECT_STATE, grantId);
      return revokeGrant(env, store, current.grant, current.token);
    });
    return Response.json(grantWire(revoked));
  }
  if (action === "model/ticket" && request.method === "POST") {
    return Response.json(await issueModelTicket(store, grant, await json(request)));
  }
  const toolHostTicket = action?.match(/^agents\/([^/]+)\/tool-host\/ticket$/);
  if (toolHostTicket && request.method === "POST") {
    if (url.search) {
      throw new ApiFailure(400, "invalid_tool_host_request", "Tool host tickets do not accept query parameters.");
    }
    requireGrantAppOrigin(request, grant);
    const requestedAgentId = decodeURIComponent(toolHostTicket[1]!);
    if (requestedAgentId !== grant.agentId) {
      throw new ApiFailure(403, "agent_not_granted", "This durable agent is outside the signed Connect authorization.");
    }
    return Response.json(await issueToolHostTicket(store, grant));
  }
  const realtimeTicket = action?.match(/^agents\/([^/]+)\/realtime\/ticket$/);
  if (realtimeTicket && request.method === "POST") {
    requireGrantAppOrigin(request, grant);
    const requestedAgentId = decodeURIComponent(realtimeTicket[1]!);
    if (requestedAgentId !== grant.agentId) {
      throw new ApiFailure(403, "agent_not_granted", "This durable agent is outside the signed Connect authorization.");
    }
    return Response.json(await issueRealtimeTicket(store, grant, await json(request)));
  }
  if (action === "mpp/charge" && request.method === "POST") {
    const body = await json(request);
    return Response.json(await withGrantMutationLock(store, grant.id, async () => {
      const current = await authenticatedGrant(request, env.CONNECT_STATE, grantId);
      return chargeGrant(store, current.grant, current.token, body);
    }));
  }
  if (action?.startsWith("agents/")) {
    requireGrantAppOrigin(request, grant);
    return proxyManagedAgent(request, env, grant, action.slice("agents/".length));
  }
  throw new ApiFailure(405, "method_not_allowed", "Unsupported grant operation.");
}

async function connectManagedAgent(
  env: Env,
  store: Kv.Kv,
  appId: string,
  assertion: ManagedGrantAssertion,
): Promise<string> {
  if (!store.create) {
    throw new ApiFailure(500, "durable_agent_unavailable", "Atomic durable-agent provisioning is unavailable.");
  }
  const recordKey = `connect-agent:${appId}:${assertion.brokerUserId}`;
  const retained = await store.get<unknown>(recordKey);
  if (isConnectAgentRecord(retained)) return retained.agentId;
  const lockKey = `${recordKey}:lock`;
  const lockValue = randomSubject();
  let acquired = false;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    acquired = await store.create(lockKey, lockValue, { ttl: 60 });
    if (acquired) break;
    await new Promise((resolve) => setTimeout(resolve, 25 * (attempt + 1)));
  }
  if (!acquired) {
    throw new ApiFailure(409, "durable_agent_busy", "The account's durable agent is already being provisioned.");
  }
  try {
    const retainedAfterLock = await store.get<unknown>(recordKey);
    if (isConnectAgentRecord(retainedAfterLock)) return retainedAfterLock.agentId;

    const agentId = await createManagedAgent(env, assertion);
    try {
      await store.set(recordKey, { agentId } satisfies ConnectAgentRecord);
      return agentId;
    } catch (cause) {
      await deleteManagedAgent(env, assertion, agentId).catch(() => {});
      throw cause;
    }
  } finally {
    await store.delete(lockKey);
  }
}

function managedGrantAssertion(grant: GrantRecord): ManagedGrantAssertion {
  return {
    brokerUserId: grant.brokerUserId,
    capabilities: grant.capabilities,
    connectors: CONNECTOR_IDS.filter((connector) => grant.capabilities.includes(connector)),
    grantId: grant.id,
    mcpIds: (grant.mcpConnections ?? []).map(({ id }) => id),
  };
}

function isConnectAgentRecord(value: unknown): value is ConnectAgentRecord {
  return isRecord(value)
    && isConnectAgentId(value.agentId);
}

function isConnectAgentId(value: unknown): value is string {
  return typeof value === "string"
    && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
}

async function connectBrokerIdentity(
  env: Env,
  store: Kv.Kv,
  accountAddress: `0x${string}`,
): Promise<{ linked: boolean; userId: string }> {
  const key = `connect-identity:${accountAddress.toLowerCase()}`;
  const retained = await store.get<unknown>(key);
  if (isConnectIdentityRecord(retained, accountAddress)) {
    return { linked: true, userId: retained.brokerUserId };
  }
  const identity = await brokerIdentity(env, accountAddress);
  if (identity.linked) {
    await store.set(key, {
      accountAddress,
      brokerUserId: identity.userId,
    } satisfies ConnectIdentityRecord, { ttl: ACCOUNT_LINK_TTL });
  }
  return identity;
}

function isConnectIdentityRecord(
  value: unknown,
  accountAddress: `0x${string}`,
): value is ConnectIdentityRecord {
  return isRecord(value)
    && typeof value.accountAddress === "string"
    && value.accountAddress.toLowerCase() === accountAddress.toLowerCase()
    && isBrokerUserId(value.brokerUserId);
}

async function bindEmbedPrincipal(
  store: Kv.Kv,
  principalId: string,
  brokerUserId: string,
): Promise<string> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(principalId) || !isBrokerUserId(brokerUserId) || !store.create) {
    throw new ApiFailure(503, "embed_identity_unavailable", "Atomic embedded identity linking is unavailable.");
  }
  const key = `embed-identity:${principalId}`;
  const retained = await store.get<unknown>(key);
  if (isEmbedIdentityBinding(retained, principalId)) {
    if (retained.brokerUserId !== brokerUserId) {
      throw new ApiFailure(
        409,
        "embed_identity_conflict",
        "This application identity is already linked to another Nanocodex profile.",
      );
    }
    return retained.principalId;
  }
  const candidate: EmbedIdentityBinding = { brokerUserId, principalId };
  if (await store.create(key, candidate)) return principalId;
  const winner = await store.get<unknown>(key);
  if (!isEmbedIdentityBinding(winner, principalId) || winner.brokerUserId !== brokerUserId) {
    throw new ApiFailure(
      409,
      "embed_identity_conflict",
      "This application identity is already linked to another Nanocodex profile.",
    );
  }
  return winner.principalId;
}

function isEmbedIdentityBinding(
  value: unknown,
  principalId: string,
): value is EmbedIdentityBinding {
  return isRecord(value)
    && isBrokerUserId(value.brokerUserId)
    && value.principalId === principalId
    && /^[A-Za-z0-9_-]{43}$/.test(value.principalId);
}

async function connectEgressSubject(
  env: Env,
  store: Kv.Kv,
  brokerUserId: string,
  appId: string,
): Promise<string> {
  if (!store.create) {
    throw new ApiFailure(500, "egress_subject_unavailable", "Atomic connector identity storage is unavailable.");
  }
  const key = `connect-subject:${appId}:${brokerUserId}`;
  const retained = await store.get<unknown>(key);
  if (isConnectSubjectRecord(retained, brokerUserId, appId)) return retained.subject;

  const candidate: ConnectSubjectRecord = { appId, brokerUserId, subject: randomSubject() };
  await bindSubject(env, candidate.subject, brokerUserId);
  if (await store.create(key, candidate)) return candidate.subject;

  const winner = await store.get<unknown>(key);
  await unbindSubject(env, candidate.subject, brokerUserId).catch(() => {});
  if (!isConnectSubjectRecord(winner, brokerUserId, appId)) {
    throw new ApiFailure(500, "egress_subject_unavailable", "The connector identity could not be retained.");
  }
  return winner.subject;
}

function isConnectSubjectRecord(
  value: unknown,
  brokerUserId: string,
  appId: string,
): value is ConnectSubjectRecord {
  return isRecord(value)
    && value.appId === appId
    && value.brokerUserId === brokerUserId
    && typeof value.subject === "string"
    && EGRESS_SUBJECT.test(value.subject);
}

async function createManagedAgent(env: Env, assertion: ManagedGrantAssertion): Promise<string> {
  const response = await env.ACCOUNTS.fetch(new Request("https://nanocodex.internal/v1/agents", {
    method: "POST",
    headers: managedGrantHeaders(assertion),
  }));
  const body = await response.json().catch(() => undefined) as unknown;
  if (!response.ok || !isRecord(body) || typeof body.agent_id !== "string") {
    throw new ApiFailure(503, "durable_agent_unavailable", "The durable Nanocodex agent could not be provisioned.");
  }
  return body.agent_id;
}

async function deleteManagedAgent(
  env: Env,
  assertion: ManagedGrantAssertion,
  agentId: string,
): Promise<void> {
  const response = await env.ACCOUNTS.fetch(new Request(
    `https://nanocodex.internal/v1/agents/${encodeURIComponent(agentId)}`,
    {
      method: "DELETE",
      headers: managedGrantHeaders(assertion),
    },
  ));
  await response.body?.cancel();
}

async function proxyManagedAgent(
  request: Request,
  env: Env,
  grant: GrantRecord,
  resource: string,
): Promise<Response> {
  const slash = resource.indexOf("/");
  const requestedAgentId = slash === -1 ? resource : resource.slice(0, slash);
  const suffix = slash === -1 ? "" : resource.slice(slash);
  if (requestedAgentId !== grant.agentId || request.method === "DELETE") {
    throw new ApiFailure(403, "agent_not_granted", "This durable agent is outside the signed Connect authorization.");
  }
  if (suffix.startsWith("/realtime/")) {
    if (!grant.capabilities.includes("chatgpt")) {
      throw new ApiFailure(403, "chatgpt_not_granted", "Connect ChatGPT before starting voice.");
    }
    if (!grant.capabilities.includes("agent.output.final")) {
      throw new ApiFailure(403, "agent_output_not_granted", "Voice requires access to final agent replies.");
    }
  }
  const target = new URL(
    `/v1/agents/${encodeURIComponent(grant.agentId)}${suffix}${new URL(request.url).search}`,
    "https://nanocodex.internal",
  );
  const headers = new Headers(managedGrantHeaders(managedGrantAssertion(grant)));
  for (const name of ["accept", "content-type", "idempotency-key", "x-nanocodex-voice-session-id"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  const upstream = await env.ACCOUNTS.fetch(new Request(target, {
    method: request.method,
    headers,
    body: request.method === "GET" || request.method === "HEAD" ? undefined : request.body,
    signal: request.signal,
  }));
  return projectManagedResponse(upstream, grant, suffix);
}

async function projectManagedResponse(
  upstream: Response,
  grant: GrantRecord,
  resource: string,
): Promise<Response> {
  const responseHeaders = new Headers();
  for (const name of ["content-type", "retry-after", "x-nanocodex-realtime-location"]) {
    const value = upstream.headers.get(name);
    if (value) responseHeaders.set(name, value);
  }
  if (!upstream.ok || !upstream.body) {
    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    });
  }
  const contentType = upstream.headers.get("content-type") ?? "";
  if (contentType.startsWith("text/event-stream")) {
    return new Response(upstream.body.pipeThrough(managedEventProjection(grant)), {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    });
  }
  if (contentType.includes("application/json")) {
    const value = await upstream.json() as unknown;
    return new Response(JSON.stringify(projectManagedJson(value, grant, resource)), {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    });
  }
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: responseHeaders,
  });
}

function projectManagedJson(value: unknown, grant: GrantRecord, resource: string): unknown {
  if (!isRecord(value)) return value;
  const history = grant.capabilities.includes("agent.history.read")
    || grant.capabilities.includes("agent.trace.read");
  if (resource === "/events/history" || resource.startsWith("/events/history?")) {
    const data = Array.isArray(value.data)
      ? history
        ? value.data.map((event) => projectManagedEvent(event, grant)).filter(Boolean)
        : []
      : [];
    return { ...value, data, has_more: history ? value.has_more : false };
  }
  const projected = projectManagedEvent(value, grant) ?? {};
  if (!history) {
    if ("active_turn_details" in projected) projected.active_turn_details = [];
    if ("input" in projected) projected.input = "";
    if ("first_prompt" in projected) projected.first_prompt = "";
    if ("completed_turns" in projected) projected.completed_turns = 0;
  }
  if (isRecord(projected.terminal)) {
    projected.terminal = projectManagedEvent(projected.terminal, grant) ?? {};
  }
  return projected;
}

function projectManagedEvent(value: unknown, grant: GrantRecord): Record<string, unknown> | undefined {
  if (!isRecord(value) || typeof value.type !== "string") return isRecord(value) ? { ...value } : undefined;
  const traces = grant.capabilities.includes("agent.trace.read");
  const actions = traces || grant.capabilities.includes("agent.output.actions");
  const history = traces || grant.capabilities.includes("agent.history.read");
  const finalMessages = traces || grant.capabilities.includes("agent.output.final");
  if (value.type === "event") {
    if (traces) return { ...value };
    if (!isRecord(value.event) || typeof value.event.type !== "string") return undefined;
    if (value.event.type === "assistant.delta" || value.event.type === "assistant.message") {
      if (!finalMessages) return undefined;
      const payload = isRecord(value.event.payload) ? value.event.payload : {};
      if (payload.phase === "commentary") return undefined;
      return {
        ...value,
        event: {
          ...value.event,
          payload: {
            ...(typeof payload.model_call_index === "number"
              ? { model_call_index: payload.model_call_index }
              : {}),
            ...(typeof payload.item_id === "string" ? { item_id: payload.item_id } : {}),
            ...(typeof payload.phase === "string" ? { phase: payload.phase } : {}),
            ...(typeof payload.text === "string" ? { text: payload.text } : {}),
          },
        },
      };
    }
    if (!actions) return undefined;
    if (value.event.type !== "tool.call" && value.event.type !== "tool.result") return undefined;
    const payload = isRecord(value.event.payload) ? value.event.payload : {};
    const safePayload = value.event.type === "tool.call"
      ? {
          ...(typeof payload.call_id === "string" ? { call_id: payload.call_id } : {}),
          ...(typeof payload.name === "string" ? { name: payload.name } : {}),
        }
      : {
          ...(typeof payload.call_id === "string" ? { call_id: payload.call_id } : {}),
          ...(typeof payload.status === "string" ? { status: payload.status } : {}),
        };
    return {
      ...value,
      event: { ...value.event, payload: safePayload },
    };
  }
  const projected = { ...value };
  if (value.type === "turn_completed" && !finalMessages) projected.final_message = "";
  if (value.type === "turn_accepted" && !history) projected.input = "";
  return projected;
}

function managedEventProjection(grant: GrantRecord): TransformStream<Uint8Array, Uint8Array> {
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let buffered = "";
  const flushFrames = (controller: TransformStreamDefaultController<Uint8Array>, final: boolean) => {
    let boundary: number;
    while ((boundary = buffered.indexOf("\n\n")) !== -1) {
      const frame = buffered.slice(0, boundary);
      buffered = buffered.slice(boundary + 2);
      const projected = projectManagedSseFrame(frame, grant);
      if (projected) controller.enqueue(encoder.encode(`${projected}\n\n`));
    }
    if (final && buffered) {
      const projected = projectManagedSseFrame(buffered, grant);
      if (projected) controller.enqueue(encoder.encode(`${projected}\n\n`));
      buffered = "";
    }
    if (buffered.length > 2_500_000) throw new Error("managed event projection frame exceeded its bound");
  };
  return new TransformStream({
    transform(chunk, controller) {
      buffered += decoder.decode(chunk, { stream: true });
      flushFrames(controller, false);
    },
    flush(controller) {
      buffered += decoder.decode();
      flushFrames(controller, true);
    },
  });
}

function projectManagedSseFrame(frame: string, grant: GrantRecord): string | undefined {
  const lines = frame.split("\n");
  const dataIndex = lines.findIndex((line) => line.startsWith("data: "));
  if (dataIndex === -1) return frame;
  let value: unknown;
  try {
    value = JSON.parse(lines[dataIndex]!.slice("data: ".length));
  } catch {
    throw new Error("managed event projection received invalid JSON");
  }
  const projected = projectManagedEvent(value, grant);
  if (!projected) return undefined;
  lines[dataIndex] = `data: ${JSON.stringify(projected)}`;
  return lines.join("\n");
}

async function handleAgentToolRoute(
  request: Request,
  env: Env,
  store: Kv.Kv,
  url: URL,
): Promise<Response | undefined> {
  const isAccountInfo = request.method === "GET" && url.pathname === "/v1/agent/account-info";
  const isEgress = request.method === "POST" && url.pathname === "/v1/egress";
  const isWeb = request.method === "POST" && url.pathname === "/api/tools/web-search";
  const isImage = request.method === "POST" && url.pathname === "/api/tools/image-generation";
  if (!isAccountInfo && !isEgress && !isWeb && !isImage) return undefined;
  const { grant } = await authenticatedGrant(request, env.CONNECT_STATE);
  requireGrantAppOrigin(request, grant);
  if (isAccountInfo) return Response.json(await connectAccountInfo(env, store, grant));
  if (isEgress) return grantBrowserEgress(request, env, grant);
  if (isWeb) return grantWebSearch(request, env, grant);
  return grantImageGeneration(request, env, grant);
}

async function connectAccountInfo(env: Env, store: Kv.Kv, current: GrantRecord) {
  const [connectorInfo, machineUsd, settlement, authorizations] = await Promise.all([
    connectorStatuses(env, current.brokerUserId),
    tokenBalance(MACHINE_USD, current.accountAddress),
    tokenBalance(USDC_E, current.accountAddress),
    accountAuthorizations(store, current),
  ]);
  return {
    ...connectorInfo,
    identity: { tempoAddress: current.accountAddress },
    stablecoins: [
      { token: MACHINE_USD, symbol: "MACHUSD", balance: machineUsd.toString(), decimals: 6 },
      { token: USDC_E, symbol: "USDC.e", balance: settlement.toString(), decimals: 6 },
    ],
    authorizations,
  };
}

async function accountAuthorizations(store: Kv.Kv, current: GrantRecord) {
  const key = grantIndexKey(current.accountAddress);
  const retained = await store.get<unknown>(key);
  const ids = new Set<string>([
    current.id,
    ...retainedGrantIds(retained),
  ]);
  ids.delete(current.id);
  const records = [
    current,
    ...await Promise.all([...ids].map((id) => store.get<GrantRecord>(`grant:${id}`))),
  ];
  const grants = records.filter((grant): grant is GrantRecord => isGrantRecord(grant)
    && grant.accountAddress.toLowerCase() === current.accountAddress.toLowerCase());
  const seen = new Set<string>();
  return grants
    .sort((left, right) => right.expiresAt - left.expiresAt)
    .map(grantAuthorization)
    .filter((authorization) => {
      const fingerprint = JSON.stringify(authorization);
      if (seen.has(fingerprint)) return false;
      seen.add(fingerprint);
      return true;
    });
}

function grantAuthorization(grant: GrantRecord) {
  const now = Math.floor(Date.now() / 1000);
  const accessKey = grant.accessKey;
  const limits = accessKey && Array.isArray(accessKey.limits) ? accessKey.limits : [];
  const scopes = accessKey && Array.isArray(accessKey.scopes) ? accessKey.scopes : [];
  return {
    appId: grant.appId,
    permission: grant.permission,
    status: grant.expiresAt <= now ? "expired" : grant.status,
    expiresAt: grant.expiresAt,
    capabilities: [...grant.capabilities],
    connectors: CONNECTOR_IDS.filter((connector) => grant.capabilities.includes(connector)),
    ...(accessKey ? { accessKey: {
      id: address(accessKey.key_id),
      expiry: safeInteger(accessKey.expiry, "access_key.expiry"),
      limits: limits.map((limit) => {
        if (!isRecord(limit) || typeof limit.limit !== "string") {
          throw new ApiFailure(500, "invalid_grant_policy", "A retained grant has an invalid spending limit.");
        }
        const token = address(limit.token);
        return {
          token,
          symbol: tokenSymbol(token),
          limit: limit.limit,
          ...(Number.isSafeInteger(limit.period) ? { period: limit.period as number } : {}),
        };
      }),
      scopes: scopes.map((scope) => {
        if (!isRecord(scope)) {
          throw new ApiFailure(500, "invalid_grant_policy", "A retained grant has an invalid call scope.");
        }
        return {
          address: address(scope.address),
          ...(typeof scope.selector === "string" ? { selector: scope.selector } : {}),
          ...(Array.isArray(scope.recipients)
            ? { recipients: scope.recipients.map(address) }
            : {}),
        };
      }),
    }, spend: {
      token: MACHINE_USD,
      symbol: "MACHUSD",
      spent: grant.spentAtomics,
      limit: MPP_LIMIT.toString(),
      period: MPP_PERIOD,
      maxPerRequest: MPP_MAX_PER_REQUEST.toString(),
    } } : { authority: "hosted" }),
  };
}

async function appendGrantIndex(
  store: Kv.Kv,
  accountAddress: `0x${string}`,
  grantId: `0x${string}`,
): Promise<void> {
  await withGrantIndexLock(store, accountAddress, async () => {
    const key = grantIndexKey(accountAddress);
    const retained = await store.get<unknown>(key);
    const ids = retainedGrantIds(retained).filter((id) => id !== grantId);
    await store.set(key, [...ids, grantId].slice(-MAX_ACCOUNT_AUTHORIZATIONS));
  });
}

function retainedGrantIds(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((id): id is string => typeof id === "string" && /^0x[0-9a-fA-F]{64}$/.test(id))
    : [];
}

function grantIndexKey(accountAddress: `0x${string}`): string {
  return `grant-index:${accountAddress.toLowerCase()}`;
}

async function grantBrowserEgress(request: Request, env: Env, grant: GrantRecord): Promise<Response> {
  if (grant.status !== "active") {
    throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  }
  if (grant.expiresAt <= Math.floor(Date.now() / 1000)) {
    throw new ApiFailure(409, "grant_expired", "The grant has expired.");
  }
  const value = await boundedJson(request, MAX_PUBLIC_EGRESS_BODY_BYTES, "browser egress");
  const targetValue = value.url;
  const threadId = value.thread_id;
  if (typeof targetValue !== "string" || typeof threadId !== "string"
    || !/^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(threadId)) {
    throw new ApiFailure(400, "invalid_egress_request", "The browser egress request is invalid.");
  }
  let target: URL;
  try { target = new URL(targetValue); } catch {
    throw new ApiFailure(400, "invalid_egress_url", "The browser egress URL is invalid.");
  }
  const connector = connectorForUrl(target);
  if (connector) {
    const result = await grantConnectorRequest(env, grant, connector, {
      path: `${target.pathname}${target.search}`,
      method: value.method,
      headers: value.headers,
      body: value.body,
    });
    return new Response(result.body, { status: result.status, headers: result.headers });
  }
  return publicBrowserEgress(target, value, request.signal);
}

function connectorForUrl(url: URL): OAuthConnectorId | undefined {
  if (url.origin === "https://api.github.com") return "github";
  if (url.origin === "https://gmail.googleapis.com") return "gmail";
  if (url.origin === "https://www.googleapis.com") return "gdrive";
  if (url.origin === "https://api.x.com") return "x";
  return undefined;
}

async function publicBrowserEgress(
  target: URL,
  value: Record<string, unknown>,
  signal: AbortSignal,
): Promise<Response> {
  let url = publicEgressUrl(target);
  let method = typeof value.method === "string" ? value.method.toUpperCase() : "GET";
  if (!CONNECTOR_METHODS.has(method)) {
    throw new ApiFailure(403, "egress_method_denied", "The browser egress method is denied.");
  }
  const headers = publicEgressHeaders(value.headers);
  const bodyValue = value.body;
  if (bodyValue !== undefined && typeof bodyValue !== "string") {
    throw new ApiFailure(400, "invalid_egress_body", "The browser egress body must be a string.");
  }
  let body: string | undefined = bodyValue;
  if (body !== undefined && (method === "GET" || method === "HEAD")) {
    throw new ApiFailure(400, "invalid_egress_body", "GET and HEAD egress requests cannot have a body.");
  }
  for (let redirects = 0; ; redirects += 1) {
    if (redirects > 5) {
      throw new ApiFailure(502, "too_many_redirects", "The public request redirected too many times.");
    }
    const outgoingHeaders = new Headers(headers);
    if (method === "GET" || method === "HEAD") {
      outgoingHeaders.delete("content-length");
      outgoingHeaders.delete("content-type");
      body = undefined;
    }
    const upstream = await fetch(url, {
      method,
      headers: outgoingHeaders,
      ...(body === undefined ? {} : { body }),
      redirect: "manual",
      signal,
    });
    if (PUBLIC_REDIRECTS.has(upstream.status)) {
      const location = upstream.headers.get("location");
      await upstream.body?.cancel();
      if (!location) throw new ApiFailure(502, "invalid_redirect", "The public request returned an invalid redirect.");
      url = publicEgressUrl(new URL(location, url));
      if (connectorForUrl(url)) {
        throw new ApiFailure(502, "redirect_to_connector_denied", "Public requests cannot redirect into a connected account.");
      }
      if (upstream.status === 303
        || ((upstream.status === 301 || upstream.status === 302) && method === "POST")) {
        method = "GET";
        body = undefined;
      }
      continue;
    }
    const responseBody = await boundedResponseBytes(upstream, publicEgressResponseLimit(url));
    return new Response(responseBody, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: projectedPublicHeaders(upstream.headers),
    });
  }
}

function publicEgressResponseLimit(url: URL): number {
  if (url.origin === "https://cdn.jsdelivr.net" && (
    url.pathname.startsWith("/pyodide/v314.0.5/full/")
    || url.pathname.startsWith("/npm/wasm-clang@0.0.1/bin/")
  )) {
    return MAX_PINNED_RUNTIME_RESPONSE_BYTES;
  }
  return MAX_PUBLIC_EGRESS_RESPONSE_BYTES;
}

function publicEgressUrl(url: URL): URL {
  if ((url.protocol !== "http:" && url.protocol !== "https:")
    || url.username || url.password || url.hash) {
    throw new ApiFailure(403, "egress_destination_denied", "The browser egress destination is denied.");
  }
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  if (!hostname || hostname === "localhost" || PRIVATE_HOST_SUFFIXES.some((suffix) => (
    hostname === suffix.slice(1) || hostname.endsWith(suffix)
  )) || deniedIpLiteral(hostname)) {
    throw new ApiFailure(403, "egress_destination_denied", "The browser egress destination is denied.");
  }
  return url;
}

function deniedIpLiteral(hostname: string): boolean {
  const ipv4 = hostname.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!ipv4) {
    if (!hostname.includes(":")) return false;
    const normalized = hostname.replace(/^\[/, "").replace(/\]$/, "").toLowerCase();
    return normalized === "::" || normalized === "::1" || normalized.startsWith("fc")
      || normalized.startsWith("fd") || normalized.startsWith("fe8")
      || normalized.startsWith("fe9") || normalized.startsWith("fea")
      || normalized.startsWith("feb") || normalized.startsWith("ff")
      || normalized.startsWith("::ffff:");
  }
  const octets = ipv4.slice(1).map(Number);
  if (octets.some((octet) => octet > 255)) return true;
  const [a, b] = octets;
  return a === 0 || a === 10 || a === 127 || a! >= 224
    || (a === 100 && b! >= 64 && b! <= 127)
    || (a === 169 && b === 254) || (a === 172 && b! >= 16 && b! <= 31)
    || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19));
}

function publicEgressHeaders(value: unknown): Headers {
  if (value === undefined) return new Headers();
  if (!isRecord(value) || Object.keys(value).length > 64) {
    throw new ApiFailure(400, "invalid_egress_headers", "Public request headers must be a bounded string map.");
  }
  const headers = new Headers();
  for (const [name, headerValue] of Object.entries(value)) {
    const lower = name.toLowerCase();
    if (typeof headerValue !== "string" || name.length > 128 || headerValue.length > 4_096
      || PRIVATE_EGRESS_HEADER.test(name) || FORBIDDEN_EGRESS_HEADERS.has(lower)
      || lower.startsWith("cf-") || lower.startsWith("forwarded")
      || lower.startsWith("sec-") || lower.startsWith("x-forwarded-")) {
      throw new ApiFailure(403, "egress_header_forbidden", "Credential and routing headers are forbidden.");
    }
    headers.set(name, headerValue);
  }
  return headers;
}

function projectedPublicHeaders(source: Headers): Headers {
  const headers = new Headers();
  for (const [name, value] of source) {
    const lower = name.toLowerCase();
    if (!PRIVATE_EGRESS_HEADER.test(name) && !BLOCKED_EGRESS_RESPONSE_HEADERS.has(lower)
      && value.length <= 16_384) {
      headers.append(name, value);
    }
  }
  return headers;
}

async function grantWebSearch(request: Request, env: Env, grant: GrantRecord): Promise<Response> {
  const value = await boundedJson(request, 64 * 1024, "web search");
  if (!isRecord(value.commands) || typeof value.session_id !== "string" || !value.session_id) {
    throw new ApiFailure(400, "invalid_web_request", "The web search request is invalid.");
  }
  return fetchGrantModelTool(env, grant, "/v1/search", {
    id: value.session_id,
    model: "gpt-5.6-sol",
    commands: value.commands,
    settings: { allowed_callers: ["direct"], external_web_access: true },
    max_output_tokens: 10_000,
  });
}

async function grantImageGeneration(request: Request, env: Env, grant: GrantRecord): Promise<Response> {
  const value = await boundedJson(request, MAX_AGENT_TOOL_BODY_BYTES, "image generation");
  const prompt = typeof value.prompt === "string" ? value.prompt.trim() : "";
  const images = Array.isArray(value.images)
    ? value.images.filter((image): image is string => typeof image === "string")
    : [];
  if (!prompt || images.length > 5 || images.some((image) => !image.startsWith("data:image/"))) {
    throw new ApiFailure(400, "invalid_image_request", "The image generation request is invalid.");
  }
  const upstream = await fetchGrantModelTool(
    env,
    grant,
    images.length ? "/v1/images/edits" : "/v1/images/generations",
    {
      ...(images.length ? { images: images.map((image_url) => ({ image_url })) } : {}),
      prompt,
      background: "auto",
      model: "gpt-image-2",
      quality: "auto",
      size: "auto",
    },
  );
  const payload = await upstream.json() as { data?: Array<{ b64_json?: unknown }>; error?: unknown };
  if (!upstream.ok) {
    throw new ApiFailure(502, "image_generation_failed", `Image generation failed with HTTP ${upstream.status}.`);
  }
  const encoded = payload.data?.[0]?.b64_json;
  if (typeof encoded !== "string" || !encoded) {
    throw new ApiFailure(502, "image_generation_failed", "Image generation returned no image.");
  }
  return Response.json({ image_url: `data:image/png;base64,${encoded}` });
}

function fetchGrantModelTool(
  env: Env,
  grant: GrantRecord,
  path: "/v1/search" | "/v1/images/generations" | "/v1/images/edits",
  body: unknown,
): Promise<Response> {
  if (grant.status !== "active" || grant.expiresAt <= Math.floor(Date.now() / 1000)
    || !grant.capabilities.includes("chatgpt") || !EGRESS_SUBJECT.test(grant.egressSubject)) {
    throw new ApiFailure(403, "chatgpt_not_granted", "The active grant does not authorize ChatGPT tools.");
  }
  return env.EGRESS.fetch(new Request(`https://nanocodex.internal${path}`, {
    method: "POST",
    headers: {
      authorization: PROVIDER_CREDENTIAL_PLACEHOLDER,
      "content-type": "application/json",
      "user-agent": "nanocodex-connect/0.1",
      "x-nanocodex-subject": grant.egressSubject,
    },
    body: JSON.stringify(body),
  }));
}

async function boundedJson(
  request: Request,
  limit: number,
  label: string,
): Promise<Record<string, unknown>> {
  const encoded = await boundedRequestText(request, limit, label);
  try { return object(JSON.parse(encoded), `${label} request`); } catch (error) {
    if (error instanceof ApiFailure) throw error;
    throw new ApiFailure(400, "invalid_json", `${label} request must be JSON.`);
  }
}

async function boundedRequestText(request: Request, limit: number, label: string): Promise<string> {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) {
    await request.body?.cancel();
    throw new ApiFailure(413, "request_too_large", `${label} request is too large.`);
  }
  if (!request.body) return "";
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      throw new ApiFailure(413, "request_too_large", `${label} request is too large.`);
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

async function boundedRequestBytes(request: Request, limit: number): Promise<ArrayBuffer> {
  const text = await boundedRequestText(request, limit, "remote MCP");
  return new TextEncoder().encode(text).buffer;
}

function tokenSymbol(token: `0x${string}`): string {
  const normalized = token.toLowerCase();
  if (normalized === MACHINE_USD.toLowerCase()) return "MACHUSD";
  if (normalized === USDC_E.toLowerCase()) return "USDC.e";
  return "TIP20";
}

async function issueModelTicket(
  store: Kv.Kv,
  grant: GrantRecord,
  body: Record<string, unknown>,
): Promise<{ ticket: string; expires_in: number }> {
  if (grant.status !== "active") throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  remainingGrantTtl(grant);
  if (!grant.capabilities.includes("chatgpt")) {
    throw new ApiFailure(403, "chatgpt_not_granted", "Connect ChatGPT before starting the agent.");
  }
  const sessionId = boundedIdentifier(body.session_id, "session_id", 128);
  const turnState = body.turn_state === undefined
    ? undefined
    : boundedIdentifier(body.turn_state, "turn_state", 512);
  const ticket = randomSubject();
  if (!store.create || !await store.create(`model-ticket:${ticket}`, {
    grantId: grant.id,
    sessionId,
    ...(turnState ? { turnState } : {}),
  } satisfies ModelTicket, { ttl: MODEL_TICKET_TTL })) {
    throw new ApiFailure(500, "model_ticket_unavailable", "The model connection could not be reserved.");
  }
  return { ticket, expires_in: MODEL_TICKET_TTL };
}

async function issueRealtimeTicket(
  store: Kv.Kv,
  grant: GrantRecord,
  body: Record<string, unknown>,
): Promise<{ ticket: string; expires_in: number }> {
  if (grant.status !== "active") throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  remainingGrantTtl(grant);
  if (!grant.capabilities.includes("chatgpt")) {
    throw new ApiFailure(403, "chatgpt_not_granted", "Connect ChatGPT before starting voice.");
  }
  if (!grant.capabilities.includes("agent.output.final")) {
    throw new ApiFailure(403, "agent_output_not_granted", "Voice requires access to final agent replies.");
  }
  const callId = realtimeCallId(body.call_id);
  const voiceSessionId = voiceSessionIdentifier(body.voice_session_id);
  const ticket = randomSubject();
  if (!store.create || !await store.create(`realtime-ticket:${ticket}`, {
    appId: grant.appId,
    appOrigin: grant.appOrigin,
    agentId: grant.agentId,
    callId,
    grantId: grant.id,
    voiceSessionId,
  } satisfies RealtimeTicket, { ttl: REALTIME_TICKET_TTL })) {
    throw new ApiFailure(500, "realtime_ticket_unavailable", "The voice connection could not be reserved.");
  }
  return { ticket, expires_in: REALTIME_TICKET_TTL };
}

async function issueToolHostTicket(
  store: Kv.Kv,
  grant: GrantRecord,
): Promise<{ ticket: string; expires_in: number }> {
  if (grant.status !== "active") throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  remainingGrantTtl(grant);
  const ticket = randomSubject();
  if (!store.create || !await store.create(`tool-host-ticket:${ticket}`, {
    appId: grant.appId,
    appOrigin: grant.appOrigin,
    agentId: grant.agentId,
    grantId: grant.id,
    mcpFingerprint: await grantMcpFingerprint(grant),
  } satisfies ToolHostTicket, { ttl: TOOL_HOST_TICKET_TTL })) {
    throw new ApiFailure(500, "tool_host_ticket_unavailable", "The tool host connection could not be reserved.");
  }
  return { ticket, expires_in: TOOL_HOST_TICKET_TTL };
}

async function openGrantToolHostWebSocket(
  request: Request,
  env: Env,
  store: Kv.Kv,
  url: URL,
  grantId: `0x${string}`,
  agentId: string,
): Promise<Response> {
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    throw new ApiFailure(426, "websocket_required", "The tool host requires a WebSocket upgrade.");
  }
  if ([...url.searchParams.keys()].some((key) => key !== "ticket")
    || url.searchParams.getAll("ticket").length !== 1) {
    throw new ApiFailure(400, "invalid_tool_host_request", "The tool host query is invalid.");
  }
  const ticketValue = boundedIdentifier(url.searchParams.get("ticket"), "ticket", 64);
  if (!store.take) {
    throw new ApiFailure(500, "tool_host_ticket_unavailable", "One-time tool host tickets are unavailable.");
  }
  const ticket = await store.take<ToolHostTicket>(`tool-host-ticket:${ticketValue}`);
  if (!isToolHostTicket(ticket)) {
    throw new ApiFailure(403, "invalid_tool_host_ticket", "The one-time tool host ticket is invalid or expired.");
  }
  const grant = await store.get<GrantRecord>(`grant:${grantId}`);
  if (!isGrantRecord(grant)
    || grant.status !== "active"
    || grant.id.toLowerCase() !== grantId.toLowerCase()
    || grant.agentId !== agentId) {
    throw new ApiFailure(403, "agent_not_granted", "The active grant does not include this durable agent.");
  }
  remainingGrantTtl(grant);
  requireGrantAppOrigin(request, grant, ticket);
  const fingerprint = await grantMcpFingerprint(grant);
  if (ticket.grantId.toLowerCase() !== grantId.toLowerCase()
    || ticket.agentId !== agentId
    || ticket.mcpFingerprint.toLowerCase() !== fingerprint.toLowerCase()) {
    throw new ApiFailure(403, "invalid_tool_host_ticket", "The one-time tool host ticket does not match this grant.");
  }

  const target = new URL(
    `/v1/agents/${encodeURIComponent(agentId)}/tool-host`,
    "https://nanocodex.internal",
  );
  const response = await env.ACCOUNTS.fetch(new Request(target, {
    headers: {
      ...managedGrantHeaders(managedGrantAssertion(grant)),
      upgrade: "websocket",
    },
  }));
  const upstream = (response as Response & { webSocket?: WorkerWebSocket }).webSocket;
  if (response.status !== 101 || !upstream) return response;

  const pair = new WebSocketPair();
  const [downstream, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  superviseGrantSocket(store, grant, server, upstream, async (current) => (
    current.id.toLowerCase() === grantId.toLowerCase()
    && current.agentId === agentId
    && current.appId === grant.appId
    && current.appOrigin === grant.appOrigin
    && (await grantMcpFingerprint(current)).toLowerCase() === fingerprint.toLowerCase()
  ));
  return new Response(null, { status: 101, webSocket: downstream } as ResponseInit);
}

async function grantMcpFingerprint(grant: Pick<GrantRecord, "mcpConnections">): Promise<`0x${string}`> {
  return digestHex(`connect-mcp:${JSON.stringify(grant.mcpConnections ?? [])}`);
}

async function openGrantRealtimeWebSocket(
  request: Request,
  env: Env,
  store: Kv.Kv,
  url: URL,
  grantId: `0x${string}`,
  agentId: string,
): Promise<Response> {
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    throw new ApiFailure(426, "websocket_required", "The voice sideband requires a WebSocket upgrade.");
  }
  const keys = [...url.searchParams.keys()];
  if (keys.some((key) => key !== "call_id" && key !== "ticket" && key !== "voice_session_id")
    || url.searchParams.getAll("call_id").length !== 1
    || url.searchParams.getAll("ticket").length !== 1
    || url.searchParams.getAll("voice_session_id").length !== 1) {
    throw new ApiFailure(400, "invalid_realtime_request", "The voice sideband query is invalid.");
  }
  const callId = realtimeCallId(url.searchParams.get("call_id"));
  const voiceSessionId = voiceSessionIdentifier(url.searchParams.get("voice_session_id"));
  const ticketValue = boundedIdentifier(url.searchParams.get("ticket"), "ticket", 64);
  const grant = await store.get<GrantRecord>(`grant:${grantId}`);
  if (!isGrantRecord(grant)
    || grant.status !== "active"
    || grant.agentId !== agentId
    || !grant.capabilities.includes("chatgpt")
    || !grant.capabilities.includes("agent.output.final")) {
    throw new ApiFailure(403, "chatgpt_not_granted", "The active grant does not include ChatGPT.");
  }
  remainingGrantTtl(grant);
  requireGrantAppOrigin(request, grant);
  if (!store.take) throw new ApiFailure(500, "realtime_ticket_unavailable", "One-time voice tickets are unavailable.");
  const ticket = await store.take<RealtimeTicket>(`realtime-ticket:${ticketValue}`);
  if (!isRealtimeTicket(ticket)
    || ticket.appId !== grant.appId
    || ticket.appOrigin !== grant.appOrigin
    || ticket.grantId.toLowerCase() !== grantId.toLowerCase()
    || ticket.agentId !== agentId
    || ticket.callId !== callId
    || ticket.voiceSessionId !== voiceSessionId) {
    throw new ApiFailure(403, "invalid_realtime_ticket", "The one-time voice ticket is invalid or expired.");
  }
  requireGrantAppOrigin(request, grant, ticket);
  const target = new URL(
    `/v1/agents/${encodeURIComponent(agentId)}/realtime/sideband?call_id=${encodeURIComponent(callId)}&voice_session_id=${encodeURIComponent(voiceSessionId)}`,
    "https://nanocodex.internal",
  );
  const response = await env.ACCOUNTS.fetch(new Request(target, {
    headers: {
      ...managedGrantHeaders(managedGrantAssertion(grant)),
      upgrade: "websocket",
    },
  }));
  const upstream = (response as Response & { webSocket?: WorkerWebSocket }).webSocket;
  if (response.status !== 101 || !upstream) return response;

  const pair = new WebSocketPair();
  const [downstream, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  superviseGrantSocket(store, grant, server, upstream);
  return new Response(null, { status: 101, webSocket: downstream } as ResponseInit);
}

async function openGrantModelWebSocket(
  request: Request,
  env: Env,
  store: Kv.Kv,
  url: URL,
  grantId: `0x${string}`,
): Promise<Response> {
  const app = requireCallerApp(request, url.searchParams.get("app_id"));
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    throw new ApiFailure(426, "websocket_required", "The model endpoint requires a WebSocket upgrade.");
  }
  const keys = [...url.searchParams.keys()];
  if (keys.some((key) => key !== "app_id" && key !== "session_id")
    || url.searchParams.getAll("app_id").length !== 1
    || url.searchParams.getAll("session_id").length !== 1) {
    throw new ApiFailure(400, "invalid_model_request", "The model connection query is invalid.");
  }
  const ticketValue = modelTicketProtocol(request);
  const sessionId = boundedIdentifier(url.searchParams.get("session_id"), "session_id", 128);
  if (!store.take) throw new ApiFailure(500, "model_ticket_unavailable", "One-time model tickets are unavailable.");
  const ticket = await store.take<ModelTicket>(`model-ticket:${ticketValue}`);
  if (!isModelTicket(ticket)
    || ticket.grantId.toLowerCase() !== grantId.toLowerCase()
    || ticket.sessionId !== sessionId) {
    throw new ApiFailure(403, "invalid_model_ticket", "The one-time model ticket is invalid or expired.");
  }
  const grant = await store.get<GrantRecord>(`grant:${grantId}`);
  if (!isGrantRecord(grant)
    || grant.appId !== app.appId
    || grant.appOrigin !== app.origin
    || grant.status !== "active"
    || !grant.capabilities.includes("chatgpt")) {
    throw new ApiFailure(403, "chatgpt_not_granted", "The active grant does not include ChatGPT.");
  }
  remainingGrantTtl(grant);
  const headers = new Headers({
    authorization: PROVIDER_CREDENTIAL_PLACEHOLDER,
    upgrade: "websocket",
    "openai-beta": "responses_websockets=2026-02-06",
    "session-id": sessionId,
    "thread-id": sessionId,
    "x-client-request-id": sessionId,
    "x-nanocodex-subject": grant.egressSubject,
    "x-openai-internal-codex-responses-lite": "true",
    "x-responsesapi-include-timing-metrics": "true",
    "user-agent": "nanocodex-connect/0.1",
  });
  if (ticket.turnState) headers.set("x-codex-turn-state", ticket.turnState);
  const response = await env.EGRESS.fetch(new Request("https://nanocodex.internal/v1/responses", {
    method: "GET",
    headers,
  }));
  const upstream = (response as Response & { webSocket?: WorkerWebSocket }).webSocket;
  if (response.status !== 101 || !upstream) return response;

  const pair = new WebSocketPair();
  const [downstream, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  superviseGrantSocket(store, grant, server, upstream);
  return new Response(null, {
    headers: { "sec-websocket-protocol": MODEL_PROTOCOL },
    status: 101,
    webSocket: downstream,
  } as ResponseInit);
}

function modelTicketProtocol(request: Request): string {
  const protocols = (request.headers.get("sec-websocket-protocol") ?? "")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  const ticketProtocols = protocols.filter((value) => value.startsWith(MODEL_TICKET_PROTOCOL_PREFIX));
  if (protocols.length !== 2 || protocols[0] !== MODEL_PROTOCOL || ticketProtocols.length !== 1) {
    throw new ApiFailure(400, "invalid_model_request", "The model connection protocols are invalid.");
  }
  return boundedIdentifier(
    ticketProtocols[0].slice(MODEL_TICKET_PROTOCOL_PREFIX.length),
    "ticket",
    64,
  );
}

function superviseGrantSocket(
  store: Kv.Kv,
  grant: GrantRecord,
  downstream: WebSocket,
  upstream: WebSocket,
  authorized: (current: GrantRecord) => boolean | Promise<boolean> = (current) => (
    current.egressSubject === grant.egressSubject
    && current.capabilities.includes("chatgpt")
  ),
): void {
  let closed = false;
  let authorizationTimer: ReturnType<typeof setTimeout> | undefined;
  const close = (code: number, reason: string) => {
    if (closed) return;
    closed = true;
    if (authorizationTimer !== undefined) clearTimeout(authorizationTimer);
    closeSocket(downstream, code, reason);
    closeSocket(upstream, code, reason);
  };
  const forward = (target: WebSocket) => (event: MessageEvent) => {
    if (closed || target.readyState !== WebSocket.OPEN) return;
    try { target.send(event.data); } catch { close(1011, "Connect socket forwarding failed"); }
  };
  downstream.addEventListener("message", forward(upstream));
  upstream.addEventListener("message", forward(downstream));
  downstream.addEventListener("close", () => close(1000, "Connect client closed"));
  upstream.addEventListener("close", () => close(1000, "ChatGPT upstream closed"));
  downstream.addEventListener("error", () => close(1011, "Connect client socket failed"));
  upstream.addEventListener("error", () => close(1011, "ChatGPT upstream failed"));

  const reauthorize = async () => {
    if (closed) return;
    try {
      const current = await store.get<GrantRecord>(`grant:${grant.id}`);
      const active = isGrantRecord(current)
        && current.status === "active"
        && current.expiresAt > Math.floor(Date.now() / 1000)
        && await authorized(current);
      if (!active) {
        close(1008, "Nanocodex Connect grant inactive");
        return;
      }
      const untilExpiry = current.expiresAt * 1000 - Date.now();
      authorizationTimer = setTimeout(
        () => { void reauthorize(); },
        Math.max(0, Math.min(5_000, untilExpiry)),
      );
    } catch {
      close(1011, "Nanocodex Connect grant check failed");
    }
  };
  void reauthorize();
}

function closeSocket(socket: WebSocket, code: number, reason: string): void {
  if (socket.readyState !== WebSocket.CONNECTING && socket.readyState !== WebSocket.OPEN) return;
  try { socket.close(code, reason.slice(0, 120)); } catch { /* Socket already failed. */ }
}

function isModelTicket(value: unknown): value is ModelTicket {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.grantId))
    && typeof value.sessionId === "string"
    && value.sessionId.length > 0
    && (value.turnState === undefined || typeof value.turnState === "string");
}

function isRealtimeTicket(value: unknown): value is RealtimeTicket {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.grantId))
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && typeof value.agentId === "string"
    && value.agentId.length > 0
    && typeof value.callId === "string"
    && validRealtimeCallId(value.callId)
    && typeof value.voiceSessionId === "string"
    && isVoiceSessionIdentifier(value.voiceSessionId);
}

function isToolHostTicket(value: unknown): value is ToolHostTicket {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.grantId))
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.mcpFingerprint))
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && isConnectAgentId(value.agentId);
}

function realtimeCallId(value: unknown): string {
  if (typeof value !== "string" || !validRealtimeCallId(value)) {
    throw new ApiFailure(400, "invalid_realtime_call", "The voice call identifier is invalid.");
  }
  return value;
}

function validRealtimeCallId(value: string): boolean {
  return /^rtc_[A-Za-z0-9._:-]{1,196}$/.test(value)
    || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}

function voiceSessionIdentifier(value: unknown): string {
  if (typeof value !== "string" || !isVoiceSessionIdentifier(value)) {
    throw new ApiFailure(400, "invalid_voice_session", "The voice session identifier is invalid.");
  }
  return value;
}

function isVoiceSessionIdentifier(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);
}

async function revokeGrant(
  env: Env,
  store: Kv.Kv,
  grant: GrantRecord,
  token: string,
): Promise<GrantRecord> {
  if (grant.status !== "active") {
    await store.delete(`grant-token:${token}`);
    return grant;
  }
  if (!EGRESS_SUBJECT.test(grant.egressSubject)) {
    throw new ApiFailure(403, "invalid_grant_binding", "The grant's broker binding is invalid.");
  }
  const ttl = remainingGrantTtl(grant);
  if (grant.sharedEgressSubject !== true) {
    await unbindSubject(env, grant.egressSubject, grant.brokerUserId);
  }
  const revoked = { ...grant, status: "revoked" as const };
  await store.set(`grant:${grant.id}`, revoked, { ttl });
  await store.delete(`grant-token:${token}`);
  return revoked;
}

async function withGrantMutationLock<value>(
  store: Kv.Kv,
  grantId: `0x${string}`,
  operation: () => Promise<value>,
): Promise<value> {
  if (!store.create) {
    throw new ApiFailure(500, "grant_lock_unavailable", "Atomic grant mutation is unavailable.");
  }
  const lockKey = `grant-lock:${grantId}`;
  const lockValue = randomSubject();
  let acquired = false;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    acquired = await store.create(lockKey, lockValue, { ttl: 60 });
    if (acquired) break;
    await new Promise((resolve) => setTimeout(resolve, 25 * (attempt + 1)));
  }
  if (!acquired) {
    throw new ApiFailure(409, "grant_busy", "Another grant operation is still in progress.");
  }
  try {
    return await operation();
  } finally {
    // The bounded operation completes well inside the lock TTL. Deleting here
    // cannot remove a successor's lock because no successor can acquire before
    // this token's entry expires or is deleted.
    await store.delete(lockKey);
  }
}

async function withGrantIndexLock<value>(
  store: Kv.Kv,
  accountAddress: `0x${string}`,
  operation: () => Promise<value>,
): Promise<value> {
  if (!store.create) {
    throw new ApiFailure(500, "grant_index_lock_unavailable", "Atomic authorization indexing is unavailable.");
  }
  const lockKey = `grant-index-lock:${accountAddress.toLowerCase()}`;
  const lockValue = randomSubject();
  let acquired = false;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    acquired = await store.create(lockKey, lockValue, { ttl: 60 });
    if (acquired) break;
    await new Promise((resolve) => setTimeout(resolve, 25 * (attempt + 1)));
  }
  if (!acquired) {
    throw new ApiFailure(409, "grant_index_busy", "Another authorization is being indexed.");
  }
  try {
    return await operation();
  } finally {
    await store.delete(lockKey);
  }
}

async function authenticatedGrant(
  request: Request,
  namespace: Kv.durableObject.Namespace,
  requestedGrantId?: `0x${string}`,
): Promise<{ grant: GrantRecord; principal: GrantPrincipal; token: string }> {
  const token = grantBearerToken(request);
  const stub = namespace.get(namespace.idFromName("default"));
  const resolved = await stub.fetch(
    `https://do.invalid/resolve-grant?token=${encodeURIComponent(token)}`,
    { method: "POST" },
  );
  if (!resolved.ok) {
    throw new ApiFailure(500, "grant_state_unavailable", "The grant session could not be resolved.");
  }
  const value = await resolved.json() as { principal?: unknown; grant?: unknown };
  const principal = value.principal;
  const app = requireCallerApp(request);
  if (!isGrantPrincipal(principal)
    || principal.appId !== app.appId
    || principal.appOrigin !== app.origin
    || (requestedGrantId && principal.grantId.toLowerCase() !== requestedGrantId.toLowerCase())) {
    throw new ApiFailure(401, "invalid_grant_token", "The grant session is invalid.");
  }
  const grant = value.grant;
  if (!isGrantRecord(grant)
    || grant.id.toLowerCase() !== principal.grantId.toLowerCase()
    || grant.appId !== principal.appId
    || grant.appOrigin !== principal.appOrigin
    || grant.externalPrincipalId !== principal.externalPrincipalId
    || grant.accountAddress.toLowerCase() !== principal.accountAddress.toLowerCase()) {
    throw new ApiFailure(401, "invalid_grant_token", "The grant session is not bound to this grant, app, and account.");
  }
  return { grant, principal, token };
}

function grantBearerToken(request: Request): string {
  const authorization = request.headers.get("authorization");
  const match = authorization?.match(/^Bearer ([A-Za-z0-9_-]{43})$/i);
  if (!match) throw new ApiFailure(401, "grant_token_required", "A grant-scoped bearer token is required.");
  return match[1]!;
}

function isGrantPrincipal(value: unknown): value is GrantPrincipal {
  return isRecord(value)
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.grantId))
    && (value.externalPrincipalId === undefined
      || /^[A-Za-z0-9_-]{43}$/.test(String(value.externalPrincipalId)));
}

function isGrantRecord(value: unknown): value is GrantRecord {
  return isRecord(value)
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && /^0x[0-9a-fA-F]{64}$/.test(String(value.id))
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && isBrokerUserId(value.brokerUserId)
    && typeof value.agentId === "string"
    && typeof value.permission === "string"
    && (value.status === "active" || value.status === "revoked")
    && Number.isSafeInteger(value.expiresAt)
    && Array.isArray(value.capabilities)
    && value.capabilities.every((capability) => typeof capability === "string")
    && (value.mcpConnections === undefined
      || (Array.isArray(value.mcpConnections)
        && value.mcpConnections.length <= 16
        && value.mcpConnections.every((connection) => isRecord(connection)
          && isMcpConnectionId(connection.id)
          && typeof connection.name === "string"
          && connection.name.length > 0
          && connection.name.length <= 256)))
    && (value.accessKey === undefined || isRecord(value.accessKey))
    && (value.balanceAtomics === undefined || /^\d+$/.test(String(value.balanceAtomics)))
    && typeof value.spentAtomics === "string"
    && typeof value.egressSubject === "string"
    && (value.externalPrincipalId === undefined
      || /^[A-Za-z0-9_-]{43}$/.test(String(value.externalPrincipalId)))
    && (value.settlementBalanceAtomics === undefined || /^\d+$/.test(String(value.settlementBalanceAtomics)))
    && (value.sharedEgressSubject === undefined || typeof value.sharedEgressSubject === "boolean");
}

function isBrokerUserId(value: unknown): value is string {
  return typeof value === "string" && (
    /^0x[0-9a-fA-F]{40}$/.test(value)
    || /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)
  );
}

function isAccessKeyRecord(value: unknown): value is AccessKeyRecord {
  return isRecord(value)
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && isRecord(value.accessKey);
}

function accessKeyStorageKey(accountAddress: `0x${string}`, keyId: unknown): string {
  return `access-key:${accountAddress.toLowerCase()}:${address(keyId).toLowerCase()}`;
}

function remainingGrantTtl(grant: GrantRecord): number {
  const ttl = grant.expiresAt - Math.floor(Date.now() / 1000);
  if (ttl <= 0) throw new ApiFailure(409, "grant_expired", "The grant has expired.");
  return ttl;
}

async function chargeGrant(
  store: Kv.Kv,
  grant: GrantRecord,
  grantToken: string,
  body: Record<string, unknown>,
) {
  if (grant.status !== "active") throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  if (!grant.capabilities.includes("mpp.machusd") || !grant.accessKey) {
    throw new ApiFailure(403, "mpp_not_granted", "This connection has no MPP authority.");
  }
  const ttl = remainingGrantTtl(grant);
  if (typeof body.amount_atomics !== "string" || !/^[1-9][0-9]*$/.test(body.amount_atomics)) {
    throw new ApiFailure(400, "invalid_mpp_amount", "MPP amount_atomics must be a positive integer string.");
  }
  const amount = BigInt(body.amount_atomics);
  if (amount > MPP_MAX_PER_REQUEST) {
    throw new ApiFailure(403, "mpp_request_limit_exceeded", "This payment exceeds the per-request permission.");
  }
  const spent = BigInt(grant.spentAtomics);
  if (spent + amount > MPP_LIMIT) {
    throw new ApiFailure(403, "mpp_period_limit_exceeded", "This payment exceeds the daily MPP permission.");
  }
  const availableBalance = await tokenBalance(MACHINE_USD, grant.accountAddress);
  if (amount > availableBalance) {
    throw new ApiFailure(402, "machine_usd_required", "Add machineUSD before paying for this capability.");
  }
  const origin = requiredOrigin(body.origin, "origin");
  const updated = {
    ...grant,
    balanceAtomics: (availableBalance - amount).toString(),
    spentAtomics: (spent + amount).toString(),
  };
  await store.set(`grant:${grant.id}`, updated, { ttl });
  const receiptSeed = `${grant.id}:${updated.spentAtomics}:${origin}:${randomSubject()}`;
  return {
    receipt: {
      id: `mpp_${(await digestHex(receiptSeed)).slice(2, 18)}`,
      amount_atomics: amount.toString(),
      origin,
      transaction_hash: await digestHex(`transaction:${receiptSeed}`),
    },
    connection: connectionWire(updated, grantToken),
  };
}

async function connectorStatuses(
  env: Env,
  brokerUserId: string,
): Promise<{ connectors: Record<ConnectorId, ConnectorStatus> }> {
  const [connectorValue, credentialValue] = await Promise.all([
    brokerJson(env, `/users/${encodeURIComponent(brokerUserId)}/connectors`),
    brokerJson(env, `/users/${encodeURIComponent(brokerUserId)}/credentials`),
  ]);
  const statuses = isRecord(connectorValue.connectors) ? connectorValue.connectors : {};
  const chatGpt = isRecord(credentialValue.chatgpt) ? credentialValue.chatgpt : {};
  return {
    connectors: {
      github: connectorStatus(statuses.github),
      gmail: connectorStatus(statuses.gmail),
      gdrive: connectorStatus(statuses.gdrive),
      x: connectorStatus(statuses.x),
      chatgpt: connectorStatus(chatGpt),
    },
  };
}

async function mcpConnectionStatuses(env: Env, brokerUserId: string): Promise<McpConnection[]> {
  const value = await brokerJson(env, `/users/${encodeURIComponent(brokerUserId)}/mcp-connections`);
  if (!Array.isArray(value.mcp_connections)) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned an invalid connection list.");
  }
  if (value.mcp_connections.length > 64) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned too many connections.");
  }
  return value.mcp_connections.map(publicMcpConnection);
}

function publicMcpConnection(value: unknown): McpConnection {
  if (!isRecord(value)
    || !isMcpConnectionId(value.id)
    || typeof value.name !== "string" || value.name.length < 1 || value.name.length > 256
    || !["authorization_required", "connected", "reauthorization_required", "disabled", "revoked"].includes(String(value.status))) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned invalid connection metadata.");
  }
  return {
    id: value.id,
    name: value.name,
    status: value.status as McpConnectionStatus,
  };
}

function isMcpConnection(value: unknown): value is McpConnection {
  return isRecord(value)
    && isMcpConnectionId(value.id)
    && typeof value.name === "string" && value.name.length > 0 && value.name.length <= 256
    && ["authorization_required", "connected", "reauthorization_required", "disabled", "revoked"].includes(String(value.status));
}

async function connectedRequestedMcpConnections(
  env: Env,
  brokerUserId: string,
  requested: readonly string[],
): Promise<McpConnection[]> {
  if (requested.length === 0) return [];
  const byId = new Map((await mcpConnectionStatuses(env, brokerUserId)).map((connection) => [connection.id, connection]));
  return requested.map((id) => {
    const connection = byId.get(id);
    if (!connection || connection.status !== "connected") {
      throw new ApiFailure(403, "mcp_not_connected", "Every requested remote MCP must be connected before creating a grant.");
    }
    return connection;
  });
}

function connectorStatus(value: unknown): ConnectorStatus {
  if (!isRecord(value) || value.connected !== true) return { connected: false };
  const label = boundedOptionalString(value.label, 256);
  const accountId = boundedOptionalString(value.account_id, 256);
  return {
    connected: true,
    ...(label ? { label } : {}),
    ...(accountId ? { account_id: accountId } : {}),
  };
}

async function startConnector(
  env: Env,
  store: Kv.Kv,
  request: Request,
  accountAddress: `0x${string}`,
  brokerUserId: string,
  connector: ConnectorId,
): Promise<Response> {
  if (connector === "chatgpt") {
    return startChatGptConnector(env, brokerUserId);
  }

  const requestOrigin = connectApiRequestOrigin(request);
  const dialogOrigin = requiredDialogOrigin(request);
  const local = localConnectorAuthorization(requestOrigin, connector, "connect");
  const requestBody = request.headers.get("x-nanocodex-connect-client") === "device"
    ? await boundedJson(request, 4 * 1024, "connector authorization")
    : undefined;
  const deviceReturn = requestBody
    ? deviceMcpReturn(requestBody.return_to, dialogOrigin)
    : undefined;
  const started = await brokerJson(env, `/users/${encodeURIComponent(brokerUserId)}/connectors/${connector}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uri: local?.redirectUri ?? `${NANOCODEX_ORIGIN}/v1/connectors/${connector}/callback`,
      return_to: "/",
    }),
  });
  const authorizationUrl = connectorAuthorizationUrl(started.authorization_url, connector);
  const state = authorizationUrl.searchParams.get("state");
  if (!state || state.length > 512) {
    throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid authorization state.");
  }
  const callbackState = local ? state : scopedConnectConnectorState(state);
  if (callbackState !== state) authorizationUrl.searchParams.set("state", callbackState);
  if (local) {
    try {
      await wrapLocalConnectorAuthorizationState(
        authorizationUrl,
        local,
        env.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY ?? "",
      );
    } catch {
      throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid authorization state.");
    }
  }
  if (!store.create) {
    throw new ApiFailure(500, "connector_state_unavailable", "Atomic connector state storage is unavailable.");
  }
  const created = await store.create(`connector-state:${callbackState}`, {
    accountAddress,
    brokerUserId,
    dialogOrigin,
    provider: connector,
    ...(deviceReturn ? { returnTo: deviceReturn } : {}),
  } satisfies ConnectorState, { ttl: CONNECTOR_STATE_TTL });
  if (!created) {
    throw new ApiFailure(502, "connector_state_conflict", "The connector authorization state could not be reserved.");
  }
  return Response.json({ authorization_url: authorizationUrl.href });
}

async function startMcpConnection(
  env: Env,
  store: Kv.Kv,
  request: Request,
  accountAddress: `0x${string}`,
  brokerUserId: string,
  connectionId: string,
): Promise<Response> {
  const existing = (await mcpConnectionStatuses(env, brokerUserId))
    .find((connection) => connection.id === connectionId);
  if (!existing || existing.status === "revoked") {
    throw new ApiFailure(404, "mcp_connection_not_found", "The remote MCP connection is unavailable.");
  }
  if (existing.status === "connected") {
    return Response.json({ mcp_connection: existing });
  }
  const requestOrigin = connectApiRequestOrigin(request);
  const dialogOrigin = requiredDialogOrigin(request);
  const requestBody = await boundedJson(request, 4 * 1024, "remote MCP authorization");
  const deviceReturn = request.headers.get("x-nanocodex-connect-client") === "device"
    ? deviceMcpReturn(requestBody.return_to, dialogOrigin)
    : undefined;
  const local = localMcpAuthorization(requestOrigin, connectionId, "connect");
  const started = await brokerJson(
    env,
    `/users/${encodeURIComponent(brokerUserId)}/mcp-connections/${connectionId}/start`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        redirect_uri: local?.redirectUri ?? `${NANOCODEX_ORIGIN}/v1/mcp-connections/${connectionId}/callback`,
        return_to: "/",
      }),
    },
  );
  const authorizationUrl = remoteMcpAuthorizationUrl(started.authorization_url);
  const state = authorizationUrl.searchParams.get("state");
  if (!state || state.length > 512 || !store.create) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned invalid authorization state.");
  }
  if (local) {
    try {
      await wrapLocalMcpAuthorizationState(
        authorizationUrl,
        local,
        env.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY ?? "",
      );
    } catch {
      throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned invalid authorization state.");
    }
  }
  if (!await store.create(`mcp-connection-state:${state}`, {
    accountAddress,
    brokerUserId,
    connectionId,
    dialogOrigin,
    ...(deviceReturn ? { returnTo: deviceReturn } : {}),
  } satisfies McpConnectionState, { ttl: CONNECTOR_STATE_TTL })) {
    throw new ApiFailure(502, "mcp_state_conflict", "The remote MCP authorization state could not be reserved.");
  }
  return Response.json({
    authorization_url: authorizationUrl.href,
    mcp_connection: existing,
  });
}

async function disconnectMcpConnection(env: Env, brokerUserId: string, connectionId: string): Promise<void> {
  const response = await brokerFetch(
    env,
    `/users/${encodeURIComponent(brokerUserId)}/mcp-connections/${connectionId}`,
    { method: "DELETE" },
  );
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(502, "mcp_broker_failed", "The remote MCP broker could not revoke the connection.");
  }
  await response.body?.cancel();
}

function remoteMcpAuthorizationUrl(value: unknown): URL {
  if (typeof value !== "string" || value.length > 8_192) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned an invalid authorization URL.");
  }
  let url: URL;
  try { url = new URL(value); } catch {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned an invalid authorization URL.");
  }
  if (url.protocol !== "https:" || url.username || url.password || url.hash) {
    throw new ApiFailure(502, "mcp_broker_invalid", "The remote MCP broker returned an unsafe authorization URL.");
  }
  return url;
}

async function startChatGptConnector(env: Env, brokerUserId: string): Promise<Response> {
  const user = encodeURIComponent(brokerUserId);
  const claim = await brokerFetch(env, `/users/${user}/credentials/chatgpt/local-claim`, {
    method: "POST",
  });
  if (claim.status !== 404) {
    const text = await boundedResponseText(claim, MAX_BROKER_BODY_BYTES);
    if (!claim.ok) {
      throw new ApiFailure(
        claim.status === 503 ? 503 : 502,
        "local_chatgpt_claim_failed",
        "No usable local ChatGPT login was found. Sign in locally, restart the development stack, and try again.",
      );
    }
    let value: Record<string, unknown>;
    try { value = object(JSON.parse(text), "local ChatGPT claim response"); } catch {
      throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid response.");
    }
    const chatGpt = connectorStatus(value.chatgpt);
    if (!chatGpt.connected) {
      throw new ApiFailure(502, "connector_broker_invalid", "The connector broker did not retain the local ChatGPT login.");
    }
    return Response.json({
      state: "authenticated",
      connected: true,
      ...(chatGpt.account_id ? { account_id: chatGpt.account_id } : {}),
    });
  }
  await claim.body?.cancel();
  return Response.json(publicChatGptLogin(await brokerJson(
    env,
    `/users/${user}/credentials/chatgpt/login`,
    { method: "POST" },
  )));
}

function connectorAuthorizationUrl(value: unknown, connector: OAuthConnectorId): URL {
  if (typeof value !== "string" || value.length > 8_192) {
    throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid authorization URL.");
  }
  let url: URL;
  try { url = new URL(value); } catch {
    throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid authorization URL.");
  }
  const expected = connector === "github"
    ? ["https://github.com", "/login/oauth/authorize"]
    : connector === "x"
      ? ["https://x.com", "/i/oauth2/authorize"]
      : ["https://accounts.google.com", "/o/oauth2/v2/auth"];
  if (url.origin !== expected[0] || url.pathname !== expected[1] || url.username || url.password || url.hash) {
    throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid authorization URL.");
  }
  return url;
}

async function disconnectConnector(
  env: Env,
  brokerUserId: string,
  connector: ConnectorId,
): Promise<void> {
  const path = connector === "chatgpt"
    ? `/users/${encodeURIComponent(brokerUserId)}/credentials/chatgpt`
    : `/users/${encodeURIComponent(brokerUserId)}/connectors/${connector}`;
  const response = await brokerFetch(env, path, { method: "DELETE" });
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(502, "connector_broker_failed", "The connector broker could not disconnect the account.");
  }
  await response.body?.cancel();
}

async function pollChatGpt(env: Env, brokerUserId: string): Promise<Response> {
  const status = publicChatGptLogin(await brokerJson(
    env,
    `/users/${encodeURIComponent(brokerUserId)}/credentials/chatgpt/login/status`,
    { method: "POST" },
  ));
  if (status.state === "authenticated") return Response.json({ ...status, connected: true });
  if (status.state === "pending") return Response.json({ ...status, connected: false }, { status: 202 });
  return Response.json({ ...status, connected: false }, { status: 409 });
}

function publicChatGptLogin(value: Record<string, unknown>): Record<string, unknown> {
  const state = typeof value.state === "string"
    && ["pending", "authenticated", "not_started", "expired"].includes(value.state)
    ? value.state
    : undefined;
  if (!state) {
    throw new ApiFailure(502, "connector_broker_invalid", "The credential broker returned an invalid login status.");
  }
  return {
    state,
    ...(boundedOptionalString(value.verification_url, 2_048)
      ? { verification_url: boundedOptionalString(value.verification_url, 2_048) }
      : {}),
    ...(boundedOptionalString(value.user_code, 256)
      ? { user_code: boundedOptionalString(value.user_code, 256) }
      : {}),
    ...(Number.isSafeInteger(value.expires_at) ? { expires_at: value.expires_at } : {}),
    ...(Number.isSafeInteger(value.poll_after_ms) ? { poll_after_ms: value.poll_after_ms } : {}),
    ...(boundedOptionalString(value.account_id, 256)
      ? { account_id: boundedOptionalString(value.account_id, 256) }
      : {}),
  };
}

async function completeConnectorCallback(
  env: Env,
  store: Kv.Kv,
  url: URL,
  provider: OAuthConnectorId,
): Promise<Response> {
  const state = url.searchParams.get("state");
  const fallbackOrigin = connectDialogOrigin(url);
  if (!state || state.length > 512) return connectorCompletionPage(provider, 400, fallbackOrigin, "invalid_state");
  if (!store.take) return connectorCompletionPage(provider, 500, fallbackOrigin, "state_unavailable");
  const correlation = await store.take<ConnectorState>(`connector-state:${state}`);
  if (!isConnectorState(correlation) || correlation.provider !== provider) {
    return connectorCompletionPage(provider, 400, fallbackOrigin, "invalid_state");
  }

  const callback: Record<string, string | null> = {};
  for (const name of ["code", "state", "error", "error_description"] as const) {
    const value = url.searchParams.get(name);
    if (value !== null && value.length > 4_096) {
      logConnectorCallback(correlation, provider, "failure", "invalid_callback", env.DEPLOYMENT_SHA);
      return correlation.returnTo
        ? connectorCompletionRedirect(provider, correlation.returnTo, "failed", "invalid_callback")
        : connectorCompletionPage(provider, 400, correlation.dialogOrigin, "invalid_callback");
    }
    callback[name] = value;
  }
  callback.state = unscopedConnectConnectorState(state) ?? state;
  let response: Response;
  try {
    response = await brokerFetch(
      env,
      `/users/${encodeURIComponent(correlation.brokerUserId)}/connectors/${provider}/callback`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(callback),
      },
    );
  } catch {
    logConnectorCallback(correlation, provider, "failure", "connector_broker_unavailable", env.DEPLOYMENT_SHA);
    return correlation.returnTo
      ? connectorCompletionRedirect(provider, correlation.returnTo, "failed", "connector_broker_unavailable")
      : connectorCompletionPage(provider, 502, correlation.dialogOrigin, "connector_broker_unavailable");
  }
  let text: string;
  try {
    text = await boundedResponseText(response, MAX_BROKER_BODY_BYTES);
  } catch {
    logConnectorCallback(correlation, provider, "failure", "connector_broker_invalid", env.DEPLOYMENT_SHA);
    return correlation.returnTo
      ? connectorCompletionRedirect(provider, correlation.returnTo, "failed", "connector_broker_invalid")
      : connectorCompletionPage(provider, 502, correlation.dialogOrigin, "connector_broker_invalid");
  }
  let result: "connected" | "cancelled" | "failed" = "failed";
  if (response.ok) {
    try {
      const completed = object(JSON.parse(text), "connector callback response");
      result = completed.connected === true
        ? "connected"
        : completed.connected === false ? "cancelled" : "failed";
    } catch {
      result = "failed";
    }
  }
  logConnectorCallback(
    correlation,
    provider,
    result === "connected" ? "success" : result === "cancelled" ? "cancelled" : "failure",
    result === "failed" ? "connector_broker_failed" : result,
    env.DEPLOYMENT_SHA,
  );
  if (correlation.returnTo) {
    return connectorCompletionRedirect(
      provider,
      correlation.returnTo,
      result,
      result === "failed" ? "connector_broker_failed" : undefined,
    );
  }
  return connectorCompletionPage(
    provider,
    result === "failed" ? 502 : 200,
    correlation.dialogOrigin,
    result === "connected" ? undefined : result === "cancelled" ? "connector_cancelled" : "connector_broker_failed",
  );
}

async function completeMcpConnectionCallback(
  env: Env,
  store: Kv.Kv,
  url: URL,
  connectionId: string,
): Promise<Response> {
  const state = url.searchParams.get("state");
  const fallbackOrigin = connectDialogOrigin(url);
  if (!state || state.length > 512 || !store.take) {
    return mcpCompletionPage(connectionId, 400, fallbackOrigin, "invalid_state");
  }
  const correlation = await store.take<McpConnectionState>(`mcp-connection-state:${state}`);
  if (!isMcpConnectionState(correlation) || correlation.connectionId !== connectionId) {
    return mcpCompletionPage(connectionId, 400, fallbackOrigin, "invalid_state");
  }
  const callback: Record<string, string | null> = {};
  for (const name of ["code", "state", "error", "error_description"] as const) {
    const value = url.searchParams.get(name);
    if (value !== null && value.length > 4_096) {
      logMcpCallback(correlation, "failure", "invalid_callback", env.DEPLOYMENT_SHA);
      return mcpCompletionResult(correlation, "failed", "invalid_callback");
    }
    callback[name] = value;
  }
  let result: "connected" | "cancelled" | "failed" = "failed";
  try {
    const response = await brokerFetch(
      env,
      `/users/${encodeURIComponent(correlation.brokerUserId)}/mcp-connections/${connectionId}/callback`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(callback),
      },
    );
    const text = await boundedResponseText(response, MAX_BROKER_BODY_BYTES);
    if (response.ok) {
      const completed = object(JSON.parse(text), "remote MCP callback response");
      const listed = Array.isArray(completed.mcp_connections) && completed.mcp_connections.length === 1
        ? completed.mcp_connections[0]
        : undefined;
      const connection = publicMcpConnection(completed.mcp_connection ?? completed.connection ?? listed);
      if (connection.id !== connectionId) {
        throw new Error("The remote MCP broker returned another connection.");
      }
      result = connection.status === "connected" ? "connected" : "failed";
    } else if (response.status === 400 && callback.error) {
      result = "cancelled";
    }
  } catch {
    result = "failed";
  }
  logMcpCallback(
    correlation,
    result === "connected" ? "success" : result === "cancelled" ? "cancelled" : "failure",
    result === "failed" ? "mcp_broker_failed" : result,
    env.DEPLOYMENT_SHA,
  );
  return mcpCompletionResult(
    correlation,
    result,
    result === "failed" ? "mcp_broker_failed" : undefined,
  );
}

function logConnectorCallback(
  correlation: ConnectorState,
  connector: OAuthConnectorId,
  outcome: "success" | "cancelled" | "failure",
  status: string,
  deploymentSha: string | undefined,
): void {
  const event = {
    type: "connect.connector.callback",
    outcome,
    user_id: correlation.brokerUserId,
    account_id: correlation.accountAddress,
    connector,
    ...(deploymentSha === undefined ? {} : { deployment_sha: deploymentSha }),
    status,
  };
  if (outcome === "success") console.info(event);
  else console.warn(event);
}

function logMcpCallback(
  correlation: McpConnectionState,
  outcome: "success" | "cancelled" | "failure",
  status: string,
  deploymentSha: string | undefined,
): void {
  const event = {
    type: "connect.mcp.callback",
    outcome,
    user_id: correlation.brokerUserId,
    account_id: correlation.accountAddress,
    mcp_connection_id: correlation.connectionId,
    ...(deploymentSha === undefined ? {} : { deployment_sha: deploymentSha }),
    status,
  };
  if (outcome === "success") console.info(event);
  else console.warn(event);
}

function mcpCompletionResult(
  correlation: McpConnectionState,
  result: "connected" | "cancelled" | "failed",
  failure?: string,
): Response {
  if (correlation.returnTo) {
    const destination = new URL(correlation.returnTo);
    destination.searchParams.set("mcp_connection", correlation.connectionId);
    destination.searchParams.set("mcp_result", result);
    if (failure) destination.searchParams.set("error", failure);
    return new Response(null, {
      status: 303,
      headers: {
        "cache-control": "no-store",
        location: destination.href,
        "referrer-policy": "no-referrer",
        "x-content-type-options": "nosniff",
      },
    });
  }
  return mcpCompletionPage(
    correlation.connectionId,
    result === "failed" ? 502 : 200,
    correlation.dialogOrigin,
    result === "connected" ? undefined : failure ?? "mcp_authorization_cancelled",
  );
}

function mcpCompletionPage(connectionId: string, status: number, targetOrigin: string, failure?: string): Response {
  const completion = JSON.stringify({
    type: "nanocodex:mcp-connection-complete",
    connection_id: connectionId,
    result: failure ? "error" : "success",
    ...(failure ? { error: failure, message: "The remote MCP authorization did not complete." } : {}),
  });
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Nanocodex MCP connection</title></head><body><p>Connection flow complete. You can close this window.</p><script>window.opener?.postMessage(${completion},${JSON.stringify(targetOrigin)});window.close();</script></body></html>`;
  return new Response(html, {
    status,
    headers: {
      "content-security-policy": "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      "content-type": "text/html; charset=utf-8",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff",
    },
  });
}

function connectorCompletionRedirect(
  provider: OAuthConnectorId,
  returnTo: string,
  result: "connected" | "cancelled" | "failed",
  failure?: string,
): Response {
  const destination = new URL(returnTo);
  destination.searchParams.set("connector", provider);
  destination.searchParams.set("connector_result", result);
  if (failure) destination.searchParams.set("error", failure);
  return new Response(null, {
    status: 303,
    headers: {
      "cache-control": "no-store",
      location: destination.href,
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff",
    },
  });
}

function connectorCompletionPage(
  provider: OAuthConnectorId,
  status: number,
  targetOrigin: string,
  failure?: string,
): Response {
  const completion = JSON.stringify({
    type: "nanocodex:connector-complete",
    connector: provider,
    result: failure ? "error" : "success",
    ...(failure ? { error: failure, message: "The connector authorization did not complete." } : {}),
  });
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Nanocodex connector</title></head><body><p>Connection flow complete. You can close this window.</p><script>window.opener?.postMessage(${completion},${JSON.stringify(targetOrigin)});window.close();</script></body></html>`;
  return new Response(html, {
    status,
    headers: {
      "content-security-policy": "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      "content-type": "text/html; charset=utf-8",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff",
    },
  });
}

function isConnectorState(value: unknown): value is ConnectorState {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && isBrokerUserId(value.brokerUserId)
    && typeof value.dialogOrigin === "string"
    && isAllowedDialogOrigin(value.dialogOrigin)
    && typeof value.provider === "string"
    && OAUTH_CONNECTOR_IDS.includes(value.provider as OAuthConnectorId)
    && (value.returnTo === undefined
      || isDeviceMcpReturn(value.returnTo, value.dialogOrigin));
}

function isMcpConnectionState(value: unknown): value is McpConnectionState {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && isBrokerUserId(value.brokerUserId)
    && isMcpConnectionId(value.connectionId)
    && typeof value.dialogOrigin === "string"
    && isAllowedDialogOrigin(value.dialogOrigin)
    && (value.returnTo === undefined
      || isDeviceMcpReturn(value.returnTo, value.dialogOrigin));
}

function deviceMcpReturn(value: unknown, dialogOrigin: string): string {
  if (typeof value !== "string" || value.length > 2_048) {
    throw new ApiFailure(400, "invalid_mcp_return", "The device return URL is invalid.");
  }
  let url: URL;
  try { url = new URL(value, dialogOrigin); } catch {
    throw new ApiFailure(400, "invalid_mcp_return", "The device return URL is invalid.");
  }
  const keys = [...url.searchParams.keys()];
  const userCodes = url.searchParams.getAll("user_code");
  const apiOrigins = url.searchParams.getAll("api_origin");
  if (url.origin !== dialogOrigin || url.pathname !== "/connect" || url.hash
    || keys.some((key) => key !== "user_code" && key !== "api_origin")
    || userCodes.length !== 1 || !/^[A-Z0-9]{8}$/.test(userCodes[0]!)
    || apiOrigins.length > 1
    || (apiOrigins[0] !== undefined
      && apiOrigins[0] !== API_ORIGIN
      && !isLocalDevelopmentOrigin(apiOrigins[0]))) {
    throw new ApiFailure(400, "invalid_mcp_return", "The device return URL is invalid.");
  }
  return url.href;
}

function isDeviceMcpReturn(value: unknown, dialogOrigin: string): value is string {
  if (typeof value !== "string") return false;
  try {
    return deviceMcpReturn(value, dialogOrigin) === value;
  } catch {
    return false;
  }
}

function requestedConnectors(value: unknown): ConnectorId[] {
  if (value === undefined) return [];
  if (!Array.isArray(value)) {
    throw new ApiFailure(400, "invalid_requested_connectors", "requested_connectors must be a connector ID array.");
  }
  const requested = new Set<ConnectorId>();
  for (const item of value) {
    if (typeof item !== "string" || !CONNECTOR_IDS.includes(item as ConnectorId)) {
      throw new ApiFailure(400, "invalid_requested_connectors", "requested_connectors contains an unknown connector.");
    }
    requested.add(item as ConnectorId);
  }
  if (requested.size !== value.length) {
    throw new ApiFailure(400, "invalid_requested_connectors", "requested_connectors cannot contain duplicates.");
  }
  return [...requested];
}

function requestedMcpConnections(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 16) {
    throw new ApiFailure(400, "invalid_requested_mcp_connections", "requested_mcp_connections must be a bounded connection ID array.");
  }
  const requested = new Set<string>();
  for (const item of value) {
    if (!isMcpConnectionId(item)) {
      throw new ApiFailure(400, "invalid_requested_mcp_connections", "requested_mcp_connections contains an invalid connection ID.");
    }
    requested.add(item);
  }
  if (requested.size !== value.length) {
    throw new ApiFailure(400, "invalid_requested_mcp_connections", "requested_mcp_connections cannot contain duplicates.");
  }
  return [...requested];
}

function requireRequestedConnectors(
  connected: readonly ConnectorId[],
  requested: readonly ConnectorId[],
): void {
  if (!requestedConnectorsSatisfied(connected, requested)) {
    throw new ApiFailure(403, "connector_not_connected", "Every requested connector must be connected before creating a grant.");
  }
}

async function connectedRequestedConnectors(
  env: Env,
  brokerUserId: string,
  requested: readonly ConnectorId[],
): Promise<ConnectorId[]> {
  if (requested.length === 0) return [];
  const current = (await connectorStatuses(env, brokerUserId)).connectors;
  return requested.filter((connector) => current[connector].connected);
}

function randomSubject(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function bindSubject(
  env: Env,
  subject: string,
  brokerUserId: string,
): Promise<void> {
  const response = await brokerFetch(env, `/subjects/${subject}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: brokerUserId }),
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(502, "egress_subject_bind_failed", "The private egress subject could not be bound.");
  }
  await response.body?.cancel();
}

async function unbindSubject(
  env: Env,
  subject: string,
  brokerUserId: string,
): Promise<void> {
  const response = await brokerFetch(env, `/subjects/${subject}`, {
    method: "DELETE",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: brokerUserId }),
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new ApiFailure(502, "egress_subject_unbind_failed", "The private egress subject could not be revoked.");
  }
  await response.body?.cancel();
}

async function grantConnectorRequest(
  env: Env,
  grant: GrantRecord,
  connector: OAuthConnectorId,
  value: Record<string, unknown>,
): Promise<{ status: number; headers: Record<string, string>; body: string }> {
  if (grant.status !== "active") {
    throw new ApiFailure(409, "grant_inactive", "The grant is not active.");
  }
  if (grant.expiresAt <= Math.floor(Date.now() / 1000)) {
    throw new ApiFailure(409, "grant_expired", "The grant has expired.");
  }
  if (!grant.capabilities.includes(connector)) {
    throw new ApiFailure(403, "connector_not_granted", "The connector is not granted to this connection.");
  }
  if (!EGRESS_SUBJECT.test(grant.egressSubject)) {
    throw new ApiFailure(409, "connector_subject_unavailable", "Reconnect to authorize connector execution.");
  }

  const method = typeof value.method === "string" ? value.method.toUpperCase() : "GET";
  if (!CONNECTOR_METHODS.has(method)) {
    throw new ApiFailure(400, "invalid_connector_method", "The connector request method is not allowed.");
  }
  const target = connectorTarget(connector, value.path);
  const headers = connectorHeaders(value.headers);
  headers.set("authorization", PROVIDER_CREDENTIAL_PLACEHOLDER);
  headers.set("x-nanocodex-subject", grant.egressSubject);
  const body = value.body;
  if (body !== undefined && typeof body !== "string") {
    throw new ApiFailure(400, "invalid_connector_body", "The connector request body must be a string.");
  }
  if (typeof body === "string" && new TextEncoder().encode(body).byteLength > MAX_CONNECTOR_REQUEST_BODY_BYTES) {
    throw new ApiFailure(413, "connector_body_too_large", "Connector request bodies are limited to 256 KiB.");
  }
  if (body !== undefined && (method === "GET" || method === "HEAD")) {
    throw new ApiFailure(400, "invalid_connector_body", "GET and HEAD connector requests cannot have a body.");
  }

  const response = await env.EGRESS.fetch(new Request(target, {
    method,
    headers,
    ...(body === undefined ? {} : { body }),
  }));
  const responseBody = await boundedResponseText(response, MAX_CONNECTOR_RESPONSE_BODY_BYTES);
  const responseHeaders: Record<string, string> = {};
  for (const [name, headerValue] of response.headers) {
    if (CONNECTOR_RESPONSE_HEADERS.has(name.toLowerCase()) && headerValue.length <= 4_096) {
      responseHeaders[name.toLowerCase()] = headerValue;
    }
  }
  return { status: response.status, headers: responseHeaders, body: responseBody };
}

async function grantMcpRequest(
  request: Request,
  env: Env,
  grant: GrantRecord,
  connectionId: string,
): Promise<Response> {
  if (grant.status !== "active" || grant.expiresAt <= Math.floor(Date.now() / 1_000)) {
    throw new ApiFailure(409, "grant_inactive", "The grant is inactive or expired.");
  }
  if (!grant.mcpConnections?.some((connection) => connection.id === connectionId)
    || !grant.capabilities.includes(`mcp:${connectionId}`)) {
    throw new ApiFailure(403, "mcp_not_granted", "This remote MCP connection is outside the grant.");
  }
  if (!EGRESS_SUBJECT.test(grant.egressSubject)) {
    throw new ApiFailure(409, "mcp_subject_unavailable", "Reconnect to authorize remote MCP execution.");
  }
  if (!CONNECTOR_METHODS.has(request.method)) {
    throw new ApiFailure(405, "method_not_allowed", "The remote MCP request method is unsupported.");
  }
  const headers = new Headers({ "x-nanocodex-subject": grant.egressSubject });
  for (const name of ["accept", "content-type", "last-event-id", "mcp-protocol-version", "mcp-session-id"] as const) {
    const value = request.headers.get(name);
    if (value && value.length <= 4_096) headers.set(name, value);
  }
  let body: ArrayBuffer | undefined;
  if (request.method !== "GET" && request.method !== "HEAD" && request.body) {
    body = await boundedRequestBytes(request, MAX_CONNECTOR_REQUEST_BODY_BYTES);
  }
  const upstream = await env.EGRESS.fetch(new Request(
    `https://mcp.internal/v1/connections/${connectionId}`,
    {
      method: request.method,
      headers,
      ...(body ? { body } : {}),
      redirect: "manual",
      signal: request.signal,
    },
  ));
  const responseHeaders = new Headers({ "cache-control": "no-store" });
  for (const name of ["content-type", "mcp-session-id", "retry-after"] as const) {
    const value = upstream.headers.get(name);
    if (value && value.length <= 4_096) responseHeaders.set(name, value);
  }
  return new Response(upstream.body, {
    status: upstream.status,
    headers: responseHeaders,
  });
}

function connectorTarget(connector: OAuthConnectorId, value: unknown): URL {
  if (typeof value !== "string" || value.length === 0 || value.length > 8_192
    || !value.startsWith("/") || value.startsWith("//")) {
    throw new ApiFailure(400, "invalid_connector_path", "The connector request path is invalid.");
  }
  const origin = connector === "github"
    ? "https://api.github.com"
    : connector === "gmail"
      ? "https://gmail.googleapis.com"
      : connector === "gdrive"
        ? "https://www.googleapis.com"
        : "https://api.x.com";
  const target = new URL(value, origin);
  const pathAllowed = connector === "github"
    || (connector === "gmail" && /^\/gmail\/v1\/users\/me(?:\/|$)/.test(target.pathname))
    || (connector === "gdrive" && /^(?:\/drive\/v3|\/upload\/drive\/v3)(?:\/|$)/.test(target.pathname))
    || (connector === "x" && /^\/2\/(?:tweets|users|lists|dm_(?:conversations|events)|media)(?:\/|$)/.test(target.pathname));
  if (target.origin !== origin || target.username || target.password || target.hash || !pathAllowed) {
    throw new ApiFailure(403, "connector_destination_denied", "The connector destination is not allowed.");
  }
  let count = 0;
  for (const [name, queryValue] of target.searchParams) {
    count += 1;
    if (count > 64 || name.length > 128 || queryValue.length > 4_096
      || /^(?:access_token|api_key|authorization|key|oauth_token)$/i.test(name)) {
      throw new ApiFailure(403, "connector_destination_denied", "The connector query is not allowed.");
    }
  }
  return target;
}

function connectorHeaders(value: unknown): Headers {
  if (value === undefined) return new Headers();
  if (!isRecord(value) || Object.keys(value).length > 16) {
    throw new ApiFailure(400, "invalid_connector_headers", "Connector request headers must be a bounded string map.");
  }
  const headers = new Headers();
  for (const [rawName, headerValue] of Object.entries(value)) {
    const name = rawName.toLowerCase();
    if (typeof headerValue !== "string" || rawName.length > 128 || headerValue.length > 4_096
      || FORBIDDEN_CONNECTOR_HEADERS.test(name)) {
      throw new ApiFailure(403, "connector_header_forbidden", "Credential and authority headers are forbidden.");
    }
    if (!CONNECTOR_REQUEST_HEADERS.has(name)) {
      throw new ApiFailure(400, "connector_header_unsupported", `Connector header ${rawName} is not supported.`);
    }
    try { headers.set(name, headerValue); } catch {
      throw new ApiFailure(400, "invalid_connector_headers", "A connector request header is invalid.");
    }
  }
  return headers;
}

async function brokerJson(
  env: Env,
  path: string,
  init?: RequestInit,
): Promise<Record<string, unknown>> {
  const response = await brokerFetch(env, path, init);
  const text = await boundedResponseText(response, MAX_BROKER_BODY_BYTES);
  if (!response.ok) {
    throw new ApiFailure(502, "connector_broker_failed", "The connector broker rejected the operation.");
  }
  try { return object(JSON.parse(text), "connector broker response"); } catch {
    throw new ApiFailure(502, "connector_broker_invalid", "The connector broker returned an invalid response.");
  }
}

function brokerFetch(env: Env, path: string, init?: RequestInit): Promise<Response> {
  return env.EGRESS.fetch(new Request(`https://broker.internal${path}`, init));
}

async function boundedResponseText(response: Response, limit: number): Promise<string> {
  return new TextDecoder().decode(await boundedResponseBytes(response, limit));
}

async function boundedResponseBytes(response: Response, limit: number): Promise<ArrayBuffer> {
  const declared = response.headers.get("content-length");
  if (declared && Number(declared) > limit) {
    await response.body?.cancel();
    throw new ApiFailure(502, "upstream_response_too_large", "The upstream response exceeded its size limit.");
  }
  if (!response.body) return new ArrayBuffer(0);
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      throw new ApiFailure(502, "upstream_response_too_large", "The upstream response exceeded its size limit.");
    }
    chunks.push(value);
  }
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return joined.buffer;
}

function boundedOptionalString(value: unknown, limit: number): string | undefined {
  return typeof value === "string" && value.length > 0 && value.length <= limit ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function createDeviceCode(env: Env, store: Kv.Kv) {
  return Handler.deviceCode({
    baseUrl(request) {
      const origin = new URL(request.url).origin;
      if (
        origin === API_ORIGIN
        || isLocalDevelopmentOrigin(origin)
      ) return origin;
      throw new Error("The device-code host origin is not allowed.");
    },
    path: "/v1/device",
    pollingInterval: 1_000,
    store,
    html: {
      async render({ record, request, userCode }) {
        const normalized = normalizeDeviceUserCode(userCode);
        if (acceptsHtml(request)) {
          const apiOrigin = new URL(request.url).origin;
          const verification = localDeviceVerificationUrl(apiOrigin, normalized)
            ?? deviceVerificationUrl(apiOrigin, normalized);
          return new Response(null, { status: 302, headers: { location: verification.href } });
        }
        if (!request.headers.get("accept")?.includes("application/json")) {
          return Response.json({ error: "not_acceptable" }, { status: 406 });
        }
        if (!normalized || !record || record.status !== "pending") {
          return Response.json({
            error: "unknown_code",
            error_description: "Unknown or expired device code.",
          }, { status: 404 });
        }
        try {
          if (record.message.type !== "rpc-requests" || record.message.payload.length !== 1) {
            throw new Error("Device authorization requires one RPC request.");
          }
          const pending = parseCliWalletRequest(record.message.payload[0]);
          const mcpResources = validateMcpResources(pending.resources);
          const requestedMcpConnections = await pendingMcpConnections(env, store, mcpResources.requested);
          return Response.json({
            user_code: normalized,
            app: cliApp,
            request: {
              jsonrpc: "2.0",
              id: pending.id,
              method: pending.method,
              params: pending.params,
            },
            requested_mcp_connections: requestedMcpConnections,
            ...(mcpResources.focus ? { focus_mcp_connection: mcpResources.focus } : {}),
          });
        } catch (cause) {
          return Response.json({
            error: "invalid_request",
            error_description: errorText(cause),
          }, { status: 400 });
        }
      },
    },
    async validate({ request, result }) {
      try {
        const pending = parseCliWalletRequest(request);
        const sanitized = sanitizeCliWalletResult(result);
        if (JSON.stringify(sanitized) !== JSON.stringify(result)) {
          throw new Error("The approved result contains fields outside the CLI grant contract.");
        }
        const account = sanitized.accounts[0]!;
        const approvalId = account.capabilities.auth.approval_id;
        const approval = await store.get<ConnectApproval>(`connect-approval:${approvalId}`);
        if (!isConnectApproval(approval)
          || approval.accountAddress.toLowerCase() !== account.address.toLowerCase()
          || approval.appId !== CLI_APP_ID
          || approval.appOrigin !== CLI_APP_ORIGIN
          || !sameResources(approval.resources, pending.resources)) {
          throw new Error("The CLI approval is not bound to this authenticated request.");
        }
        if ("mode" in account.capabilities.auth && account.capabilities.auth.mode === "hosted") {
          if (approval.authorization !== "hosted"
            || approval.keyAuthorization !== undefined
            || !pending.resources.includes(HOSTED_AUTHORIZATION_RESOURCE)
            || pending.resources.includes("urn:nanocodex:mpp:machusd:spend")) {
            throw new Error("The hosted CLI approval is not permitted for this request.");
          }
          return undefined;
        }
        if (!("personalSign" in account.capabilities)
          || !("keyAuthorization" in account.capabilities)) {
          throw new Error("The signed CLI approval is incomplete.");
        }
        const serialized = account.capabilities.personalSign.keyAuthorization;
        if (approval.authorization !== "signed"
          || approval.keyAuthorization?.toLowerCase() !== serialized.toLowerCase()) {
          throw new Error("The signed CLI approval does not match this request.");
        }
        const accessKey = accessKeyWire(
          account.capabilities.keyAuthorization,
          serialized,
          account.address,
        );
        if (!approvedCliAccessKeyMatches(pending, accessKey)) {
          throw new Error("The signed CLI access key does not match the retained request.");
        }
        return undefined;
      } catch (cause) {
        return Response.json({
          error: "invalid_approval",
          error_description: errorText(cause),
        }, { status: 403 });
      }
    },
  });
}

async function pendingMcpConnections(env: Env, store: Kv.Kv, ids: readonly string[]): Promise<McpConnection[]> {
  if (ids.length === 0) return [];
  const owners = await Promise.all(ids.map((id) => store.get<unknown>(`mcp-intent-owner:${id}`)));
  const owner = owners[0];
  if (typeof owner === "string" && owners.every((candidate) => candidate === owner) && isBrokerUserId(owner)) {
    const byId = new Map((await mcpConnectionStatuses(env, owner)).map((connection) => [connection.id, connection]));
    return ids.map((id) => byId.get(id) ?? (() => { throw new Error("A requested remote MCP connection is unavailable."); })());
  }
  return Promise.all(ids.map(async (id) => {
    const intent = await store.get<McpIntent>(`mcp-intent:${id}`);
    if (!isMcpIntent(intent) || intent.expiresAt <= Math.floor(Date.now() / 1_000)) {
      throw new Error("A requested remote MCP intent is unavailable or expired.");
    }
    return { id, name: intent.name, status: "authorization_required" as const };
  }));
}

function normalizeDeviceUserCode(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const normalized = value.replace(/[\s-]/g, "").toUpperCase();
  return /^[A-Z]{8}$/.test(normalized) ? normalized : undefined;
}

function acceptsHtml(request: Request): boolean {
  return request.headers.get("accept")?.split(",").some((value) => (
    value.trim().split(";", 1)[0] === "text/html"
  )) === true;
}

function localDeviceVerificationUrl(
  apiOrigin: string,
  userCode: string | undefined,
): URL | undefined {
  if (!isLocalDevelopmentOrigin(apiOrigin)) return undefined;
  const verification = new URL("/connect", apiOrigin);
  verification.searchParams.set("api_origin", apiOrigin);
  if (userCode) verification.searchParams.set("user_code", userCode);
  return verification;
}

function sameResources(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((resource) => right.includes(resource));
}

function errorText(cause: unknown): string {
  return cause instanceof Error && cause.message
    ? cause.message
    : "Invalid device authorization request.";
}

function createAuth(
  env: Env,
  store: Kv.Kv,
  request: Request,
  context?: AuthRequestContext,
) {
  return Handler.auth({
    cookie: false,
    cors: false,
    origin: authenticationOrigin(request),
    path: "/v1/connect/auth",
    statement: "Authorize this app to use your Nanocodex agent and bounded MPP access key.",
    store,
    // Connect accepts root passkey accounts, so Tempo's contract-account probe is
    // both unnecessary and an external RPC dependency on the login hot path.
    transport: custom({
      async request({ method }) {
        if (method === "eth_getCode") return "0x";
        throw new Error(`Connect passkey verification does not permit RPC method ${method}.`);
      },
    }),
    async onAuthenticate({ address: authenticated, message }) {
      const startedAt = performance.now();
      const timings: Array<readonly [string, number]> = [];
      const mark = (name: string) => timings.push([name, performance.now()]);
      if (!context || context.message !== message) {
        throw new Error("The authenticated Connect request is unavailable.");
      }
      const accountAddress = address(authenticated);
      const approvalId = randomSubject();
      const resources = siweResources(message);
      const app = approvedAppContext(resources);
      const externalIdentity = await takeEmbedIdentitySession(store, resources, app);
      const externalPrincipalId = externalIdentity
        ? await embedPrincipalId(externalIdentity)
        : undefined;
      let connectorsDuration = 0;
      const identity = await connectBrokerIdentity(env, store, accountAddress);
      mark("identity");
      const resourcesStartedAt = performance.now();
      const [status, mcpConnections] = await Promise.all([
        measured(connectorStatuses(env, identity.userId), (duration) => { connectorsDuration = duration; }),
        identity.linked
          ? materializeApprovedMcpConnections(env, store, app, identity.userId, resources)
          : pendingMcpConnections(env, store, approvedMcpConnectionIds(resources)),
      ]);
      mark("resources");
      const connectedConnectors = CONNECTOR_IDS.filter((connector) => status.connectors[connector].connected);
      await store.set(`connect-approval:${approvalId}`, {
        accountAddress,
        appId: app.appId,
        appOrigin: app.origin,
        authorization: "signed",
        brokerUserId: identity.userId,
        connectedConnectors,
        mcpConnections,
        ...(context.keyAuthorization ? { keyAuthorization: context.keyAuthorization } : {}),
        ...(externalPrincipalId ? { externalPrincipalId } : {}),
        profileLinked: identity.linked,
        resources,
      } satisfies ConnectApproval, { ttl: CONNECT_APPROVAL_TTL });
      if (!identity.linked && mcpConnections.length > 0) {
        await store.set(`pending-mcp-account-link:${accountAddress.toLowerCase()}`, {
          appId: app.appId,
          appOrigin: app.origin,
          resources,
        } satisfies PendingMcpAccountLink, { ttl: CONNECT_APPROVAL_TTL });
      }
      mark("approval");
      return Response.json({
        agent_id: await agentId(accountAddress),
        approval_id: approvalId,
        connectors: status.connectors,
        mcp_connections: mcpConnections,
        profile: { linked: identity.linked },
      }, { headers: { "server-timing": [
        `identity;dur=${(resourcesStartedAt - startedAt).toFixed(1)}`,
        `connectors;dur=${connectorsDuration.toFixed(1)}`,
        `approval;dur=${(performance.now() - (timings.at(-2)?.[1] ?? resourcesStartedAt)).toFixed(1)}`,
      ].join(", ") } });
    },
  });
}

async function measured<value>(
  promise: Promise<value>,
  record: (duration: number) => void,
): Promise<value> {
  const startedAt = performance.now();
  try {
    return await promise;
  } finally {
    record(performance.now() - startedAt);
  }
}

async function authRequestContext(request: Request, url: URL): Promise<AuthRequestContext | undefined> {
  if (request.method !== "POST" || url.pathname !== "/v1/connect/auth") return undefined;
  const body = await request.clone().json().catch(() => undefined);
  if (!isRecord(body) || typeof body.message !== "string") return undefined;
  const keyAuthorization = body.keyAuthorization;
  return {
    message: body.message,
    ...(typeof keyAuthorization === "string" && /^0x[0-9a-fA-F]+$/.test(keyAuthorization)
      ? { keyAuthorization: keyAuthorization as `0x${string}` }
      : {}),
  };
}

async function takeEmbedIdentitySession(
  store: Kv.Kv,
  resources: readonly string[],
  app: CallerApp,
): Promise<EmbedIdentity | undefined> {
  let token: string | undefined;
  try {
    token = identitySessionToken(resources);
  } catch (cause) {
    throw new ApiFailure(403, "invalid_embed_session", errorText(cause));
  }
  if (!token) return undefined;
  if (!store.take) {
    throw new ApiFailure(503, "embed_session_unavailable", "One-time embedded identity sessions are unavailable.");
  }
  const session = await store.take<EmbedIdentitySession>(`embed-session:${token}`);
  if (!isEmbedIdentitySession(session)
    || session.expiresAt <= Math.floor(Date.now() / 1_000)
    || session.appId !== app.appId
    || session.appOrigin !== app.origin) {
    throw new ApiFailure(403, "invalid_embed_session", "The embedded identity session is invalid or expired.");
  }
  return {
    appId: session.appId,
    appOrigin: session.appOrigin,
    issuer: session.issuer,
    subject: session.subject,
    ...(session.organization === undefined ? {} : { organization: session.organization }),
  };
}

function isEmbedIdentitySession(value: unknown): value is EmbedIdentitySession {
  if (!isRecord(value) || !Number.isSafeInteger(value.expiresAt)) return false;
  return isEmbedIdentity(value);
}

async function takeConnectApproval(
  store: Kv.Kv,
  approvalId: string,
  accountAddress: `0x${string}`,
): Promise<ConnectApproval> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(approvalId) || !store.take) {
    throw new ApiFailure(403, "approval_unavailable", "The signed Connect approval is unavailable.");
  }
  const approval = await store.take<ConnectApproval>(`connect-approval:${approvalId}`);
  if (!isConnectApproval(approval)
    || approval.accountAddress.toLowerCase() !== accountAddress.toLowerCase()) {
    throw new ApiFailure(403, "approval_unavailable", "The signed Connect approval is unavailable.");
  }
  return approval;
}

async function readConnectApproval(
  store: Kv.Kv,
  approvalId: string,
  accountAddress: `0x${string}`,
): Promise<ConnectApproval> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(approvalId)) {
    throw new ApiFailure(403, "approval_unavailable", "The signed Connect approval is unavailable.");
  }
  const approval = await store.get<ConnectApproval>(`connect-approval:${approvalId}`);
  if (!isConnectApproval(approval)
    || approval.accountAddress.toLowerCase() !== accountAddress.toLowerCase()) {
    throw new ApiFailure(403, "approval_unavailable", "The signed Connect approval is unavailable.");
  }
  return approval;
}

function isConnectApproval(value: unknown): value is ConnectApproval {
  return isRecord(value)
    && /^0x[0-9a-fA-F]{40}$/.test(String(value.accountAddress))
    && validAppId(value.appId)
    && isPublicAppOrigin(value.appOrigin)
    && (value.brokerUserId === undefined || isBrokerUserId(value.brokerUserId))
    && (value.connectedConnectors === undefined
      || (Array.isArray(value.connectedConnectors)
        && value.connectedConnectors.every((connector) => CONNECTOR_IDS.includes(connector as ConnectorId))))
    && (value.mcpConnections === undefined
      || (Array.isArray(value.mcpConnections)
        && value.mcpConnections.length <= 16
        && value.mcpConnections.every(isMcpConnection)))
    && (value.durableAgentId === undefined || isConnectAgentId(value.durableAgentId))
    && Array.isArray(value.resources)
    && value.resources.every((resource) => typeof resource === "string")
    && (value.authorization === "signed" || value.authorization === "hosted")
    && (value.profileLinked === undefined || typeof value.profileLinked === "boolean")
    && (value.externalPrincipalId === undefined
      || /^[A-Za-z0-9_-]{43}$/.test(String(value.externalPrincipalId)))
    && (value.keyAuthorization === undefined
      || (typeof value.keyAuthorization === "string" && /^0x[0-9a-fA-F]+$/.test(value.keyAuthorization)));
}

function requireApprovedCapabilities(
  resources: readonly string[],
  appId: string,
  requested: readonly ConnectorId[],
  requestedMcpIds: readonly string[],
) {
  const approvedResources = new Set(resources);
  const required = [
    ...(appId === CHROME_EXTENSION_APP_ID || appId === CLI_APP_ID
      ? ["urn:nanocodex:agent:run"]
      : BASE_APPROVAL_RESOURCES),
    `urn:nanocodex:app:${encodeURIComponent(appId)}`,
  ];
  if (required.some((resource) => !approvedResources.has(resource))) {
    throw new ApiFailure(403, "capability_not_approved", "The app grant was not present in the signed SIWE approval.");
  }
  const approved = approvedConnectors(resources);
  if (requested.some((connector) => !approved.has(connector))) {
    throw new ApiFailure(403, "connector_not_approved", "A requested connector was not present in the signed SIWE approval.");
  }
  if (appId === CLI_APP_ID
    && (approved.size !== requested.length || requested.some((connector) => !approved.has(connector)))) {
    throw new ApiFailure(403, "connector_mismatch", "The CLI grant must exchange exactly its signed connector set.");
  }
  const approvedMcpIds = approvedMcpConnectionIds(resources);
  if (requestedMcpIds.some((id) => !approvedMcpIds.includes(id))) {
    throw new ApiFailure(403, "mcp_not_approved", "A requested remote MCP was not present in the signed approval.");
  }
  if (appId === CLI_APP_ID
    && (approvedMcpIds.length !== requestedMcpIds.length
      || requestedMcpIds.some((id) => !approvedMcpIds.includes(id)))) {
    throw new ApiFailure(403, "mcp_mismatch", "The CLI grant must exchange exactly its signed remote MCP set.");
  }
}

function approvedAppContext(resources: readonly string[]): CallerApp {
  const appIds = resourceValues(resources, APP_RESOURCE_PREFIX);
  const origins = resourceValues(resources, APP_ORIGIN_RESOURCE_PREFIX);
  if (appIds.length !== 1 || origins.length !== 1) {
    throw new Error("The signed Connect approval must identify exactly one app and origin.");
  }
  return validateCallerApp(appIds[0]!, origins[0]!);
}

function resourceValues(resources: readonly string[], prefix: string): string[] {
  return resources.flatMap((resource) => {
    if (!resource.startsWith(prefix)) return [];
    try {
      return [decodeURIComponent(resource.slice(prefix.length))];
    } catch {
      throw new Error("The signed Connect app identity is malformed.");
    }
  });
}

function approvedAgentCapabilities(resources: readonly string[]): string[] {
  const approved = new Set(resources);
  const compact = new Set(resources
    .filter((resource) => resource.startsWith(AGENT_VISIBILITY_RESOURCE_PREFIX))
    .flatMap((resource) => resource.slice(AGENT_VISIBILITY_RESOURCE_PREFIX.length).split(",")));
  if (approved.has("urn:nanocodex:agent:trace:read") || compact.has("traces")) {
    return [...new Set(Object.values(AGENT_VISIBILITY_RESOURCES))];
  }
  const legacy = Object.entries(AGENT_VISIBILITY_RESOURCES)
    .filter(([resource]) => approved.has(resource))
    .map(([, capability]) => capability);
  const combined = Object.entries(AGENT_VISIBILITY_NAMES)
    .filter(([name]) => compact.has(name))
    .map(([, capability]) => capability);
  return [...new Set([...legacy, ...combined])];
}

function approvedHostedCapabilities(resources: readonly string[]): string[] {
  const approved = new Set(resources);
  return [
    ...(approved.has("urn:nanocodex:capability:mercator:boost") ? ["mercator.boost"] : []),
    ...(approved.has("urn:nanocodex:mpp:machusd:spend") ? ["mpp.machusd"] : []),
    ...(approved.has(HOSTED_HISTORY_RESOURCE) ? ["history:read"] : []),
    ...(approved.has(HOSTED_MEMORY_READ_RESOURCE) ? ["memory:read"] : []),
    ...(approved.has(HOSTED_MEMORY_WRITE_RESOURCE) ? ["memory:write"] : []),
  ];
}

function approvedConnectors(resources: readonly string[]): Set<string> {
  return new Set(resources.flatMap((resource) => {
    if (resource.startsWith("urn:nanocodex:connector:")) {
      return [resource.slice("urn:nanocodex:connector:".length)];
    }
    if (resource.startsWith(CONNECTORS_RESOURCE_PREFIX)) {
      return resource.slice(CONNECTORS_RESOURCE_PREFIX.length).split(",");
    }
    return [];
  }).filter((connector) => (CONNECTOR_IDS as readonly string[]).includes(connector)));
}

function approvedMcpConnectionIds(resources: readonly string[]): string[] {
  try {
    return [...validateMcpResources(resources).requested];
  } catch {
    throw new ApiFailure(403, "invalid_mcp_resources", "The signed remote MCP resources are invalid.");
  }
}

function siweResources(message: string): string[] {
  const lines = message.split("\n");
  const marker = lines.indexOf("Resources:");
  if (marker === -1) return [];
  const resources: string[] = [];
  for (const line of lines.slice(marker + 1)) {
    if (!line.startsWith("- ")) break;
    const resource = line.slice(2);
    if (resource.length > 0 && resource.length <= 2_048) resources.push(resource);
  }
  return resources;
}

function connectionBalances(account: `0x${string}`): Promise<readonly [bigint, bigint]> {
  return Promise.all([
    tokenBalance(MACHINE_USD, account),
    tokenBalance(USDC_E, account),
  ]);
}

function connectionWire(grant: GrantRecord, grantToken: string) {
  const balancesReady = grant.balanceAtomics !== undefined
    && grant.settlementBalanceAtomics !== undefined;
  return {
    grant_token: grantToken,
    account_id: grant.brokerUserId,
    account_address: grant.accountAddress,
    agent_id: grant.agentId,
    grant: grantWire(grant),
    mcp_connections: grant.mcpConnections ?? [],
    authorization_mode: grant.accessKey ? "access_key" : "hosted",
    ...(grant.accessKey ? { access_key: grant.accessKey } : {}),
    mpp: {
      token: MACHINE_USD,
      symbol: "MACHUSD",
      balance_atomics: grant.balanceAtomics ?? "0",
      balance_status: balancesReady ? "ready" : "pending",
      settlement_token: USDC_E,
      settlement_symbol: "USDC.e",
      settlement_balance_atomics: grant.settlementBalanceAtomics ?? "0",
      spent_atomics: grant.spentAtomics,
      limit_atomics: MPP_LIMIT.toString(),
      period: MPP_PERIOD,
      max_per_request_atomics: MPP_MAX_PER_REQUEST.toString(),
    },
  };
}

function grantWire(grant: GrantRecord) {
  return {
    id: grant.id,
    permission: grant.permission,
    status: grant.status,
    expires_at: grant.expiresAt,
    capabilities: grant.capabilities,
    mcp_connections: grant.mcpConnections ?? [],
  };
}

function accessKeyWire(
  value: Record<string, unknown>,
  serialized: `0x${string}`,
  expectedAccount?: `0x${string}`,
) {
  const authorization = KeyAuthorization.deserialize(serialized);
  if (("isAdmin" in authorization && authorization.isAdmin === true)) {
    throw new ApiFailure(403, "invalid_access_key_policy", "Administrative access keys cannot back a Nanocodex grant.");
  }
  const claimedKeyId = address(value.keyId ?? value.address);
  const keyAddress = address(authorization.address);
  if (claimedKeyId.toLowerCase() !== keyAddress.toLowerCase()) {
    throw new Error("The access-key identifier does not match the signed authorization.");
  }
  if (!authorization.signature) throw new Error("The access-key authorization is not signed.");
  if (authorization.account !== undefined
    && expectedAccount !== undefined
    && authorization.account.toLowerCase() !== expectedAccount.toLowerCase()) {
    throw new ApiFailure(403, "invalid_access_key_policy", "The access key is bound to another account.");
  }
  const witness = hex(authorization.witness, "key_authorization.witness");
  if (witness.length !== 66) throw new Error("key_authorization.witness must be 32 bytes.");
  const expiry = authorization.expiry;
  if (typeof expiry !== "number" || !Number.isSafeInteger(expiry)) {
    throw new Error("The access-key authorization must have an expiry.");
  }
  if (authorization.limits === undefined) {
    throw new ApiFailure(403, "invalid_access_key_policy", "The access key must explicitly constrain spending.");
  }
  const limits = authorization.limits.map(({ limit, period, token }) => ({
    token: address(token),
    limit: limit.toString(),
    period: Number.isSafeInteger(period) ? period : 0,
  }));
  const scopes = authorization.scopes?.map(({ address: target, recipients, selector }) => ({
    address: address(target),
    ...(selector ? { selector: hex(selector, "key_authorization.scope.selector") } : {}),
    ...(recipients ? { recipients: recipients.map(address) } : {}),
  })) ?? [];
  return {
    address: keyAddress,
    chain_id: authorization.chainId.toString(),
    key_id: keyAddress,
    key_type: authorization.type,
    limits,
    scopes,
    witness,
    expiry,
    authorization: serialized,
  };
}

async function tokenBalance(token: string, account: `0x${string}`): Promise<bigint> {
  const data = `0x70a08231000000000000000000000000${account.slice(2)}`;
  const response = await fetch(TEMPO_RPC, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_call", params: [{ to: token, data }, "latest"] }),
  });
  const body = await response.json() as { result?: string; error?: { message?: string } };
  if (!response.ok || typeof body.result !== "string") throw new Error(body.error?.message ?? "Tempo balance lookup failed.");
  return BigInt(body.result);
}

async function json(request: Request): Promise<Record<string, unknown>> {
  return object(await request.json(), "request body");
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object.`);
  return value as Record<string, unknown>;
}

function requiredString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${label} must be a non-empty string.`);
  return value;
}

function boundedIdentifier(value: unknown, label: string, limit: number): string {
  if (typeof value !== "string"
    || value.length === 0
    || value.length > limit
    || !/^[A-Za-z0-9._:-]+$/.test(value)) {
    throw new ApiFailure(400, "invalid_identifier", `${label} is invalid.`);
  }
  return value;
}

function requiredOrigin(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length > 2_048) {
    throw new ApiFailure(400, "invalid_origin", `${label} must be an HTTPS origin.`);
  }
  let url: URL;
  try { url = new URL(value); } catch {
    throw new ApiFailure(400, "invalid_origin", `${label} must be an HTTPS origin.`);
  }
  if (url.origin !== value || (url.protocol !== "https:" && !isLoopbackOrigin(url.origin))) {
    throw new ApiFailure(400, "invalid_origin", `${label} must be an HTTPS origin.`);
  }
  return url.origin;
}

function safeInteger(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) throw new Error(`${label} must be an integer.`);
  return value;
}

function address(value: unknown): `0x${string}` {
  if (typeof value !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(value)) throw new Error("Invalid Tempo address.");
  return value as `0x${string}`;
}

function hex(value: unknown, label: string): `0x${string}` {
  if (typeof value !== "string" || !/^0x[0-9a-fA-F]+$/.test(value)) throw new Error(`${label} must be hex.`);
  return value as `0x${string}`;
}

async function digestHex(value: string): Promise<`0x${string}`> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return `0x${[...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

// The SIWE verifier returns a stable pre-connection identifier. The actual
// Connection projection replaces it with the provisioned durable agent id.
async function agentId(account: `0x${string}`) {
  return `agent_${(await digestHex(`agent:${account.toLowerCase()}`)).slice(2, 18)}`;
}

function cors(response: Response, request: Request) {
  const origin = request.headers.get("origin");
  if (origin && allowedOrigin(request, origin)) {
    response.headers.set("access-control-allow-origin", origin);
    if (!isChromeExtensionOrigin(origin)) {
      response.headers.set("access-control-allow-credentials", "true");
    }
    response.headers.set(
      "access-control-allow-headers",
      "accept-payment, authorization, content-type, git-protocol, idempotency-key, last-event-id, mcp-protocol-version, mcp-session-id, payment-session, payment-session-snapshot, payment-signature, x-nanocodex-app-id, x-nanocodex-connect-client",
    );
    response.headers.set("access-control-allow-methods", "GET, POST, PUT, DELETE, OPTIONS");
    response.headers.set("access-control-max-age", "86400");
    response.headers.set(
      "access-control-expose-headers",
      "mcp-session-id, payment-receipt, payment-response, payment-session, payment-session-snapshot, retry-after, www-authenticate, x-nanocodex-realtime-location",
    );
    response.headers.set("vary", "Origin");
  }
  response.headers.set("cache-control", "no-store");
  return response;
}

function mutableResponse(response: Response): Response {
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}

async function proxyThreadGit(request: Request, env: Env, url: URL): Promise<Response> {
  if (request.method !== "GET" && request.method !== "POST") {
    throw new ApiFailure(405, "method_not_allowed", "Git workspace requests require GET or POST.");
  }
  const headers = new Headers();
  for (const name of ["accept", "content-type", "git-protocol"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  const target = new URL(`${url.pathname}${url.search}`, NANOCODEX_ORIGIN);
  const upstream = await env.NANOCODEX.fetch(new Request(target, {
    method: request.method,
    headers,
    ...(request.method === "POST" ? { body: request.body } : {}),
    redirect: "manual",
    signal: request.signal,
  }));
  const responseHeaders = new Headers();
  for (const name of ["cache-control", "content-type", "x-content-type-options"]) {
    const value = upstream.headers.get(name);
    if (value) responseHeaders.set(name, value);
  }
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: responseHeaders,
  });
}

function proxy(response: Response) {
  return new Response(response.body, {
    status: response.status,
    headers: { "content-type": response.headers.get("content-type") ?? "application/json" },
  });
}

function proxyPayment(response: Response) {
  const headers = new Headers({
    "content-type": response.headers.get("content-type") ?? "application/json",
  });
  for (const name of [
    "payment-receipt",
    "payment-response",
    "payment-session",
    "payment-session-snapshot",
    "www-authenticate",
  ]) {
    const value = response.headers.get(name);
    if (value) headers.set(name, value);
  }
  return new Response(response.body, { status: response.status, headers });
}

function requiredHeader(request: Request, name: string) {
  const value = request.headers.get(name);
  if (!value) throw new Error(`${name} header is required.`);
  return value;
}

const CLIENT_DIAGNOSTIC_STAGES = new Set([
  "experience_mounted",
  "composer_input",
  "send_pointer",
  "send_touch",
  "send_click",
  "form_submit",
  "prompt_accepted",
]);

function parseClientDiagnostic(encoded: string): { stage: string } {
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new ApiFailure(400, "invalid_diagnostic", "Client diagnostic JSON is invalid.");
  }
  const stage = isRecord(value) && typeof value.stage === "string" ? value.stage : undefined;
  if (!stage || !CLIENT_DIAGNOSTIC_STAGES.has(stage)) {
    throw new ApiFailure(400, "invalid_diagnostic", "Client diagnostic stage is invalid.");
  }
  return { stage };
}

function requireOnrampOrigin(request: Request): void {
  requireDialogOrigin(request);
}

function requirePlaygroundOrigin(request: Request): void {
  const origin = request.headers.get("origin");
  if (origin !== PLAYGROUND_ORIGIN && !developmentDialogOrigin(request, origin)) {
    throw new ApiFailure(403, "origin_denied", "This operation is available only to the registered Nanocodex app.");
  }
}

function requireGrantAppOrigin(
  request: Request,
  grant: Pick<GrantRecord, "appId" | "appOrigin">,
  ticket?: Pick<RealtimeTicket, "appId" | "appOrigin">,
): void {
  const origin = request.headers.get("origin");
  if (origin !== grant.appOrigin
    || (ticket !== undefined
      && (ticket.appId !== grant.appId || ticket.appOrigin !== grant.appOrigin))) {
    throw new ApiFailure(403, "origin_denied", "This operation is available only to the authorized Connect app.");
  }
}

function requireCallerApp(request: Request, claimedAppId?: string | null): CallerApp {
  const appId = claimedAppId ?? request.headers.get("x-nanocodex-app-id");
  const origin = request.headers.get("origin");
  if (!appId || !origin) {
    throw new ApiFailure(403, "app_identity_required", "The Connect app id and origin are required.");
  }
  try {
    return validateCallerApp(appId, origin);
  } catch (cause) {
    throw cause instanceof ApiFailure
      ? cause
      : new ApiFailure(403, "origin_denied", "This origin cannot use Nanocodex Connect.");
  }
}

function validateCallerApp(appId: unknown, origin: unknown): CallerApp {
  if (!validAppId(appId) || !isPublicAppOrigin(origin)) {
    throw new ApiFailure(403, "origin_denied", "This origin cannot use Nanocodex Connect.");
  }
  if (origin === PLAYGROUND_ORIGIN && appId !== REGISTERED_APP_ID) {
    throw new ApiFailure(403, "app_identity_mismatch", "The registered app id does not match this origin.");
  }
  if (origin === CHROME_EXTENSION_ORIGIN && appId !== CHROME_EXTENSION_APP_ID) {
    throw new ApiFailure(403, "app_identity_mismatch", "The registered app id does not match this origin.");
  }
  if (origin === CLI_APP_ORIGIN && appId !== CLI_APP_ID) {
    throw new ApiFailure(403, "app_identity_mismatch", "The registered app id does not match this origin.");
  }
  if (appId === REGISTERED_APP_ID
    && origin !== PLAYGROUND_ORIGIN
    && !isLoopbackOrigin(origin)) {
    throw new ApiFailure(403, "app_identity_mismatch", "This app id is reserved for its registered origin.");
  }
  if (appId === CHROME_EXTENSION_APP_ID && !isChromeExtensionOrigin(origin)) {
    throw new ApiFailure(403, "app_identity_mismatch", "This app id is reserved for Chrome extensions.");
  }
  if (appId === CLI_APP_ID && origin !== CLI_APP_ORIGIN) {
    throw new ApiFailure(403, "app_identity_mismatch", "This app id is reserved for the Nanocodex CLI.");
  }
  return Object.freeze({ appId, origin });
}

function validAppId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}

function isPublicAppOrigin(value: unknown): value is string {
  if (typeof value !== "string") return false;
  if (isChromeExtensionOrigin(value) || isLoopbackOrigin(value)) return true;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.origin === value && !url.username && !url.password;
  } catch {
    return false;
  }
}

function isChromeExtensionOrigin(value: string | null): boolean {
  return /^chrome-extension:\/\/[a-p]{32}$/.test(value ?? "");
}

async function scopedAppId(app: CallerApp): Promise<string> {
  return `${app.appId}:${(await digestHex(app.origin)).slice(2, 18)}`;
}

function requireDialogOrigin(request: Request): void {
  requiredDialogOrigin(request);
}

function requiredDialogOrigin(request: Request): string {
  const origin = request.headers.get("origin");
  if (origin && (origin === DIALOG_ORIGIN || developmentDialogOrigin(request, origin))) {
    return origin;
  }
  const requestOrigin = new URL(request.url).origin;
  const connectClient = request.headers.get("x-nanocodex-connect-client");
  if (!origin
    && (connectClient === "onboarding" || connectClient === "device")
    && (requestOrigin === DIALOG_ORIGIN || isLocalDeviceOrigin(requestOrigin))) {
    return requestOrigin;
  }
  throw new ApiFailure(403, "origin_denied", "This account operation is available only inside Nanocodex Connect.");
}

function isAllowedDialogOrigin(origin: string): boolean {
  return origin === DIALOG_ORIGIN || isLocalDeviceOrigin(origin);
}

function isLoopbackOrigin(origin: string | null): boolean {
  if (!origin) return false;
  let url: URL;
  try { url = new URL(origin); } catch { return false; }
  if (url.origin !== origin || (url.protocol !== "http:" && url.protocol !== "https:")) {
    return false;
  }
  const hostname = url.hostname.toLowerCase();
  const localNanocodex = hostname === "nanocodex.localhost"
    || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(hostname);
  return hostname === "localhost"
    || hostname === "127.0.0.1"
    || hostname === "[::1]"
    || (url.protocol === "http:"
      && Boolean(url.port)
      && localNanocodex);
}

function isLocalDevelopmentOrigin(origin: string | null): boolean {
  return isLoopbackOrigin(origin);
}

function authenticationOrigin(request: Request): string {
  const publicOrigin = localDevelopmentPublicOrigin(request);
  if (publicOrigin) return publicOrigin;
  const requestOrigin = new URL(request.url).origin;
  return isLocalDevelopmentOrigin(requestOrigin)
    ? requestOrigin
    : connectAuthOrigin(requestOrigin);
}

function localDevelopmentPublicOrigin(request: Request): string | undefined {
  const forwarded = request.headers.get("x-nanocodex-local-origin");
  return new URL(request.url).origin !== API_ORIGIN && isLoopbackOrigin(forwarded)
    ? forwarded!
    : undefined;
}

function developmentDialogOrigin(request: Request, origin: string | null): boolean {
  const requestOrigin = new URL(request.url).origin;
  const publicOrigin = localDevelopmentPublicOrigin(request);
  return requestOrigin !== API_ORIGIN
    && (isLoopbackOrigin(origin)
      || (isLocalDevelopmentOrigin(requestOrigin) && origin === requestOrigin)
      || (publicOrigin !== undefined
        && (origin === publicOrigin || origin === localPlaygroundOrigin(publicOrigin))));
}

function localPlaygroundOrigin(publicOrigin: string): string | undefined {
  if (!isLocalDevelopmentOrigin(publicOrigin)) return undefined;
  const url = new URL(publicOrigin);
  if (url.hostname === "nanocodex.localhost") {
    url.hostname = "playground.nanocodex.localhost";
  } else if (url.hostname.endsWith(".nanocodex.localhost")) {
    url.hostname = `playground-${url.hostname.slice(0, -".nanocodex.localhost".length)}.nanocodex.localhost`;
  } else {
    return undefined;
  }
  return url.origin;
}

function allowedOrigin(request: Request, origin: string): boolean {
  return origin === DIALOG_ORIGIN
    || isPublicAppOrigin(origin)
    || developmentDialogOrigin(request, origin);
}

function connectApiRequestOrigin(request: Request): string {
  const publicOrigin = localDevelopmentPublicOrigin(request);
  if (publicOrigin) return publicOrigin;
  const origin = new URL(request.url).origin;
  if (origin === API_ORIGIN || origin === DIALOG_ORIGIN || isLocalDevelopmentOrigin(origin)) {
    return origin;
  }
  throw new ApiFailure(403, "origin_denied", "The Connect API origin is not allowed.");
}

function connectDialogOrigin(request: Request | URL): string {
  if (request instanceof Request) {
    const publicOrigin = localDevelopmentPublicOrigin(request);
    if (publicOrigin) return publicOrigin;
  }
  const origin = request instanceof URL ? request.origin : new URL(request.url).origin;
  return isLocalDevelopmentOrigin(origin) ? origin : DIALOG_ORIGIN;
}

function error(request: Request, status: number, code: string, message: string) {
  return cors(Response.json({ error: { code, message } }, { status }), request);
}
