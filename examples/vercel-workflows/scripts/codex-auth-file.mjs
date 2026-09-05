import { open } from "node:fs/promises";
import { resolve } from "node:path";

const MAX_AUTH_FILE_BYTES = 64 * 1024;
const MIN_ACCESS_TOKEN_TTL_MS = 5 * 60_000;

export async function readCodexSubscription(path) {
  const authPath = resolve(path);
  let file;
  try {
    file = await open(authPath, "r");
    const metadata = await file.stat();
    if (!metadata.isFile()) throw new Error(`Codex auth path is not a file: ${authPath}`);
    if (metadata.size > MAX_AUTH_FILE_BYTES) throw new Error("Codex auth file is too large");
    if (process.platform !== "win32" && (metadata.mode & 0o077) !== 0) {
      throw new Error("Codex auth file must not be accessible by group or other users");
    }
    const encoded = await file.readFile();
    if (encoded.byteLength > MAX_AUTH_FILE_BYTES) throw new Error("Codex auth file is too large");
    const parsed = JSON.parse(encoded.toString("utf8"));
    if (!isRecord(parsed) || parsed.auth_mode !== "chatgpt" || !isRecord(parsed.tokens)) {
      throw new Error("Codex auth file does not contain a ChatGPT subscription login");
    }
    const accessToken = requiredString(parsed.tokens.access_token, "access token");
    const idClaims = jwtPayload(optionalString(parsed.tokens.id_token));
    const authClaims = idClaims?.["https://api.openai.com/auth"];
    const accountId = optionalString(parsed.tokens.account_id)
      ?? (isRecord(authClaims) ? optionalString(authClaims.chatgpt_account_id) : undefined);
    if (!accountId) throw new Error("Codex auth file is missing the ChatGPT account ID");
    const expiresAt = jwtPayload(accessToken)?.exp;
    if (typeof expiresAt !== "number" || expiresAt * 1_000 <= Date.now() + MIN_ACCESS_TOKEN_TTL_MS) {
      throw new Error("Codex access token expires too soon; run `codex login` and retry");
    }
    return {
      accessToken,
      accountId,
      fedramp: isRecord(authClaims) && authClaims.chatgpt_account_is_fedramp === true,
      expiresAt: expiresAt * 1_000,
    };
  } catch (error) {
    throw new Error(`cannot read Codex auth file: ${errorMessage(error)}`);
  } finally {
    await file?.close();
  }
}

function jwtPayload(token) {
  if (!token) return undefined;
  const encoded = token.split(".")[1];
  if (!encoded) return undefined;
  try {
    const value = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
    return isRecord(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

function requiredString(value, name) {
  const normalized = optionalString(value);
  if (!normalized) throw new Error(`Codex auth file is missing the ${name}`);
  return normalized;
}

function optionalString(value) {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
