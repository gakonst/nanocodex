import type { DiffIndicators } from "@pierre/diffs";
import { type CodeViewHandle, useStableCallback } from "@pierre/diffs/react";
import {
  ChevronsDownUp,
  ChevronsUpDown,
  Columns2,
  Rows3,
  Settings2,
} from "lucide-react";
import {
  memo,
  useCallback,
  useEffect,
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
import { Switch } from "./Switch";
import { useCommitStreamLoader } from "./useCommitStreamLoader";
import type { HarnessCommit, Theme } from "./Xedoc";

type CommitCodeStreamProps = {
  commits: HarnessCommit[];
  patchUrl: string;
  theme: Theme;
};

const CommitCodeStreamComponent = function CommitCodeStream({
  commits,
  patchUrl,
  theme,
}: CommitCodeStreamProps) {
  const renderer = usePierreRenderer();
  const scrollRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<CodeViewHandle<undefined> | null>(null);
  const [diffStyle, setDiffStyle] = useState<"split" | "unified">("split");
  const [collapseMode, setCollapseMode] = useState<
    "expanded" | "collapsed"
  >("expanded");
  const [overflow, setOverflow] = useState<"wrap" | "scroll">("scroll");
  const [showBackgrounds, setShowBackgrounds] = useState(true);
  const [diffIndicators, setDiffIndicators] =
    useState<DiffIndicators>("bars");
  const [lineNumbers, setLineNumbers] = useState(true);

  useEffect(() => {
    const mediaQuery = window.matchMedia("(max-width: 767px)");
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
    retryLoad,
    viewerKey,
  } = useCommitStreamLoader({
    collapseMode,
    commits,
    patchUrl,
    viewerRef,
  });

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

  const viewerAvailable =
    renderer.ready &&
    (loadState === "ready" ||
      (loadState === "streaming" && initialItems.length > 0));

  return (
    <>
      <CommitStreamToolbar
        collapseMode={collapseMode}
        commitCount={commits.length}
        diffIndicators={diffIndicators}
        diffStyle={diffStyle}
        lineNumbers={lineNumbers}
        overflow={overflow}
        showBackgrounds={showBackgrounds}
        onDiffIndicatorsChange={setDiffIndicators}
        onLineNumbersChange={setLineNumbers}
        onShowBackgroundsChange={setShowBackgrounds}
        onToggleCollapseMode={handleToggleCollapseMode}
        onToggleDiffStyle={handleToggleDiffStyle}
        onWordWrapChange={handleWordWrapChange}
      />

      {viewerAvailable ? (
        <DiffsHubViewer
          key={viewerKey}
          diffIndicators={diffIndicators}
          diffStyle={diffStyle}
          disableWorkerPool={renderer.disableWorkerPool}
          initialItems={initialItems}
          lineNumbers={lineNumbers}
          overflow={overflow}
          scrollRef={scrollRef}
          showBackgrounds={showBackgrounds}
          theme={theme}
          viewerRef={viewerRef}
        />
      ) : loadState === "error" ? (
        <div className="commit-stream-error" role="alert">
          <p>Couldn’t load commits.</p>
          <button type="button" onClick={retryLoad}>
            Try again
          </button>
        </div>
      ) : null}
    </>
  );
};

interface CommitStreamToolbarProps {
  collapseMode: "expanded" | "collapsed";
  commitCount: number;
  diffIndicators: DiffIndicators;
  diffStyle: "split" | "unified";
  lineNumbers: boolean;
  overflow: "wrap" | "scroll";
  showBackgrounds: boolean;
  onDiffIndicatorsChange(value: DiffIndicators): void;
  onLineNumbersChange(checked: boolean): void;
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
  onDiffIndicatorsChange,
  onLineNumbersChange,
  onShowBackgroundsChange,
  onToggleCollapseMode,
  onToggleDiffStyle,
  onWordWrapChange,
}: CommitStreamToolbarProps) {
  return (
    <header className="commit-stream-toolbar">
      <div className="commit-toolbar-title">
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
