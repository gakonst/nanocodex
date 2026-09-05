import {
  buildFullPackResponse,
  buildLsRefsResponse,
  buildNegotiationResponse,
  parseFetchArguments,
  parseV2Command,
  repositoryAdvertisement,
} from "./gitProtocol.ts";
import {
  isGitObjectManifest,
  selectGitObjects,
  type GitObjectManifest,
} from "./gitObjectManifest.ts";
import { createSelectedPackStream } from "./gitObjectPack.ts";
import { createRepositoryPartsStream } from "./gitPackParts.ts";
import {
  isCommitPatchManifest,
  isRepositoryPublication,
  type RepositoryPublication,
} from "./gitRepository.ts";

const SHA1_PATTERN = /^[a-f0-9]{40}$/;
const generationFilePattern = /^(publication\.json|repository\.json|commits\.json|commit-index\.json|commit-patches\.json|inventory\.json|objects\.json)$/;
const generationCommitPagePattern = /^commits\/(\d{4})\.json$/;
const generationCommitPatchPartPattern = /^commit-patches\/(\d{4})\.diff$/;
const generationObjectShardPattern = /^objects\/(\d{4})\.pack$/;
const generationPackPartPattern = /^packs\/([a-f0-9]{40})\/(\d{4})\.pack$/;
const immutableCacheControl = "public, max-age=31536000, immutable";

export type GitStorageEnv = {
  GIT_OBJECTS?: R2Bucket;
  GIT_REPOSITORY?: DurableObjectNamespace;
  GIT_MIRROR_TOKEN?: string;
};

