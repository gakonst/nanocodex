import {
  DEFAULT_THEMES,
  parsePatchFiles,
  type CodeViewItem,
  type CodeViewOptions,
} from "@pierre/diffs";
import {
  CodeView,
  type CodeViewHandle,
  useStableCallback,
} from "@pierre/diffs/react";
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { usePierreRenderer } from "./PierreWorkerProvider";
import { CODE_VIEW_CUSTOM_CSS, CODE_VIEW_LAYOUT } from "./pierreCodeView";
import type { HarnessCommit, Theme } from "./Xedoc";

const dateFormatter = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

type CommitCodeStreamProps = {
  commits: HarnessCommit[];
  theme: Theme;
};

type CommitAnnotation = undefined;
type CommitStreamItem = CodeViewItem<CommitAnnotation>;

export type CommitCodeStreamHandle = {
  focus(): void;
  scrollToCommit(index: number): void;
};

function commitItemId(commit: HarnessCommit) {
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

export const CommitCodeStream = forwardRef<
  CommitCodeStreamHandle,
  CommitCodeStreamProps
>(function CommitCodeStream({ commits, theme }, forwardedRef) {
  const renderer = usePierreRenderer();
  const viewerRef = useRef<CodeViewHandle<CommitAnnotation> | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const pendingJumpRef = useRef<number | null>(null);
  const [items, setItems] = useState<CommitStreamItem[] | null>(null);
  const commitByItemId = useMemo(
    () => new Map(commits.map((commit) => [commitItemId(commit), commit])),
    [commits],
  );

  const scrollToCommit = useCallback((index: number) => {
    const commit = commits[index];
    if (commit == null) return;
    const id = commitItemId(commit);
    const viewer = viewerRef.current;
    if (viewer == null || viewer.getItem(id) == null) {
      pendingJumpRef.current = index;
      return;
    }
    pendingJumpRef.current = null;
    viewer.scrollTo({
      type: "item",
      id,
      align: "start",
      behavior: "smooth",
    });
  }, [commits]);

  const handleViewerRef = useStableCallback(
    (viewer: CodeViewHandle<CommitAnnotation> | null) => {
      viewerRef.current = viewer;
      const pendingJump = pendingJumpRef.current;
      if (viewer != null && pendingJump != null) {
        scrollToCommit(pendingJump);
      }
    },
  );

  useImperativeHandle(
    forwardedRef,
    () => ({
      focus() {
        containerRef.current?.focus({ preventScroll: true });
      },
      scrollToCommit,
    }),
    [scrollToCommit],
  );

  const options = useMemo<CodeViewOptions<CommitAnnotation>>(
    () => ({
      layout: CODE_VIEW_LAYOUT,
      theme: DEFAULT_THEMES,
      themeType: theme,
      diffStyle: "unified",
      diffIndicators: "bars",
      overflow: "scroll",
      lineHoverHighlight: "number",
      enableLineSelection: true,
      stickyHeaders: true,
      unsafeCSS: CODE_VIEW_CUSTOM_CSS,
    }),
    [theme],
  );

  const renderCommitMetadata = useStableCallback((item: CommitStreamItem) => {
    if (item.type !== "file") return null;
    const commit = commitByItemId.get(item.id);
    if (commit == null) return null;
    return (
      <div className="commit-code-metadata">
        <span className="commit-section-hash">Commit {commit.shortHash}</span>
        <span>{commit.author}</span>
        <span>{dateFormatter.format(new Date(commit.authoredAt))}</span>
        <span>
          {commit.stats.files} file{commit.stats.files === 1 ? "" : "s"}
        </span>
        <span className="additions">+{commit.stats.additions}</span>
        <span className="deletions">−{commit.stats.deletions}</span>
      </div>
    );
  });

  useEffect(() => {
    const controller = new AbortController();
    let current = true;
    pendingJumpRef.current = null;
    setItems(null);

    async function loadCommits() {
      const patches = await Promise.all(
        commits.map(async (commit) => {
          try {
            const response = await fetch(commit.patchUrl, {
              cache: "force-cache",
              signal: controller.signal,
            });
            if (!response.ok) {
              throw new Error(`Patch request failed: ${response.status}`);
            }
            return await response.text();
          } catch (error) {
            if (controller.signal.aborted) throw error;
            return null;
          }
        }),
      );
      if (!current) return;

      const nextItems: CommitStreamItem[] = [];
      for (let commitIndex = 0; commitIndex < commits.length; commitIndex++) {
        const commit = commits[commitIndex];
        nextItems.push(createCommitItem(commit));

        const patch = patches[commitIndex];
        if (patch == null) continue;
        const fileDiffs = parsePatchFiles(patch, commit.hash).flatMap(
          (parsedPatch) => parsedPatch.files,
        );
        for (let fileIndex = 0; fileIndex < fileDiffs.length; fileIndex++) {
          nextItems.push({
            id: `${commit.hash}:${fileIndex}:${fileDiffs[fileIndex].name}`,
            type: "diff",
            fileDiff: fileDiffs[fileIndex],
          });
        }
      }

      if (current) setItems(nextItems);
    }

    void loadCommits().catch((error) => {
      if (!controller.signal.aborted) console.warn("Failed to load commit stream", error);
    });

    return () => {
      current = false;
      controller.abort();
    };
  }, [commits]);

  if (!renderer.ready || items == null) {
    return null;
  }

  return (
    <CodeView
      ref={handleViewerRef}
      containerRef={containerRef}
      initialItems={items}
      className="commit-stream code-view cv-scrollbar"
      disableWorkerPool={renderer.disableWorkerPool}
      options={options}
      renderHeaderMetadata={renderCommitMetadata}
    />
  );
});
