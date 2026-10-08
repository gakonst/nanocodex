"use client";

import {
  type ReactNode,
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { AgentEntry, ToolActivity } from "nanocodex-react/agent";
import { ArrowDown, Check, CircleAlert, Copy, FileText, Image as ImageIcon } from "lucide-react";
import { looksLikeRawError, presentAgentError, presentAssistantText } from "./errorPresentation.js";
import { RichMarkdown } from "./RichMarkdown.js";

import type { AgentStatus, AgentTerminalMode } from "./types.js";
import {
  LiveStatus, SubagentBlock, WorkGroup, groupTranscript, readableActivity, subagentRoles,
  type TranscriptRow,
} from "./TranscriptActivity.js";

export type VoiceTerminalEntry = Readonly<{
  afterEntryId?: string;
  id: string;
  kind: "user" | "assistant";
  source: "voice";
  streaming: boolean;
  text: string;
}>;

type TerminalEntry = AgentEntry | VoiceTerminalEntry;
const EMPTY_VOICE_ENTRIES: readonly VoiceTerminalEntry[] = [];

type ReadingAnchor = { element: Element; top: number };

function readingAnchors(viewport: HTMLElement): ReadingAnchor[] {
  const bounds = viewport.getBoundingClientRect();
  const anchors: ReadingAnchor[] = [];
  for (const element of Array.from(viewport.firstElementChild?.children ?? [])) {
    const row = element.getBoundingClientRect();
    if (row.bottom <= bounds.top) continue;
    anchors.push({ element, top: row.top - bounds.top });
    if (anchors.length === 2) break;
  }
  return anchors;
}

export function TerminalTranscriptSurface({
  canLoadOlder,
  composer,
  entries,
  followTailRequest = 0,
  inactiveMessage,
  isLoadingOlder,
  mode,
  running = false,
  activity,
  showToolCalls = true,
  renderTool,
  userLabel,
  status,
  voiceEntries = EMPTY_VOICE_ENTRIES,
  welcome,
  onLoadOlder,
}: {
  canLoadOlder: boolean;
  composer: ReactNode;
  entries: readonly AgentEntry[];
  followTailRequest?: number;
  inactiveMessage: string;
  isLoadingOlder: boolean;
  mode: AgentTerminalMode;
  /** The agent is producing the latest turn; drives live work groups and the activity line. */
  running?: boolean;
  /** Controller phase, such as "Running exec_command", shown while running. */
  activity?: string;
  showToolCalls?: boolean;
  renderTool?(tool: ToolActivity): ReactNode;
  userLabel?(entry: Extract<AgentEntry, { kind: "user" }>): string | undefined;
  status: AgentStatus;
  voiceEntries?: readonly VoiceTerminalEntry[];
  welcome?: string;
  onLoadOlder(): Promise<boolean>;
}) {
  const transcript = useRef<HTMLDivElement>(null);
  const followTail = useRef(true);
  const [showLatest, setShowLatest] = useState(false);
  const handledFollowTailRequest = useRef(followTailRequest);
  const loadOlderArmed = useRef(true);
  const loadOlderPending = useRef(false);
  const touchY = useRef<number | undefined>(undefined);
  const preserveScroll = useRef<{
    anchors: ReadingAnchor[];
    firstEntryId: string | undefined;
  } | undefined>(undefined);
  const transcriptEntries = useMemo(
    () => interleaveTranscriptEntries(entries, voiceEntries),
    [entries, voiceEntries],
  );
  const visibleWelcome = transcriptEntries.length === 0 ? welcome : undefined;
  // Streaming replaces only the changed tail entry. Reusing unchanged rows and
  // role labels lets completed rows skip rendering on every token.
  const rows = useReusedRows(transcriptEntries);
  const roles = useStableRoles(entries);
  const [turnStartedAt, setTurnStartedAt] = useState(() => Date.now());
  useEffect(() => { if (running) setTurnStartedAt(Date.now()); }, [running]);
  const lastRow = rows.at(-1);
  const streamingAnswer = lastRow?.type === "entry" && lastRow.entry.kind === "assistant" && lastRow.entry.streaming;

  useLayoutEffect(() => {
    const element = transcript.current;
    if (!element || mode === "hidden") return;
    if (handledFollowTailRequest.current !== followTailRequest) {
      handledFollowTailRequest.current = followTailRequest;
      followTail.current = true;
      preserveScroll.current = undefined;
    }
    const preserved = preserveScroll.current;
    // Live tokens must not consume the pending prepend anchor. Restore when
    // history arrives, using row position rather than
    // total height, which also includes output streaming below the reader.
    // The promise may resolve before the controller publishes its next frame.
    // Live voice rows can precede durable entries, so compare the durable head.
    if (preserved && preserved.firstEntryId !== entries[0]?.id) {
      preserveScroll.current = undefined;
      const anchor = preserved.anchors.find(({ element: row }) => row.isConnected && element.contains(row));
      if (anchor) element.scrollTop += anchor.element.getBoundingClientRect().top
        - element.getBoundingClientRect().top - anchor.top;
    } else if (visibleWelcome) element.scrollTop = 0;
    else if (followTail.current) element.scrollTop = element.scrollHeight;
  }, [entries, followTailRequest, mode, transcriptEntries, visibleWelcome]);

  useEffect(() => {
    const element = transcript.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      if (visibleWelcome) element.scrollTop = 0;
      else if (followTail.current) element.scrollTop = element.scrollHeight;
    });
    const content = element.firstElementChild;
    observer.observe(element);
    if (content) observer.observe(content);
    return () => observer.disconnect();
  }, [visibleWelcome]);

  function loadOlderNearTop(element: HTMLElement, upwardGesture = false) {
    if (mode === "hidden") return;
    const lineHeight = Number.parseFloat(getComputedStyle(element).lineHeight) || 22;
    if (element.scrollTop > lineHeight * 12) {
      loadOlderArmed.current = true;
      return;
    }
    // An initial tail scroll never requests history. Explicit upward gestures
    // also work when a short page cannot scroll beyond the loading threshold.
    if ((!upwardGesture && followTail.current) || !loadOlderArmed.current
      || loadOlderPending.current || isLoadingOlder || !canLoadOlder) return;
    loadOlderArmed.current = false;
    loadOlderPending.current = true;
    followTail.current = false;
    const request = { anchors: readingAnchors(element), firstEntryId: entries[0]?.id };
    preserveScroll.current = request;
    void Promise.resolve().then(onLoadOlder).then((loaded) => {
      if (loaded) loadOlderArmed.current = true;
      else if (preserveScroll.current === request) preserveScroll.current = undefined;
    }).catch(() => {
      if (preserveScroll.current === request) preserveScroll.current = undefined;
    }).finally(() => {
      loadOlderPending.current = false;
    });
  }

  function rearmShortHistory(element: HTMLElement) {
    const lineHeight = Number.parseFloat(getComputedStyle(element).lineHeight) || 22;
    // Short content cannot physically leave the threshold. A gesture away from
    // the top followed by another upward gesture is still an explicit retry.
    if (!loadOlderPending.current && element.scrollHeight - element.clientHeight <= lineHeight * 12) {
      loadOlderArmed.current = true;
    }
  }

  return (
    <section
      className={`agent-terminal-shell is-dom is-${mode}`}
      aria-label="Live Nanocodex terminal"
    >
      <div
        ref={transcript}
        className="agent-dom-transcript"
        role="log"
        aria-live="off"
        onWheel={(event) => {
          // Scrolling up releases the tail at once, before a streamed resize can pull the reader back.
          if (event.deltaY < 0 && event.currentTarget.scrollTop > 0) followTail.current = false;
          if (event.deltaY < 0) loadOlderNearTop(event.currentTarget, true);
          else if (event.deltaY > 0) rearmShortHistory(event.currentTarget);
        }}
        onTouchStart={(event) => { touchY.current = event.touches[0]?.clientY; }}
        onTouchMove={(event) => {
          const y = event.touches[0]?.clientY;
          if (y !== undefined && touchY.current !== undefined && y > touchY.current) {
            if (event.currentTarget.scrollTop > 0) followTail.current = false;
            loadOlderNearTop(event.currentTarget, true);
          } else if (y !== undefined && touchY.current !== undefined && y < touchY.current) {
            rearmShortHistory(event.currentTarget);
          }
          touchY.current = y;
        }}
        onTouchEnd={() => { touchY.current = undefined; }}
        onTouchCancel={() => { touchY.current = undefined; }}
        onScroll={(event) => {
          if (mode === "hidden") return;
          const element = event.currentTarget;
          followTail.current = element.scrollHeight - element.scrollTop - element.clientHeight < 48;
          setShowLatest(!followTail.current);
          if (preserveScroll.current) preserveScroll.current.anchors = readingAnchors(element);
          loadOlderNearTop(element);
        }}
      >
        <div className="agent-dom-transcript-inner">
          {visibleWelcome ? <article className="agent-terminal-markdown is-assistant is-welcome">
            <RichMarkdown>
              {visibleWelcome}
            </RichMarkdown>
          </article> : null}
          {rows.map((row, index) => (
            <TranscriptRowView key={row.id} row={row} live={running && index === rows.length - 1}
              roles={roles} showToolCalls={showToolCalls} renderTool={renderTool} userLabel={userLabel} />
          ))}
          {running && !streamingAnswer ? <LiveStatus activity={readableActivity(activity)} startedAt={turnStartedAt} /> : null}
          {status !== "ready" && inactiveMessage ? (
            status === "error" ? <ErrorNotice text={inactiveMessage} className="agent-terminal-status" />
              : <p className="agent-terminal-status" role="status">{looksLikeRawError(inactiveMessage) ? presentAgentError(inactiveMessage).summary : inactiveMessage}</p>
          ) : null}
          <div className="agent-transcript-keyboard-spacer" aria-hidden="true" />
        </div>
      </div>
      <div className="agent-composer-dock">
        {showLatest ? <button className="agent-jump-latest" type="button" aria-label="Jump to latest response" title="Jump to latest response" onClick={() => {
          const element = transcript.current;
          if (!element) return;
          followTail.current = true;
          preserveScroll.current = undefined;
          element.scrollTo({ top: element.scrollHeight, behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" });
        }}><ArrowDown aria-hidden="true" /></button> : null}
        {composer}
      </div>
    </section>
  );
}

export function interleaveTranscriptEntries(
  entries: readonly AgentEntry[],
  voiceEntries: readonly VoiceTerminalEntry[],
): readonly TerminalEntry[] {
  const anchored = new Map<string | undefined, VoiceTerminalEntry[]>();
  const liveVoiceByKey = new Map<string, VoiceTerminalEntry[]>();
  for (const entry of voiceEntries) {
    const group = anchored.get(entry.afterEntryId) ?? [];
    group.push(entry);
    anchored.set(entry.afterEntryId, group);
    const key = voiceEntryKey(entry);
    const matching = liveVoiceByKey.get(key) ?? [];
    matching.push(entry);
    liveVoiceByKey.set(key, matching);
  }

  const merged: TerminalEntry[] = [];
  const matchedLiveVoiceIds = new Set<string>();
  const retainedVoiceIds = new Set<string>();
  for (const voiceEntry of anchored.get(undefined) ?? []) {
    appendVoiceEntry(merged, voiceEntry);
    retainedVoiceIds.add(voiceEntry.id);
  }
  for (const entry of entries) {
    const durableVoiceEntries = projectRealtimeTranscript(entry);
    if (durableVoiceEntries !== undefined) {
      for (const voiceEntry of durableVoiceEntries) {
        const key = voiceEntryKey(voiceEntry);
        const liveEntry = liveVoiceByKey.get(key)?.find(({ id }) => !matchedLiveVoiceIds.has(id));
        if (liveEntry) {
          matchedLiveVoiceIds.add(liveEntry.id);
          if (!retainedVoiceIds.has(liveEntry.id)) {
            appendVoiceEntry(merged, liveEntry);
            retainedVoiceIds.add(liveEntry.id);
          }
        } else merged.push(voiceEntry);
      }
      for (const voiceEntry of anchored.get(entry.id) ?? []) {
        if (retainedVoiceIds.has(voiceEntry.id)) continue;
        appendVoiceEntry(merged, voiceEntry);
        retainedVoiceIds.add(voiceEntry.id);
      }
      continue;
    }
    merged.push(entry);
    for (const voiceEntry of anchored.get(entry.id) ?? []) {
      if (retainedVoiceIds.has(voiceEntry.id)) continue;
      appendVoiceEntry(merged, voiceEntry);
      retainedVoiceIds.add(voiceEntry.id);
    }
  }

  for (const entry of voiceEntries) {
    if (!retainedVoiceIds.has(entry.id) && entry.afterEntryId === undefined) {
      appendVoiceEntry(merged, entry);
    }
  }
  return merged;
}

function appendVoiceEntry(entries: TerminalEntry[], voiceEntry: VoiceTerminalEntry) {
  entries.push(voiceEntry);
}

function isVoiceEntry(entry: TerminalEntry): entry is VoiceTerminalEntry {
  return "source" in entry && entry.source === "voice";
}

function normalizeTranscript(text: string): string {
  return text.trim().replace(/\s+/g, " ");
}

function voiceEntryKey(entry: Pick<VoiceTerminalEntry, "kind" | "text">): string {
  return `${entry.kind}:${normalizeTranscript(entry.text)}`;
}

function projectRealtimeTranscript(entry: AgentEntry): VoiceTerminalEntry[] | undefined {
  if (entry.kind !== "user") return undefined;
  const envelope = entry.text.trimStart();
  if (/^<(?:realtime_conversation|source|soruce|startup_context)(?:\s|>|$)/.test(envelope)) return [];
  if (!/^<realtime_delegation(?:\s|>|$)/.test(envelope)) return undefined;
  const encoded = /<transcript_delta>([\s\S]*?)<\/transcript_delta>/.exec(envelope)?.[1];
  if (!encoded?.trim()) {
    const input = /<input>([\s\S]*?)<\/input>/.exec(envelope)?.[1];
    const onlySpeechSources = ["source", "soruce"].every(name =>
      !envelope.includes(`<${name}>`) || new RegExp(`<${name}>([\\s\\S]*?)</${name}>`).exec(envelope)?.[1]?.trim() === "voice_bootstrap");
    if (!input?.trim() || !onlySpeechSources || (envelope.includes("<transcript_delta>") && encoded === undefined)) return [];
    return [{ id: `${entry.id}-voice-0`, kind: "user", source: "voice", streaming: false, text: decodeRealtimeText(input) }];
  }

  const projected: Array<{ kind: "user" | "assistant"; text: string }> = [];
  const unlabelled: string[] = [];
  for (const line of decodeRealtimeText(encoded).split("\n")) {
    const turn = /^(user|assistant):\s?(.*)$/.exec(line);
    if (turn) {
      projected.push({ kind: turn[1] as "user" | "assistant", text: turn[2] ?? "" });
    } else if (projected.length > 0) {
      projected[projected.length - 1]!.text += `\n${line}`;
    } else unlabelled.push(line);
  }
  const unlabelledText = unlabelled.join("\n").trim();
  if (unlabelledText) projected.unshift({ kind: "assistant", text: unlabelledText });
  return projected
    .filter(({ text }) => text.trim().length > 0)
    .map(({ kind, text }, index) => ({
      id: `${entry.id}-voice-${index}`,
      kind,
      source: "voice",
      streaming: false,
      text,
    }));
}

function decodeRealtimeText(text: string): string {
  return text
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

function useReusedRows<E extends { id: string; kind: string }>(entries: readonly E[], nested = false): TranscriptRow<E>[] {
  const committed = useRef<readonly TranscriptRow<E>[]>([]);
  const rows = useMemo(() => groupTranscript(entries, committed.current, nested), [entries, nested]);
  useLayoutEffect(() => { committed.current = rows; }, [rows]);
  return rows;
}

function useStableRoles(entries: readonly AgentEntry[]): ReadonlyMap<number, string> {
  const committed = useRef<ReadonlyMap<number, string>>(new Map());
  const roles = useMemo(() => {
    const next = subagentRoles(entries);
    const previous = committed.current;
    return next.size === previous.size && [...next].every(([id, role]) => previous.get(id) === role) ? previous : next;
  }, [entries]);
  useLayoutEffect(() => { committed.current = roles; }, [roles]);
  return roles;
}

type RowProps = {
  showToolCalls: boolean;
  renderTool?: ((tool: ToolActivity) => ReactNode) | undefined;
  userLabel?: ((entry: Extract<AgentEntry, { kind: "user" }>) => string | undefined) | undefined;
};

const TranscriptRowView = memo(function TranscriptRowView({ row, live, roles, ...props }: RowProps & {
  row: TranscriptRow<TerminalEntry>;
  live: boolean;
  roles: ReadonlyMap<number, string>;
}) {
  if (row.type === "entry") return <TerminalEntryView entry={row.entry} {...props} />;
  if (row.type === "work") return <WorkGroup entries={row.entries} live={live} showToolCalls={props.showToolCalls}
    renderTool={props.renderTool} renderEntry={(entry) => <TerminalEntryView entry={entry} {...props} />} />;
  return <SubagentRowView row={row} live={live} roles={roles} {...props} />;
});

function SubagentRowView({ row, live, roles, ...props }: RowProps & {
  row: Extract<TranscriptRow<TerminalEntry>, { type: "agent" }>;
  live: boolean;
  roles: ReadonlyMap<number, string>;
}) {
  const nested = useReusedRows<TerminalEntry>(row.entries, true);
  return <SubagentBlock agentId={row.agentId} role={roles.get(row.agentId)} entries={row.entries} live={live}>
    {nested.map((child, index) => <TranscriptRowView key={child.id} row={child} live={live && index === nested.length - 1} roles={roles} {...props} />)}
  </SubagentBlock>;
}

const TerminalEntryView = memo(function TerminalEntryView({
  entry,
  showToolCalls,
  renderTool,
  userLabel,
}: RowProps & { entry: TerminalEntry }) {
  const voice = isVoiceEntry(entry);
  if (entry.kind === "user") return <UserMessage entry={entry} voice={voice}
    label={voice ? "voice" : userLabel?.(entry as Extract<AgentEntry, { kind: "user" }>) || ("author" in entry && entry.author === "guest" ? "Guest" : undefined)} />;
  if (entry.kind === "assistant" || entry.kind === "reasoning") return <AssistantMessage entry={entry} voice={voice} />;
  if (entry.kind === "error") return <ErrorNotice text={entry.text} className="agent-terminal-error" />;
  if (entry.kind === "plan") return <ol className="agent-terminal-plan">
    {entry.update.plan.map((step, index) => <li key={`${index}-${step.step}`} data-status={step.status}>
      <span aria-hidden="true">{step.status === "completed" ? "✓" : step.status === "in_progress" ? "→" : "·"}</span>
      {step.step}
    </li>)}
  </ol>;
  // Tools are normally grouped; this path only covers a lone tool outside a group.
  if (entry.kind === "tool") return <WorkGroup entries={[entry]} live={false} showToolCalls={showToolCalls}
    renderTool={renderTool} renderEntry={() => null} />;
  return null;
});

type ProseEntry = Readonly<{ kind: "assistant" | "reasoning" | "user"; text: string; streaming: boolean }>;

function AssistantMessage({ entry, voice }: { entry: ProseEntry; voice: boolean }) {
  // Only a settled answer that is entirely a payload envelope is reinterpreted.
  const settled = entry.kind === "assistant" && !entry.streaming && /^\s*[{[]/.test(entry.text);
  const shown = useMemo(() => settled ? presentAssistantText(entry.text) : undefined, [settled, entry.text]);
  if (shown?.kind === "error") return <ErrorNotice text={shown.text} className="agent-terminal-error" />;
  const text = shown?.text ?? entry.text;
  return <article className={`agent-terminal-markdown is-${entry.kind}`} data-source={voice ? "voice" : undefined}>
    {voice ? <span className="agent-terminal-entry-label">voice</span> : null}
    {entry.kind === "reasoning" ? <span className="agent-terminal-entry-label">thinking{entry.streaming ? "…" : ""}</span> : null}
    <RichMarkdown streaming={entry.streaming}>{text}</RichMarkdown>
    {entry.kind === "assistant" && !entry.streaming && text.trim() ? <ResponseActions text={text} /> : null}
  </article>;
}

function ResponseActions({ text }: { text: string }) {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const timer = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(timer);
  }, [state]);
  return <div className="agent-response-actions">
    <button type="button" aria-label={state === "copied" ? "Copied response" : "Copy response"} title={state === "copied" ? "Copied" : "Copy response"} onClick={async () => {
      try { await navigator.clipboard.writeText(text); setState("copied"); }
      catch { setState("error"); }
    }}>{state === "copied" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}</button>
    <span role="status">{state === "copied" ? "Copied" : state === "error" ? "Couldn’t copy. Select the text to copy it." : ""}</span>
  </div>;
}

type UserAttachment = Readonly<{ kind: "image" | "file" | "document" | "audio"; name?: string | undefined; url?: string | undefined }>;
const ATTACHMENT_MARKER = /^\[(image|audio|file|document)(?::\s*(.+))?\]$/;
const ATTACHED_FILE_BLOCK = /<attached_file name="([^"]*)"[^>]*>[\s\S]*?<\/attached_file>/g;

