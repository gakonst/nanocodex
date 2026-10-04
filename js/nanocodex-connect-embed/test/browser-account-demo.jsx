import React, { useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { Agent } from "nanocodex/managed";
import { AgentConversation } from "nanocodex-connect-embed/conversation";
import { createManagedAgentSource } from "nanocodex-connect-embed/managed";
import "nanocodex-connect-embed/conversation.css";
// The app owns these styles. Import the actual sources instead of approximating
// their tokens, transcript spacing, bubbles or composer in a demo theme.
import "../../account/src/index.css";
import "../../account/src/AgentTerminal.css";
import "../../account/src/Home.css";

const noop = () => {};
function AccountDemo() {
  const [theme, setTheme] = useState("dark");
  const source = useMemo(() => createManagedAgentSource(Agent.open(
    "0198d3f0-8844-7000-8000-000000000001", { baseUrl: location.origin },
  ), { history: false }), []);
  return <main className="home-page" style={{ height: "100dvh" }}>
    <div className="nanocodex-demo chat-workspace is-full">
      <div className="conversation-workspace is-sidebar-collapsed">
        <div className="conversation-main">
          <header className="agent-chat-header">
            <div className="agent-chat-heading"><strong>Nanocodex</strong></div>
            <div className="agent-chat-header-actions">
              <button className="chat-icon-button" type="button" onClick={() => {
                const next = theme === "dark" ? "light" : "dark";
                document.documentElement.dataset.theme = next;
                setTheme(next);
              }} aria-label={`Use ${theme === "dark" ? "light" : "dark"} appearance`}>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  {theme === "dark" ? <><circle cx="12" cy="12" r="4" /><path d="M12 2v2m0 16v2M2 12h2m16 0h2M4.93 4.93l1.42 1.42m11.3 11.3 1.42 1.42M4.93 19.07l1.42-1.42m11.3-11.3 1.42-1.42" /></> : <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.464.402.807a6.25 6.25 0 0 0 8.268 8.268c.344-.215.83-.004.803.397" />}
                </svg>
              </button>
            </div>
          </header>
          <AgentConversation agent={source} agentError={undefined} mode="full"
            onConversationActivity={noop} onStateChange={noop} retryAgent={noop}
            composerPlaceholder="Ask Nanocodex" welcome="# What should we work on?" />
          <p className="agent-chat-footnote">SDK demo · Actual Nanocodex conversation and app styles · Synthetic HTTP/SSE agent</p>
        </div>
      </div>
    </div>
  </main>;
}
createRoot(document.getElementById("root")).render(<AccountDemo />);
