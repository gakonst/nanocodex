"use client";

import { memo, useMemo, useState, type ReactNode } from "react";
import { projectToolOutput, type AgentEntry, type GeneratedOutput, type ToolActivity } from "nanocodex-react/agent";
import { Bot, ChevronRight } from "lucide-react";
import { GeneratedOutputView } from "./GeneratedOutputView.js";
import { RichMarkdown } from "./RichMarkdown.js";
import { KindIcon, StatusIcon, ToolRow, formatElapsed, useNow } from "./ToolActivityView.js";
import { modelTool, toolKind } from "./toolModel.js";
import { presentTool } from "./toolPresentation.js";

type Entry = AgentEntry;
type ToolEntry = Extract<AgentEntry, { kind: "tool" }>;
type ReasoningEntry = Extract<AgentEntry, { kind: "reasoning" }>;

export type TranscriptRow<E> =
  | Readonly<{ type: "entry"; id: string; entry: E }>
  | Readonly<{ type: "work"; id: string; entries: readonly (ToolEntry | ReasoningEntry)[] }>
  | Readonly<{ type: "agent"; id: string; agentId: number; entries: readonly Entry[] }>;

function isVoice(entry: unknown): boolean {
  return typeof entry === "object" && entry !== null && (entry as { source?: unknown }).source === "voice";
}

/**
 * Consecutive tool and thinking entries become one work group. Subagent output
 * is gathered per turn and agent so a child's activity stays together.
 */
export function groupTranscript<E extends { id: string; kind: string }>(
  entries: readonly E[],
  /** Rows from the previous grouping; unchanged rows keep their identity so memoized rows skip rendering. */
  previous: readonly TranscriptRow<E>[] = [],
  /** Groups one subagent's own entries, ignoring their agent identity. */
  nested = false,
): TranscriptRow<E>[] {
  const rows: Array<TranscriptRow<E> & { entries?: unknown[] }> = [];
  const agents = new Map<string, { type: "agent"; id: string; agentId: number; entries: Entry[] }>();
  for (const entry of entries) {
    if (!isVoice(entry)) {
      const durable = entry as unknown as Entry;
      const agentId = nested ? undefined : durable.responseIdentity?.agentId;
      if (agentId != null) {
        const key = `${durable.turnId ?? ""}:${agentId}`;
        let row = agents.get(key);
        if (!row) {
          row = { type: "agent", id: `agent-${key}-${entry.id}`, agentId, entries: [] };
          agents.set(key, row);
          rows.push(row);
        }
        row.entries.push(durable);
        continue;
      }
      if (durable.kind === "tool" || durable.kind === "reasoning") {
        const last = rows.at(-1);
        if (last?.type === "work") (last.entries as Entry[]).push(durable);
        else rows.push({ type: "work", id: `work-${entry.id}`, entries: [durable as ToolEntry | ReasoningEntry] });
        continue;
      }
    }
    rows.push({ type: "entry", id: entry.id, entry });
  }
  if (!previous.length) return rows;
  const prior = new Map(previous.map(row => [row.id, row]));
  return rows.map(row => {
    const old = prior.get(row.id);
    return old && sameRow(old, row) ? old : row;
  });
}

function sameRow<E>(left: TranscriptRow<E>, right: TranscriptRow<E>): boolean {
  if (left.type === "entry" || right.type === "entry") {
    return left.type === right.type && (left as { entry: E }).entry === (right as { entry: E }).entry;
  }
  return left.type === right.type && left.entries.length === right.entries.length
    && left.entries.every((entry, index) => entry === right.entries[index]);
}

function walk(tool: ToolActivity, visit: (tool: ToolActivity) => void) {
  visit(tool);
  tool.children.forEach(child => walk(child, visit));
}

export function hasRunning(tool: ToolActivity): boolean {
  return tool.status === "running" || tool.children.some(hasRunning);
}

