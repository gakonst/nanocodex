Warning: truncated output (original token count: 112726)
Total output lines: 10765

import { prepareEnvironment } from "./environment-setup";
import { SessionOperations } from "./session-operations";
import { accountToolsEnabled, configuredBootstrapPlan, parseConfiguration, type AgentConfiguration } from "./agent-configuration";
import { createHash } from "node:crypto";
import { initializeTurnInputs, inputChunks, lazyTurnInput, readTurnInput, storeTurnInput } from "./managed-turn-input";
import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import { ArchiveMaintenance } from "./archive-maintenance";
import { managedCredentialSubject, scopedManagedModelEgress, sessionCredentialOwner } from "./session-credential-ownership";
import { remoteICE } from "./hand-remote-ice";
import { REMOTE_VM_ASSERTION, type RemoteVMPublisher } from "./hand-remote";
import { serverHandTool } from "./ssh-hand-setup";
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
  NamedTool,
  PromptInput,
  ToolContext,
  Tools,
  Turn,
} from "nanocodex";
import { Agent as CloudflareAgent } from "nanocodex/cloudflare";
import { Agent as ManagedAgent } from "nanocodex/managed";
import { imageGeneration, updatePlan, viewImage, web } from "nanocodex/tools";
import { createWorkspaceFilesystem } from "nanocodex-tools";
import { SessionAttachments } from "./attachments";
import { createBrainWorkspace } from "./brain-workspace";
import { createBrainBucket } from "./brain-bucket";
import { browseX, X_API } from "nanocodex-tools/x";
import { managedCodeEvaluator } from "./code-evaluator";
import { CronTriggers, CRON_TRIGGER_ID, cronTriggerView, nextCronRun, parseCronTrigger, type CronTriggerConfig } from "./cron-triggers";
import { createCronTool } from "./cron-tool";
import {
  cloudflareSandboxTools,
  deleteCloudflareBrainWorkspace,
  deleteCloudflareSandboxWorkspace,
  destroyCloudflareSandbox,
  openSandboxPreviewCapability,
  prepareCloudflareSandboxHand,
  proxyCloudflareSandboxPreview,
  type CloudflareSandboxNamespaceMount,
} from "./sandbox-tools";
import {
  createNamespaceExecutionRuntime,
  isBrainExecution,
  machineMountRoot,
  type MachineToolResolver,
  type NamespaceMachine,
} from "./namespace-tools";
import {
  ContainerProxy,
  Sandbox,
  serveBrainFilesystem,
} from "./sandbox-runtime";
import {
  connectedManagedAccountMcps,
  createDefaultManagedTools,
  defaultManagedMcpServers,
  managedAccountMcpServerName,
  managedAccountMcpServers,
  type ManagedAccountMcpConnection,
} from "./default-mcp";
import {
  HostedToolsBroker,
  type HostedToolsLeasedAttachmentRenewal,
} from "./hosted-tools-broker";
import {
  AccountHostedTools,
  AccountHostedToolsProvider,
} from "./account-hosted-tools";
import { VmHostPool } from "./vm-host-pool";
import { isVmFactoryName } from "./vm-factory-name";
import {
  VM_HOST_ATTACHMENT_ROUTE,
  VM_HOST_DONOR,
  VM_HOST_POOL_AGENT,
  VM_HOST_POOL_LOCATOR,
  VM_HOST_POOL_OWNER,
  VM_HOST_POOL_SCOPE,
  VM_HOST_PUBLIC_ORIGIN,
} from "./vm-host-boundary";
import {
  hostedToolCatalogEntryAllowed,
  isAppToolCatalogDigest,
} from "./app-tool-catalog";
import type { HostedMachine, HostedToolCatalogEntry } from "./hosted-tools-protocol";
import {
  managedCapacitySnapshot,
  type ManagedCapacitySnapshot,
} from "./capacity";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";
import { drainRuntimeForDeletion } from "./deletion-runtime";
import { createManagedComputerRuntime } from "./computer-runtime";
import {
  createManagedBrowserRuntime,
  type ManagedBrowserRuntime,
} from "./browser-runtime";
import {
  exactConnectorAccess,
  type ManagedEgressConnectorId,
} from "./managed-egress";
import {
  CONNECTOR_CAPABILITY_IDS,
  type ConnectorConnectionSelection,
} from "./connector-status";
import {
  DurableEventLog,
  MAX_HISTORY_PAGE_SIZE,
  parseCursor,
  type DurableEvent,
  type DurableEventTail,
} from "./durable-events";
import { persistEventStreamFailure } from "./event-stream-failure";
import { watchManagedAgentFamilyEvents } from "./agent-event-watcher";
import {
  ManagedEventArchive,
  type ManagedEventArchiveState,
  type ManagedEventSealResult,
} from "./managed-event-archive";
import {
  ManagedTurnArchive,
  type ManagedTurnArchiveIdentity,
  type ManagedTurnReceipt,
  type ManagedTurnSealResult,
} from "./managed-turn-archive";
import {
  ManagedRealtimeArchive,
  type ManagedRealtimeArchiveState,
  type ManagedRealtimeReceipt,
  type ManagedRealtimeSealResult,
} from "./managed-realtime-archive";
import {
  ManagedPortabilityArchive,
  type ManagedPortableArchiveIdentity,
} from "./managed-portability-archive";
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
export { ContainerProxy, Sandbox };
export { CodemodeRuntime } from "agents/browser";

import {
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
  matchesDeviceHostLease,
  parseDeviceHostCommand,
  type DeviceHostCommand,
  type DeviceHostServerMessage,
} from "./device-host-protocol";
import {
  cancellationDeliveryMatchesLiveTurn,
  classifyTurnFailure,
  managedCancellationAlarmTarget,
  managedControlTransitionForResolution,
  materializeTurnResolution,
  type ManagedTurnTransition,
  type TurnResolution,
  type TurnTerminal,
} from "./turn-completion";
import {
  DEFAULT_AGENT_SETTINGS,
  isAgentModel,
  agentSettingsQuery,
  parseAgentCreateBody,
  parseAgentRunBody,
  parseAgentSettingsPatch,
  parseAgentSettingsQuery,
  parseCompleteAgentSettings,
  validateAgentSettings,
  type ManagedAgentSettings,
  type ManagedAgentSettingsPatch,
} from "./agent-settings";
import { initializeManagedAgentSettingsSchema } from "./agent-settings-schema";
import {
  bindAgentCredential,
  routeCredentialRequest,
  unbindAgentCredential,
} from "./credentials";
import { routeBrowserEgress } from "./browser-egress";
import {
  accountInfo,
  type AccountMachine,
} from "./account-info";
import { accountConnectorsTool } from "./account-connectors-tool";
import {
  MANAGED_CLOUDFLARE_PROVIDER,
  managedMountProviderResourceId,
  managedMountRoot,
  managedMountTool,
  type ManagedMountRequest,
  type ManagedMountResult,
} from "./mount-tool";
import { routeConnectorRequest } from "./connectors";
import {
  attachAgent,
  authenticate,
  detachAgent,
  forwardPrincipalAssertions,
  isOrganizationCapabilities,
  isUserId,
  listAgents,
  recordAgentActivity,
  recordAgentCronPresence,
  requireSameOriginMutation,
  routeAccountRequest,
  type AccountAuthEnv,
  type ConnectGrantSlice,
  type OrganizationCapability,
  type Principal,
} from "./account-auth";
import {
  chiefOfStaffIdentity,
  resolveChiefOfStaffIdentity,
  type ChiefOfStaffPrincipalEnv,
} from "./chief-of-staff-principal";
import { routeBrowserModel } from "./browser-model";
import { routeAccountLinkRequest } from "./account-links";
import {
  routeHostPrincipalRequest,
  type HostPrincipalEnv,
} from "./host-principals";
import { routeManagedRealtimeTransport } from "./managed-realtime-transport";
import {
  HistorySearchError,
  MAX_HISTORY_SEARCH_LIMIT,
  groupHistoryCitations,
  mergeHistoryCitations,
  parseHistoryFindSessionsInput,
  parseHistoryReadSessionInput,
  type FindSessionsToolResult,
  type HistoryCitation,
  type HistoryFindSessionsInput,
  type HistoryFindSessionsResponse,
  type HistoryProjection,
  type HistoryReadSessionInput,
  type HistoryReadSessionResponse,
} from "./history-search";
import {
  DurableMemoryError,
  MEMORY_TOOL_INSTRUCTIONS,
  parseMemoryKey,
  parseMemoryOperation,
  parseMemoryResult,
  type MemoryOperation,
  type MemoryResult,
} from "./durable-memory";
import { memorySessionTools } from "./memory-session-tools";
import { ManagedStartupContext } from "./startup-context";
import { MemoryScope } from "./memory-scope";
export { MemoryScope } from "./memory-scope";
export { AccountHostedTools } from "./account-hosted-tools";
export { VmHostPool } from "./vm-host-pool";
export { ApiKeyRecord, NonceStorage, Organization, UserAccount } from "./account-auth";

