"use client";

import {
  type ComponentProps,
  type ReactNode,
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { AgentEntry, ToolActivity } from "nanocodex-react/agent";
import { ArrowDown, Check, Copy } from "lucide-react";
import { Streamdown } from "streamdown";

import type { AgentStatus, AgentTerminalMode } from "./types.js";
import { boundedToolDetail, presentTool } from "./toolPresentation.js";

export type VoiceTerminalEntry = Readonly<{
  afterEntryId?: string;
  id: string;
  kind: "user" | "assistant";
  source: "voice";
  streaming: false;
  text: string;
}>;

type TerminalEntry = AgentEntry | VoiceTerminalEntry;

export function TerminalTranscriptSurface({
  canLoadOlder,
  composer,
  entries,
  followTailRequest = 0,
  inactiveMessage,
  isLoadingOlder,
  mode,
  showToolCalls = true,
  status,
  voiceEntries = [],
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
  showToolCalls?: boolean;
  status: AgentStatus;
  voiceEntries?: readonly VoiceTerminalEntry[];
  welcome?: string;
  onLoadOlder(): Promise<boolean>;
}) {
  const transcript = useRef<HTMLDivElement>(null);
  const followTail = useRef(true);
  const [showLatest, setShowLatest] = useState(false);
  const handledFollowTailRequest = useRef(followTailRequest);
  const loadOlderArmed = useRef(false);
  const preserveScroll = useRef<{ scrollHeight: number; scrollTop: number } | undefined>(undefined);
  const transcriptEntries = useMemo(
    () => interleaveTranscriptEntries(entries, voiceEntries),
    [entries, voiceEntries],
  );
  const visibleWelcome = transcriptEntries.length === 0 ? welcome : undefined;

  useLayoutEffect(() => {
    const element = transcript.current;
    if (!element) return;
    if (handledFollowTailRequest.current !== followTailRequest) {
      handledFollowTailRequest.current = followTailRequest;
      followTail.current = true;
    }
    const preserved = preserveScroll.current;
    if (preserved) {
      preserveScroll.current = undefined;
      element.scrollTop = preserved.scrollTop + element.scrollHeight - preserved.scrollHeight;
    } else if (visibleWelcome) element.scrollTop = 0;
    else if (followTail.current) element.scrollTop = element.scrollHeight;
  }, [followTailRequest, transcriptEntries, visibleWelcome]);

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
        onScroll={(event) => {
          const element = event.currentTarget;
          followTail.current = element.scrollHeight - element.scrollTop - element.clientHeight < 48;
          setShowLatest(!followTail.current);
          const lineHeight = Number.parseFloat(getComputedStyle(element).lineHeight) || 22;
          const nearTop = element.scrollTop <= lineHeight * 12;
          if (!nearTop) {
            if (!isLoadingOlder) loadOlderArmed.current = true;
            return;
          }
          if (!loadOlderArmed.current || isLoadingOlder || !canLoadOlder) return;
          loadOlderArmed.current = false;
          preserveScroll.current = {
            scrollHeight: element.scrollHeight,
            scrollTop: element.scrollTop,
          };
          void onLoadOlder().then((loaded) => {
            if (!loaded) preserveScroll.current = undefined;
          }).catch(() => {
            preserveScroll.current = undefined;
          });
        }}
      >
        <div className="agent-dom-transcript-inner">
          {visibleWelcome ? <article className="agent-terminal-markdown is-assistant is-welcome">
            <Streamdown components={MARKDOWN_COMPONENTS} controls={false} linkSafety={LINK_SAFETY} mode="static" skipHtml>
              {visibleWelcome}
            </Streamdown>
          </article> : null}
          {transcriptEntries.map((entry) => (
            <TerminalEntryView entry={entry} key={entry.id} showToolCalls={showToolCalls} />
          ))}
          {status !== "ready" && inactiveMessage ? (
            <p className="agent-terminal-status" role={status === "error" ? "alert" : "status"}>
              {inactiveMessage}
            </p>
          ) : null}
          <div className="agent-transcript-keyboard-spacer" aria-hidden="true" />
        </div>
      </div>
      <div className="agent-composer-dock">
        {showLatest ? <button className="agent-jump-latest" type="button" aria-label="Jump to latest response" title="Jump to latest response" onClick={() => {
          const element = transcript.current;
          if (!element) return;
          followTail.current = true;
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
  if (entry.kind !== "user" || !entry.text.startsWith("<realtime_delegation>")) return undefined;
  const encoded = /<transcript_delta>([\s\S]*?)<\/transcript_delta>/.exec(entry.text)?.[1];
  if (!encoded) return [];

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
    .replaceAll("&amp;", "&");
}

const TerminalEntryView = memo(function TerminalEntryView({
  entry,
  showToolCalls,
}: {
  entry: TerminalEntry;
  showToolCalls: boolean;
}) {
  const voice = isVoiceEntry(entry);
  if (entry.kind === "user") return <pre className="agent-terminal-user" data-source={voice ? "voice" : undefined}>
    {voice ? <span className="agent-terminal-entry-label">voice</span> : null}{entry.text}
  </pre>;
  if (entry.kind === "assistant" || entry.kind === "reasoning") return (
    <article className={`agent-terminal-markdown is-${entry.kind}`} data-source={voice ? "voice" : undefined}>
      {voice ? <span className="agent-terminal-entry-label">voice</span> : null}
      {entry.kind === "reasoning" ? <span className="agent-terminal-entry-label">thinking{entry.streaming ? "…" : ""}</span> : null}
      <Streamdown
        caret={entry.streaming ? "block" : undefined}
        components={MARKDOWN_COMPONENTS}
        controls={MARKDOWN_CONTROLS}
        isAnimating={entry.streaming}
        linkSafety={LINK_SAFETY}
        mode={entry.streaming ? "streaming" : "static"}
        skipHtml
      >{entry.text}</Streamdown>
      {entry.kind === "assistant" && !entry.streaming && entry.text.trim() ? <ResponseActions text={entry.text} /> : null}
    </article>
  );
  if (entry.kind === "error") return <p className="agent-terminal-error" role="alert">! {entry.text}</p>;
  if (entry.kind === "plan") return <ol className="agent-terminal-plan">
    {entry.update.plan.map((step, index) => <li key={`${index}-${step.step}`} data-status={step.status}>
      <span aria-hidden="true">{step.status === "completed" ? "✓" : step.status === "in_progress" ? "→" : "·"}</span>
      {step.step}
    </li>)}
  </ol>;
  if (entry.kind === "tool") return showToolCalls ? <TerminalToolView tool={entry.tool} /> : null;
  return null;
});

function ResponseActions({ text }: { text: string }) {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const timer = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(timer);
  }, [state]);
  return <div className="agent-response-actions">
    <button type="button" aria-label={state === "copied" ? "Copied response" : "Copy response"} title={state === "copied" ? "Copied" : "Copy response"} onClick={() => {
      void navigator.clipboard.writeText(text).then(() => setState("copied")).catch(() => setState("error"));
    }}>{state === "copied" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}</button>
    <span role="status">{state === "copied" ? "Copied" : state === "error" ? "Couldn’t copy. Select the text to copy it." : ""}</span>
  </div>;
}

function MarkdownInput({
  node: _node,
  ref: _ref,
  ...props
}: ComponentProps<"input"> & { node?: unknown }) {
  return <input
    {...props}
    aria-label={props["aria-label"] ?? (props.type === "checkbox" ? "Checklist item" : undefined)}
  />;
}

const MARKDOWN_COMPONENTS = { input: MarkdownInput };
const MARKDOWN_CONTROLS = { code: { copy: true, download: false }, table: false, mermaid: false } as const;
const LINK_SAFETY = { enabled: true } as const;

function TerminalToolView({ isChild = false, tool }: { isChild?: boolean; tool: ToolActivity }) {
  const presentation = presentTool(tool);
  const semanticWrapper = tool.name === "exec" && tool.children.length > 0;
  const input = semanticWrapper ? undefined : tool.input ?? tool.arguments;
  const output = semanticWrapper ? undefined : tool.output ?? tool.result;
  const status = tool.status === "completed" ? "Succeeded"
    : tool.status === "running" ? "Running"
      : tool.status === "cancelled" ? "Cancelled" : "Failed";
  return <details
    className={`agent-terminal-tool is-${tool.status}${isChild ? " is-child" : ""}`}
    {...(tool.status === "failed" || tool.status === "cancelled" || tool.children.length > 0
      ? { open: true }
      : {})}
  >
    <summary>
      <span className="agent-terminal-tool-glyph" aria-hidden="true">
        {tool.status === "completed" ? "✓" : tool.status === "running" ? "→" : "!"}
      </span>
      <span className="agent-terminal-tool-heading">
        <strong>{presentation.title}</strong>
        {presentation.subject ? <span>{presentation.subject}</span> : null}
        {presentation.outputSummary ? <span>{presentation.outputSummary}</span> : null}
      </span>
      <span className="agent-terminal-tool-meta">
        {presentation.source ? <span className="agent-terminal-tool-source">{presentation.source}</span> : null}
        <span className="agent-terminal-tool-status" role={tool.status === "running" ? "status" : undefined}>
          {status}
        </span>
        {presentation.duration ? <span>{presentation.duration}</span> : null}
      </span>
    </summary>
    <div className="agent-terminal-tool-body">
      <p className="agent-terminal-tool-wire"><span>Wire name</span> <code>{tool.name}</code></p>
      {presentation.inputDetail || input ? <section className="agent-terminal-tool-detail">
        <h4>{presentation.inputDetail?.label ?? "Input"}</h4>
        <pre>{boundedToolDetail(presentation.inputDetail?.value ?? input ?? "")}</pre>
      </section> : null}
      {presentation.outputDetails?.map((detail) => <section
        className="agent-terminal-tool-detail agent-terminal-tool-result"
        key={detail.label}
      >
        <h4>{detail.label}</h4>
        <pre>{boundedToolDetail(detail.value)}</pre>
      </section>)}
      {!presentation.outputDetails && output ? <section className="agent-terminal-tool-detail agent-terminal-tool-result">
        <h4>Output</h4>
        <pre>{boundedToolDetail(output)}</pre>
      </section> : null}
      {presentation.previewUrl ? <p className="agent-terminal-tool-preview">
        <a href={presentation.previewUrl} rel="noopener noreferrer" target="_blank">Open preview</a>
      </p> : null}
      {!semanticWrapper && tool.images?.length ? <div className="agent-terminal-tool-images">
        {tool.images.map((source, index) => <img
          alt={`${presentation.title} result ${index + 1}`}
          key={`${tool.callId}-image-${index}`}
          loading="lazy"
          src={source}
        />)}
      </div> : null}
      {tool.children.map((child) => <TerminalToolView isChild key={child.callId} tool={child} />)}
    </div>
  </details>;
}
