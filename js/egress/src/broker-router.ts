import type { UserCredentialBroker } from "./broker";
import {
  type AdoptResult, BROKER_MOVED_HEADER, brokerMovedTarget, homeBrokerName, MAX_PLACEMENT_HOPS,
  parseBrokerName, type PlacementState, PLACEMENT_REGIONS,
} from "./broker-placement";

export interface BrokerRouterEnv {
  USER_CREDENTIALS: DurableObjectNamespace<UserCredentialBroker>;
}
/** claim: trusted model-transport region that may adopt the canonical broker.
 * hint: ingress region probed first; never moves state. */
export type BrokerPlacement = Readonly<{ claim?: string; hint?: string }>;

const LOCATED_TTL_MS = 5 * 60_000;
/** A cooldown redirect is re-evaluated soon so the home follows the user. */
const REDIRECT_TTL_MS = 60_000;
const MAX_LOCATED = 1024;
const MAX_REPLAY_BODY_BYTES = 8 * 1024 * 1024;
/** Isolate-local hint only. A stale name is refused by its tombstone. */
const located = new Map<string, { name: string; until: number }>();

export class BrokerUnavailableError extends Error {
  constructor() { super("credential_broker_unavailable"); }
}

function stubFor(env: BrokerRouterEnv, name: string): DurableObjectStub<UserCredentialBroker> {
  const parsed = parseBrokerName(name);
  return env.USER_CREDENTIALS.getByName(name, parsed?.kind === "home" ? { locationHint: parsed.region } : undefined);
}

function remember(key: string, name: string, ttl: number): void {
  if (located.size >= MAX_LOCATED && !located.has(key)) located.delete(located.keys().next().value!);
  located.set(key, { name, until: Date.now() + ttl });
}

/** Test seam: drop isolate-local placement hints. */
export function resetBrokerLocations(): void { located.clear(); }

/**
 * The user's canonical credential broker, wherever it currently lives. Each
 * call goes straight to the located object; a non-active object refuses
 * before any work, so one re-locate and replay is always safe.
 */
export class RoutedUserBroker {
  readonly #env: BrokerRouterEnv;
  readonly #userId: string;
  readonly #claim: string | undefined;
  readonly #hint: string | undefined;
  readonly #key: string;

  constructor(env: BrokerRouterEnv, userId: string, placement: BrokerPlacement = {}) {
    this.#env = env;
    this.#userId = userId;
    this.#claim = placement.claim && PLACEMENT_REGIONS.has(placement.claim) && parseBrokerName(userId)?.kind === "legacy"
      ? placement.claim : undefined;
    this.#hint = placement.hint && PLACEMENT_REGIONS.has(placement.hint) && parseBrokerName(userId)?.kind === "legacy"
      ? placement.hint : undefined;
    this.#key = `${userId}|${this.#claim ?? ""}`;
  }

  resolveModelCredential(...args: Parameters<UserCredentialBroker["resolveModelCredential"]>): ReturnType<UserCredentialBroker["resolveModelCredential"]> {
    return this.#call((stub) => stub.resolveModelCredential(...args) as unknown as ReturnType<UserCredentialBroker["resolveModelCredential"]>);
  }
  resolveClaudeCredential(...args: Parameters<UserCredentialBroker["resolveClaudeCredential"]>): ReturnType<UserCredentialBroker["resolveClaudeCredential"]> {
    return this.#call((stub) => stub.resolveClaudeCredential(...args) as unknown as ReturnType<UserCredentialBroker["resolveClaudeCredential"]>);
  }
  registerUpgradeHolder(...args: Parameters<UserCredentialBroker["registerUpgradeHolder"]>): ReturnType<UserCredentialBroker["registerUpgradeHolder"]> {
    return this.#call((stub) => stub.registerUpgradeHolder(...args) as unknown as ReturnType<UserCredentialBroker["registerUpgradeHolder"]>);
  }
  readVaultMetadata(): ReturnType<UserCredentialBroker["readVaultMetadata"]> {
    return this.#call((stub) => stub.readVaultMetadata() as unknown as ReturnType<UserCredentialBroker["readVaultMetadata"]>);
  }
  readWalletIdentity(): ReturnType<UserCredentialBroker["readWalletIdentity"]> {
    return this.#call((stub) => stub.readWalletIdentity() as unknown as ReturnType<UserCredentialBroker["readWalletIdentity"]>);
  }

