import {
  parsePatchFiles,
  processFile,
  type CodeViewItem,
} from "@pierre/diffs";
import { type CodeViewHandle, useStableCallback } from "@pierre/diffs/react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import {
  appendCommitItemToCommitData,
  appendFileDiffToCommitData,
  createCommitDataAccumulator,
  takePendingCommitItems,
  type CommitItemIdRename,
  type CommitStreamItem,
} from "./commitDataAccumulator";
import {
  COMMIT_HASH_METADATA_PATTERN,
  getPatchMetadataHashes,
  getPatchTreePathPrefix,
} from "./commitPatchMetadata";
import { inspectStreamedCommitFileSections } from "./commitPatchSections";
import {
  CODE_VIEW_BATCH_COUNT,
  COMMIT_INITIAL_BATCH_COUNT,
} from "./pierreCodeView";
import {
  fetchPublishedRepositoryPatch,
  type PublishedCommitHistory,
  type PublishedCommitPage,
  type PublishedCommitPatchUrl,
} from "./publishedRepository";
import { streamGitPatchFiles } from "./streamGitPatchFiles";
import type { HarnessCommit } from "./threadRepositorySnapshot";

const STREAM_PUBLISH_INTERVAL_MS = 100;
const STREAM_INITIAL_PUBLISH_INTERVAL_MS = 500;
const STREAM_WORK_BUDGET_MS = 8;
const MAX_PATCH_SHARD_BYTES = 16 * 1024 * 1024;

export type CommitStreamLoadState =
  | "fetching"
  | "parsing"
  | "streaming"
  | "ready"
  | "error";

interface UseCommitStreamLoaderOptions {
  collapseMode: "expanded" | "collapsed";
  history: PublishedCommitHistory;
  initialPage: PublishedCommitPage;
  onItemsPublished?(): void;
  onPageLoaded?(page: PublishedCommitPage): void;
  preparationBatchSize: number;
  prepareItems(items: readonly CodeViewItem<undefined>[]): Promise<void>;
  viewerRef: RefObject<CodeViewHandle<undefined> | null>;
}

export type CommitStreamErrorMode = "none" | "cold" | "tail";

export interface PendingCommitJumpRef {
  current: string | null;
}

function commitItemId(hash: string): string {
  return `commit:${hash}`;
}

export function getCommitStreamErrorMode(
  loadState: CommitStreamLoadState,
  hasPublishedItems: boolean,
): CommitStreamErrorMode {
  if (loadState !== "error") return "none";
  return hasPublishedItems ? "tail" : "cold";
}

export function shouldRequestNextCommitPage(
  scrollTop: number,
  viewportHeight: number,
  scrollHeight: number,
): boolean {
  if (
    !Number.isFinite(scrollTop) ||
    !Number.isFinite(viewportHeight) ||
    !Number.isFinite(scrollHeight) ||
    viewportHeight <= 0 ||
    scrollHeight <= 0
  ) {
    return false;
  }
  return scrollTop + viewportHeight * 2 >= scrollHeight;
}

export function tryApplyPendingCommitJump(
  pendingJump: PendingCommitJumpRef,
  viewer: Pick<CodeViewHandle<undefined>, "getItem" | "scrollTo"> | null,
): boolean {
  const hash = pendingJump.current;
  if (hash == null || viewer == null) return false;
  const itemId = commitItemId(hash);
  if (viewer.getItem(itemId) == null) return false;
  viewer.scrollTo({ type: "item", id: itemId, align: "start" });
  pendingJump.current = null;
  return true;
}

function createCommitItem(commit: HarnessCommit): CommitStreamItem {
  return {
    id: commitItemId(commit.hash),
    type: "file",
    collapsed: true,
    file: {
      name: commit.subject,
      contents: "",
      lang: "markdown",
      cacheKey: `${commit.hash}:message`,
    },
  };
}