export async function handleGitRequest(
  request: Request,
  env: GitStorageEnv,
  url: URL,
  context?: ExecutionContext,
): Promise<Response | undefined> {
  if (request.method === "GET" && url.pathname === "/api/repository/snapshot") {
    const pinned = servePinnedGeneration(
      request,
      env,
      context,
      url,
      "repository.json",
    );
    if (pinned) return pinned;
    return servePublishedObject(request, env, context, "snapshotKey", false);
  }
  if (request.method === "GET" && url.pathname === "/api/repository/commit-index") {
    const pinned = servePinnedGeneration(
      request,
      env,
      context,
      url,
      "commit-index.json",
    );
    if (pinned) return pinned;
    return servePublishedGenerationObject(
      request,
      env,
      context,
      "commit-index.json",
    );
  }
  if (request.method === "GET" && url.pathname === "/api/repository/commits") {
    const page = url.searchParams.get("page");
    if (page != null) {
      if (!/^\d+$/.test(page) || Number(page) > 9_999) {
        return Response.json({ error: "invalid_commit_page" }, { status: 400 });
      }
      const generation = url.searchParams.get("generation");
      if (generation != null && !SHA1_PATTERN.test(generation)) {
        return Response.json({ error: "invalid_repository_generation" }, { status: 400 });
      }
      return generation == null
        ? servePublishedCommitPage(request, env, context, Number(page))
        : serveCommitPage(request, env, context, generation, Number(page));
    }
    return servePublishedObject(request, env, context, "commitsKey", false);
  }
  const commitPatchMatch = url.pathname.match(
    /^\/api\/repository\/commits\/([a-f0-9]{40})\.diff$/,
  );
  if (request.method === "GET" && commitPatchMatch) {
    return serveCommitPatch(env, commitPatchMatch[1]);
  }
  const commitPatchPageMatch = url.pathname.match(
    /^\/api\/repository\/commits\/([a-f0-9]{40})\/(\d{4})\.diff$/,
  );
  if (request.method === "GET" && commitPatchPageMatch) {
    return serveCommitPatchPage(
      request,
      env,
      context,
      commitPatchPageMatch[1],
      Number(commitPatchPageMatch[2]),
    );
  }
  if (
    request.method === "GET" &&
    url.pathname.startsWith("/api/repository/commits/") &&
    url.pathname.endsWith(".diff")
  ) {
    return Response.json({ error: "invalid_repository_generation" }, { status: 400 });
  }
  const blobMatch = url.pathname.match(/^\/api\/repository\/blob\/([a-f0-9]{40})$/);
  if (request.method === "GET" && blobMatch) {
    return serveObject(request, env, context, `blobs/${blobMatch[1]}.txt`, true);
  }
  const patchMatch = url.pathname.match(
    /^\/api\/repository\/commit\/([a-f0-9]{40})\.patch$/,
  );
  if (request.method === "GET" && patchMatch) {
    return serveObject(request, env, context, `patches/${patchMatch[1]}.patch`, true);
  }

  if (url.pathname === "/api/git/state" && request.method === "GET") {
    if (!(await authorizeMirrorRequest(request, env))) return unauthorized();
    const publication = await getPublication(env);
    if (publication instanceof Response) return publication;
    const inventory = await requireBucket(env).get(publication.inventoryKey);
    if (inventory == null) return storageFailure("published inventory is missing");
    const objectManifest = await requireBucket(env).get(publication.objectManifestKey);
    if (objectManifest == null) return storageFailure("published object manifest is missing");
    const parsedManifest: unknown = await objectManifest.json();
    if (!isGitObjectManifest(parsedManifest) || parsedManifest.head !== publication.head) {
      return storageFailure("published object manifest is invalid");
    }
    return Response.json({
      publication,
      inventory: await inventory.json(),
      objectManifest: parsedManifest,
    }, { headers: { "cache-control": "no-store" } });
  }

  if (url.pathname.startsWith("/api/git/objects/") && request.method === "PUT") {
    if (!(await authorizeMirrorRequest(request, env))) return unauthorized();
    const key = objectKeyFromUploadPath(url.pathname);
    if (key == null) return Response.json({ error: "invalid_object_key" }, { status: 400 });
    if (request.body == null) return Response.json({ error: "missing_body" }, { status: 400 });
    const bucket = requireBucket(env);
    const existing = await bucket.head(key);
    if (existing != null) {
      await request.body.cancel();
      return immutableUploadResponse(key, existing, false);
    }
    const uploaded = await bucket.put(key, request.body, {
      onlyIf: { etagDoesNotMatch: "*" },
      httpMetadata: {
        contentType: contentTypeForKey(key),
        cacheControl: immutableCacheControl,
      },
      customMetadata: { uploadedBy: "nanocodex-repository-mirror" },
    });
    if (uploaded == null && !request.body.locked) await request.body.cancel();
    const object = uploaded ?? await bucket.head(key);
    if (object == null) return storageFailure("immutable object upload did not resolve");
    return immutableUploadResponse(key, object, uploaded != null);
  }

  if (url.pathname === "/api/git/publish" && request.method === "PUT") {
    if (!(await authorizeMirrorRequest(request, env))) return unauthorized();
    let body: { expectedHead?: unknown; publication?: unknown; replaceInvalid?: unknown };
    try {
      body = await request.json();
    } catch {
      return Response.json({ error: "invalid_json" }, { status: 400 });
    }
    const expectedHead = body.expectedHead;
    if (
      !(expectedHead === null ||
        (typeof expectedHead === "string" && SHA1_PATTERN.test(expectedHead))) ||
      (body.replaceInvalid !== undefined && body.replaceInvalid !== true) ||
      !isRepositoryPublication(body.publication)
    ) {
      return Response.json({ error: "invalid_publication" }, { status: 400 });
    }
    const publication = body.publication;
    const generationPrefix = `generations/${publication.head}`;
    const publicationKey = `${generationPrefix}/publication.json`;
    const commitPatchManifestKey = `${generationPrefix}/commit-patches.json`;
    const commitIndexKey = `${generationPrefix}/commit-index.json`;
    const commitPageKeys = publication.commitPatchParts.map(
      (_, index) => `${generationPrefix}/commits/${String(index).padStart(4, "0")}.json`,
    );
    const requiredKeys = [
      publicationKey,
      publication.snapshotKey,
      publication.commitsKey,
      commitIndexKey,
      publication.inventoryKey,
      commitPatchManifestKey,
      publication.objectManifestKey,
      ...commitPageKeys,
      ...publication.commitPatchParts.map((part) => part.key),
      ...publication.packParts.map((part) => part.key),
    ];
    const objects = await Promise.all(requiredKeys.map((key) => requireBucket(env).head(key)));
    const missing = requiredKeys.filter((_, index) => objects[index] == null);
    if (missing.length > 0) {
      return Response.json({ error: "publication_objects_missing", missing }, { status: 409 });
    }
    const objectByKey = new Map(
      requiredKeys.map((key, index) => [key, objects[index]] as const),
    );
    let immutablePublication: unknown;
    try {
      const storedPublication = await requireBucket(env).get(publicationKey);
      immutablePublication = storedPublication == null ? undefined : await storedPublication.json();
    } catch {
      return storageFailure("immutable publication metadata is invalid");
    }
    if (
      !isRepositoryPublication(immutablePublication) ||
      JSON.stringify(immutablePublication) !== JSON.stringify(publication)
    ) {
      return Response.json(
        { error: "publication_metadata_invalid" },
        { status: 409 },
      );
    }
    let commitPatchManifest: unknown;
    try {
      const storedManifest = await requireBucket(env).get(commitPatchManifestKey);
      commitPatchManifest = storedManifest == null ? undefined : await storedManifest.json();
    } catch {
      return storageFailure("published commit patch manifest is invalid");
    }
    if (
      !isCommitPatchManifest(commitPatchManifest, publication.head) ||
      commitPatchManifest.size !== publication.commitPatchSize
    ) {
      return Response.json(
        { error: "publication_commit_patch_manifest_invalid" },
        { status: 409 },
      );
    }
    const invalidCommitPatchParts = publication.commitPatchParts
      .filter((part) => objectByKey.get(part.key)?.size !== part.size)
      .map((part) => part.key);
    if (invalidCommitPatchParts.length > 0) {
      return Response.json(
        { error: "publication_commit_patch_parts_invalid", invalid: invalidCommitPatchParts },
        { status: 409 },
      );
    }
    const invalidPackParts = publication.packParts
      .filter((part) => objectByKey.get(part.key)?.size !== part.size)
      .map((part) => part.key);
    if (invalidPackParts.length > 0) {
      return Response.json(
        { error: "publication_pack_parts_invalid", invalid: invalidPackParts },
        { status: 409 },
      );
    }
    const storedManifest = await requireBucket(env).get(publication.objectManifestKey);
    if (storedManifest == null) {
      return Response.json(
        { error: "publication_objects_missing", missing: [publication.objectManifestKey] },
        { status: 409 },
      );
    }
    const manifest: unknown = await storedManifest.json();
    if (!isGitObjectManifest(manifest) || manifest.head !== publication.head) {
      return Response.json({ error: "invalid_object_manifest" }, { status: 409 });
    }
    const shards = await Promise.all(
      manifest.shards.map((shard) => requireBucket(env).head(shard.key)),
    );
    const missingShards = manifest.shards
      .filter((_, index) => shards[index] == null)
      .map((shard) => shard.key);
    if (missingShards.length > 0) {
      return Response.json(
        { error: "publication_objects_missing", missing: missingShards },
        { status: 409 },
      );
    }
    return repositoryStub(env).fetch("https://repository.internal/publication", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        expectedHead,
        publication,
        ...(body.replaceInvalid === true ? { replaceInvalid: true } : {}),
      }),
    });
  }

  const smartRoute = gitSmartRoute(url.pathname);
  if (smartRoute?.operation === "info-refs" && request.method === "GET") {
    if (url.searchParams.get("service") !== "git-upload-pack") {
      return new Response("unsupported service\n", { status: 400 });
    }
    return new Response(byteBody(repositoryAdvertisement()), {
      headers: {
        "cache-control": "no-cache",
        "content-type": "application/x-git-upload-pack-advertisement",
      },
    });
  }

  if (smartRoute?.operation === "upload-pack" && request.method === "POST") {
    const publication = smartRoute.generation == null
      ? await getPublication(env)
      : await getGenerationPublication(env, smartRoute.generation);
    if (publication instanceof Response) return publication;
    let command: ReturnType<typeof parseV2Command>;
    try {
      const body = await readGitProtocolRequest(request);
      if (body instanceof Response) return body;
      command = parseV2Command(body);
    } catch {
      return new Response("malformed git protocol request\n", { status: 400 });
    }
    if (command.command === "ls-refs") {
      return gitUploadResponse(byteBody(buildLsRefsResponse(publication, command.arguments)));
    }
    if (command.command !== "fetch") {
      return new Response("unsupported git protocol command\n", { status: 400 });
    }
    let fetchRequest: ReturnType<typeof parseFetchArguments>;
    try {
      fetchRequest = parseFetchArguments(command.arguments);
    } catch (error) {
      const message = error instanceof Error ? error.message : "invalid fetch arguments";
      return new Response(`${message}\n`, { status: 400 });
    }
    const manifest = await getObjectManifest(env, publication);
    if (manifest instanceof Response) return manifest;
    if (
      fetchRequest.wants.length === 0 ||
      fetchRequest.wants.some((oid) => manifest.objects[oid] == null)
    ) {
      return new Response("invalid fetch wants\n", { status: 400 });
    }
    const commonHaves = [...new Set(
      fetchRequest.haves.filter((oid) => manifest.objects[oid] != null),
    )];
    if (!fetchRequest.done) {
      return gitUploadResponse(byteBody(buildNegotiationResponse(commonHaves)));
    }
    if (
      fetchRequest.haves.length === 0 &&
      fetchRequest.shallow.length === 0 &&
      fetchRequest.deepen === 0
    ) {
      try {
        const pack = await createRepositoryPartsStream(
          requireBucket(env),
          publication.packParts,
        );
        return gitUploadResponse(buildFullPackResponse(pack));
      } catch {
        return storageFailure("published pack is missing or invalid");
      }
    }
    const selection = selectGitObjects(
      manifest,
      fetchRequest.wants,
      fetchRequest.haves,
      fetchRequest.shallow,
      fetchRequest.deepen,
      fetchRequest.deepenRelative,
    );
    return gitUploadResponse(buildFullPackResponse(
      createSelectedPackStream(requireBucket(env), manifest, selection.objectIds),
      selection.shallow,
      selection.unshallow,
    ));
  }

  return undefined;
}

