import { preloadChangelog } from "./Changelog";
import { preloadDocsRoute } from "./Docs";
import { preloadEvalOverview } from "./Evals";
import { surfaceFromUrl, type Surface } from "./navigation";
import {
  loadPublishedCommitHistory,
  loadPublishedRepositorySnapshot,
  preloadPreferredPublishedFile,
  preloadPublishedRepositoryPatch,
  type PublishedCommitHistory,
  type PreparedPublishedFile,
  type PublishedRepositorySnapshot,
} from "./publishedRepository";
import {
  preloadPierreFile,
  preloadPierrePaths,
  preloadPierreWorker,
} from "./pierreWorkerResource";

const COMMIT_HASH_PATTERN = /^[0-9a-f]{40}$/;
const PREPARED_SURFACE_RETENTION_MS = 25_000;

export type PreparedDirectRoute = {
  commitHistory?: PublishedCommitHistory;
  sourceFile?: PreparedPublishedFile;
  repositorySnapshot?: PublishedRepositorySnapshot;
};

export type PreparedRepositorySurface =
  | {
      surface: "code";
      snapshot: PublishedRepositorySnapshot;
      sourceFile?: PreparedPublishedFile;
    }
  | {
      surface: "commits";
      history: PublishedCommitHistory;
      requestedCommit?: string;
    };

type PreparedCodeSurface = Extract<
  PreparedRepositorySurface,
  { surface: "code" }
>;
type PreparedCommitSurface = Extract<
  PreparedRepositorySurface,
  { surface: "commits" }
>;

type PreparedRepositoryRequest = {
  adopted: boolean;
  adopt(): void;
  expiry?: ReturnType<typeof setTimeout>;
  loading: Promise<PreparedRepositorySurface>;
  settled: boolean;
};

let repositorySnapshotRequest: Promise<PublishedRepositorySnapshot> | undefined;
const repositorySurfaceRequests = new Map<string, PreparedRepositoryRequest>();

export { preloadEvalOverview };

export function prepareRepositorySurface(
  surface: Extract<Surface, "code" | "commits">,
  requestedCommit?: string,
  adopt = false,
): Promise<PreparedRepositorySurface> {
  const key = preparedRepositoryKey(surface, requestedCommit);
  const existing = repositorySurfaceRequests.get(key);
  if (existing) {
    if (adopt) existing.adopt();
    return existing.loading;
  }

  let resolveAdopted!: () => void;
  const adopted = new Promise<void>((resolve) => {
    resolveAdopted = resolve;
  });
  const loading = surface === "code"
    ? prepareCodeSurface()
    : prepareCommitSurface(requestedCommit, adopted);
  const prepared: PreparedRepositoryRequest = {
    adopted: false,
    adopt: () => {
      if (prepared.adopted) return;
      prepared.adopted = true;
      clearTimeout(prepared.expiry);
      resolveAdopted();
      if (
        prepared.settled
        && repositorySurfaceRequests.get(key) === prepared
      ) {
        repositorySurfaceRequests.delete(key);
      }
    },
    loading,
    settled: false,
  };
  repositorySurfaceRequests.set(key, prepared);
  if (adopt) prepared.adopt();
  void loading.then(
    () => {
      prepared.settled = true;
      if (prepared.adopted) {
        if (repositorySurfaceRequests.get(key) === prepared) {
          repositorySurfaceRequests.delete(key);
        }
        return;
      }
      prepared.expiry = setTimeout(() => {
        if (repositorySurfaceRequests.get(key) === prepared) {
          repositorySurfaceRequests.delete(key);
        }
      }, PREPARED_SURFACE_RETENTION_MS);
    },
    () => {
      if (repositorySurfaceRequests.get(key) === prepared) {
        repositorySurfaceRequests.delete(key);
      }
    },
  );
  return loading;
}

function preparedRepositoryKey(
  surface: Extract<Surface, "code" | "commits">,
  requestedCommit?: string,
): string {
  return surface === "code" ? surface : `${surface}:${requestedCommit ?? "head"}`;
}

async function prepareCodeSurface(search?: string): Promise<PreparedCodeSurface> {
  const snapshotRequest = loadRepositorySnapshot();
  preloadPierreWorker();
  const sourceFileRequest = snapshotRequest.then((snapshot) =>
    preloadPreferredPublishedFile(snapshot, search)
  );
  const preparedSourceFileRequest = sourceFileRequest
    .then(async (sourceFile) => {
      if (sourceFile) {
        await preloadPierreFile(sourceFile.file, sourceFile.contents);
      }
      return sourceFile;
    });
  const [snapshot, sourceFile] = await Promise.all([
    snapshotRequest,
    preparedSourceFileRequest,
  ]);
  return { sourceFile, surface: "code", snapshot };
}

async function prepareCommitSurface(
  requestedCommit?: string,
  adopted: Promise<void> = Promise.resolve(),
): Promise<PreparedCommitSurface> {
  const historyRequest = loadPublishedCommitHistory(
    requestedCommit,
    undefined,
    undefined,
    undefined,
    adopted,
  );
  preloadPierreWorker();
  void historyRequest.then((history) => {
    preloadPublishedRepositoryPatch(history.initialPage.patchUrl);
    const initialPaths = history.initialPage.commits[0]?.files
      .map(({ path }) => path) ?? [];
    return preloadPierrePaths(initialPaths);
  }).catch(() => undefined);
  const history = await historyRequest;
  return {
    surface: "commits",
    history,
    requestedCommit,
  };
}

function loadRepositorySnapshot(): Promise<PublishedRepositorySnapshot> {
  if (repositorySnapshotRequest) return repositorySnapshotRequest;
  const loading = loadPublishedRepositorySnapshot()
    .catch((error) => {
      if (repositorySnapshotRequest === loading) {
        repositorySnapshotRequest = undefined;
      }
      throw error;
    });
  repositorySnapshotRequest = loading;
  return loading;
}

export async function preloadDirectSurface(url: URL): Promise<PreparedDirectRoute> {
  const surface = surfaceFromUrl(url);
  if (surface === "home" || surface === "agent") {
    return {};
  }
  if (surface === "multiplayer") {
    return {};
  }
  if (surface === "world") {
    return {};
  }
  if (surface === "code") {
    const prepared = await prepareCodeSurface(url.search);
    return {
      repositorySnapshot: prepared.snapshot,
      sourceFile: prepared.sourceFile,
    };
  }
  if (surface === "commits") {
    const prepared = await prepareCommitSurface(commitHashFromUrl(url));
    return { commitHistory: prepared.history };
  }
  if (surface === "changelog") {
    await preloadChangelog();
    return {};
  }
  if (surface === "docs") {
    await preloadDocsRoute(url.pathname);
    return {};
  }
  if (surface === "evals" && url.pathname.replace(/\/+$/, "") === "/evals") {
    await preloadEvalOverview();
  }
  return {};
}

function commitHashFromUrl(url: URL): string | undefined {
  const hash = url.searchParams.get("commit")?.toLowerCase();
  return hash && COMMIT_HASH_PATTERN.test(hash) ? hash : undefined;
}
