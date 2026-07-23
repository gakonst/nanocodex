import { useVirtualizer } from "@tanstack/react-virtual";
import {
  cloneElement,
  isValidElement,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type ComponentPropsWithoutRef,
  type KeyboardEvent,
  type RefObject,
} from "react";
import { Streamdown, type Components } from "streamdown";

import {
  appendError,
  applyAgentEvents,
  groupAgentEventsByTarget,
  initialTerminalState,
  pendingCount,
  queuePrompt,
  queueSteer,
  steerAdmitted,
  steerFailed,
  steerQueued,
  turnFinished,
  turnRejected,
  type AgentEvent,
  type PlanUpdate,
  type TerminalEntry,
  type TerminalState,
  type ToolActivity,
  type TuiCommand,
  type TuiMessage,
  type TuiTarget,
} from "nanocodex-tui";
import {
  useNanocodex,
  useNanocodexMessage,
} from "nanocodex-react";
import { SyntaxCode } from "./SyntaxCode";

type Target = TuiTarget;
type Focus = "main" | "btw";

type BranchView = {
  id: number;
  parentId?: number;
  conversation: TerminalState;
  draft: string;
  images: AttachedImage[];
};

type BtwView = { id: number; conversation: TerminalState };

type AttachedImage = { placeholder: string; dataUrl: string };

type HistoricalEdit = {
  sourceBranchId: number;
  promptId: number;
  entryIndex: number;
  savedDraft: string;
  savedImages: AttachedImage[];
};

type TuiState = {
  branches: BranchView[];
  activeBranchId: number;
  btw?: BtwView;
  focus: Focus;
  selectedPromptId?: number;
  historicalEdit?: HistoricalEdit;
  branchNavigatorId?: number;
};

type TranscriptController = { scrollBy(rows: number): void; jumpToBottom(): void };

const DEFAULT_STARTER_PROMPT =
  "Explain in two sentences what runs in Rust/WASM and what the browser hosts.";
const CANCEL_WINDOW_MS = 1_000;
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const staticMarkdownComponents = createMarkdownComponents(false);
const streamingMarkdownComponents = createMarkdownComponents(true);

function createMarkdownComponents(streaming: boolean): Components {
  return {
    pre({ children }) {
      return isValidElement<{ "data-block"?: string }>(children)
        ? cloneElement(children, { "data-block": "true" })
        : children;
    },
    code({ className, children }) {
      const code = String(children).replace(/\n$/, "");
      const language = /(?:^|\s)language-([^\s]+)/.exec(className ?? "")?.[1];
      return <div className="tui-markdown-code">
        <span>┌─ {language ?? "code"} · {codeLineCount(code)} LOC</span>
        <SyntaxCode code={code} language={language} streaming={streaming} tree />
        <span>└─</span>
      </div>;
    },
    inlineCode({ className, children, node: _node, ...props }) {
      return <code className={["tui-inline-code", className].filter(Boolean).join(" ")} {...props}>{children}</code>;
    },
    strong({ node: _node, ...props }) {
      return <strong {...props} />;
    },
    a({ node: _node, ...props }) {
      return <a {...props} />;
    },
  };
}

export type NanocodexTuiProps = Omit<
  ComponentPropsWithoutRef<"section">,
  "children"
> & {
  /** Initial composer text. Read once when the TUI is created. */
  starterPrompt?: string;
  /** Text rendered in the terminal title badge. */
  brand?: string;
  /** Context label rendered beside the title badge. */
  cwd?: string;
  /** Whether the embedding application currently permits agent input. */
  enabled?: boolean;
  /** Status shown when the embedding application disables agent input. */
  unavailableMessage?: string;
};

