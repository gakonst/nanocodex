import { DurableObject } from "cloudflare:workers";
import {
  getWorkspace,
  withWorkspace,
  WorkspaceServiceProxy,
  type DurableObjectStorageLike,
} from "@cloudflare/computer";
import type {
  AgentEvent,
  AgentSessionContext,
  EventWatcher,
  PromptInput,
  Turn,
} from "nanocodex";
import { Agent as CloudflareAgent } from "nanocodex/cloudflare";
import { imageGeneration, updatePlan, viewImage, web } from "nanocodex/tools";
import { justBash } from "nanocodex/tools/bash";
import { createComputerFilesystem } from "./computer-workspace";
import {
  managedCapacitySnapshot,
  type ManagedCapacitySnapshot,
} from "./capacity";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";
import { drainRuntimeForDeletion } from "./deletion-runtime";
import {
  createManagedGhCommand,
  createManagedShellFetch,
} from "./computer-shell";
import {
  DurableEventLog,
  EventLogCapacityError,
  MAX_HISTORY_PAGE_SIZE,
  parseCursor,
  type DurableEvent,
} from "./durable-events";
import {
  ManagedEventArchive,
  type ManagedEventSealResult,
} from "./managed-event-archive";
import {
  ManagedTurnArchive,
  type ManagedTurnReceipt,
  type ManagedTurnSealResult,
} from "./managed-turn-archive";
import {
  ManagedRealtimeArchive,
  type ManagedRealtimeReceipt,
  type ManagedRealtimeSealResult,
} from "./managed-realtime-archive";
import { webAsset } from "./web";
import {
  MultiplayerRoom,
  roomCookieName,
} from "./multiplayer-room";
export { MultiplayerRoom } from "./multiplayer-room";
import {
  validateCreateId,
  validateDisplayName,
} from "./multiplayer-protocol";
import {
  MULTIPLAYER_ROOM_LEASE_MS,
  MultiplayerQuota,
} from "./multiplayer-quota";
export { MultiplayerQuota } from "./multiplayer-quota";
export { WorkspaceServiceProxy };

import {
  type ActiveTurn,
  type AgentCapabilities,
  type ClientCommand,
  ProtocolError,
  type ServerMessage,
  parseCommand,
  validatePromptInput,
} from "./protocol";
import {
  DEVICE_HOST_LEASE_MS,
  DEVICE_TOOL_CALL_TIMEOUT_MS,
  DeviceHostAmbiguousError,
  DeviceHostProtocolError,
  deviceToolAmbiguous,
  deviceToolResult,
  deviceToolUnavailable,
  matchesDeviceHostLease,
  parseDeviceHostCommand,
  parseDeviceToolInput,
  type DeviceHostCommand,
  type DeviceHostServerMessage,
} from "./device-host-protocol";
import {
  classifyTurnFailure,
  materializeTurnTerminal,
  type TurnTerminal,
} from "./turn-completion";
import {
  bindAgentCredential,
  routeCredentialRequest,
  unbindAgentCredential,
} from "./credentials";
import { routeBrowserEgress } from "./browser-egress";
import { accountInfo, type AccountInfo, withInitialAccountInfo } from "./account-info";
import { routeConnectorRequest } from "./connectors";
import {
  attachAgent,
  authenticate,
  detachAgent,
  isUserId,
  listAgents,
  recordAgentActivity,
  requireSameOriginMutation,
  routeAccountRequest,
  type AccountAuthEnv,
} from "./account-auth";
import { routeBrowserModel } from "./browser-model";
import { routeAccountLinkRequest } from "./account-links";
import { routeManagedRealtimeTransport } from "./managed-realtime-transport";
export { ApiKeyRecord, NonceStorage, UserAccount } from "./account-auth";
export { MemoryScope, Organization } from "./reserved-durable-objects";

const MAX_CLIENT_MESSAGE_BYTES = 1024 * 1024;
const MAX_ACTIVE_TURNS = 16;
const MAX_CLIENT_CONNECTIONS = 64;
const MAX_REQUEST_BODY_BYTES = 1024 * 1024;
const MAX_REALTIME_REQUEST_BYTES = 64 * 1024;
const MAX_REALTIME_CONTEXT_BYTES = 1024 * 1024;
const MAX_PENDING_REALTIME_OPERATIONS = 32;
const MAX_RETRY_DELAY_MS = 60_000;
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SESSION_ID = UUID;
const ROOM_ROUTE_ID =
  /^([0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})~([A-Za-z0-9_-]{43})$/;
const AGENT_TOKEN = /^[A-Za-z0-9_-]{43}$/;
const TURN_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const IDEMPOTENCY_KEY = /^[\x21-\x7e]{1,256}$/;
const REALTIME_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const encoder = new TextEncoder();
const ENCODED_PONG = JSON.stringify({ type: "pong" });
const SESSION_DELETING_KEY = "nanocodex:session-deleting";
const SESSION_DELETION_GENERATION_KEY = "nanocodex:session-deletion-generation";
const INITIAL_ACCOUNT_CONTEXT_KEY = "nanocodex:initial-account-context";
const CREDENTIAL_BINDING_KEY = "nanocodex:credential-binding";
const CLEANUP_RETRY_ATTEMPT_KEY = "nanocodex:cleanup-retry-attempt";
const CREDENTIAL_BINDING_PREPARE_TIMEOUT_MS = 60_000;
const DEFAULT_OWNERSHIP_IO_TIMEOUT_MS = 10_000;
const DEFAULT_MULTIPLAYER_IO_TIMEOUT_MS = 10_000;
const MAX_CLEANUP_RETRY_MS = 60_000;
const SESSION_OWNER_ASSERTION = "x-nanocodex-owner-id";

export interface Env extends AccountAuthEnv {
  NANOCODEX_SESSIONS: DurableObjectNamespace<NanocodexSession>;
  NANOCODEX_ROOMS: DurableObjectNamespace<MultiplayerRoom>;
  NANOCODEX_MULTIPLAYER_QUOTA: DurableObjectNamespace<MultiplayerQuota>;
  NANOCODEX: Fetcher;
  NANOCODEX_HISTORY: R2Bucket;
  NANOCODEX_ADMIN_TOKEN: string;
  AGENT_IDLE_TIMEOUT_MS?: string;
  MANAGED_MULTIPLAYER_IO_TIMEOUT_MS?: string;
  MANAGED_OWNERSHIP_IO_TIMEOUT_MS?: string;
  MANAGED_EVENT_ARCHIVE_RECENT_EVENTS?: string;
  MANAGED_EVENT_ARCHIVE_SEGMENT_BYTES?: string;
  MANAGED_EVENT_ARCHIVE_THRESHOLD_BYTES?: string;
  MANAGED_TURN_ARCHIVE_RECENT_TURNS?: string;
  MANAGED_REALTIME_ARCHIVE_RECENT_OPERATIONS?: string;
}

type SessionRow = {
  session_id: string;
  owner_id: string;
  public_origin: string;
  runtime_profile: AgentRuntimeProfile;
  completed_turns: number;
  last_active: number;
  stream_error: string | null;
};

type DeviceHostAttachment = {
  kind: "device-host";
  sessionId: string;
  hostId?: string;
  leaseId?: string;
  epoch?: number;
};

type DeviceHostStateRow = {
  epoch: number;
  host_id: string | null;
  catalog_version: number | null;
  lease_id: string | null;
  lease_expires_at: number;
};

type PendingDeviceToolCall = {
  leaseId: string;
  epoch: number;
  deadlineAt: number;
  timeout?: ReturnType<typeof setTimeout>;
  resolve(result: { success: boolean; output: unknown }): void;
  reject(error: Error): void;
};

type SessionInitializationOwnership = {
  session_id: string | null;
  owner_id: string | null;
  runtime_profile: AgentRuntimeProfile | null;
  state: "active" | "deleted";
};

type SessionStatusRow = {
  session_id: string;
  has_snapshot: number;
  completed_turns: number;
  last_active: number;
  stream_error: string | null;
};

type InitialAccountContext = Readonly<{
  turn_id: string;
  account: AccountInfo;
}>;

type ManagedTurnState =
  | "accepted"
  | "cancelling"
  | "retryable"
  | "blocked"
  | "completed"
  | "cancelled"
  | "failed";

type ManagedTurnRow = {
  accepted_at: number | null;
  accepted_cursor: string | null;
  created_at: number;
  error: string | null;
  id: string;
  input_json: string;
  may_have_inner_operation: number;
  request_hash: string;
  request_key: string | null;
  attempt_count: number;
  retry_at: number | null;
  state: ManagedTurnState;
  terminal_cursor: string | null;
  terminal_json: string | null;
  updated_at: number;
};

type StreamMessage = Extract<ServerMessage,
  | { type: "agent_created" }
  | { type: "turn_accepted" }
  | { type: "turn_cancelling" }
  | { type: "turn_completed" }
  | { type: "turn_cancelled" }
  | { type: "turn_retryable" }
  | { type: "turn_blocked" }
  | { type: "turn_failed" }
  | { type: "event" }
  | { type: "stream_failed" }
>;

type ManagedTurnSubmission = {
  created: boolean;
  row: ManagedTurnRow;
};

type ManagedRealtimeKind = "start" | "delegate" | "stop";

type ManagedRealtimeOperationRow = {
  kind: ManagedRealtimeKind;
  operation_id: string;
  request_hash: string;
  response_json: string | null;
  state: "pending" | "completed";
  voice_session_id: string;
};

type ManagedRealtimeRequest = {
  input?: string;
  operationId: string;
  voiceSessionId: string;
};

type ManagedRealtimeSessionRow = {
  voice_session_id: string;
};

type ManagedRealtimeRouteResult = Readonly<{
  operation_id: string;
  route: "started" | "steered";
  turn_id: string;
  voice_session_id: string;
}>;

type ManagedTransition =
  TurnTerminal | Extract<StreamMessage, { type: "turn_cancelling" }>;

type AgentRuntimeProfile = "managed" | "multiplayer";

type AgentConstructionOwnership = {
  readonly deletionGeneration: number;
  readonly runtimeGeneration: number;
  promise: Promise<CloudflareAgent.Agent>;
  publication: Promise<CloudflareAgent.Agent>;
  shutdown?: Promise<void>;
};

type CredentialBindingOwnership = Readonly<{
  cleanup_at: number;
  owner_id: string;
  session_id: string;
  state: "preparing" | "active";
  subject: string;
}>;

type RoomInitializationReceipt = {
  room_id: string;
  invite: string;
  member_id: string;
  member_token: string;
  public_origin: string;
};

const AGENT_CAPABILITIES = Object.freeze({
  durable_turns: true,
  resumable_events: true,
  live_steer: true,
  live_cancel: true,
  workspace: "cloudflare-computer",
  shell_runtime: "just-bash",
  shell_egress: "connector-http-gateway",
  sandbox_escalation: false,
}) satisfies AgentCapabilities;

const json = (body: unknown, init: ResponseInit = {}) => Response.json(body, {
  ...init,
  headers: { "cache-control": "no-store", ...init.headers },
});

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    const browserModel = await routeBrowserModel(request, env, url);
    if (browserModel) return browserModel;
    const realtimeTransport = await routeManagedRealtimeTransport(
      request,
      env,
      url,
      managedOwnershipTimeoutMs(env),
    );
    if (realtimeTransport) return realtimeTransport;
    const accountLink = await routeAccountLinkRequest(request, env, url);
    if (accountLink) return accountLink;
    const account = await routeAccountRequest(request, env, url);
    if (account) return account;
    const credential = await routeCredentialRequest(request, env, url);
    if (credential) return credential;
    const connector = await routeConnectorRequest(request, env, url);
    if (connector) return connector;
    const browserEgress = await routeBrowserEgress(request, env, url);
    if (browserEgress) return browserEgress;
    if (request.method === "GET") {
      const asset = webAsset(url.pathname);
      if (asset) return asset;
    }
    if (request.method === "GET" && url.pathname === "/health") {
      return json({ service: "nanocodex", runtime: "cloudflare-durable-objects", status: "ok" });
    }
    if (request.method === "GET" && url.pathname === "/v1/agents") {
      const principal = await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      const agents = await listAgents(env, principal.userId);
      return json({
        data: agents.map(({ id }) => id),
        summaries: Object.fromEntries(agents.filter(({ createdAt }) => createdAt > 0).map(({ id, ...summary }) => [id, {
          title: summary.title,
          created_at: summary.createdAt,
          updated_at: summary.updatedAt,
          turn_count: summary.turnCount,
        }])),
      });
    }
    if (request.method === "POST" && url.pathname === "/v1/rooms") {
      const principal = await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      return createMultiplayerRoom(request, url, env, principal.userId);
    }
    const roomMatch = url.pathname.match(/^\/v1\/rooms\/([^/]+)(?:\/(join|ws))?$/);
    if (roomMatch) {
      if (!env.NANOCODEX_ADMIN_TOKEN) {
        return json({ error: "multiplayer is not configured" }, { status: 503 });
      }
      const roomId = roomMatch[1]!;
      if (!await validSignedRoomRouteId(env.NANOCODEX_ADMIN_TOKEN, roomId)) {
        return json({ error: "not_found" }, { status: 404 });
      }
      const resource = roomMatch[2];
      const room = env.NANOCODEX_ROOMS.getByName(roomId);
      if (resource === "join") {
        if (request.method !== "POST") return json({ error: "method_not_allowed" }, { status: 405 });
        if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
        const joined = await room.fetch("https://room.internal/join", {
          method: "POST",
          headers: request.headers,
          body: request.body,
        });
        if (!joined.ok) return joined;
        const joinedStatus = joined.status;
        const receipt = await joined.json<{
          room_id: string;
          member_id: string;
          member_token: string;
          public_origin: string;
        }>();
        const publicUrl = new URL(receipt.public_origin);
        const websocketUrl = new URL(`/v1/rooms/${roomId}/ws`, publicUrl);
        websocketUrl.protocol = websocketUrl.protocol === "https:" ? "wss:" : "ws:";
        return json({
          room_id: roomId,
          member_id: receipt.member_id,
          websocket_url: websocketUrl.href,
        }, {
          status: joinedStatus,
          headers: { "set-cookie": roomMemberCookie(roomId, receipt.member_token, publicUrl) },
        });
      }
      if (resource === "ws") {
        if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
          return new Response("Expected WebSocket upgrade", { status: 426 });
        }
        const queryKeys = [...url.searchParams.keys()];
        if (queryKeys.some((key) => key !== "cursor") || url.searchParams.getAll("cursor").length > 1) {
          return json({ error: "invalid_request" }, { status: 400 });
        }
        const cursor = url.searchParams.get("cursor") ?? "0";
        return room.fetch(`https://room.internal/socket?cursor=${encodeURIComponent(cursor)}`, request);
      }
      if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
      if (request.method === "GET") {
        return room.fetch("https://room.internal/state", { headers: request.headers });
      }
      if (request.method === "DELETE") {
        const administrator = Boolean(
          env.NANOCODEX_ADMIN_TOKEN && authorized(request, env.NANOCODEX_ADMIN_TOKEN),
        );
        return room.fetch(
          administrator ? "https://room.internal/admin" : "https://room.internal/room",
          { method: "DELETE", headers: request.headers },
        );
      }
      return json({ error: "method_not_allowed" }, { status: 405 });
    }
    if (request.method === "POST" && url.pathname === "/v1/agents") {
      const principal = await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      const requestKey = request.headers.get("idempotency-key");
      if (requestKey !== null && !IDEMPOTENCY_KEY.test(requestKey)) {
        return json({ error: "invalid_idempotency_key" }, { status: 400 });
      }
      const agentId = requestKey === null
        ? uuidV7()
        : await idempotentAgentId(principal.userId, requestKey);
      const subject = env.NANOCODEX_SESSIONS.idFromName(agentId).toString();
      const stub = env.NANOCODEX_SESSIONS.getByName(agentId);
      const ownershipTimeoutMs = managedOwnershipTimeoutMs(env);
      let prepared: Response;
      try {
        prepared = await fetchCreateStage(stub, "https://session.internal/credential-binding", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            owner_id: principal.userId,
            session_id: agentId,
            subject,
          }),
        }, ownershipTimeoutMs, "agent cleanup preparation", 5);
      } catch {
        return json({ error: "agent cleanup initialization failed" }, { status: 503 });
      }
      await prepared.body?.cancel();
      if (!prepared.ok) {
        if (prepared.status === 409) {
          return json({ error: "agent_creation_expired" }, { status: 409 });
        }
        return json({ error: "agent cleanup initialization failed" }, { status: 503 });
      }
      const [credentialBinding, initialization] = await Promise.allSettled([
        fetchCreateStage(
          stub,
          "https://session.internal/credential-binding/bind",
          { method: "POST" },
          ownershipTimeoutMs,
          "agent credential binding",
        ),
        fetchCreateStage(stub, "https://session.internal/initialize", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            session_id: agentId,
            owner_id: principal.userId,
            public_origin: url.origin,
          }),
        }, ownershipTimeoutMs, "agent initialization"),
      ]);
      if (initialization.status === "fulfilled") {
        await initialization.value.body?.cancel();
      }
      if (credentialBinding.status === "fulfilled") {
        await credentialBinding.value.body?.cancel();
      }
      const credentialUnavailable = credentialBinding.status === "rejected"
        || !credentialBinding.value.ok;
      if (credentialUnavailable
        || initialization.status === "rejected"
        || !initialization.value.ok) {
        // A keyed caller can safely replay this exact AgentDO. Keep the
        // persisted preparation and its watchdog alive instead of racing the
        // replay with deletion. Keyless legacy callers have no identity they
        // can rediscover after a lost response, so compensate immediately.
        if (requestKey === null) await requestSessionCleanup(stub, ownershipTimeoutMs);
        return credentialUnavailable
          ? json({ error: "credential_broker_unavailable" }, { status: 503 })
          : json({ error: "agent initialization failed" }, { status: 503 });
      }
      let committed: Response | undefined;
      try {
        committed = await fetchCreateStage(
          stub,
          "https://session.internal/credential-binding/commit",
          { method: "POST" },
          ownershipTimeoutMs,
          "agent cleanup commit",
          5,
        );
        await committed.body?.cancel();
      } catch { /* The commit may have applied; keyed replay or the watchdog owns resolution. */ }
      if (!committed?.ok) {
        if (requestKey === null) await requestSessionCleanup(stub, ownershipTimeoutMs);
        return json({ error: "agent cleanup commit failed" }, { status: 503 });
      }
      const routeBase = "/v1/agents";
      const websocketUrl = new URL(`${routeBase}/${agentId}/ws`, url);
      websocketUrl.protocol = websocketUrl.protocol === "https:" ? "wss:" : "ws:";
      return json({
        agent_id: agentId,
        session_id: agentId,
        events_url: new URL(`${routeBase}/${agentId}/events`, url).href,
        websocket_url: websocketUrl.href,
      }, {
        status: 201,
      });
    }
    const match = url.pathname.match(/^\/v1\/agents\/([^/]+)(?:\/(.*))?$/);
    if (!match || !SESSION_ID.test(match[1] ?? "")) {
      return json({ error: "not_found" }, { status: 404 });
    }
    const agentId = match[1]!;
    const resource = match[2] ?? "";
    const principal = await authenticate(request, env, url);
    if (!principal) return json({ error: "unauthorized" }, { status: 401 });
    const stub = env.NANOCODEX_SESSIONS.getByName(agentId);
    const sessionHeaders = new Headers(request.headers);
    sessionHeaders.set(SESSION_OWNER_ASSERTION, principal.userId);
    const publicOrigin = `public_origin=${encodeURIComponent(url.origin)}`;
    if (resource === "ws" || resource === "device-host") {
      if (request.method !== "GET" || request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      if (principal.kind === "account_session" && request.headers.get("origin") !== url.origin) {
        return json({ error: "forbidden_origin" }, { status: 403 });
      }
      return stub.fetch(
        `https://session.internal/${resource === "ws" ? "socket" : "device-host"}?${publicOrigin}`,
        new Request(request, { headers: sessionHeaders }),
      );
    }
    if (resource === "events" || resource === "events/history") {
      if (request.method !== "GET") return json({ error: "method_not_allowed" }, { status: 405 });
      const query = new URLSearchParams(url.searchParams);
      query.set("public_origin", url.origin);
      return stub.fetch(`https://session.internal/${resource}?${query}`, {
        headers: sessionHeaders,
        signal: request.signal,
      });
    }
    if (resource === "turns") {
      if (request.method !== "POST")
        return json({ error: "method_not_allowed" }, { status: 405 });
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      const response = await stub.fetch(
        `https://session.internal/turns?${publicOrigin}`,
        {
          method: "POST",
          headers: sessionHeaders,
          body: request.body,
        },
      );
      const created = response.headers.get("x-nanocodex-turn-created") === "1";
      const encodedSummary = response.headers.get("x-nanocodex-turn-summary");
      if (created && encodedSummary !== null) {
        let title = "";
        let turnCount = 0;
        try {
          const summary = JSON.parse(encodedSummary) as {
            title?: unknown;
            turnCount?: unknown;
          };
          if (typeof summary.title === "string") title = summary.title;
          if (
            Number.isSafeInteger(summary.turnCount) &&
            Number(summary.turnCount) >= 0
          ) {
            turnCount = Number(summary.turnCount);
          }
        } catch {
          /* Session-generated value is best effort. */
        }
        if (turnCount > 0) {
          ctx.waitUntil(
            recordAgentActivity(env, principal.userId, agentId, {
              title,
              turnCount,
            }).catch((error) => {
              console.error(
                "managed agent summary update failed",
                errorMessage(error),
              );
            }),
          );
        }
      }
      const headers = new Headers(response.headers);
      headers.delete("x-nanocodex-turn-created");
      headers.delete("x-nanocodex-turn-summary");
      return new Response(response.body, {
        status: response.status,
        statusText: response.statusText,
        headers,
      });
    }
    const realtimeMatch = resource.match(/^realtime\/(start|delegate|stop)$/);
    if (realtimeMatch) {
      if (request.method !== "POST")
        return json({ error: "method_not_allowed" }, { status: 405 });
      if (url.search !== "")
        return json({ error: "invalid_request" }, { status: 400 });
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      return stub.fetch(
        `https://session.internal/realtime/${realtimeMatch[1]}?${publicOrigin}`,
        {
          method: "POST",
          headers: sessionHeaders,
          body: request.body,
        },
      );
    }
    const turnMatch = resource.match(
      /^turns\/([A-Za-z0-9._:-]{1,128})(?:\/(steer|cancel))?$/,
    );
    if (turnMatch) {
      const action = turnMatch[2];
      const expectedMethod = action === undefined ? "GET" : "POST";
      if (request.method !== expectedMethod) {
        return json({ error: "method_not_allowed" }, { status: 405 });
      }
      if (request.method === "POST") {
        const originFailure = requireSameOriginMutation(request, url, principal);
        if (originFailure) return originFailure;
      }
      return stub.fetch(
        `https://session.internal/turns/${turnMatch[1]}${action ? `/${action}` : ""}?${publicOrigin}`,
        {
          method: request.method,
          headers: sessionHeaders,
          ...(request.method === "POST" ? { body: request.body } : {}),
        },
      );
    }
    if (!resource && request.method === "GET") {
      return stub.fetch(
        `https://session.internal/state?${publicOrigin}`,
        { headers: sessionHeaders },
      );
    }
    if (!resource && request.method === "DELETE") {
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      try {
        return await fetchWithDeadline(
          stub,
          "https://session.internal/session",
          { method: "DELETE", headers: sessionHeaders },
          managedOwnershipTimeoutMs(env),
          "agent session deletion",
        );
      } catch {
        return json({ error: "session_cleanup_pending" }, {
          status: 503,
          headers: { "retry-after": "1" },
        });
      }
    }
    return json({ error: "method_not_allowed" }, { status: 405 });
  },
};

