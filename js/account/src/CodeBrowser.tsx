import pierreDark from "@pierre/theme/pierre-dark-soft";
import pierreLight from "@pierre/theme/pierre-light";
import {
  type CodeViewItem,
  type CodeViewOptions,
  type SelectedLineRange,
} from "@pierre/diffs";
import { CodeView, type CodeViewHandle } from "@pierre/diffs/react";
import {
  FileTree as PierreFileTree,
  prepareFileTreeInput,
  themeToTreeStyles,
  type FileTreeOptions,
  type FileTreePreparedInput,
} from "@pierre/trees";
import { FileTree as FileTreeView } from "@pierre/trees/react";
import { ChevronRight, FileQuestion, GitBranch, PanelLeft, RefreshCw, Search } from "lucide-react";
import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ForwardedRef,
  type RefObject,
} from "react";
import { useLocation, useNavigate } from "react-router";
import { usePierreRenderer } from "./PierreWorkerProvider";
import {
  COARSE_POINTER_QUERY,
  observeMediaQueryMatch,
  retainedSourceTreeState,
  scaledSourceTreeScrollTop,
  sourceTreeItemHeight,
} from "./mobileInteraction";
import {
  CODE_VIEW_CUSTOM_CSS,
  CODE_VIEW_LAYOUT,
  CODE_VIEW_THEMES,
  COMPACT_WORKSPACE_QUERY,
  getInitialBatchSize,
  observePierreCodeScrollRegions,
} from "./pierreCodeView";
import type { PreparedPublishedFile } from "./publishedRepository";
import { sourceCodeViewItem } from "./sourceHighlight";
import type { RepositoryFile } from "./threadRepositorySnapshot";
import { useModalBoundary } from "./modalBoundary";
import "./SourceBrowser.css";

type CodeBrowserProps = {
  files: RepositoryFile[];
  branch: string;
  head: string;
  initialFile?: PreparedPublishedFile;
  readFile(file: RepositoryFile): Promise<string>;
  theme: "light" | "dark";
};

export type CodeBrowserHandle = {
  closeSearches(): void;
  openFileSearch(): void;
  openTreeSearch(): void;
};

export type SourceLineRange = {
  start: number;
  end: number;
};

type SourceFileError = {
  file: RepositoryFile;
  kind: "request" | "unsupported";
};

type SourceLocation = {
  path: string;
  range: SourceLineRange | null;
};

type CodeViewSelection = {
  id: string;
  range: SelectedLineRange;
};

