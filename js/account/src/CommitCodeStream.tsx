import type { DiffIndicators } from "@pierre/diffs";
import type { CodeViewHandle } from "@pierre/diffs/react";
import {
  ChevronsDownUp,
  ChevronsUpDown,
  Columns2,
  PanelLeft,
  Rows3,
  Settings2,
} from "lucide-react";
import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { DiffsHubViewer } from "./DiffsHubViewer";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./DropdownMenu";
import { usePierreRenderer } from "./PierreWorkerProvider";
import { COMPACT_WORKSPACE_QUERY } from "./pierreCodeView";
import { Switch } from "./Switch";
import {
  getCommitStreamErrorMode,
  shouldRequestNextCommitPage,
  tryApplyPendingCommitJump,
  useCommitStreamLoader,
} from "./useCommitStreamLoader";
import type { Theme } from "./NanocodexApp";
import {
  preloadPublishedRepositoryPatch,
  type PublishedCommitHistory,
  type PublishedCommitPage,
} from "./publishedRepository";
import "./Commits.css";

type CommitCodeStreamProps = {
  commitRailOpen?: boolean;
  history: PublishedCommitHistory;
  onPageLoaded(page: PublishedCommitPage): void;
  onOpenCommitRail?: () => void;
  theme: Theme;
};

export type CommitCodeStreamHandle = {
  scrollToCommit(hash: string): void;
};

const CommitCodeStreamComponent = forwardRef<
  CommitCodeStreamHandle,
  CommitCodeStreamProps