class DurableComputerObject extends DurableObject<Env> {
  get computerContext(): DurableObjectState { return this.ctx; }
}

const DurableComputerSession = withWorkspace(
  DurableComputerObject,
  (self) => ({
    storage: self.computerContext.storage as unknown as DurableObjectStorageLike,
    sessionId: self.computerContext.id.toString(),
  }),
);

export class NanocodexSession extends DurableComputerSession {
  #agent?: CloudflareAgent.Agent;
  #agentPromise?: Promise<CloudflareAgent.Agent>;
  #agentConstruction?: AgentConstructionOwnership;
  readonly #agentConstructions = new Set<AgentConstructionOwnership>();
  #agentShutdownPromise?: Promise<void>;
  #events?: EventWatcher;
  readonly #eventLog: DurableEventLog<StreamMessage>;
  readonly #eventArchive: ManagedEventArchive<StreamMessage>;
  #eventArchiveTask?: Promise<ManagedEventSealResult>;
  readonly #turnArchive: ManagedTurnArchive;
  #turnArchiveTask?: Promise<ManagedTurnSealResult>;
  readonly #realtimeArchive: ManagedRealtimeArchive;
  #realtimeArchiveTask?: Promise<ManagedRealtimeSealResult>;
  readonly #turns = new Map<string, Turn>();
  readonly #reopenInterruptedTurnIds = new Set<string>();
  readonly #eventTurnQueue: string[] = [];
  #eventTurnId?: string;
  readonly #pendingTurnIds = new Set<string>();
  readonly #turnInputs = new Map<string, PromptInput>();
  readonly #admissionTasks = new Map<string, Promise<ManagedTurnRow>>();
  #initialAccountContextTask?: Promise<InitialAccountContext | undefined>;
  readonly #cancellationTasks = new Map<string, Promise<void>>();
  readonly #pendingDeviceToolCalls = new Map<string, PendingDeviceToolCall>();
  readonly #realtimeOperations = new Map<string, Promise<unknown>>();
  #realtimeOperationTail: Promise<void> = Promise.resolve();
  readonly #inFlight = new Set<Promise<unknown>>();
  #realtimeEventBuffer?: AgentEvent[];
  #realtimeRouteTail: Promise<void> = Promise.resolve();
  #recoveryTask?: Promise<void>;
  #recoveryRequested = false;
  #streamError?: string;
  #deleting = false;
  #deleted = false;
  #credentialBinding?: CredentialBindingOwnership;
  #deletionMarkerTask?: Promise<void>;
  #deletionTask?: Promise<void>;
  #deletionGeneration = 0;
  #runtimeOwnershipGeneration = 0;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.ctx.storage.sql.exec(`
      DROP TABLE IF EXISTS terminal_turns;
      DROP TABLE IF EXISTS completed_operations;
      CREATE TABLE IF NOT EXISTS session_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        session_id TEXT NOT NULL UNIQUE,
        owner_id TEXT NOT NULL,
        public_origin TEXT NOT NULL DEFAULT '',
        runtime_profile TEXT NOT NULL DEFAULT 'managed' CHECK (runtime_profile IN ('managed', 'multiplayer')),
        accepted_turns INTEGER NOT NULL DEFAULT 0 CHECK (accepted_turns >= 0),
        completed_turns INTEGER NOT NULL DEFAULT 0,
        first_prompt TEXT NOT NULL DEFAULT '',
        stream_error TEXT,
        last_active INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS session_initialization_ownership (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        session_id TEXT,
        owner_id TEXT,
        runtime_profile TEXT CHECK (runtime_profile IN ('managed', 'multiplayer')),
        state TEXT NOT NULL CHECK (state IN ('active', 'deleted'))
      );
      CREATE TABLE IF NOT EXISTS managed_turns (
        id TEXT PRIMARY KEY,
        request_key TEXT,
        request_hash TEXT NOT NULL,
        input_json TEXT NOT NULL,
        state TEXT NOT NULL CHECK (
          state IN ('accepted', 'cancelling', 'retryable', 'blocked', 'completed', 'cancelled', 'failed')
        ),
        accepted_cursor INTEGER NOT NULL,
        terminal_json TEXT,
        terminal_cursor INTEGER,
        error TEXT,
        may_have_inner_operation INTEGER NOT NULL DEFAULT 1 CHECK (may_have_inner_operation IN (0, 1)),
        attempt_count INTEGER NOT NULL DEFAULT 0,
        retry_at INTEGER,
        created_at INTEGER NOT NULL,
        accepted_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE UNIQUE INDEX IF NOT EXISTS managed_turns_request_key
        ON managed_turns(request_key) WHERE request_key IS NOT NULL;
      CREATE TABLE IF NOT EXISTS managed_realtime_operations (
        voice_session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('start', 'delegate', 'stop')),
        request_hash TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
        response_json TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (voice_session_id, operation_id)
      );
      CREATE TABLE IF NOT EXISTS managed_realtime_session (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        voice_session_id TEXT NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS device_host_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        epoch INTEGER NOT NULL DEFAULT 0,
        host_id TEXT,
        catalog_version INTEGER,
        lease_id TEXT,
        lease_expires_at INTEGER NOT NULL DEFAULT 0
      );
      INSERT OR IGNORE INTO device_host_state (singleton) VALUES (1);
      CREATE TABLE IF NOT EXISTS device_tool_calls (
        call_id TEXT PRIMARY KEY,
        lease_id TEXT NOT NULL,
        epoch INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('dispatched', 'completed', 'ambiguous')),
        operation TEXT NOT NULL,
        arguments_json TEXT NOT NULL,
        result_json TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      UPDATE device_tool_calls
      SET state = 'ambiguous',
          result_json = '{"ok":false,"status":"ambiguous","message":"device host lifecycle restarted after dispatch"}',
          updated_at = unixepoch('subsec') * 1000
      WHERE state = 'dispatched';
    `);
    this.#eventLog = new DurableEventLog<StreamMessage>(this.ctx.storage);
    this.#eventArchive = new ManagedEventArchive<StreamMessage>(
      this.ctx.storage,
      this.env.NANOCODEX_HISTORY,
      this.ctx.id.toString(),
      {
        recentEventCount: optionalPositiveInteger(this.env.MANAGED_EVENT_ARCHIVE_RECENT_EVENTS),
        sealThresholdBytes: optionalPositiveInteger(this.env.MANAGED_EVENT_ARCHIVE_THRESHOLD_BYTES),
        segmentTargetBytes: optionalPositiveInteger(this.env.MANAGED_EVENT_ARCHIVE_SEGMENT_BYTES),
      },
    );
    this.#turnArchive = new ManagedTurnArchive(
      this.ctx.storage,
      this.env.NANOCODEX_HISTORY,
      this.ctx.id.toString(),
      optionalPositiveInteger(this.env.MANAGED_TURN_ARCHIVE_RECENT_TURNS),
    );
    this.#realtimeArchive = new ManagedRealtimeArchive(
      this.ctx.storage,
      this.env.NANOCODEX_HISTORY,
      this.ctx.id.toString(),
      optionalPositiveInteger(this.env.MANAGED_REALTIME_ARCHIVE_RECENT_OPERATIONS),
    );
    const sessionColumns = new Set(
      this.ctx.storage.sql
        .exec<{ name: string }>("PRAGMA table_info(session_state)")
        .toArray()
        .map((column) => column.name),
    );
    if (!sessionColumns.has("public_origin")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE session_state ADD COLUMN public_origin TEXT NOT NULL DEFAULT ''",
      );
    }
    if (!sessionColumns.has("owner_id")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE session_state ADD COLUMN owner_id TEXT NOT NULL DEFAULT ''",
      );
    }
    if (!sessionColumns.has("stream_error")) {
      this.ctx.storage.sql.exec("ALTER TABLE session_state ADD COLUMN stream_error TEXT");
    }
    if (!sessionColumns.has("runtime_profile")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE session_state ADD COLUMN runtime_profile TEXT NOT NULL DEFAULT 'managed'",
      );
    }
    if (!sessionColumns.has("accepted_turns")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE session_state ADD COLUMN accepted_turns INTEGER NOT NULL DEFAULT 0 CHECK (accepted_turns >= 0)",
      );
      this.ctx.storage.sql.exec(
        "UPDATE session_state SET accepted_turns = (SELECT COUNT(*) FROM managed_turns)",
      );
    }
    if (!sessionColumns.has("first_prompt")) {
      this.ctx.storage.sql.exec(
        "ALTER TABLE session_state ADD COLUMN first_prompt TEXT NOT NULL DEFAULT ''",
      );
      const retainedFirstPrompt = this.ctx.storage.sql.exec<{ input_json: string }>(
        "SELECT input_json FROM managed_turns ORDER BY created_at, id LIMIT 1",
      ).toArray()[0]?.input_json;
      if (retainedFirstPrompt !== undefined) {
        try {
          this.ctx.storage.sql.exec(
            "UPDATE session_state SET first_prompt = ? WHERE singleton = 1",
            promptInputText(JSON.parse(retainedFirstPrompt) as PromptInput),
          );
        } catch { /* Invalid retained input fails through its normal recovery path. */ }
      }
    }
    const managedTurnColumns = new Set(this.ctx.storage.sql.exec<{ name: string }>(
      "PRAGMA table_info(managed_turns)",
    ).toArray().map((column) => column.name));
    if (!managedTurnColumns.has("may_have_inner_operation")) {
      // Pre-upgrade unfinished rows may already own an inner Rust journal
      // operation. Conservatively replay them instead of orphaning that work.
      this.ctx.storage.sql.exec(
        "ALTER TABLE managed_turns ADD COLUMN may_have_inner_operation INTEGER NOT NULL DEFAULT 1 CHECK (may_have_inner_operation IN (0, 1))",
      );
    }
    this.#deleted = this.#initializationOwnership()?.state === "deleted";
    this.#streamError = this.#session()?.stream_error ?? undefined;
    this.ctx.blockConcurrencyWhile(async () => {
      const [deleting, credentialBinding, deletionGeneration] = await Promise.all([
        this.ctx.storage.get<boolean>(SESSION_DELETING_KEY),
        this.ctx.storage.get<CredentialBindingOwnership>(CREDENTIAL_BINDING_KEY),
        this.ctx.storage.get<number>(SESSION_DELETION_GENERATION_KEY),
      ]);
      this.#deleting = deleting === true;
      this.#credentialBinding = credentialBinding;
      this.#deletionGeneration = deletionGeneration ?? 0;
      // Durable state and SSE replay are immediately usable after eviction.
      // Re-admission or deletion may load external resources, so neither sits
      // on the object's request-readiness boundary.
      if (this.#deleting) this.#scheduleDeletion();
      else this.#scheduleRecovery();
    });
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const ownerAssertion = request.headers.get(SESSION_OWNER_ASSERTION);
    if (ownerAssertion !== null && ownerAssertion !== this.#session()?.owner_id) {
      return json({ error: "not_found" }, { status: 404 });
    }
    if (request.method === "PUT" && url.pathname === "/credential-binding") {
      if (this.#deleting || this.#deleted) return new Response(null, { status: 409 });
      let ownership: Partial<CredentialBindingOwnership>;
      try { ownership = await request.json<Partial<CredentialBindingOwnership>>(); }
      catch { return new Response(null, { status: 400 }); }
      if (!isUserId(ownership.owner_id)
        || typeof ownership.session_id !== "string"
        || !SESSION_ID.test(ownership.session_id)
        || typeof ownership.subject !== "string"
        || ownership.subject !== this.ctx.id.toString()) {
        return new Response(null, { status: 400 });
      }
      const current = this.#credentialBinding;
      if (current && (current.owner_id !== ownership.owner_id
        || current.session_id !== ownership.session_id
        || current.subject !== ownership.subject)) {
        return new Response(null, { status: 409 });
      }
      if (!current) {
        const prepared: CredentialBindingOwnership = {
          cleanup_at: Date.now() + this.#credentialPreparationLeaseMs(),
          owner_id: ownership.owner_id,
          session_id: ownership.session_id,
          state: "preparing",
          subject: ownership.subject,
        };
        await this.ctx.storage.transaction(async (transaction) => {
          await transaction.put(CREDENTIAL_BINDING_KEY, prepared);
          await transaction.setAlarm(prepared.cleanup_at);
        });
        this.#credentialBinding = prepared;
      } else if (current.state === "preparing") {
        const refreshed = {
          ...current,
          cleanup_at: Date.now() + this.#credentialPreparationLeaseMs(),
        };
        await this.ctx.storage.transaction(async (transaction) => {
          await transaction.put(CREDENTIAL_BINDING_KEY, refreshed);
          await transaction.setAlarm(refreshed.cleanup_at);
        });
        this.#credentialBinding = refreshed;
      }
      return new Response(null, { status: 204 });
    }
    if (request.method === "POST" && url.pathname === "/credential-binding/bind") {
      const ownership = await this.#refreshCredentialPreparation();
      if (!ownership || this.#deleting || this.#deleted) {
        return new Response(null, { status: 409 });
      }
      try {
        await this.#track(bindAgentCredential(
          this.env.NANOCODEX,
          ownership.subject,
          ownership.owner_id,
          this.#ownershipIoTimeoutMs(),
        ));
      } catch {
        return new Response(null, { status: 503 });
      }
      return new Response(null, { status: this.#deleting || this.#deleted ? 409 : 204 });
    }
    if (request.method === "POST" && url.pathname === "/credential-binding/commit") {
      if (this.#deleting || this.#deleted) return new Response(null, { status: 409 });
      const ownership = await this.#refreshCredentialPreparation();
      const session = this.#session();
      if (!ownership || !session
        || ownership.owner_id !== session.owner_id
        || ownership.session_id !== session.session_id) {
        return new Response(null, { status: 409 });
      }
      try {
        await this.#track(attachAgent(
          this.env,
          ownership.owner_id,
          ownership.session_id,
          this.#ownershipIoTimeoutMs(),
        ));
      } catch {
        return new Response(null, { status: 503 });
      }
      if (this.#deleting || this.#deleted) return new Response(null, { status: 409 });
      if (ownership.state !== "active") {
        const active = { ...ownership, state: "active" as const };
        await this.ctx.storage.put(CREDENTIAL_BINDING_KEY, active);
        this.#credentialBinding = active;
      }
      await this.#scheduleNextAlarm();
      return new Response(null, { status: 204 });
    }
    const forwardedOrigin = url.searchParams.get("public_origin");
    if (!this.#deleting
      && forwardedOrigin !== null
      && validPublicOrigin(forwardedOrigin)
      && this.#sessionId()) {
      this.ctx.storage.sql.exec(
        "UPDATE session_state SET public_origin = ? WHERE singleton = 1",
        forwardedOrigin,
      );
    }
    if (request.method === "PUT" && url.pathname === "/initialize") {
      if (this.#deleting || this.#deleted) return new Response(null, { status: 409 });
      const body = await request.text();
      if (this.#deleting || this.#deleted) return new Response(null, { status: 409 });
      if (body.length > 2048) return new Response(null, { status: 400 });
      let initialization: {
        session_id?: unknown;
        owner_id?: unknown;
        public_origin?: unknown;
        runtime_profile?: unknown;
      };
      try {
        initialization = JSON.parse(body) as typeof initialization;
      } catch {
        return new Response(null, { status: 400 });
      }
      const sessionId = initialization.session_id;
      const ownerId = initialization.owner_id;
      const publicOrigin = initialization.public_origin;
      const runtimeProfile = initialization.runtime_profile ?? "managed";
      if (typeof sessionId !== "string"
        || !SESSION_ID.test(sessionId)
        || !isUserId(ownerId)
        || typeof publicOrigin !== "string"
        || !validPublicOrigin(publicOrigin)
        || (runtimeProfile !== "managed" && runtimeProfile !== "multiplayer")) {
        return new Response(null, { status: 400 });
      }
      const credentialBinding = this.#credentialBinding;
      if (runtimeProfile === "managed" && (!credentialBinding
        || credentialBinding.owner_id !== ownerId
        || credentialBinding.session_id !== sessionId
        || credentialBinding.subject !== this.ctx.id.toString())) {
        return new Response(null, { status: 409 });
      }
      const current = this.#session();
      const currentId = current?.session_id;
      if (currentId && currentId !== sessionId) return new Response(null, { status: 409 });
      if (current && current.owner_id !== ownerId) return new Response(null, { status: 409 });
      if (current && current.runtime_profile !== runtimeProfile) return new Response(null, { status: 409 });
      let event: DurableEvent<StreamMessage> | undefined;
      try {
        this.ctx.storage.transactionSync(() => {
          const ownership = this.#initializationOwnership();
          if (this.#deleting || this.#deleted || ownership?.state === "deleted") {
            throw new ManagedRequestError(
              409,
              "agent_deleting",
              "the agent is being deleted or was already deleted",
            );
          }
          if (ownership && (ownership.session_id !== sessionId
            || ownership.owner_id !== ownerId
            || ownership.runtime_profile !== runtimeProfile)) {
            throw new ManagedRequestError(
              409,
              "agent_initialized",
              "the one-shot initialization ownership belongs to another session",
            );
          }
          if (!ownership) {
            this.ctx.storage.sql.exec(
              `INSERT INTO session_initialization_ownership (
                 singleton, session_id, owner_id, runtime_profile, state
               ) VALUES (1, ?, ?, ?, 'active')`,
              sessionId,
              ownerId,
              runtimeProfile,
            );
          }
          const retained = this.#session();
          if (retained && (retained.session_id !== sessionId
            || retained.owner_id !== ownerId
            || retained.runtime_profile !== runtimeProfile)) {
            throw new ManagedRequestError(
              409,
              "agent_initialized",
              "the agent is already initialized with different ownership",
            );
          }
          if (retained) {
            this.ctx.storage.sql.exec(
              "UPDATE session_state SET public_origin = ? WHERE singleton = 1",
              publicOrigin,
            );
            return;
          }
          this.ctx.storage.sql.exec(
            `INSERT INTO session_state
               (singleton, session_id, owner_id, public_origin, runtime_profile, last_active)
             VALUES (1, ?, ?, ?, ?, ?)`,
            sessionId,
            ownerId,
            publicOrigin,
            runtimeProfile,
            Date.now(),
          );
          event = this.#eventLog.append({
            type: "agent_created",
            agent_id: sessionId,
            capabilities: this.#capabilities(),
          }, null, true);
        });
      } catch (error) {
        if (error instanceof ManagedRequestError) {
          return new Response(null, { status: error.status });
        }
        throw error;
      }
      if (event) this.#publish(event);
      return new Response(null, { status: 204 });
    }
    if (request.method === "GET" && url.pathname === "/socket")
      return this.#upgrade();
    if (request.method === "GET" && url.pathname === "/device-host")
      return this.#upgradeDeviceHost();
    const realtimeRoute = url.pathname.match(
      /^\/realtime\/(start|delegate|stop)$/,
    );
    if (realtimeRoute) {
      if (ownerAssertion === null)
        return json({ error: "not_found" }, { status: 404 });
      if (request.method !== "POST")
        return json({ error: "method_not_allowed" }, { status: 405 });
      return this.#managedRealtime(
        realtimeRoute[1] as ManagedRealtimeKind,
        request,
      );
    }
    if (request.method === "GET" && url.pathname === "/events") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      if (!this.#sessionId())
        return json({ error: "not_found" }, { status: 404 });
      const requested =
        request.headers.get("last-event-id") ??
        url.searchParams.get("cursor") ??
        url.searchParams.get("after");
      const cursor =
        requested === "latest"
          ? this.#eventArchive.latestCursor(this.#eventLog)
          : parseCursor(requested);
      if (cursor === undefined)
        return json({ error: "invalid_cursor" }, { status: 400 });
      return this.#eventLog.streamWithPage(
        cursor,
        this.#eventArchive.latestCursor(this.#eventLog),
        this.#eventArchive.pageReader(this.#eventLog),
        request.signal,
      );
    }
    if (request.method === "GET" && url.pathname === "/events/history") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      if (!this.#sessionId())
        return json({ error: "not_found" }, { status: 404 });
      const requestedBefore = url.searchParams.get("before");
      const before =
        requestedBefore === null ? undefined : parseCursor(requestedBefore);
      const requestedLimit = url.searchParams.get("limit") ?? "128";
      if (
        (requestedBefore !== null &&
          (before === undefined || before === "0")) ||
        !/^[1-9][0-9]*$/.test(requestedLimit)
      ) {
        return json({ error: "invalid_history_page" }, { status: 400 });
      }
      const limit = Number(requestedLimit);
      if (!Number.isSafeInteger(limit) || limit > MAX_HISTORY_PAGE_SIZE) {
        return json({ error: "invalid_history_page" }, { status: 400 });
      }
      let page;
      try {
        page = await this.#eventArchive.history(this.#eventLog, before, limit);
      } catch (error) {
        return json({
          error: "event_archive_unavailable",
          message: errorMessage(error),
        }, {
          status: 503,
          headers: { "cache-control": "no-store", "retry-after": "1" },
        });
      }
      return json({
        data: page.data.map((event) => ({
          cursor: event.cursor,
          created_at: event.created_at,
          turn_id: event.turn_id,
          ...event.message,
        })),
        has_more: page.has_more,
        latest_cursor: page.latest_cursor,
      }, { headers: { "cache-control": "no-store" } });
    }
    if (request.method === "POST" && url.pathname === "/events/archive") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      if (!this.#sessionId())
        return json({ error: "not_found" }, { status: 404 });
      return json(await this.#sealEventArchive(true), {
        headers: { "cache-control": "no-store" },
      });
    }
    if (request.method === "GET" && url.pathname === "/capacity") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      const sessionId = this.#sessionId();
      if (!sessionId)
        return json({ error: "not_found" }, { status: 404 });
      return json(managedCapacitySnapshot(
        this.ctx.storage,
        sessionId,
        this.#eventArchive.capacity(),
        this.#turnArchive.capacity(),
        this.#realtimeArchive.capacity(),
      ), {
        headers: { "cache-control": "no-store" },
      });
    }
    if (request.method === "POST" && url.pathname === "/turns") {
      return this.#submitHttpTurn(request);
    }
    if (request.method === "POST" && url.pathname === "/turns/archive") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      if (!this.#sessionId())
        return json({ error: "not_found" }, { status: 404 });
      return json(await this.#sealTurnArchive(true), {
        headers: { "cache-control": "no-store" },
      });
    }
    if (request.method === "POST" && url.pathname === "/realtime/archive") {
      if (this.#deleting)
        return json({ error: "agent_deleting" }, { status: 409 });
      if (!this.#sessionId())
        return json({ error: "not_found" }, { status: 404 });
      return json(await this.#sealRealtimeArchive(true), {
        headers: { "cache-control": "no-store" },
      });
    }
    const turnRoute = url.pathname.match(/^\/turns\/([A-Za-z0-9._:-]{1,128})(?:\/(steer|cancel))?$/);
    if (turnRoute) {
      if (this.#deleting) return json({ error: "agent_deleting" }, { status: 409 });
      const turnId = turnRoute[1]!;
      if (request.method === "GET" && turnRoute[2] === undefined) {
        try {
          const row = await this.#findManagedTurn(turnId);
          return row ? json(managedTurnView(row)) : json({ error: "turn_not_found" }, { status: 404 });
        } catch (error) {
          return managedErrorResponse(error, "turn_archive_unavailable");
        }
      }
      if (request.method === "POST" && turnRoute[2] === "steer") {
        return this.#steerHttpTurn(turnId, request);
      }
      if (request.method === "POST" && turnRoute[2] === "cancel") {
        return this.#cancelHttpTurn(turnId);
      }
      return json({ error: "method_not_allowed" }, { status: 405 });
    }
    if (request.method === "GET" && url.pathname === "/state") {
      if (this.#deleting) return json({ error: "agent_deleting" }, { status: 409 });
      const session = this.#sessionStatus();
      if (!session) return json({ error: "not_found" }, { status: 404 });
      return json({
        agent_id: session.session_id,
        session_id: session.session_id,
        has_snapshot: session.has_snapshot !== 0,
        completed_turns: session.completed_turns,
        first_prompt: this.#firstPrompt(),
        last_active: session.last_active,
        active_turns: this.#activeTurnIds(),
        active_turn_details: this.#activeTurnDetails(),
        agent_loaded: this.#agent !== undefined,
        connected_clients: this.ctx.getWebSockets().length,
        capabilities: this.#capabilities(),
        latest_event_cursor: this.#eventArchive.latestCursor(this.#eventLog),
        stream_error: session.stream_error,
      });
    }
    if (request.method === "DELETE" && url.pathname === "/session") {
      try {
        if (this.#deleted && !this.#deleting && !this.#sessionId() && !this.#credentialBinding) {
          return new Response(null, { status: 204 });
        }
        await this.#beginDeletion();
        await this.#deleteOwnedSession();
      } catch (error) {
        console.error("managed session cleanup remains pending", errorMessage(error));
        let retryAfter = 1;
        try {
          retryAfter = Math.ceil(await this.#scheduleCleanupRetry() / 1_000);
        } catch { /* Durable marker retains ownership. */ }
        return json({ error: "session_cleanup_pending" }, {
          status: 503,
          headers: { "retry-after": String(retryAfter) },
        });
      }
      return new Response(null, { status: 204 });
    }
    return json({ error: "not_found" }, { status: 404 });
  }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (typeof message !== "string") {
      this.#send(socket, { type: "error", code: "binary_unsupported", message: "text frames are required" });
      return;
    }
    if (message.length > MAX_CLIENT_MESSAGE_BYTES
      || encoder.encode(message).byteLength > MAX_CLIENT_MESSAGE_BYTES) {
      closeSocket(socket, 1009, "message exceeds 1 MiB");
      return;
    }
    const attachment = socket.deserializeAttachment() as DeviceHostAttachment | { sessionId?: string } | null;
    if (attachment && "kind" in attachment && attachment.kind === "device-host") {
      await this.#dispatchDeviceHost(socket, attachment, message);
      return;
    }
    let command: ClientCommand;
    try {
      command = parseCommand(message);
    } catch (error) {
      const protocol = error instanceof ProtocolError ? error : new ProtocolError("invalid_message", errorMessage(error));
      this.#send(socket, { type: "error", code: protocol.code, message: protocol.message });
      return;
    }
    await this.#dispatch(socket, command);
  }

  webSocketClose(socket: WebSocket, code: number, reason: string): void {
    this.#retireDeviceHost(socket, reason || "peer closed");
    closeSocket(socket, code, reason || "peer closed");
  }

  webSocketError(socket: WebSocket): void {
    this.#retireDeviceHost(socket, "WebSocket failed");
    closeSocket(socket, 1011, "WebSocket failed");
  }

  async alarm(): Promise<void> {
    if (this.#deleting) {
      try {
        await this.#deleteOwnedSession();
      } catch (error) {
        console.error("managed session alarm cleanup remains pending", errorMessage(error));
        await this.#scheduleCleanupRetry();
      }
      return;
    }
    const credentialBinding = this.#credentialBinding;
    if (credentialBinding?.state === "preparing") {
      if (credentialBinding.cleanup_at > Date.now()) {
        await this.ctx.storage.setAlarm(credentialBinding.cleanup_at);
        return;
      }
      await this.#beginDeletion();
      try {
        await this.#deleteOwnedSession();
      } catch (error) {
        console.error("abandoned managed create cleanup remains pending", errorMessage(error));
        await this.#scheduleCleanupRetry();
      }
      return;
    }
    // An alarm may be the first event delivered to a freshly reconstructed
    // object. In-memory admission ownership is empty in that case even though
    // SQLite still contains accepted work. Never let the idle path fence the
    // recovery task that constructor startup (or this alarm) is about to run.
    if (this.#turns.size > 0 || this.#pendingTurnIds.size > 0 || this.#agentPromise) {
      this.#scheduleRecovery();
      await this.#scheduleNextAlarm();
      return;
    }
    if (this.#recoverableTurnCount() > 0) {
      // Recovery remains the sole owner of a retained retry_at and installs
      // the next alarm from the same ordered pass that evaluates that row.
      this.#scheduleRecovery();
      return;
    }
    this.#logCapacity("idle_shutdown");
    while (this.#eventArchive.needsSeal(this.#eventLog)) {
      if (!(await this.#sealEventArchive(false)).sealed) break;
    }
    while (this.#turnArchive.needsSeal()) {
      if (!(await this.#sealTurnArchive(false)).sealed) break;
    }
    while (this.#realtimeArchive.needsSeal()) {
      if (!(await this.#sealRealtimeArchive(false)).sealed) break;
    }
    await this.#shutdownAgent();
    this.#scheduleRecovery();
  }

  #upgrade(): Response {
    if (this.#deleting) return new Response("Agent is being deleted", { status: 409 });
    const session = this.#sessionStatus();
    if (!session) return new Response("Unknown session", { status: 404 });
    if (this.ctx.getWebSockets("client").length >= MAX_CLIENT_CONNECTIONS) {
      return new Response("Session client limit reached", { status: 429 });
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.serializeAttachment({ sessionId: session.session_id });
    this.ctx.acceptWebSocket(server, ["client"]);
    this.#send(server, {
      type: "ready",
      session_id: session.session_id,
      restored: session.has_snapshot !== 0,
      active_turns: this.#activeTurnIds(),
      active_turn_details: this.#activeTurnDetails(),
      capabilities: this.#capabilities(),
    });
    return new Response(null, { status: 101, webSocket: client });
  }

  #upgradeDeviceHost(): Response {
    if (this.#deleting) return new Response("Agent is being deleted", { status: 409 });
    const session = this.#sessionStatus();
    if (!session) return new Response("Unknown session", { status: 404 });
    if (this.#session()?.runtime_profile !== "managed") {
      return new Response("Device hosting is unavailable for multiplayer agents", { status: 409 });
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.serializeAttachment({
      kind: "device-host",
      sessionId: session.session_id,
    } satisfies DeviceHostAttachment);
    this.ctx.acceptWebSocket(server, ["device-host"]);
    return new Response(null, { status: 101, webSocket: client });
  }

  async #dispatchDeviceHost(
    socket: WebSocket,
    attachment: DeviceHostAttachment,
    encoded: string,
  ): Promise<void> {
    let command: DeviceHostCommand;
    try {
      command = parseDeviceHostCommand(encoded);
    } catch (error) {
      const protocol = error instanceof DeviceHostProtocolError
        ? error
        : new DeviceHostProtocolError("invalid_message", errorMessage(error));
      this.#sendDeviceHost(socket, { type: "error", code: protocol.code, message: protocol.message });
      return;
    }
    try {
      if (command.type === "attach") {
        this.#claimDeviceHost(socket, attachment, command.host_id, command.catalog_version);
        return;
      }
      this.#requireDeviceHostLease(socket, attachment, command.lease_id, command.epoch);
      if (command.type === "ping") {
        this.#renewDeviceHostLease(socket, command);
      } else {
        this.#completeDeviceToolCall(socket, command);
      }
    } catch (error) {
      const protocol = error instanceof DeviceHostProtocolError
        ? error
        : new DeviceHostProtocolError("device_host_failed", errorMessage(error));
      if (protocol.code !== "stale_lease") {
        this.#sendDeviceHost(socket, { type: "error", code: protocol.code, message: protocol.message });
      }
    }
  }

  #claimDeviceHost(
    socket: WebSocket,
    attachment: DeviceHostAttachment,
    hostId: string,
    catalogVersion: number,
  ): void {
    if (attachment.hostId || attachment.leaseId || attachment.epoch) {
      throw new DeviceHostProtocolError("already_attached", "this socket already holds a device-host lease");
    }
    const current = this.#deviceHostState();
    if (current.epoch >= Number.MAX_SAFE_INTEGER) {
      throw new DeviceHostProtocolError("lease_exhausted", "the device-host lease epoch is exhausted");
    }
    const epoch = current.epoch + 1;
    const leaseId = crypto.randomUUID();
    const expiresAt = Date.now() + DEVICE_HOST_LEASE_MS;
    this.ctx.storage.sql.exec(
      `UPDATE device_host_state
       SET epoch = ?, host_id = ?, catalog_version = ?, lease_id = ?, lease_expires_at = ?
       WHERE singleton = 1`,
      epoch,
      hostId,
      catalogVersion,
      leaseId,
      expiresAt,
    );
    for (const candidate of this.ctx.getWebSockets("device-host")) {
      if (candidate === socket) continue;
      const candidateAttachment = candidate.deserializeAttachment() as DeviceHostAttachment | null;
      if (candidateAttachment?.kind !== "device-host" || !candidateAttachment.leaseId) continue;
      try {
        this.#sendDeviceHost(candidate, {
          type: "fenced",
          epoch,
          reason: "a newer Android device host acquired the agent lease",
        });
      } catch { /* Closing the old socket is itself the authoritative fence. */ }
      this.#retireDeviceHost(candidate, "replaced by a newer device host");
      closeSocket(candidate, 1008, "device-host lease replaced");
    }
    socket.serializeAttachment({
      ...attachment,
      hostId,
      leaseId,
      epoch,
    } satisfies DeviceHostAttachment);
    try {
      this.#sendDeviceHost(socket, {
        type: "lease",
        protocol_version: 1,
        lease_id: leaseId,
        epoch,
        expires_at: expiresAt,
        catalog_version: catalogVersion,
      });
    } catch {
      this.#retireDeviceHost(socket, "lease delivery failed");
      closeSocket(socket, 1011, "device-host lease delivery failed");
    }
  }

  #requireDeviceHostLease(
    socket: WebSocket,
    attachment: DeviceHostAttachment,
    leaseId: string,
    epoch: number,
  ): DeviceHostStateRow {
    const state = this.#deviceHostState();
    if (attachment.leaseId !== leaseId
      || attachment.epoch !== epoch
      || !matchesDeviceHostLease(attachment, state, Date.now())) {
      try {
        this.#sendDeviceHost(socket, {
          type: "fenced",
          epoch: state.epoch,
          reason: "the device-host lease is stale or expired",
        });
      } catch { /* Closing the stale socket is itself the authoritative fence. */ }
      this.#retireDeviceHost(socket, "stale or expired lease");
      closeSocket(socket, 1008, "stale device-host lease");
      throw new DeviceHostProtocolError("stale_lease", "the device-host lease is stale or expired");
    }
    return state;
  }

  #renewDeviceHostLease(
    socket: WebSocket,
    command: Extract<DeviceHostCommand, { type: "ping" }>,
  ): void {
    const expiresAt = Date.now() + DEVICE_HOST_LEASE_MS;
    this.ctx.storage.sql.exec(
      `UPDATE device_host_state SET lease_expires_at = ?
       WHERE singleton = 1 AND lease_id = ? AND epoch = ?`,
      expiresAt,
      command.lease_id,
      command.epoch,
    );
    this.#sendDeviceHost(socket, {
      type: "pong",
      lease_id: command.lease_id,
      epoch: command.epoch,
      expires_at: expiresAt,
      ...(command.nonce === undefined ? {} : { nonce: command.nonce }),
    });
  }

  #completeDeviceToolCall(
    socket: WebSocket,
    command: Extract<DeviceHostCommand, { type: "device_tool_result" }>,
  ): void {
    const pending = this.#pendingDeviceToolCalls.get(command.call_id);
    if (!pending || pending.leaseId !== command.lease_id || pending.epoch !== command.epoch) {
      throw new DeviceHostProtocolError("unknown_call", "device tool call is not pending for this lease");
    }
    const stored = JSON.stringify(deviceToolResult(command.success, command.output));
    this.ctx.storage.sql.exec(
      `UPDATE device_tool_calls
       SET state = 'completed', result_json = ?, updated_at = ?
       WHERE call_id = ? AND lease_id = ? AND epoch = ? AND state = 'dispatched'`,
      stored,
      Date.now(),
      command.call_id,
      command.lease_id,
      command.epoch,
    );
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    this.#pendingDeviceToolCalls.delete(command.call_id);
    pending.resolve({ success: command.success, output: command.output });
    this.#sendDeviceHost(socket, {
      type: "ack",
      lease_id: command.lease_id,
      epoch: command.epoch,
      call_id: command.call_id,
      state: "completed",
    });
  }

  #retireDeviceHost(socket: WebSocket, reason: string): void {
    const attachment = socket.deserializeAttachment() as DeviceHostAttachment | null;
    if (attachment?.kind !== "device-host" || !attachment.leaseId || !attachment.epoch) return;
    const ambiguousMessage = `Android device outcome is ambiguous after disconnect: ${reason}`;
    const ambiguous = deviceToolAmbiguous(ambiguousMessage);
    this.ctx.storage.transactionSync(() => {
      this.ctx.storage.sql.exec(
        `UPDATE device_tool_calls
         SET state = 'ambiguous', result_json = ?, updated_at = ?
         WHERE lease_id = ? AND epoch = ? AND state = 'dispatched'`,
        JSON.stringify(ambiguous),
        Date.now(),
        attachment.leaseId,
        attachment.epoch,
      );
      this.ctx.storage.sql.exec(
        `UPDATE device_host_state
         SET host_id = NULL, catalog_version = NULL, lease_id = NULL, lease_expires_at = 0
         WHERE singleton = 1 AND lease_id = ? AND epoch = ?`,
        attachment.leaseId,
        attachment.epoch,
      );
    });
    for (const [callId, pending] of this.#pendingDeviceToolCalls) {
      if (pending.leaseId !== attachment.leaseId || pending.epoch !== attachment.epoch) continue;
      if (pending.timeout !== undefined) clearTimeout(pending.timeout);
      this.#pendingDeviceToolCalls.delete(callId);
      pending.reject(new DeviceHostAmbiguousError(ambiguousMessage));
    }
  }

  #deviceHostState(): DeviceHostStateRow {
    const state = this.ctx.storage.sql.exec<DeviceHostStateRow>(
      `SELECT epoch, host_id, catalog_version, lease_id, lease_expires_at
       FROM device_host_state WHERE singleton = 1`,
    ).toArray()[0];
    if (!state) throw new Error("device-host state is missing");
    return state;
  }

  #sendDeviceHost(socket: WebSocket, message: DeviceHostServerMessage): void {
    socket.send(JSON.stringify(message));
  }

  #armDeviceToolExpiry(callId: string, pending: PendingDeviceToolCall, expiresAt: number): void {
    if (pending.timeout !== undefined) clearTimeout(pending.timeout);
    pending.timeout = setTimeout(() => {
      const current = this.#pendingDeviceToolCalls.get(callId);
      if (current !== pending) return;
      const state = this.#deviceHostState();
      if (state.lease_id === pending.leaseId
        && state.epoch === pending.epoch
        && state.lease_expires_at > Date.now()
        && pending.deadlineAt > Date.now()) {
        this.#armDeviceToolExpiry(
          callId,
          pending,
          Math.min(state.lease_expires_at, pending.deadlineAt),
        );
        return;
      }
      const ambiguousMessage = "Android device did not return a result before its lease or call deadline expired";
      const ambiguous = deviceToolAmbiguous(ambiguousMessage);
      this.ctx.storage.sql.exec(
        `UPDATE device_tool_calls
         SET state = 'ambiguous', result_json = ?, updated_at = ?
         WHERE call_id = ? AND lease_id = ? AND epoch = ? AND state = 'dispatched'`,
        JSON.stringify(ambiguous),
        Date.now(),
        callId,
        pending.leaseId,
        pending.epoch,
      );
      this.#pendingDeviceToolCalls.delete(callId);
      pending.reject(new DeviceHostAmbiguousError(ambiguousMessage));
    }, Math.max(1, expiresAt - Date.now()));
  }


  async #dispatch(socket: WebSocket, command: ClientCommand): Promise<void> {
    if (this.#deleting) {
      this.#send(socket, { type: "error", code: "agent_deleting", message: "the agent is being deleted" });
      return;
    }
    if (command.type === "ping") {
      if (command.nonce === undefined) this.#sendEncoded(socket, ENCODED_PONG);
      else this.#send(socket, { type: "pong", nonce: command.nonce });
      return;
    }
    if (command.type === "status") {
      this.#send(socket, {
        type: "status",
        active_turns: this.#activeTurnIds(),
        active_turn_details: this.#activeTurnDetails(),
        agent_loaded: this.#agent !== undefined,
        connected_clients: this.ctx.getWebSockets().length,
      });
      return;
    }
    if (command.type === "cancel") {
      try {
        const row = await this.#findManagedTurn(command.id);
        if (!row) throw new ManagedRequestError(404, "turn_not_found", `turn ${command.id} does not exist`);
        if (isTerminalState(row.state)) {
          this.#send(socket, messageForManagedTurn(row));
          return;
        }
        const cancelling = this.#markCancelling(command.id);
        this.#scheduleCancellation(cancelling.id);
      } catch (error) {
        const failure = managedHttpError(error, "cancel_failed");
        this.#send(socket, { type: "error", code: failure.code, message: failure.message });
      }
      return;
    }
    if (command.type === "steer") {
      const turn = this.#turns.get(command.id);
      if (!turn) {
        this.#send(socket, { type: "error", code: "turn_not_active", message: `turn ${command.id} is not active` });
        return;
      }
      try {
        await turn.steer({ input: command.input });
      } catch (error) {
        this.#send(socket, { type: "error", code: "steer_failed", message: errorMessage(error) });
      }
      return;
    }
    try {
      const requestHash = await hashManagedInput(command.input);
      const submission = await this.#submitManagedTurn(command.id, command.input, requestHash, null);
      if (!submission.created) this.#send(socket, messageForManagedTurn(submission.row));
    } catch (error) {
      const failure = managedHttpError(error);
      this.#send(socket, { type: "error", code: failure.code, message: failure.message });
    }
  }

  async #submitHttpTurn(request: Request): Promise<Response> {
    if (this.#deleting) return json({ error: "agent_deleting" }, { status: 409 });
    let encoded: string;
    try {
      encoded = await readBoundedRequestText(request, MAX_REQUEST_BODY_BYTES);
    } catch (error) {
      return managedErrorResponse(error);
    }
    let value: unknown;
    try {
      value = JSON.parse(encoded);
    } catch {
      return json({ error: "invalid_json" }, { status: 400 });
    }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      return json(
        {
          error: "invalid_request",
          message: "turn request must be a JSON object",
        },
        { status: 400 },
      );
    }
    const body = value as Record<string, unknown>;
    if (Object.keys(body).some((key) => key !== "id" && key !== "input")) {
      return json(
        {
          error: "invalid_request",
          message: "supported fields are id and input",
        },
        { status: 400 },
      );
    }
    try {
      validatePromptInput(body.input);
    } catch (error) {
      const protocol =
        error instanceof ProtocolError
          ? error
          : new ProtocolError("invalid_prompt", errorMessage(error));
      return json(
        { error: protocol.code, message: protocol.message },
        { status: 400 },
      );
    }
    if (
      body.id !== undefined &&
      (typeof body.id !== "string" || !TURN_ID.test(body.id))
    ) {
      return json(
        {
          error: "invalid_turn_id",
          message: "turn id must be 1-128 safe ASCII characters",
        },
        { status: 400 },
      );
    }
    const requestKey = request.headers.get("idempotency-key");
    if (requestKey !== null && !IDEMPOTENCY_KEY.test(requestKey)) {
      return json({ error: "invalid_idempotency_key" }, { status: 400 });
    }
    if (body.id === undefined && requestKey === null) {
      return json(
        {
          error: "idempotency_required",
          message: "provide a stable turn id or Idempotency-Key",
        },
        { status: 400 },
      );
    }

    try {
      const input = body.input;
      const id = typeof body.id === "string" ? body.id : uuidV7();
      const requestHash = await hashManagedInput(input);
      const submission = await this.#submitManagedTurn(
        id,
        input,
        requestHash,
        requestKey,
        body.id !== undefined,
      );
      const view = managedTurnView(submission.row);
      const summary = submission.created
        ? this.#conversationSummary()
        : undefined;
      return json(view, {
        status: submission.created ? 202 : 200,
        headers: submission.created
          ? {
              "x-nanocodex-turn-created": "1",
              "x-nanocodex-turn-summary": asciiJsonHeaderValue(summary),
            }
          : undefined,
      });
    } catch (error) {
      return managedErrorResponse(error);
    }
  }

  async #managedRealtime(
    kind: ManagedRealtimeKind,
    request: Request,
  ): Promise<Response> {
    if (this.#deleting || this.#deleted) {
      return json({ error: "agent_deleting" }, { status: 409 });
    }
    let encoded: string;
    try {
      encoded = await readBoundedRequestText(
        request,
        MAX_REALTIME_REQUEST_BYTES,
      );
    } catch (error) {
      return managedErrorResponse(error);
    }
    let value: unknown;
    try {
      value = JSON.parse(encoded);
    } catch {
      return json({ error: "invalid_json" }, { status: 400 });
    }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      return json(
        {
          error: "invalid_request",
          message: "realtime request must be a JSON object",
        },
        { status: 400 },
      );
    }
    const body = value as Record<string, unknown>;
    const allowed =
      kind === "delegate"
        ? new Set(["voice_session_id", "operation_id", "input"])
        : new Set(["voice_session_id", "operation_id"]);
    if (Object.keys(body).some((key) => !allowed.has(key))) {
      return json(
        {
          error: "invalid_request",
          message: `unsupported ${kind} request field`,
        },
        { status: 400 },
      );
    }
    if (
      typeof body.voice_session_id !== "string" ||
      !REALTIME_ID.test(body.voice_session_id) ||
      typeof body.operation_id !== "string" ||
      !REALTIME_ID.test(body.operation_id)
    ) {
      return json(
        {
          error: "invalid_request",
          message:
            "voice_session_id and operation_id must be 1-128 safe ASCII characters",
        },
        { status: 400 },
      );
    }
    if (kind === "delegate") {
      if (
        typeof body.input !== "string" ||
        body.input.trim() === "" ||
        encoder.encode(body.input).byteLength > MAX_REALTIME_REQUEST_BYTES / 2
      ) {
        return json(
          {
            error: "invalid_prompt",
            message: `delegation input must be a non-empty string of at most ${MAX_REALTIME_REQUEST_BYTES / 2} bytes`,
          },
          { status: 400 },
        );
      }
    } else if (body.input !== undefined) {
      return json({ error: "invalid_request" }, { status: 400 });
    }

    const parsed: ManagedRealtimeRequest = {
      voiceSessionId: body.voice_session_id,
      operationId: body.operation_id,
      ...(kind === "delegate" ? { input: body.input as string } : {}),
    };
    const requestHash = await hashText(
      canonicalJson({
        kind,
        operation_id: parsed.operationId,
        voice_session_id: parsed.voiceSessionId,
        ...(parsed.input === undefined ? {} : { input: parsed.input }),
      }),
    );
    try {
      const result = await this.#runRealtimeOperation(
        parsed,
        kind,
        requestHash,
        async () => {
          const agent = await this.#ensureAgent();
          if (this.#deleting || this.#agent !== agent) {
            throw retryableError(
              "agent became unavailable during realtime operation",
            );
          }
          if (kind === "start") {
            const active = this.#managedRealtimeSession();
            if (active?.voice_session_id === parsed.voiceSessionId) {
              throw new ManagedRequestError(
                409,
                "voice_session_active",
                "voice session is already active with a different operation identity",
              );
            }
            if (active) {
              await this.#endManagedRealtimeSession(
                agent,
                active.voice_session_id,
              );
            }
            const context = await agent.session.realtime.start();
            assertBoundedRealtimeContext(context);
            this.ctx.storage.sql.exec(
              `INSERT INTO managed_realtime_session (singleton, voice_session_id, updated_at)
               VALUES (1, ?, ?)
               ON CONFLICT (singleton) DO UPDATE SET
                 voice_session_id = excluded.voice_session_id,
                 updated_at = excluded.updated_at`,
              parsed.voiceSessionId,
              Date.now(),
            );
            return {
              context,
              operation_id: parsed.operationId,
              voice_session_id: parsed.voiceSessionId,
            };
          }
          if (kind === "stop") {
            const active = this.#managedRealtimeSession();
            if (active?.voice_session_id !== parsed.voiceSessionId) {
              return {
                context: [],
                operation_id: parsed.operationId,
                stale: active !== undefined,
                stopped: false,
                voice_session_id: parsed.voiceSessionId,
              };
            }
            const context = await this.#endManagedRealtimeSession(
              agent,
              parsed.voiceSessionId,
            );
            return {
              context,
              operation_id: parsed.operationId,
              stopped: true,
              voice_session_id: parsed.voiceSessionId,
            };
          }
          if (this.#managedRealtimeSession()?.voice_session_id !== parsed.voiceSessionId) {
            throw new ManagedRequestError(
              409,
              "voice_session_inactive",
              "realtime delegation does not own the active voice session",
            );
          }
          return this.#routeRealtimeDelegation(agent, parsed, requestHash);
        },
      );
      return json(result, { status: kind === "delegate" ? 202 : 200 });
    } catch (error) {
      return managedErrorResponse(error, `realtime_${kind}_failed`);
    }
  }

  async #runRealtimeOperation<Result>(
    request: ManagedRealtimeRequest,
    kind: ManagedRealtimeKind,
    requestHash: string,
    operation: () => Promise<Result>,
  ): Promise<Result> {
    const key = `${request.voiceSessionId}\n${request.operationId}`;
    let existing: ManagedRealtimeOperationRow | ManagedRealtimeReceipt | undefined =
      this.#managedRealtimeOperation(
        request.voiceSessionId,
        request.operationId,
      );
    if (!existing) {
      try {
        existing = await this.#realtimeArchive.find(
          request.voiceSessionId,
          request.operationId,
        );
      } catch (error) {
        throw new ManagedRequestError(
          503,
          "realtime_archive_unavailable",
          `archived realtime lookup failed: ${errorMessage(error)}`,
        );
      }
      existing = this.#managedRealtimeOperation(
        request.voiceSessionId,
        request.operationId,
      ) ?? existing;
    }
    if (
      existing &&
      (existing.kind !== kind || existing.request_hash !== requestHash)
    ) {
      throw new ManagedRequestError(
        409,
        "idempotency_conflict",
        "realtime operation identity is already bound to a different request",
      );
    }
    const admittedInFlight = this.#realtimeOperations.get(key);
    if (admittedInFlight) return admittedInFlight as Promise<Result>;
    if (existing?.state === "completed" && existing.response_json !== null) {
      return JSON.parse(existing.response_json) as Result;
    }
    if (existing?.state === "pending") {
      throw new ManagedRequestError(
        409,
        "operation_pending",
        "realtime operation is pending and will not be replayed",
      );
    }
    const pendingOperations = this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_realtime_operations WHERE state = 'pending'",
    ).one().count;
    if (pendingOperations >= MAX_PENDING_REALTIME_OPERATIONS) {
      throw new ManagedRequestError(
        429,
        "realtime_queue_full",
        `at most ${MAX_PENDING_REALTIME_OPERATIONS} realtime operations may be pending`,
      );
    }

    const now = Date.now();
    this.ctx.storage.sql.exec(
      `INSERT INTO managed_realtime_operations (
         voice_session_id, operation_id, kind, request_hash, state, response_json, created_at, updated_at
       ) VALUES (?, ?, ?, ?, 'pending', NULL, ?, ?)
       ON CONFLICT (voice_session_id, operation_id) DO UPDATE SET updated_at = excluded.updated_at`,
      request.voiceSessionId,
      request.operationId,
      kind,
      requestHash,
      now,
      now,
    );
    const task = this.#track(
      (async () => {
        try {
          const result = await this.#serializeRealtimeOperation(operation);
          const response = JSON.stringify(result);
          if (
            encoder.encode(response).byteLength >
            MAX_REALTIME_CONTEXT_BYTES + MAX_REALTIME_REQUEST_BYTES
          ) {
            throw new ManagedRequestError(
              413,
              "response_too_large",
              "realtime response exceeds the managed limit",
            );
          }
          this.ctx.storage.sql.exec(
            `UPDATE managed_realtime_operations
           SET state = 'completed', response_json = ?, updated_at = ?
           WHERE voice_session_id = ? AND operation_id = ? AND request_hash = ?`,
            response,
            Date.now(),
            request.voiceSessionId,
            request.operationId,
            requestHash,
          );
          if (this.#realtimeArchive.needsSeal()) {
            void this.#sealRealtimeArchive(false).catch(() => {});
            void this.#scheduleNextAlarm().catch(() => {});
          }
          return result;
        } catch (error) {
          this.ctx.storage.sql.exec(
            `DELETE FROM managed_realtime_operations
           WHERE voice_session_id = ? AND operation_id = ? AND state = 'pending'`,
            request.voiceSessionId,
            request.operationId,
          );
          throw error;
        }
      })(),
    );
    this.#realtimeOperations.set(key, task);
    try {
      return await task;
    } finally {
      if (this.#realtimeOperations.get(key) === task)
        this.#realtimeOperations.delete(key);
    }
  }

  async #serializeRealtimeOperation<Result>(operation: () => Promise<Result>): Promise<Result> {
    let release!: () => void;
    const previous = this.#realtimeOperationTail;
    this.#realtimeOperationTail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous.catch(() => {});
    try {
      return await operation();
    } finally {
      release();
    }
  }

  async #routeRealtimeDelegation(
    agent: CloudflareAgent.Agent,
    request: ManagedRealtimeRequest,
    requestHash: string,
  ): Promise<ManagedRealtimeRouteResult> {
    const input = request.input!;
    this.#assertRealtimeRouteAvailable();
    let release!: () => void;
    const previous = this.#realtimeRouteTail;
    this.#realtimeRouteTail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous.catch(() => {});
    try {
      const turnId = `realtime:${(await hashText(`${request.voiceSessionId}\n${request.operationId}`)).slice(0, 48)}`;
      this.#realtimeEventBuffer = [];
      let turn: Turn | undefined;
      try {
        turn = await CloudflareAgent.route(agent, { input });
      } catch (error) {
        const buffered = this.#takeRealtimeEventBuffer();
        for (const event of buffered) this.#recordAgentEvent(event);
        throw error;
      }
      if (turn === undefined) {
        const buffered = this.#takeRealtimeEventBuffer();
        const activeTurnId = this.#eventTurnId;
        for (const event of buffered) this.#recordAgentEvent(event);
        if (activeTurnId === undefined) {
          throw new ManagedRequestError(
            503,
            "event_attribution_failed",
            "steered realtime input has no active managed turn attribution",
          );
        }
        return {
          operation_id: request.operationId,
          route: "steered",
          turn_id: activeTurnId,
          voice_session_id: request.voiceSessionId,
        };
      }

      try {
        await this.#acceptRoutedTurn(turnId, input, requestHash, request);
        this.#turns.set(turnId, turn);
        this.#turnInputs.set(turnId, input);
        this.#eventTurnQueue.push(turnId);
        const buffered = this.#takeRealtimeEventBuffer();
        for (const event of buffered) this.#recordAgentEvent(event);
        this.ctx.waitUntil(this.#track(this.#ownRoutedTurn(turnId, turn)));
      } catch (error) {
        this.#takeRealtimeEventBuffer();
        try {
          await turn.cancel();
        } catch {
          /* The failed adoption still owns disposal. */
        }
        turn.dispose();
        throw error;
      }
      return {
        operation_id: request.operationId,
        route: "started",
        turn_id: turnId,
        voice_session_id: request.voiceSessionId,
      };
    } finally {
      this.#realtimeEventBuffer = undefined;
      release();
    }
  }

  async #steerHttpTurn(id: string, request: Request): Promise<Response> {
    let row: ManagedTurnRow | undefined;
    try { row = await this.#findManagedTurn(id); }
    catch (error) { return managedErrorResponse(error, "turn_archive_unavailable"); }
    if (!row) return json({ error: "turn_not_found" }, { status: 404 });
    if (row.state !== "accepted") {
      return json(
        { error: "turn_not_steerable", state: row.state },
        { status: 409 },
      );
    }
    const turn = this.#turns.get(id);
    if (!turn)
      return json(
        { error: "turn_not_active", state: row.state },
        { status: 409 },
      );
    try {
      const encoded = await readBoundedRequestText(
        request,
        MAX_REQUEST_BODY_BYTES,
      );
      const value = JSON.parse(encoded) as { input?: unknown };
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new ProtocolError(
          "invalid_request",
          "steer request must be a JSON object",
        );
      }
      validatePromptInput(value.input);
      await turn.steer({ input: value.input });
      return json({ turn_id: id, state: "steering" }, { status: 202 });
    } catch (error) {
      if (error instanceof SyntaxError)
        return json({ error: "invalid_json" }, { status: 400 });
      if (error instanceof ProtocolError) {
        return json(
          { error: error.code, message: error.message },
          { status: 400 },
        );
      }
      return managedErrorResponse(error, "steer_failed");
    }
  }

  async #cancelHttpTurn(id: string): Promise<Response> {
    let row: ManagedTurnRow | undefined;
    try { row = await this.#findManagedTurn(id); }
    catch (error) { return managedErrorResponse(error, "turn_archive_unavailable"); }
    if (!row) return json({ error: "turn_not_found" }, { status: 404 });
    if (isTerminalState(row.state)) return json(managedTurnView(row));
    if (row.state === "blocked") {
      return json(
        {
          error: "turn_blocked",
          message:
            row.error ??
            "the durable operation requires explicit reconciliation",
        },
        { status: 409 },
      );
    }
    try {
      const cancelling = this.#markCancelling(id);
      this.#scheduleCancellation(cancelling.id);
      return json({ turn_id: id, state: "cancelling" }, { status: 202 });
    } catch (error) {
      return managedErrorResponse(error, "cancel_failed");
    }
  }

  #assertRealtimeRouteAvailable(): void {
    if (this.#deleting || this.#deleted) {
      throw new ManagedRequestError(
        409,
        "agent_deleting",
        "the agent is being deleted",
      );
    }
    if (this.#streamError) {
      throw new ManagedRequestError(
        503,
        "event_stream_failed",
        this.#streamError,
      );
    }
    const blocked = this.#managedTurns(
      "WHERE state = 'blocked' ORDER BY updated_at LIMIT 1",
    )[0];
    if (blocked) {
      throw new ManagedRequestError(
        409,
        "agent_blocked",
        `turn ${blocked.id} requires reconciliation before new work`,
      );
    }
    if (this.#unfinishedTurnCount() >= MAX_ACTIVE_TURNS) {
      throw new ManagedRequestError(
        429,
        "turn_queue_full",
        `at most ${MAX_ACTIVE_TURNS} turns may be unfinished`,
      );
    }
    if (!this.#eventLog.canAcceptTurn()) {
      throw new ManagedRequestError(
        507,
        "event_log_full",
        "delete or replace this agent before submitting more work",
      );
    }
  }

  async #acceptRoutedTurn(
    id: string,
    input: PromptInput,
    requestHash: string,
    request: ManagedRealtimeRequest,
  ): Promise<ManagedTurnRow> {
    this.#assertRealtimeRouteAvailable();
    const requestKey = `realtime:${request.voiceSessionId}:${request.operationId}`;
    const retained = await Promise.all([
      this.#findManagedTurn(id),
      this.#findManagedTurnByRequestKey(requestKey),
    ]);
    if (retained[0] || retained[1]) {
      throw new ManagedRequestError(
        409,
        "idempotency_conflict",
        "realtime turn identity already exists",
      );
    }
    const now = Date.now();
    const accepted: StreamMessage = {
      type: "turn_accepted",
      id,
      input,
      replayed: false,
    };
    const firstPrompt = promptInputText(input);
    let event: DurableEvent<StreamMessage> | undefined;
    this.ctx.storage.transactionSync(() => {
      if (this.#managedTurn(id) || this.#managedTurnByRequestKey(requestKey)) {
        throw new ManagedRequestError(
          409,
          "idempotency_conflict",
          "realtime turn identity was concurrently accepted",
        );
      }
      event = this.#eventLog.append(accepted, id);
      this.ctx.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_key, request_hash, input_json, state,
           accepted_cursor, created_at, accepted_at, updated_at
         ) VALUES (?, ?, ?, ?, 'accepted', CAST(? AS INTEGER), ?, ?, ?)`,
        id,
        requestKey,
        requestHash,
        JSON.stringify(input),
        event.cursor,
        now,
        now,
        now,
      );
      this.ctx.storage.sql.exec(
        `UPDATE session_state
         SET accepted_turns = accepted_turns + 1,
             first_prompt = CASE WHEN accepted_turns = 0 THEN ? ELSE first_prompt END
         WHERE singleton = 1`,
        firstPrompt,
      );
    });
    this.#publish(event!);
    const row = this.#managedTurn(id);
    if (!row)
      throw new Error("routed managed turn disappeared after acceptance");
    return row;
  }

  async #ownRoutedTurn(id: string, turn: Turn): Promise<void> {
    try {
      await turn.accepted();
      if (this.#deleting) {
        try {
          await turn.cancel();
        } catch {
          /* Deletion owns shutdown. */
        }
        return;
      }
      await this.#complete(id, turn);
    } catch (error) {
      this.#releaseEventTurn(id);
      if (this.#turns.get(id) === turn) this.#turns.delete(id);
      this.#turnInputs.delete(id);
      turn.dispose();
      if (this.#deleting) return;
      const failure = classifyTurnFailure(id, error);
      this.#commitManagedFailure(id, error, false, failure.terminal);
      if (failure.reopenAgent) await this.#reopenAgent(id);
      this.#scheduleRecovery();
      await this.#scheduleNextAlarm();
    }
  }

  async #submitManagedTurn(
    id: string,
    input: PromptInput,
    requestHash: string,
    requestKey: string | null,
    explicitId = true,
  ): Promise<ManagedTurnSubmission> {
    if (this.#deleting || this.#deleted) {
      throw new ManagedRequestError(409, "agent_deleting", "the agent is being deleted");
    }
    const archived = await Promise.all([
      this.#managedTurn(id) ? Promise.resolve(undefined) : this.#archivedTurnById(id),
      requestKey === null || this.#managedTurnByRequestKey(requestKey)
        ? Promise.resolve(undefined)
        : this.#archivedTurnByRequestKey(requestKey),
    ]);
    if (this.#deleting || this.#deleted) {
      throw new ManagedRequestError(409, "agent_deleting", "the agent is being deleted");
    }
    const keyed = requestKey === null
      ? undefined
      : this.#managedTurnByRequestKey(requestKey) ?? archived[1];
    if (keyed && explicitId && keyed.id !== id) {
      throw new ManagedRequestError(409, "idempotency_conflict", "idempotency key is already bound to another turn");
    }
    const identified = this.#managedTurn(id) ?? archived[0];
    if (keyed && identified && keyed.id !== identified.id) {
      throw new ManagedRequestError(409, "idempotency_conflict", "turn id and idempotency key identify different turns");
    }
    const existing = keyed ?? identified;
    if (existing) {
      if (existing.request_hash !== requestHash) {
        throw new ManagedRequestError(409, "idempotency_conflict", "the idempotent request has different input");
      }
      if (requestKey !== null && existing.request_key !== requestKey) {
        throw new ManagedRequestError(409, "idempotency_conflict", "turn is bound to a different idempotency key");
      }
      if (existing.state === "cancelling") {
        this.#scheduleCancellation(existing.id);
      } else if (!isTerminalState(existing.state) && existing.state !== "blocked") {
        if (existing.state === "retryable"
          && existing.retry_at !== null
          && existing.retry_at > Date.now()) {
          // Idempotent polling must preserve the retained retry deadline. It
          // may race the recovery task that just wrote the row, so install the
          // alarm directly without requesting another recovery pass.
          await this.#scheduleNextAlarm();
        } else {
          this.#scheduleRecovery();
        }
      }
      return { created: false, row: existing };
    }
    if (this.#streamError) {
      throw new ManagedRequestError(503, "event_stream_failed", this.#streamError);
    }
    const blocked = this.#managedTurns("WHERE state = 'blocked' ORDER BY updated_at LIMIT 1")[0];
    if (blocked) {
      throw new ManagedRequestError(
        409,
        "agent_blocked",
        `turn ${blocked.id} requires reconciliation before new work`,
      );
    }
    if (this.#unfinishedTurnCount() >= MAX_ACTIVE_TURNS) {
      throw new ManagedRequestError(429, "turn_queue_full", `at most ${MAX_ACTIVE_TURNS} turns may be unfinished`);
    }
    if (!this.#eventLog.canAcceptTurn()) {
      throw new ManagedRequestError(507, "event_log_full", "delete or replace this agent before submitting more work");
    }

    const now = Date.now();
    const accepted: StreamMessage = { type: "turn_accepted", id, input, replayed: false };
    const firstPrompt = promptInputText(input);
    let event: DurableEvent<StreamMessage> | undefined;
    this.ctx.storage.transactionSync(() => {
      if (this.#deleting || !this.#sessionId()) {
        throw new ManagedRequestError(409, "agent_deleting", "the agent is being deleted");
      }
      event = this.#eventLog.append(accepted, id);
      this.ctx.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_key, request_hash, input_json, state,
           accepted_cursor, may_have_inner_operation, created_at, accepted_at, updated_at
         ) VALUES (?, ?, ?, ?, 'accepted', CAST(? AS INTEGER), 0, ?, ?, ?)`,
        id,
        requestKey,
        requestHash,
        JSON.stringify(input),
        event.cursor,
        now,
        now,
        now,
      );
      this.ctx.storage.sql.exec(
        `UPDATE session_state
         SET accepted_turns = accepted_turns + 1,
             first_prompt = CASE WHEN accepted_turns = 0 THEN ? ELSE first_prompt END
         WHERE singleton = 1`,
        firstPrompt,
      );
    });
    this.#publish(event!);
    const row = this.#managedTurn(id);
    if (!row) throw new Error("managed turn disappeared after acceptance");
    this.#scheduleRecovery();
    return { created: true, row };
  }

  #markCancelling(id: string): ManagedTurnRow {
    const current = this.#managedTurn(id);
    if (!current) throw new ManagedRequestError(404, "turn_not_found", `turn ${id} does not exist`);
    if (isTerminalState(current.state) || current.state === "cancelling") return current;
    if (current.state === "blocked") {
      throw new ManagedRequestError(409, "turn_blocked", current.error ?? "turn requires reconciliation");
    }
    const message: StreamMessage = { type: "turn_cancelling", id };
    let event: DurableEvent<StreamMessage> | undefined;
    this.ctx.storage.transactionSync(() => {
      const row = this.#managedTurn(id);
      if (!row || isTerminalState(row.state) || row.state === "cancelling") return;
      event = this.#eventLog.append(message, id, true);
      this.ctx.storage.sql.exec(
        `UPDATE managed_turns
         SET state = 'cancelling', error = NULL, retry_at = NULL, updated_at = ?
         WHERE id = ? AND state IN ('accepted', 'retryable')`,
        Date.now(),
        id,
      );
    });
    if (event) this.#publish(event);
    return this.#managedTurn(id) ?? current;
  }

  #scheduleCancellation(id: string): void {
    if (this.#deleting || this.#cancellationTasks.has(id)) return;
    const task = Promise.resolve().then(() => this.#cancelManagedTurn(id));
    this.#cancellationTasks.set(id, task);
    const observed = task.catch((error) => {
      console.error("managed turn cancellation failed", id, errorMessage(error));
    }).finally(async () => {
      if (this.#cancellationTasks.get(id) === task) this.#cancellationTasks.delete(id);
      if (!this.#deleting) await this.#scheduleNextAlarm();
    });
    this.ctx.waitUntil(observed);
  }

  async #cancelManagedTurn(id: string): Promise<void> {
    let row = this.#managedTurn(id);
    if (!row || isTerminalState(row.state) || row.state === "blocked") return;
    if (row.state === "cancelling" && row.retry_at !== null && row.retry_at > Date.now()) {
      await this.#scheduleNextAlarm();
      return;
    }
    const admission = this.#admissionTasks.get(id);
    if (admission) await admission;
    row = this.#managedTurn(id);
    if (!row || isTerminalState(row.state) || row.state === "blocked") return;
    let turn = this.#turns.get(id);
    if (!turn && row.may_have_inner_operation === 0) {
      this.#commitManagedMessage(id, { type: "turn_cancelled", id });
      this.#scheduleRecovery();
      return;
    }
    if (!turn) {
      row = await this.#admitManagedTurn(row, true);
      if (isTerminalState(row.state) || row.state === "blocked") return;
      turn = this.#turns.get(id);
    }
    if (!turn) {
      await this.#scheduleNextAlarm();
      return;
    }
    try {
      await turn.cancel();
    } catch (error) {
      if (this.#managedTurn(id)?.state === "cancelling") {
        this.#commitManagedFailure(id, error, true);
      }
      throw error;
    }
  }

  async #admitManagedTurn(row: ManagedTurnRow, replayed: boolean): Promise<ManagedTurnRow> {
    const current = this.#admissionTasks.get(row.id);
    if (current) return current;
    const task = this.#track(this.#startManagedTurn(row, replayed));
    this.#admissionTasks.set(row.id, task);
    try {
      return await task;
    } finally {
      if (this.#admissionTasks.get(row.id) === task) {
        this.#admissionTasks.delete(row.id);
        if (!this.#deleting) await this.#scheduleNextAlarm();
      }
    }
  }

  async #startManagedTurn(row: ManagedTurnRow, replayed: boolean): Promise<ManagedTurnRow> {
    const latest = this.#managedTurn(row.id);
    if (!latest || isTerminalState(latest.state) || latest.state === "blocked") return latest ?? row;
    if (latest.state === "retryable" && latest.retry_at !== null && latest.retry_at > Date.now()) {
      await this.#scheduleNextAlarm();
      return latest;
    }
    row = latest;
    let turn: Turn | undefined;
    const input = JSON.parse(row.input_json) as PromptInput;
    this.#pendingTurnIds.add(row.id);
    this.#turnInputs.set(row.id, input);
    try {
      const agent = await this.#ensureAgent();
      if (this.#deleting || this.#agent !== agent) throw retryableError("agent became unavailable during admission");
      const initialAccountContext = await this.#initialAccountContext();
      const modelInput = initialAccountContext?.turn_id === row.id
        ? withInitialAccountInfo(input, initialAccountContext.account)
        : input;
      const dispatchable = this.#managedTurn(row.id);
      if (!dispatchable || isTerminalState(dispatchable.state) || dispatchable.state === "blocked") {
        this.#pendingTurnIds.delete(row.id);
        this.#turnInputs.delete(row.id);
        return dispatchable ?? row;
      }
      if (dispatchable.state === "cancelling" && dispatchable.may_have_inner_operation === 0) {
        this.#pendingTurnIds.delete(row.id);
        this.#turnInputs.delete(row.id);
        return dispatchable;
      }
      this.#eventTurnQueue.push(row.id);
      // This write must stay immediately before prompt dispatch with no await
      // between them. A false positive is safely replayable; a false negative
      // could orphan an accepted Rust journal operation.
      this.ctx.storage.sql.exec(
        `UPDATE managed_turns
         SET may_have_inner_operation = 1, updated_at = ?
         WHERE id = ? AND state IN ('accepted', 'retryable', 'cancelling')`,
        Date.now(),
        row.id,
      );
      turn = agent.turn.prompt({ id: row.id, input: modelInput });
      this.#turns.set(row.id, turn);
      const durableId = await turn.accepted();
      if (durableId !== undefined && durableId !== row.id) {
        throw new Error(`durable admission returned unexpected turn id ${durableId}`);
      }
      if (this.#deleting) {
        try { await turn.cancel(); } catch { /* Deletion owns shutdown. */ }
        throw retryableError("agent was deleted during admission");
      }
      this.#pendingTurnIds.delete(row.id);
      this.ctx.storage.sql.exec(
        `UPDATE managed_turns
         SET state = CASE
               WHEN state = 'cancelling' THEN 'cancelling'
               WHEN state = 'retryable' THEN 'retryable'
               ELSE 'accepted'
             END,
             error = CASE WHEN state = 'retryable' THEN error ELSE NULL END,
             retry_at = CASE WHEN state = 'retryable' THEN retry_at ELSE NULL END,
             updated_at = ?
         WHERE id = ? AND state IN ('accepted', 'retryable', 'cancelling')`,
        Date.now(),
        row.id,
      );
      this.ctx.waitUntil(this.#track(this.#complete(row.id, turn)));
      if (this.#managedTurn(row.id)?.state === "cancelling") this.#scheduleCancellation(row.id);
      return this.#managedTurn(row.id) ?? row;
    } catch (error) {
      this.#releaseEventTurn(row.id);
      if (turn && this.#turns.get(row.id) === turn) this.#turns.delete(row.id);
      turn?.dispose();
      this.#pendingTurnIds.delete(row.id);
      this.#turnInputs.delete(row.id);
      if (this.#deleting) return this.#managedTurn(row.id) ?? row;
      const failure = classifyTurnFailure(row.id, error);
      const failed = this.#commitManagedFailure(row.id, error, replayed, failure.terminal);
      if (failure.reopenAgent) await this.#reopenAgent(row.id);
      return failed;
    }
  }

  async #beginDeletion(): Promise<void> {
    if (this.#deletionMarkerTask) return this.#deletionMarkerTask;
    if (this.#deleting) return;
    // Fence reconstruction first. A crash after this transaction is recovered
    // by the retained marker/alarm even if the local SQL tombstone has not yet
    // been written. The reverse order can strand external ownership forever.
    this.#deleting = true;
    let markerCommitted = false;
    const task = (async () => {
      await this.ctx.storage.transaction(async (transaction) => {
        await transaction.put(SESSION_DELETING_KEY, true);
        await transaction.setAlarm(Date.now() + 1);
      });
      markerCommitted = true;
      this.#markInitializationDeleted();
    })();
    this.#deletionMarkerTask = task;
    try {
      await task;
    } catch (error) {
      if (!markerCommitted) this.#deleting = false;
      throw error;
    } finally {
      if (this.#deletionMarkerTask === task) this.#deletionMarkerTask = undefined;
    }
  }

  #scheduleDeletion(): void {
    const task = this.#deleteOwnedSession();
    this.ctx.waitUntil(task.catch(async (error) => {
      console.error("managed session deletion recovery failed", errorMessage(error));
      try { await this.#scheduleCleanupRetry(); } catch { /* Marker retains ownership. */ }
    }));
  }

  #deleteOwnedSession(): Promise<void> {
    if (this.#deletionTask) return this.#deletionTask;
    const generation = ++this.#deletionGeneration;
    const task = this.#performOwnedSessionDeletion(generation);
    this.#deletionTask = task;
    void task.finally(() => {
      if (this.#deletionTask === task) this.#deletionTask = undefined;
    }).catch(() => {});
    return task;
  }

  async #performOwnedSessionDeletion(generation: number): Promise<void> {
    this.#deleting = true;
    // Reconstruction can enter here from a marker committed just before a
    // crash. Reassert the permanent local tombstone before any cleanup await.
    this.#markInitializationDeleted();
    await this.ctx.storage.put(SESSION_DELETION_GENERATION_KEY, generation);
    const session = this.#session();
    const runtimeProfile = session?.runtime_profile;
    const timeoutMs = this.#ownershipIoTimeoutMs();
    await this.#releaseRuntimeOwnershipForDeletion(timeoutMs);
    for (const socket of this.ctx.getWebSockets()) closeSocket(socket, 1000, "session deleted");
    const credentialBinding = this.#credentialBinding ?? (
      session && runtimeProfile !== "multiplayer"
        ? this.#bindingOwnershipForSession(session)
        : undefined
    );
    if (credentialBinding) {
      await Promise.all([
        unbindAgentCredential(
          this.env.NANOCODEX,
          credentialBinding.subject,
          credentialBinding.owner_id,
          this.#ownershipIoTimeoutMs(),
        ),
        detachAgent(
          this.env,
          credentialBinding.owner_id,
          credentialBinding.session_id,
          this.#ownershipIoTimeoutMs(),
        ),
      ]);
    }
    await withHardDeadline("managed workspace deletion", timeoutMs, async () => {
      const workspace = await getWorkspace(this);
      try {
        await workspace.fs.rm("/workspace", { recursive: true, force: true });
      } finally {
        workspace[Symbol.dispose]();
      }
    });
    // A socket or admission event may have resumed while external cleanup was
    // awaited. The durable deletion marker makes those paths fail closed; close
    // once more before dropping the owned journal and event history.
    for (const socket of this.ctx.getWebSockets()) closeSocket(socket, 1000, "session deleted");
    this.#assertDeletionGeneration(generation);
    while (this.#eventArchiveTask || this.#turnArchiveTask || this.#realtimeArchiveTask) {
      const archiveTasks: Promise<unknown>[] = [];
      if (this.#eventArchiveTask) archiveTasks.push(this.#eventArchiveTask);
      if (this.#turnArchiveTask) archiveTasks.push(this.#turnArchiveTask);
      if (this.#realtimeArchiveTask) archiveTasks.push(this.#realtimeArchiveTask);
      await Promise.allSettled(archiveTasks);
    }
    await Promise.all([
      this.#eventArchive.deleteAll(),
      this.#turnArchive.deleteAll(),
      this.#realtimeArchive.deleteAll(),
    ]);
    this.#assertDeletionGeneration(generation);
    CloudflareAgent.destroy(this);
    this.ctx.storage.transactionSync(() => {
      this.ctx.storage.sql.exec("DELETE FROM managed_turns");
      this.#eventLog.clear();
      this.#eventArchive.clearLocalState();
      this.#turnArchive.clearLocalState();
      this.#realtimeArchive.clearLocalState();
      this.ctx.storage.sql.exec("DELETE FROM managed_realtime_operations");
      this.ctx.storage.sql.exec("DELETE FROM managed_realtime_session");
      this.ctx.storage.sql.exec("DELETE FROM session_state");
    });
    await this.ctx.storage.transaction(async (transaction) => {
      const retainedGeneration = await transaction.get<number>(SESSION_DELETION_GENERATION_KEY);
      const deleting = await transaction.get<boolean>(SESSION_DELETING_KEY);
      if (retainedGeneration !== generation || deleting !== true) {
        throw new Error("managed deletion attempt lost its durable ownership fence");
      }
      await transaction.delete(CREDENTIAL_BINDING_KEY);
      await transaction.delete(CLEANUP_RETRY_ATTEMPT_KEY);
      await transaction.delete(INITIAL_ACCOUNT_CONTEXT_KEY);
      await transaction.delete(SESSION_DELETING_KEY);
      await transaction.deleteAlarm();
    });
    this.#assertDeletionGeneration(generation);
    this.#credentialBinding = undefined;
    this.#initialAccountContextTask = undefined;
    this.#deleting = false;
  }

  async #releaseRuntimeOwnershipForDeletion(timeoutMs: number): Promise<void> {
    const agent = this.#agent;
    const construction = this.#agentConstruction;
    const shutdown = this.#agentShutdownPromise;
    const turns = [...this.#turns.values()];
    const inFlight = [...this.#inFlight];

    this.#runtimeOwnershipGeneration += 1;
    this.#agent = undefined;
    this.#agentPromise = undefined;
    this.#agentConstruction = undefined;
    this.#agentShutdownPromise = undefined;
    this.#events?.off();
    this.#events = undefined;
    this.#turns.clear();
    this.#inFlight.clear();
    this.#admissionTasks.clear();
    this.#cancellationTasks.clear();
    this.#recoveryTask = undefined;
    this.#reopenInterruptedTurnIds.clear();
    this.#eventTurnQueue.length = 0;
    this.#eventTurnId = undefined;
    this.#pendingTurnIds.clear();
    this.#turnInputs.clear();

    // The deletion attempt waits for the construction it superseded once. If
    // that drain times out, the retained ownership record keeps the late
    // result visible to its own cleanup continuation without making every
    // later deletion generation wait on the same noncooperative promise.
    const constructionShutdown = construction
      ? this.#retireAgentConstruction(construction)
      : undefined;

    await drainRuntimeForDeletion(
      timeoutMs,
      turns,
      async () => {
        if (shutdown) return shutdown;
        await Promise.all([
          agent?.session.shutdown(),
          constructionShutdown,
        ]);
      },
      inFlight,
    );
  }

  #assertDeletionGeneration(generation: number): void {
    if (!this.#deleting || this.#deletionGeneration !== generation) {
      throw new Error("managed deletion attempt lost its ownership fence");
    }
  }

  async #scheduleCleanupRetry(): Promise<number> {
    const previous = await this.ctx.storage.get<number>(CLEANUP_RETRY_ATTEMPT_KEY) ?? 0;
    const attempt = Math.min(30, previous + 1);
    const cap = Math.min(MAX_CLEANUP_RETRY_MS, 1_000 * (2 ** attempt));
    const random = crypto.getRandomValues(new Uint32Array(1))[0]! / 0x1_0000_0000;
    const delay = Math.ceil(cap / 2 + random * cap / 2);
    await this.ctx.storage.transaction(async (transaction) => {
      await transaction.put(CLEANUP_RETRY_ATTEMPT_KEY, attempt);
      await transaction.setAlarm(Date.now() + delay);
    });
    return delay;
  }

  #scheduleRecovery(): void {
    if (this.#deleting || this.#deleted) return;
    if (this.#recoveryTask) {
      this.#recoveryRequested = true;
      return;
    }
    this.#recoveryRequested = false;
    // Decide retry eligibility at scheduling time. Construction and other I/O
    // must not let work scheduled just before retry_at drift across the fence.
    const observedAt = Date.now();
    const task = Promise.resolve().then(() => this.#runRecovery(observedAt));
    this.#recoveryTask = task;
    void task.finally(() => {
      if (this.#recoveryTask !== task) return;
      this.#recoveryTask = undefined;
      if (this.#recoveryRequested) this.#scheduleRecovery();
    }).catch(() => {});
    this.ctx.waitUntil(task.catch((error) => {
      console.error("managed turn recovery failed", errorMessage(error));
    }));
  }

  async #runRecovery(observedAt: number): Promise<void> {
    if (this.#deleting || !this.#sessionId() || this.#streamError) return;
    const rows = this.#managedTurns(
      `WHERE state IN ('accepted', 'cancelling', 'retryable', 'blocked')
       ORDER BY created_at, rowid`,
    );
    for (const row of rows) {
      if (this.#deleting) return;
      const current = this.#managedTurn(row.id);
      if (!current || isTerminalState(current.state)) continue;
      if (current.state === "blocked") break;
      if ((current.state === "retryable" || current.state === "cancelling")
        && current.retry_at !== null && current.retry_at > observedAt) break;
      if (current.state === "cancelling") {
        const cancellation = this.#cancellationTasks.get(row.id);
        try {
          if (cancellation) await cancellation;
          else await this.#cancelManagedTurn(current.id);
        } catch (error) {
          // Cancellation failure is already projected into the durable row.
          // Keep the ordered recovery pump alive so it can retain that retry.
          console.error("managed turn cancellation recovery failed", row.id, errorMessage(error));
        }
        const cancelled = this.#managedTurn(current.id);
        if (cancelled && !isTerminalState(cancelled.state)) break;
        continue;
      }
      if (this.#turns.has(row.id)
        || this.#pendingTurnIds.has(row.id)
        || this.#admissionTasks.has(row.id)) {
        if (current.may_have_inner_operation === 1) continue;
        break;
      }
      try {
        validatePromptInput(JSON.parse(current.input_json));
        await this.#admitManagedTurn(current, true);
      } catch (error) {
        this.#commitManagedFailure(current.id, error, true);
      }
      const admitted = this.#managedTurn(current.id);
      if (admitted && (admitted.state === "retryable"
        || admitted.state === "cancelling"
        || admitted.state === "blocked")) break;
    }
    await this.#scheduleNextAlarm();
  }

  async #ensureAgent(): Promise<CloudflareAgent.Agent> {
    if (this.#deleting) throw retryableError("agent is being deleted");
    if (this.#agentShutdownPromise) {
      try {
        await this.#agentShutdownPromise;
      } catch (error) {
        throw retryableError(`previous agent shutdown failed: ${errorMessage(error)}`);
      }
      if (this.#deleting) throw retryableError("agent is being deleted");
      return this.#ensureAgent();
    }
    if (this.#agent) return this.#agent;
    if (this.#agentPromise) return this.#agentPromise;
    const construction: AgentConstructionOwnership = {
      deletionGeneration: this.#deletionGeneration,
      runtimeGeneration: this.#runtimeOwnershipGeneration,
      promise: undefined as unknown as Promise<CloudflareAgent.Agent>,
      publication: undefined as unknown as Promise<CloudflareAgent.Agent>,
    };
    construction.promise = this.#createAgent();
    this.#agentConstruction = construction;
    this.#agentConstructions.add(construction);
    const publication = this.#publishAgentConstruction(construction);
    construction.publication = publication;
    this.#agentPromise = publication;
    try {
      return await publication;
    } finally {
      if (this.#agentPromise === publication) this.#agentPromise = undefined;
      if (this.#agentConstruction === construction) this.#agentConstruction = undefined;
    }
  }

  async #publishAgentConstruction(
    construction: AgentConstructionOwnership,
  ): Promise<CloudflareAgent.Agent> {
    try {
      const agent = await construction.promise;
      if (!this.#ownsAgentConstruction(construction)) {
        try { await this.#retireAgentConstruction(construction, agent); }
        catch (error) {
          throw retryableError(`superseded agent shutdown failed: ${errorMessage(error)}`);
        }
        throw retryableError("agent construction was superseded");
      }
      const events = agent.events.watch();
      events.onEvent((event) => this.#recordAgentEvent(event));
      if (!this.#ownsAgentConstruction(construction)) {
        events.off();
        try { await this.#retireAgentConstruction(construction, agent); }
        catch (error) {
          throw retryableError(`superseded agent shutdown failed: ${errorMessage(error)}`);
        }
        throw retryableError("agent construction was superseded");
      }
      this.#events = events;
      this.#agent = agent;
      this.#agentConstructions.delete(construction);
      return this.#agent;
    } catch (error) {
      if (!construction.shutdown) this.#agentConstructions.delete(construction);
      throw error;
    }
  }

  #ownsAgentConstruction(construction: AgentConstructionOwnership): boolean {
    return !this.#deleting
      && !this.#deleted
      && this.#agentConstruction === construction
      && this.#agentPromise === construction.publication
      && this.#runtimeOwnershipGeneration === construction.runtimeGeneration
      && this.#deletionGeneration === construction.deletionGeneration;
  }

  #retireAgentConstruction(
    construction: AgentConstructionOwnership,
    resolved?: CloudflareAgent.Agent,
  ): Promise<void> {
    if (construction.shutdown) return construction.shutdown;
    this.#agentConstructions.add(construction);
    const shutdown = (async () => {
      let agent = resolved;
      if (!agent) {
        try { agent = await construction.promise; }
        catch { return; }
      }
      await agent.session.shutdown();
    })();
    construction.shutdown = shutdown;
    void shutdown.finally(() => {
      this.#agentConstructions.delete(construction);
    }).catch(() => {});
    this.ctx.waitUntil(shutdown.catch((error) => {
      console.error("superseded Nanocodex agent shutdown failed", errorMessage(error));
    }));
    return shutdown;
  }

  #initialAccountContext(): Promise<InitialAccountContext | undefined> {
    return this.#initialAccountContextTask ??= this.#loadInitialAccountContext();
  }

  async #loadInitialAccountContext(): Promise<InitialAccountContext | undefined> {
    const retained = await this.ctx.storage.get<InitialAccountContext>(
      INITIAL_ACCOUNT_CONTEXT_KEY,
    );
    if (retained) return retained;
    const session = this.#session();
    if (!session || session.runtime_profile === "multiplayer") return undefined;
    const first = this.ctx.storage.sql.exec<{ id: string }>(
      "SELECT id FROM managed_turns ORDER BY created_at, id LIMIT 1",
    ).toArray()[0];
    if (!first) return undefined;
    const prepared = {
      turn_id: first.id,
      account: await accountInfo(this.env.NANOCODEX, session.owner_id, true),
    } satisfies InitialAccountContext;
    await this.ctx.storage.put(INITIAL_ACCOUNT_CONTEXT_KEY, prepared);
    return prepared;
  }

  async #executePhone(input: unknown, context: { callId: string }): Promise<unknown> {
    let phone;
    try {
      phone = parseDeviceToolInput(input);
    } catch (error) {
      return {
        ok: false,
        status: "failed",
        output: {
          code: error instanceof DeviceHostProtocolError ? error.code : "invalid_phone_input",
          message: errorMessage(error),
        },
      };
    }
    const state = this.#deviceHostState();
    const socket = this.ctx.getWebSockets("device-host").find((candidate) => {
      if (candidate.readyState !== WebSocket.OPEN) return false;
      const attachment = candidate.deserializeAttachment() as DeviceHostAttachment | null;
      return attachment?.kind === "device-host"
        && matchesDeviceHostLease(attachment, state, Date.now());
    });
    if (!socket || !state.lease_id || !state.host_id || state.lease_expires_at < Date.now()) {
      if (socket) {
        try {
          this.#sendDeviceHost(socket, {
            type: "fenced",
            epoch: state.epoch,
            reason: "the device-host lease expired before tool dispatch",
          });
        } catch { /* Closing the expired socket is itself the authoritative fence. */ }
        this.#retireDeviceHost(socket, "lease expired before dispatch");
        closeSocket(socket, 1008, "device-host lease expired");
      }
      return deviceToolUnavailable();
    }
    const now = Date.now();
    try {
      this.ctx.storage.sql.exec(
        `INSERT INTO device_tool_calls
           (call_id, lease_id, epoch, state, operation, arguments_json, created_at, updated_at)
         VALUES (?, ?, ?, 'dispatched', ?, ?, ?, ?)`,
        context.callId,
        state.lease_id,
        state.epoch,
        phone.operation,
        JSON.stringify(phone.arguments),
        now,
        now,
      );
    } catch (error) {
      return deviceToolAmbiguous(`Phone call could not be admitted without risking replay: ${errorMessage(error)}`);
    }
    const result = new Promise<{ success: boolean; output: unknown }>((resolve, reject) => {
      const pending: PendingDeviceToolCall = {
        leaseId: state.lease_id!,
        epoch: state.epoch,
        deadlineAt: now + DEVICE_TOOL_CALL_TIMEOUT_MS,
        resolve,
        reject,
      };
      this.#pendingDeviceToolCalls.set(context.callId, pending);
      this.#armDeviceToolExpiry(
        context.callId,
        pending,
        Math.min(state.lease_expires_at, pending.deadlineAt),
      );
    });
    try {
      this.#sendDeviceHost(socket, {
        type: "device_tool_call",
        lease_id: state.lease_id,
        epoch: state.epoch,
        call_id: context.callId,
        tool: "phone",
        operation: phone.operation,
        arguments: phone.arguments,
      });
    } catch (error) {
      this.#retireDeviceHost(socket, `dispatch failed: ${errorMessage(error)}`);
    }
    try {
      const completed = await result;
      return deviceToolResult(completed.success, completed.output);
    } catch (error) {
      return deviceToolAmbiguous(errorMessage(error));
    }
  }


  async #createAgent(): Promise<CloudflareAgent.Agent> {
    const constructionStartedAt = performance.now();
    const session = this.#session();
    if (!session) throw new Error("session is not initialized");
    const multiplayer = session.runtime_profile === "multiplayer";
    if (!multiplayer) await this.#ensureCredentialBinding(session);
    const workspace = await getWorkspace(this);
    const sourceFilesystem = await createComputerFilesystem(workspace);
    let workspaceDisposed = false;
    const disposeWorkspace = () => {
      if (workspaceDisposed) return;
      workspaceDisposed = true;
      workspace[Symbol.dispose]();
    };
    // Shared-room members can all admit turns. Never attach the room owner's
    // connector capability to that shared tool runtime: provider destinations
    // fail closed without a subject, while ordinary public HTTP remains usable.
    const shellFetch = createManagedShellFetch(
      this.env.NANOCODEX,
      multiplayer ? undefined : this.ctx.id.toString(),
    );
    const shell = await justBash({
      filesystem: sourceFilesystem,
      maxEntries: 2_000,
      maxOutputTokens: 10_000,
      fetch: shellFetch,
      customCommands: [createManagedGhCommand(shellFetch)],
    });
    const execCommand = Object.freeze({ ...shell.tool, dispose: disposeWorkspace });
    const currentAccountInfo = () => accountInfo(
      this.env.NANOCODEX,
      session.owner_id,
      !multiplayer,
    );
    let agent: CloudflareAgent.Agent;
    try {
      agent = await CloudflareAgent.create(this, {
        eventPersistence: "caller",
        terminalReceiptRetention: 512,
        instructions: multiplayer
          ? [
            "You are the shared Nanocodex participant in a short-lived Multiplayer chat room.",
            "Reply conversationally and concisely to the room message. Use the normal Nanocodex tools when they materially help answer the room.",
            "GitHub, Gmail, Google Drive, and other account connectors are unavailable in shared rooms.",
            "Never claim to have performed an external action unless its tool completed successfully, and never expose internal runtime, routing, credential, or correlation identifiers.",
          ].join("\n\n")
          : [
            "You are Nanocodex running as a durable managed agent on Cloudflare Workers.",
            "The phone tool's current attached Android device is the authority for phone state and phone actions. A missing device host means the phone is unavailable; there is no cloud fallback.",
            "Never claim that a phone action happened unless the phone tool returned ok=true and status=completed. Report failed, unavailable, and ambiguous phone outcomes accurately.",
            "Your /workspace filesystem is durable Cloudflare Computer storage backed by this agent's Durable Object.",
            "Call accountInfo to see the current identities, stablecoin balances, and app authorization boundaries, then use gh or curl normally through transparent authenticated egress. accountInfo is a tool, not a shell command.",
          ].join("\n\n"),
        tools: [
          execCommand,
          ...(multiplayer ? [] : [{
            name: "phone",
            description: "Read current phone state or perform a phone operation on the currently attached Android device. This has no cloud fallback; inspect ok and status before claiming success.",
            supportsParallelToolCalls: false,
            parameters: {
              type: "object",
              properties: {
                operation: { type: "string", minLength: 1, maxLength: 128 },
                arguments: { type: "object" },
              },
              required: ["operation"],
              additionalProperties: false,
            },
            handler: (input: unknown, context: { callId: string }) => this.#executePhone(input, context),
          }]),
          ...(multiplayer ? [] : [{
            name: "accountInfo",
            description: "Report account authentication, stablecoin balances, and app authorization boundaries. Never returns credentials.",
            parameters: { type: "object", additionalProperties: false },
            handler: currentAccountInfo,
          }]),
          web({
            url: "https://managed-tools.internal/web-search",
            fetch: managedWebFetch(this.env, this.ctx.id.toString()),
          }),
          imageGeneration({
            url: "https://managed-tools.internal/image-generation",
            fetch: managedImageFetch(this.env, this.ctx.id.toString()),
            workspace: shell.filesystem,
          }),
          viewImage({ workspace: shell.filesystem }),
          updatePlan(),
          {
            name: "runtimeInfo",
            description: "Return information about the current durable agent runtime.",
            parameters: { type: "object", additionalProperties: false },
            handler: async () => ({
              runtime: "cloudflare-durable-object",
              shell: "nanocodex-just-bash",
              shell_network: multiplayer ? "public-http-only" : "connector-http-gateway",
              sandbox: "disabled",
              workspace: "/workspace",
              custom_commands: ["gh"],
              account: await currentAccountInfo(),
            }),
          },
        ],
      });
    } catch (error) {
      disposeWorkspace();
      throw error;
    }
    this.#logCapacity("agent_constructed", {
      construction_ms: Math.round((performance.now() - constructionStartedAt) * 100) / 100,
    });
    return agent;
  }

  async #ensureCredentialBinding(session: SessionRow): Promise<void> {
    if (this.#deleting) throw retryableError("agent is being deleted");
    let ownership = this.#credentialBinding;
    if (!ownership) {
      ownership = this.#bindingOwnershipForSession(session);
      await this.ctx.storage.put(CREDENTIAL_BINDING_KEY, ownership);
      this.#credentialBinding = ownership;
    }
    if (ownership.owner_id !== session.owner_id
      || ownership.session_id !== session.session_id
      || ownership.subject !== this.ctx.id.toString()) {
      throw new Error("credential binding ownership does not match the retained session");
    }
    await bindAgentCredential(
      this.env.NANOCODEX,
      ownership.subject,
      ownership.owner_id,
      this.#ownershipIoTimeoutMs(),
    );
    if (this.#deleting) throw retryableError("agent is being deleted");
  }

  #bindingOwnershipForSession(session: SessionRow): CredentialBindingOwnership {
    return {
      cleanup_at: Date.now(),
      owner_id: session.owner_id,
      session_id: session.session_id,
      state: "active",
      subject: this.ctx.id.toString(),
    };
  }

  async #complete(id: string, turn: Turn): Promise<void> {
    let reopenAgent = false;
    try {
      let materialized = await materializeTurnTerminal(id, turn);
      if (this.#deleting) return;
      if (this.#reopenInterruptedTurnIds.has(id)
        && materialized.terminal.type === "turn_cancelled") {
        materialized = {
          terminal: {
            type: "turn_retryable",
            id,
            error: "turn was interrupted while reopening the durable Agent",
          },
          reopenAgent: false,
        };
      }
      reopenAgent = materialized.reopenAgent;
      try {
        this.#commitManagedMessage(id, materialized.terminal);
      } catch (error) {
        if (this.#deleting) return;
        try {
          this.#commitManagedMessage(id, {
            type: "turn_retryable",
            id,
            error: `terminal projection failed: ${errorMessage(error)}`,
          });
        } catch (retryError) {
          this.#failEventStream(retryError);
        }
      }
    } finally {
      this.#turns.delete(id);
      this.#reopenInterruptedTurnIds.delete(id);
      this.#turnInputs.delete(id);
      turn.dispose();
      if (!this.#deleting) {
        if (reopenAgent) await this.#reopenAgent(id);
        this.#scheduleRecovery();
        await this.#scheduleNextAlarm();
      }
    }
  }

  #commitManagedFailure(
    id: string,
    error: unknown,
    _replayed: boolean,
    classified?: TurnTerminal,
  ): ManagedTurnRow {
    const failure = classified ?? classifyTurnFailure(id, error).terminal;
    const row = this.#managedTurn(id);
    if (row?.state === "cancelling"
      && failure.type !== "turn_cancelled"
      && failure.type !== "turn_blocked") {
      return this.#commitManagedMessage(id, {
        type: "turn_cancelling",
        id,
        error: "error" in failure ? failure.error : errorMessage(error),
      });
    }
    return this.#commitManagedMessage(id, failure);
  }

  #commitManagedMessage(id: string, requested: ManagedTransition): ManagedTurnRow {
    const original = this.#managedTurn(id);
    if (!original) throw new Error(`managed turn ${id} does not exist`);
    const now = Date.now();
    let event: DurableEvent<StreamMessage> | undefined;
    let committed = original;
    this.ctx.storage.transactionSync(() => {
      const row = this.#managedTurn(id);
      if (!row) throw new Error(`managed turn ${id} disappeared`);
      if (isTerminalState(row.state) || row.state === "blocked") {
        committed = row;
        return;
      }

      let message: ManagedTransition = requested;
      let state = managedStateForMessage(message);
      if (row.state === "cancelling" && state === "retryable") {
        message = {
          type: "turn_cancelling",
          id,
          error: "error" in requested ? requested.error : "cancellation will be retried",
        };
        state = "cancelling";
      }
      let attemptCount = row.attempt_count;
      let retryAt: number | null = null;
      const retrying = state === "retryable"
        || (state === "cancelling" && "error" in message && message.error !== undefined);
      if (retrying) {
        const detail = "error" in message ? message.error ?? null : null;
        if (row.state === state && row.error === detail && row.retry_at !== null && row.retry_at > now) {
          committed = row;
          return;
        }
        attemptCount = Math.min(Number.MAX_SAFE_INTEGER, attemptCount + 1);
        retryAt = now + retryDelayMs(attemptCount);
        if (message.type === "turn_cancelling") message = { ...message, retry_at: retryAt };
        if (row.state === state) {
          this.ctx.storage.sql.exec(
            `UPDATE managed_turns
             SET error = ?, attempt_count = ?, retry_at = ?, updated_at = ?
             WHERE id = ? AND state = ?`,
            detail,
            attemptCount,
            retryAt,
            now,
            id,
            state,
          );
          this.ctx.storage.sql.exec(
            "UPDATE session_state SET last_active = ? WHERE singleton = 1",
            now,
          );
          committed = this.#managedTurn(id) ?? row;
          return;
        }
      }

      const terminal = isTerminalState(state);
      const detail = "error" in message ? message.error ?? null : null;
      const encoded = terminal ? JSON.stringify(message) : null;
      event = this.#eventLog.append(message, id, true);
      this.ctx.storage.sql.exec(
        `UPDATE managed_turns
         SET state = ?, terminal_json = ?, terminal_cursor = ?, error = ?,
             attempt_count = ?, retry_at = ?, updated_at = ?
         WHERE id = ? AND state NOT IN ('completed', 'cancelled', 'failed')`,
        state,
        encoded,
        terminal ? event.cursor : null,
        detail,
        attemptCount,
        retryAt,
        now,
        id,
      );
      this.ctx.storage.sql.exec(
        `UPDATE session_state
         SET completed_turns = completed_turns + ?,
             last_active = ?
         WHERE singleton = 1`,
        state === "completed" ? 1 : 0,
        now,
      );
      committed = this.#managedTurn(id) ?? row;
    });
    if (event) {
      this.#publish(event);
      if (isTerminalState(committed.state)) {
        this.#maybeLogTerminalCapacity();
        if (this.#turnArchive.needsSeal()) {
          void this.#sealTurnArchive(false).catch(() => {});
        }
      }
    }
    return committed;
  }

  #maybeLogTerminalCapacity(): void {
    const terminalRows = this.ctx.storage.sql.exec<{ rows: number }>(
      `SELECT COUNT(*) AS rows FROM managed_turns
       WHERE state IN ('completed', 'cancelled', 'failed')`,
    ).toArray()[0]?.rows ?? 0;
    if (terminalRows > 0 && Number.isInteger(Math.log2(terminalRows))) {
      this.#logCapacity("terminal_milestone", { terminal_milestone: terminalRows });
    }
  }

  #logCapacity(
    reason: "agent_constructed" | "archive_seal" | "idle_shutdown" | "terminal_milestone",
    dimensions: Record<string, number> = {},
  ): void {
    const sessionId = this.#sessionId();
    if (!sessionId) return;
    try {
      const capacity: ManagedCapacitySnapshot = managedCapacitySnapshot(
        this.ctx.storage,
        sessionId,
        this.#eventArchive.capacity(),
        this.#turnArchive.capacity(),
        this.#realtimeArchive.capacity(),
      );
      console.info({
        type: "managed.capacity",
        reason,
        agent_id: sessionId,
        ...dimensions,
        ...capacity,
      });
    } catch (error) {
      console.warn("managed capacity snapshot failed", errorMessage(error));
    }
  }

  #recordAgentEvent(event: AgentEvent): void {
    if (this.#deleting) return;
    if (this.#realtimeEventBuffer) {
      this.#realtimeEventBuffer.push(event);
      return;
    }
    let turnId = this.#eventTurnId;
    if (event.type === "run.started") {
      turnId = this.#eventTurnQueue.shift();
      this.#eventTurnId = turnId;
    } else if (
      (event.type === "run.completed" || event.type === "run.failed") &&
      turnId === undefined
    ) {
      // A retained operation replays only its raw terminal event. Preserve the
      // outer admission queue until that event arrives so a following run
      // cannot inherit the replayed operation's attribution.
      turnId = this.#eventTurnQueue.shift();
    }
    this.#recordAndBroadcast({ type: "event", event }, turnId ?? null);
    if (event.type === "run.completed" || event.type === "run.failed") {
      this.#eventTurnId = undefined;
    }
  }

  #releaseEventTurn(id: string): void {
    if (this.#eventTurnId === id) this.#eventTurnId = undefined;
    const queued = this.#eventTurnQueue.indexOf(id);
    if (queued >= 0) this.#eventTurnQueue.splice(queued, 1);
  }

  #takeRealtimeEventBuffer(): AgentEvent[] {
    const buffered = this.#realtimeEventBuffer ?? [];
    this.#realtimeEventBuffer = undefined;
    return buffered;
  }

  #recordAndBroadcast(
    message: StreamMessage,
    turnId: string | null = null,
  ): void {
    if (this.#deleting || this.#streamError) return;
    try {
      const event = this.ctx.storage.transactionSync(() =>
        this.#eventLog.append(message, turnId),
      );
      this.#publish(event);
    } catch (error) {
      this.#failEventStream(error);
    }
  }

  #failEventStream(error: unknown): void {
    if (this.#streamError) return;
    const detail = `event projection failed: ${errorMessage(error)}`;
    this.#streamError = detail;
    console.error(detail);
    let event: DurableEvent<StreamMessage> | undefined;
    try {
      this.ctx.storage.transactionSync(() => {
        this.ctx.storage.sql.exec(
          "UPDATE session_state SET stream_error = ?, last_active = ? WHERE singleton = 1",
          detail,
          Date.now(),
        );
        event = this.#eventLog.append({ type: "stream_failed", error: detail }, null, true);
      });
    } catch (projectionError) {
      console.error("failed to persist event stream failure", errorMessage(projectionError));
      return;
    }
    this.#publish(event!);
  }

  #publish(event: DurableEvent<StreamMessage>): void {
    this.#eventLog.publish(event);
    this.#broadcast({
      ...event.message,
      cursor: event.cursor,
      ...(event.turn_id === null ? {} : { turn_id: event.turn_id }),
    });
    if (this.#eventArchive.needsSeal(this.#eventLog)) {
      void this.#sealEventArchive(false).catch(() => {});
    }
  }

  #sealEventArchive(force: boolean): Promise<ManagedEventSealResult> {
    if (this.#deleting) return Promise.reject(new Error("agent deletion fenced event archival"));
    const active = this.#eventArchiveTask;
    if (active) {
      return force ? active.then(() => this.#sealEventArchive(true)) : active;
    }
    const started = performance.now();
    const observed = this.#eventArchive.seal(force).then((result) => {
      if (result.sealed) {
        this.#logCapacity("archive_seal", {
          archived_bytes: result.archived_bytes,
          archived_events: result.archived_events,
          index_node_created: result.index_node_created ? 1 : 0,
          seal_ms: Math.round((performance.now() - started) * 100) / 100,
        });
      }
      return result;
    }).catch((error) => {
      console.warn("managed event archive seal failed", errorMessage(error));
      throw error;
    });
    this.#eventArchiveTask = observed;
    void observed.finally(() => {
      if (this.#eventArchiveTask === observed) this.#eventArchiveTask = undefined;
    }).catch(() => {});
    this.ctx.waitUntil(observed.catch(() => {}));
    return observed;
  }

  #sealTurnArchive(force: boolean): Promise<ManagedTurnSealResult> {
    if (this.#deleting) return Promise.reject(new Error("agent deletion fenced turn archival"));
    const active = this.#turnArchiveTask;
    if (active) {
      return force ? active.then(() => this.#sealTurnArchive(true)) : active;
    }
    const started = performance.now();
    const observed = this.#turnArchive.seal(force).then((result) => {
      if (result.sealed) {
        this.#logCapacity("archive_seal", {
          archived_receipt_bytes: result.archived_bytes,
          archived_receipts: result.archived_receipts,
          archived_receipt_objects: result.objects,
          seal_ms: Math.round((performance.now() - started) * 100) / 100,
        });
      }
      return result;
    }).catch((error) => {
      console.warn("managed turn archive seal failed", errorMessage(error));
      throw error;
    });
    this.#turnArchiveTask = observed;
    void observed.finally(() => {
      if (this.#turnArchiveTask === observed) this.#turnArchiveTask = undefined;
    }).catch(() => {});
    this.ctx.waitUntil(observed.catch(() => {}));
    return observed;
  }

  #sealRealtimeArchive(force: boolean): Promise<ManagedRealtimeSealResult> {
    if (this.#deleting) {
      return Promise.reject(new Error("agent deletion fenced realtime archival"));
    }
    const active = this.#realtimeArchiveTask;
    if (active) {
      return force ? active.then(() => this.#sealRealtimeArchive(true)) : active;
    }
    const started = performance.now();
    const observed = this.#realtimeArchive.seal(force).then((result) => {
      if (result.sealed) {
        this.#logCapacity("archive_seal", {
          archived_realtime_bytes: result.archived_bytes,
          archived_realtime_receipts: result.archived_receipts,
          archived_realtime_objects: result.objects,
          seal_ms: Math.round((performance.now() - started) * 100) / 100,
        });
      }
      return result;
    }).catch((error) => {
      console.warn("managed realtime archive seal failed", errorMessage(error));
      throw error;
    });
    this.#realtimeArchiveTask = observed;
    void observed.finally(() => {
      if (this.#realtimeArchiveTask === observed) this.#realtimeArchiveTask = undefined;
    }).catch(() => {});
    this.ctx.waitUntil(observed.catch(() => {}));
    return observed;
  }

  async #stop(strictShutdown = false): Promise<void> {
    const shutdown = this.#shutdownAgent(strictShutdown);
    const cancellations = [...this.#turns.values()].map(async (turn) => {
      try { await turn.cancel(); } catch { /* A terminal turn needs no cancellation. */ }
    });
    await Promise.all(cancellations);
    await shutdown;
    await Promise.allSettled([...this.#inFlight]);
    this.#turns.clear();
    this.#reopenInterruptedTurnIds.clear();
    this.#eventTurnQueue.length = 0;
    this.#eventTurnId = undefined;
    this.#pendingTurnIds.clear();
    this.#turnInputs.clear();
  }

  async #shutdownAgent(strict = false): Promise<void> {
    let shutdown = this.#agentShutdownPromise;
    if (!shutdown) {
      const agent = this.#agent;
      const construction = this.#agentConstruction;
      this.#runtimeOwnershipGeneration += 1;
      this.#agent = undefined;
      this.#agentPromise = undefined;
      this.#agentConstruction = undefined;
      this.#events?.off();
      this.#events = undefined;
      if (!agent && !construction) return;
      shutdown = (async () => {
        if (agent) await agent.session.shutdown();
        else if (construction) await this.#retireAgentConstruction(construction);
      })();
      this.#agentShutdownPromise = shutdown;
      void shutdown.finally(() => {
        if (this.#agentShutdownPromise === shutdown) this.#agentShutdownPromise = undefined;
      }).catch(() => {});
    }
    try {
      await shutdown;
    } catch (error) {
      if (strict) throw error;
      console.error("Nanocodex agent shutdown failed", errorMessage(error));
    }
    this.#events?.off();
    this.#events = undefined;
  }

  async #reopenAgent(failedId: string): Promise<void> {
    for (const siblingId of this.#turns.keys()) {
      if (siblingId !== failedId) this.#reopenInterruptedTurnIds.add(siblingId);
    }
    await this.#shutdownAgent();
    this.#eventTurnQueue.length = 0;
    this.#eventTurnId = undefined;
  }

  #session(): SessionRow | undefined {
    return this.ctx.storage.sql
      .exec<SessionRow>(
        `SELECT session_id, owner_id, public_origin, runtime_profile, completed_turns, last_active, stream_error
       FROM session_state WHERE singleton = 1`,
      )
      .toArray()[0];
  }

  #initializationOwnership(): SessionInitializationOwnership | undefined {
    return this.ctx.storage.sql
      .exec<SessionInitializationOwnership>(
        `SELECT session_id, owner_id, runtime_profile, state
       FROM session_initialization_ownership WHERE singleton = 1`,
      )
      .toArray()[0];
  }

  #sessionId(): string | undefined {
    return this.ctx.storage.sql
      .exec<{ session_id: string }>(
        "SELECT session_id FROM session_state WHERE singleton = 1",
      )
      .toArray()[0]?.session_id;
  }

  #sessionStatus(): SessionStatusRow | undefined {
    return this.ctx.storage.sql
      .exec<SessionStatusRow>(
        `SELECT session_id, completed_turns > 0 AS has_snapshot, completed_turns,
              last_active, stream_error
       FROM session_state WHERE singleton = 1`,
      )
      .toArray()[0];
  }

  #managedTurn(id: string): ManagedTurnRow | undefined {
    return this.#managedTurns("WHERE id = ?", id)[0];
  }

  async #findManagedTurn(id: string): Promise<ManagedTurnRow | undefined> {
    return this.#managedTurn(id) ?? await this.#archivedTurnById(id);
  }

  async #findManagedTurnByRequestKey(
    requestKey: string,
  ): Promise<ManagedTurnRow | undefined> {
    return this.#managedTurnByRequestKey(requestKey)
      ?? await this.#archivedTurnByRequestKey(requestKey);
  }

  async #archivedTurnById(id: string): Promise<ManagedTurnReceipt | undefined> {
    try { return await this.#turnArchive.findById(id); }
    catch (error) {
      throw new ManagedRequestError(
        503,
        "turn_archive_unavailable",
        `archived turn lookup failed: ${errorMessage(error)}`,
      );
    }
  }

  async #archivedTurnByRequestKey(
    requestKey: string,
  ): Promise<ManagedTurnReceipt | undefined> {
    try { return await this.#turnArchive.findByRequestKey(requestKey); }
    catch (error) {
      throw new ManagedRequestError(
        503,
        "turn_archive_unavailable",
        `archived idempotency lookup failed: ${errorMessage(error)}`,
      );
    }
  }

  #managedRealtimeOperation(
    voiceSessionId: string,
    operationId: string,
  ): ManagedRealtimeOperationRow | undefined {
    return this.ctx.storage.sql
      .exec<ManagedRealtimeOperationRow>(
        `SELECT voice_session_id, operation_id, kind, request_hash, state, response_json
       FROM managed_realtime_operations
       WHERE voice_session_id = ? AND operation_id = ?`,
        voiceSessionId,
        operationId,
      )
      .toArray()[0];
  }

  #managedRealtimeSession(): ManagedRealtimeSessionRow | undefined {
    return this.ctx.storage.sql
      .exec<ManagedRealtimeSessionRow>(
        "SELECT voice_session_id FROM managed_realtime_session WHERE singleton = 1",
      )
      .toArray()[0];
  }

  async #endManagedRealtimeSession(
    agent: CloudflareAgent.Agent,
    voiceSessionId: string,
  ): Promise<AgentSessionContext> {
    const context = await agent.session.realtime.end();
    assertBoundedRealtimeContext(context);
    this.ctx.storage.sql.exec(
      "DELETE FROM managed_realtime_session WHERE singleton = 1 AND voice_session_id = ?",
      voiceSessionId,
    );
    return context;
  }

  #firstPrompt(): string {
    return this.ctx.storage.sql.exec<{ first_prompt: string }>(
      "SELECT first_prompt FROM session_state WHERE singleton = 1",
    ).toArray()[0]?.first_prompt ?? "";
  }

  #managedTurnByRequestKey(requestKey: string): ManagedTurnRow | undefined {
    return this.#managedTurns("WHERE request_key = ?", requestKey)[0];
  }

  #managedTurns(
    clause: string,
    ...args: (string | number | null)[]
  ): ManagedTurnRow[] {
    return this.ctx.storage.sql
      .exec<ManagedTurnRow>(
        `SELECT id, request_key, request_hash, input_json, state,
              CAST(accepted_cursor AS TEXT) AS accepted_cursor,
              terminal_json, CAST(terminal_cursor AS TEXT) AS terminal_cursor,
              error, may_have_inner_operation, attempt_count, CAST(retry_at AS INTEGER) AS retry_at,
              created_at, accepted_at, updated_at
       FROM managed_turns ${clause}`,
      ...args,
    ).toArray();
  }

  #unfinishedTurnCount(): number {
    return this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_turns WHERE state IN ('accepted', 'cancelling', 'retryable', 'blocked')",
    ).toArray()[0]?.count ?? 0;
  }

  #recoverableTurnCount(): number {
    return this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_turns WHERE state IN ('accepted', 'cancelling', 'retryable')",
    ).toArray()[0]?.count ?? 0;
  }

  #conversationSummary(): { title: string; turnCount: number } {
    const row = this.ctx.storage.sql.exec<{ accepted_turns: number; first_prompt: string }>(
      "SELECT accepted_turns, first_prompt FROM session_state WHERE singleton = 1",
    ).one();
    return {
      title: conversationTitle(row.first_prompt),
      turnCount: row.accepted_turns,
    };
  }

  async #scheduleNextAlarm(): Promise<void> {
    if (this.#deleting || !this.#sessionId()) return;
    const now = Date.now();
    const targets: number[] = [];
    if (this.#eventArchive.needsSeal(this.#eventLog)
      || this.#turnArchive.needsSeal()
      || this.#realtimeArchive.needsSeal()) {
      targets.push(now + 1);
    }
    if (this.#agent || this.#agentPromise || this.#turns.size > 0 || this.#pendingTurnIds.size > 0) {
      targets.push(now + this.#idleTimeoutMs());
    }
    if (!this.#streamError) {
      for (const row of this.#managedTurns(
        "WHERE state IN ('accepted', 'cancelling', 'retryable') ORDER BY created_at",
      )) {
        if (row.state === "cancelling") {
          if (!this.#cancellationTasks.has(row.id)) {
            targets.push(row.retry_at ?? now + 1);
          }
          break;
        }
        const admissionOwned = this.#turns.has(row.id)
          || this.#pendingTurnIds.has(row.id)
          || this.#admissionTasks.has(row.id);
        if (admissionOwned) {
          if (row.may_have_inner_operation === 1) continue;
          break;
        }
        if (this.#cancellationTasks.has(row.id)) break;
        if (row.state === "retryable" && row.retry_at !== null) targets.push(row.retry_at);
        else targets.push(now + 1);
        break;
      }
    }
    if (targets.length === 0) {
      await this.ctx.storage.deleteAlarm();
      return;
    }
    await this.ctx.storage.setAlarm(Math.max(now + 1, Math.min(...targets)));
  }

  #capabilities(): AgentCapabilities {
    return AGENT_CAPABILITIES;
  }

  #track<Result>(task: Promise<Result>): Promise<Result> {
    this.#inFlight.add(task);
    void task.finally(() => this.#inFlight.delete(task)).catch(() => {});
    return task;
  }

  #activeTurnIds(): string[] {
    return [...this.#pendingTurnIds, ...this.#turns.keys()];
  }

  #activeTurnDetails(): ActiveTurn[] {
    return this.#activeTurnIds().flatMap((id) => {
      const input = this.#turnInputs.get(id);
      return input === undefined ? [] : [{ id, input }];
    });
  }

  #idleTimeoutMs(): number {
    const configured = Number(this.env.AGENT_IDLE_TIMEOUT_MS ?? 30_000);
    return Number.isFinite(configured) ? Math.min(15 * 60_000, Math.max(1_000, configured)) : 30_000;
  }

  #ownershipIoTimeoutMs(): number {
    return managedOwnershipTimeoutMs(this.env);
  }

  #credentialPreparationLeaseMs(): number {
    // Credential binding owns three bounded downstream attempts. Keep the
    // watchdog beyond that entire stage, including scheduler jitter.
    return Math.max(
      CREDENTIAL_BINDING_PREPARE_TIMEOUT_MS,
      this.#ownershipIoTimeoutMs() * 4,
    );
  }

  #markInitializationDeleted(): void {
    this.ctx.storage.transactionSync(() => {
      const ownership = this.#initializationOwnership();
      if (ownership) {
        this.ctx.storage.sql.exec(
          `UPDATE session_initialization_ownership
           SET state = 'deleted' WHERE singleton = 1`,
        );
      } else {
        this.ctx.storage.sql.exec(
          `INSERT INTO session_initialization_ownership (
             singleton, session_id, owner_id, runtime_profile, state
           ) VALUES (1, NULL, NULL, NULL, 'deleted')`,
        );
      }
    });
    this.#deleted = true;
  }

  async #refreshCredentialPreparation(): Promise<CredentialBindingOwnership | undefined> {
    const current = this.#credentialBinding;
    if (!current || current.state !== "preparing") return current;
    let retained: CredentialBindingOwnership | undefined;
    await this.ctx.storage.transaction(async (transaction) => {
      const stored = await transaction.get<CredentialBindingOwnership>(CREDENTIAL_BINDING_KEY);
      if (!stored || stored.state !== "preparing") {
        retained = stored;
        return;
      }
      retained = {
        ...stored,
        cleanup_at: Math.max(
          stored.cleanup_at,
          Date.now() + this.#credentialPreparationLeaseMs(),
        ),
      };
      await transaction.put(CREDENTIAL_BINDING_KEY, retained);
      await transaction.setAlarm(retained.cleanup_at);
    });
    const observed = this.#credentialBinding;
    if (!observed || observed.state === "active") return observed;
    this.#credentialBinding = retained;
    return this.#credentialBinding;
  }

  #broadcast(message: ServerMessage): void {
    this.#broadcastEncoded(JSON.stringify(message));
  }

  #broadcastEncoded(encoded: string): void {
    for (const socket of this.ctx.getWebSockets("client")) this.#sendEncoded(socket, encoded);
  }

  #send(socket: WebSocket, message: ServerMessage): void {
    this.#sendEncoded(socket, JSON.stringify(message));
  }

  #sendEncoded(socket: WebSocket, encoded: string): void {
    if (socket.readyState !== WebSocket.OPEN) return;
    try { socket.send(encoded); } catch { closeSocket(socket, 1011, "send failed"); }
  }
}

class ManagedRequestError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

function managedTurnView(row: ManagedTurnRow) {
  return {
    turn_id: row.id,
    state: row.state,
    input: JSON.parse(row.input_json) as PromptInput,
    accepted_cursor: row.accepted_cursor,
    terminal_cursor: row.terminal_cursor,
    created_at: row.created_at,
    accepted_at: row.accepted_at,
    updated_at: row.updated_at,
    attempt_count: row.attempt_count,
    retry_at: row.retry_at,
    ...(row.error === null ? {} : { error: row.error }),
    ...(row.terminal_json === null
      ? {}
      : { terminal: JSON.parse(row.terminal_json) as TurnTerminal }),
  };
}

function promptInputText(input: PromptInput): string {
  if (typeof input === "string") return input;
  return input.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    const value = item as unknown as Record<string, unknown>;
    if (value.type === "text" && typeof value.text === "string") return [value.text];
    if (value.type === "image") return ["[image]"];
    if (value.type === "audio") return ["[audio]"];
    return [];
  }).join("\n");
}

function conversationTitle(input: string): string {
  const text = input.replace(/\s+/g, " ").trim();
  if (!text) return "";
  return text.length > 56 ? `${text.slice(0, 55).trimEnd()}…` : text;
}

function asciiJsonHeaderValue(value: unknown): string {
  return JSON.stringify(value).replace(
    /[^\x20-\x7e]/g,
    (character) =>
      `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`,
  );
}

function assertBoundedRealtimeContext(context: AgentSessionContext): void {
  if (
    typeof context.workspace !== "string" ||
    !Array.isArray(context.history)
  ) {
    throw new ManagedRequestError(
      502,
      "invalid_agent_context",
      "agent returned an invalid session context",
    );
  }
  const encoded = JSON.stringify(context);
  if (encoder.encode(encoded).byteLength > MAX_REALTIME_CONTEXT_BYTES) {
    throw new ManagedRequestError(
      413,
      "context_too_large",
      `agent session context exceeds ${MAX_REALTIME_CONTEXT_BYTES} bytes`,
    );
  }
}

function messageForManagedTurn(row: ManagedTurnRow): ServerMessage {
  if (row.terminal_json !== null) {
    return {
      ...(JSON.parse(row.terminal_json) as TurnTerminal),
      ...(row.terminal_cursor === null ? {} : { cursor: row.terminal_cursor }),
    };
  }
  const input = JSON.parse(row.input_json) as PromptInput;
  if (row.state === "retryable") {
    return { type: "turn_retryable", id: row.id, error: row.error ?? "turn will be retried" };
  }
  if (row.state === "blocked") {
    return { type: "turn_blocked", id: row.id, error: row.error ?? "turn requires reconciliation" };
  }
  if (row.state === "cancelling") {
    return {
      type: "turn_cancelling",
      id: row.id,
      ...(row.error === null ? {} : { error: row.error }),
      ...(row.retry_at === null ? {} : { retry_at: row.retry_at }),
    };
  }
  return {
    type: "turn_accepted",
    id: row.id,
    input,
    replayed: true,
    ...(row.accepted_cursor === null ? {} : { cursor: row.accepted_cursor }),
  };
}

function isTerminalState(state: ManagedTurnState): boolean {
  return state === "completed" || state === "cancelled" || state === "failed";
}

function managedStateForMessage(message: ManagedTransition): ManagedTurnState {
  switch (message.type) {
    case "turn_cancelling": return "cancelling";
    case "turn_completed": return "completed";
    case "turn_cancelled": return "cancelled";
    case "turn_retryable": return "retryable";
    case "turn_blocked": return "blocked";
    case "turn_failed": return "failed";
  }
}

function retryableError(message: string): Error {
  return Object.assign(new Error(message), { code: "retryable" });
}

function retryDelayMs(attempt: number): number {
  return Math.min(MAX_RETRY_DELAY_MS, 1_000 * (2 ** Math.max(0, attempt - 1)));
}

function managedOwnershipTimeoutMs(env: Env): number {
  const configured = Number(env.MANAGED_OWNERSHIP_IO_TIMEOUT_MS ?? DEFAULT_OWNERSHIP_IO_TIMEOUT_MS);
  return Number.isFinite(configured)
    ? Math.min(CREDENTIAL_BINDING_PREPARE_TIMEOUT_MS, Math.max(1, configured))
    : DEFAULT_OWNERSHIP_IO_TIMEOUT_MS;
}

function optionalPositiveInteger(value: string | undefined): number | undefined {
  if (value === undefined || !/^[1-9][0-9]*$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

function managedMultiplayerTimeoutMs(env: Env): number {
  const configured = Number(env.MANAGED_MULTIPLAYER_IO_TIMEOUT_MS ?? DEFAULT_MULTIPLAYER_IO_TIMEOUT_MS);
  return Number.isFinite(configured)
    ? Math.min(60_000, Math.max(1, configured))
    : DEFAULT_MULTIPLAYER_IO_TIMEOUT_MS;
}

async function requestSessionCleanup(
  stub: DurableObjectStub<NanocodexSession>,
  timeoutMs: number,
): Promise<void> {
  try {
    const response = await fetchWithDeadline(
      stub,
      "https://session.internal/session",
      { method: "DELETE" },
      timeoutMs,
      "agent session cleanup",
    );
    await response.body?.cancel();
  } catch { /* A retained preparation/deletion marker owns later cleanup. */ }
}

async function fetchWithDeadline(
  binding: Pick<Fetcher, "fetch">,
  input: RequestInfo | URL,
  init: RequestInit,
  timeoutMs: number,
  operation: string,
): Promise<Response> {
  const controller = new AbortController();
  let timedOut = false;
  const pending = binding.fetch(input, { ...init, signal: controller.signal }).then((response) => {
    if (timedOut) void response.body?.cancel();
    return response;
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      timedOut = true;
      reject(new Error(`${operation} timed out after ${timeoutMs}ms`));
      controller.abort();
    }, timeoutMs);
  });
  try {
    return await Promise.race([pending, deadline]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

async function fetchCreateStage(
  binding: Pick<Fetcher, "fetch">,
  input: RequestInfo | URL,
  init: RequestInit,
  timeoutMs: number,
  operation: string,
  attempts = 2,
): Promise<Response> {
  let failure: unknown;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      const response = await fetchWithDeadline(binding, input, init, timeoutMs, operation);
      if (response.status !== 408 && response.status !== 429 && response.status < 500) {
        return response;
      }
      failure = new Error(`${operation} returned HTTP ${response.status}`);
      try { await response.body?.cancel(); } catch { /* Retrying owns the next attempt. */ }
    } catch (error) {
      failure = error;
    }
    if (attempt + 1 < attempts) {
      await scheduler.wait((50 * 2 ** attempt) + Math.floor(Math.random() * 50));
    }
  }
  throw failure;
}

function managedHttpError(error: unknown, fallbackCode = "managed_request_failed") {
  if (error instanceof ManagedRequestError) {
    return { status: error.status, code: error.code, message: error.message };
  }
  if (error instanceof EventLogCapacityError) {
    return { status: 507, code: error.code, message: error.message };
  }
  const code = (error as { code?: unknown } | null)?.code;
  if (code === "invalid_request") return { status: 400, code, message: errorMessage(error) };
  if (code === "conflict") return { status: 409, code, message: errorMessage(error) };
  if (code === "blocked") return { status: 409, code, message: errorMessage(error) };
  if (code === "retryable") return { status: 503, code, message: errorMessage(error) };
  return { status: 500, code: fallbackCode, message: errorMessage(error) };
}

function managedErrorResponse(error: unknown, fallbackCode?: string): Response {
  const failure = managedHttpError(error, fallbackCode);
  return json({ error: failure.code, message: failure.message }, { status: failure.status });
}

async function readBoundedRequestText(request: Request, limit: number): Promise<string> {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) {
    throw new ManagedRequestError(413, "request_too_large", `request exceeds ${limit} bytes`);
  }
  if (!request.body) return "";
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let total = 0;
  let text = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return text + decoder.decode();
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      throw new ManagedRequestError(413, "request_too_large", `request exceeds ${limit} bytes`);
    }
    text += decoder.decode(value, { stream: true });
  }
}

async function hashManagedInput(input: PromptInput): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(canonicalJson(input)));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object).sort().map((key) => (
    `${JSON.stringify(key)}:${canonicalJson(object[key])}`
  )).join(",")}}`;
}

function managedWebFetch(env: Env, subject: string): typeof fetch {
  return async (input, init) => {
    const incoming = new Request(input, init);
    const value = await incoming.json<{
      commands?: unknown;
      session_id?: unknown;
    }>();
    if (!value.commands || typeof value.commands !== "object" || Array.isArray(value.commands)
      || typeof value.session_id !== "string" || !value.session_id) {
      return json({ error: "invalid managed web request" }, { status: 400 });
    }
    return fetchManagedTool(env, subject, "/v1/search", {
      id: value.session_id,
      model: "gpt-5.6-sol",
      commands: value.commands,
      settings: { allowed_callers: ["direct"], external_web_access: true },
      max_output_tokens: 10_000,
    });
  };
}

function managedImageFetch(env: Env, subject: string): typeof fetch {
  return async (input, init) => {
    const incoming = new Request(input, init);
    const value = await incoming.json<{
      images?: unknown;
      prompt?: unknown;
    }>();
    const images = Array.isArray(value.images)
      ? value.images.filter((image): image is string => typeof image === "string")
      : [];
    if (typeof value.prompt !== "string" || !value.prompt.trim()
      || images.length > 5 || images.some((image) => !image.startsWith("data:image/"))) {
      return json({ error: "invalid managed image request" }, { status: 400 });
    }
    const upstream = await fetchManagedTool(
      env,
      subject,
      images.length ? "/v1/images/edits" : "/v1/images/generations",
      {
        ...(images.length ? { images: images.map((image_url) => ({ image_url })) } : {}),
        prompt: value.prompt.trim(),
        background: "auto",
        model: "gpt-image-2",
        quality: "auto",
        size: "auto",
      },
    );
    const payload = await upstream.json<{
      data?: Array<{ b64_json?: unknown }>;
      error?: unknown;
    }>().catch(() => undefined);
    if (!upstream.ok) {
      const error = payload?.error && typeof payload.error === "object"
        && !Array.isArray(payload.error)
        && typeof (payload.error as { message?: unknown }).message === "string"
        ? (payload.error as { message: string }).message
        : `HTTP ${upstream.status}`;
      return json({ error: `image generation failed: ${error}` }, { status: 502 });
    }
    const encoded = payload?.data?.[0]?.b64_json;
    return typeof encoded === "string" && encoded
      ? json({ image_url: `data:image/png;base64,${encoded}` })
      : json({ error: "image generation returned no image" }, { status: 502 });
  };
}

function fetchManagedTool(
  env: Env,
  subject: string,
  path: "/v1/search" | "/v1/images/generations" | "/v1/images/edits",
  body: unknown,
): Promise<Response> {
  return env.NANOCODEX.fetch(new Request(`https://nanocodex.internal${path}`, {
    method: "POST",
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "content-type": "application/json",
      "user-agent": "nanocodex-managed/0.1.0",
      "x-nanocodex-subject": subject,
    },
    body: JSON.stringify(body),
  }));
}

function authorized(request: Request, expected: string): boolean {
  const value = request.headers.get("authorization");
  return value !== null && value === `Bearer ${expected}`;
}

async function createMultiplayerRoom(
  request: Request,
  url: URL,
  env: Env,
  ownerId: string,
): Promise<Response> {
  if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
  if (!env.NANOCODEX_ADMIN_TOKEN) {
    return json({ error: "multiplayer is not configured" }, { status: 503 });
  }
  if (!request.body) return json({ error: "invalid_request" }, { status: 400 });

  let body: unknown;
  try {
    body = JSON.parse(await readBoundedRequestText(request, 4_096));
  } catch {
    return json({ error: "invalid_request" }, { status: 400 });
  }
  if (!body || typeof body !== "object" || Array.isArray(body)
    || Object.keys(body).some((key) => ![
      "create_id",
      "display_name",
    ].includes(key))) {
    return json({ error: "invalid_request" }, { status: 400 });
  }
  const creation = body as {
    create_id?: unknown;
    display_name?: unknown;
  };
  let createId: string;
  let ownerName: string;
  try {
    createId = validateCreateId(creation.create_id);
    ownerName = creation.display_name === undefined
      ? "Host"
      : validateDisplayName(creation.display_name);
  } catch {
    return json({ error: "invalid_request" }, { status: 400 });
  }
  const publicOrigin = url.origin;

  const [
    roomUuid,
    agentId,
    creatorMemberId,
    invite,
    memberToken,
    createIdHash,
    requestHash,
  ] = await Promise.all([
    scopedRuntimeId(
      env.NANOCODEX_ADMIN_TOKEN,
      `nanocodex-multiplayer-create-room-v1:${createId}`,
    ),
    scopedRuntimeId(
      env.NANOCODEX_ADMIN_TOKEN,
      `nanocodex-multiplayer-create-agent-v1:${createId}`,
    ),
    scopedRuntimeId(
      env.NANOCODEX_ADMIN_TOKEN,
      `nanocodex-multiplayer-create-member-v1:${createId}`,
    ),
    scopedCapability(
      env.NANOCODEX_ADMIN_TOKEN,
      `nanocodex-multiplayer-create-invite-v1:${createId}`,
    ),
    scopedCapability(
      env.NANOCODEX_ADMIN_TOKEN,
      `nanocodex-multiplayer-create-member-cookie-v1:${createId}`,
    ),
    hashText(`nanocodex-multiplayer-create-id-v1\n${createId}`),
    hashText(`nanocodex-multiplayer-create-request-v1\n${ownerId}\n${publicOrigin}\n${ownerName}`),
  ]);
  const roomId = await signedRoomRouteId(env.NANOCODEX_ADMIN_TOKEN, roomUuid);
  const quota = env.NANOCODEX_MULTIPLAYER_QUOTA.getByName("global");
  const room = env.NANOCODEX_ROOMS.getByName(roomId);
  const timeoutMs = managedMultiplayerTimeoutMs(env);
  let reservation: Readonly<{
    kind: "reserved";
  }> | Readonly<{
    kind: "rejected";
    retryAfter: string | null;
    status: number;
  }>;
  try {
    reservation = await fetchResponseWithDeadline(
      quota,
      "https://quota.internal/rooms",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          room_id: roomId,
          expires_at: Date.now() + MULTIPLAYER_ROOM_LEASE_MS,
          create_id_hash: createIdHash,
          request_hash: requestHash,
        }),
      },
      timeoutMs,
      "multiplayer quota reservation",
      async (response) => {
        if (!response.ok) {
          return {
            kind: "rejected" as const,
            retryAfter: response.headers.get("retry-after"),
            status: response.status,
          };
        }
        const value = await response.json<unknown>();
        if (!value || typeof value !== "object" || Array.isArray(value)
          || (value as Record<string, unknown>).room_id !== roomId
          || !Number.isSafeInteger((value as Record<string, unknown>).expires_at)) {
          throw new Error("invalid quota response");
        }
        return { kind: "reserved" as const };
      },
    );
  } catch {
    return json({ error: "multiplayer_capacity_unavailable" }, { status: 503 });
  }
  if (reservation.kind === "rejected") {
    if (reservation.status === 409) {
      return json({ error: "create_id_conflict" }, { status: 409 });
    }
    const status = reservation.status === 429 ? 429 : 503;
    return json({
      error: status === 429
        ? "multiplayer_capacity_reached"
        : "multiplayer_capacity_unavailable",
    }, {
      status,
      ...(reservation.retryAfter ? { headers: { "retry-after": reservation.retryAfter } } : {}),
    });
  }

  let initialization: Readonly<{
    kind: "initialized";
    receipt: RoomInitializationReceipt;
  }> | Readonly<{
    kind: "rejected";
    status: number;
  }>;
  try {
    initialization = await fetchResponseWithDeadline(
      room,
      "https://room.internal/initialize",
      {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          room_id: roomId,
          agent_id: agentId,
          owner_id: ownerId,
          public_origin: publicOrigin,
          owner_name: ownerName,
          create_id_hash: createIdHash,
          request_hash: requestHash,
          invite,
          member_id: creatorMemberId,
          member_token: memberToken,
        }),
      },
      timeoutMs,
      "multiplayer room initialization",
      async (response) => {
        if (!response.ok) return { kind: "rejected" as const, status: response.status };
        const receipt = validateRoomInitializationReceipt(
          await response.json<unknown>(),
          roomId,
          publicOrigin,
        );
        if (receipt.invite !== invite
          || receipt.member_id !== creatorMemberId
          || receipt.member_token !== memberToken) {
          throw new Error("room receipt does not match deterministic credentials");
        }
        return { kind: "initialized" as const, receipt };
      },
    );
  } catch {
    return json({ error: "room_initialization_failed" }, { status: 503 });
  }
  if (initialization.kind === "rejected") {
    return initialization.status === 409
      ? json({ error: "create_id_conflict" }, { status: 409 })
      : json({ error: "room_initialization_failed" }, {
        status: initialization.status >= 500 ? 503 : 400,
      });
  }
  return roomCreationResponse(initialization.receipt, 201);
}

