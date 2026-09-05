import { ChevronRight, Menu, MessageSquare, Plus, X } from "lucide-react";
import { memo, useEffect, useId, useRef } from "react";
import type { AgentStatus } from "./types.js";

export type ConversationSummary = Readonly<{
  id: string;
  title: string;
  updatedAt?: number;
  turnCount?: number;
}>;

export const ConversationHistoryRail = memo(function ConversationHistoryRail({
  agentStatus, conversations, error, mobileOpen, onClose, onCreate, onOpen, onRetry, onSelect,
  pending, runtime, selectedId,
}: {
  agentStatus: AgentStatus;
  conversations: readonly ConversationSummary[];
  error?: string;
  mobileOpen: boolean;
  onClose(): void;
  onCreate?(): void;
  onOpen(): void;
  onRetry(): void;
  onSelect(id: string): void;
  pending: boolean;
  runtime: "local" | "managed";
  selectedId?: string;
}) {
  const titleId = useId();
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!mobileOpen) return;
    const keydown = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", keydown);
    closeRef.current?.focus();
    return () => window.removeEventListener("keydown", keydown);
  }, [mobileOpen, onClose]);
  const selected = conversations.find(({ id }) => id === selectedId);
  return <>
    <div
      className={mobileOpen ? "conversation-backdrop is-visible" : "conversation-backdrop"}
      aria-hidden="true" onPointerDown={onClose}
    />
    <aside
      className={mobileOpen ? "conversation-sidebar is-mobile-open" : "conversation-sidebar"}
      aria-labelledby={titleId}
      role={mobileOpen ? "dialog" : "complementary"}
      aria-modal={mobileOpen || undefined}
    >
      <header className="conversation-sidebar-header">
        <div>
          <strong id={titleId}>Conversations</strong>
          <span><MessageSquare aria-hidden="true" /> {runtime === "local" ? "this browser" : "managed account"}</span>
        </div>
        <nav className="conversation-sidebar-actions" aria-label="Conversation actions">
          {onCreate ? <button className="conversation-icon-button" type="button" disabled={pending}
            aria-label="New conversation" title="New conversation" onClick={onCreate}>
            <Plus aria-hidden="true" />
          </button> : null}
          <button ref={closeRef} className="conversation-drawer-close" type="button"
            aria-label="Close conversations" onClick={onClose}><X aria-hidden="true" /></button>
        </nav>
      </header>
      <div className="conversation-list">
        {conversations.map((conversation) => {
          const active = conversation.id === selectedId;
          const title = conversationDisplayTitle(conversation.title);
          return <button
            className={active ? "conversation-row is-selected" : "conversation-row"}
            type="button" key={conversation.id}
            disabled={pending}
            aria-current={active ? "location" : undefined}
            onClick={() => onSelect(conversation.id)}
          >
            <strong>{title}</strong>
            <span className="conversation-row-meta">
              <span>{relativeTime(conversation.updatedAt)}</span>
              <span aria-hidden="true">·</span>
              <span>{conversation.turnCount === undefined
                ? runtime === "local" ? "Browser thread" : "Durable agent"
                : `${conversation.turnCount} turn${conversation.turnCount === 1 ? "" : "s"}`}</span>
            </span>
            <ChevronRight aria-hidden="true" />
          </button>;
        })}
        {error ? <div className="conversation-list-error">
          <p role="alert">{error}</p>
          <button type="button" disabled={pending} onClick={onRetry}>Retry conversations</button>
        </div> : null}
      </div>
    </aside>
    <header className="conversation-mobile-header">
      <button type="button" aria-label="Open conversations" onClick={onOpen}><Menu aria-hidden="true" /></button>
      <div className="conversation-mobile-title">
        <strong>{selected ? conversationDisplayTitle(selected.title) : "Conversations"}</strong>
        <span className={`conversation-mobile-status is-${agentStatus}`}>
          <i aria-hidden="true" />{statusLabel(agentStatus)}
        </span>
      </div>
      {onCreate ? <button className="conversation-mobile-new" type="button" disabled={pending}
        aria-label="New conversation" onClick={onCreate}>
        <Plus aria-hidden="true" /><span>New</span>
      </button> : null}
    </header>
  </>;
});

function relativeTime(value?: number): string {
  if (value === undefined) return "";
  const elapsed = Math.max(0, Date.now() - value);
  if (elapsed < 60_000) return "now";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)}m`;
  if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)}h`;
  return `${Math.floor(elapsed / 86_400_000)}d`;
}

function statusLabel(status: AgentStatus): string {
  if (status === "ready") return "ready";
  if (status === "error") return "needs attention";
  if (status === "starting") return "connecting";
  return "waiting";
}

function conversationDisplayTitle(title: string): string {
  return /^Conversation [a-f\d]{8}$/i.test(title) ? "New conversation" : title;
}
