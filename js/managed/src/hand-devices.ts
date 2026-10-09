/**
 * Hand device identity: Ed25519 device keys enrolled by a live account
 * principal (or a one-time owner-minted server grant), stateless MAC'd
 * proof-of-possession challenges, and short-lived digest-only publisher
 * credentials. A device credential grants only Hand publication for its
 * enrolled machine_id; it never authenticates account routes.
 */

export const HAND_DEVICE_CREDENTIAL_PREFIX = "ncxhd1.";
export const HAND_DEVICE_GRANT_PREFIX = "ncxhg1.";
const DOMAIN = "nanocodex-hand-device:v1";
const UUID_V4_SOURCE = "[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}";
const UUID_V4 = new RegExp("^" + UUID_V4_SOURCE + "$");
const CREDENTIAL = new RegExp("^ncxhd1\\.(" + UUID_V4_SOURCE + ")\\.(" + UUID_V4_SOURCE + ")\\.([A-Za-z0-9_-]{43})$");
const GRANT = new RegExp("^ncxhg1\\.(" + UUID_V4_SOURCE + ")\\.(" + UUID_V4_SOURCE + ")\\.([A-Za-z0-9_-]{43})$");
const CHALLENGE = /^[A-Za-z0-9_-]{54}$/;
const SIGNATURE = /^[A-Za-z0-9_-]{86}$/;
const MACHINE_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const RESERVED_MACHINE = /^(?:server|cf|shared|vm|screen):/i;
const FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}$/;
const ENROLL_CHALLENGE_MS = 120_000;
const DEVICE_CHALLENGE_MS = 60_000;
const GRANT_MS = 10 * 60_000;
const MAX_CREDENTIALS = 4;
const RATE_WINDOW_MS = 60_000;
const ENROLL_RATE = 30;
const DEVICE_RATE = 30;
const MAX_HOST_KEYS = 8;
export const DEFAULT_HAND_DEVICE_CREDENTIAL_TTL_SECONDS = 900;

const K = {
  macKey: "hdev:mac-key",
  policy: "hdev:policy",
  record: (id: string) => "hdev:rec:" + id,
  recordPrefix: "hdev:rec:",
  machine: (machineId: string) => "hdev:mach:" + machineId,
  key: (publicKey: string) => "hdev:key:" + publicKey,
  used: (nonce: string) => "hdev:used:" + nonce,
  usedPrefix: "hdev:used:",
  credential: (deviceId: string, digest: string) => "hdev:cred:" + deviceId + ":" + digest,
  credentialPrefix: (deviceId: string) => "hdev:cred:" + deviceId + ":",
  grant: (hostId: string) => "hdev:grant:" + hostId,
  legacy: (machineId: string) => "hdev:legacy:" + machineId,
  legacyPrefix: "hdev:legacy:",
};

export type HandDevicePurpose = "credential" | "rotate" | "ssh-host-keys";
type CredentialRecord = { key_version: number; created_at: number; expires_at: number };
/** Permanent once written: the machine requires device credentials forever. */
type MachineRecord = { active?: string; devices: string[] };
type KeyRecord = { device_id: string };
type GrantRecord = { digest: string; machine_id: string; expires_at: number };
export type HandDeviceEnrolledBy = { kind: string; organization_id?: string; team_id?: string; authorization_epoch?: number };
export type HandDeviceRecord = {
  id: string; machine_id: string; name: string; algorithm: "ed25519"; public_key: string; fingerprint: string;
  key_version: number; status: "active" | "revoked"; created_at: number; rotated_at: number | null;
  last_authenticated_at: number | null; revoked_at: number | null; enrolled_by: HandDeviceEnrolledBy;
  ssh_host_keys: { fingerprint: string; attested_at: number }[];
};
type LegacyRecord = { machine_id: string; runtime_id: string | null; auth: "account_api_key"; connected_at: number };
export type HandDeviceResult = { status: number; body: unknown };
export type HandDeviceAuthorization = Readonly<{ device: HandDeviceRecord; expiresAt: number }>;
export type HandDeviceAccount = Readonly<{ organization_id?: string }>;
type ParsedCredential = { ownerId: string; deviceId: string; secret: string };