>(function CommitCodeStream(
  { commitRailOpen, history, onOpenCommitRail, onPageLoaded, theme },
  forwardedRef,
) {
  const renderer = usePierreRenderer();
  const scrollRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<CodeViewHandle<undefined> | null>(null);
  const pendingJumpRef = useRef<string | null>(history.initialCommitHash);
  const windowRequestIdRef = useRef(0);
  const [windowPage, setWindowPage] = useState(history.initialPage);
  const [windowError, setWindowError] = useState(false);
  const [diffStyle, setDiffStyle] = useState<"split" | "unified">("split");
  const [collapseMode, setCollapseMode] = useState<
    "expanded" | "collapsed"
  >("expanded");
  const [overflow, setOverflow] = useState<"wrap" | "scroll">("scroll");
  const [showBackgrounds, setShowBackgrounds] = useState(true);
  const [diffIndicators, setDiffIndicators] =
    useState<DiffIndicators>("bars");
  const [lineNumbers, setLineNumbers] = useState(true);

  const tryApplyPendingJump = useCallback(() => {
    return tryApplyPendingCommitJump(
      pendingJumpRef,
      viewerRef.current,
    );
  }, []);

  const scrollToCommit = useCallback(
    (hash: string) => {
      if (history.pageForCommit(hash) == null) return;
      pendingJumpRef.current = hash;
      if (tryApplyPendingJump()) return;

      const pageIndex = history.pageForCommit(hash);
      if (pageIndex == null) return;
      const requestId = ++windowRequestIdRef.current;
      setWindowError(false);
      void history.loadPage(pageIndex).then((page) => {
        if (windowRequestIdRef.current !== requestId) return;
        void preloadPublishedRepositoryPatch(page.patchUrl)?.catch(() => undefined);
        onPageLoaded(page);
        setWindowPage(page);
      }).catch(() => {
        if (windowRequestIdRef.current === requestId) setWindowError(true);
      });
    },
    [history, onPageLoaded, tryApplyPendingJump],
  );

  useEffect(() => {
    windowRequestIdRef.current++;
    pendingJumpRef.current = history.initialCommitHash;
    setWindowPage(history.initialPage);
    setWindowError(false);
  }, [history]);

  useEffect(() => {
    const mediaQuery = window.matchMedia(COMPACT_WORKSPACE_QUERY);
    const updateDiffStyle = (matches: boolean) => {
      setDiffStyle(matches ? "unified" : "split");
    };
    const handleChange = (event: MediaQueryListEvent) => {
      updateDiffStyle(event.matches);
    };

    updateDiffStyle(mediaQuery.matches);
    mediaQuery.addEventListener("change", handleChange);
    return () => mediaQuery.removeEventListener("change", handleChange);
  }, []);

  const {
    applyCollapseModeToLoaded,
    initialItems,
    loadState,
    requestNextPage,
    retryLoad,
    viewerKey,
  } = useCommitStreamLoader({
    collapseMode,
    history,
    initialPage: windowPage,
    onItemsPublished: tryApplyPendingJump,
    onPageLoaded,
    preparationBatchSize: renderer.preparationBatchSize,
    prepareItems: renderer.prepareItems,
    viewerRef,
  });

  const handleViewerScroll = useCallback((
    scrollTop: number,
    viewer: { getHeight(): number; getScrollHeight(): number },
  ) => {
    if (
      shouldRequestNextCommitPage(
        scrollTop,
        viewer.getHeight(),
        viewer.getScrollHeight(),
      )
    ) {
      requestNextPage();
    }
  }, [requestNextPage]);

  const handleToggleCollapseMode = useCallback(() => {
    const next = collapseMode === "expanded" ? "collapsed" : "expanded";
    setCollapseMode(next);
    applyCollapseModeToLoaded(next);
  }, [applyCollapseModeToLoaded, collapseMode]);

  const handleToggleDiffStyle = useCallback(() => {
    setDiffStyle((current) => (current === "split" ? "unified" : "split"));
  }, []);

  const handleWordWrapChange = useCallback((checked: boolean) => {
    setOverflow(checked ? "wrap" : "scroll");
  }, []);

  useImperativeHandle(
    forwardedRef,
    () => ({ scrollToCommit }),
    [scrollToCommit],
  );

  useEffect(() => {
    if (!renderer.ready || initialItems.length === 0) return;
    const frame = window.requestAnimationFrame(tryApplyPendingJump);
    return () => window.cancelAnimationFrame(frame);
  }, [initialItems.length, renderer.ready, tryApplyPendingJump, viewerKey]);

  const viewerAvailable =
    renderer.ready &&
    (initialItems.length > 0 || loadState === "ready");
  const errorMode = getCommitStreamErrorMode(
    loadState,
    initialItems.length > 0,
  );

  return (
    <>
      <CommitStreamToolbar
        collapseMode={collapseMode}
        commitCount={history.hashes.length}
        diffIndicators={diffIndicators}
        diffStyle={diffStyle}
        lineNumbers={lineNumbers}
        overflow={overflow}
        showBackgrounds={showBackgrounds}
        commitRailOpen={commitRailOpen}
        onDiffIndicatorsChange={setDiffIndicators}
        onLineNumbersChange={setLineNumbers}
        onOpenCommitRail={onOpenCommitRail}
        onShowBackgroundsChange={setShowBackgrounds}
        onToggleCollapseMode={handleToggleCollapseMode}
        onToggleDiffStyle={handleToggleDiffStyle}
        onWordWrapChange={handleWordWrapChange}
      />

      {viewerAvailable ? (
        <>
          <DiffsHubViewer
            key={viewerKey}
            diffIndicators={diffIndicators}
            diffStyle={diffStyle}
            disableWorkerPool={renderer.disableWorkerPool}
            initialItems={initialItems}
            lineNumbers={lineNumbers}
            onScroll={handleViewerScroll}
            overflow={overflow}
            scrollRef={scrollRef}
            showBackgrounds={showBackgrounds}
            theme={theme}
            viewerRef={viewerRef}
          />
          {errorMode === "tail" || windowError ? (
            <div className="commit-stream-tail-error" role="alert">
              <span>
                {windowError
                  ? "Couldn’t load that commit window."
                  : "Commit stream stopped before all changes loaded."}
              </span>
              <button
                type="button"
                onClick={() => {
                  if (windowError && pendingJumpRef.current != null) {
                    scrollToCommit(pendingJumpRef.current);
                  } else {
                    retryLoad();
                  }
                }}
              >
                Try again
              </button>
            </div>
          ) : null}
        </>
      ) : errorMode === "cold" || windowError ? (
        <div className="commit-stream-error" role="alert">
          <p>Couldn’t load commits.</p>
          <button
            type="button"
            onClick={() => {
              if (windowError && pendingJumpRef.current != null) {
                scrollToCommit(pendingJumpRef.current);
              } else {
                retryLoad();
              }
            }}
          >
            Try again
          </button>
        </div>
      ) : null}
    </>
  );
});

