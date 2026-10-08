import { validateTotpEnrollment, validateTotpMetadata, type TotpMaterial, type TotpMetadata } from "./vault-totp";
import { providerVaultRoute } from "./provider-result-vault";
import { ClaudeSubscription } from "nanocodex/worker";
import claudeModule from "nanocodex/wasm";
import { createMercatorMcpCredential, MercatorPaymentInputError } from "./mercator-payment";
import type { CloudflareAccountVaultResult } from "nanocodex/cloudflare/egress";
import { createSshKeyPair, sshPublicKey } from "nanocodex/tools/ssh";
import { DurableObject } from "cloudflare:workers";
import type { UserCredentialSnapshot } from "./credential-snapshot";
import { Provider, ProviderRequest, secp256k1, Storage } from "accounts";
import { createClient, http } from "viem";
import { Account as TempoAccount, Actions } from "viem/tempo";
import { tempo } from "viem/tempo/chains";
import { linkedWalletProvider, newWalletLink, pairWallet, publicLinkedWallet, publicWalletLink,
  verifiedAuthorization, WALLET_OPERATION, type LinkedWallet, type WalletLink } from "./linked-wallet";

import {
  CredentialVault,
  type CredentialVaultEnv,
  type EncryptedEnvelope,
} from "./credential-vault";
import {
  BROWSER_COOKIE_JAR_ID,
  browserCookieJarMetadata,
  type BrowserCookieJarMetadata,
  type BrowserCookieJarV1,
  MAX_BROWSER_COOKIE_JAR_BODY_BYTES,
  MAX_BROWSER_COOKIE_JARS,
  publicBrowserCookieJar,
  publicBrowserCookieJarNames,
  publicBrowserCookieJarMetadata,
  sameBrowserCookieJarBinding,
  sameBrowserCookieJarMetadata,
  validateBrowserCookieJarBinding,
  validateBrowserCookieJarDelete,
  validateBrowserCookieJarUpsert,
  validateStoredBrowserCookieJar,
  validateStoredBrowserCookieJarMetadata,
} from "./browser-cookie-jar";
import {
  type BrokeredSshIdentity,
  validateSshIdentity,
  validateSshTarget,
  validSshIdentityReference,
} from "./ssh";

const STATE_KEY = "credential-state";
/** Sealed regional lease registry; separate from credential state. */
const LEASE_KEY = "model-credential-leases-v1";
const MODEL_LEASE_MS = 10 * 60_000;
const REVOCATION_RETRY_MS = 5_000;
const INVALIDATE_TIMEOUT_MS = 3_000;
const LEASE_REGIONS: ReadonlySet<string> = new Set(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc"]);
const CLAUDE_STATE_KEY = "claude-subscription-v1";
type ClaudeRow = { revision: string; envelope: EncryptedEnvelope };
type ClaudeCredential = ClaudeSubscription.PrivateCredential;
const TOKEN_ENDPOINT_PATH = "/oauth/token";
const CODEX_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann";
const LOGIN_TTL_MS = 15 * 60_000;
const REFRESH_EARLY_MS = 5 * 60_000;
const DEFAULT_REFRESH_BACKOFF_MS = 60_000;
const MAX_REFRESH_BACKOFF_MS = 15 * 60_000;
const MAX_REFRESH_BACKOFF_ATTEMPT = 5;
const MAX_PROVIDER_RESPONSE_BYTES = 16 * 1024;
const MAX_IMPORTED_TOKEN_BYTES = 32 * 1024;
const MAX_IMPORTED_ACCOUNT_ID_BYTES = 256;
const MAX_VAULT_ENTRIES = 100;
const MAX_CHATGPT_ACCOUNTS = 20;
const MAX_VAULT_BODY_BYTES = 12 * 1024;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
const VAULT_ENTRY_KEY_PREFIX = "vault-entry:";
const BROWSER_COOKIE_JAR_KEY_PREFIX = "browser-cookie-jar:";
const SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const USER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const SUBJECT_DIRECTORY_PREFIX = "agent-subject-v1:";
const SUBJECT_TOMBSTONE_PREFIX = "!deleted:";
const ROOT_WALLET_PRIVATE_KEY = /^0x[0-9a-fA-F]{64}$/;
const ROOT_WALLET_ADDRESS = /^0x[0-9a-fA-F]{40}$/;
const TEMPO_CHAIN_ID = "0x1079";
const TEMPO_RPC = "https://rpc.tempo.xyz";
const MACHINE_USD = "0x20c000000000000000000000f37de3740adec032";
const PRODUCTION_CONNECT_API_ORIGIN = "https://nanocodex-connect-api.gakonst.workers.dev";
const MAX_WALLET_RESOURCES = 64;
const MAX_WALLET_RESOURCE_BYTES = 512;
const MAX_WALLET_RESOURCE_TOTAL_BYTES = 8 * 1024;
const SPONSORED_PROMPT_STATE_KEY = "sponsored-prompt-state-v1";
const SPONSORED_CONNECTION_STATE_KEY = "sponsored-connection-state-v1";
const SPONSORED_PROMPT_LIMIT = 3;
const SPONSORED_CONNECTION_LIMIT = 3;
const SPONSORED_PROMPT_MAX_ATTEMPTS = 2;
const SPONSORED_PROMPT_LEASE_MS = 15_000;
const TEST_SPONSORED_PROMPT_LEASE_MS = 250;
const SPONSORED_PROMPT_ID = /^[A-Za-z][A-Za-z0-9_-]{1,127}$/;
const SPONSORED_PROMPT_HASH = /^[A-Za-z0-9_-]{43}$/;
const SPONSORED_CONNECTION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SPONSORED_RESPONSE_ID = /^[A-Za-z][A-Za-z0-9._:-]{1,199}$/;
const SPONSORED_CALL_ID = /^[A-Za-z][A-Za-z0-9._:-]{1,199}$/;
const MAX_SPONSORED_CALL_IDS = 64;

export interface BrokerEnv extends CredentialVaultEnv {
  AGENT_SUBJECTS: DurableObjectNamespace<AgentSubjectDirectory>;
  /** Used only to authenticate the owner named by a regional lease request. */
  USER_CREDENTIALS?: DurableObjectNamespace<UserCredentialBroker>;
  /** Optional during rollout; absent means no regional leases are granted. */
  USER_CREDENTIAL_SNAPSHOTS?: DurableObjectNamespace<UserCredentialSnapshot>;
  CHIEF_OF_STAFF_OPENAI_API_KEY?: string;
  NANOCODEX_SPONSORED_CHATGPT_USER_ID?: string;
  CHATGPT_ISSUER?: string;
  ALLOW_LOCAL_CREDENTIAL_CLAIM?: string;
  NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET?: string;
  LOCAL_CHATGPT_BOOTSTRAP?: string;
}

export type ModelCredentialValue = Readonly<{
  kind: "openai" | "chatgpt";
  secret: string;
  accountId?: string;
  fedramp?: boolean;
  expiresAt?: number;
  revision: number;
  provenance?: "user" | "sponsor";
}>;

export type ModelCredentialLeaseGrant = Readonly<{
  status: number;
  credential: ModelCredentialValue | null;
  /** Holder must discard the grant when epoch is below its invalidation floor. */
  epoch: number;
  /** Duration, not a timestamp: the holder anchors it to its own clock. */
  lease_ms: number;
}>;

export type ClaudeCredentialValue = ClaudeSubscription.PrivateCredential;
export type ClaudeCredentialLeaseGrant = Readonly<{
  status: number;
  credential: ClaudeCredentialValue | null;
  epoch: number;
  lease_ms: number;
}>;
/** Regional Claude holders re-resolve before the provider token can age out. */
const CLAUDE_LEASE_MS = 5 * 60_000;

type ModelLease = { fingerprint: string; epoch: number; expiresAt: number };
type PendingRevocation = { epoch: number; expiresAt: number };
type LeaseRegistry = {
  version: 1;
  epoch: number;
  owner?: string;
  leases: Record<string, ModelLease>;
  pending: Record<string, PendingRevocation>;
  /** Durable Claude projection: generation bumps on every Claude mutation;
   * revision tracks the last plain read, so holders are fenced on change. */
  claude?: { generation: number; revision: string | null };
};

type ApiKeyCredential = { secret: string; createdAt: number; revision: number };
type ChatGptCredential = {
  accessToken: string;
  refreshToken: string;
  accountId: string;
  fedramp: boolean;
  expiresAt: number;
  revision: number;
  provenance?: "user" | "sponsor";
  refreshState: "ready" | "in_flight";
  refreshAfter?: number;
  refreshAttempts?: number;
  deadReason: string | null;
  limitedUntil?: number;
  authorizationRevision?: number;
};
type PendingLogin = {
  deviceAuthId: string;
  userCode: string;
  verificationUrl: string;
  expiresAt: number;
  pollAfterMs: number;
  nextPollAt: number;
};
type RootWallet = {
  privateKey: `0x${string}`;
  address: `0x${string}`;
  createdAt: number;
};
export type VaultKind = "login" | "api_key" | "card" | "address" | "phone" | "totp";
export type VaultEntryPayload =
  | (TotpMaterial & Readonly<{ kind: "totp"; name: string }>)
  | Readonly<{ kind: "api_key"; name: string; api_key: string }>
  | Readonly<{ kind: "login"; name: string; username: string; password: string; browser_origin?: string }>
  | Readonly<{
      kind: "card";
      name: string;
      card_number: string;
      expiry_month: string;
      expiry_year: string;
      cvv?: string;
      billing_zip: string;
    }>
  | Readonly<{
      kind: "address";
      name: string;
      address_line_1: string;
      address_line_2?: string;
      city: string;
      state: string;
      zip: string;
      country: string;
    }>
  | Readonly<{ kind: "phone"; name: string; phone_number: string }>;
export type VaultEntry = VaultEntryPayload & Readonly<{ id: string; createdAt: number }>;
type VaultEntryMetadata = (
  | (TotpMetadata & Readonly<{ kind: "totp"; name: string }>)
  | Readonly<{ kind: "api_key"; name: string }>
  | Readonly<{ kind: "login"; name: string; username: string; browser_origin?: string }>
  | Readonly<{ kind: "card"; name: string; last4: string }>
  | Readonly<{
      kind: "address";
      name: string;
      address_line_1: string;
      address_line_2?: string;
      city: string;
      state: string;
      zip: string;
      country: string;
    }>
  | Readonly<{ kind: "phone"; name: string; phone_number: string }>
) & Readonly<{ id: string; createdAt: number }>;
type CredentialState = {
  version: 1;
  active: "openai" | "chatgpt" | null;
  openai?: ApiKeyCredential;
  chatgpt?: ChatGptCredential;
  chatgptBackups?: ChatGptCredential[];
  chatgptRevision?: number;
  login?: PendingLogin;
  ssh?: Record<string, BrokeredSshIdentity>;
  vault?: Record<string, VaultEntryMetadata>;
  wallet?: RootWallet;
  linkedWallet?: LinkedWallet;
  walletLinks?: Record<string, WalletLink>;
  walletUnlinks?: Record<string, { address: string }>;

  browserCookieJars?: Record<string, BrowserCookieJarMetadata>;
};
type StoredRow = { envelope: EncryptedEnvelope };
type SponsoredPrompt = Readonly<{
  id: string;
  hash: string;
  phase: "in_flight" | "retryable" | "continuation" | "terminal";
  attempts: number;
  leaseExpiresAt?: number;
  continuation?: Readonly<{
    responseId: string;
    callIds: readonly string[];
  }>;
}>;
type SponsoredPromptState = Readonly<{
  prompts: readonly SponsoredPrompt[];
}>;
type SponsoredConnection = Readonly<{
  id: string;
  leaseExpiresAt: number;
}>;
type SponsoredConnectionState = Readonly<{
  connections: readonly SponsoredConnection[];
}>;

export type ChatGptCredentialImport = Readonly<{
  access_token: string;
  refresh_token: string;
  account_id: string;
  expires_at: number;
  fedramp: boolean;
}>;

export class AgentSubjectDirectory extends DurableObject<BrokerEnv> {
  readonly #state: DurableObjectState;
  readonly #subject: string | undefined;

  constructor(state: DurableObjectState, env: BrokerEnv) {
    super(state, env);
    this.#state = state;
    this.#subject = state.id.name?.startsWith(SUBJECT_DIRECTORY_PREFIX)
      ? state.id.name.slice(SUBJECT_DIRECTORY_PREFIX.length)
      : undefined;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/v1/health") {
      return this.#dispatch(request);
    }
    const body = await readJson(request, 2_048);
    if (!body) return jsonError(400, "invalid_json");
    return this.#dispatch(request, body);
  }

  async #dispatch(
    request: Request,
    parsedBody?: Record<string, unknown>,
  ): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/v1/health") {
      return json({ ready: true }, 200);
    }
    const body = parsedBody ?? await readJson(request, 2_048);
    if (!body) return jsonError(400, "invalid_json");
    const subject = stringField(body, "subject");
    if ((url.pathname === "/v1/bind" || url.pathname === "/v1/unbind"
        || url.pathname === "/v1/resolve")
      && (!SUBJECT.test(subject ?? "") || subject !== this.#subject)) {
      return jsonError(400, "invalid_subject");
    }
    if (request.method === "POST" && url.pathname === "/v1/bind") {
      const userId = stringField(body, "user_id");
      if (!USER_ID.test(userId ?? "")) return jsonError(400, "invalid_user_id");
      const result = await this.#state.storage.transaction(async (transaction) => {
        const key = `subject:${subject}`;
        const current = await transaction.get<string>(key);
        if (subjectTombstoneOwner(current) !== undefined) return "deleted" as const;
        if (current && current !== userId) return "conflict" as const;
        if (!current) await transaction.put(key, userId!);
        return current ? "unchanged" as const : "bound" as const;
      });
      if (result === "deleted") return jsonError(410, "subject_deleted");
      if (result === "conflict") return jsonError(409, "subject_already_bound");
      return json({ status: result }, 200);
    }
    if (request.method === "POST" && url.pathname === "/v1/unbind") {
      const userId = stringField(body, "user_id");
      if (!USER_ID.test(userId ?? "")) return jsonError(400, "invalid_user_id");
      const removed = await this.#tombstoneSubject(subject!, userId!);
      if (removed === "mismatch") return jsonError(409, "subject_owner_mismatch");
      return new Response(null, { status: 204, headers: noStoreHeaders() });
    }
    if (request.method === "POST" && url.pathname === "/v1/resolve") {
      const retained = await this.#state.storage.get<string>(`subject:${subject}`);
      const deletedUserId = subjectTombstoneOwner(retained);
      return retained && deletedUserId === undefined
        ? json({ user_id: retained }, 200)
        : jsonError(404, "subject_not_bound");
    }
    return jsonError(404, "not_found");
  }

  async #tombstoneSubject(subject: string, userId: string): Promise<true | "mismatch"> {
    return this.#state.storage.transaction(async (transaction) => {
      const key = `subject:${subject}`;
      const current = await transaction.get<string>(key);
      const retainedOwner = subjectTombstoneOwner(current);
      if ((retainedOwner ?? current) && (retainedOwner ?? current) !== userId) {
        return "mismatch" as const;
      }
      await transaction.put(key, subjectTombstone(userId));
      return true;
    });
  }

}

function subjectTombstone(userId: string): string {
  return `${SUBJECT_TOMBSTONE_PREFIX}${userId}`;
}

function subjectTombstoneOwner(value: string | undefined): string | undefined {
  return value?.startsWith(SUBJECT_TOMBSTONE_PREFIX)
    ? value.slice(SUBJECT_TOMBSTONE_PREFIX.length)
    : undefined;
}

type CredentialOperation = "credential_rpc" | "credential_http" | "metadata_rpc" | "http" | "alarm" | "lease_rpc";
type CredentialOperationObservation = Readonly<{
  operation: CredentialOperation;
  resolveId?: string;
  /** A credential mutation must not be acknowledged while ANY holder
   * revocation is pending, even when this invocation changed nothing
   * (idempotent retry after a persisted removal). */
  mutation?: boolean;
}>;
type CredentialActivationPhase = "storage_load_ms" | "vault_open_ms" | "restore_ms"
  | "migration_ms" | "reseal_ms" | "alarm_ms";

// Workers' Date.now() only advances after I/O: these are coarse elapsed
// times, not CPU profiles. Zero does not imply that a phase did no work.
function credentialMetric(detail: Readonly<Record<string, unknown>>): void {
  try { console.info(detail); } catch { /* Observation must not affect credential state. */ }
}

/** TEMPORARY: #890 placement row, removed with #recoverFromHome. */
const MOVED_PLACEMENT_KEY = "broker-placement-v1";

export class UserCredentialBroker extends DurableObject<BrokerEnv> {
  readonly #state: DurableObjectState;
  readonly #env: BrokerEnv;
  readonly #vault: CredentialVault;
  readonly #ready: Promise<void>;
  #activatedAt = 0;
  #activationMs = 0;
  #activationPhase: CredentialActivationPhase = "storage_load_ms";
  readonly #activationPhases: Partial<Record<CredentialActivationPhase, number>> = {};
  #pendingOperations = 0;
  #activeOperation: CredentialOperation | undefined;
  #credentials: CredentialState = { version: 1, active: null };
  #committedWalletIdentity: ReturnType<typeof publicRootWallet> | null = null;
  #claude: Promise<ClaudeSubscription.Subscription> | undefined;
  #claudeCredential: ClaudeCredential | undefined;
  #tail: Promise<void> = Promise.resolve();
  #liveWalletLink: { operationId: string; controller: AbortController } | undefined;
  readonly #leaseVault: CredentialVault;
  #leaseRegistry: LeaseRegistry = { version: 1, epoch: 0, leases: {}, pending: {} };
  /** Plain-read projection that every unexpired, non-pending lease matches. */
  #reconciledProjection: string | undefined;

  constructor(state: DurableObjectState, env: BrokerEnv) {
    super(state, env);
    this.#state = state;
    this.#env = env;
    this.#vault = new CredentialVault(env, `user/${state.id.toString()}`);
    this.#leaseVault = new CredentialVault(env, `user/${state.id.toString()}/model-credential-leases`);
    const startedAt = Date.now();
    // TEMPORARY: an #890 home object only serves exportMovedRows; its rows are
    // sealed under the legacy object's scope and must not load here.
    if (state.id.name?.startsWith("~home/")) { this.#ready = Promise.reject(new Error("retired credential home")); this.#ready.catch(() => {}); return; }
    this.#ready = state.blockConcurrencyWhile(async () => {
      let completed = false;
      try {
        await this.#recoverFromHome();
        await this.#initialize();
        this.#committedWalletIdentity = this.#credentials.wallet ? publicRootWallet(this.#credentials.wallet) : null;
        this.#activatedAt = Date.now();
        this.#activationMs = this.#activatedAt - startedAt;
        completed = true;
      } finally {
        credentialMetric({
          type: "egress.credential.activation",
          outcome: completed ? "ok" : "error",
          activation_ms: Date.now() - startedAt,
          last_phase: this.#activationPhase,
          activation_phases: { ...this.#activationPhases },
        });
      }
    });
  }