/** Human summary such as "3 commands · edited 2 files · 1 failed". */
export function summarizeWork(tools: readonly ToolActivity[]): { parts: string[]; failed: number } {
  let commands = 0, code = 0, searches = 0, browser = 0, agents = 0, images = 0, other = 0, failed = 0;
  const read = new Set<string>();
  const edited = new Set<string>();
  for (const root of tools) walk(root, (tool) => {
    if (tool.status === "failed") failed++;
    const kind = toolKind(tool);
    if (kind === "code") { if (!tool.children.length) code++; return; }
    if (kind === "command") commands++;
    else if (kind === "read") read.add(modelTool(tool).target ?? tool.callId);
    else if (kind === "write" || kind === "edit" || kind === "patch") {
      const diffs = modelTool(tool).diffs ?? [];
      if (diffs.length) diffs.forEach(diff => edited.add(diff.path)); else edited.add(tool.callId);
    } else if (kind === "search") searches++;
    else if (kind === "browser") browser++;
    else if (kind === "subagent") agents++;
    else if (kind === "image") images++;
    else other++;
  });
  const plural = (count: number, noun: string, many = `${noun}s`) => `${count} ${count === 1 ? noun : many}`;
  const parts = [
    commands ? plural(commands, "command") : "",
    edited.size ? `edited ${plural(edited.size, "file")}` : "",
    read.size ? `read ${plural(read.size, "file")}` : "",
    searches ? plural(searches, "search", "searches") : "",
    browser ? plural(browser, "browser action") : "",
    code ? plural(code, "code run") : "",
    agents ? plural(agents, "agent action") : "",
    images ? plural(images, "image") : "",
    other ? plural(other, "other tool") : "",
  ].filter(Boolean);
  return { parts, failed };
}

/** Wall time spanned by a group, using persisted timestamps when available. */
export function workDuration(tools: readonly ToolActivity[], now: number, seenAt: number, active: boolean): number | undefined {
  const starts: number[] = [];
  const ends: number[] = [];
  let summed = 0;
  for (const root of tools) walk(root, (tool) => {
    if (tool.startedAtMs !== undefined) {
      starts.push(tool.startedAtMs);
      if (tool.durationNs !== undefined) ends.push(tool.startedAtMs + tool.durationNs / 1e6);
    }
  });
  for (const tool of tools) summed += (tool.durationNs ?? 0) / 1e6;
  const start = starts.length ? Math.min(...starts) : undefined;
  if (active) return now - Math.min(start ?? seenAt, now);
  if (start !== undefined && ends.length) return Math.max(...ends) - start;
  return summed > 0 ? summed : undefined;
}

/** Previews, interactive client cards, and generated media stay visible when activity collapses. */
export const ToolArtifacts = memo(function ToolArtifacts({ tool, renderTool }: { tool: ToolActivity; renderTool?: ((tool: ToolActivity) => ReactNode) | undefined }) {
  return <>
    <ToolPreviews tool={tool} />
    {renderToolTree(tool, renderTool)}
    <GeneratedOutputView items={generatedToolOutput(tool)} />
  </>;
});

function ToolPreviews({ tool }: { tool: ToolActivity }) {
  const urls = new Set<string>();
  walk(tool, (activity) => { const url = presentTool(activity).previewUrl; if (url) urls.add(url); });
  return <>{[...urls].map(url => <a className="agent-terminal-preview-card" href={url}
    target="_blank" rel="noopener noreferrer" key={url}>
    <span className="agent-terminal-preview-icon" aria-hidden="true">↗</span>
    <span><strong>Open preview</strong><span>{new URL(url).host}</span></span>
    <span className="agent-terminal-preview-action">View</span>
  </a>)}</>;
}

function renderToolTree(tool: ToolActivity, render: ((tool: ToolActivity) => ReactNode) | undefined): ReactNode {
  if (!render) return null;
  return <>{render(tool)}{tool.children.map(child => <div key={child.callId}>{renderToolTree(child, render)}</div>)}</>;
}

export function generatedToolOutput(tool: ToolActivity): GeneratedOutput[] {
  const items: GeneratedOutput[] = [];
  const seen = new Set<string>();
  walk(tool, (activity) => {
    const output = activity.generatedOutput ?? projectToolOutput(activity.images?.map((image_url, index) => ({
      type: "input_image", image_url, name: `${presentTool(activity).title} result ${index + 1}`,
    })));
    const emitsText = ["exec", "wait"].includes(activity.name.split(".").at(-1) ?? "");
    for (const item of output) {
      if (item.kind === "text" && !emitsText) continue;
      const key = item.kind === "text" ? `text:${item.text}` : `${item.kind}:${item.url}`;
      if (!seen.has(key)) { seen.add(key); items.push(item); }
    }
  });
  return items;
}

