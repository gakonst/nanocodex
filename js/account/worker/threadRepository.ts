const SHA1_PATTERN = /^[a-f0-9]{40}$/;
const PACK_KEY_PATTERN = /^thread-repositories\/[A-Za-z0-9._-]+\/[A-Za-z0-9-]+\.pack$/;
const THREAD_BRANCH = "nanocodex";
const THREAD_REF = `refs/heads/${THREAD_BRANCH}` as const;
const ZERO_OID = "0".repeat(40);
const MAX_PACK_BYTES = 32 * 1024 * 1024;
const MAX_PACK_OBJECTS = 0xffff_ffff;
const receiveLeaseTtlMs = 2 * 60 * 1_000;

export type RepositoryRef = {
  name: typeof THREAD_REF;
  oid: string;
};

export type ThreadPackMetadata = {
  key: string;
  hash: string;
  size: number;
  objectCount: number;
};

export type ThreadPack = ThreadPackMetadata & {
  oldOid: string;
  newOid: string;
};

export type RepositoryView = {
  head: string;
  branch: typeof THREAD_BRANCH;
  refs: [RepositoryRef];
  packs: ThreadPack[];
};

export type ThreadRepository = RepositoryView & {
  version: 1;
  updatedAt: string;
};

type ReceiveLease = {
  token: string;
  oldOid: string;
  newOid: string;
  ref: typeof THREAD_REF;
  expiresAt: number;
};

const threadStorageKey = "thread";
const receiveLeaseStorageKey = "receive-lease";

export class ThreadGitRepository {
  readonly #state: DurableObjectState;

  constructor(state: DurableObjectState) {
    this.#state = state;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/thread" && request.method === "GET") {
      const repository = await this.#state.storage.get<ThreadRepository>(threadStorageKey);
      return repository
        ? Response.json(repository)
        : Response.json({ error: "not_found" }, { status: 404 });
    }
    if (url.pathname === "/receive/begin" && request.method === "POST") {
      return this.#beginReceive(request);
    }
    if (url.pathname === "/receive/finalize" && request.method === "PUT") {
      return this.#finalizeReceive(request);
    }
    if (url.pathname === "/receive/abort" && request.method === "POST") {
      return this.#abortReceive(request);
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  }

  async #beginReceive(request: Request): Promise<Response> {
    const command = await request.json().catch(() => undefined) as {
      oldOid?: unknown;
      newOid?: unknown;
      ref?: unknown;
    } | undefined;
    if (!isReceiveCommand(command)) {
      return Response.json({ error: "invalid_receive" }, { status: 400 });
    }
    return this.#state.blockConcurrencyWhile(async () => {
      const now = Date.now();
      const active = await this.#state.storage.get<ReceiveLease>(receiveLeaseStorageKey);
      if (active && active.expiresAt > now) {
        return Response.json({ error: "receive_busy" }, { status: 409 });
      }
      const repository = await this.#state.storage.get<ThreadRepository>(threadStorageKey);
      const currentHead = repository?.head ?? ZERO_OID;
      if (command.oldOid !== currentHead) {
        return Response.json({ error: "stale_receive", currentHead }, { status: 409 });
      }
      const lease: ReceiveLease = {
        token: crypto.randomUUID(),
        oldOid: command.oldOid,
        newOid: command.newOid,
        ref: command.ref,
        expiresAt: now + receiveLeaseTtlMs,
      };
      await this.#state.storage.put(receiveLeaseStorageKey, lease);
      return Response.json({ lease });
    });
  }

  async #finalizeReceive(request: Request): Promise<Response> {
    const body = await request.json().catch(() => undefined) as {
      token?: unknown;
      pack?: unknown;
    } | undefined;
    const packMetadata = body?.pack;
    if (typeof body?.token !== "string" || !isThreadPackMetadata(packMetadata)) {
      return Response.json({ error: "invalid_receive" }, { status: 400 });
    }
    return this.#state.blockConcurrencyWhile(async () => {
      const lease = await this.#state.storage.get<ReceiveLease>(receiveLeaseStorageKey);
      if (!lease || lease.token !== body.token || lease.expiresAt <= Date.now()) {
        return Response.json({ error: "receive_lease_expired" }, { status: 409 });
      }
      const current = await this.#state.storage.get<ThreadRepository>(threadStorageKey);
      const currentHead = current?.head ?? ZERO_OID;
      if (currentHead !== lease.oldOid) {
        await this.#state.storage.delete(receiveLeaseStorageKey);
        return Response.json({ error: "stale_receive", currentHead }, { status: 409 });
      }
      const pack: ThreadPack = {
        ...packMetadata,
        oldOid: lease.oldOid,
        newOid: lease.newOid,
      };
      const repository: ThreadRepository = {
        version: 1,
        head: lease.newOid,
        branch: THREAD_BRANCH,
        refs: [{ name: THREAD_REF, oid: lease.newOid }],
        packs: [...(current?.packs ?? []), pack],
        updatedAt: new Date().toISOString(),
      };
      if (!isThreadRepository(repository)) {
        return Response.json({ error: "invalid_receive" }, { status: 400 });
      }
      await this.#state.storage.put(threadStorageKey, repository);
      await this.#state.storage.delete(receiveLeaseStorageKey);
      return Response.json({ repository });
    });
  }

  async #abortReceive(request: Request): Promise<Response> {
    const body = await request.json().catch(() => undefined) as { token?: unknown } | undefined;
    if (typeof body?.token !== "string") {
      return Response.json({ error: "invalid_receive" }, { status: 400 });
    }
    return this.#state.blockConcurrencyWhile(async () => {
      const lease = await this.#state.storage.get<ReceiveLease>(receiveLeaseStorageKey);
      if (lease?.token === body.token) {
        await this.#state.storage.delete(receiveLeaseStorageKey);
      }
      return Response.json({ ok: true });
    });
  }
}