function validateRoomInitializationReceipt(
  value: unknown,
  expectedRoomId: string,
  expectedPublicOrigin?: string,
): RoomInitializationReceipt {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid room receipt");
  }
  const receipt = value as Record<string, unknown>;
  if (Object.keys(receipt).some((key) => ![
    "room_id",
    "invite",
    "member_id",
    "member_token",
    "public_origin",
  ].includes(key))
    || receipt.room_id !== expectedRoomId
    || typeof receipt.invite !== "string" || !AGENT_TOKEN.test(receipt.invite)
    || typeof receipt.member_id !== "string" || !UUID.test(receipt.member_id)
    || typeof receipt.member_token !== "string" || !AGENT_TOKEN.test(receipt.member_token)
    || typeof receipt.public_origin !== "string" || !validPublicOrigin(receipt.public_origin)
    || (expectedPublicOrigin !== undefined && receipt.public_origin !== expectedPublicOrigin)) {
    throw new Error("invalid room receipt");
  }
  return receipt as RoomInitializationReceipt;
}

function roomCreationResponse(receipt: RoomInitializationReceipt, status: 200 | 201): Response {
  const publicUrl = new URL(receipt.public_origin);
  const websocketUrl = new URL(`/v1/rooms/${receipt.room_id}/ws`, publicUrl);
  websocketUrl.protocol = websocketUrl.protocol === "https:" ? "wss:" : "ws:";
  return json({
    room_id: receipt.room_id,
    member_id: receipt.member_id,
    invite: receipt.invite,
    invite_url: new URL(
      `/multiplayer?room=${encodeURIComponent(receipt.room_id)}#invite=${encodeURIComponent(receipt.invite)}`,
      publicUrl,
    ).href,
    websocket_url: websocketUrl.href,
  }, {
    status,
    headers: {
      "set-cookie": roomMemberCookie(receipt.room_id, receipt.member_token, publicUrl),
    },
  });
}

