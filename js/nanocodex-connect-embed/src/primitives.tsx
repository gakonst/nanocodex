"use client";

import {
  createContext, useContext, useEffect, useId, useRef, useState,
  type FormEvent, type HTMLAttributes, type ReactNode,
} from "react";
import {
  generatedOutputUrl, useAgentController,
  type Agent, type AgentControllerSnapshot, type AgentEntry, type GeneratedOutput,
  type ToolActivity, type UseAgentControllerOptions,
} from "nanocodex-react/agent";

export type { Agent, AgentControllerSnapshot, AgentControllerEvent, AgentEntry, GeneratedOutput, ToolActivity, UseAgentControllerOptions } from "nanocodex-react/agent";

export type AgentEmbedContext = Readonly<{
  agent: Agent | undefined;
  controller: AgentControllerSnapshot;
  /** Source/authorization errors supplied by the owner; turn errors stay in entries. */
  error: string | undefined;
  retry: (() => void) | undefined;
}>;
const Context = createContext<AgentEmbedContext | undefined>(undefined);

export type AgentProviderProps = UseAgentControllerOptions & Readonly<{
  agent: Agent | undefined;
  error?: string;
  retry?: () => void;
  children: ReactNode;
}>;

/** One controller per mounted source. Switching the source releases the previous subscription. */
export function AgentProvider({ agent, error, retry, children, ...options }: AgentProviderProps) {
  const controller = useAgentController(agent, options);
  return <Context.Provider value={{ agent, controller, error, retry }}>{children}</Context.Provider>;
}

/** Shared semantic state and actions for custom controls inside AgentProvider. */
export function useAgentEmbed(): AgentEmbedContext {
  const context = useContext(Context);
  if (!context) throw new Error("Agent UI primitives must be rendered inside AgentProvider");
  return context;
}

type DivProps = Omit<HTMLAttributes<HTMLDivElement>, "children">;
export type AgentMessagesProps = DivProps & Readonly<{
  empty?: ReactNode;
  /** Turn off when rendering activity separately with AgentActivity. @default true */
  showActivity?: boolean;
  renderEntry?: (entry: AgentEntry, controller: AgentControllerSnapshot) => ReactNode;
}>;

/** Ordered transcript. Defaults to plain text and native media; bring your own Markdown renderer. */
export function AgentMessages({ empty = "No messages yet.", showActivity = true, renderEntry, ...props }: AgentMessagesProps) {
  const { controller } = useAgentEmbed();
  const entries = controller.entries.filter(entry => showActivity || (entry.kind !== "tool" && entry.kind !== "plan"));
  return <div role="log" aria-label="Conversation" aria-live="polite" aria-relevant="additions text"
    data-agent-part="messages" {...props}>
    {entries.length ? entries.map(entry => <div key={entry.id} data-agent-part="entry" data-kind={entry.kind}>
      {renderEntry ? renderEntry(entry, controller) : <AgentMessage entry={entry} />}
    </div>) : empty}
  </div>;
}

export type AgentMessageProps = Readonly<{ entry: AgentEntry }>;
/** Stateless entry renderer, also usable outside a provider. */
export function AgentMessage({ entry }: AgentMessageProps) {
  if (entry.kind === "tool") return <AgentTool tool={entry.tool} />;
  if (entry.kind === "plan") return <div data-agent-part="plan">
    {entry.update.explanation ? <p>{entry.update.explanation}</p> : null}
    <ol>{entry.update.plan.map((step, index) => <li key={index} data-status={step.status}>
      {step.step} <span>({step.status.replaceAll("_", " ")})</span>
    </li>)}</ol>
  </div>;
  if (entry.kind === "reasoning") return <details data-agent-part="reasoning">
    <summary>Reasoning{entry.streaming ? "…" : ""}</summary><div data-agent-part="text">{entry.text}</div>
  </details>;
  return <div role={entry.kind === "error" ? "alert" : undefined} data-agent-part="message"
    data-streaming={"streaming" in entry ? entry.streaming : undefined}>
    <span data-agent-part="author">{entry.kind === "user" ? (entry.author === "guest" ? "Guest" : "You") : entry.kind === "error" ? "Error" : "Assistant"}</span>
    <div data-agent-part="text">{entry.text}</div>
  </div>;
}

