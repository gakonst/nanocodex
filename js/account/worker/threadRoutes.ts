import {
  buildFullPackResponse,
  buildLegacyFullPackResponse,
  buildLsRefsResponse,
  buildNegotiationResponse,
  buildReceiveReport,
  buildSidebandReceiveResponse,
  parseReceiveRequest,
  parsePacketLines,
  parseV2Command,
  receiveAdvertisement,
  legacyRepositoryAdvertisement,
  repositoryAdvertisement,
  type ReceiveCommand,
} from "./threadProtocol.ts";
import { createThreadPackStream } from "./threadPack.ts";
import {
  isThreadRepository,
  type RepositoryView,
  type ThreadPack,
  type ThreadPackMetadata,
} from "./threadRepository.ts";

// Smart-HTTP/DO/R2 ownership follows the MIT-licensed Git-on-Cloudflare design.
// Its account, admin, registry, and D1 product layers are intentionally absent;
// see git-on-cloudflare.LICENSE.

const SHA1_PATTERN = /^[a-f0-9]{40}$/;
const THREAD_REPOSITORY_PATTERN = /^thread-[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
const THREAD_BRANCH = "nanocodex";
const THREAD_REF = `refs/heads/${THREAD_BRANCH}`;
const ZERO_OID = "0".repeat(40);
const MAX_THREAD_PACK_BYTES = 32 * 1024 * 1024;

export type ThreadGitStorageEnv = {
  GIT_OBJECTS?: R2Bucket;
  THREAD_GIT_REPOSITORY?: DurableObjectNamespace;
};

export async function handleThreadGitRequest(
  request: Request,
  env: ThreadGitStorageEnv,
  url: URL,
  context?: ExecutionContext,
): Promise<Response | undefined> {
  const route = parseGitRoute(url.pathname);
  if (!route) return undefined;
  if (!env.GIT_OBJECTS || !env.THREAD_GIT_REPOSITORY) {
    return gitError("Git workspace storage is not configured", 503);
  }

  if (route.operation === "info/refs" && request.method === "GET") {
    const service = url.searchParams.get("service");
    if (service === "git-upload-pack") {
      const protocolV2 = request.headers.get("git-protocol")
        ?.split(":").some((value) => value.trim() === "version=2") ?? false;
      const repository = protocolV2 ? undefined : await getThreadRepository(env, route.repository);
      if (repository instanceof Response) return repository;
      const advertisement = protocolV2
        ? repositoryAdvertisement()
        : legacyRepositoryAdvertisement(repository);
      return new Response(byteBody(advertisement), {
        headers: gitHeaders("application/x-git-upload-pack-advertisement"),
      });
    }
    if (service === "git-receive-pack") {
      const repository = await getThreadRepository(env, route.repository);
      if (repository instanceof Response) return repository;
      return new Response(byteBody(receiveAdvertisement(repository)), {
        headers: gitHeaders("application/x-git-receive-pack-advertisement"),
      });
    }
    return gitError("unsupported Git service", 400);
  }

  if (route.operation === "git-upload-pack" && request.method === "POST") {
    return uploadPack(request, env, route.repository);
  }
  if (route.operation === "git-receive-pack" && request.method === "POST") {
    return receivePack(request, env, route.repository, context);
  }
  return new Response(null, { status: 405, headers: { allow: "GET, POST" } });
}

async function uploadPack(
  request: Request,
  env: ThreadGitStorageEnv,
  repositoryName: string,
): Promise<Response> {
  const repository = await getThreadRepository(env, repositoryName);
  if (repository instanceof Response) return repository;
  const body = await readGitProtocolRequest(request);
  if (body instanceof Response) return body;
  const protocolV2 = request.headers.get("git-protocol")
    ?.split(":").some((value) => value.trim() === "version=2") ?? false;
  if (!protocolV2) return legacyUploadPack(body, env, repository);
  let command: ReturnType<typeof parseV2Command>;
  try {
    command = parseV2Command(body);
  } catch {
    return gitError("malformed git-upload-pack request", 400);
  }
  if (command.command === "ls-refs") {
    return uploadResponse(byteBody(buildLsRefsResponse(repository, command.arguments)));
  }
  if (command.command !== "fetch") return gitError("unsupported Git protocol command", 400);
  if (!repository) return gitError("thread repository is empty", 404);

  let wants: string[];
  let haves: string[];
  try {
    wants = parseFetchOids(command.arguments, "want");
    haves = parseFetchOids(command.arguments, "have");
  } catch (error) {
    return gitError(errorMessage(error), 400);
  }
  const advertisedOids = new Set([repository.head, ...repository.refs.map((ref) => ref.oid)]);
  if (wants.length === 0 || wants.some((oid) => !advertisedOids.has(oid))) {
    return gitError("invalid fetch wants", 400);
  }
  const selection = selectPackSuffix(repository.packs, haves);
  if (!command.arguments.includes("done")) {
    return uploadResponse(byteBody(buildNegotiationResponse(
      selection.have ? [selection.have] : [],
    )));
  }
  const pack = await fetchPackStream(env.GIT_OBJECTS!, selection.packs);
  if (pack instanceof Response) return pack;
  return uploadResponse(buildFullPackResponse(pack));
}

async function legacyUploadPack(
  body: Uint8Array,
  env: ThreadGitStorageEnv,
  repository: RepositoryView | undefined,
): Promise<Response> {
  if (!repository) return gitError("thread repository is empty", 404);
  let wants: string[];
  let haves: string[];
  try {
    const lines = parsePacketLines(body)
      .filter((packet) => packet.kind === "data")
      .map((packet) => new TextDecoder().decode(packet.data).replace(/\n$/, ""));
    wants = parseFetchOids(lines, "want");
    haves = parseFetchOids(lines, "have");
  } catch (error) {
    return gitError(errorMessage(error), 400);
  }
  const advertisedOids = new Set([repository.head, ...repository.refs.map((ref) => ref.oid)]);
  if (wants.length === 0 || wants.some((oid) => !advertisedOids.has(oid))) {
    return gitError("invalid fetch wants", 400);
  }
  const selection = selectPackSuffix(repository.packs, haves);
  const pack = await fetchPackStream(env.GIT_OBJECTS!, selection.packs);
  if (pack instanceof Response) return pack;
  return uploadResponse(buildLegacyFullPackResponse(pack, selection.have));
}

async function receivePack(
  request: Request,
  env: ThreadGitStorageEnv,
  repositoryName: string,
  context?: ExecutionContext,
): Promise<Response> {
  const body = await readGitProtocolRequest(request, MAX_THREAD_PACK_BYTES + 512 * 1024);
  if (body instanceof Response) return body;

  let command: ReceiveCommand | undefined;
  let pack: Uint8Array | undefined;
  let packMetadata: Awaited<ReturnType<typeof validatePack>> | undefined;
  let sideBand64k = false;
  try {
    const parsed = parseReceiveRequest(body);
    sideBand64k = parsed.sideBand64k;
    if (parsed.commands.length !== 1) throw new Error("only one branch may be pushed at a time");
    command = parsed.commands[0]!;
    if (
      !SHA1_PATTERN.test(command.oldOid) ||
      !SHA1_PATTERN.test(command.newOid) ||
      command.newOid === ZERO_OID ||
      command.ref !== THREAD_REF
    ) {
      throw new Error(`only updates to refs/heads/${THREAD_BRANCH} are supported`);
    }
    pack = parsed.pack;
    packMetadata = await validatePack(pack);
  } catch (error) {
    const fallback = command ?? { oldOid: ZERO_OID, newOid: ZERO_OID, ref: THREAD_REF };
    return receiveResponse(buildReceiveReport(fallback, errorMessage(error)), sideBand64k);
  }
  if (!command || !pack || !packMetadata) return gitError("invalid receive state", 500);

  const stub = repositoryStub(env, repositoryName);
  const begin = await stub.fetch("https://repository.internal/receive/begin", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(command),
  });
  if (begin.status === 409) {
    const conflict = await begin.json().catch(() => undefined) as { error?: unknown } | undefined;
    if (conflict?.error === "stale_receive") {
      return receiveResponse(
        buildReceiveReport(command, "stale ref; pull and retry"),
        sideBand64k,
      );
    }
    return gitError("repository is busy; retry the push", 503);
  }
  if (!begin.ok) return gitError("could not reserve repository receive", 503);
  const beginBody = await begin.json() as { lease?: { token?: unknown } };
  const token = typeof beginBody.lease?.token === "string" ? beginBody.lease.token : "";
  if (!token) return gitError("repository returned an invalid receive lease", 503);

  const packKey = `thread-repositories/${repositoryName}/${crypto.randomUUID()}.pack`;
  let uploaded = false;
  let finalizeAttempted = false;
  try {
    await env.GIT_OBJECTS!.put(packKey, pack, {
      httpMetadata: {
        contentType: "application/x-git-packed-objects",
        cacheControl: "private, max-age=31536000, immutable",
      },
      customMetadata: { repository: repositoryName, head: command.newOid },
    });
    uploaded = true;
    const storedPack: ThreadPackMetadata = {
      key: packKey,
      hash: packMetadata.hash,
      size: packMetadata.size,
      objectCount: packMetadata.objectCount,
    };
    finalizeAttempted = true;
    const finalized = await stub.fetch("https://repository.internal/receive/finalize", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token, pack: storedPack }),
    });
    if (!finalized.ok) throw new Error("receive lease expired before refs were committed");
    return receiveResponse(buildReceiveReport(command), sideBand64k);
  } catch (error) {
    const cleanup = Promise.all([
      uploaded && !finalizeAttempted ? env.GIT_OBJECTS!.delete(packKey) : Promise.resolve(),
      stub.fetch("https://repository.internal/receive/abort", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ token }),
      }).then(() => undefined),
    ]);
    if (context) context.waitUntil(cleanup);
    else await cleanup;
    return receiveResponse(buildReceiveReport(command, errorMessage(error)), sideBand64k);
  }
}

