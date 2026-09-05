"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal, useTerminal, type WTerm } from "@wterm/react";
import "@wterm/react/css";

import {
  TERMINAL_SESSION_EVENT,
  parseTerminalTextFrame,
  terminalInputFrame,
  terminalResizeFrame,
  terminalSocketUrl,
  terminalStartFrame,
} from "@/lib/terminal-protocol";
import { errorMessage } from "@/lib/validation";
import { labelWtermInput } from "@/lib/wterm-accessibility";

const BROWSER_STATE_KEY = "nanocodex.vercel.workflow.web.v1";

type TerminalAttachment = { url: string; token: string };
type TerminalStatus = "detached" | "attached" | "exited" | "failed";

export function WorkspaceTerminal() {
  const { ref, write } = useTerminal();
  const terminalRef = useRef<WTerm | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const generationRef = useRef(0);
  const [mounted, setMounted] = useState(false);
  const [sessionId, setSessionId] = useState("");
  const [accessToken, setAccessToken] = useState("");
  const [status, setStatus] = useState<TerminalStatus>("detached");
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [attaching, setAttaching] = useState(false);

  useEffect(() => {
    setMounted(true);
  }, []);

  const closeSocket = useCallback((reason: string) => {
    generationRef.current += 1;
    setAttaching(false);
    const socket = socketRef.current;
    socketRef.current = null;
    if (socket && socket.readyState < WebSocket.CLOSING) {
      socket.close(1000, reason);
    }
  }, []);

  useEffect(() => {
    setSessionId(readStoredSessionId());
    const onSession = (event: Event) => {
      const detail = (event as CustomEvent<{ sessionId?: unknown }>).detail;
      const nextSessionId = typeof detail?.sessionId === "string"
        ? detail.sessionId
        : "";
      if (nextSessionId === sessionId) return;
      closeSocket("workflow session changed");
      setStatus("detached");
      setError("");
      setSessionId(nextSessionId);
    };
    window.addEventListener(TERMINAL_SESSION_EVENT, onSession);
    return () => {
      window.removeEventListener(TERMINAL_SESSION_EVENT, onSession);
      closeSocket("terminal component unmounted");
    };
  }, [closeSocket, sessionId]);

  const attach = useCallback(async () => {
    const terminal = terminalRef.current;
    if (!sessionId || !terminal || !accessToken || attaching) return;
    closeSocket("replaced by a new terminal attachment");
    const generation = generationRef.current;
    setAttaching(true);
    setError("");

    try {
      const response = await fetch(
        `/api/sessions/${encodeURIComponent(sessionId)}/terminal`,
        {
          method: "POST",
          headers: { authorization: `Bearer ${accessToken}` },
        },
      );
      const body = await response.json() as TerminalAttachment | {
        error?: { message?: string };
      };
      if (!response.ok) {
        throw new Error(
          "error" in body && body.error?.message
            ? body.error.message
            : `terminal attach failed with HTTP ${response.status}`,
        );
      }
      if (
        !("url" in body) ||
        typeof body.url !== "string" ||
        typeof body.token !== "string"
      ) {
        throw new Error("terminal attach returned an invalid endpoint");
      }
      if (generation !== generationRef.current) return;

      const socket = new WebSocket(terminalSocketUrl(body.url, body.token));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;

      socket.addEventListener("open", () => {
        if (generation !== generationRef.current) return;
        socket.send(terminalStartFrame(terminal.cols, terminal.rows));
        setStatus("attached");
      });
      socket.addEventListener("message", (event) => {
        if (generation !== generationRef.current) return;
        void deliverTerminalMessage(event.data, write, (code) => {
          setStatus("exited");
          write(`\r\n\x1b[90m[process exited${code === undefined ? "" : ` ${code}`}]\x1b[0m\r\n`);
        });
      });
      socket.addEventListener("error", () => {
        if (generation !== generationRef.current) return;
        setStatus("failed");
        setError("terminal WebSocket failed; detach and request a new attachment");
      });
      socket.addEventListener("close", () => {
        if (generation !== generationRef.current) return;
        socketRef.current = null;
        setStatus((current) => current === "failed" || current === "exited"
          ? current
          : "detached");
      });
    } catch (attachError) {
      if (generation !== generationRef.current) return;
      setStatus("failed");
      setError(errorMessage(attachError));
    } finally {
      if (generation === generationRef.current) setAttaching(false);
    }
  }, [accessToken, attaching, closeSocket, sessionId, write]);

  const detach = useCallback(() => {
    closeSocket("user detached terminal");
    setStatus("detached");
    setError("");
  }, [closeSocket]);

  const onData = useCallback((data: string) => {
    const socket = socketRef.current;
    if (socket?.readyState === WebSocket.OPEN) {
      socket.send(terminalInputFrame(data));
    }
  }, []);

  const onResize = useCallback((cols: number, rows: number) => {
    const socket = socketRef.current;
    if (socket?.readyState === WebSocket.OPEN) {
      socket.send(terminalResizeFrame(cols, rows));
    }
  }, []);

  return (
    <section className="workspace-terminal" aria-labelledby="workspace-terminal-title">
      <div className="terminal-toolbar">
        <div>
          <p className="eyebrow" id="workspace-terminal-title">WORKSPACE TERMINAL</p>
          <p className="terminal-caption">
            Ephemeral shell · persistent files · session {sessionId || "none"}
          </p>
        </div>
        <label className="terminal-token" htmlFor="terminal-token">
          Terminal token
          <input
            id="terminal-token"
            type="password"
            autoComplete="off"
            value={accessToken}
            onChange={(event) => setAccessToken(event.target.value)}
            placeholder="NANOCODEX_TERMINAL_TOKEN"
          />
        </label>
        <button
          type="button"
          onClick={() => void attach()}
          disabled={!sessionId || !ready || !accessToken || attaching || status === "attached"}
        >
          Attach
        </button>
        <button
          type="button"
          className="secondary"
          onClick={detach}
          disabled={status !== "attached"}
        >
          Detach terminal
        </button>
        <span className="pill" data-tone={status === "attached" ? "ok" : status === "failed" ? "bad" : ""}>
          {status}
        </span>
      </div>
      {error ? <p className="terminal-error" role="alert">{error}</p> : null}
      <div className="terminal-screen">
        {mounted ? (
          <Terminal
            ref={ref}
            role="group"
            aria-label="Workspace terminal"
            aria-multiline={undefined}
            autoResize
            cursorBlink
            onReady={(terminal) => {
              labelWtermInput(terminal, "Workspace terminal input");
              terminalRef.current = terminal;
              setReady(true);
            }}
            onData={onData}
            onResize={onResize}
            onError={(terminalError) => {
              setStatus("failed");
              setError(errorMessage(terminalError));
            }}
          />
        ) : null}
      </div>
    </section>
  );
}

async function deliverTerminalMessage(
  data: string | ArrayBuffer | Blob,
  write: (data: string | Uint8Array) => void,
  onExit: (code?: number) => void,
): Promise<void> {
  if (typeof data === "string") {
    const exit = parseTerminalTextFrame(data);
    if (exit) onExit(exit.code);
    else write(data);
    return;
  }
  const bytes = data instanceof Blob
    ? new Uint8Array(await data.arrayBuffer())
    : new Uint8Array(data);
  write(bytes);
}

function readStoredSessionId(): string {
  try {
    const state = JSON.parse(localStorage.getItem(BROWSER_STATE_KEY) ?? "null") as {
      sessionId?: unknown;
    } | null;
    return typeof state?.sessionId === "string" ? state.sessionId : "";
  } catch {
    return "";
  }
}