  // TEMPORARY one-shot recovery of #890's region homes. Delete after it has run.
  // A legacy object whose rows #890 moved holds only {state:"moved",target}
  // under this key; pull the sealed rows back verbatim (they were sealed under
  // this object's scope) and drop the tombstone. The home is never used again.
  async #recoverFromHome(): Promise<void> {
    const placement = await this.#state.storage.get<{ state?: string; target?: string }>(MOVED_PLACEMENT_KEY);
    if (!placement) return;
    if (placement.state === "moved" && typeof placement.target === "string" && this.#env.USER_CREDENTIALS) {
      const rows = await this.#env.USER_CREDENTIALS.getByName(placement.target).exportMovedRows() as [string, unknown][];
      await this.#state.storage.transaction(async (transaction) => {
        for (let index = 0; index < rows.length; index += 128) await transaction.put(Object.fromEntries(rows.slice(index, index + 128)));
        await transaction.delete(MOVED_PLACEMENT_KEY);
      });
      credentialMetric({ type: "egress.credential.home_recovered", rows: rows.length });
    } else await this.#state.storage.delete(MOVED_PLACEMENT_KEY);
  }

  /** TEMPORARY: raw sealed rows of an #890 home object, for its legacy object. */
  async exportMovedRows(): Promise<[string, unknown][]> {
    const rows: [string, unknown][] = [];
    for (const [key, value] of await this.#state.storage.list()) if (key !== MOVED_PLACEMENT_KEY) rows.push([key, value]);
    return rows;
  }

  async readWalletIdentity(): Promise<ReturnType<typeof publicRootWallet> | null> {
    await this.#ready;
    return this.#committedWalletIdentity ? { ...this.#committedWalletIdentity } : null;
  }

  fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/v1/wallet"
      && request.headers.get("accept") === "application/vnd.nanocodex.wallet-snapshot+json") return this.#walletSnapshot(request);
    if (url.pathname === "/v1/wallet/link") return this.#startWalletLink(request);
    if (url.pathname === "/v1/wallet/connect") return this.#walletConnect(request);
    const queuedAt = Date.now();
    const measureCredential = request.method === "POST"
      && new URL(request.url).pathname === "/v1/credential";
    return this.#exclusive(async () => {
      await this.#ready;
      const startedAt = Date.now();
      try {
        return await this.#dispatch(request);
      } finally {
        if (measureCredential) credentialMetric({
          type: "egress.credential.resolve",
          queue_scope: "after_method_entry",
          queue_ms: startedAt - queuedAt,
          operation_ms: Date.now() - startedAt,
        });
      }
    }, { operation: measureCredential ? "credential_http" : "http",
      mutation: !measureCredential && request.method !== "GET" && request.method !== "HEAD" },
    () => jsonError(503, "credential_revocation_pending"));
  }

  /**
   * Grant a regional holder one plain read plus a bounded lease. Runs on the
   * canonical queue, so refresh/selection stay serialized and no grant runs
   * while a mutation is invalidating holders. Refused while any revocation is
   * pending. The lease is durable before the grant is returned.
   */
  async grantModelCredentialLease(owner: string, region: string): Promise<ModelCredentialLeaseGrant> {
    const refuse = (status: number): ModelCredentialLeaseGrant => ({ status, credential: null, epoch: 0, lease_ms: 0 });
    const namespace = this.#env.USER_CREDENTIALS;
    if (typeof owner !== "string" || typeof region !== "string" || !USER_ID.test(owner)
      || !LEASE_REGIONS.has(region) || !namespace || !this.#env.USER_CREDENTIAL_SNAPSHOTS
      || !namespace.idFromName(owner).equals(this.#state.id)) return refuse(403);
    return this.#exclusive(async () => {
      await this.#ready;
      const registry = this.#leaseRegistry;
      try {
        // The owner is authenticated above (idFromName equals this object);
        // it names every holder this registry must later invalidate.
        if (registry.owner === undefined) registry.owner = owner;
        if (registry.owner !== owner) return refuse(403);
        if (Object.keys(registry.pending).length && !await this.#revoke([])) return refuse(503);
        const before = this.#plainProjection();
        const credential = await this.#credential(false, undefined);
        if (await this.#reconcileLeases(before) !== "ok") return refuse(503);
        const projection = this.#plainProjection();
        const now = Date.now();
        const leaseMs = Math.floor(Math.min(MODEL_LEASE_MS, credential.expiresAt === undefined
          ? MODEL_LEASE_MS : credential.expiresAt - REFRESH_EARLY_MS - now));
        // Never hand an unregistered credential to a replica: an unregistered
        // recipient would receive no invalidation. The replica falls back to
        // the canonical read instead.
        if (!Number.isSafeInteger(leaseMs) || leaseMs <= 0) return refuse(503);
        registry.owner = owner;
        registry.leases[region] = { fingerprint: await leaseFingerprint(projection), epoch: registry.epoch,
          expiresAt: now + leaseMs };
        await this.#persistLeases();
        this.#reconciledProjection = projection;
        return { status: 200, credential, epoch: registry.epoch, lease_ms: leaseMs };
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return refuse(problem.status);
      }
    }, { operation: "lease_rpc" }, () => refuse(503));
  }

  /** Read the live snapshot under the same serialization and recovery as HTTP. */
  async resolveModelCredential(recover: boolean, revision?: number, accountId?: string): Promise<{
    status: number;
    credential: ModelCredentialValue | null;
    resolve_ms: number;
    activation_ms: number;
    activation_age_ms: number;
    resolve_id: string;
  }> {
    const startedAt = Date.now();
    const resolveId = crypto.randomUUID();
    let operationAt = startedAt;
    const result = await this.#exclusive(async () => {
      await this.#ready;
      operationAt = Date.now();
      try {
        return { status: 200, credential: await this.#credential(
          recover === true, Number.isSafeInteger(revision) ? revision : undefined, accountId, resolveId,
        ) };
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return { status: problem.status, credential: null };
      }
    }, { operation: "credential_rpc", resolveId }, () => ({ status: 503, credential: null }));
    credentialMetric({ type: "egress.credential.rpc", resolve_id: resolveId, status: result.status,
      queue_scope: "after_method_entry", recover: recover === true,
      queue_ms: operationAt - startedAt, operation_ms: Date.now() - operationAt,
      activation_ms: this.#activationMs, activation_age_ms: Date.now() - this.#activatedAt,
      activation_phases: { ...this.#activationPhases } });
    return {
      resolve_id: resolveId,
      ...result,
      resolve_ms: Date.now() - startedAt,
      activation_ms: this.#activationMs,
      activation_age_ms: Date.now() - this.#activatedAt,
    };
  }

  /** Connect authentication reads this broker's credential status after verifying
   * the wallet signature. Keep those callbacks outside the credential queue. */
  async #walletConnect(request: Request): Promise<Response> {
    const prepared = await this.#exclusive(async () => {
      await this.#ready;
      try {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) {
          return jsonError(415, "invalid_content_type");
        }
        const requestBody = validateWalletConnectRequest(
          await readJson(request, MAX_VAULT_BODY_BYTES),
          this.#env,
        );
        if (!requestBody) return jsonError(400, "invalid_wallet_connect_request");
        const wallet = Object.freeze({ ...await this.#ensureRootWallet() });
        return { wallet, requestBody };
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return jsonError(problem.status, problem.code);
      }
    }, { operation: "http" });
    if (prepared instanceof Response) return prepared;
    try {
      return json(await rootWalletProvider(prepared.wallet).request(prepared.requestBody as never), 200);
    } catch (error) {
      // Recovery mutates the retained credential snapshot, so it still joins the
      // queue even though the failed authentication callback ran outside it.
      return this.#exclusive(async () => {
        const problem = await this.#recoverFailedOperation(error);
        return jsonError(problem.status, problem.code);
      }, { operation: "http" });
    }
  }

  /** One live startup read. Network balance I/O must not hold credential rotation
   * or model resolution behind an unrelated read-only RPC. */
  async #walletSnapshot(request: Request): Promise<Response> {
    const wallet = await this.#exclusive(async () => {
      await this.#ready;
      return this.#publicWallet();
    }, { operation: "http" });
    if (!wallet) return jsonError(404, "wallet_not_configured");
    const controller = new AbortController();
    const abort = () => controller.abort(request.signal.reason);
    request.signal.addEventListener("abort", abort, { once: true });
    if (request.signal.aborted) abort();
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      // Leave transport time within Managed's existing 1.5s startup deadline.
      // Timeout retains the public identity and explicitly unavailable balance.
      const deadline = new Promise<null>((resolve) => {
        timer = setTimeout(() => { controller.abort(); resolve(null); }, 1_000);
      });
      const client = createClient({ chain: tempo, transport: http(TEMPO_RPC, {
        retryCount: 0, timeout: 1_000, fetchOptions: { signal: controller.signal },
      }) });
      const balance = await Promise.race([
        Actions.token.getBalance(client, { account: wallet.address as `0x${string}`, decimals: 6, token: MACHINE_USD })
          .then((value) => ({ account: wallet.address, balance: value.amount.toString(),
            decimals: 6, symbol: "MACH", token: MACHINE_USD }))
          .catch(() => null),
        deadline,
      ]);
      const response = json({ ...wallet, balance }, 200);
      response.headers.set("content-type", "application/vnd.nanocodex.wallet-snapshot+json");
      response.headers.set("vary", "Accept");
      return response;
    } finally {
      clearTimeout(timer);
      request.signal.removeEventListener("abort", abort);
    }
  }

  /** Private service-binding RPC only: never expose this credential to account clients. */
  resolveClaudeCredential(recover = false, rejectedRevision?: string): Promise<{
    status: number; credential: ClaudeCredential | null;
  }> {
    return this.#exclusive(async () => {
      await this.#ready;
      try {
        const subscription = await this.#claudeSubscription();
        // A cached rejected generation is only a hint. Rust compares its exact
        // bearer against durable state before invalidation, fencing rotations.
        let rejected = this.#claudeCredential;
        if (recover && !rejected) rejected = await subscription.credential();
        if (recover && rejected && rejected.revision === rejectedRevision) {
          await subscription.recover(rejected.headers);
        }
        const credential = await subscription.credential();
        this.#claudeCredential = credential;
        await this.#noteClaudeRevision(credential.revision, recover);
        return { status: 200, credential };
      } catch {
        this.#claudeCredential = undefined;
        await this.#noteClaudeRevision(null, true).catch(() => {});
        return { status: 401, credential: null };
      }
    }, { operation: "credential_rpc" });
  }

  /**
   * Record a Claude plain-read change in the durable lease projection. The
   * surrounding #exclusive reconciles (invalidates) every stale holder before
   * the operation is acknowledged. Runs only on the canonical queue.
   */
  async #noteClaudeRevision(revision: string | null, mutation: boolean): Promise<void> {
    const registry = this.#leaseRegistry;
    const current = registry.claude;
    if (!mutation && current && current.revision === revision) return;
    if (!this.#leasesHeld() && current === undefined && !mutation) {
      registry.claude = { generation: 0, revision };
      return;
    }
    registry.claude = { generation: (current?.generation ?? 0) + (mutation ? 1 : 0), revision };
    await this.#persistLeases();
  }

  /** Grant a regional holder one plain Claude read plus a bounded lease. */
  async grantClaudeCredentialLease(owner: string, region: string): Promise<ClaudeCredentialLeaseGrant> {
    const refuse = (status: number): ClaudeCredentialLeaseGrant => ({ status, credential: null, epoch: 0, lease_ms: 0 });
    const namespace = this.#env.USER_CREDENTIALS;
    if (typeof owner !== "string" || typeof region !== "string" || !USER_ID.test(owner)
      || !LEASE_REGIONS.has(region) || !namespace || !this.#env.USER_CREDENTIAL_SNAPSHOTS
      || !namespace.idFromName(owner).equals(this.#state.id)) return refuse(403);
    return this.#exclusive(async () => {
      await this.#ready;
      const registry = this.#leaseRegistry;
      try {
        if (registry.owner === undefined) registry.owner = owner;
        if (registry.owner !== owner) return refuse(403);
        if (Object.keys(registry.pending).length && !await this.#revoke([])) return refuse(503);
        const before = this.#plainProjection();
        let credential: ClaudeCredentialValue;
        try {
          credential = await (await this.#claudeSubscription()).credential();
        } catch {
          this.#claudeCredential = undefined;
          return refuse(401);
        }
        this.#claudeCredential = credential;
        await this.#noteClaudeRevision(credential.revision, false);
        if (await this.#reconcileLeases(before) !== "ok") return refuse(503);
        const projection = this.#plainProjection();
        const now = Date.now();
        registry.owner = owner;
        registry.leases[region] = { fingerprint: await leaseFingerprint(projection), epoch: registry.epoch,
          expiresAt: now + CLAUDE_LEASE_MS };
        await this.#persistLeases();
        this.#reconciledProjection = projection;
        return { status: 200, credential, epoch: registry.epoch, lease_ms: CLAUDE_LEASE_MS };
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return refuse(problem.status);
      }
    }, { operation: "lease_rpc" }, () => refuse(503));
  }

  #claudeSubscription(): Promise<ClaudeSubscription.Subscription> {
    return this.#claude ??= this.#openClaudeSubscription().catch(() => {
      this.#claude = undefined;
      throw new BrokerFailure(503, "claude_broker_unavailable");
    });
  }

  async #openClaudeSubscription(): Promise<ClaudeSubscription.Subscription> {
    const id = `claude:${this.#state.id.toString()}`;
    const vault = new CredentialVault(this.#env, `user/${this.#state.id.toString()}/claude-subscription`);
    return ClaudeSubscription.open({ id, module: claudeModule, store: {
      load: async (key) => {
        if (key !== id) throw new Error("Claude scope denied");
        const row = await this.#state.storage.get<ClaudeRow>(CLAUDE_STATE_KEY);
        if (!row) return { revision: "0" };
        const opened = await vault.open<string>(row.envelope);
        if (opened.reseal) {
          const envelope = await vault.seal(opened.value);
          await this.#state.storage.transaction(async (tx) => {
            const current = await tx.get<ClaudeRow>(CLAUDE_STATE_KEY);
            if (current?.revision === row.revision) await tx.put(CLAUDE_STATE_KEY, { revision: row.revision, envelope });
          });
        }
        return { revision: row.revision, payload: opened.value };
      },
      compareAndSwap: async (key, input) => {
        if (key !== id) throw new Error("Claude scope denied");
        if (!/^(0|[1-9][0-9]*)$/.test(input.expectedRevision)
          || BigInt(input.expectedRevision) >= 18446744073709551615n) throw new Error("Claude revision exhausted");
        const next = (BigInt(input.expectedRevision) + 1n).toString();
        const envelope = await vault.seal(input.payload);
        return this.#state.storage.transaction(async (tx) => {
          const row = await tx.get<ClaudeRow>(CLAUDE_STATE_KEY);
          const actualRevision = row?.revision ?? "0";
          if (actualRevision !== input.expectedRevision) return { status: "conflict" as const, actualRevision };
          await tx.put(CLAUDE_STATE_KEY, { revision: next, envelope });
          return { status: "committed" as const, revision: next };
        });
      },
    } });
  }

  async #claudeLoginStatus(): Promise<ClaudeSubscription.Status> {
    const subscription = await this.#claudeSubscription();
    const status = await subscription.status();
    if (status.state !== "validating") return status;
    // A token exchange already committed its staged token. Public polling must
    // be able to finish the replayable profile GET after transient failure or
    // DO restart, without asking the browser to repeat its one-time code. Only
    // Rust owns recovery/refresh: never retry an ambiguous exchange, synthesize
    // a token request here, or return the private credential to this caller.
    try { this.#claudeCredential = await subscription.credential(); }
    catch { this.#claudeCredential = undefined; }
    return subscription.status();
  }

  async #claudePublicStatus(): Promise<{ connected: boolean; state: string }> {
    // An untouched account is cheap and does not initialize the WASM module.
    if (!this.#claude && !await this.#state.storage.get(CLAUDE_STATE_KEY)) {
      return { connected: false, state: "signed_out" };
    }
    const status = await this.#claudeLoginStatus();
    // Connection denotes a validated stored grant, not access-token lifetime.
    // Private catalog/egress resolves and refreshes through Rust before use.
    return { connected: status.state === "authenticated", state: status.state };
  }

  /** Private metadata RPC. Entry secrets remain in their separate vault records. */
  readVaultMetadata(): Promise<CloudflareAccountVaultResult> {
    return this.#exclusive(async () => {
      await this.#ready;
      try {
        return { status: 200, vault: this.#publicVault() };
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return { status: problem.status, vault: null };
      }
    }, { operation: "metadata_rpc" });
  }

  alarm(): Promise<void> {
    return this.#exclusive(async () => {
      await this.#ready;
      if (this.#credentials.login && this.#credentials.login.expiresAt <= Date.now()) {
        delete this.#credentials.login;
        await this.#persist();
      }
      for (const credential of this.#chatGptAccounts()) {
        if (credential && !credential.deadReason && credential.refreshToken
          && credential.expiresAt <= Date.now() + REFRESH_EARLY_MS
          && (credential.refreshAfter ?? 0) <= Date.now()) {
          try {
            await this.#refreshChatGpt(credential, "alarm");
          } catch (error) {
            console.warn({
              type: "user_credential.refresh_failed",
              code: failure(error).code,
            });
          }
        }
      }
      await this.#schedule();
    }, { operation: "alarm" }, () => undefined);
  }

  async #exclusive<T>(
    operation: () => Promise<T>,
    observation: CredentialOperationObservation,
    revocationFailure?: () => T,
  ): Promise<T> {
    // Only the application queue after method entry is visible here. The
    // runtime can hold invocation delivery behind blockConcurrencyWhile.
    const queuedAt = Date.now();
    const operationsAhead = this.#pendingOperations++;
    const activeAtEnqueue = this.#activeOperation ?? "none";
    const previous = this.#tail;
    let release!: () => void;
    this.#tail = new Promise<void>((resolve) => { release = resolve; });
    if (operationsAhead > 0) credentialMetric({
      type: "egress.credential.queued",
      operation: observation.operation,
      ...(observation.resolveId ? { resolve_id: observation.resolveId } : {}),
      operations_ahead: operationsAhead,
      active_operation_at_enqueue: activeAtEnqueue,
    });
    await previous;
    const startedAt = Date.now();
    const waitingAtStart = this.#pendingOperations - 1;
    this.#activeOperation = observation.operation;
    let completed = false;
    try {
      // Every queued operation may change what a plain read returns. Before
      // it is acknowledged, every regional lease holder whose snapshot no
      // longer matches is invalidated; no new grant can run meanwhile.
      const ready = await this.#ready.then(() => true, () => false);
      const before = ready && this.#leasesHeld() ? this.#plainProjection() : undefined;
      let result: T;
      try {
        result = await operation();
      } catch (error) {
        if (ready) await this.#reconcileLeases(before).catch(() => "failed_changed");
        throw error;
      }
      const revoked = ready ? await this.#reconcileLeases(before).catch(() => "failed_changed" as const) : "ok";
      if (revoked === "failed_changed" || (revoked === "failed_unchanged" && observation.mutation === true)) {
        credentialMetric({ type: "egress.credential.revocation_pending", operation: observation.operation });
        if (revocationFailure) return revocationFailure();
        throw new BrokerFailure(503, "credential_revocation_pending");
      }
      completed = true;
      return result;
    } finally {
      const finishedAt = Date.now();
      this.#pendingOperations -= 1;
      this.#activeOperation = undefined;
      release();
      credentialMetric({
        type: "egress.credential.operation",
        operation: observation.operation,
        ...(observation.resolveId ? { resolve_id: observation.resolveId } : {}),
        outcome: completed ? "returned" : "threw",
        queue_scope: "after_method_entry",
        operations_ahead: operationsAhead,
        active_operation_at_enqueue: activeAtEnqueue,
        waiting_at_start: waitingAtStart,
        waiting_at_finish: this.#pendingOperations,
        exclusive_wait_ms: startedAt - queuedAt,
        exclusive_operation_ms: finishedAt - startedAt,
      });
    }
  }

  #leasesHeld(): boolean {
    return Object.keys(this.#leaseRegistry.leases).length > 0
      || Object.keys(this.#leaseRegistry.pending).length > 0;
  }

  /** Side-effect-free mirror of the plain read's selection (no refresh). Any
   * difference, including the token itself, means a lease holder is stale. */
  #plainProjection(): string {
    const claude = this.#leaseRegistry.claude;
    return JSON.stringify([this.#modelProjection(), claude ? [claude.generation, claude.revision] : null]);
  }

  #modelProjection(): string {
    const state = this.#credentials;
    if (state.active === "openai" && state.openai) {
      return JSON.stringify(["openai", state.openai.revision, state.openai.secret]);
    }
    if (state.active !== "chatgpt") return JSON.stringify(["none", state.active]);
    const now = Date.now();
    const account = this.#chatGptAccounts().find((candidate) => !candidate.deadReason
      && (candidate.limitedUntil ?? 0) <= now
      && (candidate.expiresAt > now || (candidate.refreshToken && (candidate.refreshAfter ?? 0) <= now)))
      ?? state.chatgpt;
    if (!account) return JSON.stringify(["none", "chatgpt"]);
    return JSON.stringify(["chatgpt", account.accountId, account.revision, account.accessToken,
      account.expiresAt, account.fedramp, account.provenance ?? null, account.deadReason,
      (account.limitedUntil ?? 0) > now]);
  }

  async #persistLeases(): Promise<void> {
    await this.#state.storage.put(LEASE_KEY, {
      envelope: await this.#leaseVault.seal(this.#leaseRegistry),
    } satisfies StoredRow);
  }

  /**
   * Invalidate stale holders. "failed_changed" means this operation changed
   * the plain read and some holder could not be invalidated: the caller must
   * receive a failure. The pending revocation is durable and retried by the
   * alarm and before any new grant.
   */
  async #reconcileLeases(before: string | undefined): Promise<"ok" | "failed_changed" | "failed_unchanged"> {
    const registry = this.#leaseRegistry;
    if (!this.#leasesHeld()) return "ok";
    const projection = this.#plainProjection();
    const changed = before !== undefined && before !== projection;
    // No time-based pruning: expiry only stops local use, it never proves
    // revocation. Holders (bounded by the seven regions) and pending
    // revocations leave the registry only on a durable holder ACK.
    const pending = Object.keys(registry.pending).length > 0;
    if (!pending && projection === this.#reconciledProjection) return "ok";
    const digest = await leaseFingerprint(projection);
    const stale = Object.entries(registry.leases)
      .filter(([, lease]) => lease.fingerprint !== digest).map(([region]) => region);
    if (!stale.length && !pending) {
      this.#reconciledProjection = projection;
      return "ok";
    }
    if (await this.#revoke(stale)) {
      this.#reconciledProjection = projection;
      return "ok";
    }
    return changed ? "failed_changed" : "failed_unchanged";
  }

  /** Durably mark holders pending under a new epoch, then invalidate every
   * pending holder. Returns true only when none remain pending. */
  async #revoke(regions: readonly string[]): Promise<boolean> {
    const registry = this.#leaseRegistry;
    registry.epoch += 1;
    const epoch = registry.epoch;
    for (const region of regions) {
      const lease = registry.leases[region]!;
      // ABA-safe: a pending holder is invalidated even if the fingerprint
      // later matches again.
      registry.pending[region] = { epoch, expiresAt: Math.max(lease.expiresAt, registry.pending[region]?.expiresAt ?? 0) };
      delete registry.leases[region];
    }
    await this.#persistLeases();
    const owner = registry.owner;
    const targets = Object.keys(registry.pending);
    const outcomes = await Promise.all(targets.map((region) => this.#invalidateHolder(owner, region, epoch)));
    targets.forEach((region, index) => { if (outcomes[index]) delete registry.pending[region]; });
    await this.#persistLeases();
    const remaining = Object.keys(registry.pending).length;
    credentialMetric({ type: "egress.credential.lease_revocation", holders: targets.length,
      failed: remaining, epoch });
    if (remaining) await this.#ensureAlarmBy(Date.now() + REVOCATION_RETRY_MS);
    return remaining === 0;
  }

  async #invalidateHolder(owner: string | undefined, region: string, epoch: number): Promise<boolean> {
    const namespace = this.#env.USER_CREDENTIAL_SNAPSHOTS;
    if (!owner || !namespace) return false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const stub = namespace.getByName(`${region}:${owner}`, { locationHint: region as DurableObjectLocationHint });
      return await Promise.race([
        Promise.resolve(stub.invalidate(owner, region, epoch)).then((value) => value === true),
        new Promise<false>((resolve) => { timer = setTimeout(() => resolve(false), INVALIDATE_TIMEOUT_MS); }),
      ]);
    } catch {
      return false;
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
  }

  async #initialize(): Promise<void> {
    let phaseStartedAt = Date.now();
    const advance = (phase: CredentialActivationPhase): void => {
      const now = Date.now();
      this.#activationPhases[this.#activationPhase] = now - phaseStartedAt;
      this.#activationPhase = phase;
      phaseStartedAt = now;
    };
    try {
      // One storage round trip for credential state and the lease registry.
      const rows = await this.#state.storage.get<unknown>([STATE_KEY, LEASE_KEY]);
      const leaseRow = rows.get(LEASE_KEY) as StoredRow | undefined;
      if (leaseRow) {
        // Fail closed: an unreadable or invalid registry may hide holders and
        // their epochs, and no safe epoch can be guessed. Initialization
        // rejects, so every grant, read and mutation fails until an operator
        // repairs the row; nothing is acknowledged without holder ACKs.
        let leases: LeaseRegistry;
        try {
          leases = (await this.#leaseVault.open<LeaseRegistry>(leaseRow.envelope)).value;
        } catch {
          credentialMetric({ type: "egress.credential.lease_registry_invalid", reason: "unreadable" });
          throw new BrokerFailure(503, "credential_lease_registry_unreadable");
        }
        if (!validLeaseRegistry(leases)) {
          credentialMetric({ type: "egress.credential.lease_registry_invalid", reason: "invalid" });
          throw new BrokerFailure(503, "credential_lease_registry_invalid");
        }
        this.#leaseRegistry = leases;
      }
      const row = rows.get(STATE_KEY) as StoredRow | undefined;
      if (!row) {
        if (Object.keys(this.#leaseRegistry.pending).length) await this.#ensureAlarmBy(Date.now() + 1_000);
        return;
      }
      advance("vault_open_ms");
      const opened = await this.#vault.open<CredentialState>(row.envelope);
      advance("restore_ms");
      const installed = this.#installRestoredState(opened.value);
      if (installed.legacy.length) {
        advance("migration_ms");
        await this.#migrateLegacyVault(installed.legacy);
      } else if (installed.changed || opened.reseal) {
        advance("reseal_ms");
        await this.#persist();
      }
      advance("alarm_ms");
      // An existing alarm survives eviction. Rewriting it on every activation
      // adds a storage write and can postpone an alarm that woke this object.
      const alarm = this.#nextAlarm();
      // OpenAI-only and vault-only accounts have no refresh work. They do not
      // need an alarm read on the cold model/metadata path.
      if (alarm !== undefined && await this.#state.storage.getAlarm() === null) {
        await this.#state.storage.setAlarm(alarm);
      }
      if (Object.keys(this.#leaseRegistry.pending).length) await this.#ensureAlarmBy(Date.now() + 1_000);
    } finally {
      this.#activationPhases[this.#activationPhase] = Date.now() - phaseStartedAt;
    }
  }

  async #dispatch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (["/v1/provider-capture", "/v1/provider-store", "/v1/provider-card", "/v1/provider-bindings"].includes(url.pathname)) {
      return providerVaultRoute(request, { storage: this.#state.storage, vault: this.#vault, validEntry: (value, kind) => Boolean(validateVaultEntryPayload(value, kind)), dispatch: request => this.#dispatch(request) });
    }
    try {
      if (request.method === "GET" && url.pathname === "/v1/health") {
        return json({ ready: true }, 200);
      }
      if (url.pathname === "/v1/claude/login/start" && request.method === "POST") {
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        this.#claudeCredential = undefined;
        await this.#noteClaudeRevision(null, true);
        return json({ state: "pending", ...await (await this.#claudeSubscription()).startLogin() }, 200);
      }
      if (url.pathname === "/v1/claude/login/status" && request.method === "GET") {
        return json(await this.#claudeLoginStatus(), 200);
      }
      if (url.pathname === "/v1/claude/login/complete" && request.method === "POST") {
        const body = await readJson(request, 10 * 1024);
        if (!body || Object.keys(body).length !== 1 || typeof body.code !== "string"
          || body.code.length === 0 || body.code.length > 8192) return jsonError(400, "invalid_claude_code");
        try {
          this.#claudeCredential = undefined;
          await this.#noteClaudeRevision(null, true);
          return json(await (await this.#claudeSubscription()).completeLogin(body.code), 200);
        } catch {
          // Never reflect a provider response or private completion material.
          const status = await (await this.#claudeSubscription()).status();
          return json({ error: "claude_login_failed", ...status }, status.state === "exchange_uncertain" ? 409 : 400);
        }
      }
      if (url.pathname === "/v1/claude" && request.method === "DELETE") {
        this.#claudeCredential = undefined;
        await this.#noteClaudeRevision(null, true);
        await (await this.#claudeSubscription()).logout();
        return json(await this.#claudePublicStatus(), 200);
      }
      if (request.method === "GET" && url.pathname === "/v1/status") {
        return json(await this.#publicStatus(), 200);
      }
      if (request.method === "GET" && url.pathname === "/v1/vault") {
        return json({ vault: this.#publicVault() }, 200);
      }
      if (url.pathname === "/v1/sponsored-prompts") {
        if (request.method === "GET") {
          return json(await this.#sponsoredPromptStatus(), 200);
        }
        if (request.method === "POST") {
          if (!isJsonContentType(request.headers.get("content-type"))) {
            return jsonError(415, "invalid_content_type");
          }
          const body = await readJson(request, 1_024);
          const promptId = stringField(body, "prompt_id");
          const promptHash = stringField(body, "prompt_hash");
          if (!promptId || !SPONSORED_PROMPT_ID.test(promptId)
            || !promptHash || !SPONSORED_PROMPT_HASH.test(promptHash)) {
            return jsonError(400, "invalid_sponsored_prompt");
          }
          return this.#reserveSponsoredPrompt(promptId, promptHash);
        }
        return jsonError(405, "method_not_allowed");
      }
      if (url.pathname === "/v1/sponsored-prompts/reset") {
        if (!localSponsoredTrialResetEnabled(this.#env)) return jsonError(404, "not_found");
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        await this.#state.storage.delete(SPONSORED_PROMPT_STATE_KEY);
        return json(sponsoredPromptStatus([]), 200);
      }
      if (url.pathname === "/v1/sponsored-connections") {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) {
          return jsonError(415, "invalid_content_type");
        }
        const body = await readJson(request, 1_024);
        const action = stringField(body, "action");
        const connectionId = stringField(body, "connection_id");
        if (!connectionId || !SPONSORED_CONNECTION_ID.test(connectionId)
          || (action !== "acquire" && action !== "heartbeat" && action !== "release")) {
          return jsonError(400, "invalid_sponsored_connection");
        }
        return this.#setSponsoredConnection(connectionId, action);
      }
      if (url.pathname === "/v1/sponsored-prompts/continuation") {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) {
          return jsonError(415, "invalid_content_type");
        }
        const body = await readJson(request, 16 * 1024);
        const action = stringField(body, "action");
        const responseId = stringField(body, "response_id");
        const callIds = sponsoredCallIds(isRecord(body) ? body.call_ids : undefined);
        if ((responseId !== undefined && !SPONSORED_RESPONSE_ID.test(responseId)) || !callIds) {
          return jsonError(400, "invalid_sponsored_continuation");
        }
        if (action === "grant") {
          const promptId = stringField(body, "prompt_id");
          const attempt = numberField(body, "attempt");
          if (!promptId || !SPONSORED_PROMPT_ID.test(promptId) || !responseId
            || !attempt || attempt > SPONSORED_PROMPT_MAX_ATTEMPTS) {
            return jsonError(400, "invalid_sponsored_continuation");
          }
          return this.#grantSponsoredContinuation(promptId, attempt, responseId, callIds);
        }
        if (action === "consume") {
          const promptId = stringField(body, "prompt_id");
          const promptHash = stringField(body, "prompt_hash");
          if ((promptId !== undefined && !SPONSORED_PROMPT_ID.test(promptId))
            || (promptHash !== undefined && !SPONSORED_PROMPT_HASH.test(promptHash))) {
            return jsonError(400, "invalid_sponsored_continuation");
          }
          return this.#consumeSponsoredContinuation(
            responseId,
            callIds,
            promptId,
            promptHash,
          );
        }
        return jsonError(400, "invalid_sponsored_continuation");
      }
      if (url.pathname === "/v1/sponsored-prompts/lifecycle") {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) {
          return jsonError(415, "invalid_content_type");
        }
        const body = await readJson(request, 1_024);
        const action = stringField(body, "action");
        const promptId = stringField(body, "prompt_id");
        const attempt = numberField(body, "attempt");
        if (!promptId || !SPONSORED_PROMPT_ID.test(promptId)
          || !attempt || attempt > SPONSORED_PROMPT_MAX_ATTEMPTS
          || (action !== "terminal" && action !== "interrupted" && action !== "heartbeat")) {
          return jsonError(400, "invalid_sponsored_prompt_lifecycle");
        }
        return this.#setSponsoredPromptLifecycle(promptId, attempt, action);
      }
      if (url.pathname === "/v1/wallet/identity") {
        if (request.method !== "GET") return jsonError(405, "method_not_allowed");
        return this.#credentials.wallet ? json(publicRootWallet(this.#credentials.wallet), 200) : jsonError(404, "wallet_not_configured");
      }
      if (["/v1/wallet/link/poll", "/v1/wallet/link/cancel", "/v1/wallet/unlink"].includes(url.pathname)) {
        return this.#walletLinkAction(request, url.pathname);
      }
      if (url.pathname === "/v1/wallet") {
        if (request.method === "GET") {
          const wallet = this.#publicWallet();
          return wallet ? json(wallet, 200) : jsonError(404, "wallet_not_configured");
        }
        if (request.method === "PUT") {
          if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
          return json(publicRootWallet(await this.#ensureRootWallet()), 200);
        }
        return jsonError(405, "method_not_allowed");
      }
      if (url.pathname === "/v1/wallet/balance") {
        if (request.method !== "GET") return jsonError(405, "method_not_allowed");
        const wallet = this.#publicWallet();
        if (!wallet) return jsonError(404, "wallet_not_configured");
        const client = createClient({ chain: tempo, transport: http(TEMPO_RPC, { retryCount: 0, timeout: 5_000 }) });
        const balance = await Actions.token.getBalance(client, {
          account: wallet.address as `0x${string}`,
          decimals: 6,
          token: MACHINE_USD,
        });
        return json({
          account: wallet.address,
          balance: balance.amount.toString(),
          decimals: 6,
          symbol: "MACH",
          token: MACHINE_USD,
        }, 200);
      }
      if (url.pathname === "/v1/wallet/mercator/credential") {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) return jsonError(415, "invalid_content_type");
        if (!this.#credentials.wallet) return jsonError(404, "wallet_not_configured");
        let paymentRequest: unknown;
        try { paymentRequest = await readJson(request, 64 * 1024); }
        catch { return jsonError(400, "invalid_mercator_payment_request"); }
        try {
          return json({ credential: await createMercatorMcpCredential(paymentRequest, {
            store: this.#state.storage, wallet: await this.#spendingWalletProvider(), signal: request.signal,
          }) }, 200);
        } catch (error) {
          return error instanceof MercatorPaymentInputError
            ? jsonError(error.status, error.code)
            : jsonError(503, "mercator_outcome_unknown");
        }
      }
      if (url.pathname === "/v1/wallet/revoke-access-key") {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) {
          return jsonError(415, "invalid_content_type");
        }
        const requestBody = validateWalletRevokeRequest(await readJson(request, MAX_VAULT_BODY_BYTES));
        if (!requestBody) return jsonError(400, "invalid_wallet_revoke_request");
        const wallet = await this.#ensureRootWallet();
        const requested = requestBody.params[0]?.address;
        if (typeof requested !== "string" || requested.toLowerCase() !== wallet.address.toLowerCase()) {
          return jsonError(403, "wallet_address_mismatch");
        }
        const provider = rootWalletProvider(wallet);
        await provider.request({
          method: "wallet_connect",
          params: [{ chainId: TEMPO_CHAIN_ID, capabilities: { method: "login" } }],
        } as never);
        await provider.request(requestBody as never);
        return json({ ok: true }, 200);
      }
      if (url.pathname === "/v1/browser-cookie-jars") {
        if (request.method !== "GET") return jsonError(405, "method_not_allowed");
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        return json({
          browser_cookie_jars: Object.values(this.#credentials.browserCookieJars ?? {})
            .sort((left, right) => right.updatedAt - left.updatedAt
              || compareText(left.id, right.id))
            .map(publicBrowserCookieJarMetadata),
        }, 200);
      }
      const browserCookieJarMatch = url.pathname.match(
        /^\/v1\/browser-cookie-jars\/([A-Za-z0-9_-]{22,64})(?:\/(materialize|names))?$/,
      );
      if (browserCookieJarMatch) {
        const id = browserCookieJarMatch[1]!;
        const projection = browserCookieJarMatch[2];
        if (!BROWSER_COOKIE_JAR_ID.test(id)) {
          return jsonError(400, "invalid_browser_cookie_jar_id");
        }
        if (request.method === "PUT" && !projection) {
          if (!isJsonContentType(request.headers.get("content-type"))) {
            return jsonError(415, "invalid_content_type");
          }
          const upsert = validateBrowserCookieJarUpsert(
            await readJson(request, MAX_BROWSER_COOKIE_JAR_BODY_BYTES),
          );
          if (!upsert) return jsonError(400, "invalid_browser_cookie_jar");
          const current = this.#credentials.browserCookieJars?.[id];
          if (current && !sameBrowserCookieJarBinding(current, upsert)) {
            return jsonError(409, "browser_cookie_jar_binding_conflict");
          }
          if ((current?.revision ?? 0) !== upsert.revision) {
            return jsonError(409, "browser_cookie_jar_revision_conflict");
          }
          if (!current
            && Object.keys(this.#credentials.browserCookieJars ?? {}).length
              >= MAX_BROWSER_COOKIE_JARS) {
            return jsonError(409, "browser_cookie_jar_limit_reached");
          }
          if (upsert.revision >= Number.MAX_SAFE_INTEGER) {
            return jsonError(409, "browser_cookie_jar_revision_conflict");
          }
          const jar: BrowserCookieJarV1 = {
            schemaVersion: 1,
            id,
            origin: upsert.origin,
            profileId: upsert.profileId,
            storeId: upsert.storeId,
            revision: upsert.revision + 1,
            updatedAt: Date.now(),
            cookies: upsert.cookies,
          };
          const metadata = browserCookieJarMetadata(jar);
          const next: CredentialState = {
            ...this.#credentials,
            browserCookieJars: {
              ...this.#credentials.browserCookieJars,
              [id]: metadata,
            },
          };
          const [stateEnvelope, jarEnvelope] = await Promise.all([
            this.#vault.seal(next),
            this.#browserCookieJarVault(id).seal(jar),
          ]);
          await this.#state.storage.transaction(async (transaction) => {
            await transaction.put(STATE_KEY, { envelope: stateEnvelope } satisfies StoredRow);
            await transaction.put(browserCookieJarStorageKey(id), {
              envelope: jarEnvelope,
            } satisfies StoredRow);
          });
          this.#credentials = next;
          return json(publicBrowserCookieJarMetadata(metadata), current ? 200 : 201);
        }
        if (request.method === "POST" && projection) {
          if (!isJsonContentType(request.headers.get("content-type"))) {
            return jsonError(415, "invalid_content_type");
          }
          const binding = validateBrowserCookieJarBinding(await readJson(request, 8 * 1024));
          if (!binding) return jsonError(400, "invalid_browser_cookie_jar_binding");
          const metadata = this.#credentials.browserCookieJars?.[id];
          if (!metadata) return jsonError(404, "browser_cookie_jar_not_found");
          if (!sameBrowserCookieJarBinding(metadata, binding)) {
            return jsonError(409, "browser_cookie_jar_binding_conflict");
          }
          const row = await this.#state.storage.get<StoredRow>(browserCookieJarStorageKey(id));
          if (!row) return jsonError(404, "browser_cookie_jar_not_found");
          const opened = await this.#browserCookieJarVault(id).open<unknown>(row.envelope);
          const jar = validateStoredBrowserCookieJar(id, opened.value);
          if (!jar || !sameBrowserCookieJarMetadata(metadata, browserCookieJarMetadata(jar))) {
            return jsonError(503, "browser_cookie_jar_invalid");
          }
          if (opened.reseal) {
            await this.#state.storage.put(browserCookieJarStorageKey(id), {
              envelope: await this.#browserCookieJarVault(id).seal(jar),
            } satisfies StoredRow);
          }
          return json(
            projection === "names"
              ? publicBrowserCookieJarNames(jar)
              : publicBrowserCookieJar(jar),
            200,
          );
        }
        if (request.method === "DELETE" && !projection) {
          if (!isJsonContentType(request.headers.get("content-type"))) {
            return jsonError(415, "invalid_content_type");
          }
          const deletion = validateBrowserCookieJarDelete(await readJson(request, 8 * 1024));
          if (!deletion) return jsonError(400, "invalid_browser_cookie_jar_delete");
          const current = this.#credentials.browserCookieJars?.[id];
          if (!current) return new Response(null, { status: 204, headers: noStoreHeaders() });
          if (!sameBrowserCookieJarBinding(current, deletion)) {
            return jsonError(409, "browser_cookie_jar_binding_conflict");
          }
          if (current.revision !== deletion.revision) {
            return jsonError(409, "browser_cookie_jar_revision_conflict");
          }
          const jars = { ...this.#credentials.browserCookieJars };
          delete jars[id];
          const next: CredentialState = { ...this.#credentials };
          if (Object.keys(jars).length) next.browserCookieJars = jars;
          else delete next.browserCookieJars;
          const stateEnvelope = await this.#vault.seal(next);
          await this.#state.storage.transaction(async (transaction) => {
            await transaction.put(STATE_KEY, { envelope: stateEnvelope } satisfies StoredRow);
            await transaction.delete(browserCookieJarStorageKey(id));
          });
          this.#credentials = next;
          return new Response(null, { status: 204, headers: noStoreHeaders() });
        }
        return jsonError(405, "method_not_allowed");
      }
      const vaultMatch = url.pathname.match(
        /^\/v1\/vault\/(login|api_key|card|address|phone|totp)(?:\/([A-Za-z0-9_-]{22,64}))?$/,
      );
      if (vaultMatch) {
        const kind = vaultMatch[1] as VaultKind;
        const id = vaultMatch[2];
        if (request.method === "POST" && !id) {
          if (!isJsonContentType(request.headers.get("content-type"))) {
            return jsonError(415, "invalid_content_type");
          }
          const payload = validateVaultEntryPayload(
            await readJson(request, MAX_VAULT_BODY_BYTES),
            kind,
          );
          if (!payload) return jsonError(400, "invalid_vault_entry");
          const operation = request.headers.get("x-nanocodex-operation-id");
          if (operation !== null && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(operation)) return jsonError(400, "invalid_operation_id");
          const operationKey = operation ? `vault-create-operation:${operation.toLowerCase()}` : undefined;
          const priorId = operationKey ? await this.#state.storage.get<string>(operationKey) : undefined;
          if (priorId) {
            const prior = await this.#state.storage.get<StoredRow>(vaultEntryStorageKey(priorId));
            if (!prior) return jsonError(410, "vault_entry_deleted");
            const opened = await this.#entryVault(priorId).open<VaultEntry>(prior.envelope);
            const retained = validateStoredVaultEntry(priorId, opened.value);
            if (!retained) return jsonError(409, "vault_operation_unavailable");
            const { id: _id, createdAt: _created, ...previous } = retained;
            if (JSON.stringify(previous) !== JSON.stringify(payload)) return jsonError(409, "vault_operation_conflict");
            return json(publicVaultEntry(retained), 201);
          }
          if (Object.keys(this.#credentials.vault ?? {}).length >= MAX_VAULT_ENTRIES) {
            return jsonError(409, "vault_entry_limit_reached");
          }
          let generatedId: string;
          do { generatedId = randomVaultId(); }
          while (this.#credentials.vault?.[generatedId] !== undefined);
          const entry = { ...payload, id: generatedId, createdAt: Date.now() } as VaultEntry;
          const metadata = vaultEntryMetadata(entry);
          const next: CredentialState = {
            ...this.#credentials,
            vault: { ...this.#credentials.vault, [generatedId]: metadata },
          };
          const [stateEnvelope, entryEnvelope] = await Promise.all([
            this.#vault.seal(next),
            this.#entryVault(generatedId).seal(entry),
          ]);
          await this.#state.storage.transaction(async (transaction) => {
            await transaction.put(STATE_KEY, { envelope: stateEnvelope } satisfies StoredRow);
            if (operationKey) await transaction.put(operationKey, generatedId);
            await transaction.put(vaultEntryStorageKey(generatedId), {
              envelope: entryEnvelope,
            } satisfies StoredRow);
          });
          this.#credentials = next;
          return json(publicVaultEntry(entry), 201);
        }
        if (request.method === "DELETE" && id) {
          const current = this.#credentials.vault?.[id];
          if (current?.kind === kind) {
            const entries = { ...this.#credentials.vault };
            delete entries[id];
            const next: CredentialState = { ...this.#credentials };
            if (Object.keys(entries).length) next.vault = entries;
            else delete next.vault;
            const stateEnvelope = await this.#vault.seal(next);
            await this.#state.storage.transaction(async (transaction) => {
              await transaction.put(STATE_KEY, { envelope: stateEnvelope } satisfies StoredRow);
              await transaction.delete(vaultEntryStorageKey(id));
            });
            this.#credentials = next;
          }
          return new Response(null, { status: 204, headers: noStoreHeaders() });
        }
        return jsonError(405, "method_not_allowed");
      }
      const originId = url.pathname.match(/^\/v1\/vault\/login\/([A-Za-z0-9_-]{22,64})\/origin$/)?.[1];
      if (originId) {
        if (request.method !== "PUT") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) return jsonError(415, "invalid_content_type");
        const body = await readJson(request, 4096);
        if (!isRecord(body) || !hasExactKeys(body, ["browser_origin"]) || !validBrowserOrigin(body.browser_origin)) {
          return jsonError(400, "invalid_browser_origin");
        }
        const metadata = this.#credentials.vault?.[originId];
        if (metadata?.kind !== "login") return jsonError(404, "vault_entry_not_configured");
        const row = await this.#state.storage.get<StoredRow>(vaultEntryStorageKey(originId));
        if (!row) return jsonError(404, "vault_entry_not_configured");
        const opened = await this.#entryVault(originId).open<unknown>(row.envelope);
        const retained = validateStoredVaultEntry(originId, opened.value);
        if (!retained || retained.kind !== "login" || !sameVaultEntryMetadata(metadata, vaultEntryMetadata(retained))) {
          return jsonError(503, "vault_entry_invalid");
        }
        const entry = { ...retained, browser_origin: body.browser_origin };
        const next = { ...this.#credentials, vault: { ...this.#credentials.vault, [originId]: vaultEntryMetadata(entry) } };
        const [stateEnvelope, entryEnvelope] = await Promise.all([
          this.#vault.seal(next), this.#entryVault(originId).seal(entry),
        ]);
        await this.#state.storage.transaction(async transaction => {
          await transaction.put(STATE_KEY, { envelope: stateEnvelope } satisfies StoredRow);
          await transaction.put(vaultEntryStorageKey(originId), { envelope: entryEnvelope } satisfies StoredRow);
        });
        this.#credentials = next;
        return json(publicVaultEntry(entry), 200);
      }
      const vaultMaterialize = url.pathname.match(
        /^\/v1\/vault-entry\/([A-Za-z0-9_-]{22,64})$/,
      )?.[1];
      if (vaultMaterialize) {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        const metadata = this.#credentials.vault?.[vaultMaterialize];
        if (!metadata) return jsonError(404, "vault_entry_not_configured");
        const row = await this.#state.storage.get<StoredRow>(
          vaultEntryStorageKey(vaultMaterialize),
        );
        if (!row) return jsonError(404, "vault_entry_not_configured");
        const opened = await this.#entryVault(vaultMaterialize).open<unknown>(row.envelope);
        const entry = validateStoredVaultEntry(vaultMaterialize, opened.value);
        if (!entry || !sameVaultEntryMetadata(metadata, vaultEntryMetadata(entry))) {
          return jsonError(503, "vault_entry_invalid");
        }
        if (opened.reseal) {
          await this.#state.storage.put(vaultEntryStorageKey(vaultMaterialize), {
            envelope: await this.#entryVault(vaultMaterialize).seal(entry),
          } satisfies StoredRow);
        }
        return json(entry, 200);
      }
      const sshIdentity = url.pathname.match(/^\/v1\/ssh-identities\/([A-Za-z0-9][A-Za-z0-9._-]{0,63})$/)?.[1];
      if (sshIdentity && validSshIdentityReference(sshIdentity)) {
        if (request.method === "PUT") {
          const body = await readJson(request, 72 * 1024);
          let identity: BrokeredSshIdentity | undefined;
          if (body?.generate === true) {
            const target = validateSshTarget(body);
            if (!target || body.private_key !== undefined) return jsonError(400, "invalid_ssh_identity");
            // Generating must never silently rotate an already installed key.
            if (this.#credentials.ssh?.[sshIdentity]) return jsonError(409, "ssh_identity_already_exists");
            identity = { ...target, ...await createSshKeyPair() };
          } else {
            const parsed = validateSshIdentity(body);
            if (!parsed) return jsonError(400, "invalid_ssh_identity");
            const retained = this.#credentials.ssh?.[sshIdentity];
            if (retained) {
              // HTTP dispatch holds #exclusive across validation and persistence.
              // Reconciliation of the exact import is a no-op; a reference can
              // never rotate to a different key or target through PUT.
              if (retained.privateKey === parsed.privateKey
                && retained.hostname === parsed.hostname && retained.port === parsed.port
                && retained.username === parsed.username && retained.hostKeySha256 === parsed.hostKeySha256) {
                return new Response(null, { status: 204, headers: noStoreHeaders() });
              }
              return jsonError(409, "ssh_identity_already_exists");
            }
            try { identity = { ...parsed, publicKey: await sshPublicKey(parsed.privateKey) }; }
            catch { return jsonError(400, "invalid_ssh_identity"); }
          }
          this.#credentials.ssh = { ...this.#credentials.ssh, [sshIdentity]: identity };
          await this.#persist();
          return new Response(null, { status: 204, headers: noStoreHeaders() });
        }
        if (request.method === "DELETE") {
          if (this.#credentials.ssh?.[sshIdentity] !== undefined) {
            const identities = { ...this.#credentials.ssh };
            delete identities[sshIdentity];
            if (Object.keys(identities).length) this.#credentials.ssh = identities;
            else delete this.#credentials.ssh;
            await this.#persist();
          }
          return new Response(null, { status: 204, headers: noStoreHeaders() });
        }
        if (request.method === "POST") {
          if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
          const identity = this.#credentials.ssh?.[sshIdentity];
          return identity
            ? json({
                private_key: identity.privateKey,
                hostname: identity.hostname,
                port: identity.port,
                username: identity.username,
                host_key_sha256: identity.hostKeySha256,
              }, 200)
            : jsonError(404, "ssh_identity_not_configured");
        }
        return jsonError(405, "method_not_allowed");
      }
      if (request.method === "PUT" && url.pathname === "/v1/openai-key") {
        const body = await readJson(request, 16 * 1024);
        const secret = stringField(body, "api_key")?.trim();
        if (!secret || secret.length > 8_192 || /[\u0000-\u001f\u007f]/.test(secret)) {
          return jsonError(400, "invalid_openai_api_key");
        }
        this.#credentials.openai = {
          secret,
          createdAt: Date.now(),
          revision: (this.#credentials.openai?.revision ?? -1) + 1,
        };
        this.#credentials.active = "openai";
        await this.#persist();
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "PUT" && url.pathname === "/v1/chief-of-staff/openai-key") {
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        const secret = this.#env.CHIEF_OF_STAFF_OPENAI_API_KEY?.trim();
        if (!secret || secret.length > 8_192 || /[\u0000-\u001f\u007f]/.test(secret)) {
          return jsonError(503, "chief_of_staff_credential_unavailable");
        }
        if (this.#credentials.active === "openai"
          && this.#credentials.openai?.secret === secret) {
          return new Response(null, { status: 204, headers: noStoreHeaders() });
        }
        this.#credentials.openai = {
          secret,
          createdAt: Date.now(),
          revision: (this.#credentials.openai?.revision ?? -1) + 1,
        };
        this.#credentials.active = "openai";
        await this.#persist();
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "DELETE" && url.pathname === "/v1/openai-key") {
        delete this.#credentials.openai;
        if (this.#credentials.active === "openai") {
          this.#credentials.active = this.#credentials.chatgpt ? "chatgpt" : null;
        }
        await this.#persist();
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "POST" && url.pathname === "/v1/chatgpt/login/start") {
        const login = await this.#startLogin();
        this.#credentials.login = login;
        await this.#persist();
        await this.#schedule();
        return json(publicLogin(login), 200);
      }
      if (request.method === "POST" && url.pathname === "/v1/chatgpt/login/status") {
        return json(await this.#loginStatus(), 200);
      }
      if (request.method === "DELETE" && url.pathname === "/v1/chatgpt") {
        delete this.#credentials.chatgpt;
        delete this.#credentials.chatgptBackups;
        delete this.#credentials.login;
        if (this.#credentials.active === "chatgpt") {
          this.#credentials.active = this.#credentials.openai ? "openai" : null;
        }
        await this.#persist();
        await this.#schedule();
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "POST" && url.pathname === "/v1/chatgpt/local-claim") {
        if (!localCredentialClaimEnabled(this.#env)) throw new BrokerFailure(404, "not_found");
        if (await hasRequestPayload(request)) return jsonError(400, "invalid_request");
        const provenance = request.headers.get("x-nanocodex-credential-provenance");
        if (provenance !== "user" && provenance !== "sponsor") {
          return jsonError(400, "invalid_request");
        }
        await this.#claimLocalBootstrap(provenance);
        return json(await this.#publicStatus(), 200);
      }
      if (request.method === "PUT" && url.pathname === "/v1/chatgpt") {
        const body = await readJson(request, 64 * 1024);
        if (!validChatGptCredentialImport(body)) {
          return jsonError(400, "invalid_chatgpt_credential");
        }
        await this.#importChatGpt(body);
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "POST" && url.pathname === "/v1/chatgpt/limit") {
        const body = await readJson(request, 1_024);
        const accountId = stringField(body, "account_id");
        const revision = numberField(body, "revision");
        const resetAt = numberField(body, "reset_at");
        if (!accountId || !Number.isSafeInteger(revision)
          || !resetAt || !Number.isSafeInteger(resetAt) || resetAt <= Date.now()) {
          return jsonError(400, "invalid_chatgpt_limit");
        }
        const limited = this.#chatGptAccounts().find((item) => item.accountId === accountId);
        // Old sockets remain valid across refresh, but not across reauthorization.
        if (limited && revision! >= (limited.authorizationRevision ?? 0)
          && revision! <= limited.revision) {
          this.#setChatGpt({ ...limited, limitedUntil: Math.max(limited.limitedUntil ?? 0, resetAt) });
          await this.#persist();
        }
        if (this.#credentials.active !== "chatgpt") return json({ available: false }, 200);
        if (body?.select === false) return json({ available: false }, 200);
        const available = await this.#selectChatGpt();
        return json({ available: Boolean(available && available.accountId !== accountId) }, 200);
      }
      if (request.method === "POST" && url.pathname === "/v1/credential") {
        const body = await readJson(request, 1_024);
        const recover = body?.recover === true;
        const revision = numberField(body, "revision");
        return json(await this.#credential(recover, revision, stringField(body, "account_id")), 200);
      }
      return jsonError(404, "not_found");
    } catch (error) {
      const problem = await this.#recoverFailedOperation(error);
      return jsonError(problem.status, problem.code);
    }
  }

  async #recoverFailedOperation(error: unknown): Promise<BrokerFailure> {
    const problem = failure(error);
    // HTTP and RPC both restore durable state after a failed seal or write.
    // Uncommitted credentials must never remain usable in this isolate.
    await this.#restoreDurableState();
    return problem;
  }

  async #publicStatus(): Promise<Record<string, unknown>> {
    const login = this.#credentials.login;
    const claude = await this.#claudePublicStatus();
    return {
      ready: this.#credentials.active !== null || claude.connected,
      claude,
      active: this.#credentials.active,
      openai: { connected: Boolean(this.#credentials.openai) },
      chatgpt: {
        connected: this.#chatGptAccounts().some((account) => !account.deadReason),
        accounts: this.#chatGptAccounts().map((account) => ({
          account_id: account.accountId,
          connected: !account.deadReason,
          active: this.#credentials.active === "chatgpt"
            && this.#credentials.chatgpt?.accountId === account.accountId,
          ...(account.limitedUntil && account.limitedUntil > Date.now()
            ? { limited_until: account.limitedUntil } : {}),
        })),
        ...(this.#credentials.chatgpt?.accountId
          ? { account_id: this.#credentials.chatgpt.accountId }
          : {}),
        ...(login ? { login: publicLogin(login) } : {}),
      },
      ssh: await Promise.all(Object.entries(this.#credentials.ssh ?? {}).map(async ([reference, identity]) => ({
        reference,
        hostname: identity.hostname,
        port: identity.port,
        username: identity.username,
        host_key_sha256: identity.hostKeySha256,
        // Older stored identities predate public-key metadata. Derive it in
        // the broker; a malformed legacy key must not break the entire vault.
        public_key: identity.publicKey ?? await sshPublicKey(identity.privateKey).catch(() => undefined),
      }))),
      vault: this.#publicVault(),
    };
  }

  #publicVault() {
    return Object.values(this.#credentials.vault ?? {})
      .map(publicVaultEntry)
      .sort((left, right) => right.created_at - left.created_at || compareText(left.id, right.id));
  }

  async #sponsoredPromptStatus(): Promise<Readonly<{
    limit: number;
    used: number;
    remaining: number;
  }>> {
    const stored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const prompts = sponsoredPrompts(stored);
    return sponsoredPromptStatus(prompts);
  }

  async #reserveSponsoredPrompt(promptId: string, promptHash: string): Promise<Response> {
    const stored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const prompts = sponsoredPrompts(stored);
    const status = sponsoredPromptStatus(prompts);
    const existing = prompts.find(({ id }) => id === promptId);
    if (existing?.hash === promptHash) {
      const now = Date.now();
      if ((existing.phase === "retryable"
          || (existing.phase === "in_flight" && existing.leaseExpiresAt! <= now))
        && existing.attempts < SPONSORED_PROMPT_MAX_ATTEMPTS) {
        const next = prompts.map((prompt) => prompt.id === promptId
          ? {
            id: prompt.id,
            hash: prompt.hash,
            phase: "in_flight" as const,
            attempts: prompt.attempts + 1,
            leaseExpiresAt: now + sponsoredPromptLeaseMs(this.#env),
          }
          : prompt);
        await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
          prompts: next,
        } satisfies SponsoredPromptState);
        return json({
          ...status,
          reserved: false,
          dispatch: true,
          attempt: existing.attempts + 1,
        }, 200);
      }
      return json({
        ...status,
        reserved: false,
        dispatch: false,
        pending: existing.phase === "in_flight",
        attempt: existing.attempts,
        ...(existing.phase === "in_flight"
          ? { retry_after_ms: Math.max(0, existing.leaseExpiresAt! - now) }
          : {}),
      }, 200);
    }
    if (existing) return jsonError(409, "sponsored_prompt_conflict");
    if (prompts.length >= SPONSORED_PROMPT_LIMIT) {
      return json({
        error: "sponsored_prompt_limit_reached",
        ...status,
      }, 402);
    }
    const next = [...prompts, {
      id: promptId,
      hash: promptHash,
      phase: "in_flight" as const,
      attempts: 1,
      leaseExpiresAt: Date.now() + sponsoredPromptLeaseMs(this.#env),
    }];
    await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
      prompts: next,
    } satisfies SponsoredPromptState);
    return json({
      limit: SPONSORED_PROMPT_LIMIT,
      used: next.length,
      remaining: SPONSORED_PROMPT_LIMIT - next.length,
      reserved: true,
      dispatch: true,
      attempt: 1,
    }, 200);
  }

  async #grantSponsoredContinuation(
    promptId: string,
    attempt: number,
    responseId: string,
    callIds: readonly string[],
  ): Promise<Response> {
    const stored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const prompts = sponsoredPrompts(stored);
    const index = prompts.findIndex(({ id }) => id === promptId);
    if (index < 0 || prompts[index]!.attempts !== attempt) {
      return jsonError(409, "sponsored_prompt_not_admitted");
    }
    const current = prompts[index]!;
    if (current.phase !== "in_flight" || current.leaseExpiresAt! <= Date.now()) {
      return jsonError(409, "sponsored_prompt_attempt_stale");
    }
    const next = [...prompts];
    const { leaseExpiresAt: _leaseExpiresAt, ...retained } = current;
    next[index] = {
      ...retained,
      phase: "continuation",
      continuation: { responseId, callIds },
    };
    await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
      prompts: next,
    } satisfies SponsoredPromptState);
    return json({ granted: true }, 200);
  }

  async #consumeSponsoredContinuation(
    responseId: string | undefined,
    callIds: readonly string[],
    promptId?: string,
    promptHash?: string,
  ): Promise<Response> {
    const stored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const prompts = sponsoredPrompts(stored);
    const candidates = prompts.map((prompt, index) => ({ prompt, index }))
      .filter(({ prompt }) => prompt.phase === "continuation"
        && prompt.continuation
        && (!responseId || prompt.continuation.responseId === responseId)
        && (!promptId || prompt.id === promptId)
        && (!promptHash || prompt.hash === promptHash)
        && isStringSubset(prompt.continuation.callIds, callIds));
    const index = candidates.length === 1 ? candidates[0]!.index : -1;
    if (index < 0) return jsonError(409, "sponsored_continuation_unavailable");
    const current = prompts[index]!;
    const next = [...prompts];
    const { continuation: _continuation, ...retained } = current;
    next[index] = {
      ...retained,
      phase: "in_flight",
      leaseExpiresAt: Date.now() + sponsoredPromptLeaseMs(this.#env),
    };
    await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
      prompts: next,
    } satisfies SponsoredPromptState);
    return json({ prompt_id: current.id, attempt: current.attempts }, 200);
  }

  async #setSponsoredPromptLifecycle(
    promptId: string,
    attempt: number,
    action: "terminal" | "interrupted" | "heartbeat",
  ): Promise<Response> {
    const stored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const prompts = sponsoredPrompts(stored);
    const index = prompts.findIndex(({ id }) => id === promptId);
    if (index < 0) return jsonError(409, "sponsored_prompt_not_admitted");
    const current = prompts[index]!;
    if (current.attempts !== attempt) return json({ updated: false }, 200);
    if (action === "heartbeat") {
      if (current.phase === "continuation" || current.phase === "terminal") {
        return json({ updated: false, settled: true }, 200);
      }
      if (current.phase !== "in_flight" || current.leaseExpiresAt! <= Date.now()) {
        return json({ updated: false }, 200);
      }
      const next = [...prompts];
      next[index] = {
        ...current,
        leaseExpiresAt: Date.now() + sponsoredPromptLeaseMs(this.#env),
      };
      await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
        prompts: next,
      } satisfies SponsoredPromptState);
      return json({ updated: true }, 200);
    }
    if (current.phase !== "in_flight"
      || (action === "terminal" && current.leaseExpiresAt! <= Date.now())) {
      return json({ updated: false }, 200);
    }
    const {
      continuation: _continuation,
      leaseExpiresAt: _leaseExpiresAt,
      ...retained
    } = current;
    const next = [...prompts];
    next[index] = {
      ...retained,
      phase: action === "terminal" ? "terminal" : "retryable",
    };
    await this.#state.storage.put(SPONSORED_PROMPT_STATE_KEY, {
      prompts: next,
    } satisfies SponsoredPromptState);
    return json({ updated: true }, 200);
  }

  async #setSponsoredConnection(
    connectionId: string,
    action: "acquire" | "heartbeat" | "release",
  ): Promise<Response> {
    const now = Date.now();
    const stored = await this.#state.storage.get<unknown>(SPONSORED_CONNECTION_STATE_KEY);
    const current = sponsoredConnections(stored).filter(({ leaseExpiresAt }) => (
      leaseExpiresAt > now
    ));
    const index = current.findIndex(({ id }) => id === connectionId);
    if (action === "release") {
      if (index < 0) return json({ updated: false }, 200);
      const next = current.filter(({ id }) => id !== connectionId);
      await this.#persistSponsoredConnections(next);
      return json({ updated: true }, 200);
    }
    if (action === "heartbeat") {
      if (index < 0) return json({ updated: false }, 200);
      const next = [...current];
      next[index] = {
        id: connectionId,
        leaseExpiresAt: now + sponsoredPromptLeaseMs(this.#env),
      };
      await this.#persistSponsoredConnections(next);
      return json({ updated: true }, 200);
    }
    const promptStored = await this.#state.storage.get<unknown>(SPONSORED_PROMPT_STATE_KEY);
    const status = sponsoredPromptStatus(sponsoredPrompts(promptStored));
    if (status.remaining === 0) {
      await this.#persistSponsoredConnections(current);
      return json({ error: "sponsored_prompt_limit_reached", ...status }, 402);
    }
    if (index >= 0) return jsonError(409, "sponsored_connection_conflict");
    if (current.length >= SPONSORED_CONNECTION_LIMIT) {
      return jsonError(429, "sponsored_connection_limit_reached");
    }
    const next = [...current, {
      id: connectionId,
      leaseExpiresAt: now + sponsoredPromptLeaseMs(this.#env),
    }];
    await this.#persistSponsoredConnections(next);
    return json({ acquired: true }, 200);
  }

  async #persistSponsoredConnections(connections: readonly SponsoredConnection[]): Promise<void> {
    if (connections.length === 0) {
      await this.#state.storage.delete(SPONSORED_CONNECTION_STATE_KEY);
      return;
    }
    await this.#state.storage.put(SPONSORED_CONNECTION_STATE_KEY, {
      connections,
    } satisfies SponsoredConnectionState);
  }

  #publicWallet() {
    const root = this.#credentials.wallet;
    if (!root) return undefined;
    if (this.#credentials.linkedWallet) return publicLinkedWallet(this.#credentials.linkedWallet, root.address);
    return { ...publicRootWallet(root), mode: "internal" as const, original_address: root.address };
  }

  async #spendingWalletProvider() {
    if (this.#credentials.linkedWallet) return linkedWalletProvider(this.#credentials.linkedWallet);
    if (!this.#credentials.wallet) throw new BrokerFailure(404, "wallet_not_configured");
    return rootWalletProvider(this.#credentials.wallet);
  }

  async #startWalletLink(request: Request): Promise<Response> {
    let started: WalletLink | undefined;
    const initial = await this.#exclusive(async () => {
      await this.#ready;
      try {
        if (request.method !== "POST") return jsonError(405, "method_not_allowed");
        if (!isJsonContentType(request.headers.get("content-type"))) return jsonError(415, "invalid_content_type");
        const body = await readJson(request, 1_024);
        if (!isRecord(body) || !hasExactKeys(body, ["operation_id"]) || typeof body.operation_id !== "string"
          || !WALLET_OPERATION.test(body.operation_id)) return jsonError(400, "invalid_wallet_link_request");
        const id = body.operation_id;
        const old = this.#credentials.walletLinks?.[id];
        if (old) return json(publicWalletLink(old), 200);
        if (this.#credentials.walletUnlinks?.[id]) return jsonError(409, "wallet_operation_conflict");
        if (this.#credentials.linkedWallet) return jsonError(409, "wallet_already_linked");
        if (Object.values(this.#credentials.walletLinks ?? {}).some(link => link.status === "pending")) {
          return jsonError(409, "wallet_link_pending");
        }
        if (Object.keys(this.#credentials.walletLinks ?? {}).length >= 100) return jsonError(409, "wallet_link_operation_limit");
        await this.#ensureRootWallet();
        started = newWalletLink(id);
        this.#credentials.walletLinks = { ...this.#credentials.walletLinks, [id]: started };
        await this.#persist();
        return undefined;
      } catch (error) {
        const problem = await this.#recoverFailedOperation(error);
        return jsonError(problem.status, problem.code);
      }
    }, { operation: "http" });
    if (initial) return initial;
    const link = started!;
    const controller = new AbortController();
    this.#liveWalletLink = { operationId: link.operationId, controller };
    let surfaced!: () => void;
    const prompt = new Promise<void>(resolve => { surfaced = resolve; });
    const work = pairWallet(link, controller.signal, async value => {
      await this.#exclusive(async () => {
        const current = this.#credentials.walletLinks?.[link.operationId];
        if (current?.status !== "pending" || controller.signal.aborted) throw new Error("wallet_link_cancelled");
        Object.assign(current, value);
        await this.#persist();
      }, { operation: "http" });
      surfaced();
    }).then(wallet => this.#exclusive(async () => {
      const current = this.#credentials.walletLinks?.[link.operationId];
      if (current?.status !== "pending" || controller.signal.aborted) return;
      if (current.expiresAt <= Date.now()) current.status = "expired";
      else {
        verifiedAuthorization(wallet);
        this.#credentials.linkedWallet = wallet;
        current.status = "linked";
      }
      delete current.privateKey;
      try { await this.#persist(); }
      catch (error) { await this.#recoverFailedOperation(error); throw error; }
    }, { operation: "http" })).catch(() => this.#exclusive(async () => {
      const current = this.#credentials.walletLinks?.[link.operationId];
      if (current?.status !== "pending") return;
      current.status = current.expiresAt <= Date.now() ? "expired" : "rejected";
      delete current.privateKey;
      try { await this.#persist(); }
      catch (error) { await this.#recoverFailedOperation(error); }
    }, { operation: "http" })).finally(() => {
      if (this.#liveWalletLink?.operationId === link.operationId) this.#liveWalletLink = undefined;
      surfaced();
    });
    this.#state.waitUntil(work);
    await prompt;
    return this.#exclusive(async () => json(publicWalletLink(this.#credentials.walletLinks![link.operationId]!), 200), { operation: "http" });
  }

  async #walletLinkAction(request: Request, path: string): Promise<Response> {
    if (request.method !== "POST") return jsonError(405, "method_not_allowed");
    if (!isJsonContentType(request.headers.get("content-type"))) return jsonError(415, "invalid_content_type");
    const body = await readJson(request, 1_024);
    const unlink = path === "/v1/wallet/unlink";
    if (!isRecord(body) || !hasExactKeys(body, unlink ? ["operation_id", "expected_address"] : ["operation_id"])
      || typeof body.operation_id !== "string" || !WALLET_OPERATION.test(body.operation_id)) return jsonError(400, "invalid_wallet_link_request");
    const id = body.operation_id;
    if (unlink) {
      if (typeof body.expected_address !== "string" || !ROOT_WALLET_ADDRESS.test(body.expected_address)) return jsonError(400, "invalid_wallet_link_request");
      const address = body.expected_address.toLowerCase();
      const old = this.#credentials.walletUnlinks?.[id];
      if (old && old.address !== address) return jsonError(409, "wallet_operation_conflict");
      if (this.#credentials.walletLinks?.[id]) return jsonError(409, "wallet_operation_conflict");
      if (!old) {
        if (this.#credentials.linkedWallet?.address.toLowerCase() !== address) return jsonError(409, "wallet_address_mismatch");
        if (Object.keys(this.#credentials.walletUnlinks ?? {}).length >= 100) return jsonError(409, "wallet_link_operation_limit");
        this.#credentials.walletUnlinks = { ...this.#credentials.walletUnlinks, [id]: { address } };
        delete this.#credentials.linkedWallet;
        await this.#persist();
      }
      return json({ operation_id: id, status: "unlinked", address, onchain_revoked: false, wallet: this.#publicWallet() }, 200);
    }
    let current = this.#credentials.walletLinks?.[id];
    if (!current && path.endsWith("/cancel")) {
      if (this.#credentials.walletUnlinks?.[id]) return jsonError(409, "wallet_operation_conflict");
      if (Object.keys(this.#credentials.walletLinks ?? {}).length >= 100) return jsonError(409, "wallet_link_operation_limit");
      current = { operationId: id, status: "cancelled", expiresAt: Date.now() };
      this.#credentials.walletLinks = { ...this.#credentials.walletLinks, [id]: current };
      await this.#persist();
    }
    if (!current) return jsonError(404, "wallet_link_not_found");
    if (current.status === "pending" && (path.endsWith("/cancel") || current.expiresAt <= Date.now())) {
      current.status = path.endsWith("/cancel") ? "cancelled" : "expired";
      delete current.privateKey;
      await this.#persist();
      if (this.#liveWalletLink?.operationId === id) this.#liveWalletLink.controller.abort();
    }
    return json(publicWalletLink(current), 200);
  }

  async #ensureRootWallet(): Promise<RootWallet> {
    const current = this.#credentials.wallet;
    if (current) return current;
    const wallet = await createRootWallet();
    this.#credentials = { ...this.#credentials, wallet };
    await this.#persist();
    return wallet;
  }

  #chatGptAccounts(): ChatGptCredential[] {
    return [
      ...(this.#credentials.chatgpt ? [this.#credentials.chatgpt] : []),
      ...(this.#credentials.chatgptBackups ?? []),
    ];
  }

  #nextChatGptRevision(): number {
    const revision = Math.max(this.#credentials.chatgptRevision ?? -1,
      ...this.#chatGptAccounts().map((account) => account.revision)) + 1;
    this.#credentials.chatgptRevision = revision;
    return revision;
  }

  #setChatGpt(credential: ChatGptCredential, activate = false): void {
    if (activate || !this.#credentials.chatgpt
      || this.#credentials.chatgpt.accountId === credential.accountId) {
      const backups = this.#chatGptAccounts().filter((account) => account.accountId !== credential.accountId);
      this.#credentials.chatgpt = credential;
      if (backups.length) this.#credentials.chatgptBackups = backups;
      else delete this.#credentials.chatgptBackups;
    } else {
      this.#credentials.chatgptBackups = (this.#credentials.chatgptBackups ?? [])
        .map((account) => account.accountId === credential.accountId ? credential : account);
    }
  }

  #checkChatGptCapacity(accountId: string): void {
    const accounts = this.#chatGptAccounts();
    if (accounts.length >= MAX_CHATGPT_ACCOUNTS
      && !accounts.some((account) => account.accountId === accountId)) {
      throw new BrokerFailure(409, "chatgpt_account_limit");
    }
  }

  async #selectChatGpt(): Promise<ChatGptCredential | undefined> {
    const now = Date.now();
    const selected = this.#chatGptAccounts().find((account) => !account.deadReason
      && (account.limitedUntil ?? 0) <= now
      && (account.expiresAt > now || (account.refreshToken && (account.refreshAfter ?? 0) <= now)));
    if (selected && selected.accountId !== this.#credentials.chatgpt?.accountId) {
      this.#setChatGpt(selected, true);
      await this.#persist();
    }
    return selected;
  }

  async #credential(
    recover: boolean,
    revision: number | undefined,
    accountId?: string,
    resolveId?: string,
  ): Promise<ModelCredentialValue> {
    if (accountId !== undefined && (typeof accountId !== "string" || !/^[\x21-\x7e]{1,256}$/.test(accountId))) {
      throw new BrokerFailure(400, "invalid_chatgpt_account");
    }
    if (!accountId && this.#credentials.active === "openai" && this.#credentials.openai) {
      return {
        kind: "openai",
        secret: this.#credentials.openai.secret,
        revision: this.#credentials.openai.revision,
      };
    }
    const current = accountId
      ? this.#chatGptAccounts().find((account) => account.accountId === accountId)
      : await this.#selectChatGpt() ?? this.#credentials.chatgpt;
    if ((!accountId && this.#credentials.active !== "chatgpt") || !current) {
      throw new BrokerFailure(404, "credential_not_configured");
    }
    if (current.deadReason) throw new BrokerFailure(422, "chatgpt_credential_dead");
    const now = Date.now();
    if ((current.limitedUntil ?? 0) > now) {
      throw new BrokerFailure(429, "chatgpt_accounts_exhausted");
    }
    const refreshNeeded = recover
      ? revision === current.revision
      : current.expiresAt <= now + REFRESH_EARLY_MS;
    if (refreshNeeded && (current.refreshAfter ?? 0) > now) {
      if (recover || current.expiresAt <= now) {
        throw new BrokerFailure(503, "chatgpt_refresh_rate_limited");
      }
      return {
        kind: "chatgpt",
        secret: current.accessToken,
        accountId: current.accountId,
        fedramp: current.fedramp,
        expiresAt: current.expiresAt,
        revision: current.revision,
        ...(current.provenance ? { provenance: current.provenance } : {}),
      };
    }
    let credential = current;
    if (refreshNeeded) {
      try {
        credential = await this.#refreshChatGpt(
          current, recover ? "recovery" : current.expiresAt <= now ? "expired" : "expiring", resolveId,
        );
      } catch (error) {
        if (recover || !(error instanceof BrokerFailure)
          || error.code !== "chatgpt_refresh_rate_limited"
          || current.expiresAt <= Date.now()) {
          throw error;
        }
      }
    }
    if (credential.expiresAt <= Date.now()) {
      throw new BrokerFailure(503, "chatgpt_credential_expired");
    }
    return {
      kind: "chatgpt",
      secret: credential.accessToken,
      accountId: credential.accountId,
      fedramp: credential.fedramp,
      expiresAt: credential.expiresAt,
      revision: credential.revision,
      ...(credential.provenance ? { provenance: credential.provenance } : {}),
    };
  }

  async #startLogin(): Promise<PendingLogin> {
    const environment = this.#env.ENVIRONMENT?.trim().toLowerCase();
    if (this.#env.ALLOW_LOCAL_CREDENTIAL_CLAIM === "true"
      && (environment === "development" || environment === "local")) {
      throw new BrokerFailure(409, "local_credential_claim_required");
    }
    const issuer = issuerUrl(this.#env);
    const response = await providerFetch(new URL("api/accounts/deviceauth/usercode", issuer), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ client_id: CODEX_CLIENT_ID }),
    });
    if (!response.ok) {
      await cancelResponseBody(response);
      throw new BrokerFailure(503, "chatgpt_login_start_failed");
    }
    const value = await providerJson(response);
    const deviceAuthId = stringField(value, "device_auth_id");
    const userCode = stringField(value, "user_code") ?? stringField(value, "usercode");
    const interval = positiveNumber(value.interval) ?? positiveNumberString(value.interval) ?? 5;
    if (!deviceAuthId || !userCode) throw new BrokerFailure(503, "invalid_chatgpt_login_response");
    return {
      deviceAuthId,
      userCode,
      verificationUrl: new URL("codex/device", issuer).href,
      expiresAt: Date.now() + LOGIN_TTL_MS,
      pollAfterMs: Math.max(1_000, Math.min(60_000, interval * 1_000)),
      nextPollAt: Date.now(),
    };
  }

  async #loginStatus(): Promise<Record<string, unknown>> {
    const login = this.#credentials.login;
    if (!login) {
      if (this.#credentials.chatgpt && !this.#credentials.chatgpt.deadReason) {
        return { state: "authenticated", account_id: this.#credentials.chatgpt.accountId };
      }
      return { state: "not_started" };
    }
    if (login.expiresAt <= Date.now()) {
      delete this.#credentials.login;
      await this.#persist();
      return { state: "expired" };
    }
    if (login.nextPollAt > Date.now()) return publicLogin(login);

    const issuer = issuerUrl(this.#env);
    const response = await providerFetch(new URL("api/accounts/deviceauth/token", issuer), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ device_auth_id: login.deviceAuthId, user_code: login.userCode }),
    });
    if (response.status === 403 || response.status === 404) {
      await cancelResponseBody(response);
      login.nextPollAt = Date.now() + login.pollAfterMs;
      await this.#persist();
      return publicLogin(login);
    }
    if (!response.ok) {
      await cancelResponseBody(response);
      throw new BrokerFailure(503, "chatgpt_login_poll_failed");
    }
    const code = await providerJson(response);
    const authorizationCode = stringField(code, "authorization_code");
    const codeVerifier = stringField(code, "code_verifier");
    if (!authorizationCode || !codeVerifier || !stringField(code, "code_challenge")) {
      throw new BrokerFailure(503, "invalid_chatgpt_login_response");
    }
    const tokens = await exchangeAuthorizationCode(issuer, authorizationCode, codeVerifier);
    const credential = credentialFromTokens(tokens, undefined, this.#nextChatGptRevision(), "user");
    this.#checkChatGptCapacity(credential.accountId);
    this.#setChatGpt(credential, true);
    this.#credentials.active = "chatgpt";
    delete this.#credentials.login;
    await this.#persist();
    await this.#schedule();
    return { state: "authenticated", account_id: credential.accountId };
  }

  async #claimLocalBootstrap(provenance: "user" | "sponsor"): Promise<void> {
    if (!localCredentialClaimEnabled(this.#env)) {
      throw new BrokerFailure(404, "not_found");
    }
    const raw = this.#env.LOCAL_CHATGPT_BOOTSTRAP?.trim();
    if (!raw) throw new BrokerFailure(503, "local_chatgpt_bootstrap_unavailable");
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { throw new BrokerFailure(503, "invalid_local_chatgpt_bootstrap"); }
    if (!isRecord(parsed)) throw new BrokerFailure(503, "invalid_local_chatgpt_bootstrap");
    const accessToken = stringField(parsed, "access_token");
    const accountId = stringField(parsed, "account_id") ?? idTokenClaims(stringField(parsed, "id_token")).accountId;
    const expiresAt = parseExpiry(parsed.expires_at) ?? jwtExpiration(accessToken);
    if (!accessToken || !accountId || !expiresAt || expiresAt <= Date.now()) {
      throw new BrokerFailure(503, "invalid_local_chatgpt_bootstrap");
    }
    const current = this.#credentials.chatgpt;
    if (current && !current.deadReason) {
      if (current.accessToken !== accessToken || current.accountId !== accountId) return;
      if (current.provenance === provenance) return;
      this.#credentials.chatgpt = { ...current, provenance };
      await this.#persist();
      return;
    }
    const revision = this.#nextChatGptRevision();
    this.#setChatGpt({
      accessToken,
      refreshToken: stringField(parsed, "refresh_token") ?? "",
      accountId,
      fedramp: parsed.fedramp === true,
      expiresAt,
      revision,
      authorizationRevision: revision,
      provenance,
      refreshState: "ready",
      deadReason: null,
    }, true);
    this.#credentials.active = "chatgpt";
    delete this.#credentials.login;
    await this.#persist();
    await this.#schedule();
  }

  async #importChatGpt(imported: ChatGptCredentialImport): Promise<void> {
    this.#checkChatGptCapacity(imported.account_id);
    const current = this.#chatGptAccounts().find((account) => account.accountId === imported.account_id);
    // Never replace a live rotating refresh token with a replayed auth file.
    if (current && !current.deadReason && current.expiresAt > Date.now()) {
      this.#setChatGpt({ ...current, provenance: "user" }, true);
    } else {
      const revision = this.#nextChatGptRevision();
      this.#setChatGpt({
        accessToken: imported.access_token,
        refreshToken: imported.refresh_token,
        accountId: imported.account_id,
        fedramp: imported.fedramp,
        expiresAt: imported.expires_at,
        revision,
        authorizationRevision: revision,
        provenance: "user",
        refreshState: "ready",
        deadReason: null,
      }, true);
    }
    this.#credentials.active = "chatgpt";
    delete this.#credentials.login;
    await this.#persistAndSchedule();
  }

  async #refreshChatGpt(
    current: ChatGptCredential,
    cause: "alarm" | "recovery" | "expired" | "expiring",
    resolveId?: string,
  ): Promise<ChatGptCredential> {
    const startedAt = Date.now();
    let completed = false;
    try {
      const next = await this.#rotateChatGpt(current);
      completed = true;
      return next;
    } finally {
      credentialMetric({
        type: "egress.credential.refresh",
        ...(resolveId ? { resolve_id: resolveId } : {}),
        cause,
        outcome: completed ? "ok" : "error",
        refresh_ms: Date.now() - startedAt,
      });
    }
  }

  async #rotateChatGpt(current: ChatGptCredential): Promise<ChatGptCredential> {
    if (!current.refreshToken) throw new BrokerFailure(503, "chatgpt_refresh_unavailable");
    const claimed = { ...current, refreshState: "in_flight" as const };
    this.#setChatGpt(claimed);
    await this.#persist();
    let response: Response;
    try {
      const issuer = issuerUrl(this.#env);
      response = await providerFetch(new URL(TOKEN_ENDPOINT_PATH.slice(1), issuer), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          client_id: CODEX_CLIENT_ID,
          grant_type: "refresh_token",
          refresh_token: current.refreshToken,
        }),
      });
    } catch {
      await this.#markDead(claimed, "refresh_outcome_unknown");
      throw new BrokerFailure(422, "chatgpt_credential_dead");
    }
    if (!response.ok) {
      if (response.status === 429) {
        const now = Date.now();
        const refreshAttempts = nextRefreshAttempt(current.refreshAttempts);
        const refreshAfter = now + retryAfterDelayMs(
          response.headers.get("retry-after"),
          now,
          refreshAttempts,
        );
        this.#setChatGpt({
          ...current,
          refreshState: "ready",
          refreshAfter,
          refreshAttempts,
        });
        await finishRateLimitedRefresh(
          response,
          () => this.#persist(),
          () => this.#schedule(),
        );
        throw new BrokerFailure(503, "chatgpt_refresh_rate_limited");
      }
      await cancelResponseBody(response);
      await this.#markDead(claimed, `token_endpoint_http_${response.status}`);
      throw new BrokerFailure(422, "chatgpt_credential_dead");
    }
    try {
      const tokens = await providerJson(response);
      const next = credentialFromTokens(tokens, current, this.#nextChatGptRevision());
      if (next.accountId !== current.accountId) {
        await this.#markDead(claimed, "account_changed");
        throw new BrokerFailure(422, "chatgpt_credential_dead");
      }
      this.#setChatGpt(next);
      await this.#persist();
      await this.#schedule();
      return next;
    } catch (error) {
      if (error instanceof BrokerFailure && error.code === "chatgpt_credential_dead") throw error;
      await this.#markDead(claimed, "refresh_outcome_unknown");
      throw new BrokerFailure(422, "chatgpt_credential_dead");
    }
  }

  async #markDead(current: ChatGptCredential, reason: string): Promise<void> {
    this.#setChatGpt({ ...current, refreshState: "ready", deadReason: reason });
    if (this.#credentials.active === "chatgpt"
      && !this.#chatGptAccounts().some((account) => !account.deadReason)) {
      this.#credentials.active = this.#credentials.openai ? "openai" : null;
    }
    await this.#persist();
  }

  async #persist(): Promise<void> {
    const identity = this.#credentials.wallet ? publicRootWallet(this.#credentials.wallet) : null;
    await this.#state.storage.put(STATE_KEY, {
      envelope: await this.#vault.seal(this.#credentials),
    } satisfies StoredRow);
    this.#committedWalletIdentity = identity;
  }

  #entryVault(id: string): CredentialVault {
    return new CredentialVault(
      this.#env,
      `user/${this.#state.id.toString()}/vault/${id}`,
    );
  }

  #browserCookieJarVault(id: string): CredentialVault {
    return new CredentialVault(
      this.#env,
      `user/${this.#state.id.toString()}/browser-cookie-jar/${id}`,
    );
  }

  async #persistAndSchedule(): Promise<void> {
    const identity = this.#credentials.wallet ? publicRootWallet(this.#credentials.wallet) : null;
    const row = {
      envelope: await this.#vault.seal(this.#credentials),
    } satisfies StoredRow;
    const alarm = this.#nextAlarm();
    await this.#state.storage.transaction(async (transaction) => {
      await transaction.put(STATE_KEY, row);
      if (alarm === undefined) await transaction.deleteAlarm();
      else await transaction.setAlarm(alarm);
    });
    this.#committedWalletIdentity = identity;
  }

  async #restoreDurableState(): Promise<void> {
    try {
      const row = await this.#state.storage.get<StoredRow>(STATE_KEY);
      if (!row) {
        this.#credentials = { version: 1, active: null };
        this.#committedWalletIdentity = null;
        return;
      }
      const opened = await this.#vault.open<CredentialState>(row.envelope);
      const installed = this.#installRestoredState(opened.value);
      if (installed.legacy.length) await this.#migrateLegacyVault(installed.legacy);
    } catch {
      this.#credentials = { version: 1, active: null };
      this.#committedWalletIdentity = null;
    }
  }

  #installRestoredState(restored: CredentialState): Readonly<{
    changed: boolean;
    legacy: readonly VaultEntry[];
  }> {
    let changed = false;
    const legacy: VaultEntry[] = [];
    for (const link of Object.values(restored.walletLinks ?? {})) {
      if (link.status === "pending") {
        link.status = link.expiresAt <= Date.now() ? "expired" : "interrupted";
        delete link.privateKey;
        changed = true;
      }
    }
    if (restored.wallet !== undefined && !validStoredRootWallet(restored.wallet)) {
      delete restored.wallet;
      changed = true;
    }
    const validVault = Object.entries(isRecord(restored.vault) ? restored.vault : {})
      .flatMap(([id, value]) => {
        const metadata = validateStoredVaultMetadata(id, value);
        if (metadata) return [metadata];
        const entry = validateStoredVaultEntry(id, value);
        if (entry) {
          legacy.push(entry);
          changed = true;
          return [vaultEntryMetadata(entry)];
        }
        changed = true;
        return [];
      })
      .sort((left, right) => right.createdAt - left.createdAt || compareText(left.id, right.id));
    if (restored.vault !== undefined && !isRecord(restored.vault)) changed = true;
    if (validVault.length > MAX_VAULT_ENTRIES) changed = true;
    const retainedVault = validVault.slice(0, MAX_VAULT_ENTRIES);
    const retainedIds = new Set(retainedVault.map(({ id }) => id));
    if (retainedVault.length) {
      restored.vault = Object.fromEntries(retainedVault.map((entry) => [entry.id, entry]));
    } else {
      delete restored.vault;
    }
    const validBrowserCookieJars = Object.entries(
      isRecord(restored.browserCookieJars) ? restored.browserCookieJars : {},
    ).flatMap(([id, value]) => {
      const metadata = validateStoredBrowserCookieJarMetadata(id, value);
      if (metadata) return [metadata];
      changed = true;
      return [];
    }).sort((left, right) => right.updatedAt - left.updatedAt
      || compareText(left.id, right.id));
    if (restored.browserCookieJars !== undefined
      && !isRecord(restored.browserCookieJars)) changed = true;
    if (validBrowserCookieJars.length > MAX_BROWSER_COOKIE_JARS) changed = true;
    const retainedBrowserCookieJars = validBrowserCookieJars.slice(0, MAX_BROWSER_COOKIE_JARS);
    if (retainedBrowserCookieJars.length) {
      restored.browserCookieJars = Object.fromEntries(
        retainedBrowserCookieJars.map((entry) => [entry.id, entry]),
      );
    } else {
      delete restored.browserCookieJars;
    }
    this.#credentials = restored;
    this.#committedWalletIdentity = restored.wallet ? publicRootWallet(restored.wallet) : null;
    for (const chatgpt of this.#chatGptAccounts()) {
      if (chatgpt?.refreshState === "in_flight") {
        chatgpt.refreshState = "ready";
        chatgpt.deadReason = "refresh_outcome_unknown";
        changed = true;
      }
    }
    return {
      changed,
      legacy: legacy.filter(({ id }) => retainedIds.has(id)),
    };
  }

  async #migrateLegacyVault(entries: readonly VaultEntry[]): Promise<void> {
    const sealed = await Promise.all(entries.map(async (entry) => ({
      key: vaultEntryStorageKey(entry.id),
      row: { envelope: await this.#entryVault(entry.id).seal(entry) } satisfies StoredRow,
    })));
    const stateRow = {
      envelope: await this.#vault.seal(this.#credentials),
    } satisfies StoredRow;
    await this.#state.storage.transaction(async (transaction) => {
      await transaction.put(STATE_KEY, stateRow);
      for (const { key, row } of sealed) await transaction.put(key, row);
    });
  }

  async #schedule(): Promise<void> {
    const alarm = this.#nextAlarm();
    if (alarm !== undefined) await this.#state.storage.setAlarm(alarm);
    else await this.#state.storage.deleteAlarm();
  }

  #nextAlarm(): number | undefined {
    const times: number[] = [];
    if (this.#credentials.login) times.push(this.#credentials.login.expiresAt);
    for (const chatgpt of this.#chatGptAccounts()) {
      if (chatgpt?.refreshToken && !chatgpt.deadReason) {
        times.push(Math.max(
          Date.now() + 1_000,
          chatgpt.expiresAt - REFRESH_EARLY_MS,
          chatgpt.refreshAfter ?? 0,
        ));
      }
    }
    // A durable pending revocation keeps retrying across restarts.
    if (Object.keys(this.#leaseRegistry.pending).length) times.push(Date.now() + REVOCATION_RETRY_MS);
    return times.length ? Math.min(...times) : undefined;
  }

  async #ensureAlarmBy(at: number): Promise<void> {
    const current = await this.#state.storage.getAlarm();
    if (current === null || current > at) await this.#state.storage.setAlarm(at);
  }
}

