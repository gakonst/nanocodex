import { actor } from "rivetkit";
import { db, type RawAccess } from "rivetkit/db";

import {
  type AuthCapabilityProof,
  verifyCapabilityProof,
} from "./auth-capability.js";

const OAUTH_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_TOKEN_ENDPOINT = "https://auth.openai.com/oauth/token";
const REFRESH_EARLY_MS = 5 * 60_000;
const MAX_TOKEN_RESPONSE_BYTES = 16 * 1024;

export type SubscriptionSnapshot = {
  bearerToken: string;
  accountId: string;
  fedramp: boolean;
  revision: number;
};

export type SubscriptionStatus = {
  configured: boolean;
  account_id?: string;
  revision?: number;
  expires_at?: number | null;
  refreshed_at?: number;
};

type CredentialRow = {
  access_token: string;
  refresh_token: string;
  account_id: string;
  fedramp: number;
  revision: number;
  expires_at: number | null;
  refreshed_at: number;
};

type AuthVars = {
  refreshing: { revision: number; promise: Promise<CredentialRow> } | undefined;
  usedNonces: Map<string, number>;
};

type RefreshResponse = {
  access_token?: unknown;
  refresh_token?: unknown;
  id_token?: unknown;
};

export const nanocodexAuth = actor({
  state: {},
  createVars: (): AuthVars => ({ refreshing: undefined, usedNonces: new Map() }),
  db: db({
    onMigrate: async (database) => {
      await database.execute(`
        CREATE TABLE IF NOT EXISTS credentials (
          singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
          access_token TEXT NOT NULL,
          refresh_token TEXT NOT NULL,
          account_id TEXT NOT NULL,
          fedramp INTEGER NOT NULL,
          revision INTEGER NOT NULL,
          expires_at INTEGER,
          refreshed_at INTEGER NOT NULL
        )
      `);
    },
  }),
  actions: {
    snapshot: async (c, proof: AuthCapabilityProof): Promise<SubscriptionSnapshot> => {
      verifyCapabilityProof(proof, "snapshot", c.vars.usedNonces);
      let current = await credential(c.db);
      if (!current) current = await seed(c.db);
      if (current.expires_at !== null && current.expires_at <= Date.now() + REFRESH_EARLY_MS) {
        current = await refresh(c.db, c.vars, current.revision);
      }
      return toSnapshot(current);
    },
    recover: async (
      c,
      proof: AuthCapabilityProof,
      rejectedRevision: number,
    ): Promise<SubscriptionSnapshot> => {
      verifyCapabilityProof(proof, `recover:${rejectedRevision}`, c.vars.usedNonces);
      if (!Number.isSafeInteger(rejectedRevision) || rejectedRevision < 0) {
        throw new Error("revision must be a non-negative safe integer");
      }
      let current = await credential(c.db);
      if (!current) current = await seed(c.db);
      return toSnapshot(await refresh(c.db, c.vars, rejectedRevision));
    },
    status: async (c, proof: AuthCapabilityProof): Promise<SubscriptionStatus> => {
      verifyCapabilityProof(proof, "status", c.vars.usedNonces);
      const current = await credential(c.db);
      return {
        configured: current !== undefined,
        ...(current === undefined ? {} : {
          account_id: current.account_id,
          revision: current.revision,
          expires_at: current.expires_at,
          refreshed_at: current.refreshed_at,
        }),
      };
    },
    reset: async (c, proof: AuthCapabilityProof): Promise<void> => {
      verifyCapabilityProof(proof, "reset", c.vars.usedNonces);
      if (c.vars.refreshing) {
        try {
          await c.vars.refreshing.promise;
        } catch {
          // Reset wins after a failed refresh.
        }
      }
      await c.db.execute("DELETE FROM credentials");
    },
  },
  options: {
    actionTimeout: 30_000,
    sleepTimeout: 30_000,
  },
});

async function refresh(database: RawAccess, vars: AuthVars, rejectedRevision: number): Promise<CredentialRow> {
  const current = await credential(database);
  if (!current) throw new Error("ChatGPT credentials are not initialized");
  if (current.revision !== rejectedRevision) return current;
  if (vars.refreshing?.revision === rejectedRevision) return vars.refreshing.promise;

  const promise = performRefresh(database, current);
  vars.refreshing = { revision: rejectedRevision, promise };
  try {
    return await promise;
  } finally {
    if (vars.refreshing?.promise === promise) vars.refreshing = undefined;
  }
}

