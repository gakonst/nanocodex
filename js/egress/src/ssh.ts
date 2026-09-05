import { connect as cloudflareConnect } from "cloudflare:sockets";
import { createSshCommand, createWebStreamSshStream } from "nanocodex/tools/ssh";

export const SSH_IDENTITY_REFERENCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const RESERVED_REFERENCES = new Set(["__proto__", "constructor", "prototype"]);
const SSH_USERNAME = /^[A-Za-z0-9._-]{1,128}$/;
const SSH_HOST_KEY = /^SHA256:[A-Za-z0-9+/]{43}=?$/;
const MAX_COMMAND_ARGUMENTS = 256;
const MAX_COMMAND_BYTES = 64 * 1024;

export type BrokeredSshIdentity = Readonly<{
  privateKey: string;
  hostname: string;
  port: number;
  username: string;
  hostKeySha256: string;
}>;

export type BrokeredSshRequest = Readonly<{
  identityReference: string;
  hostname: string;
  port: number;
  username: string;
  command: readonly string[];
}>;

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

export function validateSshIdentity(value: unknown): BrokeredSshIdentity | undefined {
  if (!isRecord(value)) return undefined;
  const privateKey = privateKeyString(value.private_key);
  const hostname = exactString(value.hostname);
  const username = exactString(value.username);
  const hostKeySha256 = exactString(value.host_key_sha256);
  const port = value.port;
  if (!privateKey || privateKey.length < 64 || privateKey.length > 64 * 1024
    || privateKey.includes("\0") || !/-----BEGIN (?:RSA |EC )?PRIVATE KEY-----/u.test(privateKey)
    || !hostname || canonicalSshHostname(hostname) !== hostname
    || !username || !SSH_USERNAME.test(username)
    || !Number.isInteger(port) || (port as number) < 1 || (port as number) > 65_535
    || !hostKeySha256 || !SSH_HOST_KEY.test(hostKeySha256)) {
    return undefined;
  }
  return { privateKey, hostname, username, port: port as number, hostKeySha256 };
}

export function validSshIdentityReference(value: string): boolean {
  return SSH_IDENTITY_REFERENCE.test(value) && !RESERVED_REFERENCES.has(value);
}

export function validateBrokeredSshRequest(value: unknown): BrokeredSshRequest | undefined {
  if (!isRecord(value)) return undefined;
  const identityReference = exactString(value.identity_ref);
  const hostname = exactString(value.hostname);
  const username = exactString(value.username);
  const port = value.port;
  const command = value.command;
  if (!identityReference || !validSshIdentityReference(identityReference)
    || !hostname || canonicalSshHostname(hostname) !== hostname
    || !username || !SSH_USERNAME.test(username)
    || !Number.isInteger(port) || (port as number) < 1 || (port as number) > 65_535
    || !Array.isArray(command) || command.length < 1 || command.length > MAX_COMMAND_ARGUMENTS) {
    return undefined;
  }
  let bytes = 0;
  for (const argument of command) {
    if (typeof argument !== "string" || argument.includes("\0")) return undefined;
    bytes += new TextEncoder().encode(argument).byteLength;
    if (bytes > MAX_COMMAND_BYTES) return undefined;
  }
  return { identityReference, hostname, username, port: port as number, command };
}

export async function executeBrokeredSsh(
  identity: BrokeredSshIdentity,
  request: BrokeredSshRequest,
  signal?: AbortSignal,
  connect: Connect = cloudflareConnect as Connect,
) {
  if (identity.hostname !== request.hostname || identity.port !== request.port
    || identity.username !== request.username) {
    throw new BrokeredSshError(403, "ssh_identity_target_mismatch");
  }
  const command = createSshCommand({
    transport: "tcp",
    maxOutputBytes: 4 * 1024 * 1024,
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
    "-o", `HostKeySHA256=${identity.hostKeySha256}`,
    identity.hostname,
    "--",
    ...request.command,
  ], { cwd: "/", stdin: "", signal: signal ?? new AbortController().signal });
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
