import { type HandRemoteBroker, type RemoteVMPublisher } from "./hand-remote";

export const HAND_HOST_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TOKEN = /^[A-Za-z0-9_-]{43}$/;
const PREFIX = "hand-host:";
const CREDENTIAL_LIFETIME = 90 * 24 * 60 * 60 * 1000;
const headers = { "cache-control": "no-store" };
type Host = { id: string; name: string; machineId?: string; tokenDigest: string; createdAt: number; expiresAt: number };

/** Revocable publisher credentials. They cannot read the account or view another Hand. */
export class HandHosts {
  constructor(private readonly storage: DurableObjectStorage, private readonly remote: HandRemoteBroker) {}

  async setupLock(request: Request, id: string): Promise<Response> {
    if (!HAND_HOST_ID.test(id) || !["POST", "DELETE"].includes(request.method)) return failure(400);
    let value: unknown;
    try { value = await boundedJSON(request); } catch { return failure(400); }
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 1
      || !("operation_id" in value) || typeof value.operation_id !== "string" || !HAND_HOST_ID.test(value.operation_id)) return failure(400);
    const operation = value.operation_id;
    const key = `hand-setup:${id}`;
    const acquired = await this.storage.transaction(async transaction => {
      const current = await transaction.get<{ operation: string; expiresAt: number }>(key);
      if (request.method === "DELETE") {
        if (current?.operation === operation) await transaction.delete(key);
        return true;
      }
      if (current && current.expiresAt > Date.now()) return false;
      await transaction.put(key, { operation, expiresAt: Date.now() + 120_000 });
      return true;
    });
    return acquired ? new Response(null, { status: 204 }) : failure(409);
  }

  async manage(request: Request, id?: string, machineId = `server:${id}`): Promise<Response> {
    if (new URL(request.url).search || (id !== undefined && !HAND_HOST_ID.test(id))) return failure(400);
    if (request.method === "GET" && id === undefined) {
      const records = await this.storage.list<Host>({ prefix: PREFIX });
      return Response.json({ data: [...records.values()].map(metadata) }, { headers });
    }
    if (id === undefined) return failure(405);
    if (request.method === "DELETE") {
      await this.storage.delete(PREFIX + id);
      this.remote.revokePublisher(PREFIX + id);
      return new Response(null, { status: 204, headers });
    }
    if (request.method !== "PUT") return failure(405);
    let body: unknown;
    try { body = await boundedJSON(request); } catch { return failure(400); }
    if (!body || typeof body !== "object" || Array.isArray(body)
      || Object.keys(body).length !== 1 || !("name" in body)
      || typeof body.name !== "string" || !body.name.trim()
      || /[\u0000-\u001f\u007f]/u.test(body.name)
      || new TextEncoder().encode(body.name).length > 128) return failure(400);
    const token = base64url(crypto.getRandomValues(new Uint8Array(32)));
    const tokenDigest = await digest(token);
    const now = Date.now();
    const name = body.name.trim();
    const result = await this.storage.transaction(async transaction => {
      const existing = await transaction.get<Host>(PREFIX + id);
      if (!existing && (await transaction.list({ prefix: PREFIX, limit: 64 })).size >= 64) return undefined;
      const record: Host = { id, name, machineId, tokenDigest, createdAt: existing?.createdAt ?? now,
        expiresAt: now + CREDENTIAL_LIFETIME };
      await transaction.put(PREFIX + id, record);
      return record;
    });
    if (!result) return failure(429);
    this.remote.revokePublisher(PREFIX + id);
    return Response.json({ ...metadata(result), credential: token }, { status: 201, headers });
  }

  async authorize(request: Request, id: string): Promise<RemoteVMPublisher | undefined> {
    const token = request.headers.get("authorization")?.match(/^Bearer ([A-Za-z0-9_-]{43})$/)?.[1];
    if (!HAND_HOST_ID.test(id) || !token || !TOKEN.test(token)) return undefined;
    const tokenDigest = await digest(token);
    const record = await this.storage.get<Host>(PREFIX + id);
    if (!record || record.tokenDigest !== tokenDigest || record.expiresAt <= Date.now()) return undefined;
    return { machineId: record.machineId ?? `server:${id}`, routeId: PREFIX + id, expiresAt: record.expiresAt, surfaceKind: "desktop" };
  }
}

function metadata(record: Host) {
  return { id: record.id, machine_id: record.machineId ?? `server:${record.id}`, name: record.name,
    created_at: record.createdAt, expires_at: record.expiresAt };
}

export async function boundedJSON(request: Request): Promise<unknown> {
  const reader = request.body?.getReader();
  if (!reader) throw new Error("body required");
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 1024) throw new Error("body too large");
      chunks.push(value);
    }
  } finally { await reader.cancel(); reader.releaseLock(); }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
}

function failure(status: number) { return Response.json({ error: "invalid_hand_host_request" }, { status, headers }); }
async function digest(token: string) { return base64url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)))); }
function base64url(bytes: Uint8Array) { return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, ""); }

export async function serverHandID(owner: string, reference: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`server-hand:v1\0${owner}\0${reference}`))).slice(0, 16);
  bytes[6] = (bytes[6]! & 15) | 64; bytes[8] = (bytes[8]! & 63) | 128;
  const hex = [...bytes].map(byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