async function validatePack(pack: Uint8Array): Promise<{
  hash: string;
  size: number;
  objectCount: number;
}> {
  if (pack.byteLength > MAX_THREAD_PACK_BYTES) throw new Error("pack exceeds 32 MiB");
  if (pack.byteLength < 32) throw new Error("pack is truncated");
  if (new TextDecoder().decode(pack.subarray(0, 4)) !== "PACK") {
    throw new Error("body does not begin with a Git pack");
  }
  const view = new DataView(pack.buffer, pack.byteOffset, pack.byteLength);
  if (view.getUint32(4) !== 2) throw new Error("only Git pack version 2 is supported");
  const objectCount = view.getUint32(8);
  if (objectCount === 0) throw new Error("pack contains no objects");
  const expected = pack.subarray(pack.byteLength - 20);
  const digestBytes = arrayBufferView(pack, pack.byteLength - 20);
  const actual = new Uint8Array(await crypto.subtle.digest(
    "SHA-1",
    digestBytes,
  ));
  if (!constantTimeEqual(actual, expected)) throw new Error("pack checksum is invalid");
  return { hash: bytesToHex(expected), size: pack.byteLength, objectCount };
}

async function fetchPackStream(
  bucket: R2Bucket,
  packs: readonly ThreadPack[],
): Promise<ReadableStream<Uint8Array> | Response> {
  try {
    return await createThreadPackStream(bucket, packs);
  } catch (error) {
    return gitError(errorMessage(error), 503);
  }
}

