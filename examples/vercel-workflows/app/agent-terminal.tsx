"use client";

import { useEffect, useState } from "react";
import { Terminal, useTerminal, type WTerm } from "@wterm/react";
import "@wterm/react/css";

import {
  AGENT_TERMINAL_EVENT,
  AGENT_TERMINAL_READY_EVENT,
  isAgentTerminalSnapshot,
  renderAgentTerminal,
  type AgentTerminalSnapshot,
} from "@/lib/agent-terminal";
import { errorMessage } from "@/lib/validation";
import { labelWtermInput } from "@/lib/wterm-accessibility";

const EMPTY_SNAPSHOT: AgentTerminalSnapshot = { messages: [], streamedText: "" };

export function AgentTerminal() {
  const { ref, write } = useTerminal();
  const [mounted, setMounted] = useState(false);
  const [ready, setReady] = useState(false);
  const [snapshot, setSnapshot] = useState(EMPTY_SNAPSHOT);
  const [error, setError] = useState("");

  useEffect(() => {
    setMounted(true);
  }, []);

  useEffect(() => {
    const receiveSnapshot = (event: Event) => {
      const detail = (event as CustomEvent<unknown>).detail;
      if (isAgentTerminalSnapshot(detail)) setSnapshot(detail);
    };
    window.addEventListener(AGENT_TERMINAL_EVENT, receiveSnapshot);
    window.dispatchEvent(new Event(AGENT_TERMINAL_READY_EVENT));
    return () => window.removeEventListener(AGENT_TERMINAL_EVENT, receiveSnapshot);
  }, []);

  useEffect(() => {
    if (!ready) return;
    const frame = window.requestAnimationFrame(() => {
      write(renderAgentTerminal(snapshot));
    });
    return () => window.cancelAnimationFrame(frame);
  }, [ready, snapshot, write]);

  return (
    <section className="agent-terminal" aria-labelledby="agent-terminal-title">
      <div className="terminal-toolbar">
        <div>
          <p className="eyebrow" id="agent-terminal-title">AGENT TERMINAL</p>
          <p className="terminal-caption">
            Replayable workflow events · interchangeable @wterm/react renderer
          </p>
        </div>
        {error || ready ? (
          <span className="pill" data-tone={ready && !error ? "ok" : "bad"}>
            {error ? "renderer failed" : "headless agent attached"}
          </span>
        ) : null}
      </div>
      {error ? <p className="terminal-error" role="alert">{error}</p> : null}
      <div className="terminal-screen">
        {mounted ? (
          <Terminal
            ref={ref}
            role="group"
            aria-label="Agent terminal"
            aria-multiline={undefined}
            autoResize
            cursorBlink={false}
            onReady={(terminal: WTerm) => {
              labelWtermInput(terminal, "Agent terminal input");
              setReady(true);
            }}
            onError={(terminalError) => setError(errorMessage(terminalError))}
          />
        ) : null}
      </div>
    </section>
  );
}
