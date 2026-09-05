const ENVELOPE_VERSION = 1;
const DEVELOPMENT_KEY = "bmFub2NvZGV4LWRldmVsb3BtZW50LWtleS0wMDAwMDA";

export type CredentialVaultEnv = {
  ENVIRONMENT?: string;
  SESSION_CREDENTIAL_KEY?: string;
  SESSION_CREDENTIAL_KEY_PREVIOUS?: string;
};

export type EncryptedEnvelope = {
  version: typeof ENVELOPE_VERSION;
  keyId: string;
  iv: string;
  ciphertext: string;
};

/** Encrypt sensitive session state before it reaches Durable Object storage. */
export class CredentialVault {
  readonly #scope: Uint8Array<ArrayBuffer>;
  readonly #current: string;
  readonly #previous?: string;

  constructor(env: CredentialVaultEnv, scope: string) {
    const production = env.ENVIRONMENT === "production" || env.ENVIRONMENT === "preview";
    if (production && !env.SESSION_CREDENTIAL_KEY) {
      throw new Error("SESSION_CREDENTIAL_KEY is required outside development");
    }
    this.#current = env.SESSION_CREDENTIAL_KEY?.trim() || DEVELOPMENT_KEY;
    this.#previous = env.SESSION_CREDENTIAL_KEY_PREVIOUS?.trim() || undefined;
    this.#scope = ownedBytes(
      new TextEncoder().encode(`nanocodex/session-credential/v1/${scope}`),
    );
  }

  async seal(value: unknown): Promise<EncryptedEnvelope> {
    const material = await keyMaterial(this.#current);
    const iv: Uint8Array<ArrayBuffer> = crypto.getRandomValues(new Uint8Array(12));
    const plaintext = ownedBytes(new TextEncoder().encode(JSON.stringify(value)));
    const ciphertext = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv, additionalData: this.#scope, tagLength: 128 },
      material.key,
      plaintext,
    );
    return {
      version: ENVELOPE_VERSION,
      keyId: material.id,
      iv: encodeBase64Url(iv),
      ciphertext: encodeBase64Url(new Uint8Array(ciphertext)),
    };
  }

  async open<T>(envelope: unknown): Promise<{ value: T; reseal: boolean }> {
    if (!isEnvelope(envelope)) throw new Error("stored credential is not encrypted");
    const candidates = [this.#current, this.#previous].filter(
      (candidate): candidate is string => Boolean(candidate),
    );
    for (let index = 0; index < candidates.length; index += 1) {
      const material = await keyMaterial(candidates[index]!);
      if (material.id !== envelope.keyId) continue;
      try {
        const plaintext = await crypto.subtle.decrypt(
          {
            name: "AES-GCM",
            iv: decodeBase64Url(envelope.iv),
            additionalData: this.#scope,
            tagLength: 128,
          },
          material.key,
          decodeBase64Url(envelope.ciphertext),
        );
        return {
          value: JSON.parse(new TextDecoder().decode(plaintext)) as T,
          reseal: index !== 0,
        };
      } catch {
        throw new Error("stored credential failed authentication");
      }
    }
    throw new Error("stored credential uses an unavailable encryption key");
  }
}

async function keyMaterial(encoded: string): Promise<{ id: string; key: CryptoKey }> {
  const raw = decodeBase64Url(encoded);
  if (raw.byteLength !== 32) throw new Error("SESSION_CREDENTIAL_KEY must encode exactly 32 bytes");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", raw));
  return {
    id: encodeBase64Url(digest.subarray(0, 9)),
    key: await crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]),
  };
}

function isEnvelope(value: unknown): value is EncryptedEnvelope {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const found = value as Partial<EncryptedEnvelope>;
  return found.version === ENVELOPE_VERSION
    && typeof found.keyId === "string"
    && typeof found.iv === "string"
    && typeof found.ciphertext === "string";
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("invalid base64url value");
  const base64 = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(
    value.length + ((4 - value.length % 4) % 4),
    "=",
  );
  const binary = atob(base64);
  const decoded: Uint8Array<ArrayBuffer> = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    decoded[index] = binary.charCodeAt(index);
  }
  return decoded;
}

function ownedBytes(value: Uint8Array): Uint8Array<ArrayBuffer> {
  const owned: Uint8Array<ArrayBuffer> = new Uint8Array(value.byteLength);
  owned.set(value);
  return owned;
}
