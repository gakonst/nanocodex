import { createHash } from "node:crypto";
import { open, type FileHandle } from "node:fs/promises";
import { resolve } from "node:path";

import type { SubscriptionSnapshot } from "./auth.js";
import type { SubscriptionProvider } from "./model-websocket.js";

const MAX_AUTH_FILE_BYTES = 64 * 1024;
const MIN_ACCESS_TOKEN_TTL_MS = 5 * 60_000;

export function createCodexAuthFileProvider(path: string): SubscriptionProvider {
  const authPath = resolve(path);
  return {
    snapshot: () => readSnapshot(authPath),
    recover: async (rejectedRevision) => {
      const current = await readSnapshot(authPath);
      if (current.revision === rejectedRevision) {
        throw new Error(
          "ChatGPT rejected the current Codex access token; run `codex login` and retry",
        );
      }
      return current;
    },
  };
}

async function readSnapshot(path: string): Promise<SubscriptionSnapshot> {
  let file: FileHandle;
  try {
    file = await open(path, "r");
  } catch (error) {
    throw new Error(`cannot open Codex auth file: ${errorMessage(error)}`);
  }
  let encoded: Buffer;
  try {
    const metadata = await file.stat();
    if (!metadata.isFile()) throw new Error(`Codex auth path is not a file: ${path}`);
    if (metadata.size > MAX_AUTH_FILE_BYTES) {
      throw new Error(`Codex auth file exceeds ${MAX_AUTH_FILE_BYTES} bytes`);
    }
    if (process.platform !== "win32" && (metadata.mode & 0o077) !== 0) {
      throw new Error("Codex auth file must not be accessible by group or other users");
    }
    encoded = await file.readFile();
  } catch (error) {
    throw new Error(`cannot read Codex auth file: ${errorMessage(error)}`);
  } finally {
    await file.close();
  }
  if (encoded.byteLength > MAX_AUTH_FILE_BYTES) {
    throw new Error(`Codex auth file exceeds ${MAX_AUTH_FILE_BYTES} bytes`);
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(encoded.toString("utf8")) as unknown;
  } catch (error) {
    throw new Error(`cannot parse Codex auth file: ${errorMessage(error)}`);
  }
  if (!isRecord(parsed) || parsed.auth_mode !== "chatgpt" || !isRecord(parsed.tokens)) {
    throw new Error("Codex auth file does not contain a ChatGPT subscription login");
  }

  const accessToken = requiredString(parsed.tokens.access_token, "access token");
  const idToken = optionalString(parsed.tokens.id_token);
  const idClaims = idToken === undefined ? undefined : jwtPayload(idToken);
  const authClaims = idClaims?.["https://api.openai.com/auth"];
  const accountId = optionalString(parsed.tokens.account_id)
    ?? (isRecord(authClaims) ? optionalString(authClaims.chatgpt_account_id) : undefined);
  if (accountId === undefined) throw new Error("Codex auth file is missing the ChatGPT account ID");

  const accessClaims = jwtPayload(accessToken);
  const expiresAt = typeof accessClaims?.exp === "number" && Number.isFinite(accessClaims.exp)
    ? accessClaims.exp * 1_000
    : undefined;
  if (expiresAt === undefined || expiresAt <= Date.now() + MIN_ACCESS_TOKEN_TTL_MS) {
    throw new Error("Codex access token expires too soon; run `codex login` and retry");
  }

  return {
    bearerToken: accessToken,
    accountId,
    fedramp: isRecord(authClaims) && authClaims.chatgpt_account_is_fedramp === true,
    revision: Number.parseInt(
      createHash("sha256").update(accessToken).update("\0").update(accountId).digest("hex").slice(0, 12),
      16,
    ),
  };
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

function requiredString(value: unknown, name: string): string {
  const normalized = optionalString(value);
  if (normalized === undefined) throw new Error(`Codex auth file is missing the ${name}`);
  return normalized;
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
