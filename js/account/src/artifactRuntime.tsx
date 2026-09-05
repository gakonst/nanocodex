import React, { useEffect, useRef, useState, type ComponentType } from "react";
import htm from "htm";
import {
  modalFrameBoundaryMessage,
  modalFrameBoundaryReadyMessage,
  modalFrameTabBoundaryKey,
  readModalFrameBoundaryState,
} from "./artifactModalBoundary";
import {
  deepActiveElement,
  modalFocusableElements,
} from "./modalBoundary";

const html = htm.bind(React.createElement);

type GeneratedApp = ComponentType<{ sendPrompt(prompt: string): void }>;
type ArtifactView = { App: GeneratedApp; error?: never } | { App?: never; error: string };

export function ArtifactRuntime() {
  const artifactId = useRef("");
  const [view, setView] = useState<ArtifactView>();

  const sendPrompt = (prompt: unknown) => {
    if (typeof prompt !== "string" || !prompt.trim()) return;
    window.parent.postMessage({
      type: "artifact-action",
      artifactId: artifactId.current,
      prompt,
    }, "*");
  };

  useEffect(() => {
    document.documentElement.classList.add("artifact-runtime-page");
    let modalBoundaryActive = false;

    const onMessage = (event: MessageEvent) => {
      if (event.source !== window.parent) return;
      const boundaryState = readModalFrameBoundaryState(event.data);
      if (boundaryState !== undefined) {
        modalBoundaryActive = boundaryState;
        return;
      }
      const value = asRecord(event.data);
      if (
        value?.type !== "render-artifact"
        || typeof value.source !== "string"
        || typeof value.artifactId !== "string"
      ) return;
      artifactId.current = value.artifactId;
      try {
        const factory = new Function(
          "React",
          "html",
          "sendPrompt",
          `"use strict";\n${value.source}\n;return typeof App === "function" ? App : undefined;`,
        );
        const App = factory(React, html, sendPrompt) as GeneratedApp | undefined;
        if (!App) throw new TypeError("generated source must define an App component");
        setView({ App });
      } catch (error) {
        setView({ error: errorMessage(error) });
      }
    };

    const onKeyDown = (event: KeyboardEvent) => {
      if (window.parent === window || !modalBoundaryActive) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        window.parent.postMessage(modalFrameBoundaryMessage("Escape"), "*");
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = modalFocusableElements(document);
      const active = deepActiveElement(document);
      const key = modalFrameTabBoundaryKey({
        activeIndex: focusable.findIndex((element) => element === active),
        focusableCount: focusable.length,
        shiftKey: event.shiftKey,
      });
      if (!key) return;
      event.preventDefault();
      event.stopPropagation();
      window.parent.postMessage(modalFrameBoundaryMessage(key), "*");
    };

    const onError = (event: ErrorEvent) => {
      setView({ error: event.message || "generated React failed" });
    };

    window.addEventListener("message", onMessage);
    window.addEventListener("keydown", onKeyDown, { capture: true });
    window.addEventListener("error", onError);
    window.parent.postMessage({ type: "artifact-runtime-ready" }, "*");
    window.parent.postMessage(modalFrameBoundaryReadyMessage(), "*");
    return () => {
      document.documentElement.classList.remove("artifact-runtime-page");
      window.removeEventListener("message", onMessage);
      window.removeEventListener("keydown", onKeyDown, { capture: true });
      window.removeEventListener("error", onError);
    };
  }, []);

  if (view?.error) return <RuntimeError error={view.error} />;
  return view?.App ? <view.App sendPrompt={sendPrompt} /> : null;
}

function RuntimeError({ error }: { error: string }) {
  return <main className="runtime-error"><strong>Generated React failed</strong><pre>{error}</pre></main>;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.stack ?? error.message : String(error);
}