type WalletConnectRequest = Readonly<{
  method: "wallet_connect";
  params: readonly [Readonly<Record<string, unknown>>];
}>;
type WalletRevokeRequest = Readonly<{
  method: "wallet_revokeAccessKey";
  params: readonly [Readonly<Record<string, unknown>>];
}>;

function publicRootWallet(wallet: RootWallet): Readonly<{ address: string; created_at: number }> {
  return { address: wallet.address, created_at: wallet.createdAt };
}

function validStoredRootWallet(value: unknown): value is RootWallet {
  return isRecord(value)
    && Object.keys(value).length === 3
    && ROOT_WALLET_PRIVATE_KEY.test(String(value.privateKey))
    && ROOT_WALLET_ADDRESS.test(String(value.address))
    && typeof value.createdAt === "number"
    && Number.isSafeInteger(value.createdAt)
    && value.createdAt > 0;
}

async function createRootWallet(): Promise<RootWallet> {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const privateKey = randomRootPrivateKey();
    try {
      const address = TempoAccount.fromSecp256k1(privateKey).address;
      if (ROOT_WALLET_ADDRESS.test(address)) {
        return {
          privateKey,
          address: address.toLowerCase() as `0x${string}`,
          createdAt: Date.now(),
        };
      }
    } catch { /* retry an invalid scalar without exposing it */ }
  }
  throw new BrokerFailure(503, "wallet_provisioning_failed");
}

