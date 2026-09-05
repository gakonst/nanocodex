import {
  ChevronDown,
  ChevronRight,
  Copy,
  Download,
  File,
  FilePlus,
  Folder,
  FolderPlus,
  GitBranch,
  GitPullRequestArrow,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCw,
  Save,
  Trash2,
  Upload,
} from "lucide-react";
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Workspace, WorkspaceEntry } from "nanocodex/browser/workspace";

import {
  commitAndPushThread,
  getBrowserThread,
  initializeThreadGit,
  notifyThreadGitChanged,
  openKernelWorkspace,
  pullThread,
  subscribeThreadGitChanges,
  threadGitStatus,
  type ThreadGitStatus,
} from "nanocodex/tools/browser";
import {
  buildWorkspaceTree,
  parentWorkspaceDirectory,
  relativeWorkspacePath,
  type WorkspaceTreeNode,
} from "./workspaceTree";
import { listVisibleWorkspaceEntries } from "./workspaceListing";

const decoder = new TextDecoder("utf-8", { fatal: true });

export const WorkspacePanel = memo(function WorkspacePanel() {
  const thread = useMemo(getBrowserThread, []);
  const [workspace, setWorkspace] = useState<Workspace>();
  const [gitStatus, setGitStatus] = useState<ThreadGitStatus>();
  const [entries, setEntries] = useState<readonly WorkspaceEntry[]>([]);
  const [selectedPath, setSelectedPath] = useState<string>();
  const [contents, setContents] = useState("");
  const [savedContents, setSavedContents] = useState("");
  const [loadedFile, setLoadedFile] = useState<Pick<WorkspaceEntry, "modifiedAt" | "path" | "size">>();
  const [expanded, setExpanded] = useState(() => new Set<string>());
  const [creating, setCreating] = useState<"file" | "directory">();
  const [newName, setNewName] = useState("");
  const [panelOpen, setPanelOpen] = useState(true);
  const [busy, setBusy] = useState(false);
  const [gitBusy, setGitBusy] = useState(false);
  const [message, setMessage] = useState("");
  const uploadRef = useRef<HTMLInputElement>(null);
  const notificationSource = useRef(crypto.randomUUID()).current;
  const selected = entries.find((entry) => entry.path === selectedPath);
  const dirty = selected?.kind === "file"
    && selected.path === loadedFile?.path
    && contents !== savedContents;

  const refresh = useCallback(async (nextWorkspace: Workspace | undefined) => {
    if (!nextWorkspace) return;
    try {
      const nextEntries = await listVisibleWorkspaceEntries(nextWorkspace);
      setEntries(nextEntries);
      setSelectedPath((current) => current && nextEntries.some(({ path }) => path === current)
        ? current
        : undefined);
      setMessage(nextEntries.length ? "" : "No files yet. Ask the agent or create one here.");
    } catch (error) {
      setMessage(errorMessage(error));
    }
  }, []);

  useEffect(() => {
    let active = true;
    void initializeThreadGit(thread)
      .then(async (nextGitStatus) => {
        const nextWorkspace = await openKernelWorkspace();
        if (!active) return;
        setGitStatus(nextGitStatus);
        setWorkspace(nextWorkspace);
        await refresh(nextWorkspace);
      })
      .catch((error) => active && setMessage(errorMessage(error)));
    return () => { active = false; };
  }, [refresh, thread]);

  useEffect(() => {
    if (!workspace) return;
    let unsubscribe: (() => void) | undefined;
    let active = true;
    const refreshWorkspace = async () => {
      try {
        const [nextGitStatus] = await Promise.all([
          threadGitStatus(thread),
          refresh(workspace),
        ]);
        if (active) setGitStatus(nextGitStatus);
      } catch (error) {
        if (active) setMessage(errorMessage(error));
      }
    };
    unsubscribe = subscribeThreadGitChanges(thread, (source) => {
      if (source !== notificationSource) void refreshWorkspace();
    });
    const onVisible = () => {
      if (document.visibilityState === "visible") void refreshWorkspace();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      active = false;
      unsubscribe?.();
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [notificationSource, refresh, thread, workspace]);

  useEffect(() => {
    const mobile = window.matchMedia("(max-width: 740px)");
    const expandOnMobile = () => {
      if (mobile.matches) setPanelOpen(true);
    };
    expandOnMobile();
    mobile.addEventListener("change", expandOnMobile);
    return () => mobile.removeEventListener("change", expandOnMobile);
  }, []);

  useEffect(() => {
    if (!workspace || !selected || selected.kind !== "file") {
      setLoadedFile(undefined);
      return;
    }
    if (selected.path === loadedFile?.path
      && selected.modifiedAt === loadedFile.modifiedAt
      && selected.size === loadedFile.size) return;
    let active = true;
    void workspace.readFile(selected.path)
      .then((bytes) => decoder.decode(bytes))
      .then((text) => {
        if (!active) return;
        setContents(text);
        setSavedContents(text);
        setLoadedFile({
          modifiedAt: selected.modifiedAt,
          path: selected.path,
          size: selected.size,
        });
        setMessage("");
      })
      .catch((error) => {
        if (!active) return;
        setContents("");
        setSavedContents("");
        setLoadedFile({
          modifiedAt: selected.modifiedAt,
          path: selected.path,
          size: selected.size,
        });
        setMessage(`Cannot edit ${selected.path}: ${errorMessage(error)}`);
      });
    return () => { active = false; };
  }, [loadedFile, selected?.kind, selected?.modifiedAt, selected?.path, selected?.size, workspace]);

  const tree = useMemo(
    () => workspace ? buildWorkspaceTree(workspace.root, entries) : [],
    [entries, workspace],
  );
  const createBase = workspace
    ? parentWorkspaceDirectory(workspace.root, selected?.path, selected?.kind)
    : "/workspace";

  const mutate = async (operation: () => Promise<void>, success: string): Promise<boolean> => {
    if (!workspace || busy) return false;
    setBusy(true);
    try {
      await operation();
      await refresh(workspace);
      notifyThreadGitChanged(thread, notificationSource);
      setGitStatus(await threadGitStatus(thread));
      setMessage(success);
      return true;
    } catch (error) {
      setMessage(errorMessage(error));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const syncGit = async (
    operation: () => Promise<ThreadGitStatus>,
    success: string,
  ) => {
    if (!workspace || gitBusy) return;
    setGitBusy(true);
    try {
      const next = await operation();
      setGitStatus(next);
      await refresh(workspace);
      setMessage(success);
    } catch (error) {
      setMessage(errorMessage(error));
    } finally {
      setGitBusy(false);
    }
  };

  const push = async () => {
    await syncGit(
      async () => {
        if (dirty && selected?.kind === "file") {
          await workspace?.writeFile(selected.path, contents);
          setSavedContents(contents);
        }
        return commitAndPushThread(
          thread,
          "Update Nanocodex workspace",
          notificationSource,
        );
      },
      "Committed and pushed origin nanocodex.",
    );
  };

  const pull = async () => {
    if ((dirty || gitStatus?.changes.length) && !window.confirm(
      "Pulling can replace local workspace changes. Continue?",
    )) return;
    await syncGit(async () => {
      return pullThread(thread, notificationSource);
    }, "Pulled origin nanocodex into OPFS.");
  };

  const copyThreadLink = async () => {
    try {
      await navigator.clipboard.writeText(thread.shareUrl);
      setMessage("Copied this thread's workspace link.");
    } catch (error) {
      setMessage(errorMessage(error));
    }
  };

  const createEntry = async () => {
    const name = newName.trim();
    if (!workspace || !creating || !name) return;
    const path = `${createBase}/${name}`;
    const created = await mutate(
      () => creating === "directory" ? workspace.mkdir(path) : workspace.writeFile(path, ""),
      `Created ${path}`,
    );
    if (!created) return;
    setExpanded((current) => new Set(current).add(createBase));
    setCreating(undefined);
    setNewName("");
    setSelectedPath(path);
  };

  const uploadFiles = async (files: FileList | null) => {
    if (!workspace || !files?.length) return;
    const uploaded = [...files];
    await mutate(async () => {
      for (const file of uploaded) {
        await workspace.writeFile(`${createBase}/${file.name}`, await file.arrayBuffer());
      }
    }, `Uploaded ${uploaded.length} file${uploaded.length === 1 ? "" : "s"}`);
    if (uploadRef.current) uploadRef.current.value = "";
  };

  const save = () => selected?.kind === "file" && mutate(async () => {
    await workspace?.writeFile(selected.path, contents);
    setSavedContents(contents);
  }, `Saved ${selected.path}`);

  const download = async () => {
    if (!workspace || selected?.kind !== "file") return;
    try {
      const bytes = await workspace.readFile(selected.path);
      const url = URL.createObjectURL(new Blob([bytes.slice().buffer]));
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = selected.path.split("/").at(-1) ?? "download";
      anchor.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (error) {
      setMessage(errorMessage(error));
    }
  };

  const remove = () => selected && workspace && window.confirm(
    `Delete ${selected.path}${selected.kind === "directory" ? " and everything inside it" : ""}?`,
  ) && mutate(
    () => workspace.remove(selected.path, { recursive: selected.kind === "directory" }),
    `Deleted ${selected.path}`,
  );

  return (
    <aside className={panelOpen ? "workspace-panel" : "workspace-panel is-collapsed"} aria-label="Kernel workspace">
      <header className="workspace-panel-header">
        <button
          type="button"
          className="workspace-panel-toggle"
          onClick={() => setPanelOpen((open) => !open)}
          aria-label={panelOpen ? "Collapse workspace" : "Open workspace"}
        >
          {panelOpen ? <PanelLeftClose aria-hidden="true" /> : <PanelLeftOpen aria-hidden="true" />}
        </button>
        {panelOpen ? <strong>Workspace</strong> : null}
        {panelOpen ? (
          <div className="workspace-panel-actions">
            <WorkspaceAction label="New file" onClick={() => setCreating("file")}><FilePlus /></WorkspaceAction>
            <WorkspaceAction label="New folder" onClick={() => setCreating("directory")}><FolderPlus /></WorkspaceAction>
            <WorkspaceAction label="Upload files" onClick={() => uploadRef.current?.click()}><Upload /></WorkspaceAction>
            <WorkspaceAction label="Refresh" onClick={() => void refresh(workspace)}><RefreshCw /></WorkspaceAction>
          </div>
        ) : null}
      </header>

      {panelOpen ? (
        <>
          <div
            className="workspace-git"
            role="group"
            aria-label="Thread Git repository"
          >
            <span title={thread.remoteUrl}>
              <GitBranch aria-hidden="true" />
              {thread.branch}
            </span>
            <small>{gitStatus?.changes.length
              ? `${gitStatus.changes.length} changed`
              : gitStatus?.head ? gitStatus.head.slice(0, 7) : "local"}</small>
            <WorkspaceAction label="Pull origin nanocodex" disabled={gitBusy} onClick={() => void pull()}>
              <GitPullRequestArrow />
            </WorkspaceAction>
            <WorkspaceAction label="Commit and push origin nanocodex" disabled={gitBusy} onClick={() => void push()}>
              <Upload />
            </WorkspaceAction>
            <WorkspaceAction label="Copy thread workspace link" onClick={() => void copyThreadLink()}>
              <Copy />
            </WorkspaceAction>
          </div>
          <input
            ref={uploadRef}
            className="workspace-file-input"
            type="file"
            aria-label="Upload workspace files"
            multiple
            onChange={(event) => void uploadFiles(event.currentTarget.files)}
          />
          {creating ? (
            <form className="workspace-create" onSubmit={(event) => { event.preventDefault(); void createEntry(); }}>
              <span>{createBase}/</span>
              <input
                autoFocus
                value={newName}
                onChange={(event) => setNewName(event.target.value)}
                placeholder={creating === "file" ? "notes.md" : "src"}
                aria-label={`New ${creating} name`}
              />
              <button type="submit" disabled={!newName.trim() || busy}>Create</button>
              <button type="button" onClick={() => setCreating(undefined)}>Cancel</button>
            </form>
          ) : null}
          <nav className="workspace-tree" aria-label="Workspace files">
            {tree.map((node) => (
              <WorkspaceTreeItem
                key={node.path}
                node={node}
                depth={0}
                expanded={expanded}
                selectedPath={selectedPath}
                onSelect={setSelectedPath}
                onToggle={(path) => setExpanded((current) => {
                  const next = new Set(current);
                  if (next.has(path)) next.delete(path);
                  else next.add(path);
                  return next;
                })}
              />
            ))}
            {!tree.length ? <p>{message}</p> : null}
          </nav>
          <section className="workspace-editor" aria-label="Workspace file editor">
            {selected?.kind === "file" ? (
              <>
                <header title={selected.path}>
                  <span>{relativeWorkspacePath(workspace?.root ?? "/workspace", selected.path)}</span>
                  {dirty ? <i>modified</i> : null}
                </header>
                <textarea
                  value={contents}
                  onChange={(event) => setContents(event.target.value)}
                  spellCheck={false}
                  aria-label={`Edit ${selected.path}`}
                />
                <footer>
                  <WorkspaceAction label="Save file" disabled={!dirty || busy} onClick={() => void save()}><Save /></WorkspaceAction>
                  <WorkspaceAction label="Download file" onClick={() => void download()}><Download /></WorkspaceAction>
                  <WorkspaceAction label="Delete file" disabled={busy} onClick={() => void remove()}><Trash2 /></WorkspaceAction>
                  <span>{formatBytes(selected.size)}</span>
                </footer>
              </>
            ) : selected ? (
              <div className="workspace-selection">
                <Folder aria-hidden="true" />
                <span>{relativeWorkspacePath(workspace?.root ?? "/workspace", selected.path)}</span>
                <button type="button" disabled={busy} onClick={() => void remove()}>Delete folder</button>
              </div>
            ) : (
              <div className="workspace-selection">Select a file to edit it.</div>
            )}
          </section>
          {message && tree.length ? <p className="workspace-status" role="status">{message}</p> : null}
        </>
      ) : null}
    </aside>
  );
});

function WorkspaceTreeItem({
  node,
  depth,
  expanded,
  selectedPath,
  onSelect,
  onToggle,
}: {
  node: WorkspaceTreeNode;
  depth: number;
  expanded: ReadonlySet<string>;
  selectedPath: string | undefined;
  onSelect(path: string): void;
  onToggle(path: string): void;
}) {
  const open = expanded.has(node.path);
  return (
    <div>
      <button
        type="button"
        className={selectedPath === node.path ? "workspace-tree-item is-selected" : "workspace-tree-item"}
        style={{ paddingInlineStart: `${8 + depth * 14}px` }}
        onClick={() => {
          onSelect(node.path);
          if (node.kind === "directory") onToggle(node.path);
        }}
      >
        {node.kind === "directory"
          ? open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />
          : <span className="workspace-tree-spacer" />}
        {node.kind === "directory" ? <Folder aria-hidden="true" /> : <File aria-hidden="true" />}
        <span>{node.name}</span>
      </button>
      {node.kind === "directory" && open
        ? node.children.map((child) => (
            <WorkspaceTreeItem
              key={child.path}
              node={child}
              depth={depth + 1}
              expanded={expanded}
              selectedPath={selectedPath}
              onSelect={onSelect}
              onToggle={onToggle}
            />
          ))
        : null}
    </div>
  );
}

function WorkspaceAction({
  children,
  disabled,
  label,
  onClick,
}: {
  children: ReactNode;
  disabled?: boolean;
  label: string;
  onClick(): void;
}) {
  return (
    <button type="button" disabled={disabled} onClick={onClick} aria-label={label} title={label}>
      {children}
    </button>
  );
}

function formatBytes(value: number | undefined): string {
  if (value === undefined) return "";
  if (value < 1_000) return `${value} B`;
  if (value < 1_000_000) return `${(value / 1_000).toFixed(1)} KB`;
  return `${(value / 1_000_000).toFixed(1)} MB`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