type GitSmartRoute = Readonly<{
  generation?: string;
  operation: "info-refs" | "upload-pack";
}>;

/**
 * `/git/<head>` pins a complete smart-HTTP negotiation to one immutable
 * publication. Repository materializers resolve the current head once, then
 * clone this route so a concurrent publish cannot mix generations.
 */
function gitSmartRoute(pathname: string): GitSmartRoute | undefined {
  if (pathname === "/git/info/refs") return { operation: "info-refs" };
  if (pathname === "/git/git-upload-pack") return { operation: "upload-pack" };
  const match = pathname.match(
    /^\/git\/([a-f0-9]{40})\/(info\/refs|git-upload-pack)$/,
  );
  if (!match) return undefined;
  return {
    generation: match[1],
    operation: match[2] === "info/refs" ? "info-refs" : "upload-pack",
  };
}

function servePinnedGeneration(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  url: URL,
  objectName: "repository.json" | "commit-index.json",
): Response | Promise<Response> | undefined {
  const generation = requestedGeneration(url);
  if (generation instanceof Response) return generation;
  return generation == null
    ? undefined
    : serveObject(
        request,
        env,
        context,
        `generations/${generation}/${objectName}`,
        true,
        generation,
      );
}

function requestedGeneration(url: URL): string | null | Response {
  const generation = url.searchParams.get("generation");
  if (generation == null) return null;
  if (!SHA1_PATTERN.test(generation)) {
    return Response.json({ error: "invalid_repository_generation" }, { status: 400 });
  }
  return generation;
}

