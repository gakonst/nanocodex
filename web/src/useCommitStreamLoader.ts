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
  getPatchTreePathPrefix,
} from "./commitPatchMetadata";
import {
  CODE_VIEW_BATCH_COUNT,
  getInitialBatchSize,
} from "./pierreCodeView";
import {
  getStreamedPatchMetadata,
  streamGitPatchFiles,
} from "./streamGitPatchFiles";
import type { HarnessCommit } from "./Xedoc";

const STREAM_PUBLISH_INTERVAL_MS = 100;
const STREAM_INITIAL_PUBLISH_INTERVAL_MS = 500;
const STREAM_WORK_BUDGET_MS = 8;

export type CommitStreamLoadState =
  | "fetching"
  | "parsing"
  | "streaming"
  | "ready"
  | "error";

interface UseCommitStreamLoaderOptions {
  collapseMode: "expanded" | "collapsed";
  commits: HarnessCommit[];
  patchUrl: string;
  viewerRef: RefObject<CodeViewHandle<undefined> | null>;
}

function commitItemId(commit: HarnessCommit): string {
  return `commit:${commit.hash}`;
}

function createCommitItem(commit: HarnessCommit): CommitStreamItem {
  return {
    id: commitItemId(commit),
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
  commits,
  patchUrl,
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
  const collapseModeRef = useRef(collapseMode);
  collapseModeRef.current = collapseMode;

  const prepareItemsForViewer = (
    items: readonly CodeViewItem<undefined>[],
  ): void => {
    const targetCollapsed = collapseModeRef.current === "collapsed";
    for (const item of items) {
      loadedItemIdsRef.current.add(item.id);
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

    loadedItemIdsRef.current = new Set();
    setViewerKey(requestId);
    setInitialItems([]);
    setLoadState("fetching");

    async function loadPatch() {
      try {
        const cacheKeyPrefix = encodeURIComponent(patchUrl);
        const commitByHash = new Map(
          commits.map((commit) => [commit.hash, commit]),
        );
        const resolveCommit = (
          patchMetadata: string | undefined,
          patchIndex: number,
        ) => {
          const hash = patchMetadata?.match(
            COMMIT_HASH_METADATA_PATTERN,
          )?.[1];
          return (hash == null ? undefined : commitByHash.get(hash)) ??
            commits[patchIndex];
        };

        async function commitFullPatch(patchContent: string) {
          if (!isCurrentRequest()) return;
          setLoadState("parsing");
          await new Promise<void>((resolve) => window.setTimeout(resolve, 0));
          if (!isCurrentRequest()) return;

          const parsedPatches = parsePatchFiles(patchContent, cacheKeyPrefix);
          const accumulator = createCommitDataAccumulator();
          const shouldPrefixTreePaths = parsedPatches.length > 1;
          for (const [patchIndex, patch] of parsedPatches.entries()) {
            const commit = resolveCommit(patch.patchMetadata, patchIndex);
            if (commit == null) continue;
            appendCommitItemToCommitData(
              accumulator,
              createCommitItem(commit),
            );
            const treePathPrefix = shouldPrefixTreePaths
              ? getPatchTreePathPrefix(patch.patchMetadata, patchIndex)
              : undefined;
            for (const fileDiff of patch.files) {
              appendFileDiffToCommitData(
                accumulator,
                fileDiff,
                treePathPrefix,
              );
            }
          }

          if (!isCurrentRequest()) return;
          prepareItemsForViewer(accumulator.items);
          setInitialItems(accumulator.items);
          setLoadState("ready");
          await yieldToBrowser();
        }

        const response = await fetch(patchUrl, {
          cache: "no-store",
          signal: controller.signal,
        });
        if (!response.ok) {
          throw new Error(`Patch request failed (${response.status}).`);
        }

        if (response.body == null) {
          await commitFullPatch(await response.text());
          return;
        }

        setLoadState("streaming");
        await yieldToBrowser();
        if (!isCurrentRequest()) return;

        const accumulator = createCommitDataAccumulator();
        const queuedCommitHashes = new Set<string>();
        let streamPatchIndex = 0;
        let streamTreePathPrefix: string | undefined;
        let activeCommit: HarnessCommit | undefined;
        let pendingPublishFileCount = 0;
        let hasPublishedInitialItems = false;
        let lastPublishTime = performance.now();
        let lastWorkYieldTime = lastPublishTime;
        const initialPublishFileBatchSize = getInitialBatchSize();

        const queueCommitSection = (commit: HarnessCommit) => {
          if (queuedCommitHashes.has(commit.hash)) return;
          queuedCommitHashes.add(commit.hash);
          appendCommitItemToCommitData(accumulator, createCommitItem(commit));
        };

        const publishPendingData = async () => {
          if (pendingPublishFileCount === 0 || !isCurrentRequest()) return;

          pendingPublishFileCount = 0;
          lastPublishTime = performance.now();
          const pendingItems = takePendingCommitItems(accumulator);
          prepareItemsForViewer(pendingItems);
          if (!hasPublishedInitialItems) {
            hasPublishedInitialItems = true;
            setInitialItems(pendingItems);
          } else {
            const viewer = viewerRef.current;
            if (viewer != null) viewer.addItems(pendingItems);
            else setInitialItems((previous) => [...previous, ...pendingItems]);
          }
          await yieldToBrowser();
          lastWorkYieldTime = performance.now();
        };

        const publishPendingDataIfNeeded = async () => {
          if (pendingPublishFileCount === 0) return;
          const elapsed = performance.now() - lastPublishTime;
          const publishFileBatchSize = hasPublishedInitialItems
            ? CODE_VIEW_BATCH_COUNT
            : initialPublishFileBatchSize;
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

        const shouldDeferInitialPublishForBatchTarget = () => {
          if (hasPublishedInitialItems) return false;
          const elapsed = performance.now() - lastPublishTime;
          return (
            pendingPublishFileCount < initialPublishFileBatchSize &&
            elapsed < STREAM_INITIAL_PUBLISH_INTERVAL_MS
          );
        };

        const appendStreamedFile = async (fileText: string) => {
          const patchMetadata = getStreamedPatchMetadata(fileText);
          if (patchMetadata != null) {
            const patchIndex = streamPatchIndex++;
            streamTreePathPrefix = getPatchTreePathPrefix(
              patchMetadata,
              patchIndex,
            );
            activeCommit = resolveCommit(patchMetadata, patchIndex);
            if (activeCommit != null) queueCommitSection(activeCommit);
          } else if (activeCommit == null && streamPatchIndex === 0) {
            activeCommit = commits[0];
            if (activeCommit != null) queueCommitSection(activeCommit);
          }

          if (activeCommit == null) return;
          const fileDiff = processFile(fileText, {
            cacheKey: `${cacheKeyPrefix}-0-${accumulator.fileIndex}`,
            isGitDiff: true,
          });
          if (fileDiff == null) return;

          const itemIdRename = appendFileDiffToCommitData(
            accumulator,
            fileDiff,
            streamTreePathPrefix,
          );
          if (itemIdRename != null) {
            applyCommitItemIdRename(viewerRef.current, itemIdRename);
            if (loadedItemIdsRef.current.delete(itemIdRename.oldId)) {
              loadedItemIdsRef.current.add(itemIdRename.newId);
            }
          }
          pendingPublishFileCount++;
          const elapsedWork = performance.now() - lastWorkYieldTime;
          if (elapsedWork >= STREAM_WORK_BUDGET_MS) {
            if (shouldDeferInitialPublishForBatchTarget()) {
              await yieldToBrowser();
              lastWorkYieldTime = performance.now();
            } else {
              await publishPendingData();
            }
          } else {
            await publishPendingDataIfNeeded();
          }
        };

        const fallbackPatchContent = await streamGitPatchFiles(
          response.body,
          appendStreamedFile,
        );
        if (!isCurrentRequest()) return;

        await publishPendingData();
        if (fallbackPatchContent != null) {
          await commitFullPatch(fallbackPatchContent);
          return;
        }
        setLoadState("ready");
      } catch (error) {
        if (!isCurrentRequest()) return;
        console.warn("Failed to load commit diff", error);
        setLoadState("error");
      }
    }

    void loadPatch();
    return () => controller.abort();
  }, [commits, loadAttempt, patchUrl, viewerRef]);

  const retryLoad = useCallback(() => {
    setLoadAttempt((attempt) => attempt + 1);
  }, []);

  return {
    applyCollapseModeToLoaded,
    initialItems,
    loadState,
    retryLoad,
    viewerKey,
  };
}

function applyCommitItemIdRename(
  viewer: CodeViewHandle<undefined> | null,
  rename: CommitItemIdRename,
): void {
  viewer?.updateItemId(rename.oldId, rename.newId);
}

function getNextItemVersion(item: { version?: string | number }): number {
  return typeof item.version === "number" ? item.version + 1 : 1;
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