// Storage placement only: larger exact-replay receipts go directly to R2.
const INLINE_REALTIME_RESPONSE_BYTES = 512 * 1024;
const MAX_RETRY_DELAY_MS = 60_000;
const MAX_IMPORT_BATCHES_PER_CREATE = 4;
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SESSION_ID = UUID;
const CONNECT_SERVICE_ORIGIN = "https://nanocodex.internal";
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
const DURABILITY_EXPORTED_KEY = "nanocodex:durability-exported";
const DURABILITY_IMPORT_STATE_KEY = "nanocodex:durability-import-state";
const DURABILITY_IMPORT_RECEIPT_KEY = "nanocodex:durability-import-receipt";
const CREDENTIAL_BINDING_PREPARE_TIMEOUT_MS = 60_000;
const DEFAULT_OWNERSHIP_IO_TIMEOUT_MS = 10_000;
const DEFAULT_MULTIPLAYER_IO_TIMEOUT_MS = 10_000;
const MAX_CLEANUP_RETRY_MS = 60_000;
const SESSION_OWNER_ASSERTION = "x-nanocodex-owner-id";
const SESSION_CREATE_ID_ASSERTION = "x-nanocodex-create-session-id";
// ManagedTurnArchive owns the long-lived API projection. The portable Rust
// state keeps a bounded exact-replay window so cutovers do not call the model.
// The managed inbox/archive owns public exact-ID replay. Rust needs a short
// recovery tail, not hundreds of full-history checkpoints in Worker memory.
const MANAGED_TERMINAL_RECEIPT_RETENTION = 16;
const SESSION_ORGANIZATION_ASSERTION = "x-nanocodex-session-organization-id";
const SESSION_TEAM_ASSERTION = "x-nanocodex-session-team-id";
const SESSION_AUTHORIZATION_EPOCH_ASSERTION = "x-nanocodex-authorization-epoch";
const SESSION_CAPABILITIES_ASSERTION = "x-nanocodex-capabilities";
const CONNECT_GRANT_ID_ASSERTION = "x-nanocodex-connect-grant-id";
const CONNECT_CONNECTORS_ASSERTION = "x-nanocodex-connect-connectors";
const CONNECT_CONNECTOR_CONNECTIONS_ASSERTION = "x-nanocodex-connect-connector-connections";
const CONNECT_MCP_IDS_ASSERTION = "x-nanocodex-connect-mcp-ids";
const CONNECT_APP_TOOL_CATALOG_DIGEST_ASSERTION = "x-nanocodex-connect-app-tool-catalog-digest";
const MEMORY_ORGANIZATION_ASSERTION = "x-nanocodex-organization-id";
const MEMORY_TEAM_ASSERTION = "x-nanocodex-team-id";
const MEMORY_SUBJECT_ASSERTION = "x-nanocodex-subject-id";
const MEMORY_MUTATION_ASSERTION = "x-nanocodex-memory-mutation";
export interface Env extends
  AccountAuthEnv,
  ChiefOfStaffPrincipalEnv,
  HostPrincipalEnv {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
  NANOCODEX_TURN_KEY_ID?: string;
  NANOCODEX_TURN_API_TOKEN?: string;
  /** Multi-architecture desktop image, pinned by registry digest. */
  NANOCODEX_HAND_IMAGE?: string;
  NANOCODEX_VM_HOST_POOLS: DurableObjectNamespace<VmHostPool>;
  NANOCODEX_ROOMS: DurableObjectNamespace<MultiplayerRoom>;
  NANOCODEX_MULTIPLAYER_QUOTA: DurableObjectNamespace<MultiplayerQuota>;
  NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope>;
  NANOCODEX_SANDBOXES: DurableObjectNamespace<Sandbox>;
  NANOCODEX: Fetcher;
  NANOCODEX_X?: Fetcher;
  NANOCODEX_HISTORY: R2Bucket;
  NANOCODEX_WORKSPACES: R2Bucket;
  NANOCODEX_ADMIN_TOKEN: string;
  NANOCODEX_SYSTEM_HOST_TOKEN?: string;
  HISTORY_AI_SEARCH?: AiSearchInstance;
  BROWSER?: import("agents/browser").BrowserBinding;
  LOADER?: WorkerLoader;
  MANAGED_BROWSER_PROVIDER?: string;
  MANAGED_BROWSER_KEEP_ALIVE_MS?: string;
  MANAGED_BROWSER_TOOL_TIMEOUT_MS?: string;
  BROWSERBASE_API_KEY?: string;
  BROWSERBASE_PROJECT_ID?: string;
  AGENT_IDLE_TIMEOUT_MS?: string;
  MANAGED_MULTIPLAYER_IO_TIMEOUT_MS?: string;
  MANAGED_OWNERSHIP_IO_TIMEOUT_MS?: string;
  MANAGED_AGENT_DIRECT_CREDENTIALS?: string;
  MANAGED_EVENT_ARCHIVE_RECENT_EVENTS?: string;
  MANAGED_EVENT_ARCHIVE_SEGMENT_BYTES?: string;
  MANAGED_EVENT_ARCHIVE_THRESHOLD_BYTES?: string;
  MANAGED_TURN_ARCHIVE_RECENT_TURNS?: string;
  MANAGED_REALTIME_ARCHIVE_RECENT_OPERATIONS?: string;
  DEPLOYMENT_SHA?: string;
  NANOCODEX_SANDBOX_LOCAL?: string;
  NANOCODEX_SANDBOX_DESKTOPS?: string;
}

type SessionRow = {
  session_id: string;
  owner_id: string;
  organization_id: string;
  team_id: string;
  authorization_epoch: number;
  public_origin: string;
  runtime_profile: AgentRuntimeProfile;
  accepted_turns: number;
  completed_turns: number;
  last_active: number;
  stream_error: string | null;
};

type SessionInitialization = {
  session_id?: unknown;
  owner_id?: unknown;
  organization_id?: unknown;
  team_id?: unknown;
  authorization_epoch?: unknown;
  public_origin?: unknown;
  runtime_profile?: unknown;
  settings?: unknown;
  configuration?: unknown;
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
  accepted_turns: number;
  completed_turns: number;
  last_active: number;
  stream_error: string | null;
};

type AgentSettingsRow = {
  model: ManagedAgentSettings["model"];
  thinking: ManagedAgentSettings["thinking"];
  reasoning_mode: ManagedAgentSettings["reasoning_mode"];
  fast_mode: number;
};

type ManagedMountState = "mounting" | "mounted" | "failed";

type ManagedMountRow = {
  id: string;
  provider: string;
  name: string;
  root: string;
  provider_resource_id: string;
  configuration_json: string;
  state: ManagedMountState;
  created_at: number;
  updated_at: number;
};

type ManagedMountCallRow = {
  provider: string;
  name: string;
  mount_id: string;
  created: number;
};

type ManagedMountConfiguration = Readonly<{
  namespace_slot?: number;
  vm_factory_name?: string;
  vm_pool_locator?: string;
  vm_host?: Readonly<{
    pool_locator: string;
    allocation_id: string;
    generation: number;
    machine_id: string;
    route_id?: string;
  }>;
  [key: string]: unknown;
}>;

type VmHostPoolScope = "agent" | "account" | "system";

type VmHostAllocation = Readonly<{
  allocation_id: string;
  generation: number;
  factory_name: string;
  machine_id: string;
  host_id: string;
  slot: number;
  route_id: string;
}>;

type VmHostAttachmentGrant = Readonly<{
  valid: true;
  allocation_id: string;
  generation: number;
  agent_id: string;
  owner_id: string;
  organization_id: string;
  team_id: string;
  authorization_epoch: number;
  public_origin: string;
  machine_id: string;
  lease_expires_at: number;
  route_id: string;
}>;

type VmHostAttachmentRenewalClaim = Readonly<{
  pool_locator: string;
  allocation_id: string;
  generation: number;
  bearer: string;
}>;

function validVmHostAllocation(value: unknown): value is VmHostAllocation {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const allocation = value as Partial<VmHostAllocation>;
  return typeof allocation.allocation_id === "string" && UUID.test(allocation.allocation_id)
    && Number.isSafeInteger(allocation.generation) && Number(allocation.generation) >= 1
    && typeof allocation.factory_name === "string"
    && /^[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?$/.test(allocation.factory_name)
    && typeof allocation.machine_id === "string"
    && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/.test(allocation.machine_id)
    && typeof allocation.host_id === "string" && UUID.test(allocation.host_id)
    && Number.isSafeInteger(allocation.slot) && Number(allocation.slot) >= 0
    && typeof allocation.route_id === "string" && VM_HOST_ATTACHMENT_ROUTE.test(allocation.route_id);
}

function managedMountConfiguration(encoded: string): ManagedMountConfiguration {
  const value = JSON.parse(encoded) as unknown;
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("retained mount configuration is invalid");
  }
  return value as ManagedMountConfiguration;
}

function managedMountStorageProvider(provider: string): "cloudflare" | "host" {
  return provider === MANAGED_CLOUDFLARE_PROVIDER ? "cloudflare" : "host";
}

function sameManagedMountProvider(left: string, right: string): boolean {
  const normalized = (provider: string) => provider === "cloudflare"
    ? MANAGED_CLOUDFLARE_PROVIDER
    : provider;
  return normalized(left) === normalized(right);
}

function vmHostFactoryName(
  mount: Pick<ManagedMountRow, "configuration_json" | "provider">,
): string | undefined {
  if (mount.provider !== "host") return undefined;
  const name = managedMountConfiguration(mount.configuration_json).vm_factory_name;
  return isVmFactoryName(name) ? name : undefined;
}

function managedMountUsesProvider(mount: ManagedMountRow, provider: string): boolean {
  return mount.provider === "cloudflare"
    ? provider === MANAGED_CLOUDFLARE_PROVIDER
    : mount.provider === "host" && vmHostFactoryName(mount) === provider;
}

function managedMountPublicProvider(mount: ManagedMountRow): string {
  if (mount.provider === "cloudflare") return MANAGED_CLOUDFLARE_PROVIDER;
  const factoryName = vmHostFactoryName(mount);
  if (factoryName !== undefined) return factoryName;
  throw new Error("retained VM host mount has no valid factory name");
}

function vmHostMountAllocation(
  mount: Pick<ManagedMountRow, "configuration_json" | "provider">,
): ManagedMountConfiguration["vm_host"] | undefined {
  if (mount.provider !== "host") return undefined;
  const allocation = managedMountConfiguration(mount.configuration_json).vm_host;
  if (!allocation || typeof allocation !== "object"
    || typeof allocation.pool_locator !== "string"
    || !/^[A-Za-z0-9_-]{43}$/.test(allocation.pool_locator)
    || typeof allocation.allocation_id !== "string" || !UUID.test(allocation.allocation_id)
    || !Number.isSafeInteger(allocation.generation) || allocation.generation < 1
    || typeof allocation.machine_id !== "string"
    || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/.test(allocation.machine_id)
    || (allocation.route_id !== undefined
      && (typeof allocation.route_id !== "string"
        || !VM_HOST_ATTACHMENT_ROUTE.test(allocation.route_id)))) {
    return undefined;
  }
  return allocation;
}

type ManagedTurnState =
  | "accepted"
  | "cancelling"
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
  dispatch_input_chunks: number | null;
  may_have_inner_operation: number;
  authorization_json: string;
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
  blocked: number;
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
  authorization_json: string;
};

type ManagedRealtimeRouteResult = Readonly<{
  operation_id: string;
  route: "started" | "steered";
  turn_id: string;
  voice_session_id: string;
}>;

type TurnAuthorization = Readonly<{
  capabilities: readonly OrganizationCapability[];
  connectGrant?: ConnectGrantSlice;
}>;

type ManagedSubagentDescriptor = Readonly<{
  agentId: string;
  parentAgentId: string | null;
  sessionId: string;
  role: string;
  task: string;
}>;

type ManagedSubagentAuthorizationRow = ManagedSubagentDescriptor & Readonly<{
  authorization_json: string;
  host_context_ref: string;
  root_session_id: string;
}>;

type SessionSocketAttachment = Readonly<{
  sessionId: string;
  authorization: TurnAuthorization;
  replayAfter: string | null;
}>;

type HistoryProjectionOutboxRow = {
  source_cursor: string;
  turn_id: string;
  payload_json: string;
  attempt_count: number;
  retry_at: number;
};

