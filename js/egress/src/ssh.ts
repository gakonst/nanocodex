import { connect as cloudflareConnect } from "cloudflare:sockets";
import { createSshCommand, createWebStreamSshStream } from "nanocodex/tools/ssh";

export const SSH_IDENTITY_REFERENCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const RESERVED_REFERENCES = new Set(["__proto__", "constructor", "prototype"]);
const SSH_USERNAME = /^[A-Za-z0-9._-]{1,128}$/;
const SSH_HOST_KEY = /^SHA256:[A-Za-z0-9+/]{43}=?$/;
/** Device attestations use the canonical OpenSSH form without padding. */
const ATTESTED_HOST_KEY = /^SHA256:[A-Za-z0-9+/]{43}$/;
const DEVICE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
/**
 * Owner-saved device-trust binding of a Vault SSH target. `device` names the
 * reference's own server Hand machine (`server:{serverHandID(owner, ref)}`);
 * `hand:MACHINE_ID` names one exact enrolled Hand. Never derived from a hostname.
 */
export const SSH_HOST_KEY_TRUST = /^(?:device|hand:[A-Za-z0-9][A-Za-z0-9._:-]{0,127})$/;
export const MAX_DEVICE_HOST_KEYS = 8;
const MAX_COMMAND_ARGUMENTS = 256;
const MAX_COMMAND_BYTES = 64 * 1024;
const REQUEST_FIELDS = new Set(["identity_ref", "hostname", "port", "username", "command", "stdin", "host_key_trust"]);

export type SshHostKeyTrust = "device" | `hand:${string}`;

/**
 * A saved Vault target. At least one host authority is required: the pinned
 * `hostKeySha256`, or an owner-saved device-trust binding `hostKeyTrust`.
 */
export type BrokeredSshTarget = Readonly<{
  hostname: string;
  port: number;
  username: string;
  hostKeySha256?: string;
  hostKeyTrust?: SshHostKeyTrust;
}>;

/**
 * `savedAt` is stamped by the credential broker when this immutable target
 * snapshot (reference, hostname, port, username, pin, binding) is created.
 * Changing any of them means delete and re-create, which restamps it.
 */
export type BrokeredSshIdentity = BrokeredSshTarget & Readonly<{
  privateKey: string;
  publicKey?: string;
  savedAt?: number;
}>;

/** The ACTIVE current-key device of the bound machine, read from the owner's account DO. */
export type DeviceHostKeyAttestation = Readonly<{
  deviceId: string;
  createdAt: number;
  hostKeys: readonly Readonly<{ fingerprint: string; attestedAt: number }>[];
}>;

export type BrokeredSshRequest = Readonly<{
  identityReference: string;
  hostname: string;
  port: number;
  username: string;
  command: readonly string[];
  /**
   * Explicit per-call opt-in only. Callers never supply fingerprints or a
   * machine: egress reads the attestation itself after resolving the owner
   * and the Vault target, for the machine that target is bound to.
   */
  hostKeyTrust?: "device";
  stdin?: string;
}>;

/** Which accepted authority matched the presented host key. */
export type SshHostKeyMatch = Readonly<{ source: "vault_pin" | `device:${string}`; fingerprint: string }>;

type SocketLike = Readonly<{
  readable: ReadableStream<Uint8Array>;
  writable: WritableStream<Uint8Array>;
  opened: Promise<unknown>;
  closed: Promise<void>;
  close(): Promise<void>;
}>;

type Connect = (
  address: Readonly<{ hostname: string; port: number }>,
  options: Readonly<{ allowHalfOpen: boolean; secureTransport: "off" }>,
) => SocketLike;

export function validSshHostKeyTrust(value: unknown): value is SshHostKeyTrust {
  return typeof value === "string" && SSH_HOST_KEY_TRUST.test(value);
}

export function validateSshTarget(value: unknown): BrokeredSshTarget | undefined {
  if (!isRecord(value)) return undefined;
  const hostname = exactString(value.hostname), username = exactString(value.username);
  const hostKeySha256 = value.host_key_sha256, hostKeyTrust = value.host_key_trust, port = value.port;
  if (!hostname || canonicalSshHostname(hostname) !== hostname || !username || !SSH_USERNAME.test(username)
    || !Number.isInteger(port) || (port as number) < 1 || (port as number) > 65535
    || (hostKeySha256 !== undefined && (typeof hostKeySha256 !== "string" || !SSH_HOST_KEY.test(hostKeySha256)))
    || (hostKeyTrust !== undefined && !validSshHostKeyTrust(hostKeyTrust))
    // Neither a pin nor an explicit device binding would be trust on first use.
    || (hostKeySha256 === undefined && hostKeyTrust === undefined)) return undefined;
  return { hostname, username, port: port as number,
    ...(hostKeySha256 === undefined ? {} : { hostKeySha256: hostKeySha256 as string }),
    ...(hostKeyTrust === undefined ? {} : { hostKeyTrust: hostKeyTrust as SshHostKeyTrust }) };
}