export function useCommitStreamLoader({
  collapseMode,
  history,
  initialPage,
  onItemsPublished,
  onPageLoaded,
  preparationBatchSize,
  prepareItems,
  viewerRef,
}: UseCommitStreamLoaderOptions) {
  const [initialItems, setInitialItems] = useState<CodeViewItem<undefined>[]>(
    [],
  );
  const [loadState, setLoadState] =
    useState<CommitStreamLoadState>("fetching");
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [viewerKey, setViewerKey] = useState(0);
  const requestIdRef = useRef(0);
  const loadedItemIdsRef = useRef<Set<string>>(new Set());
  const requestNextPageRef = useRef<() => void>(() => undefined);
  const collapseModeRef = useRef(collapseMode);
  collapseModeRef.current = collapseMode;
  const publishItems = useStableCallback(() => onItemsPublished?.());
  const publishPage = useStableCallback((page: PublishedCommitPage) =>
    onPageLoaded?.(page)
  );
  const preparePendingItems = useStableCallback(prepareItems);
  const preparedItemBatchSize = Math.max(1, Math.floor(preparationBatchSize));

  const prepareItemsForViewer = (
    items: readonly CodeViewItem<undefined>[],
    loadedItemIds = loadedItemIdsRef.current,
  ): void => {
    const targetCollapsed = collapseModeRef.current === "collapsed";
    for (const item of items) {
      loadedItemIds.add(item.id);
      if (item.type === "diff") item.collapsed = targetCollapsed;
    }
  };

  const applyCollapseModeToLoaded = useStableCallback(
    (mode: "expanded" | "collapsed") => {
      const targetCollapsed = mode === "collapsed";
      const viewer = viewerRef.current;
      if (viewer == null) {
        setInitialItems((previous) => {
          let changed = false;
          const next = previous.map((item) => {
            if (
              item.type !== "diff" ||
              (item.collapsed === true) === targetCollapsed
            ) {
              return item;
            }
            changed = true;
            return { ...item, collapsed: targetCollapsed };
          });
          return changed ? next : previous;
        });
        return;
      }

      for (const itemId of loadedItemIdsRef.current) {
        const item = viewer.getItem(itemId);
        if (item == null || item.type !== "diff") continue;
        if ((item.collapsed === true) === targetCollapsed) continue;
        item.collapsed = targetCollapsed;
        item.version = getNextItemVersion(item);
        viewer.updateItem(item);
      }
    },
  );

  useEffect(() => {
    const controller = new AbortController();
    const requestId = ++requestIdRef.current;
    const isCurrentRequest = () =>
      requestIdRef.current === requestId && !controller.signal.aborted;
    const nextLoadedItemIds = new Set<string>();
    const accumulator = createCommitDataAccumulator();
    const queuedCommitHashes = new Set<string>();
    let pendingPublishFileCount = 0;
    let hasPublishedInitialItems = false;
    let lastPublishTime = performance.now();
    let lastWorkYieldTime = lastPublishTime;
    let nextPageIndex = initialPage.index + 1;
    let pageLoading = false;
    let nextPageRequested = false;
    let failed = false;

    setLoadState("fetching");
    markCommitPerformance("patch-window-start", {
      page: initialPage.index,
      requestId,
    });

    const queueCommitSection = (commit: HarnessCommit) => {
      if (queuedCommitHashes.has(commit.hash)) return;
      queuedCommitHashes.add(commit.hash);
      appendCommitItemToCommitData(accumulator, createCommitItem(commit));
    };

    const publishPendingData = async () => {
      if (accumulator.pendingItems.length === 0 || !isCurrentRequest()) return;

      pendingPublishFileCount = 0;
      lastPublishTime = performance.now();
      const pendingItems = takePendingCommitItems(accumulator);
      for (let offset = 0; offset < pendingItems.length; offset += preparedItemBatchSize) {
        const preparedItems = pendingItems.slice(offset, offset + preparedItemBatchSize);
        await preparePendingItems(preparedItems);
        if (!isCurrentRequest()) return;
        prepareItemsForViewer(preparedItems, nextLoadedItemIds);
        const isInitialPublish = !hasPublishedInitialItems;
        if (isInitialPublish) {
          hasPublishedInitialItems = true;
          loadedItemIdsRef.current = nextLoadedItemIds;
          setViewerKey(requestId);
          setInitialItems(preparedItems);
          markCommitPerformance("patch-initial-publish", {
            itemCount: preparedItems.length,
            requestId,
          });
        } else {
          const viewer = viewerRef.current;
          if (viewer != null) viewer.addItems(preparedItems);
          else setInitialItems((previous) => [...previous, ...preparedItems]);
          markCommitPerformance("patch-batch-publish", {
            itemCount: preparedItems.length,
            requestId,
          });
        }
        await yieldToBrowser();
        if (isCurrentRequest()) publishItems();
        if (isInitialPublish) {
          await waitForViewer(viewerRef, controller.signal);
        }
        lastWorkYieldTime = performance.now();
      }
    };

    const publishPendingDataIfNeeded = async () => {
      if (pendingPublishFileCount === 0) return;
      const elapsed = performance.now() - lastPublishTime;
      const publishFileBatchSize = hasPublishedInitialItems
        ? CODE_VIEW_BATCH_COUNT
        : COMMIT_INITIAL_BATCH_COUNT;
      const publishInterval = hasPublishedInitialItems
        ? STREAM_PUBLISH_INTERVAL_MS
        : STREAM_INITIAL_PUBLISH_INTERVAL_MS;
      if (
        pendingPublishFileCount < publishFileBatchSize &&
        elapsed < publishInterval
      ) {
        return;
      }
      await publishPendingData();
    };

    const appendFullPatch = async (
      page: PublishedCommitPage,
      patchContent: string,
      cacheKeyPrefix: string,
    ) => {
      if (!isCurrentRequest()) return;
      setLoadState("parsing");
      await new Promise<void>((resolve) => window.setTimeout(resolve, 0));
      if (!isCurrentRequest()) return;

      const commitByHash = new Map(
        page.commits.map((commit) => [commit.hash, commit]),
      );
      const parsedPatches = parsePatchFiles(patchContent, cacheKeyPrefix);
      const observedHashes = getPatchMetadataHashes(patchContent);
      requirePatchPageHashes(page, observedHashes);
      const patchByHash = new Map(parsedPatches.flatMap((patch) => {
        const hash = patch.patchMetadata?.match(COMMIT_HASH_METADATA_PATTERN)?.[1];
        return hash == null ? [] : [[hash, patch] as const];
      }));
      for (const [patchIndex, commit] of page.commits.entries()) {
        queueCommitSection(commit);
        const patch = patchByHash.get(commit.hash);
        if (patch == null) continue;
        const treePathPrefix = getPatchTreePathPrefix(
          patch.patchMetadata,
          patchIndex,
        );
        for (const fileDiff of patch.files) {
          appendFileDiffToCommitData(
            accumulator,
            fileDiff,
            treePathPrefix,
          );
          pendingPublishFileCount++;
        }
      }
      await publishPendingData();
    };

    const streamPage = async (page: PublishedCommitPage) => {
      setLoadState("fetching");
      markCommitPerformance("patch-page-request-start", {
        page: page.index,
        requestId,
      });
      const cacheKeyPrefix = encodeURIComponent(
        typeof page.patchUrl === "string"
          ? page.patchUrl
          : page.commits[0] == null
          ? `repository-commits-page-${page.index}`
          : page.patchUrl(page.commits[0]),
      );
      const commitByHash = new Map(
        page.commits.map((commit) => [commit.hash, commit]),
      );
      let responseBody: ReadableStream<Uint8Array>;
      if (typeof page.patchUrl === "string") {
        const response = await fetchPublishedRepositoryPatch(
          page.patchUrl,
          controller.signal,
        );
        if (!response.ok) {
          throw new Error(`Patch page request failed (${response.status}).`);
        }
        const generation = response.headers.get("x-repository-generation");
        if (generation !== history.repository.head) {
          throw new Error("Repository patch generation changed while loading");
        }
        markCommitPerformance("patch-page-response-headers", {
          page: page.index,
          requestId,
          status: response.status,
        });
        if (response.body == null) {
          await appendFullPatch(page, await response.text(), cacheKeyPrefix);
          publishPage(page);
          setLoadState("ready");
          return;
        }
        responseBody = boundPatchShard(response.body);
      } else {
        responseBody = boundPatchShard(streamCommitPatches(
          page.commits,
          page.patchUrl,
          controller.signal,
        ));
      }

      setLoadState("streaming");
      await yieldToBrowser();
      if (!isCurrentRequest()) return;

      let pagePatchIndex = 0;
      let streamTreePathPrefix: string | undefined;
      let activeCommit: HarnessCommit | undefined;
      let hasReceivedFirstStreamedFile = false;
      const observedHashes: string[] = [];

      const observeCommitSections = (hashes: readonly string[]) => {
        for (const hash of hashes) {
          const commit = commitByHash.get(hash);
          if (commit == null) {
            throw new Error(`Patch page contains unknown commit ${hash}`);
          }
          observedHashes.push(hash);
          queueCommitSection(commit);
        }
      };

      const appendStreamedFile = async (fileText: string) => {
        if (!hasReceivedFirstStreamedFile) {
          hasReceivedFirstStreamedFile = true;
          markCommitPerformance("patch-first-file", {
            page: page.index,
            requestId,
          });
        }
        const sections = inspectStreamedCommitFileSections(
          fileText,
          activeCommit?.hash,
        );
        observeCommitSections(sections.leadingHashes);
        if (sections.leadingHashes.length > 0) {
          const patchIndex = pagePatchIndex + sections.leadingHashes.length - 1;
          pagePatchIndex += sections.leadingHashes.length;
          const hash = sections.fileCommitHash;
          streamTreePathPrefix = getPatchTreePathPrefix(
            hash == null ? undefined : `From ${hash} `,
            patchIndex,
          );
          activeCommit = hash == null ? undefined : commitByHash.get(hash);
        } else if (activeCommit == null && pagePatchIndex === 0) {
          activeCommit = page.commits[0];
        }

        if (activeCommit != null) {
          const fileDiff = processFile(fileText, {
            cacheKey: `${cacheKeyPrefix}-${accumulator.fileIndex}`,
            isGitDiff: true,
          });
          if (fileDiff != null) {
            const itemIdRename = appendFileDiffToCommitData(
              accumulator,
              fileDiff,
              streamTreePathPrefix,
            );
            if (itemIdRename != null) {
              applyCommitItemIdRename(viewerRef.current, itemIdRename);
              if (nextLoadedItemIds.delete(itemIdRename.oldId)) {
                nextLoadedItemIds.add(itemIdRename.newId);
              }
            }
            pendingPublishFileCount++;
            const elapsedWork = performance.now() - lastWorkYieldTime;
            if (elapsedWork >= STREAM_WORK_BUDGET_MS) {
              await publishPendingData();
            } else {
              await publishPendingDataIfNeeded();
            }
          }
        }

        observeCommitSections(sections.trailingHashes);
        if (sections.trailingHashes.length > 0) {
          pagePatchIndex += sections.trailingHashes.length;
          activeCommit = commitByHash.get(sections.nextCommitHash!);
        }
      };

      const fallbackPatchContent = await streamGitPatchFiles(
        responseBody,
        appendStreamedFile,
      );
      if (!isCurrentRequest()) return;
      await publishPendingData();
      if (fallbackPatchContent != null) {
        await appendFullPatch(page, fallbackPatchContent, cacheKeyPrefix);
      } else {
        requirePatchPageHashes(page, observedHashes);
      }
      if (!isCurrentRequest()) return;
      publishPage(page);
      setLoadState("ready");
      markCommitPerformance("patch-page-ready", {
        itemCount: accumulator.items.length,
        page: page.index,
        requestId,
      });
    };

    const fail = (error: unknown) => {
      if (!isCurrentRequest()) return;
      failed = true;
      console.warn("Failed to load commit diff", error);
      setLoadState("error");
      markCommitPerformance("patch-stream-error", { requestId });
    };

    const drainNextPageRequest = async () => {
      if (
        pageLoading ||
        failed ||
        !nextPageRequested ||
        nextPageIndex >= history.pageCount ||
        !isCurrentRequest()
      ) {
        return;
      }
      nextPageRequested = false;
      pageLoading = true;
      try {
        const page = await history.loadPage(nextPageIndex);
        if (!isCurrentRequest()) return;
        await streamPage(page);
        nextPageIndex++;
      } catch (error) {
        fail(error);
      } finally {
        pageLoading = false;
      }
      if (nextPageRequested) void drainNextPageRequest();
    };

    requestNextPageRef.current = () => {
      if (nextPageIndex >= history.pageCount || failed) return;
      nextPageRequested = true;
      void drainNextPageRequest();
    };

    const start = async () => {
      pageLoading = true;
      try {
        await streamPage(initialPage);
      } catch (error) {
        fail(error);
      } finally {
        pageLoading = false;
      }
      if (nextPageRequested) void drainNextPageRequest();
    };
    void start();

    return () => {
      requestNextPageRef.current = () => undefined;
      controller.abort();
    };
  }, [
    history,
    initialPage,
    loadAttempt,
    preparedItemBatchSize,
    publishItems,
    publishPage,
    viewerRef,
  ]);

  const requestNextPage = useStableCallback(() => {
    requestNextPageRef.current();
  });
  const retryLoad = useCallback(() => {
    setLoadAttempt((attempt) => attempt + 1);
  }, []);

  return {
    applyCollapseModeToLoaded,
    initialItems,
    loadState,
    requestNextPage,
    retryLoad,
    viewerKey,
  };
}

