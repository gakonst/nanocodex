"use client";

import {
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  ExternalLink,
  GitBranch,
  GitPullRequest,
  Menu,
  Search,
  X,
} from "lucide-react";
import {
  startTransition,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { flushSync } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import { DeviceConnect } from "./DeviceConnect";
import { AgentExperience } from "./AgentExperience";
import { Changelog, preloadChangelog } from "./Changelog";
import { ChiefOfStaffDemo } from "./ChiefOfStaffDemo";
import { CodeBrowser } from "./CodeBrowser";
import { CommitCodeStream } from "./CommitCodeStream";
import { Docs, preloadDocsRoute } from "./Docs";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./DropdownMenu";
import { Evals, preloadEvalOverview } from "./Evals";
import { HostedToolsDemo } from "./HostedToolsDemo";
import { MonsterWorld } from "./MonsterWorld";
import { lockDocumentScroll, useModalBoundary } from "./modalBoundary";
import { Multiplayer } from "./Multiplayer";
import { PierreWorkerProvider } from "./PierreWorkerProvider";
import { VirtualCommitList } from "./VirtualCommitList";
import type { CodeBrowserHandle } from "./CodeBrowser";
import type { CommitCodeStreamHandle } from "./CommitCodeStream";
import {
  commitPreparationMatchesIntent,
  settleRepositoryNavigationIntent,
} from "./commitRouteState";
import { fuzzyScore } from "./fuzzy";
import {
  accountNavigation,
  connectDemoUrl,
  demoNavigation,
  gitNavigation,
  pathForCommit,
  pathForSurface,
  primaryNavigation,
  surfaceFromUrl,
  type ProductNavigationItem,
  type Surface,
} from "./navigation";
import { COMPACT_WORKSPACE_QUERY } from "./pierreCodeView";
import { visualViewportKeyboardInset } from "./mobileInteraction";
import { RouteErrorBoundary } from "./RouteErrorBoundary";
import type {
  PublishedCommitHistory,
  PublishedCommitPage,
  PreparedPublishedFile,
  PublishedRepositorySnapshot,
} from "./publishedRepository";
import type { HarnessCommit } from "./threadRepositorySnapshot";
import { loadWorldAssets } from "./monsterWorldRenderer";
import { getBrowserThread } from "nanocodex/tools/browser";
import {
  prepareRepositorySurface,
  type PreparedDirectRoute,
  type PreparedRepositorySurface,
} from "./routeLoaders";

export type Theme = "light" | "dark";
type Scope = "all" | "eval" | "fix" | "docs" | "perf";
const emptyCommits: HarnessCommit[] = [];
const emptyCommitPages: PublishedCommitPage[] = [];
const COMMIT_HASH_PATTERN = /^[0-9a-f]{40}$/;

function commitHashFromSearch(search: string): string | undefined {
  const hash = new URLSearchParams(search).get("commit")?.toLowerCase();
  return hash && COMMIT_HASH_PATTERN.test(hash) ? hash : undefined;
}

function commitHashFromDestination(destination: string): string | undefined {
  return commitHashFromSearch(new URL(destination, "https://nanocodex.invalid").search);
}

const MODAL_FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function modalFocusableElements(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(MODAL_FOCUSABLE_SELECTOR))
    .filter((element) => !element.hidden && element.tabIndex >= 0);
}

function containModalFocus(event: KeyboardEvent, panel: HTMLElement | null) {
  if (event.key !== "Tab" || !panel) return;
  const focusable = modalFocusableElements(panel);
  const first = focusable[0];
  const last = focusable.at(-1);
  const active = window.document.activeElement;
  if (!first || !last) return;
  if (event.shiftKey && (active === first || !panel.contains(active))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (active === last || !panel.contains(active))) {
    event.preventDefault();
    first.focus();
  }
}

function activeFocusOwner(): HTMLElement | null {
  const active = window.document.activeElement;
  return active instanceof HTMLElement && active !== window.document.body ? active : null;
}

function restoreModalFocus(opener: { current: HTMLElement | null }) {
  const target = opener.current;
  opener.current = null;
  if (target?.isConnected && !target.closest("[inert]")) target.focus();
}

function isPlainProductNavigation(event: ReactMouseEvent<HTMLAnchorElement>): boolean {
  return !event.defaultPrevented
    && event.button === 0
    && !event.metaKey
    && !event.ctrlKey
    && !event.shiftKey
    && !event.altKey;
}

type RepositorySurface = Extract<Surface, "code" | "commits">;

type RouteLoadFailure = {
  error: Error;
  surface: Surface;
};

type RepositoryFailureTarget = {
  requestedCommit?: string;
  surface: RepositorySurface;
};

function routeLoadError(error: unknown, surface: Surface): RouteLoadFailure {
  return {
    error: error instanceof Error ? error : new Error(`${surface} route failed to load`),
    surface,
  };
}

const scopes: Array<{ id: Scope; label: string }> = [
  { id: "all", label: "All commits" },
  { id: "eval", label: "Eval" },
  { id: "fix", label: "Fix" },
  { id: "docs", label: "Docs" },
  { id: "perf", label: "Perf" },
];

function subjectScope(subject: string) {
  const prefix = subject.match(/^([a-z]+)(?:\([^)]*\))?:/i)?.[1]?.toLowerCase();
  return scopes.some(({ id }) => id === prefix) ? (prefix as Scope) : "other";
}

function commitSearchScore(commit: HarnessCommit, tokens: readonly string[]) {
  if (!tokens.length) return 0;
  const fields = [
    { value: commit.hash, weight: 160 },
    { value: commit.subject, weight: 120 },
    { value: commit.author, weight: 60 },
    { value: commit.body, weight: 30 },
    ...commit.files.map((file) => ({ value: file.path, weight: 90 })),
  ];

  let total = 0;
  for (const token of tokens) {
    const best = fields.reduce<number | null>((current, field) => {
      const score = fuzzyScore(field.value, token);
      if (score === null) return current;
      const weighted = score + field.weight;
      return current === null || weighted > current ? weighted : current;
    }, null);
    if (best === null) return null;
    total += best;
  }
  return total;
}

const installCommand = "curl -fsSL https://nanocodex.paradigm.xyz | bash";
const installOptions = [
  { id: "rust", label: "Rust", command: "cargo add nanocodex" },
  { id: "javascript", label: "JavaScript", command: "npm install nanocodex" },
] as const;
type InstallTarget = "shell" | (typeof installOptions)[number]["id"];

function RepositorySurfaceError({
  failed,
  onRetry,
}: {
  failed: boolean;
  onRetry(): void;
}) {
  if (!failed) return null;
  return (
    <section className="requests-empty page-grid" role="alert">
      <GitBranch aria-hidden="true" />
      <p className="eyebrow">Repository</p>
      <h1>Published repository unavailable.</h1>
      <p>The Source and Commits publication could not be loaded.</p>
      <button className="button button--medium" type="button" onClick={onRetry}>
        Try again
      </button>
    </section>
  );
}

