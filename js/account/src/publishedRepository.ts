import type {
  HarnessCommit,
  RepositoryFile,
} from "./threadRepositorySnapshot";

type PublishedRepositoryFile = RepositoryFile & {
  contentUrl: string | null;
};

type PublishedRepositoryMetadata = {
  repository: {
    fullName: string;
    branch: string;
    head: string;
    totalCommits: number;
    indexedCommits: number;
    commitPageSize: number;
    dirty: boolean;
    dirtyCount: number;
  };
  generatedAt: string;
};

type PublishedRepositoryDocument = PublishedRepositoryMetadata & {
  tree: PublishedRepositoryFile[];
};

export type CommitScope = "all" | "eval" | "fix" | "docs" | "perf";

type PublishedCommitIndexDocument = PublishedRepositoryMetadata & {
  version: 1;
  hashes: string[];
  scopeCounts: Record<CommitScope, number>;
};

export type PublishedRepositorySnapshot = PublishedRepositoryDocument & {
  readFile(file: RepositoryFile): Promise<string>;
};

export type PreparedPublishedFile = {
  contents: string;
  file: RepositoryFile;
};

export type PublishedCommitPatchUrl =
  | string
  | ((commit: HarnessCommit) => string);

export type PublishedCommitPage = {
  generation: string;
  index: number;
  commits: HarnessCommit[];
  patchUrl: PublishedCommitPatchUrl;
};

export type PublishedCommitHistory = PublishedRepositoryMetadata & {
  hashes: readonly string[];
  initialCommitHash: string;
  initialPage: PublishedCommitPage;
  pageCount: number;
  pageSize: number;
  scopeCounts: Readonly<Record<CommitScope, number>>;
  pageForCommit(hash: string): number | undefined;
  loadPage(page: number): Promise<PublishedCommitPage>;
  loadAllPages(): Promise<PublishedCommitPage[]>;
};

type Fetch = typeof fetch;

type PrefetchedPatch = {
  controller: AbortController;
  expiry?: ReturnType<typeof setTimeout>;
  response: Promise<Response>;
};

const SHA1_PATTERN = /^[a-f0-9]{40}$/;
const ALL_PAGE_LOAD_CONCURRENCY = 4;
const MAX_COMMIT_INDEX_BYTES = 512 * 1024;
const MAX_COMMIT_PAGE_BYTES = 2 * 1024 * 1024;
const MAX_COMMIT_PAGE_COUNT = 256;
const MAX_COMMIT_PAGE_SIZE = 32;
const PREFETCHED_PATCH_RETENTION_MS = 30_000;
const ADOPTED_PATCH_RETENTION_MS = 5 * 60_000;
const DEPLOYMENT_META_NAME = "nanocodex-deployment-sha";

let snapshotPreload: Promise<PublishedRepositorySnapshot> | undefined;
let commitIndexPreload: Promise<PublishedCommitIndexDocument> | undefined;
const commitPagePreloads = new Map<string, Promise<PublishedCommitPage>>();
const prefetchedPatches = new Map<string, PrefetchedPatch>();

export async function loadPublishedRepositorySnapshot(
  request: Fetch = fetch,
  development = import.meta.env?.DEV ?? false,
  generation = publishedRepositoryGeneration(),
): Promise<PublishedRepositorySnapshot> {
  if (request === fetch && !development) {
    return preloadPublishedRepositorySnapshot();
  }
  return loadPublishedRepositorySnapshotUncached(request, development, generation);
}

export function preloadPublishedRepositorySnapshot(): Promise<PublishedRepositorySnapshot> {
  if (snapshotPreload) return snapshotPreload;
  const loading = loadPublishedRepositorySnapshotUncached(
    fetch,
    false,
    publishedRepositoryGeneration(),
  ).catch(
    (error) => {
      if (snapshotPreload === loading) snapshotPreload = undefined;
      throw error;
    },
  );
  snapshotPreload = loading;
  return loading;
}