export function NanocodexTui({
  starterPrompt = DEFAULT_STARTER_PROMPT,
  brand = "nanocodex",
  cwd = "/browser",
  enabled = true,
  unavailableMessage = "Agent unavailable",
  className,
  "aria-label": ariaLabel = "Nanocodex terminal",
  onClick,
  onKeyDown: onRootKeyDown,
  ...rootProps
}: NanocodexTuiProps) {
  const { status: workerStatus, error: workerError, dispatch, stop } = useNanocodex<TuiCommand>();
  const ready = workerStatus === "ready";
  const stopped = workerStatus === "stopped";
  const [tui, setTui] = useState<TuiState>(() => ({
    branches: [{ id: 0, conversation: initialTerminalState(), draft: starterPrompt, images: [] }],
    activeBranchId: 0,
    focus: "main",
  }));
  const [draft, setDraft] = useState(starterPrompt);
  const [images, setImages] = useState<AttachedImage[]>([]);
  const [externalEditor, setExternalEditor] = useState(false);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const terminalRef = useRef<HTMLElement>(null);
  const nextPromptId = useRef(1);
  const nextBranchId = useRef(1);
  const nextBtwId = useRef(1);
  const escapeArmedAt = useRef(0);
  const eventQueue = useRef<Array<{ target: Target; event: AgentEvent }>>([]);
  const animationFrame = useRef<number | undefined>(undefined);
  const transcriptControllers = useRef(new Map<string, TranscriptController>());

  const flushEvents = useCallback(() => {
    if (animationFrame.current !== undefined) {
      cancelAnimationFrame(animationFrame.current);
      animationFrame.current = undefined;
    }
    const queued = eventQueue.current;
    eventQueue.current = [];
    if (!queued.length) return;
    setTui((current) => {
      let next = current;
      for (const batch of groupAgentEventsByTarget(queued)) {
        next = updateConversation(next, batch.target, (conversation) =>
          applyAgentEvents(conversation, batch.events),
        );
      }
      return next;
    });
  }, []);

  useNanocodexMessage<TuiMessage>((data) => {
      if (data.type === "event") {
        eventQueue.current.push({ target: data.target, event: data.event });
        animationFrame.current ??= requestAnimationFrame(flushEvents);
        return;
      }
      flushEvents();
      if (data.type === "ready") return;
      if (data.type === "turnFinished") {
        setTui((current) => updateConversation(current, data.target, (conversation) =>
          turnFinished(conversation, data.error),
        ));
        return;
      }
      if (data.type === "steerAdmitted") {
        setTui((current) => updateConversation(current, data.target, (conversation) =>
          steerAdmitted(conversation, data.id),
        ));
        return;
      }
      if (data.type === "steerQueued") {
        setTui((current) => updateConversation(current, data.target, (conversation) =>
          steerQueued(conversation, data.id, data.prompt),
        ));
        return;
      }
      if (data.type === "steerFailed") {
        setTui((current) => updateConversation(current, data.target, (conversation) =>
          steerFailed(conversation, data.id, data.error),
        ));
        return;
      }
      if (data.type === "cancelAccepted") {
        setTui((current) => updateConversation(current, data.target, (conversation) => ({
          ...conversation,
          status: conversation.status === "Cancelling" ? "Cancellation accepted" : conversation.status,
        })));
        return;
      }
      if (data.type === "cancelFailed") {
        setTui((current) => updateConversation(current, data.target, (conversation) =>
          appendError({ ...conversation, status: conversation.running ? "Working" : "Ready" }, data.error),
        ));
        return;
      }
      if (data.type === "btwOpened") {
        setTui((current) => current.btw?.id === data.id ? {
          ...current,
          btw: {
            ...current.btw,
            conversation: {
              ...current.btw.conversation,
              status: current.btw.conversation.pendingTurns ? "Starting" : "Ready",
            },
          },
        } : current);
        return;
      }
      if (data.type === "btwOpenFailed") {
        setTui((current) => current.btw?.id === data.id ? {
          ...current,
          btw: {
            ...current.btw,
            conversation: turnRejected(
              { ...current.btw.conversation, status: "Fork failed" },
              data.error,
            ),
          },
        } : current);
        return;
      }
      if (data.type === "branchOpened") return;
      if (data.type === "branchOpenFailed") {
        setTui((current) => updateConversation(
          current,
          { pane: "main", branchId: data.id },
          (conversation) => turnRejected({ ...conversation, status: "Historical edit failed" }, data.error),
        ));
        return;
      }
      if (data.type === "fatal") {
        setTui((current) => updateActiveConversation(current, (conversation) =>
          appendError({ ...conversation, status: "Agent worker stopped" }, data.message),
        ));
      }
  });

  useEffect(() => () => {
    if (animationFrame.current !== undefined) cancelAnimationFrame(animationFrame.current);
  }, []);

  useLayoutEffect(() => {
    const composer = composerRef.current;
    if (!composer) return;
    composer.style.height = "0";
    const lineHeight = 18;
    composer.style.height = `${Math.min(composer.scrollHeight, lineHeight * 7)}px`;
  }, [draft]);

  const conversation = activeConversation(tui);
  const activeBranch = branchById(tui, tui.activeBranchId)!;
  const target = activeTarget(tui);
  const mode = tui.historicalEdit
    ? "edit"
    : tui.branchNavigatorId !== undefined
      ? "branches"
      : tui.selectedPromptId !== undefined
        ? "history"
        : "normal";

  const submit = (intent: "immediate" | "queue") => {
    const text = draft;
    if ((!text.trim() && !images.length) || !ready || stopped || !enabled) return;
    if (tui.historicalEdit) {
      if (intent === "immediate") commitHistoricalEdit(text.trim());
      return;
    }
    setDraft("");
    const submittedImages = images;
    setImages([]);
    classifyAndSubmit(text, intent, submittedImages);
  };

  const classifyAndSubmit = (raw: string, intent: "immediate" | "queue", submittedImages: AttachedImage[]) => {
    const trimmed = raw.trim();
    if (trimmed === "/btw" || trimmed.startsWith("/btw ")) {
      const question = trimmed === "/btw" ? undefined : trimmed.slice(5).trim() || undefined;
      if (tui.btw) {
        setTui((current) => ({ ...current, focus: "btw" }));
        if (question) queueInput({ pane: "btw", id: tui.btw.id }, question, "queue", submittedImages);
      } else {
        const id = nextBtwId.current++;
        const promptId = question ? nextPromptId.current++ : undefined;
        setTui((current) => ({
          ...current,
          focus: "btw",
          btw: {
            id,
            conversation: question && promptId !== undefined
              ? queuePrompt(initialTerminalState("Forking latest checkpoint"), promptId, question)
              : initialTerminalState("Forking latest checkpoint"),
          },
        }));
        dispatch({
          type: "openBtw",
          id,
          sourceBranchId: tui.activeBranchId,
          promptId,
          prompt: question,
          images: submittedImages.map((image) => image.dataUrl),
        });
      }
      return;
    }
    if (trimmed === "/close") {
      if (!tui.btw) return;
      if (tui.btw.conversation.running || tui.btw.conversation.pendingTurns > 0) {
        setTui((current) => current.btw ? {
          ...current,
          btw: {
            ...current.btw,
            conversation: appendError(
              { ...current.btw.conversation, status: "BTW still running" },
              "BTW has an active or queued turn; wait for it to finish before /close",
            ),
          },
        } : current);
      } else {
        dispatch({ type: "closeBtw", id: tui.btw.id });
        setTui((current) => ({ ...current, btw: undefined, focus: "main" }));
      }
      return;
    }
    if (trimmed === "/cancel") {
      cancelTurn();
      return;
    }
    if (trimmed === "/trace") {
      window.open("http://127.0.0.1:16686/search?service=nanocodex", "_blank", "noopener");
      return;
    }
    queueInput(target, raw, intent, submittedImages);
  };

  const queueInput = (inputTarget: Target, text: string, intent: "immediate" | "queue", promptImages: AttachedImage[] = []) => {
    const id = nextPromptId.current++;
    const currentConversation = conversationForTarget(tui, inputTarget);
    const steering = intent === "immediate" && currentConversation?.running;
    setTui((current) => updateConversation(current, inputTarget, (value) =>
      steering ? queueSteer(value, id, text) : queuePrompt(value, id, text),
    ));
    dispatch({ type: "prompt", target: inputTarget, id, prompt: text, images: promptImages.map((image) => image.dataUrl), intent });
  };

  const cancelTurn = () => {
    setTui((current) => updateConversation(current, activeTarget(current), (value) => ({
      ...value,
      status: "Cancelling",
    })));
    dispatch({ type: "cancel", target });
  };

  const commitHistoricalEdit = (replacement: string) => {
    const edit = tui.historicalEdit;
    if (!edit) return;
    const source = branchById(tui, edit.sourceBranchId);
    if (!source) return;
    const newBranchId = nextBranchId.current++;
    const newPromptId = nextPromptId.current++;
    const prefix = source.conversation.entries.slice(0, edit.entryIndex);
    const branch: BranchView = {
      id: newBranchId,
      parentId: source.id,
      conversation: queuePrompt(
        { ...initialTerminalState("Forking before selected prompt"), entries: prefix },
        newPromptId,
        replacement,
      ),
      draft: "",
      images: [],
    };
    setDraft("");
    setImages([]);
    setTui((current) => ({
      ...current,
      branches: [...current.branches, branch],
      activeBranchId: newBranchId,
      selectedPromptId: undefined,
      historicalEdit: undefined,
      focus: "main",
    }));
    dispatch({
      type: "historicalFork",
      sourceBranchId: source.id,
      newBranchId,
      selectedPromptId: edit.promptId,
      newPromptId,
      prompt: replacement,
    });
  };

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    const key = event.key;
    const ctrl = event.ctrlKey;
    const alt = event.altKey;
    if (ctrl && alt && !tui.historicalEdit) {
      if (key.toLowerCase() === "b") {
        event.preventDefault();
        toggleBranchNavigator();
        return;
      }
      if (key === "ArrowUp" || key === "ArrowDown") {
        event.preventDefault();
        cycleBranch(key === "ArrowUp" ? -1 : 1);
        return;
      }
    }
    if (tui.branchNavigatorId !== undefined) {
      if (["ArrowUp", "k", "ArrowDown", "j", "Enter", "Escape", "q"].includes(key)) {
        event.preventDefault();
        if (key === "ArrowUp" || key === "k") moveBranchNavigator(-1);
        else if (key === "ArrowDown" || key === "j") moveBranchNavigator(1);
        else if (key === "Enter") switchNavigatedBranch();
        else setTui((current) => ({ ...current, branchNavigatorId: undefined }));
      }
      return;
    }
    if (tui.selectedPromptId !== undefined && !tui.historicalEdit) {
      if (key === "ArrowUp" || (ctrl && key.toLowerCase() === "p")) {
        event.preventDefault();
        moveHistorySelection(-1);
      } else if (key === "ArrowDown" || (ctrl && key.toLowerCase() === "n")) {
        event.preventDefault();
        moveHistorySelection(1);
      } else if (key.toLowerCase() === "e" && !ctrl && !alt) {
        event.preventDefault();
        startHistoricalEdit();
      } else if (key === "Escape") {
        event.preventDefault();
        setTui((current) => ({ ...current, selectedPromptId: undefined }));
      }
      return;
    }
    if (ctrl) {
      if (key.toLowerCase() === "c") {
        event.preventDefault();
        stop();
        setTui((current) => updateActiveConversation(current, (value) => ({ ...value, status: "Session stopped" })));
        return;
      }
      if (key.toLowerCase() === "g") {
        event.preventDefault();
        setExternalEditor(true);
        return;
      }
      if (key.toLowerCase() === "d" && !draft) {
        event.preventDefault();
        stop();
        setTui((current) => updateActiveConversation(current, (value) => ({ ...value, status: "Session stopped" })));
        return;
      }
      if (key === "End") {
        event.preventDefault();
        activeTranscriptController()?.jumpToBottom();
        return;
      }
      if (handleReadlineKey(event, draft, updateDraft, composerRef.current)) return;
    }
    if (alt && (key === "ArrowLeft" || key.toLowerCase() === "b" || key === "ArrowRight" || key.toLowerCase() === "f")) {
      event.preventDefault();
      moveWord(composerRef.current, key === "ArrowLeft" || key.toLowerCase() === "b" ? -1 : 1);
      return;
    }
    if (key === "Enter" && (event.shiftKey || alt)) {
      event.preventDefault();
      insertAtCursor("\n");
      return;
    }
    if (key === "Enter") {
      event.preventDefault();
      submit("immediate");
      return;
    }
    if (key === "Tab") {
      event.preventDefault();
      if (event.shiftKey) {
        if (tui.btw) setTui((current) => ({ ...current, focus: current.focus === "main" ? "btw" : "main" }));
      } else if (draft.trim()) submit("queue");
      else if (tui.btw) setTui((current) => ({ ...current, focus: current.focus === "main" ? "btw" : "main" }));
      return;
    }
    if (key === "Escape") {
      event.preventDefault();
      if (tui.historicalEdit) {
        setDraft(tui.historicalEdit.savedDraft);
        setImages(tui.historicalEdit.savedImages);
        setTui((current) => ({ ...current, historicalEdit: undefined, selectedPromptId: undefined }));
      } else if (!conversation.running) {
        setDraft("");
        setImages([]);
      } else if (Date.now() - escapeArmedAt.current <= CANCEL_WINDOW_MS) {
        escapeArmedAt.current = 0;
        cancelTurn();
      } else {
        escapeArmedAt.current = Date.now();
        setTui((current) => updateActiveConversation(current, (value) => ({ ...value, status: "Stop Agent Turn — Esc again to confirm" })));
      }
      return;
    }
    if (key === "PageUp" || key === "PageDown") {
      event.preventDefault();
      activeTranscriptController()?.scrollBy(key === "PageUp" ? -12 : 12);
      return;
    }
    if (key === "ArrowUp" && composerAtFirstVisualLine()) {
      const users = userEntries(conversation);
      const last = users.at(-1);
      if (last?.promptId !== undefined) {
        event.preventDefault();
        setTui((current) => ({ ...current, selectedPromptId: last.promptId }));
      }
    }
  };

  const insertAtCursor = (text: string) => {
    const node = composerRef.current;
    if (!node) return;
    const start = node.selectionStart;
    const end = node.selectionEnd;
    const value = draft.slice(0, start) + text + draft.slice(end);
    updateDraft(value);
    requestAnimationFrame(() => node.setSelectionRange(start + text.length, start + text.length));
  };

  const updateDraft = useCallback((value: string) => {
    setDraft(value);
    setImages((current) => current.filter((image) => value.includes(image.placeholder)));
  }, []);

  const handleImagePaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const pasted = [...event.clipboardData.files].filter((file) => file.type.startsWith("image/"));
    if (!pasted.length) return;
    event.preventDefault();
    const node = event.currentTarget;
    const start = node.selectionStart;
    const end = node.selectionEnd;
    const retained = images.filter((image) => draft.includes(image.placeholder));
    void Promise.all(pasted.map(readAsDataUrl)).then((dataUrls) => {
      const added = dataUrls.map((dataUrl, index) => ({
        placeholder: `[Image #${retained.length + index + 1}]`,
        dataUrl,
      }));
      const inserted = `${added.map((image) => image.placeholder).join(" ")} `;
      setImages([...retained, ...added]);
      setDraft((value) => value.slice(0, start) + inserted + value.slice(end));
      requestAnimationFrame(() => node.setSelectionRange(start + inserted.length, start + inserted.length));
    }).catch((error) => {
      setTui((current) => updateActiveConversation(current, (value) =>
        appendError(value, `Clipboard image failed: ${errorMessage(error)}`),
      ));
    });
  };

  const composerAtFirstVisualLine = () => {
    const node = composerRef.current;
    if (!node) return true;
    return !draft.slice(0, node.selectionStart).includes("\n");
  };

  const activeTranscriptController = () => {
    const key = tui.focus === "btw" && tui.btw ? `btw-${tui.btw.id}` : `main-${tui.activeBranchId}`;
    return transcriptControllers.current.get(key);
  };

  const moveHistorySelection = (direction: number) => {
    const users = userEntries(conversation);
    const index = users.findIndex((entry) => entry.promptId === tui.selectedPromptId);
    const targetEntry = users[index + direction];
    setTui((current) => ({ ...current, selectedPromptId: targetEntry?.promptId }));
    if (!targetEntry && direction > 0) composerRef.current?.focus();
  };

  const startHistoricalEdit = () => {
    if (tui.focus !== "main" || tui.btw || tui.selectedPromptId === undefined) return;
    const entryIndex = conversation.entries.findIndex(
      (entry) => entry.kind === "user" && entry.promptId === tui.selectedPromptId,
    );
    const entry = conversation.entries[entryIndex];
    if (entry?.kind !== "user" || entry.promptId === undefined) return;
    setTui((current) => ({
      ...current,
      historicalEdit: {
        sourceBranchId: current.activeBranchId,
        promptId: entry.promptId!,
        entryIndex,
        savedDraft: draft,
        savedImages: images,
      },
    }));
    setDraft(entry.text);
    setImages([]);
    requestAnimationFrame(() => composerRef.current?.focus());
  };

  const toggleBranchNavigator = () => {
    if (tui.btw || tui.branches.length < 2 || tui.historicalEdit) return;
    setTui((current) => ({
      ...current,
      selectedPromptId: undefined,
      branchNavigatorId: current.branchNavigatorId === undefined ? current.activeBranchId : undefined,
    }));
  };

  const moveBranchNavigator = (direction: number) => {
    if (tui.branchNavigatorId === undefined) return;
    const ordered = orderedBranches(tui.branches);
    const index = ordered.findIndex(({ branch }) => branch.id === tui.branchNavigatorId);
    const selected = ordered[(index + direction + ordered.length) % ordered.length]!.branch;
    const idle = tui.branches.every((branch) => !branch.conversation.running && !branch.conversation.pendingTurns);
    const branches = persistActiveDraft(tui, draft, images);
    setTui({
      ...tui,
      branches,
      branchNavigatorId: selected.id,
      activeBranchId: idle ? selected.id : tui.activeBranchId,
    });
    if (idle && selected.id !== tui.activeBranchId) {
      setDraft(selected.draft);
      setImages(selected.images);
    }
  };

  const switchNavigatedBranch = () => {
    if (tui.branchNavigatorId === undefined) return;
    const selected = branchById(tui, tui.branchNavigatorId);
    if (!selected) return;
    setTui({
      ...tui,
      branches: persistActiveDraft(tui, draft, images),
      activeBranchId: selected.id,
      branchNavigatorId: undefined,
    });
    if (selected.id !== tui.activeBranchId) {
      setDraft(selected.draft);
      setImages(selected.images);
    }
  };

  const cycleBranch = (direction: number) => {
    if (tui.btw || tui.branches.some((branch) => branch.conversation.running || branch.conversation.pendingTurns)) return;
    const ordered = orderedBranches(tui.branches);
    const index = ordered.findIndex(({ branch }) => branch.id === tui.activeBranchId);
    const selected = ordered[(index + direction + ordered.length) % ordered.length]!.branch;
    setTui({ ...tui, branches: persistActiveDraft(tui, draft, images), activeBranchId: selected.id });
    setDraft(selected.draft);
    setImages(selected.images);
  };

  const registerController = useCallback((key: string, controller?: TranscriptController) => {
    if (controller) transcriptControllers.current.set(key, controller);
    else transcriptControllers.current.delete(key);
  }, []);

  const previewBranch = tui.branchNavigatorId === undefined
    ? activeBranch
    : branchById(tui, tui.branchNavigatorId) ?? activeBranch;
  const showPending = Math.max(
    pendingCount(previewBranch.conversation),
    tui.btw ? pendingCount(tui.btw.conversation) : 0,
  ) > 0;

  return (
    <section
      {...rootProps}
      className={["agent-tui", className].filter(Boolean).join(" ")}
      data-nc-part="root"
      data-mode={mode}
      data-running={conversation.running ? "" : undefined}
      data-split={tui.btw ? "" : undefined}
      data-disabled={!enabled || !ready ? "" : undefined}
      aria-disabled={!enabled || !ready}
      aria-label={ariaLabel}
      ref={terminalRef}
      tabIndex={0}
      onKeyDown={(event) => {
        onKeyDown(event);
        if (!event.defaultPrevented) onRootKeyDown?.(event);
      }}
      onClick={(event) => {
        onClick?.(event);
        if (!event.defaultPrevented) composerRef.current?.focus();
      }}
    >
      <header className="agent-tui-header" data-nc-part="header">
        <span className="tui-brand" data-nc-part="brand"> {brand} </span>
        <span className="tui-cwd" data-nc-part="context">  {cwd}</span>
        {tui.branches.length > 1 ? (
          <span className="tui-branches">  branches {branchGraph(tui)} · Ctrl+Alt+B browse · Ctrl+Alt+↑/↓ cycle</span>
        ) : null}
      </header>

      <div
        className={`agent-tui-transcripts${tui.btw ? " is-split" : tui.branchNavigatorId !== undefined ? " is-navigating" : ""}`}
        data-nc-part="transcripts"
      >
        {tui.btw ? (
          <>
            <TranscriptPane
              title="Main"
              conversation={activeBranch.conversation}
              focused={tui.focus === "main"}
              selectedPromptId={tui.focus === "main" ? tui.selectedPromptId : undefined}
              historicalEdit={tui.historicalEdit}
              editValue={tui.historicalEdit ? draft : undefined}
              onEdit={tui.historicalEdit ? updateDraft : undefined}
              editorRef={composerRef}
              controllerKey={`main-${activeBranch.id}`}
              register={registerController}
            />
            <TranscriptPane
              title="BTW · forked context"
              conversation={tui.btw.conversation}
              focused={tui.focus === "btw"}
              controllerKey={`btw-${tui.btw.id}`}
              register={registerController}
            />
          </>
        ) : tui.branchNavigatorId !== undefined ? (
          <>
            <TranscriptPane
              title={`Branch ${previewBranch.id} preview`}
              conversation={previewBranch.conversation}
              focused
              controllerKey={`main-${previewBranch.id}`}
              register={registerController}
            />
            <BranchNavigator tui={tui} />
          </>
        ) : (
          <TranscriptPane
            title="Main"
            conversation={activeBranch.conversation}
            focused
            selectedPromptId={tui.selectedPromptId}
            historicalEdit={tui.historicalEdit}
            editValue={tui.historicalEdit ? draft : undefined}
            onEdit={tui.historicalEdit ? updateDraft : undefined}
            editorRef={composerRef}
            controllerKey={`main-${activeBranch.id}`}
            register={registerController}
          />
        )}
      </div>

      {showPending ? <PendingPanes tui={tui} /> : null}

      <fieldset
        className={`agent-tui-composer${conversation.running ? " is-running" : ""}`}
        data-nc-part="composer"
        data-running={conversation.running ? "" : undefined}
      >
        <legend>{composerTitle(tui, mode)}</legend>
        {mode === "branches" || mode === "edit" ? (
          <span className="tui-draft-preserved"> draft preserved </span>
        ) : (
          <textarea
            ref={composerRef}
            value={draft}
            disabled={!ready || stopped || !enabled}
            aria-label="Message Nanocodex"
            onChange={(event) => updateDraft(event.target.value)}
            onPaste={handleImagePaste}
            rows={1}
            spellCheck={false}
          />
        )}
      </fieldset>

      <Footer
        tui={tui}
        conversation={conversation}
        mode={mode}
        workerStatus={workerStatus}
        workerError={workerError}
        enabled={enabled}
        unavailableMessage={unavailableMessage}
      />

      {externalEditor ? (
        <div className="agent-tui-editor" data-nc-part="editor" role="dialog" aria-modal="true" aria-label="External editor">
          <fieldset>
            <legend> $EDITOR · Ctrl+Enter save · Esc cancel </legend>
            <textarea
              autoFocus
              defaultValue={draft}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setExternalEditor(false);
                } else if (event.key === "Enter" && event.ctrlKey) {
                  event.preventDefault();
                  event.stopPropagation();
                  updateDraft(event.currentTarget.value.replace(/\s+$/, ""));
                  setExternalEditor(false);
                }
              }}
            />
          </fieldset>
        </div>
      ) : null}
    </section>
  );
}

