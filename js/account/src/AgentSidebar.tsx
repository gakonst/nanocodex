import {
  BookOpen,
  ChevronDown,
  CircleUserRound,
  Compass,
  Layers,
  Link2,
  MessageCircle,
  PanelLeftClose,
  Search,
  SquarePen,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { Link, useLocation } from "react-router";
import { AgentSearchDialog } from "./AgentSearchDialog";
import type { ManagedConversation } from "./managedAgentRuntime";
import { useModalBoundary } from "./modalBoundary";
import {
  connectDemoUrl,
  demoNavigation,
  gitNavigation,
  pathForSurface,
  primaryNavigation,
} from "./navigation";

/** Web navigation owns presentation; the managed runtime still owns conversation selection. */
export function AgentSidebar({
  conversations,
  error,
  landing,
  onClose,
  onCollapse,
  collapsed,
  onCreate,
  onRetry,
  onSelect,
  open,
  pending,
  persistent,
  selectedId,
  triggerRef,
  active,
}: {
  active: boolean;
  conversations: readonly ManagedConversation[];
  error?: string;
  landing: boolean;
  onClose(): void;
  onCollapse(): void;
  collapsed: boolean;
  onCreate(): void;
  onRetry(): void;
  onSelect(id: string): void;
  open: boolean;
  pending: boolean;
  persistent: boolean;
  selectedId?: string;
  triggerRef: RefObject<HTMLButtonElement | null>;
}) {
  const [searchOpen, setSearchOpen] = useState(false);
  const panelRef = useRef<HTMLElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const location = useLocation();
  const dismiss = useCallback(() => onClose(), [onClose]);
  useModalBoundary({
    open,
    onDismiss: dismiss,
    panelRef,
    backdropRef,
    initialFocusRef: closeRef,
    returnFocusRef: triggerRef,
  });
  useEffect(() => {
    onClose();
  }, [location.pathname, location.search, onClose]);
  useEffect(() => {
    const desktop = window.matchMedia("(min-width: 761px)");
    const closeOnDesktop = () => {
      if (desktop.matches) onClose();
    };
    desktop.addEventListener("change", closeOnDesktop);
    return () => desktop.removeEventListener("change", closeOnDesktop);
  }, [onClose]);
  useEffect(() => {
    if (!active || landing) return;
    const searchShortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        onClose();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", searchShortcut);
    return () => window.removeEventListener("keydown", searchShortcut);
  }, [active, landing, onClose]);

  return (
    <>
      {open ? (
        <div
          className="agent-navigation-backdrop"
          ref={backdropRef}
          onClick={onClose}
          aria-hidden="true"
        />
      ) : null}
      <aside
        id="agent-navigation"
        ref={panelRef}
        className={`agent-navigation${open ? " is-open" : ""}${collapsed ? " is-collapsed" : ""}`}
        aria-label="Workspace navigation"
        role={open ? "dialog" : undefined}
        aria-modal={open || undefined}
      >
        <div className="agent-navigation-brand">
          <Link to="/" aria-label="Nanocodex home">
            <span className="paradigm-mark" aria-hidden="true" />
            <span>Nanocodex</span>
          </Link>
          {!landing ? (
            <button
              className="chat-icon-button agent-search-open"
              type="button"
              aria-label="Search agents"
              title="Search agents (⌘K / Ctrl+K)"
              onClick={() => {
                onClose();
                setSearchOpen(true);
              }}
            >
              <Search aria-hidden="true" />
            </button>
          ) : null}
          <button
            ref={closeRef}
            className="agent-navigation-close chat-icon-button"
            onClick={() => {
              if (open) onClose();
              else onCollapse();
            }}
            aria-label="Close sidebar"
            title="Close sidebar"
            type="button"
          >
            <PanelLeftClose />
          </button>
        </div>
        <nav className="agent-navigation-primary" aria-label="Chat navigation">
          <button type="button" onClick={onCreate} disabled={pending}>
            <SquarePen />
            <span>{landing ? "New chat" : "New agent"}</span>
          </button>
          <Link to="/" aria-current={landing ? "page" : undefined}>
            <MessageCircle />
            <span>Chat</span>
          </Link>
          <Link to="/agent" aria-current={!landing ? "page" : undefined}>
            <Layers aria-hidden="true" />
            <span>Agents</span>
          </Link>
          <Link to="/connect">
            <Link2 />
            <span>Connections</span>
          </Link>
        </nav>
        <div className="agent-navigation-history">
          <div className="agent-navigation-heading">
            <span>{landing ? "Your workspace" : "Recents"}</span>
          </div>
          <div className="agent-navigation-list" aria-busy={pending}>
            {!landing
              ? conversations.map((conversation) => (
                  <button
                    className="agent-navigation-thread"
                    key={conversation.id}
                    type="button"
                    title={conversation.title}
                    disabled={pending}
                    aria-current={
                      conversation.id === selectedId ? "location" : undefined
                    }
                    onClick={() => {
                      onSelect(conversation.id);
                    }}
                  >
                    <span>
                      {/^Conversation [a-f\d]{8}$/i.test(conversation.title)
                        ? "New agent"
                        : conversation.title}
                    </span>
                  </button>
                ))
              : null}
            {landing ? (
              <div className="agent-navigation-empty">
                <p>Give your work a place to keep going.</p>
                <Link to="/agent">
                  Open your agents <span aria-hidden="true">↗</span>
                </Link>
              </div>
            ) : !conversations.length ? (
              <p className="agent-navigation-empty">
                {pending
                  ? "Loading your agents…"
                  : "Your agents will appear here."}
              </p>
            ) : null}
            {error ? (
              <div className="agent-navigation-error">
                <p role="alert">{error}</p>
                <button type="button" disabled={pending} onClick={onRetry}>
                  Try again
                </button>
              </div>
            ) : null}
          </div>
        </div>
        <div className="agent-navigation-footer">
          <Link to="/docs">
            <BookOpen aria-hidden="true" />
            <span>Documentation</span>
          </Link>
          <details className="agent-navigation-explore">
            <summary>
              <Compass aria-hidden="true" />
              <span>Explore</span>
              <ChevronDown aria-hidden="true" />
            </summary>
            <nav aria-label="Explore Nanocodex">
              {[
                ...demoNavigation.filter(({ surface }) => surface !== "agent"),
                ...primaryNavigation.filter(
                  ({ surface }) => surface !== "docs",
                ),
                ...gitNavigation,
              ].map(({ surface, label }) => (
                <Link key={surface} to={pathForSurface(surface)}>
                  {label}
                </Link>
              ))}
              <a
                href={connectDemoUrl(window.location.origin)}
                target="_blank"
                rel="noreferrer"
              >
                Connect playground ↗
              </a>
            </nav>
          </details>
          <Link className="agent-navigation-account" to="/connect">
            <CircleUserRound aria-hidden="true" />
            <span>
              <strong>{persistent ? "Your account" : "Get started"}</strong>
              <small>
                {persistent ? "Connections & settings" : "Sign in to Nanocodex"}
              </small>
            </span>
          </Link>
        </div>
      </aside>
      {searchOpen && active ? (
        <AgentSearchDialog
          conversations={conversations}
          onClose={() => setSearchOpen(false)}
          onSelect={onSelect}
        />
      ) : null}
    </>
  );
}