export function preloadPreferredPublishedFile(
  snapshot: PublishedRepositorySnapshot,
  search = typeof window === "undefined" ? "" : window.location.search,
): Promise<PreparedPublishedFile> | undefined {
  const requestedPath = new URLSearchParams(search).get("path");
  const preferredFile = snapshot.tree.find((file) =>
    file.path === requestedPath && file.contentUrl != null
  ) ?? snapshot.tree.find((file) =>
    file.path === "src/main.rs" && file.contentUrl != null
  ) ?? snapshot.tree.find((file) =>
    file.path === "README.md" && file.contentUrl != null
  ) ?? snapshot.tree.find((file) => file.contentUrl != null);
  return preferredFile == null
    ? undefined
    : snapshot.readFile(preferredFile).then((contents) => ({
        contents,
        file: preferredFile,
      }));
}

export async function loadPublishedCommitHistory(
  requestedHash?: string,
  request: Fetch = fetch,
  development = import.meta.env?.DEV ?? false,
  generation = publishedRepositoryGeneration(),
  adopted?: Promise<void>,
): Promise<PublishedCommitHistory> {
  const index = request === fetch && !development
    ? await preloadPublishedCommitIndex()
    : await loadPublishedCommitIndexUncached(request, development, generation);
  const base = "/api/repository";
  const pageSize = index.repository.commitPageSize;
  const pageCount = Math.ceil(index.hashes.length / pageSize);
  const pageByHash = new Map(
    index.hashes.map((hash, commitIndex) => [
      hash,
      Math.floor(commitIndex / pageSize),
    ]),
  );
  const initialCommitHash = requestedHash ?? index.repository.head;
  const initialPageIndex = pageByHash.get(initialCommitHash);
  if (initialPageIndex == null) {
    throw new Error(`Published commit ${initialCommitHash} was not found`);
  }

  const initialPatchUrl = `${base}/commits/${index.repository.head}/${String(initialPageIndex).padStart(4, "0")}.diff`;
  if (request === fetch && initialPatchUrl != null) {
    void preloadPublishedRepositoryPatch(initialPatchUrl)?.catch(() => undefined);
    if (adopted) {
      void adopted.then(() => {
        adoptPublishedRepositoryPatch(initialPatchUrl);
      });
    }
  }

  const localPages = new Map<number, Promise<PublishedCommitPage>>();
  const loadPage = (page: number): Promise<PublishedCommitPage> => {
    if (!Number.isSafeInteger(page) || page < 0 || page >= pageCount) {
      return Promise.reject(new Error(`Commit page ${page} is out of range`));
    }
    const cacheKey = `${index.repository.head}:${page}`;
    const useGlobalCache = request === fetch && !development;
    const existing = useGlobalCache
      ? commitPagePreloads.get(cacheKey)
      : localPages.get(page);
    if (existing) return existing;

    const pageUrl = `${base}/commits?${new URLSearchParams({
      generation: index.repository.head,
      page: String(page),
    })}`;
    const loading = request(pageUrl, {
      cache: development ? "no-store" : "default",
    }).then(async (response) => {
      if (!response.ok) {
        throw new Error(`Commit page request failed (${response.status})`);
      }
      requireGeneration(response, index.repository.head, false);
      const body = await readBoundedJson(
        response,
        MAX_COMMIT_PAGE_BYTES,
        `Commit page ${page}`,
      );
      requireCommitPage(
        body,
        index.hashes.slice(page * pageSize, (page + 1) * pageSize),
      );
      markCommitPerformance("repository-commit-page-parsed", {
        commitCount: body.length,
        page,
      });
      return {
        generation: index.repository.head,
        index: page,
        commits: body,
        patchUrl: `${base}/commits/${index.repository.head}/${String(page).padStart(4, "0")}.diff`,
      } satisfies PublishedCommitPage;
    }).catch((error) => {
      if (commitPagePreloads.get(cacheKey) === loading) {
        commitPagePreloads.delete(cacheKey);
      }
      if (localPages.get(page) === loading) localPages.delete(page);
      throw error;
    });
    if (useGlobalCache) commitPagePreloads.set(cacheKey, loading);
    else localPages.set(page, loading);
    return loading;
  };

  const initialPage = await loadPage(initialPageIndex);
  return {
    repository: index.repository,
    generatedAt: index.generatedAt,
    hashes: index.hashes,
    initialCommitHash,
    initialPage,
    pageCount,
    pageSize,
    scopeCounts: index.scopeCounts,
    pageForCommit: (hash) => pageByHash.get(hash),
    loadPage,
    loadAllPages: () => mapConcurrentPages(pageCount, loadPage),
  };
}