interface CommitStreamToolbarProps {
  collapseMode: "expanded" | "collapsed";
  commitCount: number;
  diffIndicators: DiffIndicators;
  diffStyle: "split" | "unified";
  lineNumbers: boolean;
  overflow: "wrap" | "scroll";
  showBackgrounds: boolean;
  commitRailOpen?: boolean;
  onDiffIndicatorsChange(value: DiffIndicators): void;
  onLineNumbersChange(checked: boolean): void;
  onOpenCommitRail?: () => void;
  onShowBackgroundsChange(checked: boolean): void;
  onToggleCollapseMode(): void;
  onToggleDiffStyle(): void;
  onWordWrapChange(checked: boolean): void;
}

const CommitStreamToolbar = memo(function CommitStreamToolbar({
  collapseMode,
  commitCount,
  diffIndicators,
  diffStyle,
  lineNumbers,
  overflow,
  showBackgrounds,
  commitRailOpen,
  onDiffIndicatorsChange,
  onLineNumbersChange,
  onOpenCommitRail,
  onShowBackgroundsChange,
  onToggleCollapseMode,
  onToggleDiffStyle,
  onWordWrapChange,
}: CommitStreamToolbarProps) {
  return (
    <header className="commit-stream-toolbar">
      <div className="commit-toolbar-title">
        {onOpenCommitRail ? (
          <button
            className="mobile-tree-toggle"
            type="button"
            onClick={onOpenCommitRail}
            aria-label="Open commit index"
            aria-controls="commit-index"
            aria-expanded={commitRailOpen ?? false}
          >
            <PanelLeft aria-hidden="true" />
          </button>
        ) : null}
        <strong>All commits</strong>
        <span>{commitCount}</span>
      </div>
      <div className="commit-view-controls">
        <span className="commit-order">Newest to oldest</span>
        <button
          className="commit-view-button commit-diff-style-toggle"
          type="button"
          title={
            diffStyle === "split"
              ? "Switch to unified view"
              : "Switch to split view"
          }
          aria-label={
            diffStyle === "split"
              ? "Switch to unified view"
              : "Switch to split view"
          }
          onClick={onToggleDiffStyle}
        >
          {diffStyle === "split" ? (
            <Columns2 aria-hidden="true" />
          ) : (
            <Rows3 aria-hidden="true" />
          )}
        </button>
        <button
          className="commit-view-button"
          type="button"
          aria-pressed={collapseMode === "collapsed"}
          title={
            collapseMode === "expanded"
              ? "Collapse all files"
              : "Expand all files"
          }
          aria-label={
            collapseMode === "expanded"
              ? "Collapse all files"
              : "Expand all files"
          }
          onClick={onToggleCollapseMode}
        >
          {collapseMode === "expanded" ? (
            <ChevronsDownUp aria-hidden="true" />
          ) : (
            <ChevronsUpDown aria-hidden="true" />
          )}
        </button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="commit-view-button"
              aria-label="Display settings"
              title="Display settings"
            >
              <Settings2 aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="commit-display-menu">
            <DropdownMenuItem
              className="commit-display-menu-item"
              onSelect={(event) => event.preventDefault()}
            >
              <label className="commit-setting-row">
                <span>Backgrounds</span>
                <Switch
                  checked={showBackgrounds}
                  onCheckedChange={onShowBackgroundsChange}
                />
              </label>
            </DropdownMenuItem>
            <DropdownMenuItem
              className="commit-display-menu-item"
              onSelect={(event) => event.preventDefault()}
            >
              <label className="commit-setting-row">
                <span>Line numbers</span>
                <Switch
                  checked={lineNumbers}
                  onCheckedChange={onLineNumbersChange}
                />
              </label>
            </DropdownMenuItem>
            <DropdownMenuItem
              className="commit-display-menu-item"
              onSelect={(event) => event.preventDefault()}
            >
              <label className="commit-setting-row">
                <span>Word wrap</span>
                <Switch
                  checked={overflow === "wrap"}
                  onCheckedChange={onWordWrapChange}
                />
              </label>
            </DropdownMenuItem>
            <DropdownMenuItem
              className="commit-display-menu-item commit-indicator-row"
              onSelect={(event) => event.preventDefault()}
            >
              <span>Indicator style</span>
              <div className="commit-indicator-options">
                {(["bars", "classic", "none"] as const).map((value) => (
                  <button
                    type="button"
                    className={diffIndicators === value ? "is-active" : ""}
                    aria-pressed={diffIndicators === value}
                    onClick={() => onDiffIndicatorsChange(value)}
                    key={value}
                  >
                    {value}
                  </button>
                ))}
              </div>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </header>
  );
});

export const CommitCodeStream = memo(CommitCodeStreamComponent);