function parseFetchOids(lines: readonly string[], kind: "want" | "have"): string[] {
  const prefix = `${kind} `;
  return lines.filter((line) => line.startsWith(prefix)).map((line) => {
    const oid = line.slice(prefix.length).split(" ", 1)[0]!;
    if (!SHA1_PATTERN.test(oid)) throw new Error(`invalid fetch ${kind}`);
    return oid;
  });
}

function selectPackSuffix(
  packs: readonly ThreadPack[],
  haves: readonly string[],
): { packs: readonly ThreadPack[]; have?: string } {
  const clientOids = new Set(haves);
  for (let index = packs.length - 1; index >= 0; index--) {
    const newOid = packs[index]!.newOid;
    if (clientOids.has(newOid)) {
      return { packs: packs.slice(index + 1), have: newOid };
    }
  }
  return { packs };
}

async function getThreadRepository(
  env: ThreadGitStorageEnv,
  repositoryName: string,
): Promise<RepositoryView | undefined | Response> {
  const response = await repositoryStub(env, repositoryName)
    .fetch("https://repository.internal/thread");
  if (response.status === 404) return undefined;
  if (!response.ok) return gitError("repository lookup failed", 503);
  const repository: unknown = await response.json();
  return isThreadRepository(repository)
    ? repository
    : gitError("repository state is invalid", 503);
}

