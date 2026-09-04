import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  ArrowUpRight,
  ChevronDown,
  Cloud,
  Code2,
  Cpu,
  Folder,
  Layers,
  LoaderCircle,
  Monitor,
  PanelLeft,
  PanelTop,
  Plus,
  Search,
  Settings2,
  Square,
  SquarePen,
  Terminal,
  X,
  Zap,
} from "lucide-react";
import type {
  DesktopState,
  HandConfig,
  Layout,
  Settings,
  Tab,
  ThreadSnapshot,
} from "../shared/types";
import { discoveredHands, timeline } from "./timeline";
import { SignIn } from "./SignIn";
import {
  displayPrompt,
  HandDialog,
  HandsPage,
  Message,
  Modal,
  RemoteDialog,
  SettingsPage,
} from "./components";

const bridge = window.nanocodex;
const defaultSettings: Settings = {
  model: "gpt-5.6-sol",
  thinking: "high",
  reasoning_mode: "standard",
  fast_mode: false,
};
const initial: DesktopState = {
  connected: false,
  baseUrl: "https://nanocodex.gakonst.workers.dev",
  threads: [],
  hands: [],
  defaults: {},
  platform: "darwin",
  version: "0.1.0",
};
const newTab = (): Tab => ({
  id: crypto.randomUUID(),
  draft: "",
  target: "",
  folder: "",
});
const newLayout = (): Layout => {
  const tab = newTab();
  return {
    tabs: [tab],
    activeTabId: tab.id,
    tabPosition: "left",
    theme: "system",
  };
};
function restoreLayout(value: DesktopState["layout"]): Layout {
  if (!value || !Array.isArray(value.tabs)) return newLayout();
  const ids = new Set<string>();
  const tabs = value.tabs.slice(0, 100).flatMap((item) => {
    if (!item || typeof item.id !== "string" || !item.id || ids.has(item.id))
      return [];
    ids.add(item.id);
    return [
      {
        id: item.id,
        draft: typeof item.draft === "string" ? item.draft : "",
        target: typeof item.target === "string" ? item.target : "",
        folder: typeof item.folder === "string" ? item.folder : "",
        ...(typeof item.threadId === "string" && item.threadId
          ? { threadId: item.threadId }
          : {}),
        ...(typeof item.title === "string" ? { title: item.title } : {}),
        ...(typeof item.seenCursor === "string"
          ? { seenCursor: item.seenCursor }
          : {}),
      },
    ];
  });
  if (!tabs.length) return newLayout();
  return {
    tabs,
    activeTabId: ids.has(value.activeTabId) ? value.activeTabId : tabs[0].id,
    tabPosition: value.tabPosition === "top" ? "top" : "left",
    theme:
      value.theme === "dark" || value.theme === "light"
        ? value.theme
        : "system",
  };
}
type Page = "thread" | "hands" | "settings";
type Pending = {
  id: string;
  text: string;
  agentId?: string;
  status: string;
  failed?: boolean;
};

