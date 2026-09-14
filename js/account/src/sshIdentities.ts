export type SshIdentityMetadata = Readonly<{
  reference: string;
  hostname: string;
  port: number;
  username: string;
  hostKeySha256: string;
  publicKey?: string;
}>;

export type SshIdentityProvisioning = Readonly<{
  reference: string;
  hostname: string;
  port: number;
  username: string;
  hostKeySha256: string;
}>;

export type SshIdentityPayload = Readonly<{
  private_key: string;
  hostname: string;
  port: number;
  username: string;
  host_key_sha256: string;
}>;

const REFERENCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const RESERVED_REFERENCES = new Set(["__proto__", "constructor", "prototype"]);
const USERNAME = /^[A-Za-z0-9._-]{1,128}$/;
const HOST_FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}=?$/;

export function decodeSshIdentities(value: unknown): readonly SshIdentityMetadata[] {
  if (!Array.isArray(value)) throw new Error("Invalid SSH identity status response.");
  return value.map((candidate) => {
    if (!isRecord(candidate)) throw new Error("Invalid SSH identity status response.");
    const { reference, hostname, port, username, host_key_sha256: hostKeySha256, public_key: publicKey } = candidate;
    if (typeof reference !== "string" || !validReference(reference)
      || typeof hostname !== "string" || !validLowercaseHostname(hostname)
      || !Number.isInteger(port) || (port as number) < 1 || (port as number) > 65_535
      || typeof username !== "string" || !USERNAME.test(username)
      || typeof hostKeySha256 !== "string" || !HOST_FINGERPRINT.test(hostKeySha256)) {
      throw new Error("Invalid SSH identity status response.");
    }
    if (publicKey !== undefined && (typeof publicKey !== "string" || publicKey.length > 16384
      || !/^(?:ssh-rsa|ecdsa-sha2-nistp(?:256|384|521)) [A-Za-z0-9+/]+={0,2}$/.test(publicKey))) throw new Error("Invalid SSH public key.");
    return { reference, hostname, port: port as number, username, hostKeySha256, ...(publicKey ? { publicKey } : {}) };
  });
}

export function createSshTargetPayload(provisioning: SshIdentityProvisioning) {
  if (!validReference(provisioning.reference)) {
    throw new Error("Reference must start with a letter or number and use at most 64 letters, numbers, dots, underscores, or hyphens.");
  }
  if (!validLowercaseHostname(provisioning.hostname)) {
    throw new Error("Hostname must be a public lowercase DNS name or public IPv4 address.");
  }
  if (!Number.isInteger(provisioning.port) || provisioning.port < 1 || provisioning.port > 65_535) {
    throw new Error("Port must be a whole number from 1 to 65535.");
  }
  if (!USERNAME.test(provisioning.username)) {
    throw new Error("Username may contain only letters, numbers, dots, underscores, and hyphens.");
  }
  if (!HOST_FINGERPRINT.test(provisioning.hostKeySha256)) {
    throw new Error("Trusted host fingerprint must be a SHA256 fingerprint, including the SHA256: prefix.");
  }
  return { hostname: provisioning.hostname, port: provisioning.port, username: provisioning.username, host_key_sha256: provisioning.hostKeySha256 };
}

export function createSshIdentityPayload(provisioning: SshIdentityProvisioning, privateKey: string): SshIdentityPayload {
  const target = createSshTargetPayload(provisioning);
  const privateKeyBytes = new TextEncoder().encode(privateKey).byteLength;
  if (privateKeyBytes < 64 || privateKeyBytes > 64 * 1024
    || privateKey.includes("\0")
    || !/-----BEGIN (?:RSA |EC )?PRIVATE KEY-----/u.test(privateKey)) {
    throw new Error("Choose an unencrypted PEM private-key file no larger than 64 KiB.");
  }
  return {
    private_key: privateKey,
    ...target,
  };
}

export function sshIdentityPath(reference: string): string {
  if (!validReference(reference)) throw new Error("Invalid SSH identity reference.");
  return `/v1/credentials/ssh/${encodeURIComponent(reference)}`;
}

function validReference(value: string): boolean {
  return REFERENCE.test(value) && !RESERVED_REFERENCES.has(value);
}

function validLowercaseHostname(value: string): boolean {
  if (!value || value.length > 253 || value !== value.toLowerCase() || value.endsWith(".")
    || value.includes("@") || value.includes(":") || /\s/u.test(value)) return false;
  const ipv4 = /^(?:\d{1,3}\.){3}\d{1,3}$/u.test(value);
  const dns = value.includes(".") && value.split(".").every((label) => (
    /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/u.test(label)
  ));
  if ((!ipv4 && !dns) || [".localhost", ".internal", ".invalid", ".local", ".test", ".home.arpa"].some(suffix => value.endsWith(suffix))) return false;
  if (ipv4) {
    const octets = value.split(".").map(Number), [a, b] = octets;
    if (octets.some(octet => octet > 255)) return false;
    // Match the credential broker's public target policy before reading a key file.
    if (a === 0 || a === 10 || a === 127 || a >= 224
      || (a === 100 && b >= 64 && b <= 127)
      || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
      || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19))) return false;
  }
  return true;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