async function performRefresh(database: RawAccess, current: CredentialRow): Promise<CredentialRow> {
  if (!current.refresh_token) {
    throw new Error(
      "ChatGPT access token needs rotation; run `codex login` and redeploy the subscription demo",
    );
  }
  const response = await fetch(process.env.CHATGPT_TOKEN_ENDPOINT ?? DEFAULT_TOKEN_ENDPOINT, {
    method: "POST",
    signal: AbortSignal.timeout(25_000),
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_id: OAUTH_CLIENT_ID,
      grant_type: "refresh_token",
      refresh_token: current.refresh_token,
    }),
  });
  const encoded = await readBoundedText(response, MAX_TOKEN_RESPONSE_BYTES);
  if (!response.ok) {
    const code = parseRefreshErrorCode(encoded);
    throw new Error(code
      ? `ChatGPT token refresh was rejected: ${code}`
      : `ChatGPT token refresh failed with HTTP ${response.status}`);
  }

  let refreshed: RefreshResponse;
  try {
    refreshed = JSON.parse(encoded) as RefreshResponse;
  } catch {
    throw new Error("ChatGPT token refresh returned invalid JSON");
  }
  const accessToken = optionalString(refreshed.access_token);
  if (accessToken === undefined) throw new Error("ChatGPT token refresh omitted access_token");
  const refreshToken = optionalString(refreshed.refresh_token) ?? current.refresh_token;
  const idToken = optionalString(refreshed.id_token);
  const claims = idToken === undefined ? undefined : jwtPayload(idToken);
  const claimedAccount = nestedString(claims, "https://api.openai.com/auth", "chatgpt_account_id");
  if (claimedAccount !== undefined && claimedAccount !== current.account_id) {
    throw new Error("the refreshed ChatGPT credential changed accounts");
  }
  const claimedFedramp = nestedBoolean(
    claims,
    "https://api.openai.com/auth",
    "chatgpt_account_is_fedramp",
  );
  const next: CredentialRow = {
    access_token: accessToken,
    refresh_token: refreshToken,
    account_id: current.account_id,
    fedramp: claimedFedramp === undefined ? current.fedramp : (claimedFedramp ? 1 : 0),
    revision: current.revision + 1,
    expires_at: jwtExpiration(accessToken),
    refreshed_at: Date.now(),
  };
  await database.execute(
    `UPDATE credentials SET
       access_token = ?, refresh_token = ?, fedramp = ?, revision = ?, expires_at = ?, refreshed_at = ?
     WHERE singleton = 1 AND revision = ?`,
    next.access_token,
    next.refresh_token,
    next.fedramp,
    next.revision,
    next.expires_at,
    next.refreshed_at,
    current.revision,
  );
  return next;
}

async function seed(database: RawAccess): Promise<CredentialRow> {
  const accessToken = requiredSecret("CHATGPT_ACCESS_TOKEN");
  const refreshToken = optionalString(process.env.CHATGPT_REFRESH_TOKEN) ?? "";
  const accountId = requiredSecret("CHATGPT_ACCOUNT_ID");
  const row: CredentialRow = {
    access_token: accessToken,
    refresh_token: refreshToken,
    account_id: accountId,
    fedramp: process.env.CHATGPT_FEDRAMP === "true" ? 1 : 0,
    revision: 0,
    expires_at: jwtExpiration(accessToken),
    refreshed_at: Date.now(),
  };
  await database.execute(
    `INSERT OR IGNORE INTO credentials
     (singleton, access_token, refresh_token, account_id, fedramp, revision, expires_at, refreshed_at)
     VALUES (1, ?, ?, ?, ?, ?, ?, ?)`,
    row.access_token,
    row.refresh_token,
    row.account_id,
    row.fedramp,
    row.revision,
    row.expires_at,
    row.refreshed_at,
  );
  const persisted = await credential(database);
  if (!persisted) throw new Error("failed to initialize ChatGPT credentials");
  return persisted;
}

async function credential(database: RawAccess): Promise<CredentialRow | undefined> {
  return (await database.execute<CredentialRow>(
    `SELECT access_token, refresh_token, account_id, fedramp, revision, expires_at, refreshed_at
     FROM credentials WHERE singleton = 1`,
  ))[0];
}

function toSnapshot(row: CredentialRow): SubscriptionSnapshot {
  return {
    bearerToken: row.access_token,
    accountId: row.account_id,
    fedramp: row.fedramp !== 0,
    revision: row.revision,
  };
}

function requiredSecret(name: string): string {
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`${name} is not configured`);
  return value;
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function jwtExpiration(token: string): number | null {
  const payload = jwtPayload(token);
  return typeof payload?.exp === "number" && Number.isFinite(payload.exp)
    ? payload.exp * 1000
    : null;
}

function jwtPayload(token: string): Record<string, unknown> | undefined {
  const encoded = token.split(".")[1];
  if (!encoded) return undefined;
  try {
    const value = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8")) as unknown;
    return isRecord(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

function nestedString(
  value: Record<string, unknown> | undefined,
  parent: string,
  child: string,
): string | undefined {
  const nested = value?.[parent];
  return isRecord(nested) && typeof nested[child] === "string" ? nested[child] : undefined;
}

function nestedBoolean(
  value: Record<string, unknown> | undefined,
  parent: string,
  child: string,
): boolean | undefined {
  const nested = value?.[parent];
  return isRecord(nested) && typeof nested[child] === "boolean" ? nested[child] : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function readBoundedText(response: Response, limit: number): Promise<string> {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > limit) throw new Error(`response exceeded ${limit} bytes`);
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const combined = new Uint8Array(bytes);
  let offset = 0;
  for (const chunk of chunks) {
    combined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(combined);
}

function parseRefreshErrorCode(encoded: string): string | undefined {
  try {
    const parsed = JSON.parse(encoded) as unknown;
    if (!isRecord(parsed)) return undefined;
    const error = parsed.error;
    if (typeof error === "string") return error.slice(0, 128);
    if (isRecord(error) && typeof error.code === "string") return error.code.slice(0, 128);
  } catch {
    // The HTTP status is enough when the body is not JSON.
  }
  return undefined;
}