function randomRootPrivateKey(): `0x${string}` {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return `0x${Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("")}`;
}

function rootWalletProvider(wallet: RootWallet) {
  return Provider.create({
    adapter: secp256k1({ privateKey: wallet.privateKey }),
    chains: [tempo],
    storage: Storage.memory({ key: "nanocodex-root-wallet" }),
    transports: { [tempo.id]: http(TEMPO_RPC, { retryCount: 1, timeout: 5_000 }) },
    mpp: false,
  });
}

function validateWalletConnectRequest(value: unknown, env: BrokerEnv): WalletConnectRequest | undefined {
  if (!isRecord(value) || !hasExactKeys(value, ["request"]) || !isRecord(value.request)
    || !hasExactKeys(value.request, ["method", "params"])
    || value.request.method !== "wallet_connect" || !Array.isArray(value.request.params)
    || value.request.params.length !== 1 || !isRecord(value.request.params[0])) return undefined;
  const params = value.request.params[0];
  if (!hasOnlyKeys(params, ["chainId", "capabilities"]) || params.chainId !== TEMPO_CHAIN_ID
    || !isRecord(params.capabilities)) return undefined;
  const capabilities = params.capabilities;
  if (!hasOnlyKeys(capabilities, ["auth", "authorizeAccessKey", "method", "showDeposit"])
    || capabilities.method !== "login" || !validWalletAuth(capabilities.auth, env)) return undefined;
  const request = { method: "wallet_connect", params: [params] } as const;
  try {
    ProviderRequest.parse(request, { method: "wallet_connect" });
    return request;
  } catch { return undefined; }
}