async function hashText(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(value));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function authorizeAgent(
  request: Request,
  agentId: string,
  expected: string,
): "bearer" | "cookie" | undefined {
  if (authorized(request, expected)) return "bearer";
  if (cookieValue(request.headers.get("cookie"), agentCookieName(agentId)) === expected) return "cookie";
  return undefined;
}

async function signedRoomRouteId(secret: string, roomUuid: string): Promise<string> {
  return `${roomUuid}~${await scopedCapability(secret, `nanocodex-room-route:${roomUuid}`)}`;
}

async function validSignedRoomRouteId(secret: string, roomId: string): Promise<boolean> {
  const match = ROOM_ROUTE_ID.exec(roomId);
  if (!match) return false;
  let signature: Uint8Array;
  try {
    const encoded = match[2]!.replaceAll("-", "+").replaceAll("_", "/");
    const binary = atob(`${encoded}${"=".repeat((4 - encoded.length % 4) % 4)}`);
    signature = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    return false;
  }
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  return crypto.subtle.verify(
    "HMAC",
    key,
    signature,
    encoder.encode(`nanocodex-room-route:${match[1]}`),
  );
}

async function scopedCapability(secret: string, scope: string): Promise<string> {
  const signature = await scopedSignature(secret, scope);
  let binary = "";
  for (const byte of signature) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function scopedRuntimeId(secret: string, scope: string): Promise<string> {
  const bytes = (await scopedSignature(secret, scope)).slice(0, 16);
  bytes[6] = (bytes[6]! & 0x0f) | 0x70;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function scopedSignature(secret: string, scope: string): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(scope)));
}