type NanocodexAppProps = {
  preparedRoute?: PreparedDirectRoute;
};

export function NanocodexApp({ preparedRoute = {} }: NanocodexAppProps) {
  return <NanocodexShell preparedRoute={preparedRoute} />;
}

function NanocodexShell({ preparedRoute }: Required<NanocodexAppProps>) {
  const location = useLocation();
  const navigate = useNavigate();
  const [theme, setTheme] = useState<Theme>(() => {
    const stored = localStorage.getItem("nanocodex-theme");
    const initial = stored === "light" || stored === "dark" ? stored : "dark";
    document.documentElement.dataset.theme = initial;
    document
      .querySelector('meta[name="theme-color"]')
      ?.setAttribute("content", initial === "dark" ? "#161616" : "#ffffff");
    return initial;
  });
  const surface = surfaceFromUrl({
    pathname: location.pathname,
    searchParams: new URLSearchParams(location.search),
  });
  const connectDemoHref = connectDemoUrl(window.location.origin);
  const requestedCommit = surface === "commits"
    ? commitHashFromSearch(location.search)
    : undefined;
  const [threadId, setThreadId] = useState<string | undefined>(() =>
    surface === "home" || surface === "docs" ? undefined : getBrowserThread().id
  );
  const [snapshot, setSnapshot] = useState<PublishedRepositorySnapshot | undefined>(
    preparedRoute.repositorySnapshot,
  );
  const [sourceFile, setSourceFile] = useState<PreparedPublishedFile | undefined>(
    preparedRoute.sourceFile,
  );
  const [commitHistory, setCommitHistory] = useState<PublishedCommitHistory | undefined>(
    preparedRoute.commitHistory,
  );
  const [commitPages, setCommitPages] = useState<PublishedCommitPage[]>(() =>
    preparedRoute.commitHistory
      ? [preparedRoute.commitHistory.initialPage]
      : emptyCommitPages
  );
  const [repositoryLoadError, setRepositoryLoadError] = useState<RepositorySurface | null>(null);
  const [routeLoadFailure, setRouteLoadFailure] = useState<RouteLoadFailure | null>(null);
  const [commitMetadataError, setCommitMetadataError] = useState(false);
  const [scope, setScope] = useState<Scope>("all");
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [selectedHash, setSelectedHash] = useState<string | undefined>(() =>
    commitHashFromSearch(location.search)
  );
  const [commitRailOpen, setCommitRailOpen] = useState(false);
  const [headerInstallCopied, setHeaderInstallCopied] = useState<InstallTarget | null>(null);
  const [mobileNavigationOpen, setMobileNavigationOpen] = useState(false);
  const [demoNavigationOpen, setDemoNavigationOpen] = useState(false);
  const [gitNavigationOpen, setGitNavigationOpen] = useState(false);
  const [agentExperienceMounted, setAgentExperienceMounted] = useState(
    surface === "home" || surface === "agent",
  );
  const [retainedAgentSurface, setRetainedAgentSurface] = useState<"home" | "agent" | undefined>(
    surface === "home" || surface === "agent" ? surface : undefined,
  );
  const agentExperienceSurface = surface === "home" || surface === "agent"
    ? surface
    : retainedAgentSurface;
  const terminalSurfaceActive = surface === "home" || surface === "agent";
  const needsRepository = surface === "code" || surface === "commits";
  const shellRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchDialogRef = useRef<HTMLElement>(null);
  const searchOpenerRef = useRef<HTMLElement | null>(null);
  const headerCenterRef = useRef<HTMLDivElement>(null);
  const mobileNavigationBackdropRef = useRef<HTMLDivElement>(null);
  const mobileNavigationCloseRef = useRef<HTMLButtonElement>(null);
  const mobileNavigationPanelRef = useRef<HTMLElement>(null);
  const mobileNavigationTriggerRef = useRef<HTMLButtonElement>(null);
  const codeBrowserRef = useRef<CodeBrowserHandle>(null);
  const commitWorkspaceRef = useRef<HTMLElement>(null);
  const commitRailRef = useRef<HTMLElement>(null);
  const commitRailCloseRef = useRef<HTMLButtonElement>(null);
  const commitRailOpenerRef = useRef<HTMLElement | null>(null);
  const commitStreamRef = useRef<CommitCodeStreamHandle>(null);
  const repositoryRequestId = useRef(0);
  const surfaceNavigationId = useRef(0);
  const commitHistoryTargetRef = useRef<string | undefined>(
    preparedRoute.commitHistory && surface === "commits" ? requestedCommit : undefined,
  );
  const commitIntentTargetRef = useRef<string | undefined>(requestedCommit);
  const repositoryFailureTargetRef = useRef<RepositoryFailureTarget | undefined>(undefined);
  const commitHistoryHeadRef = useRef(commitHistory?.repository.head);
  commitHistoryHeadRef.current = commitHistory?.repository.head;
  const commitRailModalOpen = surface === "commits" && commitRailOpen;
  const commitSearchModalOpen = surface === "commits" && searchOpen;
  const commitModalOpen = commitRailModalOpen || commitSearchModalOpen;

  const closeCommitRail = useCallback(() => setCommitRailOpen(false), []);
  const closeMobileNavigation = useCallback(() => setMobileNavigationOpen(false), []);
  const toggleMobileNavigation = useCallback(
    () => setMobileNavigationOpen((current) => !current),
    [],
  );
  const openCommitRail = useCallback(() => {
    commitRailOpenerRef.current = activeFocusOwner();
    setCommitRailOpen(true);
  }, []);
  const closeCommitSearch = useCallback(() => setSearchOpen(false), []);
  const openCommitSearch = useCallback(() => {
    searchOpenerRef.current = commitRailModalOpen
      ? commitRailOpenerRef.current
      : activeFocusOwner();
    setCommitRailOpen(false);
    setSearchOpen(true);
  }, [commitRailModalOpen]);

  const retainAgentExperience = useCallback((nextSurface: Surface) => {
    if (nextSurface === "home" || nextSurface === "agent") {
      setAgentExperienceMounted(true);
    }
  }, []);

  const commits = useMemo(
    () => commitPages.length === 0
      ? emptyCommits
      : commitPages
        .slice()
        .sort((left, right) => left.index - right.index)
        .flatMap((page) => page.commits),
    [commitPages],
  );

  const copyHeaderInstall = useCallback((command: string, target: InstallTarget) => {
    void navigator.clipboard.writeText(command).then(() => {
      setHeaderInstallCopied(target);
      window.setTimeout(() => {
        setHeaderInstallCopied((current) => current === target ? null : current);
      }, 1_500);
    });
  }, []);

  const selected = useMemo(
    () =>
      commits.find((commit) => commit.hash === selectedHash) ??
      commits[0] ??
      null,
    [commits, selectedHash],
  );
  const scopeCounts = commitHistory?.scopeCounts ?? {
    all: commits.length,
    eval: 0,
    fix: 0,
    docs: 0,
    perf: 0,
  };
  const queryTokens = useMemo(
    () => query.trim().toLowerCase().split(/\s+/).filter(Boolean),
    [query],
  );

  const filteredCommits = useMemo(() => {
    const scoped = commits.filter(
      (commit) => scope === "all" || subjectScope(commit.subject) === scope,
    );
    if (!queryTokens.length) return scoped;
    return scoped
      .map((commit) => ({
        commit,
        score: commitSearchScore(commit, queryTokens),
      }))
      .filter(
        (match): match is { commit: HarnessCommit; score: number } =>
          match.score !== null,
      )
      .sort((left, right) => right.score - left.score)
      .map((match) => match.commit);
  }, [commits, queryTokens, scope]);

  const searchResults = useMemo(
    () => {
      if (!searchOpen) return [];
      return commits
        .map((commit) => ({
          commit,
          score: commitSearchScore(commit, queryTokens),
        }))
        .filter(
          (match): match is { commit: HarnessCommit; score: number } =>
            match.score !== null,
        )
        .sort((left, right) => right.score - left.score)
        .slice(0, 12)
        .map((match) => match.commit);
    },
    [commits, queryTokens, searchOpen],
  );

  const commitPreparedRepository = useCallback((loaded: PreparedRepositorySurface) => {
    if (loaded.surface === "code") {
      setSnapshot(loaded.snapshot);
      setSourceFile(loaded.sourceFile);
      return;
    }
    commitHistoryTargetRef.current = loaded.requestedCommit;
    commitHistoryHeadRef.current = loaded.history.repository.head;
    setCommitHistory(loaded.history);
    setCommitPages([loaded.history.initialPage]);
    setCommitMetadataError(false);
    setSelectedHash(loaded.history.initialCommitHash);
  }, []);

  const commitLoadedPage = useCallback((page: PublishedCommitPage) => {
    if (page.generation !== commitHistoryHeadRef.current) return;
    setCommitPages((current) => {
      const existing = current.findIndex(({ index }) => index === page.index);
      if (existing < 0) return [...current, page];
      if (current[existing] === page) return current;
      const next = current.slice();
      next[existing] = page;
      return next;
    });
    setCommitMetadataError(false);
  }, []);

  const requestRepository = useCallback((
    nextSurface: RepositorySurface,
    requestedCommit?: string,
  ) => {
    const requestId = ++repositoryRequestId.current;
    void prepareRepositorySurface(nextSurface, requestedCommit, true).then(
      (loaded) => {
        if (repositoryRequestId.current !== requestId) return;
        if (
          loaded.surface === "commits"
          && !commitPreparationMatchesIntent(
            loaded.requestedCommit,
            commitIntentTargetRef.current,
          )
        ) return;
        startTransition(() => {
          commitPreparedRepository(loaded);
          repositoryFailureTargetRef.current = undefined;
          setRepositoryLoadError((current) => current === nextSurface ? null : current);
        });
      },
      () => {
        if (repositoryRequestId.current !== requestId) return;
        if (
          nextSurface === "commits"
          && !commitPreparationMatchesIntent(
            requestedCommit,
            commitIntentTargetRef.current,
          )
        ) return;
        repositoryFailureTargetRef.current = { requestedCommit, surface: nextSurface };
        setRepositoryLoadError(nextSurface);
      },
    );
  }, [commitPreparedRepository]);

  const refreshRepository = useCallback(() => {
    if (!needsRepository) return;
    requestRepository(
      surface === "commits" ? "commits" : "code",
      surface === "commits" ? requestedCommit : undefined,
    );
  }, [needsRepository, requestRepository, requestedCommit, surface]);

  useLayoutEffect(() => {
    surfaceNavigationId.current++;
    commitIntentTargetRef.current = surface === "commits" ? requestedCommit : undefined;
  }, [location.key, requestedCommit, surface]);

  useLayoutEffect(() => {
    retainAgentExperience(surface);
    if (surface === "home" || surface === "agent") setRetainedAgentSurface(surface);
  }, [retainAgentExperience, surface]);

  useEffect(() => {
    if (!needsRepository) return;
    const ready = surface === "code"
      ? Boolean(snapshot)
      : Boolean(
          commitHistory
          && commitPreparationMatchesIntent(
            commitHistoryTargetRef.current,
            requestedCommit,
          )
        );
    if (ready) return;

    const failed = repositoryFailureTargetRef.current;
    const failureIsCurrent = failed?.surface === surface
      && (
        surface === "code"
        || commitPreparationMatchesIntent(failed.requestedCommit, requestedCommit)
      );
    if (repositoryLoadError === surface && failureIsCurrent) return;
    if (repositoryLoadError === surface) {
      repositoryFailureTargetRef.current = undefined;
      setRepositoryLoadError(null);
    }
    refreshRepository();
  }, [
    commitHistory,
    needsRepository,
    refreshRepository,
    repositoryLoadError,
    requestedCommit,
    snapshot,
    surface,
  ]);

  useEffect(() => {
    if (!requestedCommit || !commitHistory) return;
    const pageIndex = commitHistory.pageForCommit(requestedCommit);
    if (pageIndex == null) {
      setCommitMetadataError(true);
      return;
    }
    setSelectedHash(requestedCommit);
    let active = true;
    void commitHistory.loadPage(pageIndex).then((page) => {
      if (!active) return;
      commitLoadedPage(page);
      const frame = window.requestAnimationFrame(() => {
        commitStreamRef.current?.scrollToCommit(requestedCommit);
      });
      if (!active) window.cancelAnimationFrame(frame);
    }).catch(() => {
      if (active) setCommitMetadataError(true);
    });
    return () => {
      active = false;
    };
  }, [commitHistory, commitLoadedPage, requestedCommit]);

  const loadAllCommitMetadata = useCallback(() => {
    if (!commitHistory) return;
    const generation = commitHistory.repository.head;
    setCommitMetadataError(false);
    void commitHistory.loadAllPages().then((pages) => {
      if (
        commitHistoryHeadRef.current !== generation ||
        pages.some((page) => page.generation !== generation)
      ) {
        return;
      }
      setCommitPages(pages);
    }).catch(() => {
      if (commitHistoryHeadRef.current === generation) {
        setCommitMetadataError(true);
      }
    });
  }, [commitHistory]);

  const loadNextCommitMetadataPage = useCallback(() => {
    if (!commitHistory || commitPages.length === 0) return;
    const loadedPages = new Set(commitPages.map(({ index }) => index));
    const maximumLoadedPage = Math.max(...loadedPages);
    const nextPage = maximumLoadedPage + 1 < commitHistory.pageCount
      ? maximumLoadedPage + 1
      : Array.from(
        { length: commitHistory.pageCount },
        (_, page) => page,
      ).find((page) => !loadedPages.has(page));
    if (nextPage == null) return;
    const generation = commitHistory.repository.head;
    void commitHistory.loadPage(nextPage).then(commitLoadedPage).catch(() => {
      if (commitHistoryHeadRef.current === generation) {
        setCommitMetadataError(true);
      }
    });
  }, [commitHistory, commitLoadedPage, commitPages]);

  useEffect(() => {
    if (!searchOpen && scope === "all") return;
    if (commitPages.length >= (commitHistory?.pageCount ?? 0)) return;
    loadAllCommitMetadata();
  }, [
    commitHistory?.pageCount,
    commitPages.length,
    loadAllCommitMetadata,
    scope,
    searchOpen,
  ]);

  useEffect(() => () => {
    repositoryRequestId.current++;
    surfaceNavigationId.current++;
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document
      .querySelector('meta[name="theme-color"]')
      ?.setAttribute("content", theme === "dark" ? "#161616" : "#ffffff");
    localStorage.setItem("nanocodex-theme", theme);
  }, [theme]);

  useLayoutEffect(() => {
    if (!terminalSurfaceActive) return;
    const root = document.documentElement;
    const body = document.body;
    const agentSurface = shellRef.current;
    const viewport = window.visualViewport;
    const roots = [root, body] as const;
    const alreadyLocked = roots.map((element) =>
      element.classList.contains("agent-viewport-locked")
    );
    roots.forEach((element) => element.classList.add("agent-viewport-locked"));
    window.scrollTo(0, 0);
    const restoreScroll = lockDocumentScroll(root, body);
    const isComposerTarget = (target: EventTarget | null) =>
      target instanceof Element
      && agentSurface?.contains(target)
      && target.matches(".agent-touch-composer textarea");
    let keyboardTracking = isComposerTarget(document.activeElement);
    let appliedKeyboardInset: number | undefined;
    const anchorViewport = () => {
      if (!agentSurface?.isConnected || !viewport) return;
      const keyboardInset = keyboardTracking && Math.abs(viewport.scale - 1) < 0.01
        ? visualViewportKeyboardInset({
          baselineHeight: agentSurface.clientHeight,
          viewportHeight: viewport.height,
          viewportOffsetTop: viewport.offsetTop,
        })
        : 0;
      if (!isComposerTarget(document.activeElement) && keyboardInset === 0) {
        keyboardTracking = false;
      }
      if (appliedKeyboardInset !== keyboardInset) {
        appliedKeyboardInset = keyboardInset;
        agentSurface.style.setProperty("--terminal-keyboard-inset", `${keyboardInset}px`);
      }
    };
    const trackComposerFocus = (event: FocusEvent) => {
      if (isComposerTarget(event.target)) keyboardTracking = true;
      anchorViewport();
    };
    anchorViewport();
    viewport?.addEventListener("resize", anchorViewport);
    viewport?.addEventListener("scroll", anchorViewport);
    window.addEventListener("resize", anchorViewport);
    document.addEventListener("focusin", trackComposerFocus);
    document.addEventListener("focusout", trackComposerFocus);
    return () => {
      viewport?.removeEventListener("resize", anchorViewport);
      viewport?.removeEventListener("scroll", anchorViewport);
      window.removeEventListener("resize", anchorViewport);
      document.removeEventListener("focusin", trackComposerFocus);
      document.removeEventListener("focusout", trackComposerFocus);
      agentSurface?.style.removeProperty("--terminal-keyboard-inset");
      restoreScroll();
      roots.forEach((element, index) => {
        if (!alreadyLocked[index]) element.classList.remove("agent-viewport-locked");
      });
    };
  }, [terminalSurfaceActive]);

  useEffect(() => {
    if (surface === "docs") return;
    document.title = surface === "home"
      ? "Nanocodex · headless Rust agents SDK"
      : `${surface === "code"
        ? "Source"
        : surface === "connect"
          ? "Account"
          : surface === "tools"
            ? "Attached Tools"
            : surface === "chief-of-staff"
              ? "Chief of Staff"
            : `${surface[0].toUpperCase()}${surface.slice(1)}`} · Nanocodex`;
  }, [surface]);

  useLayoutEffect(() => {
    if (surface !== "home" || location.pathname !== "/") return;
    const search = new URLSearchParams(location.search);
    if (!search.has("thread")) return;
    search.delete("thread");
    const query = search.toString();
    navigate(query ? `/?${query}` : "/", { replace: true });
  }, [location.pathname, location.search, navigate, surface]);

  const threadSurfacePath = useCallback(
    (nextSurface: Surface) =>
      nextSurface === "home"
        ? pathForSurface("home")
        : nextSurface === "tools"
          ? pathForSurface("tools")
          : threadId
            ? `${pathForSurface(nextSurface)}?thread=${threadId}`
            : pathForSurface(nextSurface),
    [threadId],
  );

  const preloadSurface = useCallback((nextSurface: Surface) => {
    if (nextSurface === "home" || nextSurface === "agent") {
      return;
    }
    if (nextSurface === "multiplayer") {
      return;
    }
    if (nextSurface === "world") {
      void loadWorldAssets().catch(() => undefined);
      return;
    }
    if (nextSurface === "changelog") {
      void preloadChangelog().catch(() => undefined);
      return;
    }
    if (nextSurface === "docs") {
      void preloadDocsRoute("/docs").catch(() => undefined);
      return;
    }
    if (nextSurface === "code" || nextSurface === "commits") {
      void prepareRepositorySurface(nextSurface).catch(() => undefined);
      return;
    }
    if (nextSurface === "evals") {
      void preloadEvalOverview().catch(() => undefined);
    }
  }, []);

  const navigateToPreparedRepository = useCallback((
    nextSurface: RepositorySurface,
    destination: string,
    navigationId: number,
    nextThreadId: string,
  ) => {
    const requestedCommit = nextSurface === "commits"
      ? commitHashFromDestination(destination)
      : undefined;
    if (nextSurface === "commits") {
      commitIntentTargetRef.current = requestedCommit;
    }
    void settleRepositoryNavigationIntent({
      navigationId,
      latestNavigationId: () => surfaceNavigationId.current,
      preparation: prepareRepositorySurface(
        nextSurface,
        requestedCommit,
        true,
      ),
      onPrepared: (preparedRepository) => {
        flushSync(() => {
          if (!threadId) setThreadId(nextThreadId);
          commitPreparedRepository(preparedRepository);
          repositoryFailureTargetRef.current = undefined;
          setRepositoryLoadError((current) => current === nextSurface ? null : current);
        });
      },
      onFailure: () => {
        flushSync(() => {
          if (!threadId) setThreadId(nextThreadId);
          repositoryFailureTargetRef.current = { requestedCommit, surface: nextSurface };
          setRepositoryLoadError(nextSurface);
        });
      },
      navigate: () => startTransition(() => navigate(destination)),
    });
  }, [commitPreparedRepository, navigate, threadId]);

  const navigateToSurface = useCallback((nextSurface: Surface) => {
    retainAgentExperience(nextSurface);
    preloadSurface(nextSurface);
    const navigationId = ++surfaceNavigationId.current;
    if (nextSurface === "docs") {
      const destination = pathForSurface(nextSurface);
      if (`${location.pathname}${location.search}` === destination) return;
      repositoryRequestId.current++;
      // The Docs route resolves its small source document as part of intent.
      // Keep the complete current surface visible until that atomic page is
      // ready, then navigate outside React's lower-priority transition lane.
      void preloadDocsRoute(destination).then(
          () => {
            if (surfaceNavigationId.current !== navigationId) return;
            flushSync(() => {
              setRouteLoadFailure((current) =>
                current?.surface === "docs" ? null : current
              );
            });
            navigate(destination);
          },
          (error: unknown) => {
            if (surfaceNavigationId.current !== navigationId) return;
            flushSync(() => setRouteLoadFailure(routeLoadError(error, "docs")));
            navigate(destination);
          },
        );
      return;
    }
    if (nextSurface === "home") {
      const destination = pathForSurface("home");
      if (`${location.pathname}${location.search}` === destination) return;
      repositoryRequestId.current++;
      startTransition(() => navigate(destination));
      return;
    }
    if (nextSurface === "tools") {
      const destination = pathForSurface("tools");
      if (`${location.pathname}${location.search}` === destination) return;
      repositoryRequestId.current++;
      startTransition(() => navigate(destination));
      return;
    }
    const nextThreadId = threadId ?? crypto.randomUUID();
    const destination = `${pathForSurface(nextSurface)}?thread=${nextThreadId}`;
    if (`${location.pathname}${location.search}` === destination) return;
    repositoryRequestId.current++;
    if (nextSurface === "code" || nextSurface === "commits") {
      const ready = nextSurface === "code"
        ? Boolean(snapshot)
        : Boolean(
            commitHistory
            && commitPreparationMatchesIntent(
              commitHistoryTargetRef.current,
              undefined,
            )
          );
      if (ready) {
        if (nextSurface === "commits") commitIntentTargetRef.current = undefined;
        if (!threadId) setThreadId(nextThreadId);
        startTransition(() => navigate(destination));
        return;
      }
      navigateToPreparedRepository(nextSurface, destination, navigationId, nextThreadId);
      return;
    }
    if (!threadId) setThreadId(nextThreadId);
    startTransition(() => navigate(destination));
  }, [
    commitHistory,
    location.pathname,
    location.search,
    navigate,
    navigateToPreparedRepository,
    preloadSurface,
    retainAgentExperience,
    snapshot,
    threadId,
  ]);

  const handleSurfaceClick = useCallback((
    event: ReactMouseEvent<HTMLAnchorElement>,
    nextSurface: Surface,
  ) => {
    if (!isPlainProductNavigation(event)) return;
    event.preventDefault();
    navigateToSurface(nextSurface);
  }, [navigateToSurface]);

  const handleCommitClick = useCallback((
    event: ReactMouseEvent<HTMLAnchorElement>,
    hash: string,
  ) => {
    if (!isPlainProductNavigation(event)) return;
    event.preventDefault();
    const destination = pathForCommit(hash);
    if (`${location.pathname}${location.search}` === destination) return;
    retainAgentExperience("commits");
    const navigationId = ++surfaceNavigationId.current;
    const nextThreadId = threadId ?? crypto.randomUUID();
    repositoryRequestId.current++;
    navigateToPreparedRepository("commits", destination, navigationId, nextThreadId);
  }, [
    location.pathname,
    location.search,
    navigateToPreparedRepository,
    retainAgentExperience,
    threadId,
  ]);

  useLayoutEffect(() => {
    const headerCenter = headerCenterRef.current;
    const activeButton =
      headerCenter?.querySelector<HTMLElement>(".is-active");
    if (
      !headerCenter ||
      !activeButton ||
      headerCenter.scrollWidth <= headerCenter.clientWidth
    )
      return;
    headerCenter.scrollLeft =
      activeButton.offsetLeft -
      (headerCenter.clientWidth - activeButton.offsetWidth) / 2;
  }, [surface]);

  useEffect(() => {
    if (surface === "commits") return;
    setSearchOpen(false);
    setCommitRailOpen(false);
  }, [surface]);

  useEffect(closeMobileNavigation, [closeMobileNavigation, surface]);

  useEffect(() => {
    const mobile = window.matchMedia("(max-width: 740px)");
    const closeOnDesktop = () => {
      if (!mobile.matches) closeMobileNavigation();
    };
    mobile.addEventListener("change", closeOnDesktop);
    return () => mobile.removeEventListener("change", closeOnDesktop);
  }, [closeMobileNavigation]);

  useModalBoundary({
    backdropRef: mobileNavigationBackdropRef,
    initialFocusRef: mobileNavigationCloseRef,
    onDismiss: closeMobileNavigation,
    open: mobileNavigationOpen,
    panelRef: mobileNavigationPanelRef,
    returnFocusRef: mobileNavigationTriggerRef,
  });

  useEffect(() => {
    const compact = window.matchMedia(COMPACT_WORKSPACE_QUERY);
    const closeRailOnDesktop = () => {
      if (!compact.matches) closeCommitRail();
    };
    closeRailOnDesktop();
    compact.addEventListener("change", closeRailOnDesktop);
    return () => compact.removeEventListener("change", closeRailOnDesktop);
  }, [closeCommitRail]);

  useEffect(() => {
    if (!commitModalOpen) return;
    const root = window.document.documentElement;
    const body = window.document.body;
    const previousRootOverflow = root.style.overflow;
    const previousRootOverscroll = root.style.overscrollBehavior;
    const previousBodyOverflow = body.style.overflow;
    root.style.overflow = "hidden";
    root.style.overscrollBehavior = "none";
    body.style.overflow = "hidden";
    return () => {
      root.style.overflow = previousRootOverflow;
      root.style.overscrollBehavior = previousRootOverscroll;
      body.style.overflow = previousBodyOverflow;
    };
  }, [commitModalOpen]);

  useEffect(() => {
    if (!commitRailModalOpen) return;
    const workspace = commitWorkspaceRef.current;
    const rail = commitRailRef.current;
    const background = new Map<HTMLElement, boolean>();
    const inertBackground = () => {
      for (const element of Array.from(workspace?.children ?? [])) {
        if (
          !(element instanceof HTMLElement)
          || element === rail
          || element.classList.contains("workspace-backdrop")
        ) continue;
        if (!background.has(element)) background.set(element, element.inert);
        element.inert = true;
      }
    };
    inertBackground();
    let backgroundObserver: MutationObserver | undefined;
    if (workspace) {
      backgroundObserver = new MutationObserver(inertBackground);
      backgroundObserver.observe(workspace, { childList: true });
    }
    const focusFrame = window.requestAnimationFrame(() => commitRailCloseRef.current?.focus());
    const trapFocus = (event: KeyboardEvent) => containModalFocus(event, commitRailRef.current);
    window.addEventListener("keydown", trapFocus);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      window.removeEventListener("keydown", trapFocus);
      backgroundObserver?.disconnect();
      for (const [element, inert] of background) element.inert = inert;
      restoreModalFocus(commitRailOpenerRef);
    };
  }, [commitRailModalOpen]);

  useEffect(() => {
    if (!commitSearchModalOpen) return;
    const focusFrame = window.requestAnimationFrame(() => searchInputRef.current?.focus());
    const trapFocus = (event: KeyboardEvent) => containModalFocus(event, searchDialogRef.current);
    window.addEventListener("keydown", trapFocus);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      window.removeEventListener("keydown", trapFocus);
      restoreModalFocus(searchOpenerRef);
    };
  }, [commitSearchModalOpen]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const originalTarget = event.composedPath()[0];
      const target =
        originalTarget instanceof HTMLElement
          ? originalTarget
          : (event.target as HTMLElement | null);
      const isTyping = target?.matches(
        "input, textarea, [contenteditable='true']"
      );
      const primaryModifier = event.ctrlKey || event.metaKey;
      const key = event.key.toLowerCase();

      if (
        surface === "code" &&
        primaryModifier &&
        !event.altKey &&
        key === "p"
      ) {
        event.preventDefault();
        event.stopPropagation();
        codeBrowserRef.current?.openTreeSearch();
        return;
      }
      if (event.key === "Escape") {
        if (commitSearchModalOpen) {
          event.preventDefault();
          event.stopPropagation();
          closeCommitSearch();
          return;
        }
        if (commitRailModalOpen) {
          event.preventDefault();
          event.stopPropagation();
          closeCommitRail();
          return;
        }
        codeBrowserRef.current?.closeSearches();
        return;
      }
      if (commitModalOpen) return;
      if (isTyping || primaryModifier || event.altKey) return;
      if (surface === "world"
        && target === document.activeElement
        && target?.matches(".monster-world-stage canvas")
        && ["w", "a", "s", "d"].includes(key)) {
        // The World surface owns WASD only while its game canvas has focus.
        return;
      }
      if (key === "f") {
        if (surface !== "commits") return;
        event.preventDefault();
        event.stopPropagation();
        openCommitSearch();
        return;
      }
      if (key === "m") {
        event.preventDefault();
        event.stopPropagation();
        setTheme((current) => (current === "light" ? "dark" : "light"));
        return;
      }
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [
    closeCommitRail,
    closeCommitSearch,
    commitModalOpen,
    commitRailModalOpen,
    commitSearchModalOpen,
    openCommitSearch,
    surface,
  ]);

  const selectCommit = (commit: HarnessCommit) => {
    commitIntentTargetRef.current = commit.hash;
    commitHistoryTargetRef.current = commit.hash;
    setSelectedHash(commit.hash);
    closeCommitSearch();
    closeCommitRail();
    setQuery("");
    navigate(pathForCommit(commit.hash), { replace: true });
    commitStreamRef.current?.scrollToCommit(commit.hash);
  };

  const demoNavigationActive = demoNavigation.some((item) => item.surface === surface);
  const gitNavigationActive = gitNavigation.some((item) => item.surface === surface);
  const surfaceNavigationLink = (
    item: ProductNavigationItem,
    context: "desktop" | "group" | "mobile",
    closeMenu?: () => void,
  ) => (
    <a
      className={`${context === "group" ? "surface-navigation-menu-item" : ""}${
        surface === item.surface ? " is-active" : ""
      }`.trim()}
      href={item.surface === "docs"
        ? pathForSurface(item.surface)
        : threadSurfacePath(item.surface)}
      aria-current={surface === item.surface ? "page" : undefined}
      key={item.surface}
      onFocus={() => preloadSurface(item.surface)}
      onPointerEnter={() => preloadSurface(item.surface)}
      onPointerDown={() => preloadSurface(item.surface)}
      onClick={(event) => {
        closeMenu?.();
        if (context === "mobile") closeMobileNavigation();
        handleSurfaceClick(event, item.surface);
      }}
    >
      {context === "mobile" ? <>
        <span>{item.label}</span><small>{item.description}</small>
      </> : <span className="surface-label">{item.label}</span>}
    </a>
  );

  return (
    <div className={`site-shell surface-${surface}`} ref={shellRef}>
        <header
          className="site-header"
          inert={commitModalOpen ? true : undefined}
        >
          <div className="site-brand">
            <a
              className="brand-parent"
              href="https://paradigm.xyz"
              target="_blank"
              rel="noreferrer"
              aria-label="Paradigm"
              title="Paradigm"
            >
              <span className="paradigm-mark" aria-hidden="true" />
            </a>
            <a
              className={surface === "home" ? "wordmark is-active" : "wordmark"}
              href={threadSurfacePath("home")}
              aria-label="Nanocodex home"
              aria-current={surface === "home" ? "page" : undefined}
              onFocus={() => preloadSurface("home")}
              onPointerEnter={() => preloadSurface("home")}
              onPointerDown={() => preloadSurface("home")}
              onClick={(event) => handleSurfaceClick(event, "home")}
            >
              Nanocodex
            </a>
          </div>
          <div className="header-center" ref={headerCenterRef}>
            <nav className="surface-switch" aria-label="Product navigation">
              <a
                className={surface === "home" ? "is-active" : ""}
                href={threadSurfacePath("home")}
                aria-current={surface === "home" ? "page" : undefined}
                onFocus={() => preloadSurface("home")}
                onPointerEnter={() => preloadSurface("home")}
                onPointerDown={() => preloadSurface("home")}
                onClick={(event) => handleSurfaceClick(event, "home")}
              >
                <span className="surface-label">Home</span>
              </a>
              {surfaceNavigationLink(accountNavigation, "desktop")}
              <span
                className={`surface-navigation-group${demoNavigationActive ? " is-active" : ""}`}
              >
                <DropdownMenu open={demoNavigationOpen} onOpenChange={setDemoNavigationOpen}>
                  <DropdownMenuTrigger asChild>
                    <button type="button" aria-label="Demos navigation">
                      <span className="surface-label">Demos</span><ChevronDown aria-hidden="true" />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="surface-navigation-menu" aria-label="Demos">
                    {demoNavigation.map((item) => (
                      <DropdownMenuItem asChild key={item.surface}>
                        {surfaceNavigationLink(item, "group", () => setDemoNavigationOpen(false))}
                      </DropdownMenuItem>
                    ))}
                    <DropdownMenuItem asChild>
                      <a
                        className="surface-navigation-menu-item"
                        href={connectDemoHref}
                        target="_blank"
                        rel="noreferrer"
                        aria-label="Connect demo (opens in a new tab)"
                        onClick={() => setDemoNavigationOpen(false)}
                      >
                        <span className="surface-label">Connect</span><ExternalLink aria-hidden="true" />
                      </a>
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </span>
              {primaryNavigation.map((item) => surfaceNavigationLink(item, "desktop"))}
              <span
                className={`surface-navigation-group${gitNavigationActive ? " is-active" : ""}`}
              >
                <DropdownMenu open={gitNavigationOpen} onOpenChange={setGitNavigationOpen}>
                  <DropdownMenuTrigger asChild>
                    <button type="button" aria-label="Git navigation">
                      <span className="surface-label">Git</span><ChevronDown aria-hidden="true" />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="surface-navigation-menu" aria-label="Git">
                    {gitNavigation.map((item) => (
                      <DropdownMenuItem asChild key={item.surface}>
                        {surfaceNavigationLink(item, "group", () => setGitNavigationOpen(false))}
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuContent>
                </DropdownMenu>
              </span>
            </nav>
          </div>
          <nav className="header-actions" aria-label="Site actions">
            <button
              ref={mobileNavigationTriggerRef}
              className="mobile-navigation-trigger"
              type="button"
              aria-expanded={mobileNavigationOpen}
              aria-controls="mobile-product-navigation"
              aria-label={mobileNavigationOpen ? "Close Explore navigation" : "Open Explore navigation"}
              onClick={toggleMobileNavigation}
            >
              <Menu aria-hidden="true" />
            </button>
            <div className="header-install">
              <button
                className="header-install-trigger"
                type="button"
                aria-label="Copy Nanocodex install command"
                onClick={() => copyHeaderInstall(installCommand, "shell")}
              >
                {headerInstallCopied === "shell" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                <span>{headerInstallCopied === "shell" ? "copied" : "install"}</span>
              </button>
              <div className="header-install-menu" aria-label="Package install commands">
                <div className="header-install-menu-inner">
                  {installOptions.map((option) => (
                    <button
                      type="button"
                      key={option.id}
                      aria-label={`Copy ${option.label} install command`}
                      onClick={() => copyHeaderInstall(option.command, option.id)}
                    >
                      <span>{option.label}</span>
                      <code>{option.command}</code>
                      {headerInstallCopied === option.id ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </nav>
        </header>

        {mobileNavigationOpen ? <>
          <div
            ref={mobileNavigationBackdropRef}
            className="mobile-navigation-backdrop"
            aria-hidden="true"
            onClick={closeMobileNavigation}
          />
          <section
            ref={mobileNavigationPanelRef}
            className="mobile-product-navigation"
            id="mobile-product-navigation"
            role="dialog"
            aria-modal="true"
            aria-labelledby="mobile-product-navigation-title"
            tabIndex={-1}
          >
            <header>
              <div>
                <strong id="mobile-product-navigation-title">Explore Nanocodex</strong>
                <span>Choose a workspace</span>
              </div>
              <button ref={mobileNavigationCloseRef} type="button" aria-label="Close product navigation"
                onClick={closeMobileNavigation}><X aria-hidden="true" /></button>
            </header>
            <nav className="mobile-navigation-sections" aria-label="Mobile product navigation">
              <a
                className={`mobile-navigation-home${surface === "home" ? " is-active" : ""}`}
                href={threadSurfacePath("home")}
                aria-current={surface === "home" ? "page" : undefined}
                onClick={(event) => {
                  closeMobileNavigation();
                  handleSurfaceClick(event, "home");
                }}
              >
                <span>Home</span><small>Overview</small>
              </a>
              <div className="mobile-navigation-grid mobile-navigation-account">
                {surfaceNavigationLink(accountNavigation, "mobile")}
              </div>
              <section className="mobile-navigation-group" aria-labelledby="mobile-demos-title">
                <h2 id="mobile-demos-title">Demos</h2>
                <div className="mobile-navigation-grid">
                  {demoNavigation.map((item) => surfaceNavigationLink(item, "mobile"))}
                  <a
                    href={connectDemoHref}
                    target="_blank"
                    rel="noreferrer"
                    aria-label="Connect demo (opens in a new tab)"
                    onClick={closeMobileNavigation}
                  >
                    <span>Connect <ExternalLink aria-hidden="true" /></span><small>External demo</small>
                  </a>
                </div>
              </section>
              <div className="mobile-navigation-grid mobile-navigation-direct">
                {primaryNavigation.map((item) => surfaceNavigationLink(item, "mobile"))}
              </div>
              <section className="mobile-navigation-group" aria-labelledby="mobile-git-title">
                <h2 id="mobile-git-title">Git</h2>
                <div className="mobile-navigation-grid">
                  {gitNavigation.map((item) => surfaceNavigationLink(item, "mobile"))}
                </div>
              </section>
            </nav>
          </section>
        </> : null}

        <main
          id="top"
          inert={commitSearchModalOpen ? true : undefined}
        >
          <RouteErrorBoundary surface={agentExperienceMounted ? "agent" : "home"}>
          {surface === "home" ||
          surface === "agent" ||
          agentExperienceMounted ? (
            <section
              className={
                surface === "home"
                  ? "home-page is-home"
                  : surface === "agent"
                    ? "home-page is-agent"
                    : "home-page is-stashed"
              }
              hidden={surface !== "home" && surface !== "agent"}
              inert={surface !== "home" && surface !== "agent" ? true : undefined}
              aria-hidden={surface !== "home" && surface !== "agent"}
              aria-labelledby={surface === "agent" ? "agent-page-title" : "home-title"}
            >
              <article className="home-article">
                <h1
                  className="sr-only"
                  id={surface === "agent" ? "agent-page-title" : "home-title"}
                >
                  {surface === "agent" ? "Nanocodex durable agent" : "High-performance Codex SDK. Runs anywhere."}
                </h1>
                <section className="home-demo" id="agent-demo">
                  <AgentExperience
                    landing={agentExperienceSurface === "home"}
                    mode={
                      surface === "agent"
                        ? "full"
                        : surface === "home"
                          ? "preview"
                          : "hidden"
                    }
                  />
                </section>
              </article>
            </section>
          ) : null}
          </RouteErrorBoundary>

          {surface === "home" || surface === "agent" ? null : (
          <RouteErrorBoundary
            key={surface}
            failure={
              routeLoadFailure?.surface === surface
                ? routeLoadFailure.error
                : undefined
            }
            surface={surface}
          >
          {surface === "connect" ? (
            <DeviceConnect />
          ) : surface === "chief-of-staff" ? (
            <ChiefOfStaffDemo />
          ) : surface === "tools" ? (
            <HostedToolsDemo />
          ) : surface === "multiplayer" ? (
            <Multiplayer />
          ) : surface === "world" ? (
            <MonsterWorld />
          ) : surface === "changelog" ? (
            <Changelog onCommitClick={handleCommitClick} />
          ) : surface === "docs" ? (
            <Docs />
          ) : surface === "code" ? repositoryLoadError === "code" ? (
            <RepositorySurfaceError
              failed
              onRetry={refreshRepository}
            />
          ) : snapshot ? (
            <PierreWorkerProvider>
              <CodeBrowser
                key={snapshot.repository.head}
                ref={codeBrowserRef}
                files={snapshot.tree}
                branch={snapshot.repository.branch}
                head={snapshot.repository.head}
                initialFile={sourceFile}
                readFile={snapshot.readFile}
                theme={theme}
              />
            </PierreWorkerProvider>
          ) : null : surface === "commits" ? repositoryLoadError === "commits" ? (
            <RepositorySurfaceError
              failed
              onRetry={refreshRepository}
            />
          ) : commitHistory ? (
            <PierreWorkerProvider>
                <section
                  ref={commitWorkspaceRef}
                  className="commits-workspace"
                  aria-label="Repository commits"
                >
                <h1 className="sr-only">Nanocodex repository commits</h1>
                <button
                  className={
                    commitRailModalOpen
                      ? "workspace-backdrop is-visible"
                      : "workspace-backdrop"
                  }
                  type="button"
                  aria-hidden="true"
                  tabIndex={-1}
                  onPointerDown={closeCommitRail}
                />
                <aside
                  ref={commitRailRef}
                  id="commit-index"
                  className={
                    commitRailModalOpen
                      ? "commit-sidebar is-mobile-open"
                      : "commit-sidebar"
                  }
                  aria-labelledby="history-title"
                  role={commitRailModalOpen ? "dialog" : "complementary"}
                  aria-modal={commitRailModalOpen ? true : undefined}
                >
                  <header className="commit-sidebar-header">
                    <div>
                      <strong id="history-title">Jump to commit</strong>
                      <span>
                        <GitBranch aria-hidden="true" />{" "}
                        {commitHistory.repository.branch} · {commitHistory.hashes.length}
                      </span>
                    </div>
                    <nav
                      className="commit-sidebar-actions"
                      aria-label="Commit index actions"
                    >
                      <button
                        className="icon-button"
                        type="button"
                        onClick={openCommitSearch}
                      >
                        <Search aria-hidden="true" />
                        <span className="sr-only">Find commits</span>
                        <kbd>F</kbd>
                      </button>
                      <button
                        ref={commitRailCloseRef}
                        className="mobile-drawer-close"
                        type="button"
                        onClick={closeCommitRail}
                        aria-label="Close commit index"
                      >
                        <X aria-hidden="true" />
                      </button>
                    </nav>
                  </header>

                  <nav
                    className="commit-scope-tabs"
                    aria-label="Quick jump scopes"
                  >
                    {scopes.map((item) => (
                      <button
                        className={scope === item.id ? "is-active" : ""}
                        type="button"
                        key={item.id}
                        onClick={() => setScope(item.id)}
                      >
                      {item.label} <span>{scopeCounts[item.id]}</span>
                      </button>
                    ))}
                  </nav>

                  {query ? (
                    <div className="commit-query">
                      <span>
                        {filteredCommits.length} matches for “{query}”
                      </span>
                      <button
                        type="button"
                        onClick={() => setQuery("")}
                        aria-label="Clear commit search"
                      >
                        <X aria-hidden="true" />
                      </button>
                    </div>
                  ) : null}

                  <VirtualCommitList
                    commits={filteredCommits}
                    hasMore={
                      scope === "all" &&
                      !query &&
                      commitPages.length < commitHistory.pageCount
                    }
                    selectedHash={selected?.hash}
                    onClearSearch={() => setQuery("")}
                    onLoadMore={loadNextCommitMetadataPage}
                    onSelectCommit={selectCommit}
                  />
                  {commitMetadataError ? (
                    <div className="commit-stream-tail-error" role="alert">
                      <span>Couldn’t load complete commit metadata.</span>
                      <button type="button" onClick={loadAllCommitMetadata}>
                        Try again
                      </button>
                    </div>
                  ) : null}
                </aside>
                <CommitCodeStream
                  key={`${commitHistory.repository.head}:${commitHistory.initialPage.index}`}
                  ref={commitStreamRef}
                  commitRailOpen={commitRailModalOpen}
                  history={commitHistory}
                  onPageLoaded={commitLoadedPage}
                  onOpenCommitRail={openCommitRail}
                  theme={theme}
                />
                </section>
            </PierreWorkerProvider>
          ) : null : surface === "requests" ? (
            <section
              className="requests-empty page-grid"
              aria-labelledby="requests-title"
            >
              <GitPullRequest aria-hidden="true" />
              <p className="eyebrow">Requests</p>
              <h1 id="requests-title">No requests yet.</h1>
              <p>
                This view is reserved for proposed changes. We’ll leave it quiet
                for now.
              </p>
            </section>
          ) : (
            <Evals />
          )}
          </RouteErrorBoundary>
          )}
        </main>

        {commitSearchModalOpen ? (
          <div
            className="overlay"
            role="presentation"
            onPointerDown={closeCommitSearch}
          >
            <section
              ref={searchDialogRef}
              className="search-dialog"
              role="dialog"
              aria-modal="true"
              aria-label="Find commits"
              onPointerDown={(event) => event.stopPropagation()}
            >
              <div className="search-field">
                <Search aria-hidden="true" />
                <input
                  ref={searchInputRef}
                  aria-label="Find commits"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Search hashes, messages, authors, and paths"
                />
                <button
                  type="button"
                  onClick={closeCommitSearch}
                  aria-label="Close search"
                >
                  <X aria-hidden="true" />
                </button>
              </div>
              <div className="search-results">
                {searchResults.length ? (
                  searchResults.map((commit, index) => (
                    <button
                      className={
                        index === 0 ? "search-result is-first" : "search-result"
                      }
                      type="button"
                      key={commit.hash}
                      onClick={() => selectCommit(commit)}
                    >
                      <span>{commit.shortHash}</span>
                      <strong>{commit.subject}</strong>
                      <small>{commit.author}</small>
                      <ChevronRight aria-hidden="true" />
                    </button>
                  ))
                ) : commitMetadataError ? (
                  <div className="search-empty" role="alert">
                    <p>Couldn’t load complete commit metadata.</p>
                    <button type="button" onClick={loadAllCommitMetadata}>
                      Try again
                    </button>
                  </div>
                ) : commitPages.length >= (commitHistory?.pageCount ?? 0) ? (
                  <p className="search-empty">No commits found.</p>
                ) : null}
              </div>
              <footer className="search-footer">
                <span>{searchResults.length} results</span>
                <span>Esc to close</span>
              </footer>
            </section>
          </div>
        ) : null}

    </div>
  );
}