export function preloadPublishedRepositoryPatch(
  patchUrl: PublishedCommitPatchUrl,
): Promise<Response> | undefined {
  if (typeof patchUrl !== "string") return undefined;
  const existing = prefetchedPatches.get(patchUrl);
  if (existing != null) {
    retainPrefetchedPatch(patchUrl, existing);
    return existing.response;
  }
  const controller = new AbortController();
  markCommitPerformance("patch-prefetch-start");
  let prefetched!: PrefetchedPatch;
  const response = fetch(patchUrl, {
    cache: "default",
    signal: controller.signal,
  }).then((result) => {
    markCommitPerformance("patch-prefetch-headers");
    return result;
  }).catch((error) => {
    if (prefetchedPatches.get(patchUrl) === prefetched) {
      prefetchedPatches.delete(patchUrl);
    }
    throw error;
  });
  prefetched = { controller, response } satisfies PrefetchedPatch;
  prefetchedPatches.set(patchUrl, prefetched);
  retainPrefetchedPatch(patchUrl, prefetched);
  return response;
}

export function adoptPublishedRepositoryPatch(patchUrl: string): void {
  const prefetched = prefetchedPatches.get(patchUrl);
  if (prefetched) retainPrefetchedPatch(patchUrl, prefetched, ADOPTED_PATCH_RETENTION_MS);
}

function retainPrefetchedPatch(
  patchUrl: string,
  prefetched: PrefetchedPatch,
  retentionMs = PREFETCHED_PATCH_RETENTION_MS,
): void {
  clearTimeout(prefetched.expiry);
  prefetched.expiry = undefined;
  prefetched.expiry = setTimeout(() => {
    if (prefetchedPatches.get(patchUrl) !== prefetched) return;
    prefetched.controller.abort("unused patch preload expired");
    prefetchedPatches.delete(patchUrl);
  }, retentionMs);
}

export function fetchPublishedRepositoryPatch(
  patchUrl: string,
  signal: AbortSignal,
): Promise<Response> {
  const prefetched = prefetchedPatches.get(patchUrl);
  if (prefetched == null) {
    return fetch(patchUrl, { cache: "default", signal });
  }

  prefetchedPatches.delete(patchUrl);
  clearTimeout(prefetched.expiry);
  if (signal.aborted) prefetched.controller.abort(signal.reason);
  else {
    signal.addEventListener(
      "abort",
      () => prefetched.controller.abort(signal.reason),
      { once: true },
    );
  }
  return prefetched.response;
}

async function loadPublishedRepositorySnapshotUncached(
  request: Fetch,
  development: boolean,
  generation?: string,
): Promise<PublishedRepositorySnapshot> {
  const base = "/api/repository";
  markCommitPerformance("repository-request-start");
  const mutableUrl = `${base}/snapshot`;
  const response = await requestPublishedMetadata(
    request,
    generation == null ? mutableUrl : `${mutableUrl}?generation=${generation}`,
    mutableUrl,
    development,
  );
  if (!response.ok) {
    throw new Error(`Repository request failed (${response.status})`);
  }
  const snapshot: unknown = await response.json();
  requireRepositoryDocument(snapshot);
  requireGeneration(response, snapshot.repository.head, false);
  markCommitPerformance("repository-snapshot-parsed");

  const fileContents = new Map<string, Promise<string>>();
  return {
    ...snapshot,
    async readFile(file) {
      const cached = fileContents.get(file.objectId);
      if (cached) return cached;
      const published = snapshot.tree.find((candidate) =>
        candidate.objectId === file.objectId && candidate.path === file.path
      );
      if (published?.contentUrl == null) {
        throw new Error(`${file.path} is not available as published text`);
      }
      const pending = request(published.contentUrl, {
        cache: development ? "no-store" : "default",
      }).then((fileResponse) => {
        if (!fileResponse.ok) {
          throw new Error(`File request failed (${fileResponse.status})`);
        }
        return fileResponse.text();
      }).catch((error) => {
        if (fileContents.get(file.objectId) === pending) {
          fileContents.delete(file.objectId);
        }
        throw error;
      });
      fileContents.set(file.objectId, pending);
      return pending;
    },
  };
}