function immutableUploadResponse(key: string, object: R2Object, stored: boolean): Response {
  return Response.json({
    key,
    etag: object.httpEtag,
    size: object.size,
    stored,
  });
}

export async function readGitProtocolRequest(
  request: Request,
  maxBytes = 4 * 1024 * 1024,
): Promise<Uint8Array | Response> {
  const encoding = request.headers.get("content-encoding")?.trim().toLowerCase();
  if (encoding && encoding !== "identity" && encoding !== "gzip") {
    return new Response("unsupported upload-pack content encoding\n", { status: 415 });
  }
  if (request.body == null) return new Uint8Array();
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
        return new Response("Git request is too large\n", { status: 413 });
      }
      chunks.push(next.value);
    }
  } catch {
    return new Response("Git request body could not be decoded\n", { status: 400 });
  }
  const body = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

function gitUploadResponse(body: BodyInit): Response {
  return new Response(body, {
    headers: {
      "cache-control": "no-cache",
      "content-type": "application/x-git-upload-pack-result",
    },
  });
}

async function servePublishedObject(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  field: "snapshotKey" | "commitsKey",
  immutable: boolean,
): Promise<Response> {
  const cached = await matchEdgeCache(request);
  if (cached != null) return cached;
  const publication = await getPublication(env);
  if (publication instanceof Response) return publication;
  return serveObject(
    request,
    env,
    context,
    publication[field],
    immutable,
    publication.head,
    false,
  );
}

