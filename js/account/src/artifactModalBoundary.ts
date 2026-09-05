const MODAL_FRAME_BOUNDARY_MESSAGE = "nanocodex-modal-boundary-key";
const MODAL_FRAME_BOUNDARY_READY_MESSAGE = "nanocodex-modal-boundary-ready";
const MODAL_FRAME_BOUNDARY_STATE_MESSAGE = "nanocodex-modal-boundary-state";

type ModalFrameBoundaryKey = "Escape" | "TabBackward" | "TabForward";

export function modalFrameTabBoundaryKey({
  activeIndex,
  focusableCount,
  shiftKey,
}: {
  activeIndex: number;
  focusableCount: number;
  shiftKey: boolean;
}): ModalFrameBoundaryKey | undefined {
  if (focusableCount <= 0) return shiftKey ? "TabBackward" : "TabForward";
  if (shiftKey && activeIndex <= 0) return "TabBackward";
  if (!shiftKey && activeIndex === focusableCount - 1) return "TabForward";
  return undefined;
}

export function modalFrameBoundaryMessage(key: ModalFrameBoundaryKey) {
  return { type: MODAL_FRAME_BOUNDARY_MESSAGE, key } as const;
}

export function modalFrameBoundaryReadyMessage() {
  return { type: MODAL_FRAME_BOUNDARY_READY_MESSAGE } as const;
}

export function readModalFrameBoundaryState(value: unknown): boolean | undefined {
  if (
    typeof value !== "object"
    || value === null
    || Array.isArray(value)
    || (value as Record<string, unknown>).type !== MODAL_FRAME_BOUNDARY_STATE_MESSAGE
  ) return undefined;
  const active = (value as Record<string, unknown>).active;
  return typeof active === "boolean" ? active : undefined;
}