function preloadPublishedCommitIndex(): Promise<PublishedCommitIndexDocument> {
  if (commitIndexPreload) return commitIndexPreload;
  const loading = loadPublishedCommitIndexUncached(
    fetch,
    false,
    publishedRepositoryGeneration(),
  ).catch((error) => {
    if (commitIndexPreload === loading) commitIndexPreload = undefined;
    throw error;
  });
  commitIndexPreload = loading;
  return loading;
}

async function loadPublishedCommitIndexUncached(
  request: Fetch,
  development: boolean,
  generation?: string,
): Promise<PublishedCommitIndexDocument> {
  const base = "/api/repository";
  markCommitPerformance("repository-commit-index-request-start");
  const mutableUrl = `${base}/commit-index`;
  const response = await requestPublishedMetadata(
    request,
    generation == null ? mutableUrl : `${mutableUrl}?generation=${generation}`,
    mutableUrl,
    development,
  );
  if (!response.ok) {
    throw new Error(`Commit index request failed (${response.status})`);
  }
  const index = await readBoundedJson(
    response,
    MAX_COMMIT_INDEX_BYTES,
    "Commit index",
  );
  requireCommitIndex(index);
  requireGeneration(response, index.repository.head, false);
  markCommitPerformance("repository-commit-index-parsed", {
    commitCount: index.hashes.length,
  });
  return index;
}

async function requestPublishedMetadata(
  request: Fetch,
  url: string,
  mutableUrl: string,
  development: boolean,
): Promise<Response> {
  const init: RequestInit = {
    cache: development ? "no-store" : "default",
  };
  const response = await request(url, init);
  return response.status === 404 && url !== mutableUrl
    ? request(mutableUrl, init)
    : response;
}

export function publishedRepositoryGeneration(): string | undefined {
  if (typeof document === "undefined") return undefined;
  const generation = document
    .querySelector<HTMLMetaElement>(`meta[name="${DEPLOYMENT_META_NAME}"]`)
    ?.content;
  return generation != null && SHA1_PATTERN.test(generation)
    ? generation
    : undefined;
}

async function mapConcurrentPages(
  pageCount: number,
  loadPage: (page: number) => Promise<PublishedCommitPage>,
): Promise<PublishedCommitPage[]> {
  const pages = new Array<PublishedCommitPage>(pageCount);
  let nextPage = 0;
  await Promise.all(Array.from(
    { length: Math.min(pageCount, ALL_PAGE_LOAD_CONCURRENCY) },
    async () => {
      for (;;) {
        const page = nextPage++;
        if (page >= pageCount) return;
        pages[page] = await loadPage(page);
      }
    },
  ));
  return pages;
}

function markCommitPerformance(
  name: string,
  detail?: Record<string, number>,
): void {
  if (typeof performance === "undefined") return;
  performance.mark(`nanocodex:commits:${name}`, { detail });
}

function requireRepositoryMetadata(
  value: unknown,
): asserts value is PublishedRepositoryMetadata {
  if (
    value == null ||
    typeof value !== "object" ||
    !("repository" in value) ||
    value.repository == null ||
    typeof value.repository !== "object" ||
    !("head" in value.repository) ||
    !SHA1_PATTERN.test(String(value.repository.head)) ||
    !("branch" in value.repository) ||
    typeof value.repository.branch !== "string" ||
    !("indexedCommits" in value.repository) ||
    typeof value.repository.indexedCommits !== "number" ||
    !Number.isSafeInteger(value.repository.indexedCommits) ||
    value.repository.indexedCommits < 1 ||
    !("commitPageSize" in value.repository) ||
    typeof value.repository.commitPageSize !== "number" ||
    !Number.isSafeInteger(value.repository.commitPageSize) ||
    value.repository.commitPageSize < 1 ||
    value.repository.commitPageSize > MAX_COMMIT_PAGE_SIZE ||
    !("generatedAt" in value) ||
    typeof value.generatedAt !== "string"
  ) {
    throw new Error("Published repository metadata is invalid");
  }
}

function requireRepositoryDocument(
  value: unknown,
): asserts value is PublishedRepositoryDocument {
  requireRepositoryMetadata(value);
  if (!("tree" in value) || !Array.isArray(value.tree)) {
    throw new Error("Repository snapshot is invalid");
  }
}