type Kv = Pick<SyncKvStorage, "get" | "put" | "delete" | "list">;

const result = (status: number, body: unknown): HandDeviceResult => ({ status, body });
const failure = (status: number, code: string) => result(status, { error: code });

/** Any bearer in the device-credential namespace, valid or not. */
export function isHandDeviceAuthorization(header: string | null): boolean {
  return header !== null && /^Bearer\s+ncxh[dg]1\./i.test(header);
}

export function parseHandDeviceCredential(header: string | null): ParsedCredential | undefined {
  const match = CREDENTIAL.exec(header?.match(/^Bearer (ncxhd1\.\S+)$/)?.[1] ?? "");
  return match ? { ownerId: match[1]!, deviceId: match[2]!, secret: match[3]! } : undefined;
}

export function parseHandDeviceGrant(header: string | null): { ownerId: string; hostId: string; secret: string } | undefined {
  const match = GRANT.exec(header?.match(/^Bearer (ncxhg1\.\S+)$/)?.[1] ?? "");
  return match ? { ownerId: match[1]!, hostId: match[2]!, secret: match[3]! } : undefined;
}

export function handDeviceCredentialTtlMs(value: unknown): number {
  const parsed = typeof value === "string" && /^[0-9]{1,6}$/.test(value) ? Number(value) : DEFAULT_HAND_DEVICE_CREDENTIAL_TTL_SECONDS;
  return Math.min(900, Math.max(5, parsed)) * 1000;
}

export function validHandDeviceId(value: unknown): value is string { return typeof value === "string" && UUID_V4.test(value); }
export function reservedHandDeviceMachine(machineId: string): boolean { return RESERVED_MACHINE.test(machineId); }

/** Signed message: UTF-8 fields joined by LF with no trailing newline. */
export function handDeviceMessage(fields: readonly (string | number)[]): Uint8Array {
  return new TextEncoder().encode([DOMAIN, ...fields.map(String)].join("\n"));
}

function decode(value: string, length: number): Uint8Array | undefined {
  try {
    const binary = atob(value.replaceAll("-", "+").replaceAll("_", "/"));
    if (binary.length !== length) return undefined;
    const decoded = Uint8Array.from(binary, char => char.charCodeAt(0));
    // Canonical encoding only: one string per byte sequence.
    return base64url(decoded) === value ? decoded : undefined;
  } catch { return undefined; }
}

function base64url(value: Uint8Array): string {
  return btoa(String.fromCharCode(...value)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function sha256(value: Uint8Array | string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", typeof value === "string" ? new TextEncoder().encode(value) : value));
}

export async function handDeviceGrantDigest(secret: string): Promise<string> { return base64url(await sha256(secret)); }

const P = (1n << 255n) - 19n;
// y-coordinates (sign bit cleared) of the small-order points of edwards25519.
const SMALL_ORDER_Y = new Set([0n, 1n, P - 1n,
  0x7a03ac9277fdc74ec6cc392cfa53202a0f67100d760b3cba4fd84d3d706a17c7n,
  0x05fc536d880238b13933c6d305acdfd5f098eff289f4c345b027b2c28f95e826n]);

/** Canonical (y < p), not a small-order point, 32 raw bytes. */
export function validHandDevicePublicKey(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const raw = decode(value, 32);
  if (!raw) return false;
  let y = 0n;
  for (let index = 31; index >= 0; index--) y = (y << 8n) | BigInt(index === 31 ? raw[index]! & 0x7f : raw[index]!);
  return y < P && !SMALL_ORDER_Y.has(y);
}

export async function handDeviceFingerprint(publicKey: string): Promise<string> {
  return "SHA256:" + btoa(String.fromCharCode(...await sha256(decode(publicKey, 32)!))).replace(/=+$/, "");
}

async function verify(publicKey: string, signature: string, message: Uint8Array): Promise<boolean> {
  const key = decode(publicKey, 32), sig = decode(signature, 64);
  if (!key || !sig) return false;
  try {
    const imported = await crypto.subtle.importKey("raw", key, { name: "Ed25519" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "Ed25519" }, imported, sig, message);
  } catch { return false; }
}

function exactObject(value: unknown, keys: readonly string[], optional: readonly string[] = []): value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const present = Object.keys(value);
  return keys.every(key => Object.hasOwn(value, key)) && present.every(key => keys.includes(key) || optional.includes(key));
}

function validName(value: unknown): value is string {
  return typeof value === "string" && !!value.trim() && !/[\u0000-\u001f\u007f]/u.test(value)
    && new TextEncoder().encode(value.trim()).length <= 128;
}

function timingSafeEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index++) difference |= left[index]! ^ right[index]!;
  return difference === 0;
}