  async fetch(input: string, init: RequestInit = {}): Promise<Response> {
    let body = init.body;
    if (body instanceof ReadableStream) {
      const bytes = await readBounded(body, MAX_REPLAY_BODY_BYTES);
      if (!bytes) return Response.json({ error: "request_body_too_large" }, { status: 413 });
      body = bytes;
    }
    const replayable: RequestInit = { ...init, ...(body === undefined ? {} : { body }) };
    let name = this.#initial();
    for (let attempt = 0; ; attempt += 1) {
      const response = await stubFor(this.#env, name).fetch(input, replayable);
      if (response.status !== 421 || !response.headers.has(BROKER_MOVED_HEADER) || attempt > 0) return response;
      const target = response.headers.get(BROKER_MOVED_HEADER) ?? "";
      await response.body?.cancel().catch(() => {});
      name = await this.#relocate(parseBrokerName(target) ? target : undefined);
    }
  }

  /** Activate (and, for a claim, adopt) the canonical broker. Outcome only. */
  async warm(): Promise<"warm" | "filled" | "unavailable"> {
    if (!this.#claim) {
      try { await this.#walk(); return "warm"; } catch { return "unavailable"; }
    }
    const result = await this.#adopt();
    return result === "migrated" ? "filled" : result === "unavailable" ? "unavailable" : "warm";
  }

  async #call<T>(invoke: (stub: DurableObjectStub<UserCredentialBroker>) => Promise<T>): Promise<Awaited<T>> {
    let name = this.#initial();
    for (let attempt = 0; ; attempt += 1) {
      try {
        return await invoke(stubFor(this.#env, name));
      } catch (error) {
        const moved = brokerMovedTarget(error);
        if (!moved || attempt > 0) throw error;
        name = await this.#relocate(moved.target);
      }
    }
  }

  #initial(): string {
    const cached = located.get(this.#key);
    if (cached && cached.until > Date.now()) return cached.name;
    // Optimistic: the regional home is normally the active broker.
    const region = this.#claim ?? this.#hint;
    return region ? homeBrokerName(region, this.#userId) : this.#userId;
  }

  async #relocate(target: string | undefined): Promise<string> {
    located.delete(this.#key);
    if (this.#claim) {
      const result = await this.#adopt();
      if (result === "unavailable") throw new BrokerUnavailableError();
      return located.get(this.#key)!.name;
    }
    if (target && parseBrokerName(target)?.kind === "home") {
      remember(this.#key, target, LOCATED_TTL_MS);
      return target;
    }
    return this.#walk();
  }

  async #adopt(): Promise<"active" | "migrated" | "redirect" | "unavailable"> {
    const home = homeBrokerName(this.#claim!, this.#userId);
    let result: AdoptResult;
    try { result = await stubFor(this.#env, home).adoptHome(); } catch { return "unavailable"; }
    if (result.status === "active") {
      remember(this.#key, home, LOCATED_TTL_MS);
      // Regionless callers in this isolate (tools, control) share the answer.
      remember(`${this.#userId}|`, home, LOCATED_TTL_MS);
      return result.migrated ? "migrated" : "active";
    }
    if (result.status === "redirect" && parseBrokerName(result.target)) {
      remember(this.#key, result.target, REDIRECT_TTL_MS);
      return "redirect";
    }
    return "unavailable";
  }

  /** Follow the legacy directory to the active broker. */
  async #walk(): Promise<string> {
    let name = this.#userId;
    for (let hop = 0; hop < MAX_PLACEMENT_HOPS; hop += 1) {
      const status = await stubFor(this.#env, name).placementState() as PlacementState;
      if (status.state === "active") {
        remember(this.#key, name, LOCATED_TTL_MS);
        return name;
      }
      if (status.state !== "moved" || !parseBrokerName(status.target)) break;
      name = status.target;
    }
    throw new BrokerUnavailableError();
  }
}

async function readBounded(stream: ReadableStream, limit: number): Promise<Uint8Array | undefined> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = value instanceof Uint8Array ? value : new Uint8Array(value as ArrayBuffer);
      total += chunk.byteLength;
      if (total > limit) { await reader.cancel().catch(() => {}); return undefined; }
      chunks.push(chunk);
    }
  } finally { reader.releaseLock(); }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
  return body;
}