function formatBytes(value: number | null) {
  if (value === null) return "—";
  if (value < 1_000) return `${value} B`;
  if (value < 1_000_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} KB`;
  return `${(value / 1_000_000).toFixed(1)} MB`;
}

function countLines(contents: string | null): number | null {
  if (contents === null) return null;
  if (!contents) return 0;
  let lines = 1;
  for (let index = 0; index < contents.length; index += 1) {
    if (contents.charCodeAt(index) === 10) lines += 1;
  }
  return lines;
}

export function parseSourceLineHash(hash: string): SourceLineRange | null {
  const match = /^#L([1-9]\d*)(?:-L?([1-9]\d*))?$/.exec(hash);
  if (!match) return null;
  const first = Number(match[1]);
  const second = match[2] ? Number(match[2]) : first;
  if (!Number.isSafeInteger(first) || !Number.isSafeInteger(second)) return null;
  return {
    start: Math.min(first, second),
    end: Math.max(first, second),
  };
}

export function classifySourceFileError(error: unknown): SourceFileError["kind"] {
  const message = error instanceof Error ? error.message : String(error);
  return /not (?:a text file|available as published text)|binary|unsupported/i.test(message)
    ? "unsupported"
    : "request";
}

function readSourceLocation(
  filePaths: ReadonlySet<string>,
  defaultPath: string,
  search: string,
  hash: string,
): SourceLocation {
  const requestedPath = new URLSearchParams(search).get("path");
  const validPath = requestedPath == null || filePaths.has(requestedPath);
  return {
    path: requestedPath != null && validPath
      ? requestedPath
      : defaultPath,
    range: validPath ? parseSourceLineHash(hash) : null,
  };
}

function normalizeLineRange(
  range: SourceLineRange,
  totalLines: number | null,
): SourceLineRange | null {
  if (totalLines === 0) return null;
  const maximum = totalLines == null ? Number.MAX_SAFE_INTEGER : Math.max(1, totalLines);
  const start = Math.max(1, Math.min(maximum, range.start));
  const end = Math.max(start, Math.min(maximum, range.end));
  return { start, end };
}

function currentCompactWorkspace(): boolean {
  return typeof window !== "undefined" && window.matchMedia(COMPACT_WORKSPACE_QUERY).matches;
}

function currentCoarsePointer(): boolean {
  return typeof window !== "undefined" && window.matchMedia(COARSE_POINTER_QUERY).matches;
}

function useCompactWorkspace(): boolean {
  const [compact, setCompact] = useState(currentCompactWorkspace);
  useEffect(() => {
    const media = window.matchMedia(COMPACT_WORKSPACE_QUERY);
    const update = () => setCompact(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  return compact;
}

function useCoarsePointer(): boolean {
  const [coarse, setCoarse] = useState(currentCoarsePointer);
  useEffect(() => {
    return observeMediaQueryMatch(
      window.matchMedia(COARSE_POINTER_QUERY),
      setCoarse,
    );
  }, []);
  return coarse;
}

function sourceDirectoryPaths(files: readonly RepositoryFile[]): readonly string[] {
  const directories = new Set<string>();
  for (const file of files) {
    for (
      let index = file.path.indexOf("/");
      index >= 0;
      index = file.path.indexOf("/", index + 1)
    ) {
      directories.add(file.path.slice(0, index));
    }
  }
  return [...directories];
}

type SourceTreeDomState = {
  focusedPath: string | null;
  focusedViewportOffset: number | null;
  ownsFocus: boolean;
  scaledScrollTop: number | null;
  searchOwnsFocus: boolean;
};

function captureSourceTreeDomState(
  model: PierreFileTree,
  nextItemHeight: number,
): SourceTreeDomState {
  const container = model.getFileTreeContainer();
  const shadow = container?.shadowRoot;
  const active = shadow?.activeElement as HTMLElement | null | undefined;
  const scroll = shadow?.querySelector<HTMLElement>(
    "[data-file-tree-virtualized-scroll]",
  );
  const focusedPath = active?.closest<HTMLElement>("[data-item-path]")?.dataset.itemPath
    ?? model.getFocusedPath();
  const focusedRow = focusedPath
    ? Array.from(shadow?.querySelectorAll<HTMLElement>("[data-item-path]") ?? [])
      .find((element) => element.dataset.itemPath === focusedPath)
    : undefined;
  return {
    focusedPath,
    focusedViewportOffset: focusedRow && scroll
      ? focusedRow.getBoundingClientRect().top - scroll.getBoundingClientRect().top
      : null,
    ownsFocus: container?.ownerDocument.activeElement === container,
    scaledScrollTop: scroll
      ? scaledSourceTreeScrollTop({
          itemHeight: model.getItemHeight(),
          nextItemHeight,
          scrollTop: scroll.scrollTop,
        })
      : null,
    searchOwnsFocus: active?.matches("[data-file-tree-search-input]") === true,
  };
}

function restoreSourceTreeDomState(
  model: PierreFileTree,
  state: SourceTreeDomState,
): boolean {
  const shadow = model.getFileTreeContainer()?.shadowRoot;
  if (!shadow) return false;
  const scroll = shadow.querySelector<HTMLElement>(
    "[data-file-tree-virtualized-scroll]",
  );
  if (scroll && state.scaledScrollTop !== null) {
    scroll.scrollTop = state.scaledScrollTop;
  }
  const focusedRow = state.focusedPath
    ? Array.from(shadow.querySelectorAll<HTMLElement>("[data-item-path]"))
      .find((element) => element.dataset.itemPath === state.focusedPath)
    : undefined;
  if (state.focusedViewportOffset !== null && scroll && !focusedRow) return false;
  if (state.focusedViewportOffset !== null && scroll && focusedRow) {
    scroll.scrollTop += focusedRow.getBoundingClientRect().top
      - scroll.getBoundingClientRect().top
      - state.focusedViewportOffset;
  }
  if (!state.ownsFocus) return true;
  const target = state.searchOwnsFocus
    ? shadow.querySelector<HTMLElement>("[data-file-tree-search-input]")
    : focusedRow;
  if (!target) return false;
  target.focus({ preventScroll: true });
  return true;
}

function useResponsiveFileTree(
  options: FileTreeOptions,
  itemHeight: number,
  directoryPaths: readonly string[],
  focusFallbackRef: RefObject<HTMLElement | null>,
): PierreFileTree {
  const initialOptions = useRef(options);
  const cleanupTimers = useRef(new Map<PierreFileTree, number>());
  const [model, setModel] = useState(
    () => new PierreFileTree({ ...options, itemHeight }),
  );
  const currentModel = useRef(model);
  const pendingDomState = useRef<SourceTreeDomState | null>(null);

  const cancelCleanup = (candidate: PierreFileTree) => {
    const timer = cleanupTimers.current.get(candidate);
    if (timer === undefined) return;
    window.clearTimeout(timer);
    cleanupTimers.current.delete(candidate);
  };
  const scheduleCleanup = (candidate: PierreFileTree) => {
    if (cleanupTimers.current.has(candidate)) return;
    const timer = window.setTimeout(() => {
      candidate.cleanUp();
      cleanupTimers.current.delete(candidate);
    }, 1);
    cleanupTimers.current.set(candidate, timer);
  };

  useLayoutEffect(() => {
    const previous = currentModel.current;
    if (previous.getItemHeight() === itemHeight) return;
    const retained = retainedSourceTreeState(previous, directoryPaths);
    const domState = captureSourceTreeDomState(previous, itemHeight);
    const next = new PierreFileTree({
      ...initialOptions.current,
      initialExpandedPaths: retained.expandedPaths,
      initialExpansion: "closed",
      initialSearchQuery: retained.searchQuery,
      initialSelectedPaths: retained.selectedPaths,
      itemHeight,
    });
    if (retained.focusedPath) next.focusPath(retained.focusedPath);
    if (domState.ownsFocus) focusFallbackRef.current?.focus({ preventScroll: true });
    pendingDomState.current = domState;
    currentModel.current = next;
    scheduleCleanup(previous);
    setModel(next);
  }, [directoryPaths, itemHeight]);

  useLayoutEffect(() => {
    if (!pendingDomState.current) return;
    let frame = 0;
    let attempts = 0;
    const restore = () => {
      const pending = pendingDomState.current;
      if (!pending) return;
      if (restoreSourceTreeDomState(model, pending) || attempts >= 2) {
        pendingDomState.current = null;
        return;
      }
      attempts += 1;
      frame = window.requestAnimationFrame(restore);
    };
    frame = window.requestAnimationFrame(restore);
    return () => window.cancelAnimationFrame(frame);
  }, [model]);

  useEffect(() => {
    cancelCleanup(model);
    return () => scheduleCleanup(model);
  }, [model]);

  return model;
}

function CodeBrowserComponent(
  { files, branch, head, initialFile, readFile, theme }: CodeBrowserProps,
  ref: ForwardedRef<CodeBrowserHandle>,
) {
  const coarsePointer = useCoarsePointer();
  const location = useLocation();
  const navigate = useNavigate();
  const defaultPath = useMemo(
    () =>
      files.find((file) => file.path === "src/main.rs")?.path ??
      files.find((file) => file.path === "README.md")?.path ??
      files[0]?.path ??
      "",
    [files],
  );
  const fileByPath = useMemo(
    () => new Map(files.map((file) => [file.path, file])),
    [files],
  );
  const filePaths = useMemo(() => new Set(fileByPath.keys()), [fileByPath]);
  const initialLocation = useMemo(
    () => readSourceLocation(filePaths, defaultPath, location.search, location.hash),
    [defaultPath, filePaths, location.hash, location.search],
  );
  const [selectedPath, setSelectedPath] = useState(initialLocation.path);
  const [lineTarget, setLineTarget] = useState<SourceLineRange | null>(initialLocation.range);
  const selectedPathRef = useRef(selectedPath);
  selectedPathRef.current = selectedPath;
  const readFileRef = useRef(readFile);
  readFileRef.current = readFile;
  const [loaded, setLoaded] = useState<PreparedPublishedFile | null>(() =>
    initialFile?.file.path === initialLocation.path
      && fileByPath.get(initialLocation.path)?.objectId === initialFile.file.objectId
      ? initialFile
      : null
  );
  const [fileError, setFileError] = useState<SourceFileError | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [treeOpen, setTreeOpen] = useState(false);
  const compact = useCompactWorkspace();
  const modalOpen = compact && treeOpen;
  const workspaceRef = useRef<HTMLElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);
  const treePanelRef = useRef<HTMLDivElement>(null);
  const treeCloseRef = useRef<HTMLButtonElement>(null);
  const treeOpenerRef = useRef<HTMLButtonElement>(null);
  const codeViewContainerRef = useRef<HTMLDivElement>(null);
  const codeViewRef = useRef<CodeViewHandle<undefined>>(null);
  const suppressTreeSelectionRef = useRef(false);
  const renderer = usePierreRenderer();
  const initialVisibleRowCount = useMemo(getInitialBatchSize, []);
  const treeInput = useMemo(
    () => prepareFileTreeInput(files.map((file) => file.path), {
      flattenEmptyDirectories: true,
    }),
    [files],
  );
  const directoryPaths = useMemo(() => sourceDirectoryPaths(files), [files]);
  const model = useResponsiveFileTree({
    preparedInput: treeInput as unknown as FileTreePreparedInput,
    flattenEmptyDirectories: true,
    initialExpansion: 1,
    initialSelectedPaths: initialLocation.path ? [initialLocation.path] : [],
    initialSearchQuery: null,
    fileTreeSearchMode: "hide-non-matches",
    search: true,
    searchBlurBehavior: "close",
    stickyFolders: true,
    density: "compact",
    icons: { set: "standard", colored: false },
    initialVisibleRowCount,
    overscan: 10,
  }, sourceTreeItemHeight(coarsePointer), directoryPaths, workspaceRef);
  const modelRef = useRef(model);
  modelRef.current = model;
  const selected = fileByPath.get(selectedPath) ?? files[0];
  const displayed = loaded?.file;
  const contents = loaded?.contents ?? null;
  const viewFile = displayed ?? selected;
  const codeItems = useMemo<CodeViewItem<undefined>[]>(
    () => loaded ? [sourceCodeViewItem(loaded.file, loaded.contents)] : [],
    [loaded],
  );
  const codeReady = loaded != null && renderer.ready;
  const lineCount = useMemo(() => countLines(contents), [contents]);
  const normalizedLineTarget = useMemo(
    () => lineTarget == null ? null : normalizeLineRange(lineTarget, lineCount),
    [lineCount, lineTarget],
  );
  const codeItemId = codeItems[0]?.id ?? "";
  const selectedLines = useMemo<CodeViewSelection | null>(
    () => codeReady && loaded?.file.path === selectedPath && normalizedLineTarget
      ? {
          id: codeItemId,
          range: {
            start: normalizedLineTarget.start,
            end: normalizedLineTarget.end,
          },
        }
      : null,
    [codeItemId, codeReady, loaded?.file.path, normalizedLineTarget, selectedPath],
  );
  const treeTheme = useMemo(
    () => themeToTreeStyles(theme === "dark" ? pierreDark : pierreLight) as CSSProperties,
    [theme],
  );
  const locationRef = useRef(location);
  locationRef.current = location;
  const writeSourceLocation = useCallback((
    path: string,
    range: SourceLineRange | null,
    mode: "push" | "replace",
  ) => {
    const current = locationRef.current;
    const search = new URLSearchParams(current.search);
    if (path) search.set("path", path);
    else search.delete("path");
    const encodedSearch = search.toString();
    const hash = range == null
      ? ""
      : range.start === range.end
        ? `#L${range.start}`
        : `#L${range.start}-L${range.end}`;
    void navigate({
      pathname: current.pathname,
      search: encodedSearch ? `?${encodedSearch}` : "",
      hash,
    }, {
      replace: mode === "replace",
      preventScrollReset: true,
    });
  }, [navigate]);

  const closeTree = useCallback(() => {
    modelRef.current.closeSearch();
    setTreeOpen(false);
  }, []);

  const openTreeSearch = useCallback(() => {
    if (compact) setTreeOpen(true);
    modelRef.current.openSearch();
  }, [compact]);

  const closeSearches = useCallback(() => {
    modelRef.current.closeSearch();
    setTreeOpen(false);
  }, []);

  useImperativeHandle(
    ref,
    () => ({
      closeSearches,
      // Compatibility for the shell's existing handle; Source now has one search.
      openFileSearch: openTreeSearch,
      openTreeSearch,
    }),
    [closeSearches, openTreeSearch],
  );

  const syncTreeSelection = useCallback((path: string) => {
    const current = modelRef.current;
    suppressTreeSelectionRef.current = true;
    try {
      for (const selectedFile of current.getSelectedPaths()) {
        if (selectedFile !== path) current.getItem(selectedFile)?.deselect();
      }
      current.getItem(path)?.select();
      current.focusPath(path);
      current.scrollToPath(path, { offset: "center" });
    } finally {
      suppressTreeSelectionRef.current = false;
    }
  }, []);

  useEffect(() => {
    const requestedPath = new URLSearchParams(location.search).get("path");
    const next = readSourceLocation(
      filePaths,
      defaultPath,
      location.search,
      location.hash,
    );
    selectedPathRef.current = next.path;
    setSelectedPath(next.path);
    setLineTarget(next.range);
    setFileError(null);
    if (next.path) syncTreeSelection(next.path);
    closeTree();
    if (requestedPath != null && !filePaths.has(requestedPath) && defaultPath) {
      writeSourceLocation(defaultPath, null, "replace");
    }
  }, [
    closeTree,
    defaultPath,
    filePaths,
    location.hash,
    location.search,
    syncTreeSelection,
    writeSourceLocation,
  ]);

  useEffect(() => {
    return model.subscribe(() => {
      if (suppressTreeSelectionRef.current) return;
      const nextPath = model
        .getSelectedPaths()
        .slice()
        .reverse()
        .find((path) => fileByPath.has(path));
      if (!nextPath || nextPath === selectedPathRef.current) return;
      selectedPathRef.current = nextPath;
      setSelectedPath(nextPath);
      setLineTarget(null);
      setFileError(null);
      writeSourceLocation(nextPath, null, "push");
      closeTree();
    });
  }, [closeTree, fileByPath, model, writeSourceLocation]);

  useEffect(() => {
    if (!selected) {
      setFileError(null);
      return;
    }
    if (
      loaded?.file.path === selected.path
      && loaded.file.objectId === selected.objectId
    ) {
      setFileError(null);
      return;
    }
    let active = true;
    setFileError(null);
    readFileRef.current(selected)
      .then(async (nextContents) => {
        if (!active) return;
        await renderer.prepareItems([sourceCodeViewItem(selected, nextContents)]);
        if (!active) return;
        setLoaded({ contents: nextContents, file: selected });
        setFileError(null);
      })
      .catch((error: unknown) => {
        if (!active) return;
        setFileError({ file: selected, kind: classifySourceFileError(error) });
      });
    return () => {
      active = false;
    };
  }, [
    loadAttempt,
    loaded?.file.objectId,
    loaded?.file.path,
    renderer.prepareItems,
    selected?.objectId,
    selected?.path,
  ]);

  const applyLineTarget = useCallback(() => {
    if (!selectedLines) return;
    codeViewRef.current?.scrollTo({
      type: "range",
      id: selectedLines.id,
      range: selectedLines.range,
      align: "center",
      behavior: "instant",
    });
  }, [selectedLines]);

  useEffect(() => {
    applyLineTarget();
  }, [applyLineTarget]);

  useEffect(() => {
    const container = codeViewContainerRef.current;
    if (!container || !viewFile || !codeReady) return;
    container.tabIndex = 0;
    container.setAttribute("role", "region");
    container.setAttribute("aria-label", `${viewFile.path} source code`);
    return observePierreCodeScrollRegions(container, applyLineTarget);
  }, [applyLineTarget, codeReady, viewFile]);

  useEffect(() => {
    const panel = treePanelRef.current;
    if (!panel) return;
    let frame: number | undefined;
    let stopped = false;

    const exposeVirtualizedRows = (): boolean => {
      const shadowRoot = panel.querySelector("file-tree-container")?.shadowRoot;
      const root = shadowRoot?.querySelector<HTMLElement>(
        "[data-file-tree-virtualized-root]",
      );
      const rows = root?.querySelector<HTMLElement>(
        "[data-file-tree-virtualized-scroll]",
      );
      if (!root || !rows) return false;

      if (root.hasAttribute("role")) root.removeAttribute("role");
      if (root.hasAttribute("aria-label")) root.removeAttribute("aria-label");
      if (rows.getAttribute("role") !== "tree") rows.setAttribute("role", "tree");
      if (rows.getAttribute("aria-label") !== "Repository files") {
        rows.setAttribute("aria-label", "Repository files");
      }
      const rowsId = `${root.id || "repository-file-tree"}__rows`;
      if (rows.id !== rowsId) rows.id = rowsId;
      const searchInput = root.querySelector<HTMLInputElement>(
        "[data-file-tree-search-input]",
      );
      if (!searchInput) return false;
      if (searchInput.getAttribute("aria-label") !== "Search repository files") {
        searchInput.setAttribute("aria-label", "Search repository files");
      }
      if (searchInput.getAttribute("aria-controls") !== rowsId) {
        searchInput.setAttribute("aria-controls", rowsId);
      }
      return true;
    };

    const attach = () => {
      if (stopped) return;
      if (exposeVirtualizedRows()) return;
      frame = window.requestAnimationFrame(attach);
    };

    attach();
    return () => {
      stopped = true;
      if (frame !== undefined) window.cancelAnimationFrame(frame);
    };
  }, [model]);

  useEffect(() => {
    if (modalOpen && !model.isSearchOpen()) model.openSearch();
  }, [modalOpen, model]);

  useModalBoundary({
    backdropRef,
    fallbackFocusRef: workspaceRef,
    initialFocusRef: treeCloseRef,
    onDismiss: closeTree,
    open: modalOpen,
    panelRef: treePanelRef,
    returnFocusRef: treeOpenerRef,
  });

  useEffect(() => {
    if (!compact && treeOpen) closeTree();
  }, [closeTree, compact, treeOpen]);

  const codeViewOptions = useMemo<CodeViewOptions<undefined>>(
    () => ({
      layout: CODE_VIEW_LAYOUT,
      theme: CODE_VIEW_THEMES,
      themeType: theme,
      overflow: "scroll",
      disableFileHeader: true,
      lineHoverHighlight: "number",
      enableLineSelection: true,
      stickyHeaders: true,
      unsafeCSS: CODE_VIEW_CUSTOM_CSS,
    }),
    [theme],
  );

  const handleSelectedLinesChange = useCallback((selection: CodeViewSelection | null) => {
    if (!loaded || loaded.file.path !== selectedPathRef.current) return;
    const next = selection == null
      ? null
      : normalizeLineRange({
          start: selection.range.start,
          end: selection.range.end,
        }, lineCount);
    setLineTarget(next);
    writeSourceLocation(loaded.file.path, next, "replace");
  }, [lineCount, loaded, writeSourceLocation]);

  const errorCopy = fileError?.kind === "unsupported"
    ? `${fileError.file.path} is not available as text.`
    : fileError
      ? `Couldn’t load ${fileError.file.path}.`
      : "";

  return (
    <section
      ref={workspaceRef}
      className="code-workspace source-browser"
      aria-label="Code browser"
      tabIndex={-1}
    >
      <h1 className="sr-only">Nanocodex source code</h1>
      <div
        ref={backdropRef}
        className={modalOpen ? "workspace-backdrop is-visible" : "workspace-backdrop"}
        aria-hidden="true"
        onPointerDown={closeTree}
      />
      <div
        ref={treePanelRef}
        id="source-file-tree"
        className={modalOpen ? "code-tree-panel is-mobile-open" : "code-tree-panel"}
        aria-labelledby="source-tree-title"
        role={modalOpen ? "dialog" : "complementary"}
        aria-modal={modalOpen ? true : undefined}
        tabIndex={modalOpen ? -1 : undefined}
      >
        <header className="pierre-tree-heading source-tree-toolbar">
          <div className="source-tree-identity">
            <strong id="source-tree-title">Files</strong>
            <span>
              <GitBranch aria-hidden="true" /> {branch} · {head.slice(0, 7)}
            </span>
          </div>
          <div className="source-tree-actions">
            <span className="source-file-count">{files.length}</span>
            <button
              className="tree-search-trigger"
              type="button"
              onClick={openTreeSearch}
              aria-label="Search repository files"
              aria-keyshortcuts="Meta+P Control+P"
            >
              <Search aria-hidden="true" />
              <kbd>⌘/Ctrl P</kbd>
            </button>
            <button
              ref={treeCloseRef}
              className="tree-close-button"
              type="button"
              onClick={closeTree}
              aria-label="Close file tree"
            >
              <span aria-hidden="true">×</span>
            </button>
          </div>
        </header>
        <FileTreeView
          className="pierre-file-tree"
          model={model}
          style={treeTheme}
        />
      </div>

      <article
        className="code-file"
        aria-label={viewFile?.path ?? "File viewer"}
      >
        {viewFile ? (
          <>
            <header className="code-file-header">
              <button
                ref={treeOpenerRef}
                className="mobile-tree-toggle"
                type="button"
                onClick={openTreeSearch}
                aria-label="Open file tree and search files"
                aria-controls="source-file-tree"
                aria-expanded={modalOpen}
              >
                <PanelLeft aria-hidden="true" />
              </button>
              <div
                className="file-breadcrumb"
                role="group"
                aria-label={`File path: ${viewFile.path}`}
              >
                {viewFile.path.split("/").map((part, index, parts) => (
                  <span key={`${part}-${index}`}>
                    {part}
                    {index < parts.length - 1 ? <ChevronRight aria-hidden="true" /> : null}
                  </span>
                ))}
              </div>
              <div className="code-file-meta">
                <span>{formatBytes(viewFile.size)}</span>
                {lineCount !== null ? <span>{lineCount} lines</span> : null}
              </div>
            </header>
            {fileError && loaded ? (
              <div className="code-file-tail-error" role="alert">
                <span>{errorCopy}</span>
                {fileError.kind === "request" ? (
                  <button type="button" onClick={() => setLoadAttempt((attempt) => attempt + 1)}>
                    <RefreshCw aria-hidden="true" /> Retry
                  </button>
                ) : null}
              </div>
            ) : null}
            {codeReady ? (
              <CodeView
                ref={codeViewRef}
                key={renderer.disableWorkerPool ? "main" : "workers"}
                items={codeItems}
                className="code-file-frame code-view cv-scrollbar"
                containerRef={codeViewContainerRef}
                disableWorkerPool={renderer.disableWorkerPool}
                options={codeViewOptions}
                selectedLines={selectedLines}
                onSelectedLinesChange={handleSelectedLinesChange}
              />
            ) : fileError ? (
              <div className="code-file-frame">
                <div className="code-file-message" role="alert">
                  <FileQuestion aria-hidden="true" />
                  <p>{errorCopy}</p>
                  {fileError.kind === "request" ? (
                    <button type="button" onClick={() => setLoadAttempt((attempt) => attempt + 1)}>
                      <RefreshCw aria-hidden="true" /> Retry
                    </button>
                  ) : null}
                </div>
              </div>
            ) : null}
          </>
        ) : (
          <div className="code-file-message">This snapshot has no files.</div>
        )}
      </article>
    </section>
  );
}

export const CodeBrowser = memo(forwardRef(CodeBrowserComponent));