const TranscriptPane = memo(function TranscriptPane({
  title,
  conversation,
  focused,
  selectedPromptId,
  historicalEdit,
  editValue,
  onEdit,
  editorRef,
  controllerKey,
  register,
}: {
  title: string;
  conversation: TerminalState;
  focused: boolean;
  selectedPromptId?: number;
  historicalEdit?: HistoricalEdit;
  editValue?: string;
  onEdit?(value: string): void;
  editorRef?: RefObject<HTMLTextAreaElement | null>;
  controllerKey: string;
  register(key: string, controller?: TranscriptController): void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const followTail = useRef(true);
  const [unseen, setUnseen] = useState(false);
  const virtualizer = useVirtualizer({
    count: conversation.entries.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => estimateHeight(conversation.entries[index]),
    getItemKey: (index) => conversation.entries[index]?.id ?? index,
    overscan: 8,
  });

  useLayoutEffect(() => {
    if (!conversation.entries.length) return;
    if (followTail.current && selectedPromptId === undefined) {
      virtualizer.scrollToIndex(conversation.entries.length - 1, { align: "end" });
      setUnseen(false);
    } else setUnseen(true);
  }, [conversation.entries, selectedPromptId, virtualizer]);

  useEffect(() => {
    register(controllerKey, {
      scrollBy(rows) {
        const node = scrollRef.current;
        node?.scrollBy({ top: rows * 18, behavior: "auto" });
      },
      jumpToBottom() {
        followTail.current = true;
        setUnseen(false);
        if (conversation.entries.length) {
          virtualizer.scrollToIndex(conversation.entries.length - 1, { align: "end" });
        }
      },
    });
    return () => register(controllerKey, undefined);
  }, [controllerKey, conversation.entries.length, register, virtualizer]);

  const displayTitle = unseen ? `${title} ↓ New output · Ctrl+End` : title;
  return (
    <fieldset
      className={`agent-tui-pane${focused ? " is-focused" : ""}`}
      data-nc-part="transcript"
      data-focused={focused ? "" : undefined}
    >
      <legend> {displayTitle} </legend>
      <div
        className="agent-tui-scroll"
        data-nc-part="scroll-area"
        ref={scrollRef}
        onScroll={(event) => {
          const node = event.currentTarget;
          followTail.current = node.scrollHeight - node.scrollTop - node.clientHeight < 24;
          if (followTail.current) setUnseen(false);
        }}
      >
        {!conversation.entries.length ? (
          <p className="agent-tui-empty">  Ask Nanocodex to inspect, edit, run, or explain this workspace.</p>
        ) : (
          <div className="agent-tui-virtual" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((row) => {
              const entry = conversation.entries[row.index];
              if (!entry) return null;
              return (
                <div
                  className="agent-tui-row"
                  data-nc-part="transcript-row"
                  data-index={row.index}
                  key={entry.id}
                  ref={virtualizer.measureElement}
                  style={{ transform: `translateY(${row.start}px)` }}
                >
                  <TranscriptRow
                    entry={entry}
                    selected={entry.kind === "user" && entry.promptId === selectedPromptId}
                    editing={historicalEdit?.entryIndex === row.index}
                    editValue={editValue}
                    onEdit={onEdit}
                    editorRef={editorRef}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
    </fieldset>
  );
});

const TranscriptRow = memo(function TranscriptRow({
  entry,
  selected,
  editing,
  editValue,
  onEdit,
  editorRef,
}: {
  entry: TerminalEntry;
  selected: boolean;
  editing: boolean;
  editValue?: string;
  onEdit?(value: string): void;
  editorRef?: RefObject<HTMLTextAreaElement | null>;
}) {
  if (editing) {
    return <fieldset className="tui-inline-edit" data-nc-part="inline-editor"><legend> Edit message · Esc cancel </legend><textarea
      ref={editorRef}
      autoFocus
      value={editValue ?? ""}
      aria-label="Edit historical message"
      onChange={(event) => onEdit?.(event.target.value)}
      rows={1}
      spellCheck={false}
    /></fieldset>;
  }
  if (entry.kind === "user") {
    return <div className={`tui-entry tui-user${selected ? " is-selected" : ""}`} data-nc-part="entry" data-kind="user" data-selected={selected ? "" : undefined}><strong>› You</strong><pre>{entry.text}</pre></div>;
  }
  if (entry.kind === "reasoning") {
    return <div className="tui-entry tui-reasoning" data-nc-part="entry" data-kind="reasoning" data-streaming={entry.streaming ? "" : undefined}><span>• </span><pre>{entry.text}</pre></div>;
  }
  if (entry.kind === "assistant") {
    return <div className="tui-entry tui-assistant" data-nc-part="entry" data-kind="assistant" data-streaming={entry.streaming ? "" : undefined}>
      <strong className="tui-assistant-label">● Nanocodex</strong>
      <div className="tui-markdown-body"><Streamdown
        animated={false}
        components={entry.streaming ? streamingMarkdownComponents : staticMarkdownComponents}
        controls={false}
        isAnimating={entry.streaming}
        mode={entry.streaming ? "streaming" : "static"}
        parseIncompleteMarkdown={entry.streaming}
      >{entry.text}</Streamdown></div>
    </div>;
  }
  if (entry.kind === "error") {
    return <div className="tui-entry tui-error" data-nc-part="entry" data-kind="error">✗ {entry.text}</div>;
  }
  if (entry.kind === "plan") {
    return <PlanRow update={entry.update} />;
  }
  return <ToolRow tool={entry.tool} />;
});

function PlanRow({ update }: { update: PlanUpdate }) {
  const explanation = update.explanation?.trim();
  return <div className="tui-entry tui-plan" data-nc-part="entry" data-kind="plan">
    <div><span className="tui-dim">• </span><strong>Updated Plan</strong></div>
    {explanation ? <div className="tui-plan-line tui-dim"><span>└ </span><em>{explanation}</em></div> : null}
    {update.plan.map((item, index) => {
      const marker = item.status === "completed" ? "✔" : "□";
      const connector = !explanation && index === 0 ? "└ " : "  ";
      return <div className={`tui-plan-line is-${item.status}`} key={`${index}-${item.step}`}><span className="tui-dim">{connector}</span>{marker} {item.step}</div>;
    })}
  </div>;
}

function ToolRow({ tool }: { tool: ToolActivity }) {
  const style = toolStyle(tool.status);
  const details = toolDetails(tool);
  const displayName = tool.name === "exec" ? "Code Mode" : tool.name;
  return (
    <div className="tui-entry tui-tool" data-nc-part="entry" data-kind="tool" data-state={tool.status}>
      <div><strong className={style.className}>{style.icon} {displayName}</strong>{details ? <span className="tui-dim">  {details}</span> : null}</div>
      {tool.name === "exec" && tool.arguments && !tool.children.length ? (
        <div className="tui-tool-code"><span>  ┌─ javascript · {codeLineCount(tool.arguments)} LOC</span><SyntaxCode code={tool.arguments} language="javascript" tree /><span>  └─</span></div>
      ) : !tool.children.length && (tool.arguments || tool.result) ? (
        <pre className="tui-tool-detail"><span>  └─ </span>{[tool.arguments, tool.result].filter(Boolean).join(" · ")}</pre>
      ) : null}
      {tool.children.map((child, index) => <ToolChild key={child.callId} tool={child} last={index + 1 === tool.children.length} />)}
      {tool.images?.length ? <div className="tui-tool-images">{tool.images.map((imageUrl, index) => (
        <img alt={`Generated output ${index + 1}`} key={`${tool.callId}-image-${index}`} src={imageUrl} />
      ))}</div> : null}
    </div>
  );
}

function codeLineCount(source: string): number {
  let end = source.length;
  while (end > 0 && (source[end - 1] === "\n" || source[end - 1] === "\r")) end -= 1;
  return end ? lineCount(source, end) : 0;
}

function ToolChild({ tool, last }: { tool: ToolActivity; last: boolean }) {
  const style = toolStyle(tool.status);
  const displayName = tool.name === "exec_command"
    ? tool.status === "running" ? "Running" : "Ran"
    : tool.name;
  const detail = [tool.arguments, tool.durationNs ? formatDuration(tool.durationNs) : undefined, tool.result]
    .filter(Boolean).join(" · ");
  return <div className="tui-tool-child"><span className="tui-dim">  {last ? "└─" : "├─"}</span><span className={style.className}> {style.icon} {displayName}</span>{detail ? <span className="tui-dim">  {detail}</span> : null}</div>;
}

function BranchNavigator({ tui }: { tui: TuiState }) {
  const ordered = orderedBranches(tui.branches);
  const busy = tui.branches.some((branch) => branch.conversation.running || branch.conversation.pendingTurns);
  return (
    <fieldset className="agent-tui-pane tui-branch-tree" data-nc-part="branch-navigator">
      <legend> Branch tree · {busy ? "live preview; switch when idle" : "moving switches"} </legend>
      {ordered.map(({ branch, prefix, depth }) => {
        const selected = tui.branchNavigatorId === branch.id;
        const active = tui.activeBranchId === branch.id;
        const prompt = [...branch.conversation.entries].reverse().find((entry) => entry.kind === "user");
        return (
          <div className="tui-branch" key={branch.id}>
            <div className={selected ? "is-selected" : active ? "is-active" : ""}>{selected ? "›" : " "} {prefix}{active ? "●" : "○"} branch {branch.id}{active ? " current" : ""}</div>
            <div className="tui-dim">  {"  ".repeat(depth)}{prompt && "text" in prompt ? preview(prompt.text) : "(branch point)"}</div>
          </div>
        );
      })}
    </fieldset>
  );
}

function PendingPanes({ tui }: { tui: TuiState }) {
  const main = branchById(tui, tui.activeBranchId)!.conversation;
  return <div className={`agent-tui-pending${tui.btw ? " is-split" : ""}`} data-nc-part="pending">
    <PendingPane title={tui.btw ? "Main pending input" : "Pending input"} conversation={main} focused={tui.focus === "main"} />
    {tui.btw ? <PendingPane title="BTW pending input" conversation={tui.btw.conversation} focused={tui.focus === "btw"} /> : null}
  </div>;
}

function PendingPane({ title, conversation, focused }: { title: string; conversation: TerminalState; focused: boolean }) {
  return <fieldset className={`agent-tui-pending-pane${focused ? " is-focused" : ""}`}><legend> {title} </legend>
    {conversation.pendingSteers.slice(0, 3).map((steer) => <div key={steer.id}><span className={steer.state === "admitted" ? "tui-yellow" : "tui-dim"}>{steer.state === "admitted" ? "↳ steer   " : "… steer   "}</span>{preview(steer.text)}</div>)}
    {conversation.queuedPrompts.slice(0, Math.max(0, 3 - conversation.pendingSteers.length)).map((prompt) => <div key={prompt.id}><span className="tui-dim">⏳ queued </span>{preview(prompt.text)}</div>)}
  </fieldset>;
}

const Footer = memo(function Footer({ tui, conversation, mode, workerStatus, workerError, enabled, unavailableMessage }: {
  tui: TuiState;
  conversation: TerminalState;
  mode: "normal" | "history" | "edit" | "branches";
  workerStatus: "idle" | "starting" | "ready" | "stopped" | "error";
  workerError?: string;
  enabled: boolean;
  unavailableMessage: string;
}) {
  const spinning = mode === "normal" && conversation.running && !conversation.status.startsWith("Stop Agent");
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    if (!spinning) return;
    const timer = window.setInterval(() => setFrame((value) => value + 1), 80);
    return () => window.clearInterval(timer);
  }, [spinning]);
  let status = workerStatus === "stopped"
    ? "Session stopped"
    : workerStatus === "error"
      ? workerError ?? "Agent worker failed"
      : workerStatus !== "ready"
      ? "Loading Rust/WASM..."
      : !enabled
        ? unavailableMessage
        : conversation.status;
  if (mode === "branches") status = "Branches — ↑/↓ or j/k switch + preview · Esc close";
  else if (mode === "edit") status = "Editing history — Enter fork/send · Shift+Enter newline · Esc cancel · Ctrl+G $EDITOR";
  else if (mode === "history") status = "History — ↑/↓ navigate · e fork-edit · Esc return";
  else if (spinning) status = `${SPINNER[frame % SPINNER.length]} Thinking...`;
  const queued = Math.max(0, conversation.pendingTurns - Number(conversation.running));
  const steers = conversation.pendingSteers.length;
  const counts = [steers ? `${steers} steer${steers === 1 ? "" : "s"}` : "", queued ? `${queued} queued` : ""].filter(Boolean).join(" · ");
  const help = tui.btw
    ? "BackTab switch · Ctrl+V image · /close dismiss · Enter send/steer · Tab queue · Esc Esc stop · Ctrl+C quit"
    : "/btw <question> side fork · Ctrl+V image · Enter send/steer · Tab queue · Esc Esc stop · Ctrl+C quit";
  return <footer className="agent-tui-footer" data-nc-part="footer"> {status}{counts ? ` · ${counts}` : ""}  {help}</footer>;
});

function composerTitle(tui: TuiState, mode: string): string {
  if (mode === "edit") return " Message composer · editing history inline above ";
  if (mode === "branches") return " Message composer · browsing branches ";
  const conversation = activeConversation(tui);
  const target = tui.focus === "btw" ? "BTW" : "Main";
  return conversation.running
    ? ` Message → ${target} (Enter steers · Tab queues) `
    : ` Message → ${target} `;
}

function updateConversation(tui: TuiState, target: Target, update: (state: TerminalState) => TerminalState): TuiState {
  if (target.pane === "btw") {
    return tui.btw?.id === target.id
      ? { ...tui, btw: { ...tui.btw, conversation: update(tui.btw.conversation) } }
      : tui;
  }
  return {
    ...tui,
    branches: tui.branches.map((branch) => branch.id === target.branchId
      ? { ...branch, conversation: update(branch.conversation) }
      : branch),
  };
}

function updateActiveConversation(tui: TuiState, update: (state: TerminalState) => TerminalState): TuiState {
  return updateConversation(tui, activeTarget(tui), update);
}

function activeTarget(tui: TuiState): Target {
  return tui.focus === "btw" && tui.btw
    ? { pane: "btw", id: tui.btw.id }
    : { pane: "main", branchId: tui.activeBranchId };
}

function activeConversation(tui: TuiState): TerminalState {
  return conversationForTarget(tui, activeTarget(tui)) ?? initialTerminalState("Unavailable");
}

function conversationForTarget(tui: TuiState, target: Target): TerminalState | undefined {
  return target.pane === "btw"
    ? tui.btw?.id === target.id ? tui.btw.conversation : undefined
    : branchById(tui, target.branchId)?.conversation;
}

function branchById(tui: TuiState, id: number): BranchView | undefined {
  return tui.branches.find((branch) => branch.id === id);
}

function persistActiveDraft(tui: TuiState, draft: string, images: AttachedImage[]): BranchView[] {
  return tui.branches.map((branch) => branch.id === tui.activeBranchId ? { ...branch, draft, images } : branch);
}

function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => typeof reader.result === "string"
      ? resolve(reader.result)
      : reject(new Error("clipboard image did not produce a data URL"));
    reader.onerror = () => reject(reader.error ?? new Error("clipboard image could not be read"));
    reader.readAsDataURL(file);
  });
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function userEntries(conversation: TerminalState) {
  return conversation.entries.filter((entry): entry is Extract<TerminalEntry, { kind: "user" }> => entry.kind === "user" && entry.promptId !== undefined);
}

function orderedBranches(branches: BranchView[]): Array<{ branch: BranchView; prefix: string; depth: number }> {
  const byParent = new Map<number | undefined, BranchView[]>();
  for (const branch of branches) {
    const children = byParent.get(branch.parentId) ?? [];
    children.push(branch);
    byParent.set(branch.parentId, children);
  }
  for (const children of byParent.values()) children.sort((a, b) => a.id - b.id);
  const output: Array<{ branch: BranchView; prefix: string; depth: number }> = [];
  const visit = (branch: BranchView, guides: boolean[], last?: boolean) => {
    const prefix = guides.map((more) => more ? "│ " : "  ").join("") + (last === undefined ? "" : last ? "└─" : "├─");
    output.push({ branch, prefix, depth: guides.length + Number(last !== undefined) });
    const children = byParent.get(branch.id) ?? [];
    children.forEach((child, index) => visit(child, last === undefined ? guides : [...guides, !last], index + 1 === children.length));
  };
  const ids = new Set(branches.map((branch) => branch.id));
  branches.filter((branch) => branch.parentId === undefined || !ids.has(branch.parentId)).sort((a, b) => a.id - b.id).forEach((branch) => visit(branch, []));
  return output;
}

function branchGraph(tui: TuiState): string {
  return orderedBranches(tui.branches).map(({ branch }) => `${branch.id}${branch.id === tui.activeBranchId ? "*" : ""}${branch.parentId === undefined ? "" : `←${branch.parentId}`}`).join(" ");
}

function preview(text: string): string {
  const compact = text.split(/\s+/).filter(Boolean).join(" ");
  return [...compact].length <= 96 ? compact : `${[...compact].slice(0, 95).join("")}…`;
}

function toolStyle(status: ToolActivity["status"]): { icon: string; className: string } {
  if (status === "completed") return { icon: "✓", className: "tui-green" };
  if (status === "cancelled") return { icon: "■", className: "tui-yellow" };
  if (status === "failed") return { icon: "✗", className: "tui-red" };
  return { icon: "◌", className: "tui-yellow" };
}

function toolDetails(tool: ToolActivity): string {
  const details: string[] = [];
  if (tool.children.length) details.push(`${tool.children.length} call${tool.children.length === 1 ? "" : "s"}`);
  if (tool.durationNs) details.push(formatDuration(tool.durationNs));
  if (tool.children.length >= 2 && tool.durationNs) {
    const childDuration = tool.children.reduce((sum, child) => sum + (child.durationNs ?? 0), 0);
    details.push(childDuration > tool.durationNs * 1.2 ? "overlapping" : "sequence");
  }
  if (tool.children.length && tool.result) details.push(tool.result);
  return details.join(" · ");
}

function formatDuration(nanoseconds: number): string {
  if (nanoseconds < 1_000_000) return `${Math.floor(nanoseconds / 1_000)}µs`;
  if (nanoseconds < 1_000_000_000) return `${Math.floor(nanoseconds / 1_000_000)}ms`;
  return `${(nanoseconds / 1_000_000_000).toFixed(1)}s`;
}

function estimateHeight(entry: TerminalEntry | undefined): number {
  if (!entry) return 36;
  if (entry.kind === "tool") return 54 + entry.tool.children.length * 18 + lineCount(entry.tool.arguments) * 18;
  if (entry.kind === "plan") return 28 + (entry.update.plan.length + (entry.update.explanation ? 1 : 0)) * 18;
  const lines = lineCount(entry.text);
  return Math.min(520, 28 + lines * 18);
}

function lineCount(source: string, end = source.length): number {
  let lines = 1;
  for (let index = 0; index < end; index += 1) {
    if (source.charCodeAt(index) === 10) lines += 1;
  }
  return lines;
}

function handleReadlineKey(
  event: KeyboardEvent<HTMLElement>,
  value: string,
  setValue: (value: string) => void,
  node: HTMLTextAreaElement | null,
): boolean {
  if (!node) return false;
  const key = event.key.toLowerCase();
  const start = node.selectionStart;
  const end = node.selectionEnd;
  const apply = (next: string, cursor: number) => {
    event.preventDefault();
    setValue(next);
    requestAnimationFrame(() => node.setSelectionRange(cursor, cursor));
  };
  if (key === "a") {
    event.preventDefault();
    const cursor = value.lastIndexOf("\n", start - 1) + 1;
    requestAnimationFrame(() => node.setSelectionRange(cursor, cursor));
    return true;
  }
  if (key === "e") {
    event.preventDefault();
    const newline = value.indexOf("\n", end);
    const cursor = newline < 0 ? value.length : newline;
    requestAnimationFrame(() => node.setSelectionRange(cursor, cursor));
    return true;
  }
  if (key === "b" || key === "f") {
    event.preventDefault();
    const cursor = key === "b" ? Math.max(0, start - 1) : Math.min(value.length, end + 1);
    requestAnimationFrame(() => node.setSelectionRange(cursor, cursor));
    return true;
  }
  if (key === "p" || key === "n") {
    event.preventDefault();
    const lineStart = value.lastIndexOf("\n", start - 1) + 1;
    const column = start - lineStart;
    let cursor = start;
    if (key === "p" && lineStart > 0) {
      const previousEnd = lineStart - 1;
      const previousStart = value.lastIndexOf("\n", previousEnd - 1) + 1;
      cursor = Math.min(previousStart + column, previousEnd);
    } else if (key === "n") {
      const lineEnd = value.indexOf("\n", end);
      if (lineEnd >= 0) {
        const nextStart = lineEnd + 1;
        const nextEnd = value.indexOf("\n", nextStart);
        cursor = Math.min(nextStart + column, nextEnd < 0 ? value.length : nextEnd);
      }
    }
    requestAnimationFrame(() => node.setSelectionRange(cursor, cursor));
    return true;
  }
  if (key === "h") {
    if (start !== end) apply(value.slice(0, start) + value.slice(end), start);
    else if (start > 0) apply(value.slice(0, start - 1) + value.slice(start), start - 1);
    else event.preventDefault();
    return true;
  }
  if (key === "d") {
    if (start !== end) apply(value.slice(0, start) + value.slice(end), start);
    else if (start < value.length) apply(value.slice(0, start) + value.slice(start + 1), start);
    else event.preventDefault();
    return true;
  }
  if (key === "j") {
    apply(value.slice(0, start) + "\n" + value.slice(end), start + 1);
    return true;
  }
  if (key === "w") {
    const before = value.slice(0, start);
    const cut = before.search(/\s*\S+\s*$/);
    const from = cut < 0 ? 0 : cut;
    apply(value.slice(0, from) + value.slice(end), from);
    return true;
  }
  if (key === "u") {
    const from = value.lastIndexOf("\n", start - 1) + 1;
    const cut = from === start && from > 0 ? from - 1 : from;
    apply(value.slice(0, cut) + value.slice(end), cut);
    return true;
  }
  if (key === "k") {
    const newline = value.indexOf("\n", end);
    const to = newline < 0 ? value.length : newline === end ? newline + 1 : newline;
    apply(value.slice(0, start) + value.slice(to), start);
    return true;
  }
  return false;
}

function moveWord(node: HTMLTextAreaElement | null, direction: -1 | 1): void {
  if (!node) return;
  const value = node.value;
  let cursor = direction < 0 ? node.selectionStart : node.selectionEnd;
  if (direction < 0) {
    while (cursor > 0 && /\s/.test(value[cursor - 1]!)) cursor -= 1;
    while (cursor > 0 && !/\s/.test(value[cursor - 1]!)) cursor -= 1;
  } else {
    while (cursor < value.length && !/\s/.test(value[cursor]!)) cursor += 1;
    while (cursor < value.length && /\s/.test(value[cursor]!)) cursor += 1;
  }
  node.setSelectionRange(cursor, cursor);
}