async function servePublishedGenerationObject(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  name: string,
): Promise<Response> {
  const cached = await matchEdgeCache(request);
  if (cached != null) return cached;
  const publication = await getPublication(env);
  if (publication instanceof Response) return publication;
  return serveObject(
    request,
    env,
    context,
    `generations/${publication.head}/${name}`,
    false,
    publication.head,
    false,
  );
}

async function servePublishedCommitPage(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  page: number,
): Promise<Response> {
  const cached = await matchEdgeCache(request);
  if (cached != null) return cached;
  const publication = await getPublication(env);
  if (publication instanceof Response) return publication;
  return serveObject(
    request,
    env,
    context,
    `generations/${publication.head}/commits/${String(page).padStart(4, "0")}.json`,
    false,
    publication.head,
    false,
  );
}

function serveCommitPage(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  generation: string,
  page: number,
): Promise<Response> {
  return serveObject(
    request,
    env,
    context,
    `generations/${generation}/commits/${String(page).padStart(4, "0")}.json`,
    true,
    generation,
  );
}

function serveCommitPatchPage(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  generation: string,
  page: number,
): Promise<Response> {
  return serveObject(
    request,
    env,
    context,
    `generations/${generation}/commit-patches/${String(page).padStart(4, "0")}.diff`,
    true,
    generation,
  );
}

async function serveCommitPatch(
  env: GitStorageEnv,
  generation: string,
): Promise<Response> {
  const manifestKey = `generations/${generation}/commit-patches.json`;
  let manifest: unknown;
  try {
    const storedManifest = await requireBucket(env).get(manifestKey);
    if (storedManifest == null) {
      return Response.json({ error: "repository_generation_not_published" }, { status: 404 });
    }
    manifest = await storedManifest.json();
  } catch {
    return storageFailure("published commit patch manifest is invalid");
  }
  if (!isCommitPatchManifest(manifest, generation)) {
    return storageFailure("published commit patch manifest is invalid");
  }
  let body: ReadableStream<Uint8Array>;
  try {
    body = await createRepositoryPartsStream(
      requireBucket(env),
      manifest.parts,
    );
  } catch {
    return storageFailure("published commit patch is missing or invalid");
  }
  const headers = new Headers({
    "cache-control": immutableCacheControl,
    "content-length": String(manifest.size),
    "content-type": "text/plain; charset=utf-8",
    "x-content-type-options": "nosniff",
    "x-repository-generation": generation,
  });
  return new Response(body, { headers });
}