function requireCommitIndex(
  value: unknown,
): asserts value is PublishedCommitIndexDocument {
  requireRepositoryMetadata(value);
  if (
    !("version" in value) ||
    value.version !== 1 ||
    !("hashes" in value) ||
    !Array.isArray(value.hashes) ||
    value.hashes.length !== value.repository.indexedCommits ||
    !value.hashes.every((hash) => typeof hash === "string" && SHA1_PATTERN.test(hash)) ||
    new Set(value.hashes).size !== value.hashes.length ||
    value.hashes[0] !== value.repository.head ||
    Math.ceil(value.hashes.length / value.repository.commitPageSize) >
      MAX_COMMIT_PAGE_COUNT ||
    !("scopeCounts" in value) ||
    !isCommitScopeCounts(value.scopeCounts, value.hashes.length)
  ) {
    throw new Error("Published commit index is invalid");
  }
}

function isCommitScopeCounts(
  value: unknown,
  total: number,
): value is Record<CommitScope, number> {
  if (value == null || typeof value !== "object") return false;
  const counts = value as Record<string, unknown>;
  return counts.all === total && ["eval", "fix", "docs", "perf"].every(
    (scope) =>
      Number.isSafeInteger(counts[scope]) &&
      Number(counts[scope]) >= 0 &&
      Number(counts[scope]) <= total,
  );
}

function requireCommitPage(
  value: unknown,
  expectedHashes: readonly string[],
): asserts value is HarnessCommit[] {
  if (
    !Array.isArray(value) ||
    value.length !== expectedHashes.length ||
    value.some((commit, index) =>
      commit == null ||
      typeof commit !== "object" ||
      !("hash" in commit) ||
      commit.hash !== expectedHashes[index] ||
      !("shortHash" in commit) ||
      typeof commit.shortHash !== "string" ||
      !("author" in commit) ||
      typeof commit.author !== "string" ||
      !("authoredAt" in commit) ||
      typeof commit.authoredAt !== "string" ||
      !("subject" in commit) ||
      typeof commit.subject !== "string" ||
      !("body" in commit) ||
      typeof commit.body !== "string" ||
      !("parents" in commit) ||
      !isStringArray(commit.parents) ||
      !("refs" in commit) ||
      !isStringArray(commit.refs) ||
      !("files" in commit) ||
      !Array.isArray(commit.files) ||
      !("stats" in commit) ||
      !isCommitStats(commit.stats)
    )
  ) {
    throw new Error("Published commit page is invalid");
  }
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isCommitStats(
  value: unknown,
): value is { files: number; additions: number; deletions: number } {
  if (value == null || typeof value !== "object") return false;
  const stats = value as Record<string, unknown>;
  return ["files", "additions", "deletions"].every((field) =>
    Number.isSafeInteger(stats[field]) && Number(stats[field]) >= 0
  );
}

async function readBoundedJson(
  response: Response,
  maximumBytes: number,
  description: string,
): Promise<unknown> {
  const contentLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > maximumBytes) {
    await response.body?.cancel();
    throw new Error(`${description} exceeded its ${maximumBytes}-byte limit`);
  }

  const reader = response.body?.getReader();
  let text = "";
  let observedBytes = 0;
  if (reader == null) {
    text = await response.text();
    observedBytes = new TextEncoder().encode(text).byteLength;
  } else {
    const decoder = new TextDecoder();
    try {
      for (;;) {
        const result = await reader.read();
        if (result.done) break;
        observedBytes += result.value.byteLength;
        if (observedBytes > maximumBytes) {
          await reader.cancel(`${description} exceeded its byte limit`);
          throw new Error(`${description} exceeded its ${maximumBytes}-byte limit`);
        }
        text += decoder.decode(result.value, { stream: true });
      }
      text += decoder.decode();
    } finally {
      reader.releaseLock();
    }
  }
  if (observedBytes > maximumBytes) {
    throw new Error(`${description} exceeded its ${maximumBytes}-byte limit`);
  }
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new Error(`${description} is not valid JSON`);
  }
}

function requireGeneration(
  response: Response,
  expected: string,
  allowMissing: boolean,
): void {
  const generation = response.headers.get("x-repository-generation");
  if ((!allowMissing && generation == null) || (generation != null && generation !== expected)) {
    throw new Error("Repository publication changed while loading");
  }
}