/** Splits trailing attachment markers (and any inlined file envelopes) out of user prose. */
export function splitUserAttachments(text: string): { text: string; attachments: UserAttachment[] } {
  const attachments: UserAttachment[] = [];
  let body = text.replace(ATTACHED_FILE_BLOCK, (_match, name: string) => {
    attachments.push({ kind: "file", name: name.replaceAll("&quot;", '"').replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&") });
    return "";
  });
  const lines = body.split("\n");
  const trailing: UserAttachment[] = [];
  while (lines.length) {
    const marker = ATTACHMENT_MARKER.exec(lines.at(-1)!.trim());
    if (!marker) break;
    lines.pop();
    trailing.unshift({ kind: marker[1] as UserAttachment["kind"], ...(marker[2] ? { name: marker[2] } : {}) });
  }
  body = lines.join("\n").trimEnd();
  return { text: body, attachments: [...attachments, ...trailing] };
}

function UserMessage({ entry, label, voice }: { entry: TerminalEntry & { text: string }; label?: string | undefined; voice: boolean }) {
  const { text, attachments: markers } = useMemo(() => splitUserAttachments(entry.text), [entry.text]);
  const local = "attachments" in entry ? entry.attachments : undefined;
  // Local previews carry thumbnails; history only has markers in the same order.
  const attachments: UserAttachment[] = local?.length ? local.map((item, index) => ({ ...markers[index], ...item })) : markers;
  return <div className="agent-terminal-user" data-source={voice ? "voice" : undefined}>
    {label ? <span className="agent-terminal-entry-label">{label}</span> : null}
    {text ? <p className="agent-terminal-user-text">{text}</p> : null}
    {attachments.length ? <ul className="agent-user-attachments" aria-label="Attachments">
      {attachments.map((item, index) => <li key={index} className={`is-${item.kind}`}>
        {item.kind === "image" && item.url ? <img src={item.url} alt={item.name ?? "Attached image"} />
          : <>{item.kind === "image" ? <ImageIcon aria-hidden="true" /> : <FileText aria-hidden="true" />}
            <span>{item.name ?? (item.kind === "image" ? "Image" : item.kind === "audio" ? "Audio" : item.kind === "document" ? "Document" : "File")}</span></>}
      </li>)}
    </ul> : null}
  </div>;
}

/** Tidy inline notice: one readable sentence; raw source only behind Details. */
function ErrorNotice({ text, className }: { text: string; className: string }) {
  const { summary, detail } = useMemo(() => presentAgentError(text), [text]);
  return <div className={`${className} agent-error-notice`} role="alert">
    <CircleAlert className="agent-error-notice-icon" aria-hidden="true" />
    <div className="agent-error-notice-body">
      <p>{summary}</p>
      {detail ? <details className="agent-tool-protocol agent-error-notice-details">
        <summary>Details</summary>
        <pre>{detail}</pre>
      </details> : null}
    </div>
  </div>;
}