function streamCommitPatches(
  commits: readonly HarnessCommit[],
  patchUrl: (commit: HarnessCommit) => string,
  signal: AbortSignal,
): ReadableStream<Uint8Array> {
  let nextCommit = 0;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const commit = commits[nextCommit];
      if (commit == null) {
        controller.close();
        return;
      }
      const response = await fetch(patchUrl(commit), {
        cache: "default",
        signal,
      });
      if (!response.ok) {
        throw new Error(`Patch request failed (${response.status}).`);
      }
      const patch = new Uint8Array(await response.arrayBuffer());
      if (nextCommit === 0) {
        controller.enqueue(patch);
      } else {
        const separated = new Uint8Array(patch.byteLength + 1);
        separated[0] = 10;
        separated.set(patch, 1);
        controller.enqueue(separated);
      }
      nextCommit++;
    },
  });
}

function boundPatchShard(
  body: ReadableStream<Uint8Array>,
): ReadableStream<Uint8Array> {
  const reader = body.getReader();
  let totalBytes = 0;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const result = await reader.read();
      if (result.done) {
        controller.close();
        reader.releaseLock();
        return;
      }
      totalBytes += result.value.byteLength;
      if (totalBytes > MAX_PATCH_SHARD_BYTES) {
        await reader.cancel("Commit patch shard exceeded 16 MiB");
        controller.error(new Error("Commit patch shard exceeded 16 MiB"));
        return;
      }
      controller.enqueue(result.value);
    },
    async cancel(reason) {
      await reader.cancel(reason);
    },
  });
}