export function isThreadRepository(value: unknown): value is ThreadRepository {
  if (value == null || typeof value !== "object") return false;
  const repository = value as Partial<ThreadRepository>;
  if (
    repository.version !== 1 ||
    typeof repository.head !== "string" ||
    !SHA1_PATTERN.test(repository.head) ||
    repository.branch !== THREAD_BRANCH ||
    !Array.isArray(repository.refs) ||
    repository.refs.length !== 1 ||
    repository.refs[0]?.name !== THREAD_REF ||
    repository.refs[0].oid !== repository.head ||
    !Array.isArray(repository.packs) ||
    repository.packs.length === 0 ||
    !repository.packs.every(isThreadPack) ||
    typeof repository.updatedAt !== "string" ||
    !Number.isFinite(Date.parse(repository.updatedAt))
  ) {
    return false;
  }
  const keys = new Set(repository.packs.map(({ key }) => key));
  let previousOid = ZERO_OID;
  let objectCount = 0;
  for (const pack of repository.packs) {
    if (pack.oldOid !== previousOid) return false;
    previousOid = pack.newOid;
    objectCount += pack.objectCount;
  }
  return keys.size === repository.packs.length &&
    objectCount <= MAX_PACK_OBJECTS &&
    previousOid === repository.head;
}

export function isThreadPack(value: unknown): value is ThreadPack {
  if (!isThreadPackMetadata(value)) return false;
  const pack = value as Partial<ThreadPack>;
  return typeof pack.oldOid === "string" && SHA1_PATTERN.test(pack.oldOid) &&
    typeof pack.newOid === "string" && SHA1_PATTERN.test(pack.newOid) &&
    pack.newOid !== ZERO_OID;
}

export function isThreadPackMetadata(value: unknown): value is ThreadPackMetadata {
  if (value == null || typeof value !== "object") return false;
  const pack = value as Partial<ThreadPackMetadata>;
  return typeof pack.key === "string" && PACK_KEY_PATTERN.test(pack.key) &&
    typeof pack.hash === "string" && SHA1_PATTERN.test(pack.hash) &&
    typeof pack.size === "number" && Number.isSafeInteger(pack.size) &&
    pack.size >= 32 && pack.size <= MAX_PACK_BYTES &&
    typeof pack.objectCount === "number" && Number.isSafeInteger(pack.objectCount) &&
    pack.objectCount > 0 && pack.objectCount <= MAX_PACK_OBJECTS;
}

function isReceiveCommand(value: unknown): value is Pick<ReceiveLease, "oldOid" | "newOid" | "ref"> {
  if (value == null || typeof value !== "object") return false;
  const command = value as { oldOid?: unknown; newOid?: unknown; ref?: unknown };
  return typeof command.oldOid === "string" && SHA1_PATTERN.test(command.oldOid) &&
    typeof command.newOid === "string" && SHA1_PATTERN.test(command.newOid) &&
    command.newOid !== ZERO_OID && command.ref === THREAD_REF;
}