function lastLine(text: string): string {
  const lines = text.split("\n").map(line => line.replace(/[*_`#>]/g, "").trim()).filter(Boolean);
  return lines.at(-1) ?? "";
}

/** Thinking summary row: live last line while streaming, full text on demand. */
export const ThinkingRow = memo(function ThinkingRow({ entry }: { entry: ReasoningEntry }) {
  const [open, setOpen] = useState(false);
  const preview = entry.streaming ? lastLine(entry.text) : entry.text.split("\n").map(line => line.replace(/[*_`#>]/g, "").trim()).find(Boolean) ?? "";
  return <div className={`agent-tool-row is-thinking${entry.streaming ? " is-running" : ""}`} data-tool-kind="thinking">
    <details open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        {entry.streaming ? <StatusIcon status="thinking" /> : <span className="agent-tool-status-icon" aria-hidden="true" />}
        <KindIcon kind="thinking" />
        <span className="agent-tool-title">
          <strong>{entry.streaming ? "Thinking" : "Thought"}</strong>
          {preview ? <span className="agent-tool-detail">{preview}</span> : null}
        </span>
        <span className="agent-tool-meta"><ChevronRight className="agent-tool-chevron" aria-hidden="true" /></span>
      </summary>
      {open ? <div className="agent-tool-body agent-tool-thinking"><RichMarkdown streaming={entry.streaming}>{entry.text}</RichMarkdown></div> : null}
    </details>
  </div>;
});

function useDisclosure(automatic: boolean) {
  const [chosen, setChosen] = useState<boolean>();
  const open = chosen ?? automatic;
  return {
    open,
    onToggle(event: { currentTarget: HTMLDetailsElement }) {
      if (event.currentTarget.open !== open) setChosen(event.currentTarget.open);
    },
  };
}

export const WorkGroup = memo(function WorkGroup({ entries, live, showToolCalls, renderTool, renderEntry }: {
  entries: readonly (ToolEntry | ReasoningEntry)[];
  /** The turn is still producing this group. */
  live: boolean;
  showToolCalls: boolean;
  renderTool?: ((tool: ToolActivity) => ReactNode) | undefined;
  renderEntry(entry: Entry): ReactNode;
}) {
  const tools = useMemo(() => entries.flatMap(entry => entry.kind === "tool" ? [entry.tool] : []), [entries]);
  // The live clock re-renders an active group every second; its summary only changes with entries.
  const summary = useMemo(() => summarizeWork(tools), [tools]);
  const thinking = entries.some(entry => entry.kind === "reasoning" && entry.streaming);
  const active = tools.some(hasRunning) || thinking || live;
  const now = useNow(active);
  const [seenAt] = useState(() => Date.now());
  const disclosure = useDisclosure(active);
  const artifacts = tools.map(tool => <div className="agent-terminal-tool-entry" key={tool.callId}><ToolArtifacts tool={tool} renderTool={renderTool} /></div>);
  if (!showToolCalls) return <div className="agent-work">
    {entries.map(entry => entry.kind === "reasoning" ? <div key={entry.id}>{renderEntry(entry)}</div> : null)}
    {artifacts}
  </div>;
  const items = entries.map(entry => entry.kind === "tool"
    ? <ToolRow key={entry.id} tool={entry.tool} />
    : <ThinkingRow key={entry.id} entry={entry} />);
  if (entries.length === 1) return <div className="agent-work is-single">{items}{artifacts}</div>;
  const { parts, failed } = summary;
  const duration = workDuration(tools, now, seenAt, active);
  const status = active ? "running" : failed ? "failed" : "completed";
  const heading = active ? "Working" : "Worked";
  return <div className="agent-work">
    <details className={`agent-work-group is-${status}`} open={disclosure.open} onToggle={disclosure.onToggle}>
      <summary>
        <StatusIcon status={status} />
        <span className="agent-work-heading">
          <strong>{heading}{duration !== undefined && duration >= 1000 ? ` for ${formatElapsed(duration)}` : active ? "…" : ""}</strong>
          {parts.map(part => <span key={part}>{part}</span>)}
          {failed ? <span className="is-failed">{failed} failed</span> : null}
        </span>
        <ChevronRight className="agent-tool-chevron" aria-hidden="true" />
      </summary>
      <div className="agent-work-body">{items}</div>
    </details>
    {artifacts}
  </div>;
});

/** Subagent roles announced by spawn results, used to label child activity. */
export function subagentRoles(entries: readonly { kind: string }[]): ReadonlyMap<number, string> {
  const roles = new Map<number, string>();
  for (const entry of entries) {
    if (entry.kind !== "tool") continue;
    walk((entry as ToolEntry).tool, (tool) => {
      if (!/spawn_agent$/.test(tool.name)) return;
      try {
        const output = JSON.parse(tool.output ?? tool.result ?? "") as { agent_id?: unknown; role?: unknown };
        const input = JSON.parse(tool.input ?? tool.arguments ?? "{}") as { role?: unknown };
        const role = typeof output.role === "string" ? output.role : typeof input.role === "string" ? input.role : undefined;
        if (typeof output.agent_id === "number" && role) roles.set(output.agent_id, role);
      } catch { /* Incomplete spawn results carry no identity yet. */ }
    });
  }
  return roles;
}

export const SubagentBlock = memo(function SubagentBlock({ agentId, role, entries, live, children }: {
  agentId: number;
  role?: string | undefined;
  entries: readonly Entry[];
  live: boolean;
  children: ReactNode;
}) {
  const tools = entries.flatMap(entry => entry.kind === "tool" ? [entry.tool] : []);
  const streaming = entries.some(entry => (entry.kind === "assistant" || entry.kind === "reasoning") && entry.streaming);
  const active = tools.some(hasRunning) || streaming || live;
  const { parts, failed } = summarizeWork(tools);
  const errors = entries.filter(entry => entry.kind === "error").length;
  const answer = [...entries].reverse().find(entry => entry.kind === "assistant");
  const disclosure = useDisclosure(false);
  const status = active ? "running" : failed || errors ? "failed" : "completed";
  const preview = answer && answer.kind === "assistant" ? answer.text.split("\n").map(line => line.trim()).find(Boolean) : undefined;
  return <details className={`agent-terminal-child agent-subagent is-${status}`} data-agent-id={agentId}
    open={disclosure.open} onToggle={disclosure.onToggle}>
    <summary>
      <StatusIcon status={status} />
      <Bot className="agent-tool-kind-icon" aria-hidden="true" />
      <span className="agent-work-heading">
        <strong>{role ? `${role} · Agent ${agentId}` : `Agent ${agentId}`}</strong>
        <span>{active ? "working" : status === "failed" ? "needs attention" : "finished"}</span>
        {parts.map(part => <span key={part}>{part}</span>)}
        {preview && !disclosure.open ? <span className="agent-subagent-preview">{preview.slice(0, 160)}</span> : null}
      </span>
      <ChevronRight className="agent-tool-chevron" aria-hidden="true" />
    </summary>
    {disclosure.open ? <div className="agent-subagent-body">{children}</div> : null}
  </details>;
});

/** Live turn indicator with elapsed time. Only phase changes are announced. */
export function LiveStatus({ activity, startedAt }: { activity: string; startedAt: number }) {
  const now = useNow(true);
  return <div className="agent-live-status">
    <StatusIcon status="running" />
    <span role="status" aria-live="polite">{activity}</span>
    <span className="agent-live-elapsed" aria-hidden="true">{formatElapsed(now - startedAt)}</span>
  </div>;
}

/** Turns controller phases such as "Running exec_command" into readable activity. */
export function readableActivity(status: string | undefined): string {
  if (!status) return "Working…";
  const running = /^Running (.+)$/.exec(status);
  if (running) {
    const tool = { callId: "status", name: running[1]!, arguments: "", status: "running" as const, children: [] };
    const model = modelTool(tool);
    return `${model.kind === "command" ? "Running command" : `Running ${model.label.toLowerCase()}`}…`;
  }
  if (/^(Ready|Cancelled|Turn failed)$/.test(status)) return "Working…";
  return status.endsWith("...") ? `${status.slice(0, -3)}…` : status;
}