type AgentRuntimeProfile = "managed" | "multiplayer";

type AgentConstructionOwnership = {
  readonly deletionGeneration: number;
  readonly runtimeGeneration: number;
  promise: Promise<CloudflareAgent.Agent>;
  publication: Promise<CloudflareAgent.Agent>;
  shutdown?: Promise<void>;
};

type DurabilityImportOwnership = Readonly<{
  deletionGeneration: number;
  promise: Promise<Response>;
}>;

type CredentialBindingOwnership = Readonly<{
  cleanup_at: number;
  owner_id: string;
  session_id: string;
  state: "preparing" | "active";
  subject: string;
  strategy?: "session_v1";
}>;

type PortableDurabilityArchive = Readonly<{
  records: readonly Readonly<{ key: string; value: string }>[];
  format: "nanocodex-durability-state-v2";
  payload: string;
  revision: string;
  stateId: string;
}>;

type ManagedDurabilityArchive = Readonly<{
  durability: PortableDurabilityArchive;
  format: "nanocodex-managed-durability-state-v2";
  managed_durability_records: ManagedPortableArchiveIdentity;
  managed_events: ManagedEventPortability;
  managed_realtime: ManagedRealtimePortability;
  managed_session: ManagedSessionPortability;
  managed_turn_receipts: ManagedTurnArchiveIdentity;
  source_agent_id: string;
}>;

type ManagedTurnArchiveAdoption = Readonly<{
  durability_records: ManagedPortableArchiveIdentity;
  events: ManagedEventPortability;
  realtime: ManagedRealtimePortability;
  session: ManagedSessionPortability;
  source_storage_id: string;
  turn_receipts: ManagedTurnArchiveIdentity;
}>;

type ManagedEventPortability = Readonly<{
  archive: ManagedPortableArchiveIdentity;
  state: ManagedEventArchiveState;
  tail: DurableEventTail<StreamMessage>;
}>;

type ManagedRealtimePortableOperation = Readonly<{
  blocked: 0 | 1;
  created_at: number;
  kind: ManagedRealtimeKind;
  operation_id: string;
  request_hash: string;
  response_json: string | null;
  state: "pending" | "completed";
  updated_at: number;
  voice_session_id: string;
}>;

type ManagedRealtimePortability = Readonly<{
  archive: ManagedPortableArchiveIdentity;
  state: ManagedRealtimeArchiveState;
  tail: readonly ManagedRealtimePortableOperation[];
}>;

type ManagedSessionPortability = Readonly<{
  accepted_turns: number;
  completed_turns: number;
  /** Display preview; full input is retained in accepted events and turn receipts. */
  first_prompt: string;
  last_active: number;
  stream_error: string | null;
  title: string;
  settings: ManagedAgentSettings;
}>;

type ManagedDurabilityImport = Readonly<{
  durability: unknown;
  turn_archive_adoption?: ManagedTurnArchiveAdoption;
}>;

type DurabilityImportReceipt = Readonly<{
  adoption?: ManagedTurnArchiveAdoption;
  owner_id: string;
  request_hash: string;
  source_agent_id: string | null;
  stage: "pending" | "authorized" | "complete";
  state_id: string;
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
  execution_environments: true,
  execution_namespace: "cwd-root-v1",
  native_cross_mounts: false,
}) satisfies AgentCapabilities;

const SANDBOX_HAND_CAPABILITIES = Object.freeze([
  "filesystem",
  "native-linux",
  "packages",
  "processes",
  "servers",
]);
// One alias per peer bucket is required by the Sandbox SDK mount protocol.
const CLOUDFLARE_NAMESPACE_BINDING_COUNT = 16;

const json = (body: unknown, init: ResponseInit = {}) => Response.json(body, {
  ...init,
  headers: { "cache-control": "no-store", ...init.headers },
});

function forwardedPrincipal(headers: Headers): Readonly<{
  ownerId: string;
  organizationId: string;
  teamId: string;
  authorizationEpoch: number;
  authorization: TurnAuthorization;
}> | undefined {
  const ownerId = headers.get(SESSION_OWNER_ASSERTION);
  const organizationId = headers.get(SESSION_ORGANIZATION_ASSERTION);
  const teamId = headers.get(SESSION_TEAM_ASSERTION);
  const encodedEpoch = headers.get(SESSION_AUTHORIZATION_EPOCH_ASSERTION);
  const encodedCapabilities = headers.get(SESSION_CAPABILITIES_ASSERTION);
  if (!isUserId(ownerId) || !organizationId || !UUID.test(organizationId)
    || !teamId || !UUID.test(teamId) || !encodedEpoch || !/^\d+$/u.test(encodedEpoch)
    || encodedCapabilities === null) return undefined;
  const authorizationEpoch = Number(encodedEpoch);
  if (!Number.isSafeInteger(authorizationEpoch) || authorizationEpoch < 1) return undefined;
  let authorization: TurnAuthorization;
  try {
    const grantId = headers.get(CONNECT_GRANT_ID_ASSERTION);
    const encodedConnectors = headers.get(CONNECT_CONNECTORS_ASSERTION);
    const encodedConnectorConnections = headers.get(CONNECT_CONNECTOR_CONNECTIONS_ASSERTION);
    const encodedMcpIds = headers.get(CONNECT_MCP_IDS_ASSERTION);
    const appToolCatalogDigest = headers.get(CONNECT_APP_TOOL_CATALOG_DIGEST_ASSERTION);
    const connectAssertions = [grantId, encodedConnectors, encodedMcpIds];
    if (connectAssertions.some((value) => value !== null)
      && connectAssertions.some((value) => value === null)) return undefined;
    if (encodedConnectorConnections !== null && grantId === null) return undefined;
    if (appToolCatalogDigest !== null && grantId === null) return undefined;
    authorization = parseTurnAuthorization(JSON.stringify({
      capabilities: JSON.parse(encodedCapabilities),
      ...(grantId === null ? {} : {
        connectGrant: {
          grantId,
          connectors: JSON.parse(encodedConnectors!),
          ...(encodedConnectorConnections === null ? {} : {
            connectorConnections: JSON.parse(encodedConnectorConnections),
          }),
          mcpIds: JSON.parse(encodedMcpIds!),
          ...(appToolCatalogDigest === null ? {} : { appToolCatalogDigest }),
        },
      }),
    }));
  } catch {
    return undefined;
  }
  return { ownerId, organizationId, teamId, authorizationEpoch, authorization };
}

function parseTurnAuthorization(encoded: string): TurnAuthorization {
  const value = JSON.parse(encoded) as unknown;
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).some((key) => key !== "capabilities" && key !== "connectGrant")
    || !isOrganizationCapabilities((value as { capabilities?: unknown }).capabilities)) {
    throw new Error("invalid turn authorization");
  }
  const parsed = value as {
    capabilities: OrganizationCapability[];
    connectGrant?: unknown;
  };
  if (parsed.connectGrant === undefined) return { capabilities: parsed.capabilities };
  if (!isConnectGrantSlice(parsed.connectGrant)) throw new Error("invalid turn authorization");
  return { capabilities: parsed.capabilities, connectGrant: parsed.connectGrant };
}

function managedSubagentDescriptor(value: unknown): ManagedSubagentDescriptor {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("invalid managed subagent descriptor");
  }
  const descriptor = value as Record<string, unknown>;
  if (Object.keys(descriptor).sort().join("\0")
      !== ["agentId", "parentAgentId", "role", "sessionId", "task"].sort().join("\0")
    || typeof descriptor.agentId !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/u.test(descriptor.agentId)
    || (descriptor.parentAgentId !== null
      && (typeof descriptor.parentAgentId !== "string"
        || !/^[A-Za-z0-9._:-]{1,128}$/u.test(descriptor.parentAgentId)))
    || typeof descriptor.sessionId !== "string" || !SESSION_ID.test(descriptor.sessionId)
    || typeof descriptor.role !== "string" || descriptor.role.length === 0
    || descriptor.role.includes("\0")
    || typeof descriptor.task !== "string" || descriptor.task.length === 0
    || descriptor.task.includes("\0")) {
    throw new TypeError("invalid managed subagent descriptor");
  }
  return Object.freeze({
    agentId: descriptor.agentId,
    parentAgentId: descriptor.parentAgentId,
    sessionId: descriptor.sessionId,
    role: descriptor.role,
    task: descriptor.task,
  }) as ManagedSubagentDescriptor;
}

