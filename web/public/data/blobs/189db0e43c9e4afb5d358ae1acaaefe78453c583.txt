"use client";

import {
  ArrowUpRight,
  ChevronRight,
  GitBranch,
  GitPullRequest,
  Moon,
  PanelLeft,
  Search,
  Sun,
  X,
} from "lucide-react";
import {
  Suspense,
  lazy,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { CodeBrowserHandle } from "./CodeBrowser";
import {
  CommitCodeStream,
  type CommitCodeStreamHandle,
} from "./CommitCodeStream";
import harborSummaryData from "./data/harbor-summary.json";
import repositoryData from "./data/harness-repository.json";
import type { EvalComparison } from "./Harbor";
import { fuzzyScore } from "./fuzzy";
import { PierreWorkerProvider } from "./PierreWorkerProvider";
import { registerRepositorySyntax } from "./syntax";

const Harbor = lazy(() =>
  import("./Harbor").then((module) => ({ default: module.Harbor }))
);
const CodeBrowser = lazy(() =>
  import("./CodeBrowser").then((module) => ({ default: module.CodeBrowser }))
);

export type Theme = "light" | "dark";
type Scope = "all" | "eval" | "fix" | "docs" | "perf";
type ProposalState = "ready" | "submitting" | "payment-required";
type Surface =
  | "home"
  | "code"
  | "commits"
  | "requests"
  | "evals";

type RepositoryFile = {
  path: string;
  mode: string;
  objectId: string;
  size: number | null;
  contentUrl: string | null;
};

type SerializedTreeInput = {
  paths: string[];
  preparedPaths: Array<{
    basename: string;
    isDirectory: boolean;
    path: string;
    segments: string[];
  }>;
};

export type ChangedFile = {
  path: string;
  previousPath: string | null;
  status: string;
  additions: number | null;
  deletions: number | null;
};

export type HarnessCommit = {
  hash: string;
  shortHash: string;
  parents: string[];
  author: string;
  authoredAt: string;
  refs: string[];
  subject: string;
  body: string;
  files: ChangedFile[];
  stats: {
    files: number;
    additions: number;
    deletions: number;
  };
  patchUrl: string;
};

type RepositorySnapshot = {
  repository: {
    fullName: string;
    branch: string;
    head: string;
    totalCommits: number;
    dirty: boolean;
    dirtyCount: number;
  };
  generatedAt: string;
  tree: RepositoryFile[];
  treeInput: SerializedTreeInput;
  commits: HarnessCommit[];
};

const snapshot = repositoryData as RepositorySnapshot;
const evalComparison = (
  harborSummaryData as { comparison: EvalComparison | null }
).comparison;
registerRepositorySyntax([
  ...snapshot.tree.map((file) => file.path),
  ...snapshot.commits.flatMap((commit) =>
    commit.files.flatMap((file) =>
      file.previousPath ? [file.path, file.previousPath] : [file.path]
    )
  ),
]);
const scopes: Array<{ id: Scope; label: string }> = [
  { id: "all", label: "All commits" },
  { id: "eval", label: "Eval" },
  { id: "fix", label: "Fix" },
  { id: "docs", label: "Docs" },
  { id: "perf", label: "Perf" },
];

const dateFormatter = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

const relativeFormatter = new Intl.RelativeTimeFormat("en", {
  numeric: "auto",
});

function relativeDate(value: string) {
  const milliseconds = new Date(value).getTime() - Date.now();
  const hours = Math.round(milliseconds / 3_600_000);
  if (Math.abs(hours) < 24) return relativeFormatter.format(hours, "hour");
  const days = Math.round(milliseconds / 86_400_000);
  if (Math.abs(days) < 30) return relativeFormatter.format(days, "day");
  return dateFormatter.format(new Date(value));
}

function subjectScope(subject: string) {
  const prefix = subject.split(":", 1)[0].toLowerCase();
  return scopes.some(({ id }) => id === prefix) ? (prefix as Scope) : "other";
}

function scopeCount(scope: Scope) {
  if (scope === "all") return snapshot.commits.length;
  return snapshot.commits.filter(
    (commit) => subjectScope(commit.subject) === scope
  ).length;
}

function commitSearchScore(commit: HarnessCommit, query: string) {
  const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
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

function compactMetric(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}k`;
  return String(value);
}

function compactMinutes(milliseconds: number) {
  return `${(milliseconds / 60_000).toFixed(1)}m`;
}

function clockDuration(milliseconds: number | null) {
  if (milliseconds === null) return "—";
  const seconds = Math.round(milliseconds / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function reduction(value: number, baseline: number) {
  if (baseline <= 0) return null;
  return Math.round((1 - value / baseline) * 100);
}

const homeEvalMetrics = evalComparison
  ? [
      {
        label: "Input tokens",
        nanoValue: evalComparison.harness.tokens.input,
        codexValue: evalComparison.codex.tokens.input,
        nanoLabel: compactMetric(evalComparison.harness.tokens.input),
        codexLabel: compactMetric(evalComparison.codex.tokens.input),
      },
      {
        label: "Uncached input",
        nanoValue:
          evalComparison.harness.tokens.input -
          evalComparison.harness.tokens.cached,
        codexValue:
          evalComparison.codex.tokens.input -
          evalComparison.codex.tokens.cached,
        nanoLabel: compactMetric(
          evalComparison.harness.tokens.input -
            evalComparison.harness.tokens.cached
        ),
        codexLabel: compactMetric(
          evalComparison.codex.tokens.input - evalComparison.codex.tokens.cached
        ),
      },
      {
        label: "Model calls",
        nanoValue: evalComparison.harness.modelCalls,
        codexValue: evalComparison.codex.modelCalls,
        nanoLabel: String(evalComparison.harness.modelCalls),
        codexLabel: String(evalComparison.codex.modelCalls),
      },
      {
        label: "Agent time",
        nanoValue: evalComparison.harness.agentDurationMs,
        codexValue: evalComparison.codex.agentDurationMs,
        nanoLabel: compactMinutes(evalComparison.harness.agentDurationMs),
        codexLabel: compactMinutes(evalComparison.codex.agentDurationMs),
      },
      {
        label: "Wall time",
        nanoValue: evalComparison.harness.durationMs ?? 0,
        codexValue: evalComparison.codex.durationMs ?? 0,
        nanoLabel: clockDuration(evalComparison.harness.durationMs),
        codexLabel: clockDuration(evalComparison.codex.durationMs),
      },
    ]
  : [];

export function Xedoc() {
  const [theme, setTheme] = useState<Theme>(() => {
    const initialTheme = document.documentElement.dataset.theme;
    if (initialTheme === "dark" || initialTheme === "light")
      return initialTheme;
    const stored =
      localStorage.getItem("nanocodex-theme") ??
      localStorage.getItem("xedoc-theme");
    return stored === "dark" ? "dark" : "light";
  });
  const [surface, setSurface] = useState<Surface>(() => {
    const view = new URLSearchParams(window.location.search).get("view");
    if (
      view === "code" ||
      view === "commits" ||
      view === "requests" ||
      view === "evals"
    ) {
      return view;
    }
    return "home";
  });
  const [scope, setScope] = useState<Scope>("all");
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [selectedHash, setSelectedHash] = useState(snapshot.repository.head);
  const [proposalOpen, setProposalOpen] = useState(false);
  const [proposalState, setProposalState] = useState<ProposalState>("ready");
  const [proposalTitle, setProposalTitle] = useState("");
  const [commitRailOpen, setCommitRailOpen] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const headerCenterRef = useRef<HTMLDivElement>(null);
  const codeBrowserRef = useRef<CodeBrowserHandle>(null);
  const commitStreamRef = useRef<CommitCodeStreamHandle>(null);

  const selected =
    snapshot.commits.find((commit) => commit.hash === selectedHash) ??
    snapshot.commits[0];

  const filteredCommits = useMemo(() => {
    const matches = snapshot.commits
      .filter(
        (commit) => scope === "all" || subjectScope(commit.subject) === scope
      )
      .map((commit) => ({ commit, score: commitSearchScore(commit, query) }))
      .filter(
        (match): match is { commit: HarnessCommit; score: number } =>
          match.score !== null
      );
    if (query.trim()) matches.sort((left, right) => right.score - left.score);
    return matches.map((match) => match.commit);
  }, [query, scope]);

  const searchResults = useMemo(
    () =>
      snapshot.commits
        .map((commit) => ({ commit, score: commitSearchScore(commit, query) }))
        .filter(
          (match): match is { commit: HarnessCommit; score: number } =>
            match.score !== null
        )
        .sort((left, right) => right.score - left.score)
        .slice(0, 12)
        .map((match) => match.commit),
    [query]
  );

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document
      .querySelector('meta[name="theme-color"]')
      ?.setAttribute("content", theme === "dark" ? "#161616" : "#ffffff");
    localStorage.setItem("nanocodex-theme", theme);
  }, [theme]);

  useEffect(() => {
    const url = new URL(window.location.href);
    if (surface === "home") url.searchParams.delete("view");
    else url.searchParams.set("view", surface);
    window.history.replaceState(null, "", url);
  }, [surface]);

  useLayoutEffect(() => {
    const headerCenter = headerCenterRef.current;
    const activeButton =
      headerCenter?.querySelector<HTMLButtonElement>(".is-active");
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
    if (searchOpen)
      requestAnimationFrame(() => searchInputRef.current?.focus());
  }, [searchOpen]);

  useEffect(() => {
    if (surface !== "commits") return;
    requestAnimationFrame(() => commitStreamRef.current?.focus());
  }, [surface]);

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
      if (
        surface === "code" &&
        primaryModifier &&
        !event.altKey &&
        key === "f"
      ) {
        event.preventDefault();
        event.stopPropagation();
        codeBrowserRef.current?.openFileSearch();
        return;
      }

      if (event.key === "Escape") {
        setSearchOpen(false);
        setProposalOpen(false);
        setCommitRailOpen(false);
        codeBrowserRef.current?.closeSearches();
        return;
      }
      if (isTyping || primaryModifier || event.altKey) return;
      if (key === "f") {
        if (surface !== "commits") return;
        event.preventDefault();
        event.stopPropagation();
        setSearchOpen(true);
        return;
      }
      if (key === "m") {
        event.preventDefault();
        event.stopPropagation();
        setTheme((current) => (current === "light" ? "dark" : "light"));
        return;
      }
      if (key === "p") {
        event.preventDefault();
        event.stopPropagation();
        setProposalState("ready");
        setProposalOpen(true);
        return;
      }
      const nextSurface =
        key === "h"
          ? "home"
          : key === "t"
          ? "code"
          : key === "c"
          ? "commits"
          : key === "r"
          ? "requests"
          : key === "e"
          ? "evals"
          : null;
      if (nextSurface) {
        event.preventDefault();
        event.stopPropagation();
        target?.blur();
        setSurface(nextSurface);
      }
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [surface]);

  const selectCommit = (commit: HarnessCommit) => {
    const index = snapshot.commits.findIndex(
      (candidate) => candidate.hash === commit.hash
    );
    setSelectedHash(commit.hash);
    setSearchOpen(false);
    setCommitRailOpen(false);
    setQuery("");
    if (index >= 0) commitStreamRef.current?.scrollToCommit(index);
  };

  const submitProposal = async () => {
    setProposalState("submitting");
    try {
      await fetch("/api/proposals", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          repository: snapshot.repository.fullName,
          base: selected.hash,
          title: proposalTitle || "Untitled proposal",
        }),
      });
    } finally {
      setProposalState("payment-required");
    }
  };

  return (
    <PierreWorkerProvider>
      <div className={`site-shell surface-${surface}`}>
        <header className="site-header">
          <a
            className="wordmark"
            href="/"
            aria-label="nanocodex home"
            onClick={(event) => {
              event.preventDefault();
              setSurface("home");
            }}
          >
            nanocodex <span>[H]</span>
          </a>
          <div className="header-center" ref={headerCenterRef}>
            <nav className="surface-switch" aria-label="Repository surfaces">
              <button
                className={surface === "code" ? "is-active" : ""}
                type="button"
                onClick={() => setSurface("code")}
              >
                Code <span>[T]</span>
              </button>
              <button
                className={surface === "commits" ? "is-active" : ""}
                type="button"
                onClick={() => setSurface("commits")}
              >
                Commits <span>[C]</span>
              </button>
              <button
                className={surface === "requests" ? "is-active" : ""}
                type="button"
                onClick={() => setSurface("requests")}
              >
                Requests <span>[R]</span>
              </button>
              <button
                className={surface === "evals" ? "is-active" : ""}
                type="button"
                onClick={() => setSurface("evals")}
              >
                Evals <span>[E]</span>
              </button>
            </nav>
          </div>
          <nav className="header-actions" aria-label="Site actions">
            <button
              className="text-action"
              type="button"
              onClick={() =>
                setTheme((current) => (current === "light" ? "dark" : "light"))
              }
              aria-label={`Use ${theme === "light" ? "dark" : "light"} theme`}
            >
              {theme === "light" ? (
                <Moon aria-hidden="true" />
              ) : (
                <Sun aria-hidden="true" />
              )}
              <span>Theme</span>
              <span className="keycap">[M]</span>
            </button>
            <button
              className="button button--medium header-propose"
              type="button"
              onClick={() => {
                setProposalState("ready");
                setProposalOpen(true);
              }}
            >
              Propose <span>[P]</span>
            </button>
          </nav>
        </header>

        <main id="top">
          {surface === "home" ? (
            <section className="home-page" aria-labelledby="home-title">
                <article className="home-article">
                  <header className="home-intro">
                    <h1 id="home-title">
                      Nanocodex: Minimal Codex reimplementation for the
                      frontier.
                    </h1>
                    <p>
                      Codex is awesome. It is also almost 1M Rust LoC. nanocodex
                      is a small experiment in rebuilding only what the latest
                      models need.
                    </p>
                  </header>

                  {evalComparison ? (
                    <section
                      className="home-progress-section"
                      aria-labelledby="home-progress-title"
                    >
                      <h2 id="home-progress-title">Terminal Bench 2</h2>
                      <div className="home-progress">
                        <div className="home-progress-chart">
                          {homeEvalMetrics.map((metric) => {
                            const maximum = Math.max(
                              metric.nanoValue,
                              metric.codexValue,
                              1
                            );
                            const delta = reduction(
                              metric.nanoValue,
                              metric.codexValue
                            );
                            const nanoWidth = Math.max(
                              2,
                              (metric.nanoValue / maximum) * 100
                            );
                            const codexWidth = Math.max(
                              2,
                              (metric.codexValue / maximum) * 100
                            );
                            return (
                              <div
                                className="home-progress-metric"
                                key={metric.label}
                              >
                                <div className="home-progress-metric-heading">
                                  <span>{metric.label}</span>
                                  <small>
                                    <b>{metric.nanoLabel}</b> / {metric.codexLabel}
                                  </small>
                                </div>
                                <div className="home-progress-bars">
                                  <div className="home-progress-bar-line">
                                    <span
                                      className="is-nano"
                                      style={{ width: `${nanoWidth}%` }}
                                    />
                                    <strong
                                      style={{ left: `calc(${nanoWidth}% + 7px)` }}
                                    >
                                      {delta === null
                                        ? "—"
                                        : delta >= 0
                                        ? `${delta}% less`
                                        : `${Math.abs(delta)}% more`}
                                    </strong>
                                  </div>
                                  <div className="home-progress-bar-line">
                                    <span
                                      className="is-codex"
                                      style={{ width: `${codexWidth}%` }}
                                    />
                                  </div>
                                </div>
                              </div>
                            );
                          })}
                        </div>

                        <p className="home-progress-legend">
                          <span>
                            <i className="is-nano" /> NanoCodex
                          </span>
                          <span>
                            <i className="is-codex" /> Codex
                          </span>
                        </p>
                      </div>
                    </section>
                  ) : null}

                  <div className="home-copy">
                    <section>
                      <h2>Why</h2>
                      <ul>
                        <li>
                          If you squint, a coding agent is an elaborate WebSocket
                          client around a tool loop and a working tree. The loop
                          is the easy part.
                        </li>
                        <li>
                          The generic agent SDK layer feels leaky. Exact tool
                          shapes, cancellation, compaction, process cleanup & what
                          the model sees after every action are the actual
                          contract.
                        </li>
                        <li>
                          Stock harnesses should win long term as the model and
                          harness get co-designed. A 3p harness probably only
                          keeps up if it uses ~the same tools and lifecycle.
                        </li>
                      </ul>
                    </section>

                    <section>
                      <h2>How</h2>
                      <ul>
                        <li>
                          Copy the Codex tools + lifecycle the model was trained
                          against, without carrying its backwards compatibility
                          surface.
                        </li>
                        <li>
                          Build one vertical slice at a time, eval it on real
                          tasks & trace failures through the verifier + tool
                          trajectory.
                        </li>
                        <li>
                          Keep the runtime small enough that every behavior is
                          legible. No multi-agent yet; first make one agent
                          compatible, inspectable & good.
                        </li>
                      </ul>
                    </section>
                  </div>

                </article>
            </section>
          ) : surface === "code" ? (
            <Suspense fallback={null}>
              <CodeBrowser
                ref={codeBrowserRef}
                files={snapshot.tree}
                treeInput={snapshot.treeInput}
                branch={snapshot.repository.branch}
                head={snapshot.repository.head}
                theme={theme}
              />
            </Suspense>
          ) : surface === "commits" ? (
            <section
              className="commits-workspace"
              aria-label="Repository commits"
            >
                <button
                  className={
                    commitRailOpen
                      ? "workspace-backdrop is-visible"
                      : "workspace-backdrop"
                  }
                  type="button"
                  aria-label="Close commit list"
                  onClick={() => setCommitRailOpen(false)}
                />
                <aside
                  className={
                    commitRailOpen
                      ? "commit-sidebar is-mobile-open"
                      : "commit-sidebar"
                  }
                  aria-labelledby="history-title"
                >
                  <header className="commit-sidebar-header">
                    <div>
                      <strong id="history-title">Jump to commit</strong>
                      <span>
                        <GitBranch aria-hidden="true" />{" "}
                        {snapshot.repository.branch} · {snapshot.commits.length}
                      </span>
                    </div>
                    <nav
                      className="commit-sidebar-actions"
                      aria-label="Commit index actions"
                    >
                      <button
                        className="icon-button"
                        type="button"
                        onClick={() => setSearchOpen(true)}
                      >
                        <Search aria-hidden="true" />
                        <span className="sr-only">Find commits</span>
                        <kbd>F</kbd>
                      </button>
                      <button
                        className="mobile-drawer-close"
                        type="button"
                        onClick={() => setCommitRailOpen(false)}
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
                        {item.label} <span>{scopeCount(item.id)}</span>
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

                  <div className="commit-list">
                    {filteredCommits.length ? (
                      filteredCommits.map((commit) => {
                          const isSelected = commit.hash === selected.hash;
                          return (
                            <button
                              className={
                                isSelected
                                  ? "commit-row is-selected"
                                  : "commit-row"
                              }
                              type="button"
                              key={commit.hash}
                              aria-current={isSelected ? "location" : undefined}
                              onClick={() => selectCommit(commit)}
                            >
                              <span className="commit-meta">
                                <span>{commit.shortHash}</span>
                                <span>{relativeDate(commit.authoredAt)}</span>
                              </span>
                              <strong>{commit.subject}</strong>
                              <span className="commit-byline">
                                {commit.author} · {commit.stats.files} file
                                {commit.stats.files === 1 ? "" : "s"}
                              </span>
                              <ChevronRight
                                className="commit-chevron"
                                aria-hidden="true"
                              />
                            </button>
                          );
                        })
                    ) : (
                      <div className="empty-state">
                        <p>No commits match this filter.</p>
                        <button type="button" onClick={() => setQuery("")}>
                          Clear search
                        </button>
                      </div>
                    )}
                  </div>
                </aside>

                <section
                  className="commit-stream-pane"
                  aria-label="All commits"
                >
                  <header className="commit-stream-toolbar">
                    <div>
                      <button
                        className="mobile-tree-toggle"
                        type="button"
                        onClick={() => setCommitRailOpen(true)}
                        aria-label="Open commit index"
                      >
                        <PanelLeft aria-hidden="true" />
                      </button>
                      <strong>All commits</strong>
                      <span>{snapshot.commits.length}</span>
                    </div>
                    <span>Newest to oldest</span>
                  </header>
                  <CommitCodeStream
                    ref={commitStreamRef}
                    commits={snapshot.commits}
                    theme={theme}
                  />
                </section>
            </section>
          ) : surface === "requests" ? (
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
            <Suspense fallback={null}>
              <Harbor />
            </Suspense>
          )}
        </main>

        {searchOpen && surface === "commits" ? (
          <div
            className="overlay"
            role="presentation"
            onMouseDown={() => setSearchOpen(false)}
          >
            <section
              className="search-dialog"
              role="dialog"
              aria-modal="true"
              aria-label="Find commits"
              onMouseDown={(event) => event.stopPropagation()}
            >
              <div className="search-field">
                <Search aria-hidden="true" />
                <input
                  ref={searchInputRef}
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Search hashes, messages, authors, and paths"
                />
                <button
                  type="button"
                  onClick={() => setSearchOpen(false)}
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
                ) : (
                  <p className="search-empty">No commits found.</p>
                )}
              </div>
              <footer className="search-footer">
                <span>{searchResults.length} results</span>
                <span>Esc to close</span>
              </footer>
            </section>
          </div>
        ) : null}

        {proposalOpen ? (
          <div
            className="overlay"
            role="presentation"
            onMouseDown={() => setProposalOpen(false)}
          >
            <section
              className="proposal-dialog"
              role="dialog"
              aria-modal="true"
              aria-labelledby="proposal-title"
              onMouseDown={(event) => event.stopPropagation()}
            >
              <button
                className="dialog-close"
                type="button"
                onClick={() => setProposalOpen(false)}
              >
                <X aria-hidden="true" /> <span className="sr-only">Close</span>
              </button>
              <p className="eyebrow">MPP proposal gate · testnet preview</p>
              <h2 id="proposal-title">Propose a change</h2>
              {proposalState === "payment-required" ? (
                <div className="payment-required">
                  <div className="payment-mark">402</div>
                  <h3>Payment challenge ready</h3>
                  <p>
                    The Worker returned the preview MPP challenge. No funds
                    moved; a live recipient and settlement policy still need to
                    be configured.
                  </p>
                  <button
                    className="button button--high"
                    type="button"
                    onClick={() => setProposalOpen(false)}
                  >
                    Done
                  </button>
                </div>
              ) : (
                <>
                  <p className="proposal-intro">
                    Submit a patch against <strong>{selected.shortHash}</strong>
                    . The $0.20 proposal fee is a rate limit, not access to the
                    repository.
                  </p>
                  <label>
                    Proposal title
                    <input
                      value={proposalTitle}
                      onChange={(event) => setProposalTitle(event.target.value)}
                      placeholder="What should change?"
                    />
                  </label>
                  <div className="proposal-summary">
                    <div>
                      <span>Repository</span>
                      <strong>nanocodex</strong>
                    </div>
                    <div>
                      <span>Base</span>
                      <strong>{selected.shortHash}</strong>
                    </div>
                    <div>
                      <span>Preview fee</span>
                      <strong>$0.20</strong>
                    </div>
                  </div>
                  <button
                    className="button button--high proposal-submit"
                    type="button"
                    disabled={proposalState === "submitting"}
                    onClick={submitProposal}
                  >
                    {proposalState === "submitting"
                      ? "Requesting challenge…"
                      : "Continue to payment"}
                    <ArrowUpRight aria-hidden="true" />
                  </button>
                </>
              )}
            </section>
          </div>
        ) : null}
      </div>
    </PierreWorkerProvider>
  );
}