export type AgentActivityProps = DivProps & Readonly<{
  empty?: ReactNode;
  renderEntry?: (entry: Extract<AgentEntry, { kind: "tool" | "plan" }>, controller: AgentControllerSnapshot) => ReactNode;
}>;
export function AgentActivity({ empty = null, renderEntry, ...props }: AgentActivityProps) {
  const { controller } = useAgentEmbed();
  const entries = controller.entries.filter((entry): entry is Extract<AgentEntry, { kind: "tool" | "plan" }> => entry.kind === "tool" || entry.kind === "plan");
  return <div aria-label="Agent activity" data-agent-part="activity" {...props}>
    {entries.length ? entries.map(entry => <div key={entry.id}>
      {renderEntry ? renderEntry(entry, controller) : <AgentMessage entry={entry} />}
    </div>) : empty}
  </div>;
}

/** Tool details and generated results remain available with no CSS or custom renderer. */
export function AgentTool({ tool }: Readonly<{ tool: ToolActivity }>) {
  return <div data-agent-part="tool" data-status={tool.status}>
    <details><summary>{tool.name} — {tool.status}</summary>
      {tool.input || tool.arguments ? <pre>{tool.input || tool.arguments}</pre> : null}
      {tool.output || tool.result ? <pre>{tool.output || tool.result}</pre> : null}
      {tool.children.map(child => <AgentTool key={child.callId} tool={child} />)}
    </details>
    <AgentOutput items={tool.generatedOutput ?? (tool.images ?? []).map(url => ({ kind: "image", url }))} />
  </div>;
}

/** Safe, unstyled generated output. Invalid URL schemes are never inserted into links/media. */
export function AgentOutput({ items }: Readonly<{ items: readonly GeneratedOutput[] }>) {
  return <>{items.map((item, index) => {
    if (item.kind === "text") return <div key={index} data-agent-part="text">{item.text}</div>;
    const url = generatedOutputUrl(item.url, item.kind);
    const label = item.name || `Generated ${item.kind} ${index + 1}`;
    if (!url) return <p key={index}>{label} — preview unavailable</p>;
    return <figure key={index} data-agent-part="output">
      {item.kind === "image" ? <img src={url} alt={label} loading="lazy" referrerPolicy="no-referrer" /> : null}
      {item.kind === "audio" ? <audio src={url} controls preload="none" aria-label={label} /> : null}
      {item.kind === "video" ? <video src={url} controls playsInline preload="metadata" aria-label={label} /> : null}
      <figcaption><a href={url} download={label} target="_blank" rel="noopener noreferrer">{label}</a></figcaption>
    </figure>;
  })}</>;
}

export type AgentStatusProps = DivProps & Readonly<{
  children?: (context: AgentEmbedContext) => ReactNode;
}>;
export function AgentStatus({ children, ...props }: AgentStatusProps) {
  const context = useAgentEmbed();
  const { agent, controller, error, retry } = context;
  return <div role="status" aria-live="polite" data-agent-part="status" data-running={controller.running} {...props}>
    {children ? children(context) : <>
      <span>{error || (agent ? controller.status : "Not connected")}</span>
      {controller.pendingTurns > 0 ? <span> · {controller.pendingTurns} pending</span> : null}
      {error && retry ? <button type="button" onClick={retry}>Retry connection</button> : null}
    </>}
  </div>;
}

