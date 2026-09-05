import {
  Download,
  Maximize2,
  Minimize2,
  PanelRightClose,
  PanelRightOpen,
  RefreshCw,
  Sparkles,
  Trash2,
} from "lucide-react";
import { memo, useCallback, useEffect, useId, useRef, useState } from "react";
import {
  ArtifactStore,
  ArtifactDocument,
  ArtifactInput,
} from "nanocodex/tools/artifact";
import type { Workspace } from "nanocodex/browser/workspace";
import { LiveReactArtifact } from "./LiveReactArtifact";
import {
  getBrowserThread,
  openKernelWorkspace,
  subscribeThreadWorkspaceChanges,
} from "nanocodex/tools/browser";
import { useModalBoundary } from "./modalBoundary";
import { useModalFrameBoundary } from "./useModalFrameBoundary";

export const COMPACT_WORKSPACE_MEDIA_QUERY = "(max-width: 740px), (pointer: coarse) and (orientation: landscape) and (max-width: 950px)";

function compactWorkspace(): boolean {
  return window.matchMedia(COMPACT_WORKSPACE_MEDIA_QUERY).matches;
}

export const ArtifactDock = memo(function ArtifactDock({
  agentReady,
  onPrompt,
  workspace,
  workspaceId,
}: {
  agentReady: boolean;
  onPrompt(artifact: ArtifactDocument, prompt: string, path: string): void;
  workspace?: Workspace;
  workspaceId?: string;
}) {
  const [store, setStore] = useState<ArtifactStore>();
  const [artifacts, setArtifacts] = useState<readonly ArtifactDocument[]>([]);
  const [selectedId, setSelectedId] = useState<string>();
  const [collapsed, setCollapsed] = useState(true);
  const [fullscreen, setFullscreen] = useState(false);
  const [compact, setCompact] = useState(compactWorkspace);
  const [message, setMessage] = useState("");
  const refreshEpoch = useRef(0);
  const backdropRef = useRef<HTMLButtonElement>(null);
  const dockRef = useRef<HTMLElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const refocusToggle = useRef(false);
  const canvasId = useId();
  const selected = artifacts.find((artifact) => artifact.id === selectedId) ?? artifacts[0];
  const artifactCount = `${artifacts.length} existing artifact${artifacts.length === 1 ? "" : "s"}`;

  const refresh = useCallback(async (nextStore: ArtifactStore | undefined) => {
    if (!nextStore) return;
    const epoch = ++refreshEpoch.current;
    try {
      const { artifacts: next, rejected } = await nextStore.scan();
      if (epoch !== refreshEpoch.current) return;
      setArtifacts(next);
      setSelectedId((current) => current && next.some(({ id }) => id === current) ? current : next[0]?.id);
      setMessage(rejected.length
        ? `Skipped ${rejected.length} invalid artifact document${rejected.length === 1 ? "" : "s"}.`
        : next.length ? "" : "Ask the agent to create any custom interface, or preview the live React demo.");
    } catch (error) {
      if (epoch === refreshEpoch.current) setMessage(errorMessage(error));
    }
  }, []);

  useEffect(() => {
    let active = true;
    void (workspace ? Promise.resolve(workspace) : openKernelWorkspace()).then(async (nextWorkspace) => {
      if (!active) return;
      const nextStore = new ArtifactStore(nextWorkspace);
      setStore(nextStore);
      await refresh(nextStore);
    }).catch((error) => active && setMessage(errorMessage(error)));
    return () => {
      active = false;
      refreshEpoch.current++;
    };
  }, [refresh, workspace]);

  useEffect(() => {
    return subscribeThreadWorkspaceChanges(
      workspaceId ?? getBrowserThread().id,
      () => void refresh(store),
    );
  }, [refresh, store, workspaceId]);

  useEffect(() => {
    if (!store) return;
    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh(store);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [refresh, store]);

  useEffect(() => {
    const query = window.matchMedia(COMPACT_WORKSPACE_MEDIA_QUERY);
    const updateCompactWorkspace = () => {
      setCompact(query.matches);
      if (query.matches) setFullscreen(false);
    };
    updateCompactWorkspace();
    query.addEventListener("change", updateCompactWorkspace);
    return () => query.removeEventListener("change", updateCompactWorkspace);
  }, []);

  useEffect(() => {
    if (!refocusToggle.current) return;
    refocusToggle.current = false;
    toggleRef.current?.focus();
  }, [collapsed]);

  const remove = async () => {
    if (!store || !selected || !window.confirm(`Delete the artifact “${selected.title}”?`)) return;
    try {
      await store.remove(selected.id);
      await refresh(store);
    } catch (error) {
      setMessage(errorMessage(error));
    }
  };

  const download = () => {
    if (!selected) return;
    const url = URL.createObjectURL(new Blob([selected.source], { type: "text/javascript" }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${selected.id}.ui.js`;
    anchor.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
  };

  const ask = (prompt: string) => {
    if (!selected || !store) return;
    if (!agentReady) {
      setMessage("Connect the agent before running an artifact action.");
      return;
    }
    if (!window.confirm(`Send this artifact action to the agent?\n\n${prompt}`)) return;
    onPrompt(selected, prompt, store.path(selected.id));
  };

  const createExample = async () => {
    if (!store) return;
    try {
      const artifact = await store.save(exampleArtifact());
      await refresh(store);
      setSelectedId(artifact.id);
      setFullscreen(!compactWorkspace());
    } catch (error) {
      setMessage(errorMessage(error));
    }
  };

  const collapse = useCallback(() => {
    refocusToggle.current = true;
    setFullscreen(false);
    setCollapsed(true);
  }, []);

  const expand = useCallback(() => {
    refocusToggle.current = true;
    setCollapsed(false);
  }, []);

  const modalOpen = compact && !collapsed;
  useModalBoundary({
    backdropRef,
    initialFocusRef: toggleRef,
    onDismiss: collapse,
    open: modalOpen,
    panelRef: dockRef,
    returnFocusRef: toggleRef,
  });
  useModalFrameBoundary({
    onDismiss: collapse,
    open: modalOpen,
    panelRef: dockRef,
  });

  if (collapsed) {
    return (
      <aside className={`artifact-dock is-collapsed${artifacts.length === 0 ? " is-empty" : ""}`} aria-label="Artifacts">
        <button
          ref={toggleRef}
          type="button"
          aria-controls={canvasId}
          aria-expanded={false}
          aria-label={`Open artifacts, ${artifactCount}`}
          title="Open artifacts"
          onClick={expand}
        >
          <PanelRightOpen aria-hidden="true" />
          <span aria-hidden="true">{artifacts.length}</span>
        </button>
        <div id={canvasId} hidden />
      </aside>
    );
  }

  return (
    <>
      {modalOpen ? (
        <button
          ref={backdropRef}
          className="artifact-dock-backdrop"
          type="button"
          aria-hidden="true"
          tabIndex={-1}
          onPointerDown={collapse}
        />
      ) : null}
      <aside
        ref={dockRef}
        className={`artifact-dock${fullscreen ? " is-fullscreen" : ""}`}
        aria-label="Artifacts"
        aria-modal={modalOpen ? true : undefined}
        role={modalOpen ? "dialog" : "complementary"}
        tabIndex={modalOpen ? -1 : undefined}
      >
        <header className="artifact-dock-header">
          <Sparkles aria-hidden="true" />
          {artifacts.length > 1 ? (
            <select value={selected?.id} onChange={(event) => setSelectedId(event.target.value)} aria-label="Selected artifact">
              {artifacts.map((artifact) => <option key={artifact.id} value={artifact.id}>{artifact.title}</option>)}
            </select>
          ) : <strong>{selected?.title ?? "Artifacts"}</strong>}
          <div>
            <DockAction label="Refresh artifacts" onClick={() => void refresh(store)}><RefreshCw /></DockAction>
            <DockAction label="Download artifact" disabled={!selected} onClick={download}><Download /></DockAction>
            <DockAction label="Delete artifact" disabled={!selected} onClick={() => void remove()}><Trash2 /></DockAction>
            <DockAction label={fullscreen ? "Exit fullscreen" : "View fullscreen"} onClick={() => setFullscreen((value) => !value)}>
              {fullscreen ? <Minimize2 /> : <Maximize2 />}
            </DockAction>
            <DockAction
              buttonRef={toggleRef}
              className="artifact-dock-toggle"
              controls={canvasId}
              expanded
              label="Collapse artifacts"
              onClick={collapse}
            >
              <PanelRightClose />
            </DockAction>
          </div>
        </header>
        <div className="artifact-canvas" id={canvasId}>
          {selected ? (
            <LiveReactArtifact artifact={selected} onAction={ask} />
          ) : (
            <div className="artifact-empty">
              <PanelRightOpen aria-hidden="true" />
              {message ? <p>{message}</p> : null}
              <button className="artifact-preview-button" type="button" onClick={() => void createExample()}>
                Preview custom UI
              </button>
            </div>
          )}
        </div>
        {message && selected ? <p className="artifact-dock-status" role="status">{message}</p> : null}
      </aside>
    </>
  );
});

function DockAction({
  buttonRef,
  children,
  className,
  controls,
  disabled,
  expanded,
  label,
  onClick,
}: {
  buttonRef?: React.Ref<HTMLButtonElement>;
  children: React.ReactNode;
  className?: string;
  controls?: string;
  disabled?: boolean;
  expanded?: boolean;
  label: string;
  onClick(): void;
}) {
  return (
    <button
      ref={buttonRef}
      type="button"
      className={className}
      disabled={disabled}
      aria-controls={controls}
      aria-expanded={expanded}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function exampleArtifact(): ArtifactInput & { id: string } {
  return {
    id: "artifact-demo",
    title: "Live React artifact demo",
    source: `
function App({ sendPrompt }) {
  const [theme, setTheme] = React.useState("electric");
  return html\`<main className=\${theme}>
    <style>\${\`
      html, body, #root { min-height: 100%; margin: 0; }
      main { min-height: 100vh; padding: clamp(32px, 8vw, 110px); color: #eaffff; background: radial-gradient(circle at 15% 10%, #154f68, #071116 55%); transition: .5s; }
      main.steampunk { color: #ffe6ae; background: radial-gradient(circle at 15% 10%, #70451e, #17100a 58%); }
      h1 { max-width: 850px; margin: 0; font: 800 clamp(50px, 9vw, 130px)/.86 system-ui; letter-spacing: -.07em; }
      p { max-width: 650px; font-size: clamp(18px, 2.2vw, 28px); opacity: .78; }
      button { margin: 12px 12px 0 0; padding: 13px 18px; color: inherit; background: #ffffff12; border: 1px solid currentColor; border-radius: 999px; cursor: pointer; }
    \`}</style>
    <h1>Speak the interface into existence.</h1>
    <p>This is real React generated at runtime, isolated from the credential-bearing host page.</p>
    <button onClick=\${() => setTheme(theme === "electric" ? "steampunk" : "electric")}>Retheme locally</button>
    <button onClick=\${() => sendPrompt("Turn this live interface into an animated mission control dashboard")}>Ask the agent to evolve it</button>
  </main>\`;
}`,
  };
}