function applyCommitItemIdRename(
  viewer: CodeViewHandle<undefined> | null,
  rename: CommitItemIdRename,
): void {
  viewer?.updateItemId(rename.oldId, rename.newId);
}

function requirePatchPageHashes(
  page: PublishedCommitPage,
  observedHashes: readonly string[],
): void {
  if (
    observedHashes.length !== page.commits.length ||
    observedHashes.some((hash, index) => hash !== page.commits[index]?.hash)
  ) {
    throw new Error(`Patch page ${page.index} does not match its commit metadata`);
  }
}

function getNextItemVersion(item: { version?: string | number }): number {
  return typeof item.version === "number" ? item.version + 1 : 1;
}

function markCommitPerformance(
  name: string,
  detail?: Record<string, number>,
): void {
  if (typeof performance === "undefined") return;
  performance.mark(`nanocodex:commits:${name}`, { detail });
}

function yieldToBrowser(): Promise<void> {
  return new Promise((resolve) => {
    let didResolve = false;
    const resolveOnce = () => {
      if (didResolve) return;
      didResolve = true;
      window.clearTimeout(timeout);
      resolve();
    };
    const timeout = window.setTimeout(resolveOnce, 50);
    window.requestAnimationFrame(resolveOnce);
  });
}

async function waitForViewer(
  viewerRef: RefObject<CodeViewHandle<undefined> | null>,
  signal: AbortSignal,
): Promise<void> {
  while (!signal.aborted && viewerRef.current == null) {
    await yieldToBrowser();
  }
}
