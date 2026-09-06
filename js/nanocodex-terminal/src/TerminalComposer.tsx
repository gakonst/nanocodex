"use client";

import { ArrowUp, Square } from "lucide-react";
import { useEffect, useRef, type ReactNode } from "react";
import type { AgentStatus } from "./types.js";
import { COARSE_POINTER_QUERY, terminalComposerAction } from "./policy.js";

/** One paste-capable composer shared by desktop and touch terminals. */
export function TerminalComposer({
  controls,
  draft,
  pending,
  placeholder,
  running,
  status,
  onCancel,
  onChange,
  onSubmit,
}: {
  controls?: ReactNode;
  draft: string;
  pending: boolean;
  placeholder?: string;
  running: boolean;
  status: AgentStatus;
  onCancel(): void;
  onChange(value: string): void;
  onSubmit(value: string): void;
}) {
  const composing = useRef(false);
  const textarea = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const element = textarea.current;
    if (
      !element
      || status !== "ready"
      || window.matchMedia(COARSE_POINTER_QUERY).matches
      || (document.activeElement !== document.body && document.activeElement !== null)
    ) return;
    const frame = window.requestAnimationFrame(() => element.focus({ preventScroll: true }));
    return () => window.cancelAnimationFrame(frame);
  }, [status]);

  const submit = () => {
    const value = textarea.current?.value ?? draft;
    if (pending || status !== "ready" || !value.trim()) return;
    onSubmit(value);
  };
  const action = terminalComposerAction(running, draft);

  return (
    <form
      className={`agent-touch-composer${running ? " is-running" : ""}`}
      aria-label="Nanocodex message composer"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="agent-touch-field">
        <textarea
          ref={textarea}
          aria-label="Message Nanocodex"
          enterKeyHint="send"
          rows={1}
          placeholder={placeholder}
          value={draft}
          onChange={(event) => onChange(event.currentTarget.value)}
          onCompositionStart={() => { composing.current = true; }}
          onCompositionEnd={(event) => {
            composing.current = false;
            onChange(event.currentTarget.value);
          }}
          onKeyDown={(event) => {
            if (!isSubmitKeyEvent(event.nativeEvent, composing.current)) return;
            event.preventDefault();
            submit();
          }}
        />
        <div className="agent-touch-actions">
          {controls}
          {action === "stop" ? (
            <button type="button" aria-label="Stop response" title="Stop response" disabled={status !== "ready"} onClick={onCancel}>
              <Square aria-hidden="true" />
            </button>
          ) : null}
          <button type="submit" aria-label="Send message" title="Send message" disabled={pending || status !== "ready" || !draft.trim()}>
            <ArrowUp aria-hidden="true" />
          </button>
        </div>
      </div>
    </form>
  );
}
function isSubmitKeyEvent(
  event: Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing">,
  composing: boolean,
): boolean {
  return event.key === "Enter"
    && !event.shiftKey
    && !event.isComposing
    && !composing;
}