function validateWalletRevokeRequest(value: unknown): WalletRevokeRequest | undefined {
  if (!isRecord(value) || !hasExactKeys(value, ["request"]) || !isRecord(value.request)
    || !hasExactKeys(value.request, ["method", "params"])
    || value.request.method !== "wallet_revokeAccessKey" || !Array.isArray(value.request.params)
    || value.request.params.length !== 1 || !isRecord(value.request.params[0])) return undefined;
  const request = { method: "wallet_revokeAccessKey", params: [value.request.params[0]] } as const;
  try {
    ProviderRequest.parse(request, { method: "wallet_revokeAccessKey" });
    return request;
  } catch { return undefined; }
}

function validWalletAuth(value: unknown, env: BrokerEnv): boolean {
  if (!isRecord(value)
    || !hasOnlyKeys(value, ["url", "challenge", "verify", "logout", "resources", "returnToken"])
    || !Array.isArray(value.resources)
    || (value.returnToken !== undefined && value.returnToken !== true)) return false;
  const base = value.url === undefined
    ? undefined
    : typeof value.url === "string"
      ? validConnectAuthEndpoint(value.url, "/v1/connect/auth", env)
      : undefined;
  if (value.url !== undefined && !base) return false;
  const challenge = typeof value.challenge === "string"
    ? validConnectAuthEndpoint(value.challenge, "/v1/connect/auth/challenge", env)
    : base
      ? validConnectAuthEndpoint(new URL("/v1/connect/auth/challenge", base).href, "/v1/connect/auth/challenge", env)
      : undefined;
  const verify = typeof value.verify === "string"
    ? validConnectAuthEndpoint(value.verify, "/v1/connect/auth", env)
    : base;
  const logout = typeof value.logout === "string"
    ? validConnectAuthEndpoint(value.logout, "/v1/connect/auth/logout", env)
    : value.logout === undefined && base
      ? validConnectAuthEndpoint(new URL("/v1/connect/auth/logout", base).href, "/v1/connect/auth/logout", env)
      : undefined;
  if (!challenge || !verify || challenge.origin !== verify.origin
    || (value.logout !== undefined && !logout)
    || (logout && logout.origin !== verify.origin)
    || value.resources.length > MAX_WALLET_RESOURCES) return false;
  let total = 0;
  const seen = new Set<string>();
  for (const resource of value.resources) {
    if (typeof resource !== "string" || !resource || /[\u0000-\u001f\u007f]/.test(resource)) return false;
    const bytes = new TextEncoder().encode(resource).byteLength;
    if (bytes > MAX_WALLET_RESOURCE_BYTES || (total += bytes) > MAX_WALLET_RESOURCE_TOTAL_BYTES
      || seen.has(resource)) return false;
    seen.add(resource);
  }
  return true;
}

