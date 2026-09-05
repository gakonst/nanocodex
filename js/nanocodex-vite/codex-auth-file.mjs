import { createHash } from "node:crypto";
import { open } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const MAX_AUTH_FILE_BYTES = 64 * 1024;
const DEFAULT_MINIMUM_TTL_MS = 5 * 60_000;

export function defaultCodexAuthFile(environment = process.env) {
  const explicit = nonEmpty(environment.NANOCODEX_AUTH_FILE)
    ?? nonEmpty(environment.NANOCODEX_CODEX_AUTH_FILE);
  if (explicit !== undefined) return resolve(explicit);
  const codexHome = nonEmpty(environment.CODEX_HOME);
  return resolve(codexHome === undefined ? join(homedir(), ".codex", "auth.json") : join(codexHome, "auth.json"));
}

export async function readCodexSubscription(path, options = {}) {
  const authPath = resolve(path);
  const minimumTtlMs = options.minimumTtlMs ?? DEFAULT_MINIMUM_TTL_MS;
  if (!Number.isSafeInteger(minimumTtlMs) || minimumTtlMs < 0) {
    throw new TypeError("minimumTtlMs must be a non-negative integer");
  }

  let file;
  try {
    file = await open(authPath, "r");
  } catch (error) {
    throw authError("missing", `cannot open Codex auth file: ${errorMessage(error)}`);
  }

  let encoded;
  try {
    const metadata = await file.stat();
    if (!metadata.isFile()) throw new Error(`Codex auth path is not a file: ${authPath}`);
    if (metadata.size > MAX_AUTH_FILE_BYTES) {
      throw new Error(`Codex auth file exceeds ${MAX_AUTH_FILE_BYTES} bytes`);
    }
    if (process.platform !== "win32" && (metadata.mode & 0o077) !== 0) {
      throw authError("insecure", "Codex auth file must not be accessible by group or other users");
    }
    encoded = await file.readFile();
  } catch (error) {
    if (error?.code === "NANOCODEX_AUTH_INSECURE") throw error;
    throw authError("invalid", `cannot read Codex auth file: ${errorMessage(error)}`);
  } finally {
    await file.close();
  }
  if (encoded.byteLength > MAX_AUTH_FILE_BYTES) {
    throw authError("invalid", `Codex auth file exceeds ${MAX_AUTH_FILE_BYTES} bytes`);
  }

  let parsed;
  try {
    parsed = JSON.parse(encoded.toString("utf8"));
  } catch (error) {
    throw authError("invalid", `cannot parse Codex auth file: ${errorMessage(error)}`);
  }
  if (!isRecord(parsed) || parsed.auth_mode !== "chatgpt" || !isRecord(parsed.tokens)) {
    throw authError("missing", "Codex auth file does not contain a ChatGPT subscription login");
  }

  const accessToken = requiredString(parsed.tokens.access_token, "access token");
  const idClaims = jwtPayload(optionalString(parsed.tokens.id_token));
  const accessClaims = jwtPayload(accessToken);
  const idAuth = nestedRecord(idClaims, "https://api.openai.com/auth");
  const accessAuth = nestedRecord(accessClaims, "https://api.openai.com/auth");
  const accountIds = [
    optionalString(parsed.tokens.account_id),
    optionalString(idAuth?.chatgpt_account_id),
    optionalString(accessAuth?.chatgpt_account_id),
  ].filter((value) => value !== undefined);
  const accountId = accountIds[0];
  if (accountId === undefined) {
    throw authError("invalid", "Codex auth file is missing the ChatGPT account ID");
  }
  if (accountIds.some((candidate) => candidate !== accountId)) {
    throw authError("invalid", "Codex auth file contains conflicting ChatGPT account IDs");
  }

  const expiresAt = typeof accessClaims?.exp === "number" && Number.isFinite(accessClaims.exp)
    ? accessClaims.exp * 1_000
    : undefined;
  if (expiresAt === undefined || expiresAt <= Date.now() + minimumTtlMs) {
    throw authError("expired", "Codex access token expires too soon; run `codex login` and retry");
  }

  const fedrampClaims = [
    optionalBoolean(idAuth?.chatgpt_account_is_fedramp),
    optionalBoolean(accessAuth?.chatgpt_account_is_fedramp),
  ].filter((value) => value !== undefined);
  if (fedrampClaims.some((candidate) => candidate !== fedrampClaims[0])) {
    throw authError("invalid", "Codex auth file contains conflicting FedRAMP claims");
  }

  return Object.freeze({
    accessToken,
    accountId,
    fedramp: fedrampClaims[0] ?? false,
    expiresAt,
    revision: createHash("sha256")
      .update(accessToken)
      .update("\0")
      .update(accountId)
      .digest("base64url"),
  });
}

function jwtPayload(token) {
  if (token === undefined) return undefined;
  const encoded = token.split(".")[1];
  if (!encoded) return undefined;
  try {
    const value = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
    return isRecord(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

function nestedRecord(value, key) {
  const nested = value?.[key];
  return isRecord(nested) ? nested : undefined;
}

function requiredString(value, name) {
  const normalized = optionalString(value);
  if (normalized === undefined) {
    throw authError("invalid", `Codex auth file is missing the ${name}`);
  }
  return normalized;
}

function optionalString(value) {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function optionalBoolean(value) {
  return typeof value === "boolean" ? value : undefined;
}

function nonEmpty(value) {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function authError(kind, message) {
  return Object.assign(new Error(message), {
    code: `NANOCODEX_AUTH_${kind.toUpperCase()}`,
    kind,
  });
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