export function validateSshIdentity(value: unknown): BrokeredSshIdentity | undefined {
  const target = validateSshTarget(value);
  if (!target || !isRecord(value)) return undefined;
  const privateKey = privateKeyString(value.private_key);
  if (!privateKey || privateKey.length < 64 || privateKey.length > 64 * 1024
    || privateKey.includes("\0") || !/-----BEGIN (?:RSA |EC )?PRIVATE KEY-----/u.test(privateKey)) return undefined;
  return { ...target, privateKey };
}

/** Parses the broker's resolution, which also carries its own saved_at stamp. */
export function validateResolvedSshIdentity(value: unknown): BrokeredSshIdentity | undefined {
  const identity = validateSshIdentity(value);
  if (!identity || !isRecord(value)) return undefined;
  const savedAt = value.saved_at;
  if (savedAt === undefined) return identity;
  if (!Number.isSafeInteger(savedAt) || (savedAt as number) < 0) return undefined;
  return { ...identity, savedAt: savedAt as number };
}

export function validSshIdentityReference(value: string): boolean {
  return SSH_IDENTITY_REFERENCE.test(value) && !RESERVED_REFERENCES.has(value);
}

export function validateBrokeredSshRequest(value: unknown): BrokeredSshRequest | undefined {
  if (!isRecord(value)) return undefined;
  // Exact fields only: in particular a caller can never inject host keys.
  if (Object.keys(value).some(key => !REQUEST_FIELDS.has(key))) return undefined;
  const identityReference = exactString(value.identity_ref);
  const hostname = exactString(value.hostname);
  const username = exactString(value.username);
  const port = value.port;
  const command = value.command;
  const stdin = value.stdin;
  const hostKeyTrust = value.host_key_trust;
  if (!identityReference || !validSshIdentityReference(identityReference)
    || !hostname || canonicalSshHostname(hostname) !== hostname
    || !username || !SSH_USERNAME.test(username)
    || !Number.isInteger(port) || (port as number) < 1 || (port as number) > 65_535
    || (hostKeyTrust !== undefined && hostKeyTrust !== "device")
    || !Array.isArray(command) || command.length < 1 || command.length > MAX_COMMAND_ARGUMENTS
    || (stdin !== undefined && (typeof stdin !== "string" || new TextEncoder().encode(stdin).byteLength > 64 * 1024))) {
    return undefined;
  }
  let bytes = 0;
  for (const argument of command) {
    if (typeof argument !== "string" || argument.includes("\0")) return undefined;
    bytes += new TextEncoder().encode(argument).byteLength;
    if (bytes > MAX_COMMAND_BYTES) return undefined;
  }
  return { identityReference, hostname, username, port: port as number, command,
    ...(hostKeyTrust === "device" ? { hostKeyTrust } : {}),
    ...(typeof stdin === "string" ? { stdin } : {}) };
}

/** Device trust applies on the per-call opt-in or an owner-saved binding. */
export function deviceTrustRequested(identity: BrokeredSshIdentity, request: BrokeredSshRequest): boolean {
  return request.hostKeyTrust === "device" || identity.hostKeyTrust !== undefined;
}

/** Parses the account DO's answer; anything malformed fails closed. */
export function parseDeviceHostKeyAttestation(value: unknown): DeviceHostKeyAttestation | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || Object.keys(value).length !== 3) return undefined;
  const { device_id: deviceId, created_at: createdAt, host_keys: hostKeys } = value;
  if (typeof deviceId !== "string" || !DEVICE_ID.test(deviceId)
    || !Number.isSafeInteger(createdAt) || (createdAt as number) <= 0
    || !Array.isArray(hostKeys) || hostKeys.length > MAX_DEVICE_HOST_KEYS) return undefined;
  const keys: { fingerprint: string; attestedAt: number }[] = [];
  for (const entry of hostKeys) {
    if (!isRecord(entry) || Object.keys(entry).length !== 2) return undefined;
    const { fingerprint, attested_at: attestedAt } = entry;
    if (typeof fingerprint !== "string" || !ATTESTED_HOST_KEY.test(fingerprint)
      || !Number.isSafeInteger(attestedAt) || (attestedAt as number) <= 0
      || keys.some(key => key.fingerprint === fingerprint)) return undefined;
    keys.push({ fingerprint, attestedAt: attestedAt as number });
  }
  return { deviceId, createdAt: createdAt as number, hostKeys: keys };
}

/**
 * Host keys this call accepts: the Vault pin, if saved, OR fingerprints the
 * bound machine's active device attested after this Vault target snapshot was
 * saved. A `device` (server Hand) binding also requires the device itself to
 * be enrolled after the snapshot, i.e. bootstrapped through this exact target.
 * Empty means the call fails closed before any connection: no TOFU.
 */
