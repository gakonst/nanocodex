import { MessageCircle, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ManagedConversation } from "./managedAgentRuntime";

export function AgentSearchDialog({
  conversations,
  onClose,
  onSelect,
}: {
  conversations: readonly ManagedConversation[];
  onClose(): void;
  onSelect(id: string): void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [query, setQuery] = useState("");
  const matches = conversations.filter(({ title }) =>
    title.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()),
  );
  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);
  return (
    <dialog
      className="agent-search-dialog"
      ref={dialog}
      aria-label="Search agents"
      onClose={onClose}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="agent-search-panel">
        <header>
          <Search aria-hidden="true" />
          <input
            autoFocus
            type="search"
            aria-label="Search agents"
            placeholder="Search your agents…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && matches[0]) {
                event.preventDefault();
                onSelect(matches[0].id);
                onClose();
              }
            }}
          />
          <button
            className="chat-icon-button"
            aria-label="Close search"
            type="button"
            onClick={onClose}
          >
            <X aria-hidden="true" />
          </button>
        </header>
        <div className="agent-search-results">
          <p>
            {query
              ? `${matches.length} ${matches.length === 1 ? "result" : "results"}`
              : "Recent agents"}
          </p>
          {matches.map((conversation) => (
            <button
              key={conversation.id}
              type="button"
              onClick={() => {
                onSelect(conversation.id);
                onClose();
              }}
            >
              <MessageCircle aria-hidden="true" />
              <span>
                {/^Conversation [a-f\d]{8}$/i.test(conversation.title)
                  ? "New agent"
                  : conversation.title}
              </span>
            </button>
          ))}
          {!matches.length ? (
            <div className="agent-search-empty">
              {query
                ? "No agents found. Try a different search."
                : "Your agents will appear here."}
            </div>
          ) : null}
        </div>
        <footer>
          Enter to open the first result <span>Esc to close</span>
        </footer>
      </div>
    </dialog>
  );
}