export function App() {
  const [state, setState] = useState(initial);
  const [layout, setLayout] = useState(newLayout);
  const [ready, setReady] = useState(false);
  const [signInRequested, setSignInRequested] = useState(false);
  const [page, setPage] = useState<Page>("thread");
  const [threads, setThreads] = useState<Record<string, ThreadSnapshot>>({});
  const [settings, setSettings] = useState(defaultSettings);
  const [pending, setPending] = useState<Record<string, Pending>>({});
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [sidebar, setSidebar] = useState(true);
  const [handModal, setHandModal] = useState<Partial<HandConfig> | null>(null);
  const [remoteModal, setRemoteModal] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const [renaming, setRenaming] = useState<string>();
  const [atBottom, setAtBottom] = useState(true);
  const composer = useRef<HTMLTextAreaElement>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const opened = useRef(new Set<string>());
  const submitting = useRef(new Set<string>());
  const closed = useRef<Tab[]>([]);
  const commands = useRef<(name: string) => void>(() => {});
  const previousConnected = useRef(false);
  const layoutScope = useRef<string | undefined>(undefined);
  const tab =
    layout.tabs.find((item) => item.id === layout.activeTabId) ??
    layout.tabs[0];
  const selectedId = tab?.threadId;
  const thread = selectedId ? threads[selectedId] : undefined;
  const entries = useMemo(
    () => timeline(thread?.events ?? []),
    [thread?.events]
  );
  const remoteHands = useMemo(() => discoveredHands(entries), [entries]);
  const running = !!thread?.activeTurns.length;
  const modelLocked =
    (thread?.acceptedTurns ??
      thread?.events.filter((event) => event.data.type === "turn_accepted")
        .length ??
      0) > 0;
  const connecting = !state.connected && state.hasCredentials && !state.error;
  const visibleError =
    error || state.error || (page === "thread" ? thread?.error : undefined);
  const sending = pending[tab?.id];
  const busy = !!sending && !sending.failed;
  const connectedHands = state.hands.filter(
    (hand) => hand.status === "connected"
  );
  const title = useCallback(
    (item: Tab) =>
      item.title ||
      state.threads.find((thread) => thread.id === item.threadId)?.title ||
      "New tab",
    [state.threads]
  );
  const report = useCallback(
    (cause: unknown) =>
      setError(
        (cause instanceof Error ? cause.message : String(cause)).replace(
          /^Error invoking remote method '[^']+': Error: /,
          ""
        )
      ),
    []
  );
  const patchTab = useCallback(
    (id: string, patch: Partial<Tab>) =>
      setLayout((value) => ({
        ...value,
        tabs: value.tabs.map((item) =>
          item.id === id ? { ...item, ...patch } : item
        ),
      })),
    []
  );
  function adoptAccount(value: DesktopState) {
    layoutScope.current = value.accountScope;
    setState(value);
    setLayout(restoreLayout(value.layout));
    setThreads({});
    setPending({});
    opened.current.clear();
    setPage("thread");
    setError("");
    setSignInRequested(false);
  }

  useEffect(() => {
    if (!bridge) {
      setError("Open the Nanocodex desktop app to get started.");
      return;
    }
    void bridge
      .state()
      .then((value) => {
        layoutScope.current = value.accountScope;
        setState(value);
        let restored = restoreLayout(value.layout);
        const hash = location.hash.slice(1);
        if (hash) {
          const existing = restored.tabs.find((item) => item.threadId === hash);
          if (existing) restored = { ...restored, activeTabId: existing.id };
          else {
            const fresh = { ...newTab(), threadId: hash };
            restored = {
              ...restored,
              tabs: [...restored.tabs, fresh],
              activeTabId: fresh.id,
            };
          }
        }
        setLayout(restored);
        setReady(true);
      })
      .catch(report);
    return bridge.onEvent((event) => {
      if (event.type === "state") setState(event.state);
      if (event.type === "thread")
        setThreads((value) => ({ ...value, [event.thread.id]: event.thread }));
      if (event.type === "command") commands.current(event.command);
    });
  }, [report]);

  useEffect(() => {
    if (!ready) return;
    const savedLayout = { ...layout, accountScope: layoutScope.current };
    const timer = setTimeout(
      () => void bridge.saveLayout(savedLayout).catch(report),
      180
    );
    const save = () => {
      void bridge.saveLayout(savedLayout);
    };
    window.addEventListener("beforeunload", save);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("beforeunload", save);
    };
  }, [layout, ready, report]);
  useEffect(() => {
    document.documentElement.dataset.theme = layout.theme;
  }, [layout.theme]);
  useEffect(() => {
    if (previousConnected.current && !state.connected && !state.error) {
      setLayout(newLayout());
      setThreads({});
      setPending({});
      opened.current.clear();
    }
    previousConnected.current = state.connected;
  }, [state.connected, state.error]);
  const threadIds = layout.tabs
    .map((item) => item.threadId)
    .filter(Boolean)
    .join(",");
  useEffect(() => {
    if (!state.connected || !ready) return;
    const ids = new Set(threadIds.split(",").filter(Boolean));
    for (const id of opened.current)
      if (!ids.has(id)) {
        void bridge.closeThread(id);
        opened.current.delete(id);
      }
    for (const id of ids)
      if (!opened.current.has(id)) {
        opened.current.add(id);
        void bridge
          .openThread(id)
          .then((value) =>
            setThreads((current) => ({ ...current, [id]: value }))
          )
          .catch((cause) => {
            opened.current.delete(id);
            report(cause);
          });
      }
  }, [threadIds, state.connected, ready, report]);
  useEffect(() => {
    location.hash = selectedId ?? "";
    setError("");
    setModelOpen(false);
    follow.current = true;
    setAtBottom(true);
    requestAnimationFrame(() => {
      composer.current?.focus();
      if (viewport.current)
        viewport.current.scrollTop = viewport.current.scrollHeight;
    });
  }, [layout.activeTabId, selectedId]);
  useEffect(() => {
    setSettings(thread?.settings ?? defaultSettings);
  }, [thread?.settings, selectedId]);
  useEffect(() => {
    if (
      thread &&
      !running &&
      thread.events.length &&
      tab.seenCursor !== thread.events.at(-1)?.cursor
    )
      patchTab(tab.id, { seenCursor: thread.events.at(-1)?.cursor });
    if (follow.current && viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [thread, running, tab?.id, tab?.seenCursor, patchTab]);
  useEffect(() => {
    setPending((value) => {
      let next = value;
      for (const [id, item] of Object.entries(value))
        if (
          item.agentId &&
          threads[item.agentId]?.events.some(
            (event) =>
              event.data.type === "turn_accepted" && event.data.id === item.id
          )
        ) {
          if (next === value) next = { ...value };
          delete next[id];
        }
      return next;
    });
  }, [threads]);

  function selectTab(id: string) {
    setLayout((value) => ({ ...value, activeTabId: id }));
    setPage("thread");
  }
  function createTab() {
    if (layout.tabs.length >= 100) {
      report(
        "Close a tab before opening another. Your conversations stay in Recent threads."
      );
      return;
    }
    const fresh = newTab();
    setLayout((value) => ({
      ...value,
      tabs: [...value.tabs, fresh],
      activeTabId: fresh.id,
    }));
    setPage("thread");
  }
  function closeTab(id: string) {
    const removed = layout.tabs.find((item) => item.id === id);
    if (removed) closed.current = [...closed.current.slice(-19), removed];
    setLayout((value) => {
      const index = value.tabs.findIndex((item) => item.id === id);
      const remaining = value.tabs.filter((item) => item.id !== id);
      if (!remaining.length) remaining.push(newTab());
      return {
        ...value,
        tabs: remaining,
        activeTabId:
          value.activeTabId === id
            ? remaining[Math.max(0, index - 1)]?.id ?? remaining[0].id
            : value.activeTabId,
      };
    });
  }
  function reopenTab() {
    const item = closed.current.pop();
    if (item) {
      setLayout((value) => ({
        ...value,
        tabs: [...value.tabs, item],
        activeTabId: item.id,
      }));
      setPage("thread");
    }
  }
  function openRecent(id: string) {
    const existing = layout.tabs.find((item) => item.threadId === id);
    if (existing) selectTab(existing.id);
    else {
      const fresh = { ...newTab(), threadId: id };
      setLayout((value) => ({
        ...value,
        tabs: [...value.tabs, fresh],
        activeTabId: fresh.id,
      }));
      setPage("thread");
    }
    setSearching(false);
    setQuery("");
  }
  async function action(operation: () => Promise<unknown>) {
    setError("");
    try {
      await operation();
    } catch (cause) {
      report(cause);
    }
  }
  async function reconnect() {
    await action(async () => {
      const next = await bridge.refresh();
      setState(next);
      if (!next.connected) return;
      if (selectedId) {
        await bridge.closeThread(selectedId);
        const snapshot = await bridge.openThread(selectedId);
        setThreads((value) => ({ ...value, [selectedId]: snapshot }));
      }
    });
  }
  async function chooseFolder() {
    const path = await bridge.choosePath("directory");
    if (path) patchTab(tab.id, { folder: path, target: "" });
  }
  commands.current = (name) => {
    if (name === "new-tab") createTab();
    if (name === "close-tab") closeTab(tab.id);
    if (name === "reopen-tab") reopenTab();
    if (name === "choose-folder") void action(chooseFolder);
    if (name === "settings") setPage("settings");
  };
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setHandModal(null);
        setRemoteModal(false);
        setModelOpen(false);
        setSearching(false);
      }
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      if (
        ["t", "n", "w", "k", ",", "o", "tab"].includes(key) ||
        /^[1-9]$/.test(key)
      )
        event.preventDefault();
      if (key === "t" && event.shiftKey) reopenTab();
      else if (key === "t" || key === "n") createTab();
      if (key === "w") closeTab(tab.id);
      if (key === "k") setSearching(true);
      if (key === ",") setPage("settings");
      if (key === "o") void action(chooseFolder);
      if (/^[1-9]$/.test(key)) {
        const item =
          key === "9" ? layout.tabs.at(-1) : layout.tabs[Number(key) - 1];
        if (item) selectTab(item.id);
      }
      if (key === "tab") {
        const index = layout.tabs.findIndex((item) => item.id === tab.id);
        selectTab(
          layout.tabs[
            (index + (event.shiftKey ? -1 : 1) + layout.tabs.length) %
              layout.tabs.length
          ].id
        );
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  async function send(
    text = tab.draft,
    intent: "prompt" | "steer" = "prompt",
    targetOverride?: string
  ) {
    if (!text.trim() || submitting.current.has(tab.id)) return;
    if (!state.connected) {
      setPage("settings");
      return;
    }
    const tabId = tab.id;
    let target = targetOverride ?? tab.target;
    let requestId: string = crypto.randomUUID();
    submitting.current.add(tabId);
    setPending((value) => ({
      ...value,
      [tabId]: {
        id: requestId,
        text: text.trim(),
        agentId: selectedId,
        status: "Sending…",
      },
    }));
    setError("");
    setPage("thread");
    follow.current = true;
    try {
      const hand = state.hands.find((hand) => hand.id === target);
      if (hand && hand.status !== "connected") {
        setPending((value) => ({
          ...value,
          [tabId]: { ...value[tabId], status: "Starting your Hand…" },
        }));
        const next = await bridge.startHand(hand.id);
        setState(next);
        if (
          next.hands.find((item) => item.id === hand.id)?.status !== "connected"
        )
          throw new Error(
            "Your Hand is still starting. Send again when it is ready."
          );
      }
      let id = selectedId;
      if (!id) {
        const created = await bridge.createThread(settings);
        id = created.id;
        patchTab(tabId, { threadId: id });
      }
      if (tab.folder && !target && targetOverride === undefined) {
        setPending((value) => ({
          ...value,
          [tabId]: { ...value[tabId], status: "Preparing your folder…" },
        }));
        const prepared = await bridge.prepareFolderHand({
          agentId: id,
          workspace: tab.folder,
        });
        target = prepared.id;
        patchTab(tabId, { target });
      }
      const context = target
        ? `\n\n[Execution context: Use only the Hand identified as ${target}. Call accountInfo to resolve its exact current mount path. If unavailable, report that and do not substitute another Hand.]`
        : "";
      const folder =
        tab.folder && !target && targetOverride === undefined
          ? `\n\n[Workspace folder: ${tab.folder}. Use a connected Hand with access to this folder.]`
          : "";
      const input = `${text.trim()}${context}${folder}`;
      if (sending?.failed && sending.text === input) requestId = sending.id;
      setPending((value) => ({
        ...value,
        [tabId]: {
          id: requestId,
          text: input,
          agentId: id,
          status: "Sending…",
        },
      }));
      if (intent === "steer" && thread?.activeTurns[0]) {
        await bridge.steer({
          agentId: id,
          turnId: thread.activeTurns[0],
          input,
        });
        setPending((value) => {
          const next = { ...value };
          delete next[tabId];
          return next;
        });
      } else await bridge.prompt({ agentId: id, input, requestId });
      setLayout((value) => ({
        ...value,
        tabs: value.tabs.map((item) =>
          item.id === tabId && item.draft === text
            ? { ...item, draft: "" }
            : item
        ),
      }));
      if (composer.current && composer.current.value === text)
        composer.current.style.height = "auto";
    } catch (cause) {
      setPending((value) => ({
        ...value,
        [tabId]: {
          ...value[tabId],
          failed: true,
          status: "Not sent · retry safely",
        },
      }));
      report(cause);
    } finally {
      submitting.current.delete(tabId);
    }
  }
  async function changeSettings(next: Settings) {
    const compatible =
      next.model === "gpt-6-astra"
        ? {
            ...next,
            thinking: next.thinking === "none" ? "high" : next.thinking,
            reasoning_mode: "standard" as const,
          }
        : next;
    if (selectedId)
      await action(async () =>
        setSettings(
          await bridge.settings({ agentId: selectedId, settings: compatible })
        )
      );
    else setSettings(compatible);
  }
  function tabState(item: Tab) {
    const snapshot = item.threadId ? threads[item.threadId] : undefined;
    if (
      (pending[item.id] && !pending[item.id].failed) ||
      snapshot?.activeTurns.length
    )
      return "working";
    if (snapshot?.error || snapshot?.events.at(-1)?.data.type === "turn_failed")
      return "attention";
    if (
      snapshot?.events.length &&
      snapshot.events.at(-1)?.cursor !== item.seenCursor &&
      item.id !== tab.id
    )
      return "done";
    return "idle";
  }
  const tabStrip = (
    <div
      className={`tabs tabs-${layout.tabPosition}`}
      role="tablist"
      aria-label="Open tabs"
      aria-orientation={
        layout.tabPosition === "left" ? "vertical" : "horizontal"
      }
    >
      {layout.tabs.map((item, index) => (
        <div
          key={item.id}
          className={`tab-row ${
            item.id === tab.id && page === "thread" ? "selected" : ""
          }`}
          draggable={!renaming}
          onDragStart={(event) =>
            event.dataTransfer.setData("text/nanocodex-tab", item.id)
          }
          onDragOver={(event) => event.preventDefault()}
          onDrop={(event) => {
            event.preventDefault();
            const from = event.dataTransfer.getData("text/nanocodex-tab");
            setLayout((value) => {
              const moved = value.tabs.find((tab) => tab.id === from);
              if (!moved) return value;
              const tabs = value.tabs.filter((tab) => tab.id !== from);
              tabs.splice(index, 0, moved);
              return { ...value, tabs };
            });
          }}
        >
          <button
            role="tab"
            aria-selected={item.id === tab.id && page === "thread"}
            title={`${title(item)} · ${tabState(item)} · ⌘${index + 1}`}
            onClick={() => selectTab(item.id)}
            onDoubleClick={() => setRenaming(item.id)}
            onKeyDown={(event) => {
              const direction =
                event.key === "ArrowRight" || event.key === "ArrowDown"
                  ? 1
                  : event.key === "ArrowLeft" || event.key === "ArrowUp"
                  ? -1
                  : 0;
              if (direction) {
                event.preventDefault();
                selectTab(
                  layout.tabs[
                    (index + direction + layout.tabs.length) %
                      layout.tabs.length
                  ].id
                );
              }
            }}
          >
            <i
              className={`tab-status ${tabState(item)}`}
              aria-label={tabState(item)}
            />
            {renaming === item.id ? (
              <input
                autoFocus
                aria-label="Tab name"
                defaultValue={item.title || title(item)}
                onClick={(e) => e.stopPropagation()}
                onBlur={(e) => {
                  patchTab(item.id, {
                    title: e.target.value.trim() || undefined,
                  });
                  setRenaming(undefined);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                  e.stopPropagation();
                }}
              />
            ) : (
              <span>{title(item)}</span>
            )}
          </button>
          <button
            className="tab-close"
            title="Close tab · agent keeps running"
            aria-label={`Close ${title(item)}`}
            onClick={() => closeTab(item.id)}
          >
            <X size={12} />
          </button>
        </div>
      ))}
      <button
        className="new-tab-button"
        aria-label="New tab"
        title="New tab · ⌘T"
        onClick={createTab}
      >
        <Plus size={15} />
        {layout.tabPosition === "left" && <span>New tab</span>}
      </button>
    </div>
  );

  if (signInRequested || (ready && !state.connected && !state.hasCredentials)) {
    return (
      <div className="onboarding-shell">
        <div className="onboarding-titlebar">Nanocodex</div>
        <main className="onboarding-content">
          <SignIn
            baseUrl={state.baseUrl}
            onSignedIn={adoptAccount}
            onCancel={
              signInRequested ? () => setSignInRequested(false) : undefined
            }
          />
        </main>
      </div>
    );
  }
  return (
    <div className={`desktop ${sidebar ? "" : "sidebar-hidden"}`}>
      {sidebar && (
        <aside className="sidebar">
          <div className="sidebar-top">
            <span className="window-controls-space" />
            <button
              className="icon-button"
              aria-label="Hide sidebar"
              onClick={() => setSidebar(false)}
            >
              <PanelLeft size={17} />
            </button>
          </div>
          <nav className="primary-nav" aria-label="Main navigation">
            <button onClick={createTab}>
              <SquarePen />
              New tab<span className="shortcut">⌘ T</span>
            </button>
            <button onClick={() => setSearching(true)}>
              <Search />
              Search threads<span className="shortcut">⌘ K</span>
            </button>
            <button
              onClick={() => setPage("hands")}
              className={page === "hands" ? "selected" : ""}
            >
              <Monitor />
              Hands
              {connectedHands.length > 0 && (
                <span className="nav-count">
                  {connectedHands.length}
                  <i className="status-dot" />
                </span>
              )}
            </button>
            <button onClick={() => void action(() => bridge.openAccount())}>
              <Layers />
              Connections
              <ArrowUpRight className="nav-end" size={13} />
            </button>
          </nav>
          <div className="sidebar-scroll">
            <div className="section-label">
              <span>
                {layout.tabPosition === "left" ? "Tabs" : "Your workspace"}
              </span>
              <button
                title={
                  layout.tabPosition === "left"
                    ? "Move tabs to top"
                    : "Move tabs to sidebar"
                }
                aria-label={
                  layout.tabPosition === "left"
                    ? "Move tabs to top"
                    : "Move tabs to sidebar"
                }
                onClick={() =>
                  setLayout((value) => ({
                    ...value,
                    tabPosition: value.tabPosition === "left" ? "top" : "left",
                  }))
                }
              >
                {layout.tabPosition === "left" ? (
                  <PanelTop size={15} />
                ) : (
                  <PanelLeft size={15} />
                )}
              </button>
            </div>
            {layout.tabPosition === "left" && tabStrip}
            <div className="section-label threads-label">
              <span>Recent threads</span>
              <button
                aria-label="Search all threads"
                onClick={() => setSearching(true)}
              >
                <Search size={13} />
              </button>
            </div>
            <div className="thread-list">
              {state.threads
                .filter(
                  (item) => !layout.tabs.some((tab) => tab.threadId === item.id)
                )
                .slice(0, 12)
                .map((item) => (
                  <button
                    key={item.id}
                    className="thread-link"
                    onClick={() => openRecent(item.id)}
                  >
                    <span>{item.title}</span>
                    <time>{relativeTime(item.updatedAt)}</time>
                  </button>
                ))}
            </div>
            {!state.threads.length && (
              <p className="sidebar-empty">
                Start a conversation.
                <br />
                Your work stays here.
              </p>
            )}
          </div>
          <div className="sidebar-footer">
            <button onClick={() => setPage("settings")}>
              <span className="avatar">N</span>
              <span>
                Nanocodex
                <span className="account-caption">
                  {state.connected
                    ? "Connected account"
                    : "Connect your account"}
                </span>
              </span>
              <Settings2 size={16} />
            </button>
          </div>
        </aside>
      )}
      <div className="main-column">
        <div className="titlebar">
          {!sidebar && (
            <button
              className="icon-button"
              aria-label="Show sidebar"
              onClick={() => setSidebar(true)}
            >
              <PanelLeft size={17} />
            </button>
          )}
          <span className="brand-wordmark">Nanocodex</span>
          <span className="titlebar-right">
            <i
              className={`status-dot ${
                state.connected ? "" : connecting ? "connecting" : "offline"
              }`}
            />
            {state.connected
              ? "Managed agents"
              : connecting
              ? "Connecting…"
              : "Sign in"}
          </span>
        </div>
        {layout.tabPosition === "top" && tabStrip}
        <main className="main-surface">
          <header className="content-header">
            <div className="header-title">
              <span>
                {page === "hands"
                  ? "Hands"
                  : page === "settings"
                  ? "Settings"
                  : title(tab)}
              </span>
              {page === "thread" && tab.folder && (
                <span className="header-project">
                  <Folder size={13} />
                  {basename(tab.folder)}
                </span>
              )}
            </div>
            <div className="header-actions">
              {page === "thread" && (
                <button
                  className="small-button"
                  onClick={() => setPage("hands")}
                >
                  <Monitor size={14} />
                  {connectedHands.length
                    ? `${connectedHands.length} Hand${
                        connectedHands.length === 1 ? "" : "s"
                      }`
                    : "Compute"}
                </button>
              )}
              <button
                className="icon-button"
                aria-label="New tab"
                onClick={createTab}
              >
                <SquarePen size={16} />
              </button>
            </div>
          </header>
          {visibleError && (
            <div className="error-banner" role="alert">
              <span>{visibleError}</span>
              {(state.error || thread?.error) && state.hasCredentials && (
                <button
                  className="retry-connection"
                  onClick={() => void reconnect()}
                >
                  Retry connection
                </button>
              )}
              {state.error && !state.connected && state.hasCredentials && (
                <button
                  className="retry-connection"
                  onClick={() => setSignInRequested(true)}
                >
                  Sign in again
                </button>
              )}
              <button
                aria-label="Dismiss error"
                onClick={() => {
                  setError("");
                  setState((value) => ({ ...value, error: undefined }));
                  if (selectedId && thread)
                    setThreads((value) => ({
                      ...value,
                      [selectedId]: { ...thread, error: undefined },
                    }));
                }}
              >
                <X size={15} />
              </button>
            </div>
          )}
          {page === "settings" ? (
            <SettingsPage
              state={state}
              layout={layout}
              onLayout={setLayout}
              onState={adoptAccount}
              report={report}
            />
          ) : page === "hands" ? (
            <HandsPage
              hands={state.hands}
              remoteHands={remoteHands}
              connected={state.connected}
              onAdd={() =>
                setHandModal({
                  ...state.defaults,
                  ...(tab.folder ? { workspace: tab.folder } : {}),
                })
              }
              onEdit={setHandModal}
              onRemote={() => setRemoteModal(true)}
              onStart={(id) =>
                void action(async () => setState(await bridge.startHand(id)))
              }
              onStop={(id) =>
                void action(async () => setState(await bridge.stopHand(id)))
              }
              onRemove={(id) =>
                void action(async () => setState(await bridge.removeHand(id)))
              }
              onDiscover={() =>
                void send(
                  "Call accountInfo and show the connected Hands, their exact mount paths, and capabilities. Do not run commands or provision compute.",
                  "prompt",
                  ""
                )
              }
              onCloud={() => {
                patchTab(tab.id, { target: "", folder: "" });
                void send(
                  "Provision a Cloudflare compute Hand using mount with provider cloudflare and name desktop-workspace. Then call accountInfo and report its exact mount path when ready.",
                  "prompt",
                  ""
                );
              }}
              onUse={(id) => {
                const hand = state.hands.find((item) => item.id === id);
                const selection = {
                  target: id,
                  folder: hand?.kind === "local" ? hand.workspace : "",
                };
                if (hand?.agentId) {
                  setLayout((value) => {
                    const owner = value.tabs.find(
                      (item) => item.threadId === hand.agentId
                    );
                    const selected = owner
                      ? { ...owner, ...selection }
                      : { ...newTab(), threadId: hand.agentId, ...selection };
                    return {
                      ...value,
                      tabs: owner
                        ? value.tabs.map((item) =>
                            item.id === owner.id ? selected : item
                          )
                        : [...value.tabs, selected],
                      activeTabId: selected.id,
                    };
                  });
                } else patchTab(tab.id, selection);
                setPage("thread");
              }}
            />
          ) : (
            <div
              className={`thread-view ${
                !selectedId && !sending ? "is-new" : ""
              }`}
            >
              <div
                className="conversation-scroll"
                ref={viewport}
                onScroll={() => {
                  const element = viewport.current!;
                  const bottom =
                    element.scrollHeight -
                      element.scrollTop -
                      element.clientHeight <
                    100;
                  follow.current = bottom;
                  setAtBottom(bottom);
                }}
              >
                {!selectedId && !sending ? (
                  <div className="welcome">
                    <div className="nano-glyph">
                      <Terminal size={35} strokeWidth={1.5} />
                    </div>
                    <h1>Let’s build</h1>
                    <span className="welcome-project">Something great</span>
                    <button
                      className="welcome-folder"
                      title="Choose a folder for this thread. Its Hand starts when you send."
                      onClick={() => void action(chooseFolder)}
                    >
                      <Folder size={14} />
                      {tab.folder ? basename(tab.folder) : "Choose a folder"}
                      <ChevronDown size={12} />
                    </button>
                    {tab.folder && (
                      <p className="folder-hint">
                        This thread can use this folder when you send.
                      </p>
                    )}
                  </div>
                ) : (
                  <div className="transcript">
                    {selectedId && !thread && !sending && (
                      <div className="loading-line">
                        <LoaderCircle className="spin" size={15} />
                        Opening thread…
                      </div>
                    )}
                    {thread?.hasMore && (
                      <button
                        className="load-older"
                        onClick={() =>
                          void action(async () => {
                            const older = await bridge.older(selectedId!);
                            setThreads((value) => ({
                              ...value,
                              [older.id]: older,
                            }));
                          })
                        }
                      >
                        Load earlier messages
                      </button>
                    )}
                    {entries.map((entry) => (
                      <Message key={entry.id} entry={entry} />
                    ))}
                    {sending &&
                      !entries.some(
                        (e) => e.kind === "user" && e.turnId === sending.id
                      ) && (
                        <div className="message user-message pending">
                          <p>{displayPrompt(sending.text)}</p>
                          <small>{sending.status}</small>
                          {sending.failed && (
                            <button
                              className="text-button"
                              onClick={() => void send()}
                            >
                              Retry
                            </button>
                          )}
                        </div>
                      )}
                    {running && (
                      <div className="working-line">
                        <span className="working-dot" />
                        Working
                      </div>
                    )}
                  </div>
                )}
              </div>
              {!atBottom && (
                <button
                  className="scroll-bottom"
                  aria-label="Scroll to latest"
                  onClick={() => {
                    follow.current = true;
                    viewport.current?.scrollTo({
                      top: viewport.current.scrollHeight,
                      behavior: "smooth",
                    });
                  }}
                >
                  <ArrowDown size={16} />
                </button>
              )}
              <div className="composer-region">
                {!selectedId && !sending && (
                  <div className="suggestions">
                    <button
                      onClick={() => {
                        patchTab(tab.id, {
                          draft:
                            "Explore this codebase and explain its architecture.",
                        });
                        composer.current?.focus();
                      }}
                    >
                      <Code2 size={16} />
                      Explore code
                    </button>
                    <button
                      onClick={() => {
                        patchTab(tab.id, { draft: "Help me build " });
                        composer.current?.focus();
                      }}
                    >
                      <Zap size={16} />
                      Build something
                    </button>
                    <button onClick={() => setPage("hands")}>
                      <Cpu size={16} />
                      Provide compute
                    </button>
                  </div>
                )}
                <form
                  className="composer"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void send();
                  }}
                >
                  {tab.folder && (
                    <div className="folder-chip">
                      <Folder size={12} />
                      {basename(tab.folder)}
                      <button
                        type="button"
                        aria-label="Remove folder"
                        onClick={() => patchTab(tab.id, { folder: "" })}
                      >
                        <X size={12} />
                      </button>
                    </div>
                  )}
                  <textarea
                    key={tab.id}
                    ref={composer}
                    aria-label="Message Nanocodex"
                    placeholder={
                      running
                        ? "Follow up, or steer the current task…"
                        : "Ask Nanocodex to do anything"
                    }
                    value={tab.draft}
                    rows={2}
                    onChange={(event) => {
                      patchTab(tab.id, { draft: event.target.value });
                      event.target.style.height = "auto";
                      event.target.style.height = `${Math.min(
                        220,
                        event.target.scrollHeight
                      )}px`;
                    }}
                    onKeyDown={(event) => {
                      if (
                        event.key === "Enter" &&
                        !event.shiftKey &&
                        !event.nativeEvent.isComposing
                      ) {
                        event.preventDefault();
                        void send();
                      }
                    }}
                  />
                  <div className="composer-actions">
                    <div className="composer-left">
                      <button
                        type="button"
                        className="icon-button"
                        aria-label="Choose folder"
                        title="Choose folder · ⌘O"
                        onClick={() => void action(chooseFolder)}
                      >
                        <Plus size={19} />
                      </button>
                      <div className="model-picker">
                        <button
                          type="button"
                          className="model-trigger"
                          aria-expanded={modelOpen}
                          onClick={() => setModelOpen((value) => !value)}
                        >
                          {settings.model.replace("gpt-", "GPT-")}
                          <span>
                            {settings.thinking === "xhigh"
                              ? "Extra high"
                              : settings.thinking}
                          </span>
                          <ChevronDown size={12} />
                        </button>
                        {modelOpen && (
                          <div className="model-popover">
                            <label>
                              Model
                              <select
                                aria-label="Model"
                                value={settings.model}
                                disabled={modelLocked}
                                title={
                                  modelLocked
                                    ? "Start a new thread to change models."
                                    : undefined
                                }
                                onChange={(e) =>
                                  void changeSettings({
                                    ...settings,
                                    model: e.target.value,
                                  })
                                }
                              >
                                {[
                                  "gpt-5.6-sol",
                                  "gpt-5.6-terra",
                                  "gpt-5.6-luna",
                                  "gpt-6-astra",
                                ].map((model) => (
                                  <option key={model}>{model}</option>
                                ))}
                              </select>
                            </label>
                            <label>
                              Thinking
                              <select
                                aria-label="Reasoning effort"
                                value={settings.thinking}
                                onChange={(e) =>
                                  void changeSettings({
                                    ...settings,
                                    thinking: e.target.value,
                                  })
                                }
                              >
                                {[
                                  "none",
                                  "low",
                                  "medium",
                                  "high",
                                  "xhigh",
                                  "max",
                                ].map((effort) => (
                                  <option
                                    key={effort}
                                    disabled={
                                      settings.model === "gpt-6-astra" &&
                                      effort === "none"
                                    }
                                  >
                                    {effort}
                                  </option>
                                ))}
                              </select>
                            </label>
                            <label className="check-label">
                              <input
                                type="checkbox"
                                checked={settings.fast_mode}
                                onChange={(e) =>
                                  void changeSettings({
                                    ...settings,
                                    fast_mode: e.target.checked,
                                  })
                                }
                              />
                              Fast mode
                            </label>
                            <label
                              className="check-label"
                              title={
                                modelLocked
                                  ? "Start a new thread to change reasoning mode."
                                  : settings.model === "gpt-6-astra"
                                  ? "Astra uses standard reasoning."
                                  : undefined
                              }
                            >
                              <input
                                type="checkbox"
                                checked={settings.reasoning_mode === "pro"}
                                disabled={
                                  modelLocked ||
                                  settings.model === "gpt-6-astra"
                                }
                                onChange={(e) =>
                                  void changeSettings({
                                    ...settings,
                                    reasoning_mode: e.target.checked
                                      ? "pro"
                                      : "standard",
                                  })
                                }
                              />
                              Pro reasoning
                            </label>
                            <button
                              type="button"
                              className="small-button"
                              onClick={() => setModelOpen(false)}
                            >
                              Done
                            </button>
                          </div>
                        )}
                      </div>
                    </div>
                    <div className="composer-right">
                      {running && (
                        <>
                          <button
                            type="button"
                            className="steer-button"
                            disabled={!tab.draft.trim() || busy}
                            onClick={() => void send(tab.draft, "steer")}
                          >
                            Steer
                          </button>
                          <button
                            type="button"
                            className="stop-button"
                            aria-label="Stop current turn"
                            onClick={() =>
                              void action(() =>
                                bridge.cancel({
                                  agentId: selectedId!,
                                  turnId: thread!.activeTurns[0],
                                })
                              )
                            }
                          >
                            <Square size={13} fill="currentColor" />
                          </button>
                        </>
                      )}
                      <button
                        className="send-button"
                        type="submit"
                        aria-label={running ? "Queue message" : "Send message"}
                        disabled={
                          !tab.draft.trim() || busy || !ready || !!connecting
                        }
                      >
                        {busy ? (
                          <LoaderCircle size={17} className="spin" />
                        ) : (
                          <ArrowUp size={19} />
                        )}
                      </button>
                    </div>
                  </div>
                </form>
                <div className="composer-utility">
                  <label className="target-select">
                    <Cloud size={13} />
                    <select
                      aria-label="Execution Hand"
                      value={tab.target}
                      onChange={(e) =>
                        patchTab(tab.id, { target: e.target.value })
                      }
                    >
                      <option value="">Automatic</option>
                      {state.hands
                        .filter(
                          (hand) => !hand.agentId || hand.agentId === selectedId
                        )
                        .map((hand) => (
                          <option key={hand.id} value={hand.id}>
                            {hand.name}
                            {hand.status === "connected"
                              ? ""
                              : " · starts on send"}
                          </option>
                        ))}
                      {remoteHands
                        .filter(
                          (hand) =>
                            !state.hands.some(
                              (local) => hand.id === `user:${local.id}`
                            )
                        )
                        .map((hand) => (
                          <option key={hand.id} value={hand.id}>
                            {hand.name}
                          </option>
                        ))}
                    </select>
                    <ChevronDown size={11} />
                  </label>
                  <span>
                    {selectedId
                      ? thread?.connected
                        ? "Synced to cloud"
                        : "Reconnecting…"
                      : "Your agent keeps working when you leave"}
                  </span>
                  <button
                    onClick={() => setPage("hands")}
                    aria-label="Manage compute"
                  >
                    <Monitor size={13} />
                  </button>
                </div>
              </div>
            </div>
          )}
        </main>
      </div>
      {handModal && (
        <HandDialog
          initial={handModal}
          defaults={state.defaults}
          selectedId={selectedId}
          onClose={() => setHandModal(null)}
          onSave={async (config) => {
            const saved = await bridge.saveHand(config);
            const next = await bridge.startHand(config.id);
            setState(next);
            const hand = next.hands.find((item) => item.id === config.id);
            if (hand?.error) throw new Error(hand.error);
            setHandModal(null);
            setPage("hands");
          }}
        />
      )}
      {remoteModal && (
        <RemoteDialog
          origin={state.baseUrl}
          onClose={() => setRemoteModal(false)}
        />
      )}
      {searching && (
        <Modal title="Search threads" onClose={() => setSearching(false)}>
          <div className="search-dialog">
            <input
              autoFocus
              aria-label="Search threads"
              placeholder="Find a conversation…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <div>
              {state.threads
                .filter((item) =>
                  item.title.toLowerCase().includes(query.toLowerCase())
                )
                .map((item) => (
                  <button key={item.id} onClick={() => openRecent(item.id)}>
                    <SquarePen size={15} />
                    <span>{item.title}</span>
                    <ArrowUpRight size={13} />
                  </button>
                ))}
              {!state.threads.some((item) =>
                item.title.toLowerCase().includes(query.toLowerCase())
              ) && <p>No matching threads.</p>}
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
function basename(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) || path;
}
function relativeTime(value: number) {
  if (!value) return "";
  const minutes = Math.max(1, Math.floor((Date.now() - value) / 60000));
  return minutes < 60
    ? `${minutes}m`
    : minutes < 1440
    ? `${Math.floor(minutes / 60)}h`
    : `${Math.floor(minutes / 1440)}d`;
}