function validConnectAuthEndpoint(
  value: string,
  pathname: string,
  env: BrokerEnv,
): URL | undefined {
  let url: URL;
  try { url = new URL(value); } catch { return undefined; }
  if (url.username || url.password || url.search || url.hash || url.pathname !== pathname) return undefined;
  if (url.origin === PRODUCTION_CONNECT_API_ORIGIN && url.protocol === "https:" && !url.port) return url;
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  const local = environment !== "production" && environment !== "preview";
  return local && (url.protocol === "http:" || url.protocol === "https:")
    && (url.hostname === "nanocodex.localhost" || url.hostname.endsWith(".nanocodex.localhost"))
    ? url : undefined;
}

function hasOnlyKeys(value: Record<string, unknown>, allowed: readonly string[]): boolean {
  const allowedSet = new Set(allowed);
  return Object.keys(value).every((key) => allowedSet.has(key));
}

export async function finishRateLimitedRefresh(
  response: Response,
  persist: () => Promise<void>,
  schedule: () => Promise<void>,
): Promise<void> {
  try {
    await persist();
    await schedule();
  } finally {
    await cancelResponseBody(response);
  }
}

export function retryAfterDelayMs(
  value: string | null,
  now = Date.now(),
  fallbackAttempt = 1,
  jitter = Math.random(),
): number {
  const candidate = value?.trim() ?? "";
  const numeric = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(candidate);
  const seconds = numeric ? Number(candidate) : Number.NaN;
  const parsed = numeric
    ? Number.isFinite(seconds) && seconds >= 0 ? seconds * 1_000 : Number.NaN
    : candidate ? Date.parse(candidate) - now : Number.NaN;
  if (!Number.isFinite(parsed)) {
    const attempt = Number.isSafeInteger(fallbackAttempt) && fallbackAttempt > 0
      ? Math.min(MAX_REFRESH_BACKOFF_ATTEMPT, fallbackAttempt)
      : 1;
    const ceiling = Math.min(
      MAX_REFRESH_BACKOFF_MS,
      DEFAULT_REFRESH_BACKOFF_MS * 2 ** (attempt - 1),
    );
    const boundedJitter = Number.isFinite(jitter) ? Math.max(0, Math.min(1, jitter)) : 0.5;
    return Math.round(ceiling / 2 + ceiling / 2 * boundedJitter);
  }
  return Math.max(1_000, Math.min(MAX_REFRESH_BACKOFF_MS, parsed));
}