/** Public projection: never key material beyond its fingerprint, never digests, tokens or challenges. */
export function handDeviceJSON(record: HandDeviceRecord) {
  return { id: record.id, machine_id: record.machine_id, name: record.name, algorithm: record.algorithm,
    fingerprint: record.fingerprint, key_version: record.key_version, status: record.status,
    created_at: record.created_at, rotated_at: record.rotated_at, last_authenticated_at: record.last_authenticated_at,
    revoked_at: record.revoked_at, enrolled_by: { kind: record.enrolled_by.kind },
    ssh_host_keys: record.ssh_host_keys.map(entry => ({ fingerprint: entry.fingerprint, attested_at: entry.attested_at })) };
}

export class HandDevices {
  #mac?: Promise<CryptoKey>;

  constructor(
    private readonly kv: Kv,
    private readonly transaction: <T>(callback: () => T) => T,
    private readonly credentialTtlMs: number,
    private readonly now: () => number = Date.now,
  ) {}

  #list<T>(prefix: string): [string, T][] { return [...this.kv.list<T>({ prefix })]; }

  /** Fixed-window issuance limit per scope; exceeding it never consumes state. */
  #allow(scope: string, limit: number): boolean {
    const now = this.now(), window = Math.floor(now / RATE_WINDOW_MS);
    return this.transaction(() => {
      const key = "hdev:rate:" + scope;
      const current = this.kv.get<{ window: number; count: number }>(key);
      const count = current?.window === window ? current.count : 0;
      if (count >= limit) return false;
      this.kv.put(key, { window, count: count + 1 });
      return true;
    });
  }

  #macKey(): Promise<CryptoKey> {
    return this.#mac ??= (async () => {
      const raw = this.transaction(() => {
        let secret = this.kv.get<string>(K.macKey);
        if (!secret) { secret = base64url(crypto.getRandomValues(new Uint8Array(32))); this.kv.put(K.macKey, secret); }
        return secret;
      });
      return crypto.subtle.importKey("raw", decode(raw, 32)!, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    })();
  }

  async #tag(binding: string, nonce: Uint8Array, expiresAt: number): Promise<Uint8Array> {
    const input = new TextEncoder().encode(binding + "\n" + base64url(nonce) + "\n" + expiresAt);
    return new Uint8Array(await crypto.subtle.sign("HMAC", await this.#macKey(), input)).slice(0, 16);
  }

  /** challenge = base64url(random16 || expires_at u64 BE ms || HMAC(binding)[0..16]); no server table. */
  async #newChallenge(binding: string, ttl: number): Promise<{ challenge: string; expiresAt: number }> {
    const nonce = crypto.getRandomValues(new Uint8Array(16));
    const expiresAt = this.now() + ttl;
    const packed = new Uint8Array(40);
    packed.set(nonce, 0);
    new DataView(packed.buffer).setBigUint64(16, BigInt(expiresAt));
    packed.set(await this.#tag(binding, nonce, expiresAt), 24);
    return { challenge: base64url(packed), expiresAt };
  }

  /**
   * Authenticity check, then a synchronous check-and-insert into the used set
   * before any signature verification: a presented challenge is burned even
   * when the signature later fails.
   */
  async #consume(binding: string, challenge: unknown): Promise<boolean> {
    if (typeof challenge !== "string" || !CHALLENGE.test(challenge)) return false;
    const packed = decode(challenge, 40);
    if (!packed) return false;
    const nonce = packed.slice(0, 16);
    const expiresAt = Number(new DataView(packed.buffer).getBigUint64(16));
    if (!Number.isSafeInteger(expiresAt) || expiresAt <= this.now()) return false;
    if (!timingSafeEqual(packed.slice(24), await this.#tag(binding, nonce, expiresAt))) return false;
    const now = this.now();
    return this.transaction(() => {
      if (expiresAt <= now) return false;
      const key = K.used(base64url(nonce));
      if (this.kv.get(key) !== undefined) return false;
      this.kv.put(key, expiresAt);
      let pruned = 0;
      for (const [used, until] of this.kv.list<number>({ prefix: K.usedPrefix, limit: 64 })) {
        if (until <= now) { this.kv.delete(used); pruned++; }
      }
      void pruned;
      return true;
    });
  }

  async enrollChallenge(ownerId: string): Promise<HandDeviceResult> {
    if (!this.#allow("enroll", ENROLL_RATE)) return failure(429, "rate_limited");
    const { challenge, expiresAt } = await this.#newChallenge(["enroll", ownerId].join("\n"), ENROLL_CHALLENGE_MS);
    return result(201, { challenge, owner_id: ownerId, expires_at: expiresAt });
  }

  #install(machineId: string, publicKey: string, name: string, fingerprint: string, enrolledBy: HandDeviceEnrolledBy,
    replaceActive: boolean): HandDeviceResult & { replaced?: HandDeviceRecord } {
    const now = this.now();
    const machine = this.kv.get<MachineRecord>(K.machine(machineId)) ?? { devices: [] };
    const active = machine.active ? this.kv.get<HandDeviceRecord>(K.record(machine.active)) : undefined;
    if (active?.status === "active" && active.public_key === publicKey) return result(200, handDeviceJSON(active));
    if (active?.status === "active" && !replaceActive) return failure(409, "hand_device_exists");
    // A key is single-use for life: revoked or rotated-away keys never re-enroll.
    if (this.kv.get<KeyRecord>(K.key(publicKey))) return failure(409, "hand_device_key_in_use");
    let replaced: HandDeviceRecord | undefined;
    if (active?.status === "active") {
      replaced = { ...active, status: "revoked", revoked_at: now };
      for (const [key] of this.#list(K.credentialPrefix(active.id))) this.kv.delete(key);
      this.kv.put(K.record(active.id), replaced);
    }
    const record: HandDeviceRecord = { id: crypto.randomUUID(), machine_id: machineId, name, algorithm: "ed25519",
      public_key: publicKey, fingerprint, key_version: 1, status: "active", created_at: now, rotated_at: null,
      last_authenticated_at: null, revoked_at: null, enrolled_by: { ...enrolledBy }, ssh_host_keys: [] };
    this.kv.put(K.record(record.id), record);
    this.kv.put(K.key(publicKey), { device_id: record.id } satisfies KeyRecord);
    this.kv.put(K.machine(machineId), { active: record.id, devices: [...machine.devices, record.id] } satisfies MachineRecord);
    this.kv.delete(K.legacy(machineId));
    return { ...result(201, handDeviceJSON(record)), ...(replaced ? { replaced } : {}) };
  }

  /** Account-principal enrollment. The caller has already rejected machines published by other publisher kinds. */
  async enroll(ownerId: string, origin: string, body: unknown, enrolledBy: HandDeviceEnrolledBy): Promise<HandDeviceResult> {
    if (!exactObject(body, ["machine_id", "name", "algorithm", "public_key", "challenge", "signature"])
      || typeof body.machine_id !== "string" || !MACHINE_ID.test(body.machine_id) || !validName(body.name)
      || body.algorithm !== "ed25519" || typeof body.challenge !== "string"
      || typeof body.signature !== "string" || !SIGNATURE.test(body.signature)) return failure(400, "invalid_hand_device_request");
    if (reservedHandDeviceMachine(body.machine_id)) return failure(400, "hand_device_machine_reserved");
    if (!validHandDevicePublicKey(body.public_key)) return failure(400, "invalid_hand_device_key");
    const machineId = body.machine_id, publicKey = body.public_key, name = body.name.trim();
    if (!await this.#consume(["enroll", ownerId].join("\n"), body.challenge)) return failure(401, "challenge_invalid");
    const message = handDeviceMessage(["enroll", origin, ownerId, body.challenge, machineId, publicKey]);
    if (!await verify(publicKey, body.signature, message)) return failure(401, "signature_invalid");
    const fingerprint = await handDeviceFingerprint(publicKey);
    return this.transaction(() => this.#install(machineId, publicKey, name, fingerprint, enrolledBy, false));
  }

  /** One-time server bootstrap grant bound to a HandHosts record's machine. */
  async mintGrant(ownerId: string, hostId: string, machineId: string): Promise<{ grant: string; expires_at: number }> {
    const secret = base64url(crypto.getRandomValues(new Uint8Array(32)));
    const digest = await handDeviceGrantDigest(secret);
    const expiresAt = this.now() + GRANT_MS;
    this.transaction(() => this.kv.put(K.grant(hostId), { digest, machine_id: machineId, expires_at: expiresAt } satisfies GrantRecord));
    return { grant: HAND_DEVICE_GRANT_PREFIX + ownerId + "." + hostId + "." + secret, expires_at: expiresAt };
  }

  revokeGrant(hostId: string): void { this.transaction(() => this.kv.delete(K.grant(hostId))); }

  async enrollWithGrant(ownerId: string, origin: string, hostId: string, header: string | null, body: unknown,
    machineId: string): Promise<HandDeviceResult & { replaced?: HandDeviceRecord }> {
    const grant = parseHandDeviceGrant(header);
    if (!grant || grant.ownerId !== ownerId || grant.hostId !== hostId) return failure(401, "unauthorized");
    if (!this.#allow("grant:" + hostId, ENROLL_RATE)) return failure(429, "rate_limited");
    if (!exactObject(body, ["algorithm", "public_key", "signature"], ["name"]) || body.algorithm !== "ed25519"
      || typeof body.signature !== "string" || !SIGNATURE.test(body.signature)
      || (body.name !== undefined && !validName(body.name))) return failure(400, "invalid_hand_device_request");
    if (!validHandDevicePublicKey(body.public_key)) return failure(400, "invalid_hand_device_key");
    const publicKey = body.public_key;
    const digest = await handDeviceGrantDigest(grant.secret);
    const now = this.now();
    // Consume before verifying: a presented grant is single use even when its signature fails.
    const consumed = this.transaction(() => {
      const record = this.kv.get<GrantRecord>(K.grant(hostId));
      if (!record || !timingSafeEqual(new TextEncoder().encode(record.digest), new TextEncoder().encode(digest))) return false;
      this.kv.delete(K.grant(hostId));
      return record.expires_at > now && record.machine_id === machineId;
    });
    if (!consumed) return failure(401, "unauthorized");
    const message = handDeviceMessage(["enroll", origin, ownerId, digest, machineId, publicKey]);
    if (!await verify(publicKey, body.signature, message)) return failure(401, "signature_invalid");
    const fingerprint = await handDeviceFingerprint(publicKey);
    const name = typeof body.name === "string" ? body.name.trim() : machineId;
    return this.transaction(() => this.#install(machineId, publicKey, name, fingerprint, { kind: "server_grant" }, true));
  }

  policy(): { require_device_keys: boolean } {
    return { require_device_keys: this.kv.get<boolean>(K.policy) === true };
  }

  setPolicy(requireDeviceKeys: boolean): void { this.transaction(() => this.kv.put(K.policy, requireDeviceKeys)); }

  list(): HandDeviceResult {
    const devices = this.#list<HandDeviceRecord>(K.recordPrefix).map(([, record]) => record)
      .sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
    const legacy = this.#list<LegacyRecord>(K.legacyPrefix).map(([, record]) => record)
      .filter(record => !this.machineBound(record.machine_id))
      .sort((a, b) => a.machine_id.localeCompare(b.machine_id))
      .map(record => ({ machine_id: record.machine_id, runtime_id: record.runtime_id, auth: "account_api_key" as const, connected_at: record.connected_at }));
    return result(200, { data: devices.map(handDeviceJSON), legacy, policy: this.policy() });
  }

  get(deviceId: string): HandDeviceRecord | undefined {
    return validHandDeviceId(deviceId) ? this.kv.get<HandDeviceRecord>(K.record(deviceId)) : undefined;
  }

  /** One transaction: revoked and credentials deleted; the machine stays device-bound forever. */
  revoke(deviceId: string): { record: HandDeviceRecord; changed: boolean } | undefined {
    if (!validHandDeviceId(deviceId)) return undefined;
    const now = this.now();
    return this.transaction(() => {
      const record = this.kv.get<HandDeviceRecord>(K.record(deviceId));
      if (!record) return undefined;
      for (const [key] of this.#list(K.credentialPrefix(deviceId))) this.kv.delete(key);
      const machine = this.kv.get<MachineRecord>(K.machine(record.machine_id)) ?? { devices: [deviceId] };
      if (machine.active === deviceId) delete machine.active;
      this.kv.put(K.machine(record.machine_id), machine);
      if (record.status === "revoked") return { record, changed: false };
      const revoked: HandDeviceRecord = { ...record, status: "revoked", revoked_at: now };
      this.kv.put(K.record(deviceId), revoked);
      return { record: revoked, changed: true };
    });
  }

  async deviceChallenge(ownerId: string, deviceId: string, body: unknown): Promise<HandDeviceResult> {
    if (!exactObject(body, ["purpose"]) || !["credential", "rotate", "ssh-host-keys"].includes(body.purpose as string)) {
      return failure(400, "invalid_hand_device_request");
    }
    const device = this.get(deviceId);
    if (!device || device.status !== "active") return failure(401, "hand_reenroll_required");
    if (!this.#allow(deviceId, DEVICE_RATE)) return failure(429, "rate_limited");
    const { challenge, expiresAt } = await this.#newChallenge(
      [body.purpose as string, ownerId, deviceId, device.key_version].join("\n"), DEVICE_CHALLENGE_MS);
    return result(201, { challenge, key_version: device.key_version, fingerprint: device.fingerprint, expires_at: expiresAt });
  }

  async #prove(ownerId: string, origin: string, deviceId: string, purpose: HandDevicePurpose, challenge: unknown,
    signature: unknown, extra: readonly string[], additional?: { publicKey: string; signature: string },
  ): Promise<{ failure: HandDeviceResult } | { device: HandDeviceRecord }> {
    if (typeof challenge !== "string" || !CHALLENGE.test(challenge) || typeof signature !== "string" || !SIGNATURE.test(signature)) {
      return { failure: failure(400, "invalid_hand_device_request") } as const;
    }
    const device = this.get(deviceId);
    if (!device || device.status !== "active") return { failure: failure(401, "hand_reenroll_required") } as const;
    if (!await this.#consume([purpose, ownerId, deviceId, device.key_version].join("\n"), challenge)) {
      return { failure: failure(401, "challenge_invalid") } as const;
    }
    const message = handDeviceMessage([purpose, origin, ownerId, deviceId, device.key_version, challenge, ...extra]);
    if (!await verify(device.public_key, signature, message)) return { failure: failure(401, "signature_invalid") } as const;
    if (additional && !await verify(additional.publicKey, additional.signature, message)) {
      return { failure: failure(401, "signature_invalid") } as const;
    }
    return { device } as const;
  }

  /** Revocation or rotation may have committed while a signature was checked. */
  #current(proved: HandDeviceRecord): { current: HandDeviceRecord } | { failure: HandDeviceResult } {
    const current = this.kv.get<HandDeviceRecord>(K.record(proved.id));
    if (!current || current.status !== "active") return { failure: failure(401, "hand_reenroll_required") };
    if (current.key_version !== proved.key_version || current.public_key !== proved.public_key) return { failure: failure(401, "signature_invalid") };
    return { current };
  }

  async credential(ownerId: string, origin: string, deviceId: string, body: unknown, account: HandDeviceAccount): Promise<HandDeviceResult> {
    if (!exactObject(body, ["challenge", "signature"])) return failure(400, "invalid_hand_device_request");
    const proof = await this.#prove(ownerId, origin, deviceId, "credential", body.challenge, body.signature, []);
    if ("failure" in proof) return proof.failure;
    if (proof.device.enrolled_by.organization_id !== undefined && account.organization_id !== proof.device.enrolled_by.organization_id) {
      return failure(403, "hand_device_account_unauthorized");
    }
    const secret = base64url(crypto.getRandomValues(new Uint8Array(32)));
    const digest = base64url(await sha256(secret));
    const now = this.now(), expiresAt = now + this.credentialTtlMs;
    return this.transaction(() => {
      const checked = this.#current(proof.device);
      if ("failure" in checked) return checked.failure;
      const current = checked.current;
      const live: [string, CredentialRecord][] = [];
      for (const [key, value] of this.#list<CredentialRecord>(K.credentialPrefix(deviceId))) {
        if (value.expires_at <= now || value.key_version !== current.key_version) this.kv.delete(key); else live.push([key, value]);
      }
      live.sort((a, b) => a[1].created_at - b[1].created_at);
      while (live.length >= MAX_CREDENTIALS) this.kv.delete(live.shift()![0]);
      this.kv.put(K.credential(deviceId, digest), { key_version: current.key_version, created_at: now, expires_at: expiresAt } satisfies CredentialRecord);
      this.kv.put(K.record(deviceId), { ...current, last_authenticated_at: now });
      return result(201, { credential: HAND_DEVICE_CREDENTIAL_PREFIX + ownerId + "." + deviceId + "." + secret,
        expires_at: expiresAt, key_version: current.key_version });
    });
  }

  async rotate(ownerId: string, origin: string, deviceId: string, body: unknown): Promise<HandDeviceResult & { rotated?: HandDeviceRecord }> {
    if (!exactObject(body, ["challenge", "signature", "new_public_key", "new_signature"])
      || typeof body.new_signature !== "string" || !SIGNATURE.test(body.new_signature)) return failure(400, "invalid_hand_device_request");
    if (!validHandDevicePublicKey(body.new_public_key)) return failure(400, "invalid_hand_device_key");
    const newKey = body.new_public_key;
    const proof = await this.#prove(ownerId, origin, deviceId, "rotate", body.challenge, body.signature, [newKey],
      { publicKey: newKey, signature: body.new_signature });
    if ("failure" in proof) return proof.failure;
    const fingerprint = await handDeviceFingerprint(newKey);
    const now = this.now();
    return this.transaction(() => {
      const checked = this.#current(proof.device);
      if ("failure" in checked) return checked.failure;
      const current = checked.current;
      if (this.kv.get<KeyRecord>(K.key(newKey))) return failure(409, "hand_device_key_in_use");
      // Attestations were made by the old key; the device must re-attest.
      const rotated: HandDeviceRecord = { ...current, public_key: newKey, fingerprint, key_version: current.key_version + 1,
        rotated_at: now, ssh_host_keys: [] };
      for (const [key] of this.#list(K.credentialPrefix(deviceId))) this.kv.delete(key);
      this.kv.put(K.key(newKey), { device_id: deviceId } satisfies KeyRecord);
      this.kv.put(K.record(deviceId), rotated);
      return { ...result(200, handDeviceJSON(rotated)), rotated };
    });
  }

  async sshHostKeys(ownerId: string, origin: string, deviceId: string, body: unknown): Promise<HandDeviceResult> {
    if (!exactObject(body, ["challenge", "signature", "fingerprints"]) || !Array.isArray(body.fingerprints)
      || body.fingerprints.length > MAX_HOST_KEYS
      || !body.fingerprints.every(value => typeof value === "string" && FINGERPRINT.test(value))
      || new Set(body.fingerprints).size !== body.fingerprints.length) return failure(400, "invalid_hand_device_request");
    const fingerprints = [...body.fingerprints as string[]].sort();
    const proof = await this.#prove(ownerId, origin, deviceId, "ssh-host-keys", body.challenge, body.signature, [fingerprints.join(",")]);
    if ("failure" in proof) return proof.failure;
    const now = this.now();
    return this.transaction(() => {
      const checked = this.#current(proof.device);
      if ("failure" in checked) return checked.failure;
      const current = checked.current;
      const updated: HandDeviceRecord = { ...current, ssh_host_keys: fingerprints.map(fingerprint => ({ fingerprint, attested_at: now })) };
      this.kv.put(K.record(deviceId), updated);
      return result(200, handDeviceJSON(updated));
    });
  }

  /** Asynchronous half of authorization: hash outside any transaction. */
  async credentialDigest(header: string | null): Promise<{ parsed: ParsedCredential; digest: string } | undefined> {
    const parsed = parseHandDeviceCredential(header);
    return parsed ? { parsed, digest: base64url(await sha256(parsed.secret)) } : undefined;
  }

  /** Synchronous re-check against the device row; callers accept sockets with no await after this. */
  authorizeDigest(ownerId: string, presented: { parsed: ParsedCredential; digest: string } | undefined): HandDeviceAuthorization | undefined {
    if (!presented || presented.parsed.ownerId !== ownerId) return undefined;
    const { parsed, digest } = presented;
    const now = this.now();
    return this.transaction(() => {
      const credential = this.kv.get<CredentialRecord>(K.credential(parsed.deviceId, digest));
      if (!credential) return undefined;
      const device = this.kv.get<HandDeviceRecord>(K.record(parsed.deviceId));
      if (credential.expires_at <= now || !device || device.status !== "active" || device.key_version !== credential.key_version) {
        this.kv.delete(K.credential(parsed.deviceId, digest));
        return undefined;
      }
      const updated = { ...device, last_authenticated_at: now };
      this.kv.put(K.record(device.id), updated);
      return { device: updated, expiresAt: credential.expires_at };
    });
  }

  async authorize(ownerId: string, header: string | null): Promise<HandDeviceAuthorization | undefined> {
    return this.authorizeDigest(ownerId, await this.credentialDigest(header));
  }

  /** Downgrade fence: any device record ever, or the account policy, requires device credentials. */
  machineBound(machineId: string): boolean { return this.kv.get<MachineRecord>(K.machine(machineId)) !== undefined; }
  deviceRequired(machineId: string): boolean { return this.machineBound(machineId) || this.policy().require_device_keys; }

  activeDevice(machineId: string): HandDeviceRecord | undefined {
    const machine = this.kv.get<MachineRecord>(K.machine(machineId));
    const device = machine?.active ? this.kv.get<HandDeviceRecord>(K.record(machine.active)) : undefined;
    return device?.status === "active" && device.machine_id === machineId ? device : undefined;
  }

  activeSshHostKeys(machineId: string): { device_id: string; created_at: number; key_version: number; host_keys: { fingerprint: string; attested_at: number }[] } | null {
    const device = this.activeDevice(machineId);
    return device ? { device_id: device.id, created_at: device.created_at, key_version: device.key_version,
      host_keys: device.ssh_host_keys.map(entry => ({ fingerprint: entry.fingerprint, attested_at: entry.attested_at })) } : null;
  }

  recordLegacy(machineId: string, runtimeId: string | undefined): void {
    if (!MACHINE_ID.test(machineId) || this.machineBound(machineId)) return;
    this.kv.put(K.legacy(machineId), { machine_id: machineId, runtime_id: runtimeId ?? null, auth: "account_api_key", connected_at: this.now() } satisfies LegacyRecord);
  }

  legacy(machineId: string): boolean { return this.kv.get(K.legacy(machineId)) !== undefined; }

  /**
   * Owner forget: revokes the active device and removes device listings, but
   * the machine's device-required mark is permanent. Returns every device ID
   * whose live sockets must be closed.
   */
  forgetMachine(machineId: string): string[] {
    return this.transaction(() => {
      const machine = this.kv.get<MachineRecord>(K.machine(machineId));
      this.kv.delete(K.legacy(machineId));
      if (!machine) return [];
      for (const id of machine.devices) {
        for (const [key] of this.#list(K.credentialPrefix(id))) this.kv.delete(key);
        this.kv.delete(K.record(id));
      }
      this.kv.put(K.machine(machineId), { devices: [] } satisfies MachineRecord);
      return [...machine.devices];
    });
  }
}
