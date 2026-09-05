export const chatGptCredentialImportResourcePrefix =
  "urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:";

const credentialImportResourcePrefix = "urn:nanocodex:credential-import:";
const commitmentDomain = "nanocodex/chatgpt-credential-import/v1\0";
const encoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export type ChatGptCredentialImport = Readonly<{
  access_token: string;
  refresh_token: string;
  account_id: string;
  expires_at: number;
  fedramp: boolean;
}>;

export const maxChatGptCredentialTokenBytes = 32 * 1024;
export const maxChatGptCredentialAccountBytes = 256;

export function parseChatGptCredentialImport(value: unknown): ChatGptCredentialImport {
  if (!isRecord(value)
    || !sameKeys(value, [
      "access_token", "refresh_token", "account_id", "expires_at", "fedramp",
    ])) {
    throw new Error("chatgpt_credential_import must contain exactly five credential fields.");
  }
  const accessToken = boundedUtf8(
    value.access_token,
    "chatgpt_credential_import.access_token",
    maxChatGptCredentialTokenBytes,
  );
  const refreshToken = boundedUtf8(
    value.refresh_token,
    "chatgpt_credential_import.refresh_token",
    maxChatGptCredentialTokenBytes,
  );
  const accountId = boundedUtf8(
    value.account_id,
    "chatgpt_credential_import.account_id",
    maxChatGptCredentialAccountBytes,
  );
  if (!isSafeInteger(value.expires_at) || value.expires_at < 0) {
    throw new Error("chatgpt_credential_import.expires_at must be a non-negative safe integer.");
  }
  if (typeof value.fedramp !== "boolean") {
    throw new Error("chatgpt_credential_import.fedramp must be a boolean.");
  }
  const expiresAt = value.expires_at as number;
  return Object.freeze({
    access_token: accessToken,
    refresh_token: refreshToken,
    account_id: accountId,
    expires_at: expiresAt,
    fedramp: value.fedramp,
  });
}

export async function chatGptCredentialImportDigest(value: unknown): Promise<string> {
  const credential = parseChatGptCredentialImport(value);
  const fields = [
    encoder.encode(credential.access_token),
    encoder.encode(credential.refresh_token),
    encoder.encode(credential.account_id),
  ];
  const domain = encoder.encode(commitmentDomain);
  const length = domain.byteLength
    + fields.reduce((total, field) => total + 4 + field.byteLength, 0)
    + 8
    + 1;
  const bytes = new Uint8Array(length);
  const view = new DataView(bytes.buffer);
  let offset = 0;
  bytes.set(domain, offset);
  offset += domain.byteLength;
  for (const field of fields) {
    view.setUint32(offset, field.byteLength, false);
    offset += 4;
    bytes.set(field, offset);
    offset += field.byteLength;
  }
  view.setBigUint64(offset, BigInt(credential.expires_at), false);
  offset += 8;
  bytes[offset] = credential.fedramp ? 1 : 0;
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return base64Url(digest);
}

export async function chatGptCredentialImportResource(value: unknown): Promise<string> {
  return `${chatGptCredentialImportResourcePrefix}${await chatGptCredentialImportDigest(value)}`;
}

export function credentialImportDigestFromResources(resources: unknown): string | undefined {
  if (!Array.isArray(resources)) throw new Error("Credential import resources must be an array.");
  const imports = [];
  for (const resource of resources) {
    if (typeof resource !== "string" || !resource.startsWith(credentialImportResourcePrefix)) continue;
    if (!isAllowedChatGptCredentialImportResource(resource)) {
      throw new Error("The ChatGPT credential import resource is malformed.");
    }
    imports.push(resource.slice(chatGptCredentialImportResourcePrefix.length));
  }
  if (imports.length > 1) {
    throw new Error("Only one ChatGPT credential import resource is allowed.");
  }
  return imports[0];
}

export function isAllowedChatGptCredentialImportResource(resource: unknown): boolean {
  return typeof resource === "string"
    && resource.startsWith(chatGptCredentialImportResourcePrefix)
    && /^[A-Za-z0-9_-]{43}$/.test(
      resource.slice(chatGptCredentialImportResourcePrefix.length),
    );
}

function boundedUtf8(value: unknown, label: string, maximum: number): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${label} must be a non-empty string.`);
  }
  const encoded = encoder.encode(value);
  if (encoded.byteLength > maximum
    || new TextDecoder("utf-8", { fatal: true }).decode(encoded) !== value) {
    throw new Error(`${label} exceeds its UTF-8 limit or is malformed.`);
  }
  return value;
}

function sameKeys(value: UnknownRecord, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  return actual.length === expected.length
    && [...expected].sort().every((key, index) => actual[index] === key);
}

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function isSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value);
}

function isRecord(value: unknown): value is UnknownRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