function nextRefreshAttempt(previous: number | undefined): number {
  const attempt = typeof previous === "number" && Number.isSafeInteger(previous) && previous >= 0
    ? previous + 1
    : 1;
  return Math.min(MAX_REFRESH_BACKOFF_ATTEMPT, attempt);
}

async function cancelResponseBody(response: Response): Promise<void> {
  try { await response.body?.cancel(); } catch { /* Response disposal is best-effort. */ }
}

async function hasRequestPayload(request: Request): Promise<boolean> {
  if (request.body === null) return false;
  const reader = request.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return false;
      if (value.byteLength > 0) return true;
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

async function leaseFingerprint(projection: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256",
    new TextEncoder().encode(`nanocodex/model-credential-lease/v1\n${projection}`));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function validLeaseRegistry(value: unknown): value is LeaseRegistry {
  if (!value || typeof value !== "object") return false;
  const registry = value as Partial<LeaseRegistry>;
  const records = (entries: unknown) => !!entries && typeof entries === "object"
    && Object.entries(entries).every(([region, entry]) => LEASE_REGIONS.has(region) && !!entry
      && typeof entry === "object" && Number.isSafeInteger((entry as ModelLease).epoch)
      && typeof (entry as ModelLease).expiresAt === "number");
  return registry.version === 1 && Number.isSafeInteger(registry.epoch)
    && (registry.owner === undefined || (typeof registry.owner === "string" && USER_ID.test(registry.owner)))
    && records(registry.leases) && records(registry.pending)
    && (registry.claude === undefined || (Number.isSafeInteger(registry.claude.generation)
      && (registry.claude.revision === null || typeof registry.claude.revision === "string")));
}

class BrokerFailure extends Error {
  constructor(readonly status: number, readonly code: string) { super(code); }
}

function failure(error: unknown): BrokerFailure {
  return error instanceof BrokerFailure ? error : new BrokerFailure(503, "credential_broker_failed");
}

function publicLogin(login: PendingLogin): Record<string, unknown> {
  return {
    state: "pending",
    verification_url: login.verificationUrl,
    user_code: login.userCode,
    expires_at: login.expiresAt,
    poll_after_ms: Math.max(0, login.nextPollAt - Date.now()),
  };
}

function issuerUrl(env: BrokerEnv): URL {
  const raw = env.CHATGPT_ISSUER?.trim() || "https://auth.openai.com/";
  const issuer = new URL(raw.endsWith("/") ? raw : `${raw}/`);
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  const local = environment === "development" || environment === "local" || environment === "test";
  if (issuer.username || issuer.password || issuer.search || issuer.hash
    || (!local && (issuer.protocol !== "https:" || issuer.hostname !== "auth.openai.com"))
    || (local && issuer.protocol !== "https:" && !(issuer.protocol === "http:"
      && (issuer.hostname === "127.0.0.1" || issuer.hostname === "localhost")))) {
    throw new BrokerFailure(503, "invalid_chatgpt_issuer");
  }
  return issuer;
}

async function exchangeAuthorizationCode(
  issuer: URL,
  code: string,
  verifier: string,
): Promise<Record<string, unknown>> {
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: new URL("deviceauth/callback", issuer).href,
    client_id: CODEX_CLIENT_ID,
    code_verifier: verifier,
  });
  const response = await providerFetch(new URL("oauth/token", issuer), {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!response.ok) {
    await cancelResponseBody(response);
    throw new BrokerFailure(503, "chatgpt_token_exchange_failed");
  }
  return providerJson(response);
}

function credentialFromTokens(
  tokens: Record<string, unknown>,
  previous: ChatGptCredential | undefined,
  revision: number,
  provenance: ChatGptCredential["provenance"] = previous?.provenance,
): ChatGptCredential {
  const accessToken = stringField(tokens, "access_token");
  const claims = idTokenClaims(stringField(tokens, "id_token"));
  const accountId = claims.accountId ?? previous?.accountId;
  const expiresAt = jwtExpiration(accessToken)
    ?? (positiveNumber(tokens.expires_in) ? Date.now() + positiveNumber(tokens.expires_in)! * 1_000 : undefined);
  if (!accessToken || !accountId || !expiresAt || expiresAt <= Date.now()) {
    throw new BrokerFailure(503, "invalid_chatgpt_token_response");
  }
  return {
    accessToken,
    refreshToken: stringField(tokens, "refresh_token") ?? previous?.refreshToken ?? "",
    accountId,
    fedramp: claims.fedramp ?? previous?.fedramp ?? false,
    expiresAt,
    revision,
    ...(provenance ? { provenance } : {}),
    refreshState: "ready",
    deadReason: null,
    ...(previous?.limitedUntil ? { limitedUntil: previous.limitedUntil } : {}),
    authorizationRevision: previous?.authorizationRevision ?? previous?.revision ?? revision,
  };
}

function idTokenClaims(token: string | undefined): { accountId?: string; fedramp?: boolean } {
  const claims = jwtPayload(token);
  const auth = claims?.["https://api.openai.com/auth"];
  if (!isRecord(auth)) return {};
  const accountId = stringField(auth, "chatgpt_account_id");
  return {
    ...(accountId ? { accountId } : {}),
    ...(typeof auth.chatgpt_account_is_fedramp === "boolean"
      ? { fedramp: auth.chatgpt_account_is_fedramp }
      : {}),
  };
}

function jwtExpiration(token: string | undefined): number | undefined {
  const exp = jwtPayload(token)?.exp;
  return typeof exp === "number" && Number.isFinite(exp) ? exp * 1_000 : undefined;
}

function jwtPayload(token: string | undefined): Record<string, unknown> | undefined {
  const encoded = token?.split(".")[1];
  if (!encoded) return undefined;
  try {
    const base64 = encoded.replaceAll("-", "+").replaceAll("_", "/")
      .padEnd(Math.ceil(encoded.length / 4) * 4, "=");
    const bytes = Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
    const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
    return isRecord(parsed) ? parsed : undefined;
  } catch { return undefined; }
}

export function validChatGptCredentialImport(
  value: unknown,
  now = Date.now(),
): value is ChatGptCredentialImport {
  if (!isRecord(value)) return false;
  const keys = Object.keys(value);
  if (keys.length !== 5 || keys.some((key) => ![
    "access_token",
    "refresh_token",
    "account_id",
    "expires_at",
    "fedramp",
  ].includes(key))) return false;

  const accessToken = exactBoundedString(value.access_token, MAX_IMPORTED_TOKEN_BYTES);
  const refreshToken = exactBoundedString(value.refresh_token, MAX_IMPORTED_TOKEN_BYTES);
  const accountId = exactBoundedString(value.account_id, MAX_IMPORTED_ACCOUNT_ID_BYTES);
  const expiresAt = value.expires_at;
  if (!accessToken || !refreshToken || !accountId
    || !Number.isSafeInteger(expiresAt) || typeof expiresAt !== "number"
    || expiresAt <= now + REFRESH_EARLY_MS || typeof value.fedramp !== "boolean") {
    return false;
  }

  const accessClaims = strictJwtPayload(accessToken);
  if (!accessClaims
    || !Number.isSafeInteger(accessClaims.exp)
    || (accessClaims.exp as number) * 1_000 !== expiresAt
    || !matchingImportedAuthClaims(accessClaims, accountId, value.fedramp)) {
    return false;
  }
  return true;
}

export function validateVaultEntryPayload(
  value: unknown,
  kind: VaultKind,
): VaultEntryPayload | undefined {
  if (!isRecord(value)) return undefined;
  if (kind === "totp") {
    const name = vaultText(value.name, 120);
    const material = validateTotpEnrollment(value);
    return name && material ? { kind, name, ...material } : undefined;
  }
  const expected = vaultPayloadKeys(kind, Object.prototype.hasOwnProperty.call(value, "address_line_2"), Object.prototype.hasOwnProperty.call(value, "browser_origin"), Object.prototype.hasOwnProperty.call(value, "cvv"));
  const keys = Object.keys(value);
  if (keys.length !== expected.length || keys.some((key) => !expected.includes(key))) {
    return undefined;
  }
  const name = vaultText(value.name, 120);
  if (!name) return undefined;
  if (kind === "api_key") {
    const apiKey = vaultSecret(value.api_key, 8_192);
    return apiKey ? { kind, name, api_key: apiKey } : undefined;
  }
  if (kind === "login") {
    const username = vaultText(value.username, 512);
    const password = vaultSecret(value.password, 8_192);
    const origin = value.browser_origin;
    if (origin !== undefined && !validBrowserOrigin(origin)) return undefined;
    return username && password ? { kind, name, username, password, ...(typeof origin === "string" ? { browser_origin: origin } : {}) } : undefined;
  }
  if (kind === "card") {
    const cardNumber = vaultCardNumber(value.card_number);
    const expiryMonth = typeof value.expiry_month === "string"
      && /^(?:0?[1-9]|1[0-2])$/.test(value.expiry_month) ? value.expiry_month : undefined;
    const expiryYear = typeof value.expiry_year === "string"
      && /^[0-9]{4}$/.test(value.expiry_year) ? value.expiry_year : undefined;
    const cvv = typeof value.cvv === "string" && /^[0-9]{3,4}$/.test(value.cvv)
      ? value.cvv : undefined;
    const billingZip = vaultText(value.billing_zip, 32);
    return cardNumber && expiryMonth && expiryYear && (value.cvv === undefined || cvv) && billingZip
      ? {
          kind,
          name,
          card_number: cardNumber,
          expiry_month: expiryMonth,
          expiry_year: expiryYear,
          ...(cvv ? {cvv} : {}),
          billing_zip: billingZip,
        }
      : undefined;
  }
  if (kind === "address") {
    const addressLine1 = vaultText(value.address_line_1, 256);
    const addressLine2 = value.address_line_2 === undefined
      ? undefined : vaultText(value.address_line_2, 256);
    const city = vaultText(value.city, 120);
    const state = vaultText(value.state, 120);
    const zip = vaultText(value.zip, 32);
    const country = vaultText(value.country, 120);
    if (!addressLine1 || (value.address_line_2 !== undefined && !addressLine2)
      || !city || !state || !zip || !country) return undefined;
    return {
      kind,
      name,
      address_line_1: addressLine1,
      ...(addressLine2 ? { address_line_2: addressLine2 } : {}),
      city,
      state,
      zip,
      country,
    };
  }
  const phoneNumber = vaultText(value.phone_number, 64);
  return phoneNumber ? { kind, name, phone_number: phoneNumber } : undefined;
}