// Authorization needs identity, not another retained copy of task content.
function descriptorDigest(value: string): string {
  return createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

/** One-time atomic conversion; runtime authorization has one digest format. */
export function initializeManagedSubagentDigests(storage: DurableObjectStorage): void {
  const columns = storage.sql.exec<{ name: string }>("PRAGMA table_info(managed_subagent_authorizations)");
  if (![...columns].some(({ name }) => name === "task")) return;
  storage.transactionSync(() => {
    storage.sql.exec("ALTER TABLE managed_subagent_authorizations RENAME COLUMN role TO role_digest");
    storage.sql.exec("ALTER TABLE managed_subagent_authorizations RENAME COLUMN task TO task_digest");
    // Iterate rows directly: never materialize all retained tasks together.
    for (const row of storage.sql.exec<{ session_id: string; role_digest: string; task_digest: string }>(
      "SELECT session_id, role_digest, task_digest FROM managed_subagent_authorizations",
    )) {
      storage.sql.exec(`UPDATE managed_subagent_authorizations SET role_digest = ?, task_digest = ? WHERE session_id = ?`,
        descriptorDigest(row.role_digest), descriptorDigest(row.task_digest), row.session_id);
    }
  });
}

function sameManagedSubagentDescriptor(
  row: ManagedSubagentAuthorizationRow,
  descriptor: ManagedSubagentDescriptor,
): boolean {
  return row.agentId === descriptor.agentId
    && row.parentAgentId === descriptor.parentAgentId
    && row.sessionId === descriptor.sessionId
    && row.role === descriptorDigest(descriptor.role)
    && row.task === descriptorDigest(descriptor.task);
}

/** Managed half of the private Cloudflare subagent lifecycle transaction. */
export function applyManagedSubagentLifecycle(
  storage: DurableObjectStorage,
  value: unknown,
): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("invalid managed subagent lifecycle event");
  }
  const event = value as Record<string, unknown>;
  const type = event.type;
  if ((type !== "bind" && type !== "reconstruct" && type !== "release")
    || typeof event.rootSessionId !== "string" || !SESSION_ID.test(event.rootSessionId)
    || typeof event.sessionId !== "string" || !SESSION_ID.test(event.sessionId)) {
    throw new TypeError("invalid managed subagent lifecycle event");
  }
  const rootSessionId = event.rootSessionId;
  const sessionId = event.sessionId;
  if (event.hostContextRef === undefined && (type === "reconstruct" || type === "release")) {
    const allowedKeys = type === "release"
      ? ["type", "rootSessionId", "sessionId", "hostContextRef"]
      : ["type", "rootSessionId", "sessionId", "descriptor", "hostContextRef"];
    if (Object.keys(event).some((key) => !allowedKeys.includes(key))) {
      throw new TypeError("invalid managed subagent lifecycle event");
    }
    if (type === "reconstruct") {
      const descriptor = managedSubagentDescriptor(event.descriptor);
      if (descriptor.sessionId !== sessionId) {
        throw new Error("managed subagent session does not match its descriptor");
      }
    }
    // Rows created before private host provenance cannot inherit authority.
    // Revoke any divergent retained snapshot while allowing the Cloudflare
    // adapter to reconstruct or release the child itself. This is idempotent,
    // and the absent managed row makes every capability lookup fail closed.
    storage.sql.exec(
      `DELETE FROM managed_subagent_authorizations
       WHERE session_id = ? AND root_session_id = ?`,
      sessionId,
      rootSessionId,
    );
    return;
  }
  if (typeof event.hostContextRef !== "string" || !TURN_ID.test(event.hostContextRef)) {
    throw new TypeError("invalid managed subagent lifecycle event");
  }
  const hostContextRef = event.hostContextRef;
  const retained = storage.sql.exec<ManagedSubagentAuthorizationRow>(
    `SELECT root_session_id, session_id AS sessionId, agent_id AS agentId,
            parent_agent_id AS parentAgentId, role_digest AS role, task_digest AS task, host_context_ref, authorization_json
     FROM managed_subagent_authorizations WHERE session_id = ?`,
    sessionId,
  ).toArray()[0];
  if (type === "release") {
    if (Object.keys(event).some((key) => !["type", "rootSessionId", "sessionId", "hostContextRef"].includes(key))
      || retained === undefined
      || retained.root_session_id !== rootSessionId
      || retained.host_context_ref !== hostContextRef) {
      throw new Error("managed subagent release does not match retained authorization");
    }
    storage.sql.exec(
      `DELETE FROM managed_subagent_authorizations
       WHERE session_id = ? AND root_session_id = ? AND host_context_ref = ?`,
      sessionId,
      rootSessionId,
      hostContextRef,
    );
    return;
  }
  if (Object.keys(event).some((key) => ![
    "type", "rootSessionId", "sessionId", "descriptor", "hostContextRef",
  ].includes(key))) {
    throw new TypeError("invalid managed subagent lifecycle event");
  }
  const descriptor = managedSubagentDescriptor(event.descriptor);
  if (descriptor.sessionId !== sessionId) {
    throw new Error("managed subagent session does not match its descriptor");
  }
  if (retained !== undefined) {
    if (retained.root_session_id !== rootSessionId
      || retained.host_context_ref !== hostContextRef
      || !sameManagedSubagentDescriptor(retained, descriptor)) {
      throw new Error("managed subagent binding conflicts with retained authorization");
    }
    return;
  }
  let authorizationJson: string;
  if (descriptor.parentAgentId === null) {
    const turn = storage.sql.exec<Pick<ManagedTurnRow, "authorization_json">>(
      "SELECT authorization_json FROM managed_turns WHERE id = ?",
      hostContextRef,
    ).toArray()[0];
    if (turn === undefined) throw new Error("managed subagent authorization turn is missing");
    authorizationJson = JSON.stringify(parseTurnAuthorization(turn.authorization_json));
  } else {
    const parent = storage.sql.exec<ManagedSubagentAuthorizationRow>(
      `SELECT authorization_json, host_context_ref
       FROM managed_subagent_authorizations
       WHERE root_session_id = ? AND agent_id = ?`,
      rootSessionId,
      descriptor.parentAgentId,
    ).toArray()[0];
    if (parent === undefined || parent.host_context_ref !== hostContextRef) {
      throw new Error("managed nested subagent authorization parent is missing");
    }
    authorizationJson = JSON.stringify(parseTurnAuthorization(parent.authorization_json));
  }
  storage.sql.exec(
    `INSERT INTO managed_subagent_authorizations
       (session_id, root_session_id, agent_id, parent_agent_id, role_digest, task_digest,
        host_context_ref, authorization_json, created_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    descriptor.sessionId,
    rootSessionId,
    descriptor.agentId,
    descriptor.parentAgentId,
    descriptorDigest(descriptor.role),
    descriptorDigest(descriptor.task),
    hostContextRef,
    authorizationJson,
    Date.now(),
  );
}

export function managedAuthorizationForToolContext(
  storage: DurableObjectStorage,
  rootSessionId: string | undefined,
  activeAuthorization: TurnAuthorization | undefined,
  context: Pick<ToolContext, "sessionId" | "subagent">,
): TurnAuthorization | undefined {
  if (context.subagent === undefined) {
    return rootSessionId !== undefined && context.sessionId === rootSessionId
      ? activeAuthorization
      : undefined;
  }
  let descriptor: ManagedSubagentDescriptor;
  try { descriptor = managedSubagentDescriptor(context.subagent); }
  catch { return undefined; }
  if (descriptor.sessionId !== context.sessionId) return undefined;
  const retained = storage.sql.exec<ManagedSubagentAuthorizationRow>(
    `SELECT root_session_id, session_id AS sessionId, agent_id AS agentId,
            parent_agent_id AS parentAgentId, role_digest AS role, task_digest AS task, host_context_ref, authorization_json
     FROM managed_subagent_authorizations WHERE session_id = ?`,
    context.sessionId,
  ).toArray()[0];
  if (retained === undefined || retained.root_session_id !== rootSessionId
    || !sameManagedSubagentDescriptor(retained, descriptor)) return undefined;
  try { return parseTurnAuthorization(retained.authorization_json); }
  catch { return undefined; }
}

export function turnCanUseExecutionNamespace(
  authorization: Pick<TurnAuthorization, "capabilities"> & { connectGrant?: unknown } | undefined,
): boolean {
  return authorization !== undefined
    // Retained execution hands are available only to full account authority.
    && authorization.connectGrant === undefined
    && authorization.capabilities.includes("agents:write")
    && authorization.capabilities.includes("tools:use");
}

export function turnControlAuthorizationMatches(
  retained: TurnAuthorization,
  requester: TurnAuthorization,
): boolean {
  if (retained.connectGrant === undefined && requester.connectGrant === undefined) return true;
  return JSON.stringify(retained) === JSON.stringify(requester);
}

export function createSharedBrainReadWorkspace(
  bucket: R2Bucket,
  resourceId: string,
  fallback: Readonly<{ readFile(path: string): Promise<Uint8Array> }>,
): Readonly<{ readFile(path: string): Promise<Uint8Array> }> {
  return Object.freeze({
    readFile: async (path: string): Promise<Uint8Array> => {
      const key = sharedBrainObjectKey(resourceId, path);
      if (key === undefined) return fallback.readFile(path);
      return createBrainWorkspace(bucket, resourceId).readFile(path);
    },
  });
}

function sharedBrainObjectKey(resourceId: string, path: string): string | undefined {
  if (!path.startsWith("/brain/")) return undefined;
  if (!/^[A-Za-z0-9._:-]{1,256}$/.test(resourceId)) {
    throw new Error("brain workspace has an invalid resource id");
  }
  const parts = path.slice("/brain/".length).split("/");
  if (parts.some((part) => part.length === 0 || part === "." || part === ".." || part.includes("\0"))) {
    throw new Error("brain workspace path must name a canonical file beneath /brain");
  }
  return `brains/${resourceId}/${parts.join("/")}`;
}

function isConnectGrantSlice(value: unknown): value is ConnectGrantSlice {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const grant = value as Partial<ConnectGrantSlice>;
  return Object.keys(value).every((key) => (
    key === "grantId" || key === "connectors" || key === "connectorConnections"
    || key === "mcpIds" || key === "appToolCatalogDigest"
  ))
    && typeof grant.grantId === "string" && /^0x[0-9a-f]{64}$/.test(grant.grantId)
    && isUniqueStringArray(grant.connectors)
    && grant.connectors.every((connector) => (
      connector === "chatgpt" || CONNECTOR_CAPABILITY_IDS.includes(connector as ManagedEgressConnectorId)
    ))
    && (grant.connectorConnections === undefined
      || isConnectorConnectionSelection(grant.connectorConnections, grant.connectors))
    && isUniqueStringArray(grant.mcpIds) && grant.mcpIds.length <= 16
    && grant.mcpIds.every((id) => /^[A-Za-z0-9_-]{43}$/.test(id))
    && (grant.appToolCatalogDigest === undefined
      || isAppToolCatalogDigest(grant.appToolCatalogDigest));
}

function isConnectorConnectionSelection(
  value: unknown,
  connectors: readonly string[],
): value is ConnectorConnectionSelection {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  return Object.entries(value).every(([capability, ids]) => (
    CONNECTOR_CAPABILITY_IDS.includes(capability as ManagedEgressConnectorId)
    && connectors.includes(capability)
    && Array.isArray(ids) && ids.length <= 64
    && ids.every((id) => typeof id === "string" && /^[A-Za-z0-9_-]{43}$/.test(id))
    && new Set(ids).size === ids.length
  ));
}

function isUniqueStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    && new Set(value).size === value.length;
}

const SAFE_OBSERVATION_FIELDS = new Set([
  "account_mcp_refresh_ms",
  "attempt_count",
  "auth_kind",
  "auth_ms",
  "commit_ms",
  "create_ms",
  "credential_prepare_ms",
  "error_code",
  "error_kind",
  "initialization_ms",
  "message_type",
  "method",
  "operation_kind",
  "outcome",
  "resource",
  "state",
  "status",
  "terminal",
  "transport",
]);

function safeObservationDetail(
  detail: Record<string, unknown>,
): Record<string, boolean | number | string> {
  const safe: Record<string, boolean | number | string> = {};
  for (const [key, value] of Object.entries(detail)) {
    if (!SAFE_OBSERVATION_FIELDS.has(key)) continue;
    if (typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
      safe[key] = value;
    }
  }
  return safe;
}

function errorKind(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}

function roundMilliseconds(value: number): number {
  return Math.round(value * 100) / 100;
}

function accountConnectorProjection(
  authorization: TurnAuthorization,
): readonly ManagedEgressConnectorId[] | undefined {
  if (!authorization.connectGrant) return undefined;
  return authorization.connectGrant.connectors.filter(
    (connector): connector is ManagedEgressConnectorId => connector !== "chatgpt",
  );
}

function accountConnectionProjection(
  authorization: TurnAuthorization,
): ConnectorConnectionSelection | undefined {
  return authorization.connectGrant?.connectorConnections;
}

function observeManagedPrincipal(
  env: Env,
  type: string,
  principal: Principal,
  detail: Record<string, unknown> = {},
): void {
  console.info({
    type,
    auth_kind: principal.kind,
    ...(env.DEPLOYMENT_SHA === undefined ? {} : { deployment_sha: env.DEPLOYMENT_SHA }),
    ...safeObservationDetail(detail),
  });
}

async function managedFetch(
  request: Request,
  env: Env,
  ctx: Pick<ExecutionContext, "waitUntil">,
  trustedAgentPrincipal?: Principal,
): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/sandbox-preview/")) {
      const sandboxPreview = await routeSandboxPreviewRequest(request, env, url);
      if (sandboxPreview) return sandboxPreview;
    }
    const browserModel = await routeBrowserModel(request, env, url);
    if (browserModel) return browserModel;
    const realtimeTransport = await routeManagedRealtimeTransport(
      request,
      env,
      url,
      managedOwnershipTimeoutMs(env),
    );
    if (realtimeTransport) return realtimeTransport;
    const hostPrincipal = await routeHostPrincipalRequest(request, env, url);
    if (hostPrincipal) return hostPrincipal;
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
    const handPublisher = url.pathname.match(/^\/v1\/hand-hosts\/([0-9a-f-]{36})\/([0-9a-f-]{36})\/hands\/(host|ice|renew)$/);
    if (handPublisher && isUserId(handPublisher[1])) {
      // Only the per-machine bearer is forwarded. Caller-supplied account and
      // VM assertions cannot widen a server publisher's authority.
      const headers = new Headers();
      for (const name of ["authorization", "upgrade", "content-type"]) {
        const value = request.headers.get(name);
        if (value !== null) headers.set(name, value);
      }
      headers.set(SESSION_OWNER_ASSERTION, handPublisher[1]!);
      return env.NANOCODEX_ACCOUNT_TOOLS.getByName(handPublisher[1]!).fetch(
        `https://account-tools.internal/hand-hosts/${handPublisher[2]}/hands/${handPublisher[3]}${url.search}`,
        new Request(request, { headers }),
      );
    }
    const handManagement = url.pathname.match(/^\/v1\/account\/hand-hosts(?:\/([0-9a-f-]{36}))?$/);
    if (handManagement) {
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant || !principal.capabilities.includes("agents:write")
        || !principal.capabilities.includes("tools:use")) return json({ error: "forbidden" }, { status: 403 });
      if (principal.kind !== "api_key" && request.method !== "GET"
        && request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, { status: 403 });
      const headers = new Headers({ [SESSION_OWNER_ASSERTION]: principal.userId });
      const suffix = handManagement[1] ? `/${handManagement[1]}` : "";
      const response = await env.NANOCODEX_ACCOUNT_TOOLS.getByName(principal.userId).fetch(
        `https://account-tools.internal/hand-hosts${suffix}${url.search}`, new Request(request, { headers }),
      );
      if (response.status !== 201) return response;
      const receipt = await response.json<{ id: string }>();
      return json({ ...receipt, url: `${url.origin}/v1/hand-hosts/${principal.userId}/${receipt.id}/hands` }, {
        status: 201, headers: { "cache-control": "no-store" },
      });
    }
    const leasedVmHost = url.pathname.match(
      /^\/v1\/vm-host-attachments\/([A-Za-z0-9_-]{43})\/([0-9a-f-]{36})\/(tool-host|hands\/(?:host|ice|renew))$/,
    );
    if (leasedVmHost) {
      return routeVmHostToolAttachment(
        request,
        env,
        url,
        leasedVmHost[1]!,
        leasedVmHost[2]!,
        leasedVmHost[3]!,
      );
    }
    if (url.pathname === "/v1/system/vm-host") {
      if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
      if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      if (!await authorizedSystemVmHost(request, env.NANOCODEX_SYSTEM_HOST_TOKEN)) {
        return json({ error: "unauthorized" }, { status: 401 });
      }
      const locator = await vmHostPoolLocator("system", "system");
      return vmHostPoolUpgrade(request, env, {
        scope: "system",
        donor: "system",
        locator,
        publicOrigin: url.origin,
      });
    }
    if (url.pathname === "/v1/account/vm-host") {
      if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
      if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant
        || !principal.capabilities.includes("agents:write")
        || !principal.capabilities.includes("tools:use")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      if (principal.kind !== "api_key" && request.headers.get("origin") !== url.origin) {
        return json({ error: "forbidden_origin" }, { status: 403 });
      }
      const locator = await vmHostPoolLocator("account", principal.userId);
      return vmHostPoolUpgrade(request, env, {
        scope: "account",
        owner: principal.userId,
        donor: principal.userId,
        locator,
        publicOrigin: url.origin,
      });
    }
    if (url.pathname.startsWith("/v1/account/hands/")) {
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant || !principal.capabilities.includes("agents:read")
        || !principal.capabilities.includes("tools:use")
        || (url.pathname.endsWith("/host") && !principal.capabilities.includes("agents:write"))) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      if (principal.kind !== "api_key" && (request.method !== "GET" || request.headers.has("upgrade"))
        && request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, { status: 403 });
      if (url.pathname === "/v1/account/hands/ice") {
        if (request.method !== "POST" || url.search) return json({ error: "invalid_request" }, { status: 400 });
        return remoteICE(env, principal.userId);
      }
      const headers = new Headers(request.headers);
      headers.delete(REMOTE_VM_ASSERTION);
      forwardPrincipalAssertions(headers, principal);
      return env.NANOCODEX_ACCOUNT_TOOLS.getByName(principal.userId).fetch(
        `https://account-tools.internal${url.pathname.slice("/v1/account".length)}${url.search}`,
        new Request(request, { headers }),
      );
    }
    if (url.pathname === "/v1/account/tool-host") {
      if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
      if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant
        || !principal.capabilities.includes("agents:write")
        || !principal.capabilities.includes("tools:use")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      if (principal.kind !== "api_key" && request.headers.get("origin") !== url.origin) {
        return json({ error: "forbidden_origin" }, { status: 403 });
      }
      const headers = new Headers(request.headers);
      forwardPrincipalAssertions(headers, principal);
      return env.NANOCODEX_ACCOUNT_TOOLS.getByName(principal.userId).fetch(
        "https://account-tools.internal/tool-host",
        new Request(request, { headers }),
      );
    }
    if (url.pathname === "/v1/account/hands") {
      if (url.search !== "") return json({ error: "invalid_request" }, { status: 400 });
      if (request.method !== "GET") return json({ error: "method_not_allowed" }, { status: 405 });
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant || !principal.capabilities.includes("agents:read")
        || !principal.capabilities.includes("tools:use")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      const response = await env.NANOCODEX_ACCOUNT_TOOLS.getByName(principal.userId).fetch(
        "https://account-tools.internal/snapshot", {
          method: "POST", body: JSON.stringify({ owner_id: principal.userId }),
        },
      );
      if (response.status === 404) return json({ data: [] }, { headers: { "cache-control": "no-store" } });
      if (!response.ok) return json({ error: "hands_unavailable" }, { status: 503 });
      const snapshot = await response.json<{ machines: Array<{ online: boolean; machine: {
        id: string; name: string; capabilities: readonly string[];
      } }> }>();
      // Expose only the public machine projection, never routing tokens, tool
      // credentials, or the host's physical workspace path.
      return json({ data: snapshot.machines.filter(({ online }) => online).map(({ machine }) => ({
        id: machine.id, name: machine.name, workspace: machineMountRoot(machine.id),
        capabilities: machine.capabilities,
      })) }, { headers: { "cache-control": "no-store" } });
    }
    if (/^\/v1\/(agent-definitions|environment-templates)(?:\/|$)/.test(url.pathname)) {
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (principal.connectGrant || !principal.capabilities.includes(request.method === "GET" ? "agents:read" : "agents:write"))
        return json({ error: "forbidden" }, { status: 403 });
      if (request.method !== "GET") {
        const failure = requireSameOriginMutation(request, url, principal);
        if (failure) return failure;
      }
      return env.NANOCODEX_USERS.getByName(principal.userId).fetch(`https://account.internal${url.pathname.slice(3)}${url.search}`, {
        method: request.method, body: request.body, headers: { "content-type": "application/json" },
      });
    }
    if (request.method === "GET" && url.pathname === "/v1/agents") {
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (!principal.capabilities.includes("agents:read")) return json({ error: "forbidden" }, { status: 403 });
      const agents = await listAgents(env, principal.userId);
      return json({
        data: agents.map(({ id }) => id),
        summaries: Object.fromEntries(agents.filter(({ createdAt }) => createdAt > 0).map(({ id, ...summary }) => [id, {
          title: summary.title,
          created_at: summary.createdAt,
          updated_at: summary.updatedAt,
          turn_count: summary.turnCount,
          ...(principal.connectGrant ? {} : { may_have_scheduled_jobs: summary.mayHaveScheduledJobs }),
        }])),
      });
    }
    const history = await routeHistoryRequest(request, env, url);
    if (history) return history;
    if (request.method === "GET" && url.pathname === "/v1/agents/live") {
      let settings: ManagedAgentSettings;
      try {
        settings = parseAgentSettingsQuery(url.searchParams);
      } catch {
        return json({ error: "invalid_request" }, { status: 400 });
      }
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return new Response("Expected WebSocket upgrade", { status: 426 });
      }
      const creationStartedAt = performance.now();
      const principal = trustedAgentPrincipal ?? await authenticate(request, env, url);
      if (!principal) return json({ error: "unauthorized" }, { status: 401 });
      if (!principal.capabilities.includes("agents:read")
        || !principal.capabilities.includes("agents:write")
        || !principal.capabilities.includes("tools:use")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      if (principal.connectGrant
        && !principal.connectGrant.connectors.includes("chatgpt")) {
        return json({ error: "connector_forbidden" }, { status: 403 });
      }
      if (principal.kind !== "api_key" && request.headers.get("origin") !== url.origin) {
        return json({ error: "forbidden_origin" }, { status: 403 });
      }
      const agentId = uuidV7();
      const headers = new Headers(request.headers);
      forwardPrincipalAssertions(headers, principal);
      headers.set(SESSION_CREATE_ID_ASSERTION, agentId);
      const stub = env.NANOCODEX_SESSIONS.getByName(agentId);
      const internalQuery = agentSettingsQuery(settings);
      internalQuery.set("public_origin", url.origin);
      const response = await stub.fetch(
        `https://session.internal/create-live?${internalQuery}`,
        new Request(request, { headers }),
      );
      observeManagedPrincipal(env, "managed.agent.live_created", principal, {
        agent_id: agentId,
        thread_id: agentId,
        outcome: response.status === 101 ? "success" : "failure",
        create_ms: roundMilliseconds(performance.now() - creationStartedAt),
      });
      return response;
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
        const receipt = aw…82726 tokens truncated…spatchInput(current);
      if (retained !== undefined) {
        if (retained !== inputJson) {
          throw new Error(`managed turn ${id} already has different dispatch input`);
        }
      } else {
        for (let index = 0; index < chunks.length; index += 1) {
          this.ctx.storage.sql.exec(
            `INSERT INTO managed_turn_dispatch_chunks (turn_id, chunk_index, input_json)
             VALUES (?, ?, ?)`,
            id,
            index,
            chunks[index],
          );
        }
      }
      this.ctx.storage.sql.exec(
        `UPDATE managed_turns
         SET dispatch_input_chunks = COALESCE(dispatch_input_chunks, ?),
             may_have_inner_operation = 1, updated_at = ?
         WHERE id = ? AND state IN ('accepted', 'cancelling')`,
        chunks.length,
        Date.now(),
        id,
      );
    });
  }

  #recoverableTurnCount(): number {
    return this.ctx.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_turns WHERE state IN ('accepted', 'cancelling')",
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
    const webhookAlarm = this.#operations.nextAlarm();
    if (webhookAlarm !== undefined) targets.push(webhookAlarm);
    if (!this.#durabilityExported && this.#durabilityImportState !== "pending") {
      const cronAlarm = this.#cronTriggers.nextAlarm();
      if (cronAlarm !== undefined) targets.push(cronAlarm);
    }
    if (this.#archivesNeedMaintenance()) {
      targets.push(Math.max(now + 1, this.#archiveMaintenance.nextAttemptAt()));
    }
    const unfinished = this.#recoverableTurnCount() > 0;
    // Keep a durable wakeup while in-memory work is owned, including when a
    // hibernatable socket is connected. Losing the isolate also loses those
    // handles; the alarm must still reconstruct the accepted work.
    if (unfinished) targets.push(now + MAX_RETRY_DELAY_MS);
    if (!unfinished && (this.#agent || this.#agentPromise)
      && this.#managedRealtimeSession() === undefined) {
      const session = this.#session();
      const lastActive = session?.last_active ?? now;
      targets.push(Math.max(now + 1, lastActive + this.#idleTimeoutMs()));
    }
    if (!this.#streamError) {
      for (const row of this.#managedTurns(
        "WHERE state IN ('accepted', 'cancelling') ORDER BY created_at",
      )) {
        if (row.state === "cancelling") {
          const cancellationInFlight = this.#cancellationTasks.has(row.id);
          const deliveredToLiveTurn = this.#deliveredCancellationTurnIds.has(row.id)
            && this.#turns.has(row.id);
          if (this.#deliveredCancellationTurnIds.has(row.id) && !deliveredToLiveTurn) {
            this.#deliveredCancellationTurnIds.delete(row.id);
          }
          targets.push(managedCancellationAlarmTarget({
            now,
            retryAt: row.retry_at,
            cancellationInFlight,
            deliveredToLiveTurn,
            recoveryLeaseMs: MAX_RETRY_DELAY_MS,
          }));
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
        if (row.retry_at !== null) targets.push(row.retry_at);
        else targets.push(now + 1);
        break;
      }
    }
    const projection = this.ctx.storage.sql.exec<{ retry_at: number }>(
      "SELECT retry_at FROM history_projection_outbox ORDER BY retry_at LIMIT 1",
    ).toArray()[0];
    if (projection) targets.push(Math.max(now + 1, projection.retry_at));
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
    return this.ctx.storage.sql.exec<{ id: string }>(
      "SELECT id FROM managed_turns WHERE state IN ('accepted', 'cancelling') ORDER BY created_at, rowid",
    ).toArray().map(({ id }) => id);
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

  async #refreshCredentialPreparation(
    importOwnership?: DurabilityImportOwnership,
  ): Promise<CredentialBindingOwnership | undefined> {
    const current = this.#credentialBinding;
    if (!current || current.state !== "preparing") return current;
    let retained: CredentialBindingOwnership | undefined;
    await this.ctx.storage.transaction(async (transaction) => {
      if (importOwnership) this.#assertDurabilityImportOwnership(importOwnership);
      const stored = await transaction.get<CredentialBindingOwnership>(CREDENTIAL_BINDING_KEY);
      if (importOwnership) this.#assertDurabilityImportOwnership(importOwnership);
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
    if (importOwnership) this.#assertDurabilityImportOwnership(importOwnership);
    const observed = this.#credentialBinding;
    if (!observed || observed.state === "active") return observed;
    this.#credentialBinding = retained;
    return this.#credentialBinding;
  }

  #broadcast(message: ServerMessage): void {
    this.#broadcastEncoded(JSON.stringify(message));
  }

  #broadcastEncoded(encoded: string): void {
    for (const socket of this.ctx.getWebSockets("client")) {
      const attachment = socket.deserializeAttachment() as Partial<SessionSocketAttachment> | null;
      if (attachment?.replayAfter !== undefined && attachment.replayAfter !== null) continue;
      this.#sendEncoded(socket, encoded);
    }
  }

  #send(socket: WebSocket, message: ServerMessage): boolean {
    return this.#sendEncoded(socket, JSON.stringify(message));
  }

  #sendEncoded(socket: WebSocket, encoded: string): boolean {
    if (socket.readyState !== WebSocket.OPEN) return false;
    try {
      socket.send(encoded);
      return true;
    } catch {
      closeSocket(socket, 1011, "send failed");
      return false;
    }
  }
}

/** Atomically commits a runtime transition and its durable history projection. */
export function commitManagedTransition(
  storage: DurableObjectStorage,
  eventLog: DurableEventLog<StreamMessage>,
  id: string,
  requested: ManagedTurnTransition,
): { committed: ManagedTurnRow; event?: DurableEvent<StreamMessage> } {
  const original = managedTurns(storage, "WHERE id = ?", id)[0];
  if (!original) throw new Error(`managed turn ${id} does not exist`);
  const now = Date.now();
  let event: DurableEvent<StreamMessage> | undefined;
  let committed = original;
  storage.transactionSync(() => {
    const row = managedTurns(storage, "WHERE id = ?", id)[0];
    if (!row) throw new Error(`managed turn ${id} disappeared`);
    if (isTerminalState(row.state)) {
      committed = row;
      return;
    }

    let message: ManagedTurnTransition = requested;
    let state = managedStateForMessage(message);
    if (row.state === "cancelling" && message.type === "turn_retryable") {
      message = {
        type: "turn_cancelling",
        id,
        error: "error" in requested ? requested.error : "cancellation will be retried",
      };
      state = "cancelling";
    }
    let attemptCount = row.attempt_count;
    let retryAt: number | null = null;
    const retrying = message.type === "turn_retryable"
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
    }

    const terminal = isTerminalState(state);
    const detail = "error" in message ? message.error ?? null : null;
    storage.sql.exec("DELETE FROM managed_turn_terminal_chunks WHERE turn_id = ?", id);
    const encoded = terminal
      ? storeTurnInput(storage, id, JSON.stringify(message), "managed_turn_terminal_chunks")
      : null;
    event = eventLog.append(message, id);
    storage.sql.exec(
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
    if (state === "completed") {
      const session = storage.sql.exec<{ runtime_profile: string; session_id: string; first_prompt: string }>(
        "SELECT runtime_profile, session_id, first_prompt FROM session_state WHERE singleton = 1",
      ).toArray()[0];
      if (session?.runtime_profile === "managed" && message.type === "turn_completed") {
        const projection: HistoryProjection = {
          thread_id: session.session_id,
          turn_id: id,
          cursor: event.cursor,
          title: conversationTitle(session.first_prompt),
          input: JSON.parse(row.input_json) as PromptInput,
          final_message: message.final_message,
          created_at: row.created_at,
        };
        storage.sql.exec("DELETE FROM managed_history_projection_chunks WHERE turn_id = ?", id);
        storage.sql.exec(
          `INSERT INTO history_projection_outbox (turn_id, payload_json, attempt_count, retry_at, source_cursor)
           VALUES (?, ?, 0, 0, ?)
           ON CONFLICT(turn_id) DO UPDATE SET payload_json = excluded.payload_json,
             source_cursor = excluded.source_cursor, attempt_count = 0, retry_at = 0`,
          id,
          storeTurnInput(storage, id, JSON.stringify(projection), "managed_history_projection_chunks"),
          event.cursor,
        );
      }
    }
    storage.sql.exec(
      `UPDATE session_state
       SET completed_turns = completed_turns + ?,
           last_active = ?
       WHERE singleton = 1`,
      state === "completed" ? 1 : 0,
      now,
    );
    if (terminal) {
      storage.sql.exec("DELETE FROM turn_history_citations WHERE turn_id = ?", id);
    }
    committed = managedTurns(storage, "WHERE id = ?", id)[0] ?? row;
  });
  return { committed, event };
}

function managedTurns(storage: DurableObjectStorage, clause: string, ...args: (string | number | null)[]): ManagedTurnRow[] {
  return storage.sql
    .exec<ManagedTurnRow>(
      `SELECT id, request_key, request_hash, input_json, authorization_json, state,
            dispatch_input_chunks,
            CAST(accepted_cursor AS TEXT) AS accepted_cursor,
            terminal_json, CAST(terminal_cursor AS TEXT) AS terminal_cursor,
            error, may_have_inner_operation, attempt_count, CAST(retry_at AS INTEGER) AS retry_at,
            created_at, accepted_at, updated_at
     FROM managed_turns ${clause}`,
    ...args,
  ).toArray().map((row) => lazyTurnInput(storage, row));
}

class ManagedRequestError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly state?: ManagedTurnRow["state"],
  ) {
    super(message);
  }
}