export type AgentComposerProps = Omit<HTMLAttributes<HTMLFormElement>, "children" | "onSubmit" | "onChange"> & Readonly<{
  /** Supply both draft and onDraftChange for controlled input. */
  draft?: string;
  onDraftChange?: (draft: string) => void;
  promptIntent?: "queue" | "steer";
  label?: string;
  placeholder?: string;
  submitLabel?: string;
  cancelLabel?: string;
  disabled?: boolean;
  /** Prevent submission while keeping the draft editable (for validation). */
  submitDisabled?: boolean;
  onSubmitted?: (input: string) => void;
}>;
export function AgentComposer({ draft, onDraftChange, promptIntent, label = "Message", placeholder = "Ask the agent…",
  submitLabel = "Send", cancelLabel = "Stop", disabled = false, submitDisabled = false, onSubmitted, ...props }: AgentComposerProps) {
  const { agent, controller, error } = useAgentEmbed();
  const [localDraft, setLocalDraft] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const text = draft ?? localDraft;
  const latest = useRef({ agent, text });
  latest.current = { agent, text };
  const inputId = useId();
  // An uncontrolled draft belongs to its source, never the next connected account/session.
  useEffect(() => { setLocalDraft(""); setSubmitting(false); }, [agent]);
  const unavailable = disabled || !agent || Boolean(error);
  function change(value: string) {
    if (draft === undefined) setLocalDraft(value);
    onDraftChange?.(value);
  }
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (unavailable || submitDisabled || submitting || !text.trim()) return;
    const submitted = text;
    const source = agent;
    setSubmitting(true);
    try {
      const turn = await controller.submit(submitted, { intent: promptIntent });
      if (source !== latest.current.agent) return;
      // Rejected prompts retain the draft for retry. Slash commands intentionally return no turn.
      if (turn || ["/clear", "/cancel", "/exit"].includes(submitted.trim())) {
        if (latest.current.text === submitted) change("");
        if (turn) onSubmitted?.(submitted.trim());
      }
    } finally {
      if (source === latest.current.agent) setSubmitting(false);
    }
  }
  return <form data-agent-part="composer" {...props} onSubmit={submit}>
    <label htmlFor={inputId}>{label}</label>
    <textarea id={inputId} value={text} placeholder={placeholder} disabled={unavailable}
      onChange={event => change(event.currentTarget.value)} />
    <div data-agent-part="actions">
      <button type="submit" disabled={unavailable || submitDisabled || submitting || !text.trim()}>{submitLabel}</button>
      <button type="button" disabled={!agent || !controller.running} onClick={() => { void controller.cancel(); }}>{cancelLabel}</button>
    </div>
  </form>;
}

/** Pending root prompts have independent cancellation; never cancels a different active prompt. */
export function AgentPendingPrompts(props: DivProps) {
  const { controller } = useAgentEmbed();
  return <div aria-label="Queued messages" data-agent-part="pending" {...props}>
    {controller.pendingPrompts.map(prompt => <div key={prompt.id}>
      <span>{prompt.text}</span>
      <button type="button" disabled={prompt.state === "cancelling"} onClick={() => { void controller.cancelPrompt(prompt.id); }}>
        {prompt.state === "cancelling" ? "Cancelling…" : "Cancel queued message"}
      </button>
    </div>)}
  </div>;
}

/** History errors are recoverable without dropping the existing transcript. */
export function AgentLoadOlder({ children = "Load earlier messages", ...props }: Omit<HTMLAttributes<HTMLDivElement>, "children"> & { children?: ReactNode }) {
  const { agent, controller } = useAgentEmbed();
  const [error, setError] = useState<string>();
  const source = useRef(agent);
  source.current = agent;
  useEffect(() => { setError(undefined); }, [agent]);
  async function load() {
    const current = agent;
    setError(undefined);
    try { await controller.loadOlder(); }
    catch (cause) { if (source.current === current) setError(cause instanceof Error ? cause.message : String(cause)); }
  }
  if (!controller.canLoadOlder && !error) return null;
  return <div data-agent-part="history" {...props}>
    <button type="button" disabled={controller.isLoadingOlder || !controller.canLoadOlder} onClick={() => { void load(); }}>
      {controller.isLoadingOlder ? "Loading…" : children}
    </button>
    {error ? <p role="alert">{error}</p> : null}
  </div>;
}

export type AgentEmbedProps = Omit<AgentProviderProps, "children"> & Readonly<{
  /** Omit to inherit host colors. Import styles.css and themes.css explicitly to opt in. */
  theme?: "light" | "dark";
  containerProps?: HTMLAttributes<HTMLDivElement>;
  messages?: AgentMessagesProps;
  composer?: AgentComposerProps;
}>;
/** Ready-made minimal embed assembled exclusively from the public unstyled primitives. */
export function AgentEmbed({ theme, containerProps, messages, composer, ...provider }: AgentEmbedProps) {
  return <AgentProvider {...provider}>
    <div data-agent-embed="" data-theme={theme} {...containerProps}>
      <AgentStatus /><AgentLoadOlder /><AgentMessages {...messages} /><AgentPendingPrompts /><AgentComposer {...composer} />
    </div>
  </AgentProvider>;
}