function agentCookie(routeBase: string, agentId: string, token: string, url: URL): string {
  const secure = url.protocol === "https:";
  return `${agentCookieName(agentId)}=${token}; Path=${routeBase}/${agentId}; HttpOnly; SameSite=Strict; Max-Age=604800${secure ? "; Secure" : ""}`;
}

function agentCookieName(agentId: string): string {
  return `nanocodex_agent_${agentId}`;
}

function cookieValue(encoded: string | null, name: string): string | undefined {
  if (!encoded) return undefined;
  for (const field of encoded.split(";")) {
    const separator = field.indexOf("=");
    if (separator < 0 || field.slice(0, separator).trim() !== name) continue;
    const value = field.slice(separator + 1).trim();
    return AGENT_TOKEN.test(value) ? value : undefined;
  }
  return undefined;
}

function roomMemberCookie(roomId: string, token: string, url: URL): string {
  const secure = url.protocol === "https:";
  return `${roomCookieName(roomId)}=${token}; Path=/v1/rooms/${roomId}; HttpOnly; SameSite=Strict; Max-Age=604800${secure ? "; Secure" : ""}`;
}

function validPublicOrigin(value: string): boolean {
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol)
      && !url.username
      && !url.password
      && url.href === `${url.origin}/`;
  } catch {
    return false;
  }
}