export async function readGitProtocolRequest(
  request: Request,
  maxBytes = 4 * 1024 * 1024,
): Promise<Uint8Array | Response> {
  const encoding = request.headers.get("content-encoding")?.trim().toLowerCase();
  if (encoding && encoding !== "identity" && encoding !== "gzip") {
    return gitError("unsupported Git request encoding", 415);
  }
  if (!request.body) return new Uint8Array();
  const stream = encoding === "gzip"
    ? request.body.pipeThrough(new DecompressionStream("gzip"))
    : request.body;
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let totalBytes = 0;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      totalBytes += next.value.byteLength;
      if (totalBytes > maxBytes) {
        await reader.cancel("Git request is too large");
        return gitError("Git request is too large", 413);
      }
      chunks.push(next.value);
    }
  } catch {
    return gitError("Git request body could not be decoded", 400);
  }
  const body = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

function parseGitRoute(pathname: string): {
  repository: string;
  operation: "info/refs" | "git-upload-pack" | "git-receive-pack";
} | undefined {
  const match = pathname.match(/^\/git\/(thread-[a-f0-9-]+)\/(info\/refs|git-upload-pack|git-receive-pack)$/);
  if (!match || !THREAD_REPOSITORY_PATTERN.test(match[1]!)) return undefined;
  return {
    repository: match[1]!,
    operation: match[2] as "info/refs" | "git-upload-pack" | "git-receive-pack",
  };
}

function repositoryStub(env: ThreadGitStorageEnv, repositoryName: string): DurableObjectStub {
  const namespace = env.THREAD_GIT_REPOSITORY!;
  return namespace.get(namespace.idFromName(repositoryName));
}

function uploadResponse(body: BodyInit): Response {
  return new Response(body, { headers: gitHeaders("application/x-git-upload-pack-result") });
}

function receiveResponse(body: Uint8Array, sideBand64k: boolean): Response {
  const responseBody = sideBand64k ? buildSidebandReceiveResponse(body) : body;
  return new Response(byteBody(responseBody), { headers: gitHeaders("application/x-git-receive-pack-result") });
}

function gitHeaders(contentType: string): HeadersInit {
  return {
    "cache-control": "no-store",
    "content-type": contentType,
    "x-content-type-options": "nosniff",
  };
}

function gitError(message: string, status: number): Response {
  return new Response(`${message}\n`, {
    status,
    headers: { "cache-control": "no-store", "content-type": "text/plain; charset=utf-8" },
  });
}

function byteBody(bytes: Uint8Array): ArrayBuffer {
  return bytes.slice().buffer;
}

function arrayBufferView(bytes: Uint8Array, byteLength: number): Uint8Array<ArrayBuffer> {
  if (bytes.buffer instanceof ArrayBuffer) {
    return new Uint8Array(bytes.buffer, bytes.byteOffset, byteLength);
  }
  return bytes.subarray(0, byteLength).slice();
}

function bytesToHex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function constantTimeEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  let difference = 0;
  for (let index = 0; index < left.byteLength; index++) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