export function acceptedHostKeys(
  identity: BrokeredSshIdentity,
  request: BrokeredSshRequest,
  attestation: DeviceHostKeyAttestation | null = null,
): SshHostKeyMatch[] {
  const accepted: SshHostKeyMatch[] = identity.hostKeySha256 === undefined ? []
    : [{ source: "vault_pin", fingerprint: identity.hostKeySha256 }];
  const savedAt = identity.savedAt ?? 0;
  if (deviceTrustRequested(identity, request) && attestation
    && ((identity.hostKeyTrust ?? "device") !== "device" || attestation.createdAt > savedAt)) {
    for (const key of attestation.hostKeys) {
      if (key.attestedAt > savedAt && !accepted.some(match => sameFingerprint(match.fingerprint, key.fingerprint))) {
        accepted.push({ source: `device:${attestation.deviceId}`, fingerprint: key.fingerprint });
      }
    }
  }
  if (!accepted.length) {
    throw new BrokeredSshError(403, deviceTrustRequested(identity, request)
      ? "ssh_host_key_unattested" : "ssh_host_key_trust_required");
  }
  return accepted;
}

export async function executeBrokeredSsh(
  identity: BrokeredSshIdentity,
  request: BrokeredSshRequest,
  signal?: AbortSignal,
  connect: Connect = cloudflareConnect as Connect,
  attestation: DeviceHostKeyAttestation | null = null,
  onHostKeyMatch?: (match: SshHostKeyMatch) => void,
) {
  if (identity.hostname !== request.hostname || identity.port !== request.port
    || identity.username !== request.username) {
    throw new BrokeredSshError(403, "ssh_identity_target_mismatch");
  }
  // Decided before any socket opens. The Vault key stays the only client
  // authority; device attestations only widen which server key is accepted.
  const accepted = acceptedHostKeys(identity, request, attestation);
  const command = createSshCommand({
    transport: "tcp",
    maxOutputBytes: 4 * 1024 * 1024,
    onHostKeyAccepted(fingerprint) {
      const match = accepted.find(candidate => sameFingerprint(candidate.fingerprint, fingerprint));
      if (match) onHostKeyMatch?.(match);
    },
    async readIdentity(path) {
      if (path !== "brokered-identity") throw new Error("invalid brokered SSH identity path");
      return identity.privateKey;
    },
    async openStream(endpoint, commandSignal) {
      if (endpoint instanceof URL) throw new Error("brokered SSH requires TCP");
      const socket = connect(endpoint, { allowHalfOpen: true, secureTransport: "off" });
      try {
        await abortable(socket.opened, commandSignal);
        return createWebStreamSshStream(socket, commandSignal);
      } catch (error) {
        await socket.close();
        throw error;
      }
    },
  });
  return command.execute([
    "-p", String(identity.port),
    "-l", identity.username,
    "-i", "brokered-identity",
    ...accepted.flatMap(({ fingerprint }) => ["-o", `HostKeySHA256=${fingerprint}`]),
    identity.hostname,
    "--",
    ...request.command,
  ], { cwd: "/", stdin: request.stdin ?? "", signal: signal ?? new AbortController().signal });
}

function sameFingerprint(left: string, right: string): boolean {
  return left.replace(/=+$/u, "") === right.replace(/=+$/u, "");
}

export class BrokeredSshError extends Error {
  constructor(readonly status: number, readonly code: string) {
    super(code);
  }
}

function canonicalSshHostname(value: string): string | undefined {
  if (!value || value.length > 253 || value !== value.toLowerCase() || value.endsWith(".")
    || value.includes("@") || value.includes(":") || /\s/u.test(value)) return undefined;
  const ipv4 = /^(?:\d{1,3}\.){3}\d{1,3}$/u.test(value);
  const labels = value.split(".");
  const dns = value.includes(".") && labels.every((label) => (
    /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/u.test(label)
  ));
  if ((!ipv4 && !dns) || value === "localhost" || value.endsWith(".localhost")
    || value.endsWith(".internal") || value.endsWith(".invalid")
    || value.endsWith(".local") || value.endsWith(".test")
    || value.endsWith(".home.arpa") || deniedIp(value)) {
    return undefined;
  }
  return value;
}

function deniedIp(hostname: string): boolean {
  const ipv4 = hostname.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/u);
  if (ipv4) {
    const octets = ipv4.slice(1).map(Number);
    if (octets.some((value) => value > 255)) return true;
    const [a, b] = octets;
    return a === 0 || a === 10 || a === 127 || a >= 224
      || (a === 100 && b >= 64 && b <= 127)
      || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
      || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19));
  }
  return false;
}

function abortable<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (signal === undefined) return promise;
  if (signal.aborted) return Promise.reject(signal.reason ?? new Error("SSH command cancelled"));
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason ?? new Error("SSH command cancelled"));
    signal.addEventListener("abort", abort, { once: true });
    promise.then(
      (value) => { signal.removeEventListener("abort", abort); resolve(value); },
      (error) => { signal.removeEventListener("abort", abort); reject(error); },
    );
  });
}

function exactString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() === value ? value : undefined;
}

function privateKeyString(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const body = value.endsWith("\r\n")
    ? value.slice(0, -2)
    : value.endsWith("\n")
      ? value.slice(0, -1)
      : value;
  return body.trim() === body ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