function managedTurnRowFromReceipt(receipt: ManagedTurnReceipt): ManagedTurnRow {
  return {
    ...receipt,
    dispatch_input_chunks: null,
    authorization_json: JSON.stringify({ capabilities: [] } satisfies TurnAuthorization),
  };
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

function sameAgentSettings(
  left: ManagedAgentSettings,
  right: ManagedAgentSettings,
): boolean {
  return left.model === right.model
    && left.thinking === right.thinking
    && left.reasoning_mode === right.reasoning_mode
    && left.fast_mode === right.fast_mode;
}

function dispatchInputChunks(input: string): string[] {
  return [...inputChunks(input)];
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

function assertRealtimeContext(context: AgentSessionContext): void {
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
}

function messageForManagedTurn(row: ManagedTurnRow): ServerMessage {
  if (row.terminal_json !== null) {
    return {
      ...(JSON.parse(row.terminal_json) as TurnTerminal),
      ...(row.terminal_cursor === null ? {} : { cursor: row.terminal_cursor }),
    };
  }
  const input = JSON.parse(row.input_json) as PromptInput;
  if (row.state === "accepted" && row.retry_at !== null) {
    return {
      type: "turn_retryable",
      id: row.id,
      error: row.error ?? "turn will be retried",
      ...(row.accepted_cursor === null ? {} : { cursor: row.accepted_cursor }),
    };
  }
  if (row.state === "cancelling") {
    return {
      type: "turn_cancelling",
      id: row.id,
      ...(row.error === null ? {} : { error: row.error }),
      ...(row.retry_at === null ? {} : { retry_at: row.retry_at }),
      ...(row.accepted_cursor === null ? {} : { cursor: row.accepted_cursor }),
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

function managedStateForMessage(message: ManagedTurnTransition): ManagedTurnState {
  switch (message.type) {
    case "turn_cancelling": return "cancelling";
    case "turn_completed": return "completed";
    case "turn_cancelled": return "cancelled";
    case "turn_retryable": return "accepted";
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

async function resolveManagedDurabilityImport(
  env: Env,
  principal: Principal,
  value: unknown,
  timeoutMs: number,
): Promise<ManagedDurabilityImport> {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || (value as { format?: unknown }).format !== "nanocodex-managed-durability-state-v2") {
    return { durability: value };
  }
  const archive = validateManagedDurabilityArchive(value);
  const headers = new Headers();
  forwardPrincipalAssertions(headers, principal);
  const source = env.NANOCODEX_SESSIONS.getByName(archive.source_agent_id);
  const response = await fetchWithDeadline(
    source,
    "https://session.internal/durability/adoption",
    { method: "POST", headers },
    timeoutMs,
    "managed durability adoption authorization",
  );
  if (!response.ok) {
    await response.body?.cancel();
    if (response.status === 404 || response.status === 409) {
      throw new ManagedRequestError(
        400,
        "invalid_durability_import",
        "managed durability source is unavailable for adoption",
      );
    }
    throw new Error(`managed durability source returned ${response.status}`);
  }
  const adopted = await response.json<{
    archive?: unknown;
    source_storage_id?: unknown;
  }>();
  const authoritative = validateManagedDurabilityArchive(adopted.archive);
  if (JSON.stringify(authoritative) !== JSON.stringify(archive)
    || typeof adopted.source_storage_id !== "string"
    || !/^[0-9a-f]{64}$/.test(adopted.source_storage_id)) {
    throw new ManagedRequestError(
      400,
      "invalid_durability_import",
      "managed durability archive does not match its authoritative source",
    );
  }
  return {
    durability: authoritative.durability,
    turn_archive_adoption: {
      durability_records: authoritative.managed_durability_records,
      events: authoritative.managed_events,
      realtime: authoritative.managed_realtime,
      session: authoritative.managed_session,
      source_storage_id: adopted.source_storage_id,
      turn_receipts: authoritative.managed_turn_receipts,
    },
  };
}

function validateManagedDurabilityArchive(value: unknown): ManagedDurabilityArchive {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ManagedRequestError(400, "invalid_durability_import", "managed durability archive is invalid");
  }
  const archive = value as Record<string, unknown>;
  const durability = archive.durability as Record<string, unknown> | undefined;
  const identity = archive.managed_turn_receipts as Record<string, unknown> | undefined;
  const events = archive.managed_events;
  const realtime = archive.managed_realtime;
  const session = archive.managed_session;
  if (Object.keys(archive).some((key) => ![
    "durability",
    "format",
    "managed_durability_records",
    "managed_events",
    "managed_realtime",
    "managed_session",
    "managed_turn_receipts",
    "source_agent_id",
  ].includes(key))
    || archive.format !== "nanocodex-managed-durability-state-v2"
    || typeof archive.source_agent_id !== "string" || !SESSION_ID.test(archive.source_agent_id)
    || !durability || Array.isArray(durability)
    || Object.keys(durability).some((key) => !["format", "stateId", "revision", "payload", "records"].includes(key))
    || durability.format !== "nanocodex-durability-state-v2"
    || typeof durability.stateId !== "string" || durability.stateId.length === 0
    || typeof durability.revision !== "string" || !/^[1-9][0-9]*$/.test(durability.revision)
    || typeof durability.payload !== "string"
    || !Array.isArray(durability.records) || durability.records.length !== 0
    || !validManagedPortableArchiveIdentity(archive.managed_durability_records)
    || !identity || Array.isArray(identity)
    || Object.keys(identity).some((key) => ![
      "archived_bytes",
      "archived_receipts",
      "digest",
      "objects",
      "version",
    ].includes(key))
    || identity.version !== 1
    || !Number.isSafeInteger(identity.archived_bytes) || Number(identity.archived_bytes) < 0
    || !Number.isSafeInteger(identity.archived_receipts) || Number(identity.archived_receipts) < 0
    || !Number.isSafeInteger(identity.objects) || Number(identity.objects) < 0
    || Number(identity.archived_receipts) > Number(identity.objects)
    || typeof identity.digest !== "string" || !/^[0-9a-f]{64}$/.test(identity.digest)
    || !validManagedEventPortability(events)
    || !validManagedRealtimePortability(realtime)
    || !validManagedSessionPortability(session)) {
    throw new ManagedRequestError(400, "invalid_durability_import", "managed durability archive is invalid");
  }
  return value as ManagedDurabilityArchive;
}

function validManagedEventPortability(value: unknown): value is ManagedEventPortability {
  if (!isRecord(value) || !exactKeys(value, ["archive", "state", "tail"])
    || !validManagedPortableArchiveIdentity(value.archive)
    || !isRecord(value.state) || !exactKeys(value.state, [
      "archived_bytes", "archived_events", "archived_through", "index_node_count",
      "index_root_key", "recent_json", "segment_count",
    ])
    || !nonnegativeSafeInteger(value.state.archived_bytes)
    || !nonnegativeSafeInteger(value.state.archived_events)
    || !validCursor(value.state.archived_through)
    || !nonnegativeSafeInteger(value.state.index_node_count)
    || (value.state.index_root_key !== null && typeof value.state.index_root_key !== "string")
    || typeof value.state.recent_json !== "string"
    || !nonnegativeSafeInteger(value.state.segment_count)
    || !validManagedEventTail(value.tail)) return false;
  let recent: unknown;
  try { recent = JSON.parse(value.state.recent_json); } catch { return false; }
  const archivedThrough = value.state.archived_through;
  return Array.isArray(recent) && recent.length <= 16
    && (value.state.index_node_count === 0) === (value.state.index_root_key === null)
    && value.state.archived_events >= value.state.segment_count
    && value.archive.objects === value.state.segment_count + value.state.index_node_count
    && value.archive.bytes >= value.state.archived_bytes
    && BigInt(value.tail.high_water_cursor) >= BigInt(value.state.archived_through)
    && value.tail.events.every(
      (event) => BigInt(event.cursor) > BigInt(archivedThrough),
    );
}

function validManagedEventTail(value: unknown): value is DurableEventTail<StreamMessage> {
  if (!isRecord(value) || !exactKeys(value, ["events", "high_water_cursor"])
    || !validCursor(value.high_water_cursor) || !Array.isArray(value.events)
    || value.events.length > 256) return false;
  let previous = "0";
  for (const event of value.events) {
    if (!isRecord(event) || !exactKeys(event, ["created_at", "cursor", "message", "turn_id"])
      || !validCursor(event.cursor) || event.cursor === "0"
      || BigInt(event.cursor) <= BigInt(previous)
      || BigInt(event.cursor) > BigInt(value.high_water_cursor)
      || !nonnegativeSafeInteger(event.created_at)
      || (event.turn_id !== null && typeof event.turn_id !== "string")
      || !isRecord(event.message) || typeof event.message.type !== "string") return false;
    previous = event.cursor;
  }
  return true;
}

function validManagedRealtimePortability(value: unknown): value is ManagedRealtimePortability {
  if (!isRecord(value) || !exactKeys(value, ["archive", "state", "tail"])
    || !validManagedPortableArchiveIdentity(value.archive)
    || !isRecord(value.state) || !exactKeys(value.state, [
      "archived_bytes", "archived_receipts", "object_count",
    ])
    || !nonnegativeSafeInteger(value.state.archived_bytes)
    || !nonnegativeSafeInteger(value.state.archived_receipts)
    || !nonnegativeSafeInteger(value.state.object_count)
    || value.state.archived_receipts !== value.state.object_count
    || !Array.isArray(value.tail) || value.tail.length > 512) return false;
  const identities = new Set<string>();
  return value.archive.objects === value.state.object_count
    && value.archive.bytes === value.state.archived_bytes
    && value.tail.every((operation) => {
    if (!isRecord(operation) || !exactKeys(operation, [
      "blocked", "created_at", "kind", "operation_id", "request_hash", "response_json",
      "state", "updated_at", "voice_session_id",
    ])) return false;
    const complete = operation.state === "completed";
    if ((operation.blocked !== 0 && operation.blocked !== 1)
      || !nonnegativeSafeInteger(operation.created_at)
      || !nonnegativeSafeInteger(operation.updated_at)
      || Number(operation.updated_at) < Number(operation.created_at)
      || !["start", "delegate", "stop"].includes(String(operation.kind))
      || typeof operation.operation_id !== "string" || operation.operation_id.length === 0
      || typeof operation.voice_session_id !== "string" || operation.voice_session_id.length === 0
      || typeof operation.request_hash !== "string" || !/^[0-9a-f]{64}$/.test(operation.request_hash)
      || (complete ? typeof operation.response_json !== "string" : operation.response_json !== null)
      || (!complete && operation.state !== "pending")
      || (complete && operation.blocked !== 0)
      || (!complete && operation.blocked !== 1)) return false;
    const identity = `${operation.voice_session_id}\0${operation.operation_id}`;
    if (identities.has(identity)) return false;
    identities.add(identity);
    if (complete) {
      try { JSON.parse(operation.response_json as string); } catch { return false; }
    }
    return true;
    });
}

function validManagedSessionPortability(value: unknown): value is ManagedSessionPortability {
  return isRecord(value) && exactKeys(value, [
    "accepted_turns", "completed_turns", "first_prompt", "last_active", "settings", "stream_error", "title",
  ])
    && nonnegativeSafeInteger(value.accepted_turns)
    && nonnegativeSafeInteger(value.completed_turns)
    && Number(value.completed_turns) <= Number(value.accepted_turns)
    && typeof value.first_prompt === "string"
    && nonnegativeSafeInteger(value.last_active)
    && (value.stream_error === null || typeof value.stream_error === "string")
    && validAgentSettings(value.settings)
    && typeof value.title === "string"
    && value.title === conversationTitle(value.first_prompt);
}

function validAgentSettings(value: unknown): value is ManagedAgentSettings {
  if (!isRecord(value) || !exactKeys(value, [
    "fast_mode", "model", "reasoning_mode", "thinking",
  ])) return false;
  try {
    parseCompleteAgentSettings(value);
    return true;
  } catch {
    return false;
  }
}

function validManagedPortableArchiveIdentity(value: unknown): value is ManagedPortableArchiveIdentity {
  return isRecord(value) && exactKeys(value, ["bytes", "digest", "objects", "version"])
    && value.version === 1
    && nonnegativeSafeInteger(value.bytes)
    && nonnegativeSafeInteger(value.objects)
    && typeof value.digest === "string" && /^[0-9a-f]{64}$/.test(value.digest);
}

function validCursor(value: unknown): value is string {
  return typeof value === "string" && parseCursor(value) === value;
}

function nonnegativeSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  return Object.keys(value).length === keys.length
    && Object.keys(value).every((key) => keys.includes(key));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function portableDurabilityStateId(value: unknown): string {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("portable durability archive is invalid");
  }
  const archive = value as Record<string, unknown>;
  if (Object.keys(archive).some((key) => !["format", "stateId", "revision", "payload", "records"].includes(key))
    || archive.format !== "nanocodex-durability-state-v2"
    || typeof archive.stateId !== "string" || archive.stateId.length === 0
    || typeof archive.revision !== "string" || !/^[1-9][0-9]*$/.test(archive.revision)
    || typeof archive.payload !== "string") {
    throw new Error("portable durability archive is invalid");
  }
  return archive.stateId;
}

function validDurabilityImportPreparation(value: unknown): boolean {
  if (value === null) return true;
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const prepared = value as Record<string, unknown>;
  return !Object.keys(prepared).some((key) => ![
    "request_hash",
    "source_agent_id",
    "state_id",
  ].includes(key))
    && typeof prepared.request_hash === "string" && /^[0-9a-f]{64}$/.test(prepared.request_hash)
    && (prepared.source_agent_id === null
      || (typeof prepared.source_agent_id === "string" && SESSION_ID.test(prepared.source_agent_id)))
    && typeof prepared.state_id === "string" && prepared.state_id.length > 0;
}

async function requestSessionCleanup(
  stub: DurableObjectStub<DurableAgentSession>,
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
  const code = (error as { code?: unknown } | null)?.code;
  if (code === "invalid_request") return { status: 400, code, message: errorMessage(error) };
  if (code === "conflict") return { status: 409, code, message: errorMessage(error) };
  if (code === "retryable") return { status: 503, code, message: errorMessage(error) };
  return { status: 500, code: fallbackCode, message: errorMessage(error) };
}

function managedErrorResponse(error: unknown, fallbackCode?: string): Response {
  const failure = managedHttpError(error, fallbackCode);
  return json({ error: failure.code, message: failure.message }, { status: failure.status });
}

async function parseHistoryRequestBody(request: Request): Promise<unknown> {
  let value: unknown;
  try {
    value = await request.json();
  } catch (error) {
    if (error instanceof ManagedRequestError) throw error;
    throw new HistorySearchError(400, "invalid_json", "request body must be JSON");
  }
  return value;
}

async function routeHistoryRequest(
  request: Request,
  env: Env,
  url: URL,
): Promise<Response | undefined> {
  const find = url.pathname === "/v1/history/sessions/search";
  const read = url.pathname.match(/^\/v1\/history\/sessions\/([^/]+)\/read$/);
  const memory = url.pathname === "/v1/memory";
  const memoryDelete = url.pathname.match(/^\/v1\/memory\/([^/]+)$/);
  if (!find && !read && !memory && !memoryDelete) return undefined;
  const validMethod = (find || read) ? request.method === "POST"
    : memory ? request.method === "GET" || request.method === "POST"
      : request.method === "DELETE";
  if (!validMethod) {
    return json({ error: "method_not_allowed" }, { status: 405 });
  }
  const principal = await authenticate(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, { status: 401 });
  if (memory && request.method === "GET" && url.search) {
    return json({ error: "invalid_request" }, { status: 400 });
  }
  if ((find || read) && !principal.capabilities.includes("history:read")) {
    return json({ error: "forbidden" }, { status: 403 });
  }
  if (memory && request.method === "GET" && !principal.capabilities.includes("memory:read")) {
    return json({ error: "forbidden" }, { status: 403 });
  }
  if (memoryDelete && !principal.capabilities.includes("memory:write")) {
    return json({ error: "memory_read_only" }, { status: 403 });
  }
  const originFailure = request.method === "GET"
    ? undefined
    : requireSameOriginMutation(request, url, principal);
  if (originFailure) return originFailure;

  try {
    let internalPath: "/search" | "/read" | "/memories" | "/memory";
    let input: HistoryFindSessionsInput | HistoryReadSessionInput | MemoryOperation | undefined;
    let mutatingMemory = false;
    if (find) {
      input = parseHistoryFindSessionsInput(await parseHistoryRequestBody(request));
      internalPath = "/search";
    } else if (read) {
      const value = await parseHistoryRequestBody(request);
      if (!value || typeof value !== "object" || Array.isArray(value)
        || Object.keys(value).some((key) => key !== "turn_ids")) {
        throw new HistorySearchError(400, "invalid_request", "supported field is turn_ids");
      }
      input = parseHistoryReadSessionInput({
        ...value,
        session_id: read[1],
      });
      internalPath = "/read";
    } else if (memory && request.method === "GET") {
      internalPath = "/memories";
    } else {
      const operation = memoryDelete
        ? { operation: "delete" as const, key: parseMemoryDeleteKey(url, memoryDelete[1]!) }
        : parseMemoryOperation(await parseHistoryRequestBody(request));
      input = operation;
      mutatingMemory = operation.operation === "put" || operation.operation === "delete";
      if (!mutatingMemory && !principal.capabilities.includes("memory:read")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      if (mutatingMemory && !principal.capabilities.includes("memory:write")) {
        return json({ error: "memory_read_only" }, { status: 403 });
      }
      internalPath = "/memory";
    }

    const memoryScope = env.NANOCODEX_MEMORY.getByName(principal.organizationId);
    const initialized = await initializeMemoryScope(memoryScope, principal.organizationId);
    if (!initialized.ok) return initialized;
    const response = await memoryScope.fetch(`https://memory.internal${internalPath}`, {
      method: internalPath === "/memories" ? "GET" : "POST",
      headers: {
        ...(input === undefined ? {} : { "content-type": "application/json" }),
        [MEMORY_ORGANIZATION_ASSERTION]: principal.organizationId,
        [MEMORY_TEAM_ASSERTION]: principal.teamId,
        [MEMORY_SUBJECT_ASSERTION]: `${principal.subjectId}:${principal.authorizationEpoch}`,
        ...(mutatingMemory ? { [MEMORY_MUTATION_ASSERTION]: "1" } : {}),
      },
      ...(input === undefined ? {} : { body: JSON.stringify(input) }),
    });
    if (!response.ok || memory) return response;
    if (memoryDelete) {
      await response.body?.cancel();
      return new Response(null, { status: 204 });
    }
    if (find) {
      const found = await response.json<HistoryFindSessionsResponse>();
      return json({
        query: found.query,
        results: found.results.map((result) => ({
          session_id: result.thread_id,
          title: result.title,
          turn_id: result.turn_id,
          cursor: result.cursor,
          score: result.score,
          snippet: result.snippet,
        })),
        citations: found.citations,
      });
    }
    const result = await response.json<HistoryReadSessionResponse>();
    return json({
      turns: result.turns.map((turn) => ({
        session_id: turn.thread_id,
        title: turn.title,
        turn_id: turn.turn_id,
        cursor: turn.cursor,
        user: turn.user,
        assistant: turn.assistant,
      })),
      citations: result.citations,
    });
  } catch (error) {
    return historySearchErrorResponse(error);
  }
}

function parseMemoryDeleteKey(url: URL, encodedId: string) {
  const version = url.searchParams.get("version");
  if (!/^[1-9][0-9]*$/.test(encodedId)
    || version === null
    || !/^[1-9][0-9]*$/.test(version)
    || [...url.searchParams.keys()].length !== 1) {
    throw new DurableMemoryError("invalid_key", "memory delete requires one positive id and version");
  }
  return parseMemoryKey({ id: Number(encodedId), version: Number(version) });
}

function historySearchErrorResponse(error: unknown): Response {
  if (error instanceof HistorySearchError) {
    return json({ error: error.code, message: error.message }, { status: error.status });
  }
  if (error instanceof DurableMemoryError) {
    return json({ error: error.code, message: error.message }, { status: 400 });
  }
  if (error instanceof ManagedRequestError) return managedErrorResponse(error);
  return json({ error: "history_search_failed", message: errorMessage(error) }, { status: 500 });
}

async function historySearchResponseError(response: Response): Promise<HistorySearchError> {
  const value = await response.json<{ error?: unknown; message?: unknown }>().catch(() => undefined);
  const code = typeof value?.error === "string" ? value.error : "history_search_failed";
  const message = typeof value?.message === "string" ? value.message : `history search failed with HTTP ${response.status}`;
  return new HistorySearchError(response.status, code, message);
}

function initializeMemoryScope(
  memory: DurableObjectStub<MemoryScope>,
  organizationId: string,
): Promise<Response> {
  return memory.fetch("https://memory.internal/initialize", {
    method: "PUT",
    headers: { [MEMORY_ORGANIZATION_ASSERTION]: organizationId },
  });
}

async function hashManagedInput(input: PromptInput): Promise<string> {
  return createHash("sha256").update(canonicalJson(input)).digest("hex");
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
      model?: unknown;
      session_id?: unknown;
    }>();
    if (!value.commands || typeof value.commands !== "object" || Array.isArray(value.commands)
      || typeof value.session_id !== "string" || !value.session_id
      || (value.model !== undefined && !isAgentModel(value.model))) {
      return json({ error: "invalid managed web request" }, { status: 400 });
    }
    return fetchManagedTool(env, subject, "/v1/search", {
      id: value.session_id,
      model: value.model ?? DEFAULT_AGENT_SETTINGS.model,
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
    body = await request.json();
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

function sameAccountMcpConnections(
  left: readonly ManagedAccountMcpConnection[] | undefined,
  right: readonly ManagedAccountMcpConnection[],
): boolean {
  return left !== undefined
    && left.length === right.length
    && left.every((connection, index) => (
      connection.id === right[index]?.id && connection.name === right[index]?.name
    ));
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