async function serveObject(
  request: Request,
  env: GitStorageEnv,
  context: ExecutionContext | undefined,
  key: string,
  immutable: boolean,
  generation?: string,
  checkCache = true,
): Promise<Response> {
  const edgeCache = typeof caches === "undefined"
    ? undefined
    : (caches as CacheStorage & { default: Cache }).default;
  const cacheKey = repositoryCacheKey(request, key);
  if (checkCache) {
    const cached = await edgeCache?.match(cacheKey);
    if (cached != null) return cached;
  }
  const object = await requireBucket(env).get(key);
  if (object == null) return Response.json({ error: "repository_object_not_found" }, { status: 404 });
  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set("content-type", contentTypeForKey(key));
  headers.set(
    "cache-control",
    immutable ? immutableCacheControl : "public, max-age=60, stale-while-revalidate=300",
  );
  headers.set("etag", object.httpEtag);
  if (generation) headers.set("x-repository-generation", generation);
  headers.set("x-content-type-options", "nosniff");
  const response = new Response(object.body, { headers });
  if (edgeCache != null && context != null) {
    context.waitUntil(edgeCache.put(cacheKey, response.clone()));
  }
  return response;
}

function repositoryCacheKey(request: Request, key: string): Request {
  const url = new URL(request.url);
  if (key.endsWith(".diff") || key.endsWith(".patch")) {
    url.searchParams.set("__nanocodex_patch", "text");
  }
  return new Request(url, { method: "GET" });
}

async function matchEdgeCache(request: Request): Promise<Response | undefined> {
  const edgeCache = typeof caches === "undefined"
    ? undefined
    : (caches as CacheStorage & { default: Cache }).default;
  return edgeCache?.match(new Request(request.url, { method: "GET" }));
}

async function getPublication(env: GitStorageEnv): Promise<RepositoryPublication | Response> {
  if (!env.GIT_REPOSITORY || !env.GIT_OBJECTS) {
    return storageFailure("repository storage is not configured");
  }
  const response = await repositoryStub(env).fetch("https://repository.internal/publication");
  if (response.status === 404) {
    return Response.json({ error: "repository_not_published" }, { status: 503 });
  }
  if (!response.ok) return storageFailure("repository publication lookup failed");
  const publication: unknown = await response.json();
  return isRepositoryPublication(publication)
    ? publication
    : storageFailure("repository publication is invalid");
}

const generationPublicationMemo = new WeakMap<object, Map<string, RepositoryPublication>>();

async function getGenerationPublication(
  env: GitStorageEnv,
  generation: string,
): Promise<RepositoryPublication | Response> {
  if (!env.GIT_OBJECTS) return storageFailure("repository storage is not configured");
  const bucket = env.GIT_OBJECTS;
  let publications = generationPublicationMemo.get(bucket as object);
  const cached = publications?.get(generation);
  if (cached != null) return cached;
  const stored = await bucket.get(`generations/${generation}/publication.json`);
  if (stored == null) {
    return Response.json({ error: "repository_generation_not_found" }, { status: 404 });
  }
  let value: unknown;
  try {
    value = await stored.json();
  } catch {
    return storageFailure("repository generation is invalid");
  }
  if (!isRepositoryPublication(value) || value.head !== generation) {
    return storageFailure("repository generation is invalid");
  }
  if (publications == null) {
    publications = new Map();
    generationPublicationMemo.set(bucket as object, publications);
  }
  publications.set(generation, value);
  while (publications.size > 2) publications.delete(publications.keys().next().value!);
  return value;
}

const objectManifestMemo = new WeakMap<object, Map<string, GitObjectManifest>>();

async function getObjectManifest(
  env: GitStorageEnv,
  publication: RepositoryPublication,
): Promise<GitObjectManifest | Response> {
  const bucket = requireBucket(env);
  let manifests = objectManifestMemo.get(bucket as object);
  const cached = manifests?.get(publication.head);
  if (cached != null) return cached;
  const stored = await bucket.get(publication.objectManifestKey);
  if (stored == null) return storageFailure("published object manifest is missing");
  const value: unknown = await stored.json();
  if (!isGitObjectManifest(value) || value.head !== publication.head) {
    return storageFailure("published object manifest is invalid");
  }
  if (manifests == null) {
    manifests = new Map();
    objectManifestMemo.set(bucket as object, manifests);
  }
  manifests.set(publication.head, value);
  while (manifests.size > 2) manifests.delete(manifests.keys().next().value!);
  return value;
}

