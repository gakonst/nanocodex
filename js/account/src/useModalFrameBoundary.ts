import { useLayoutEffect, type RefObject } from "react";
import { modalFocusableElements } from "./modalBoundary.ts";

const MODAL_FRAME_BOUNDARY_MESSAGE = "nanocodex-modal-boundary-key";
const MODAL_FRAME_BOUNDARY_READY_MESSAGE = "nanocodex-modal-boundary-ready";
const MODAL_FRAME_BOUNDARY_STATE_MESSAGE = "nanocodex-modal-boundary-state";

type ModalFrameBoundaryKey = "Escape" | "TabBackward" | "TabForward";

export function modalFrameBoundaryStateMessage(active: boolean) {
  return { type: MODAL_FRAME_BOUNDARY_STATE_MESSAGE, active } as const;
}

export function isModalFrameBoundaryReadyMessage(value: unknown): boolean {
  return isRecordWithType(value, MODAL_FRAME_BOUNDARY_READY_MESSAGE);
}

export function readModalFrameBoundaryKey(
  value: unknown,
): ModalFrameBoundaryKey | undefined {
  if (!isRecordWithType(value, MODAL_FRAME_BOUNDARY_MESSAGE)) return undefined;
  const key = (value as Record<string, unknown>).key;
  return key === "Escape" || key === "TabBackward" || key === "TabForward"
    ? key
    : undefined;
}

function focusAdjacentToModalFrame(
  panel: HTMLElement,
  frame: HTMLIFrameElement,
  direction: Exclude<ModalFrameBoundaryKey, "Escape">,
) {
  const focusable = modalFocusableElements(panel);
  if (focusable.length === 0) {
    panel.focus();
    return;
  }
  const frameIndex = focusable.findIndex((element) => element === frame);
  const offset = direction === "TabBackward" ? -1 : 1;
  const origin = frameIndex < 0
    ? direction === "TabBackward" ? 0 : focusable.length - 1
    : frameIndex;
  focusable[(origin + offset + focusable.length) % focusable.length]?.focus();
}

export function useModalFrameBoundary({
  onDismiss,
  open,
  panelRef,
}: {
  onDismiss(): void;
  open: boolean;
  panelRef: RefObject<HTMLElement | null>;
}) {
  useLayoutEffect(() => {
    if (!open) return;
    const panel = panelRef.current;
    const window = panel?.ownerDocument.defaultView;
    if (!panel || !window) return;
    const frames = () => panel.querySelectorAll<HTMLIFrameElement>("iframe");
    const setState = (active: boolean) => {
      const message = modalFrameBoundaryStateMessage(active);
      for (const frame of frames()) frame.contentWindow?.postMessage(message, "*");
    };
    const onMessage = (event: MessageEvent) => {
      for (const frame of frames()) {
        if (frame.contentWindow !== event.source) continue;
        if (isModalFrameBoundaryReadyMessage(event.data)) {
          frame.contentWindow?.postMessage(modalFrameBoundaryStateMessage(true), "*");
          return;
        }
        const key = readModalFrameBoundaryKey(event.data);
        if (key === "Escape") onDismiss();
        else if (key) focusAdjacentToModalFrame(panel, frame, key);
        return;
      }
    };
    setState(true);
    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      setState(false);
    };
  }, [onDismiss, open, panelRef]);
}

function isRecordWithType(value: unknown, type: string): boolean {
  return typeof value === "object"
    && value !== null
    && !Array.isArray(value)
    && (value as Record<string, unknown>).type === type;
}
