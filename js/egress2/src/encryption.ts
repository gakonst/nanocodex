/** AES-256-GCM storage envelope; no legacy plaintext reader. */
export class CredentialCipher {
  private readonly key: Promise<CryptoKey>;
  private readonly scope: string;

  constructor(encodedKey: string, owner: string) {
    if (typeof encodedKey !== "string" || !/^[A-Za-z0-9+/]{43}=$/.test(encodedKey)) {
      throw new Error("CREDENTIAL_ENCRYPTION_KEY must be base64-encoded 32 bytes");
    }
    const raw = decode(encodedKey);
    if (raw.length !== 32 || encode(raw) !== encodedKey) {
      throw new Error("CREDENTIAL_ENCRYPTION_KEY must be base64-encoded 32 bytes");
    }
    this.key = crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
    this.scope = `nanocodex/egress2/v1/${owner}/`;
  }

  async seal(value: string, purpose: "openai" | "chatgpt"): Promise<string> {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ciphertext = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv, additionalData: this.aad(purpose) }, await this.key,
      new TextEncoder().encode(value),
    );
    return `v1:${encode(iv)}:${encode(new Uint8Array(ciphertext))}`;
  }

  async open(envelope: string, purpose: "openai" | "chatgpt"): Promise<string> {
    const match = /^v1:([A-Za-z0-9+/]{16}):([A-Za-z0-9+/]+={0,2})$/.exec(envelope);
    if (!match) throw new Error("Invalid encrypted credential");
    try {
      const iv = decode(match[1]!);
      if (iv.length !== 12) throw new Error("Invalid IV");
      const plaintext = await crypto.subtle.decrypt(
        { name: "AES-GCM", iv, additionalData: this.aad(purpose) }, await this.key,
        decode(match[2]!),
      );
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(plaintext);
    } catch {
      throw new Error("Encrypted credential authentication failed");
    }
  }

  private aad(purpose: string): Uint8Array<ArrayBuffer> {
    const bytes = new TextEncoder().encode(this.scope + purpose);
    return new Uint8Array(bytes);
  }
}

function encode(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function decode(value: string): Uint8Array<ArrayBuffer> {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}