function repositoryStub(env: GitStorageEnv): DurableObjectStub {
  if (!env.GIT_REPOSITORY) throw new Error("GIT_REPOSITORY is not configured");
  return env.GIT_REPOSITORY.get(env.GIT_REPOSITORY.idFromName("nanocodex"));
}

function requireBucket(env: GitStorageEnv): R2Bucket {
  if (!env.GIT_OBJECTS) throw new Error("GIT_OBJECTS is not configured");
  return env.GIT_OBJECTS;
}

function objectKeyFromUploadPath(pathname: string): string | null {
  const relative = pathname.slice("/api/git/objects/".length);
  const blob = relative.match(/^blobs\/([a-f0-9]{40})$/);
  if (blob) return `blobs/${blob[1]}.txt`;
  const patch = relative.match(/^patches\/([a-f0-9]{40})$/);
  if (patch) return `patches/${patch[1]}.patch`;
  const generation = relative.match(/^generations\/([a-f0-9]{40})\/([^/]+)$/);
  if (generation && generationFilePattern.test(generation[2])) {
    return `generations/${generation[1]}/${generation[2]}`;
  }
  const commitPage = relative.match(
    /^generations\/([a-f0-9]{40})\/(commits\/\d{4}\.json)$/,
  );
  if (commitPage && generationCommitPagePattern.test(commitPage[2])) {
    return `generations/${commitPage[1]}/${commitPage[2]}`;
  }
  const commitPatchPart = relative.match(
    /^generations\/([a-f0-9]{40})\/(commit-patches\/\d{4}\.diff)$/,
  );
  if (commitPatchPart && generationCommitPatchPartPattern.test(commitPatchPart[2])) {
    return `generations/${commitPatchPart[1]}/${commitPatchPart[2]}`;
  }
  const objectShard = relative.match(
    /^generations\/([a-f0-9]{40})\/(objects\/\d{4}\.pack)$/,
  );
  if (objectShard && generationObjectShardPattern.test(objectShard[2])) {
    return `generations/${objectShard[1]}/${objectShard[2]}`;
  }
  const packPart = relative.match(
    /^generations\/([a-f0-9]{40})\/(packs\/[a-f0-9]{40}\/\d{4}\.pack)$/,
  );
  if (packPart && generationPackPartPattern.test(packPart[2])) {
    return `generations/${packPart[1]}/${packPart[2]}`;
  }
  return null;
}

function contentTypeForKey(key: string): string {
  if (key.endsWith(".json")) return "application/json; charset=utf-8";
  // `text/x-diff` is not in Cloudflare's default compression allowlist. These
  // are UTF-8 text streams, so use the standard text type and let the edge
  // negotiate Brotli/gzip without buffering or recompressing in the Worker.
  if (key.endsWith(".diff") || key.endsWith(".patch")) return "text/plain; charset=utf-8";
  if (key.endsWith(".pack") || key.endsWith(".idx")) return "application/octet-stream";
  return "text/plain; charset=utf-8";
}

async function authorizeMirrorRequest(request: Request, env: GitStorageEnv): Promise<boolean> {
  const expected = env.GIT_MIRROR_TOKEN ?? "";
  const presented = request.headers.get("authorization")?.match(/^Bearer\s+(.+)$/i)?.[1] ?? "";
  if (!expected || !presented) return false;
  const encoder = new TextEncoder();
  const [left, right] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(expected)),
    crypto.subtle.digest("SHA-256", encoder.encode(presented)),
  ]);
  const leftBytes = new Uint8Array(left);
  const rightBytes = new Uint8Array(right);
  let difference = 0;
  for (let index = 0; index < leftBytes.length; index++) {
    difference |= leftBytes[index]! ^ rightBytes[index]!;
  }
  return difference === 0;
}

function unauthorized(): Response {
  return Response.json(
    { error: "unauthorized" },
    { status: 401, headers: { "cache-control": "no-store", "www-authenticate": "Bearer" } },
  );
}

function storageFailure(message: string): Response {
  return Response.json(
    { error: message },
    { status: 503, headers: { "cache-control": "no-store" } },
  );
}

function byteBody(bytes: Uint8Array): ArrayBuffer {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}