function vaultPayloadKeys(kind: VaultKind, hasAddressLine2 = false, hasBrowserOrigin = false, hasCvv = true): readonly string[] {
  switch (kind) {
    case "totp": return ["name", "seed", "issuer", "account", "origin", "algorithm", "digits", "period"];
    case "api_key": return ["name", "api_key"];
    case "login": return ["name", "username", "password", ...(hasBrowserOrigin ? ["browser_origin"] : [])];
    case "card": return [
      "name", "card_number", "expiry_month", "expiry_year", ...(hasCvv ? ["cvv"] : []), "billing_zip",
    ];
    case "address": return [
      "name", "address_line_1",
      ...(hasAddressLine2 ? ["address_line_2"] : []),
      "city", "state", "zip", "country",
    ];
    case "phone": return ["name", "phone_number"];
  }
}

function validateStoredVaultEntry(id: string, value: unknown): VaultEntry | undefined {
  if (!VAULT_ID.test(id) || !isRecord(value) || value.id !== id
    || !Number.isSafeInteger(value.createdAt) || (value.createdAt as number) < 0
    || !["login", "api_key", "card", "address", "phone", "totp"].includes(String(value.kind))) return undefined;
  const kind = value.kind as VaultKind;
  const payloadKeys = vaultPayloadKeys(
    kind,
    Object.prototype.hasOwnProperty.call(value, "address_line_2"),
    Object.prototype.hasOwnProperty.call(value, "browser_origin"),
    Object.prototype.hasOwnProperty.call(value, "cvv"),
  );
  const payload = Object.fromEntries(
    payloadKeys.map((key) => [key, value[key]]),
  );
  if (Object.keys(value).length !== payloadKeys.length + 3) return undefined;
  const validated = validateVaultEntryPayload(payload, kind);
  return validated
    ? { ...validated, id, createdAt: value.createdAt as number } as VaultEntry
    : undefined;
}

export function validateMaterializedVaultEntry(
  id: string,
  value: unknown,
): VaultEntry | undefined {
  return validateStoredVaultEntry(id, value);
}

function validateStoredVaultMetadata(
  id: string,
  value: unknown,
): VaultEntryMetadata | undefined {
  if (!VAULT_ID.test(id) || !isRecord(value) || value.id !== id
    || !Number.isSafeInteger(value.createdAt) || (value.createdAt as number) < 0
    || !["login", "api_key", "card", "address", "phone", "totp"].includes(String(value.kind))) {
    return undefined;
  }
  const kind = value.kind as VaultKind;
  const common = {
    id,
    kind,
    name: vaultText(value.name, 120),
    createdAt: value.createdAt as number,
  };
  if (!common.name) return undefined;
  if (kind === "totp") {
    const metadata = validateTotpMetadata(value);
    return metadata && hasExactKeys(value, ["id", "kind", "name", "createdAt", "issuer", "account", "origin", "algorithm", "digits", "period"])
      ? { ...common, kind, name: common.name, ...metadata } : undefined;
  }
  if (kind === "api_key") {
    return hasExactKeys(value, ["id", "kind", "name", "createdAt"])
      ? { ...common, kind, name: common.name } : undefined;
  }
  if (kind === "login") {
    const username = vaultText(value.username, 512);
    const origin = value.browser_origin;
    if (origin !== undefined && !validBrowserOrigin(origin)) return undefined;
    return username && hasExactKeys(value, ["id", "kind", "name", "username", "createdAt", ...(origin === undefined ? [] : ["browser_origin"])])
      ? { ...common, kind, name: common.name, username, ...(typeof origin === "string" ? { browser_origin: origin } : {}) }
      : undefined;
  }
  if (kind === "card") {
    const last4 = typeof value.last4 === "string" && /^[0-9]{4}$/.test(value.last4)
      ? value.last4 : undefined;
    return last4 && hasExactKeys(value, ["id", "kind", "name", "last4", "createdAt"])
      ? { ...common, kind, name: common.name, last4 }
      : undefined;
  }
  if (kind === "address") {
    const addressLine1 = vaultText(value.address_line_1, 256);
    const addressLine2 = value.address_line_2 === undefined
      ? undefined : vaultText(value.address_line_2, 256);
    const city = vaultText(value.city, 120);
    const state = vaultText(value.state, 120);
    const zip = vaultText(value.zip, 32);
    const country = vaultText(value.country, 120);
    const expected = [
      "id", "kind", "name", "address_line_1",
      ...(addressLine2 ? ["address_line_2"] : []),
      "city", "state", "zip", "country", "createdAt",
    ];
    return addressLine1 && city && state && zip && country
      && (value.address_line_2 === undefined || addressLine2)
      && hasExactKeys(value, expected)
      ? {
          ...common,
          kind,
          name: common.name,
          address_line_1: addressLine1,
          ...(addressLine2 ? { address_line_2: addressLine2 } : {}),
          city,
          state,
          zip,
          country,
        }
      : undefined;
  }
  const phoneNumber = vaultText(value.phone_number, 64);
  return phoneNumber && hasExactKeys(value, ["id", "kind", "name", "phone_number", "createdAt"])
    ? { ...common, kind, name: common.name, phone_number: phoneNumber }
    : undefined;
}

function vaultEntryMetadata(entry: VaultEntry): VaultEntryMetadata {
  const common = {
    id: entry.id,
    name: entry.name,
    createdAt: entry.createdAt,
  };
  switch (entry.kind) {
    case "totp": return { ...common, kind: entry.kind, issuer: entry.issuer, account: entry.account,
      origin: entry.origin, algorithm: entry.algorithm, digits: entry.digits, period: entry.period };
    case "api_key": return { ...common, kind: entry.kind };
    case "login": return { ...common, kind: entry.kind, username: entry.username, ...(entry.browser_origin ? { browser_origin: entry.browser_origin } : {}) };
    case "card": return {
      ...common,
      kind: entry.kind,
      last4: entry.card_number.replaceAll(" ", "").replaceAll("-", "").slice(-4),
    };
    case "address": return {
      ...common,
      kind: entry.kind,
      address_line_1: entry.address_line_1,
      ...(entry.address_line_2 ? { address_line_2: entry.address_line_2 } : {}),
      city: entry.city,
      state: entry.state,
      zip: entry.zip,
      country: entry.country,
    };
    case "phone": return { ...common, kind: entry.kind, phone_number: entry.phone_number };
  }
}

function sameVaultEntryMetadata(
  left: VaultEntryMetadata,
  right: VaultEntryMetadata,
): boolean {
  if (left.id !== right.id || left.kind !== right.kind || left.name !== right.name
    || left.createdAt !== right.createdAt) return false;
  switch (left.kind) {
    case "totp": return right.kind === left.kind && left.issuer === right.issuer && left.account === right.account
      && left.origin === right.origin && left.algorithm === right.algorithm && left.digits === right.digits && left.period === right.period;
    case "api_key": return true;
    case "login": return right.kind === left.kind && left.username === right.username && left.browser_origin === right.browser_origin;
    case "card": return right.kind === left.kind && left.last4 === right.last4;
    case "address": return right.kind === left.kind
      && left.address_line_1 === right.address_line_1
      && left.address_line_2 === right.address_line_2
      && left.city === right.city
      && left.state === right.state
      && left.zip === right.zip
      && left.country === right.country;
    case "phone": return right.kind === left.kind && left.phone_number === right.phone_number;
  }
}

function vaultEntryStorageKey(id: string): string {
  return `${VAULT_ENTRY_KEY_PREFIX}${id}`;
}

function browserCookieJarStorageKey(id: string): string {
  return `${BROWSER_COOKIE_JAR_KEY_PREFIX}${id}`;
}

function publicVaultEntry(entry: VaultEntry | VaultEntryMetadata): Readonly<{
  id: string;
  kind: VaultKind;
  name: string;
  created_at: number;
  issuer?: string;
  account?: string;
  origin?: string;
  algorithm?: "SHA1" | "SHA256" | "SHA512";
  digits?: 6 | 8;
  period?: number;
  username?: string;
  browser_origin?: string;
  last4?: string;
  address_line_1?: string;
  address_line_2?: string;
  city?: string;
  state?: string;
  zip?: string;
  country?: string;
  phone_number?: string;
}> {
  const metadata = "seed" in entry || "password" in entry || "api_key" in entry || "card_number" in entry
    ? vaultEntryMetadata(entry as VaultEntry)
    : entry as VaultEntryMetadata;
  const common = {
    id: metadata.id,
    kind: metadata.kind,
    name: metadata.name,
    created_at: metadata.createdAt,
  };
  switch (metadata.kind) {
    case "totp": return { ...common, issuer: metadata.issuer, account: metadata.account, origin: metadata.origin,
      algorithm: metadata.algorithm, digits: metadata.digits, period: metadata.period };
    case "api_key": return common;
    case "login": return { ...common, username: metadata.username, ...(metadata.browser_origin ? { browser_origin: metadata.browser_origin } : {}) };
    case "card": return { ...common, last4: metadata.last4 };
    case "address": return {
      ...common,
      address_line_1: metadata.address_line_1,
      ...(metadata.address_line_2 ? { address_line_2: metadata.address_line_2 } : {}),
      city: metadata.city,
      state: metadata.state,
      zip: metadata.zip,
      country: metadata.country,
    };
    case "phone": return { ...common, phone_number: metadata.phone_number };
  }
}

function hasExactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const expectedSet = new Set(expected);
  return Object.keys(value).length === expectedSet.size
    && Object.keys(value).every((key) => expectedSet.has(key));
}

function randomVaultId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(24));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function vaultText(value: unknown, maxBytes: number): string | undefined {
  return exactBoundedString(value, maxBytes);
}

function vaultSecret(value: unknown, maxBytes: number): string | undefined {
  return typeof value === "string" && value.length > 0 && !value.includes("\0")
    && new TextEncoder().encode(value).byteLength <= maxBytes ? value : undefined;
}

function vaultCardNumber(value: unknown): string | undefined {
  const cardNumber = vaultText(value, 23);
  if (!cardNumber || !/^[0-9][0-9 -]*[0-9]$/.test(cardNumber)) return undefined;
  return /^[0-9]{12,19}$/.test(cardNumber.replaceAll(" ", "").replaceAll("-", ""))
    ? cardNumber : undefined;
}

function strictJwtPayload(token: string): Record<string, unknown> | undefined {
  return /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)
    ? jwtPayload(token)
    : undefined;
}

function matchingImportedAuthClaims(
  claims: Record<string, unknown>,
  accountId: string,
  fedramp: boolean,
): boolean {
  const auth = claims["https://api.openai.com/auth"];
  if (!isRecord(auth)) return false;
  const claimedAccount = auth.chatgpt_account_id;
  const claimedFedramp = auth.chatgpt_account_is_fedramp;
  if (typeof claimedAccount !== "string" || claimedAccount !== accountId) return false;
  if (claimedFedramp !== undefined && typeof claimedFedramp !== "boolean") return false;
  if (claimedFedramp !== undefined) return claimedFedramp === fedramp;
  return fedramp === false;
}

function exactBoundedString(value: unknown, maxBytes: number): string | undefined {
  if (typeof value !== "string" || !value || value.trim() !== value
    || /[\u0000-\u001f\u007f]/.test(value)
    || new TextEncoder().encode(value).byteLength > maxBytes) return undefined;
  return value;
}

function localCredentialClaimEnabled(env: BrokerEnv): boolean {
  const environment = env.ENVIRONMENT?.trim().toLowerCase();
  return env.ALLOW_LOCAL_CREDENTIAL_CLAIM === "true"
    && (environment === "development" || environment === "local" || environment === "test");
}

function localSponsoredTrialResetEnabled(env: BrokerEnv): boolean {
  return localCredentialClaimEnabled(env)
    && env.NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET === "true";
}

function parseExpiry(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value) && value > 0) {
    return value < 10_000_000_000 ? value * 1_000 : value;
  }
  if (typeof value === "string" && value.trim()) {
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

async function providerFetch(url: URL, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, { ...init, redirect: "manual", signal: AbortSignal.timeout(30_000) });
  } catch { throw new BrokerFailure(503, "chatgpt_provider_unavailable"); }
}

async function providerJson(response: Response): Promise<Record<string, unknown>> {
  const text = await readBoundedText(response, MAX_PROVIDER_RESPONSE_BYTES);
  try {
    const value: unknown = JSON.parse(text);
    if (!isRecord(value)) throw new Error();
    return value;
  } catch { throw new BrokerFailure(503, "invalid_chatgpt_provider_response"); }
}

async function readJson(request: Request, limit: number): Promise<Record<string, unknown> | undefined> {
  try {
    const value: unknown = JSON.parse(await readBoundedText(request, limit));
    return isRecord(value) ? value : undefined;
  } catch (error) {
    if (error instanceof BrokerFailure) throw error;
    return undefined;
  }
}

async function readBoundedText(message: Request | Response, limit: number): Promise<string> {
  if (!message.body) return "";
  const reader = message.body.getReader();
  const decoder = new TextDecoder();
  let total = 0;
  let text = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return text + decoder.decode();
      total += value.byteLength;
      if (total > limit) {
        await reader.cancel();
        throw new BrokerFailure(413, "body_too_large");
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}

function stringField(value: unknown, key: string): string | undefined {
  return isRecord(value) && typeof value[key] === "string" && value[key].trim()
    ? value[key] as string
    : undefined;
}
function numberField(value: unknown, key: string): number | undefined {
  return isRecord(value) && Number.isSafeInteger(value[key]) ? value[key] as number : undefined;
}
function positiveNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}
function positiveNumberString(value: unknown): number | undefined {
  if (typeof value !== "string" || !/^[1-9][0-9]*$/.test(value)) return undefined;
  return positiveNumber(Number(value));
}
function sponsoredConnections(value: unknown): readonly SponsoredConnection[] {
  if (value === undefined) return [];
  if (!isRecord(value)
    || Object.keys(value).length !== 1
    || !Array.isArray(value.connections)
    || value.connections.length > SPONSORED_CONNECTION_LIMIT
    || value.connections.some((connection) => !isRecord(connection)
      || Object.keys(connection).length !== 2
      || typeof connection.id !== "string"
      || !SPONSORED_CONNECTION_ID.test(connection.id)
      || !Number.isSafeInteger(connection.leaseExpiresAt)
      || (connection.leaseExpiresAt as number) <= 0)
    || new Set(value.connections.map((connection) => (
      (connection as Record<string, unknown>).id
    ))).size !== value.connections.length) {
    throw new BrokerFailure(503, "sponsored_connection_state_invalid");
  }
  return value.connections as readonly SponsoredConnection[];
}
function sponsoredPrompts(
  value: unknown,
): readonly SponsoredPrompt[] {
  if (value === undefined) return [];
  if (!isRecord(value)
    || Object.keys(value).length !== 1
    || !Array.isArray(value.prompts)
    || value.prompts.length > SPONSORED_PROMPT_LIMIT
    || value.prompts.some((prompt) => !isRecord(prompt)
      || Object.keys(prompt).some((key) => ![
        "id", "hash", "phase", "attempts", "leaseExpiresAt", "continuation",
      ].includes(key))
      || Object.keys(prompt).length < 4
      || typeof prompt.id !== "string"
      || !SPONSORED_PROMPT_ID.test(prompt.id)
      || typeof prompt.hash !== "string"
      || !SPONSORED_PROMPT_HASH.test(prompt.hash)
      || (prompt.phase !== "in_flight" && prompt.phase !== "retryable"
        && prompt.phase !== "continuation" && prompt.phase !== "terminal")
      || !Number.isSafeInteger(prompt.attempts)
      || (prompt.attempts as number) < 1
      || (prompt.attempts as number) > SPONSORED_PROMPT_MAX_ATTEMPTS
      || (prompt.phase === "in_flight") !== (Number.isSafeInteger(prompt.leaseExpiresAt)
        && (prompt.leaseExpiresAt as number) > 0)
      || (prompt.phase !== "in_flight" && prompt.leaseExpiresAt !== undefined)
      || (prompt.phase === "continuation") !== validSponsoredContinuation(prompt.continuation)
      || (prompt.phase !== "continuation" && prompt.continuation !== undefined))
    || new Set(value.prompts.map((prompt) => (prompt as Record<string, unknown>).id)).size
      !== value.prompts.length) {
    throw new BrokerFailure(503, "sponsored_prompt_state_invalid");
  }
  return value.prompts as readonly SponsoredPrompt[];
}
function validSponsoredContinuation(value: unknown): boolean {
  return isRecord(value)
    && Object.keys(value).length === 2
    && typeof value.responseId === "string"
    && SPONSORED_RESPONSE_ID.test(value.responseId)
    && sponsoredCallIds(value.callIds) !== undefined;
}
function sponsoredCallIds(value: unknown): readonly string[] | undefined {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_SPONSORED_CALL_IDS
    || value.some((callId) => typeof callId !== "string" || !SPONSORED_CALL_ID.test(callId))) {
    return undefined;
  }
  const unique = [...new Set(value as string[])].sort();
  return unique.length === value.length ? unique : undefined;
}
function isStringSubset(required: readonly string[], supplied: readonly string[]): boolean {
  const values = new Set(supplied);
  return required.every((value) => values.has(value));
}
function sponsoredPromptStatus(prompts: readonly unknown[]) {
  return {
    limit: SPONSORED_PROMPT_LIMIT,
    used: prompts.length,
    remaining: SPONSORED_PROMPT_LIMIT - prompts.length,
  };
}
function sponsoredPromptLeaseMs(env: BrokerEnv): number {
  return env.ENVIRONMENT?.trim().toLowerCase() === "test"
    ? TEST_SPONSORED_PROMPT_LEASE_MS
    : SPONSORED_PROMPT_LEASE_MS;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function isJsonContentType(value: string | null): boolean {
  return value?.split(";", 1)[0]?.trim().toLowerCase() === "application/json";
}
function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
function noStoreHeaders(): Record<string, string> {
  return { "cache-control": "no-store", pragma: "no-cache" };
}
function json(body: unknown, status: number): Response {
  return Response.json(body, { status, headers: noStoreHeaders() });
}
function jsonError(status: number, error: string): Response {
  return json({ error }, status);
}

export function validBrowserOrigin(value: unknown): value is string {
  if (typeof value !== "string" || value.length > 2048) return false;
  try { const url = new URL(value); return url.protocol === "https:" && url.origin === value && !url.username && !url.password; } catch { return false; }
}