function uuidV7(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let timestamp = BigInt(Date.now());
  for (let index = 5; index >= 0; index -= 1) {
    bytes[index] = Number(timestamp & 0xffn);
    timestamp >>= 8n;
  }
  bytes[6] = (bytes[6]! & 0x0f) | 0x70;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function idempotentAgentId(userId: string, requestKey: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest(
    "SHA-256",
    encoder.encode(`${userId}\0${requestKey}`),
  ));
  const bytes = digest.slice(0, 16);
  bytes[6] = (bytes[6]! & 0x0f) | 0x80;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function closeSocket(socket: WebSocket, code: number, reason: string): void {
  if (socket.readyState !== WebSocket.CONNECTING && socket.readyState !== WebSocket.OPEN) return;
  const standard = code >= 1000 && code <= 1014 && ![1004, 1005, 1006].includes(code);
  const safeCode = standard || (code >= 3000 && code <= 4999) ? code : 1011;
  socket.close(safeCode, reason.slice(0, 120));
}

async function readBoundedText(response: Response, limit: number): Promise<string> {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let total = 0;
  let body = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return body + decoder.decode();
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      return `${body}${decoder.decode(value.subarray(0, Math.max(0, limit - (total - value.byteLength))))}`;
    }
    body += decoder.decode(value, { stream: true });
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
