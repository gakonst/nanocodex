import {
  useLayoutEffect,
  type RefObject,
} from "react";

const MODAL_FOCUSABLE_SELECTOR = 'a[href],area[href],audio[controls],button:not(:disabled),[contenteditable]:not([contenteditable="false"]),details > summary:first-of-type,embed,iframe,input:not(:disabled):not([type="hidden"]),object,select:not(:disabled),textarea:not(:disabled),video[controls],[tabindex]:not([tabindex="-1"])';

type ScrollStyle = {
  overflow: string;
  overscrollBehavior: string;
};

type ScrollOwner = {
  style: ScrollStyle;
};

export type OutsideInertOwner = {
  refresh(): void;
  restore(): void;
};

export function lockDocumentScroll(
  root: ScrollOwner,
  body: ScrollOwner,
): () => void {
  const rootStyle = root.style;
  const bodyStyle = body.style;
  const previous = [
    rootStyle.overflow,
    rootStyle.overscrollBehavior,
    bodyStyle.overflow,
    bodyStyle.overscrollBehavior,
  ];
  rootStyle.overflow = bodyStyle.overflow = "hidden";
  rootStyle.overscrollBehavior = bodyStyle.overscrollBehavior = "none";
  return () => {
    rootStyle.overflow = previous[0];
    rootStyle.overscrollBehavior = previous[1];
    bodyStyle.overflow = previous[2];
    bodyStyle.overscrollBehavior = previous[3];
  };
}

export function createOutsideInertOwner(
  boundary: HTMLElement,
  root: HTMLElement,
  exemptions: readonly HTMLElement[] = [],
): OutsideInertOwner {
  const previous = new Map<HTMLElement, boolean>();
  const refresh = () => {
    let current: HTMLElement | null = boundary;
    while (current && current !== root) {
      const parent: HTMLElement | null = current.parentElement;
      if (!parent) break;
      for (const sibling of parent.children) {
        if (
          sibling === current
          || exemptions.includes(sibling as HTMLElement)
          || !("inert" in sibling)
        ) continue;
        const element = sibling as HTMLElement;
        if (!previous.has(element)) previous.set(element, element.inert);
        element.inert = true;
      }
      current = parent;
    }
  };
  refresh();
  return {
    refresh,
    restore() {
      for (const [element, inert] of previous) element.inert = inert;
      previous.clear();
    },
  };
}

export function wrappedModalFocusIndex({
  activeIndex,
  focusableCount,
  shiftKey,
}: {
  activeIndex: number;
  focusableCount: number;
  shiftKey: boolean;
}): number | undefined {
  if (focusableCount <= 0) return undefined;
  if (activeIndex < 0) return shiftKey ? focusableCount - 1 : 0;
  if (shiftKey && activeIndex === 0) return focusableCount - 1;
  if (!shiftKey && activeIndex === focusableCount - 1) return 0;
  return undefined;
}

export function containModalFocus(event: KeyboardEvent, panel: HTMLElement) {
  if (event.key !== "Tab") return;
  const focusable = modalFocusableElements(panel);
  const active = deepActiveElement(panel.ownerDocument);
  const activeIndex = focusable.findIndex((element) => element === active);
  const nextIndex = wrappedModalFocusIndex({
    activeIndex: isWithinDeepRoot(panel, active) ? activeIndex : -1,
    focusableCount: focusable.length,
    shiftKey: event.shiftKey,
  });
  if (nextIndex === undefined) return;
  event.preventDefault();
  focusable[nextIndex]?.focus();
}

export function focusModal(panel: HTMLElement, preferred?: HTMLElement | null) {
  const active = deepActiveElement(panel.ownerDocument);
  if (isWithinDeepRoot(panel, active)) return;
  const target = preferred ?? modalFocusableElements(panel)[0] ?? panel;
  target.focus();
}

export function modalContainsFocus(panel: HTMLElement, event: FocusEvent): boolean {
  const target = event.composedPath()[0];
  return isElementLike(target) && isWithinDeepRoot(panel, target);
}

export function restoreModalFocus(
  primary?: HTMLElement | null,
  fallback?: HTMLElement | null,
): boolean {
  for (const target of [primary, fallback]) {
    if (!target || !canRestoreModalFocus(target)) continue;
    target.focus({ preventScroll: true });
    const active = deepActiveElement(target.ownerDocument);
    if (active === target || isWithinDeepRoot(target, active)) return true;
  }
  return false;
}

export function modalFocusableElements(
  root: Document | ShadowRoot | HTMLElement,
): HTMLElement[] {
  const elements: HTMLElement[] = [];
  for (const element of root.querySelectorAll<HTMLElement>("*")) {
    if (
      element.matches(MODAL_FOCUSABLE_SELECTOR)
      && element.tabIndex >= 0
      && !element.matches(":disabled")
      && isRenderedForFocus(element)
    ) elements.push(element);
    if (element.shadowRoot) elements.push(...modalFocusableElements(element.shadowRoot));
  }
  return orderModalTabSequence(
    elements.filter((element) => isRadioTabStop(element, elements)),
  );
}

export function orderModalTabSequence<T extends { tabIndex: number }>(
  elements: readonly T[],
): T[] {
  return [...elements]
    .sort((left, right) => {
      const leftPositive = left.tabIndex > 0;
      const rightPositive = right.tabIndex > 0;
      if (leftPositive !== rightPositive) return leftPositive ? -1 : 1;
      return leftPositive ? left.tabIndex - right.tabIndex : 0;
    });
}

export function deepActiveElement(root: Document | ShadowRoot): Element | null {
  let active = root.activeElement;
  while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
  return active;
}

function canRestoreModalFocus(target: HTMLElement): boolean {
  return target.isConnected
    && isRenderedForFocus(target)
    && !target.matches(":disabled");
}

function isRenderedForFocus(element: HTMLElement): boolean {
  if (
    element.closest("[hidden], [inert]")
    || element.getClientRects().length === 0
  ) return false;
  const style = element.ownerDocument.defaultView?.getComputedStyle(element);
  return style?.display !== "none"
    && style?.visibility !== "hidden"
    && style?.visibility !== "collapse"
    && style?.contentVisibility !== "hidden";
}

function isRadioTabStop(
  element: HTMLElement,
  candidates: readonly HTMLElement[],
): boolean {
  if (!element.matches('input[type="radio"]')) return true;
  const radio = element as HTMLInputElement;
  if (!radio.name) return true;
  const root = radio.getRootNode();
  const group = candidates.filter((candidate): candidate is HTMLInputElement => {
    if (!candidate.matches('input[type="radio"]')) return false;
    const grouped = candidate as HTMLInputElement;
    return grouped.name === radio.name
      && grouped.form === radio.form
      && grouped.getRootNode() === root;
  });
  return (group.find((candidate) => candidate.checked) ?? group[0]) === radio;
}

function isWithinDeepRoot(container: Element, element: Element | null): boolean {
  let current = element;
  while (current) {
    if (container.contains(current)) return true;
    const root = current.getRootNode() as ShadowRoot;
    current = root.host ?? null;
  }
  return false;
}

function isElementLike(value: unknown): value is Element {
  return typeof value === "object"
    && value !== null
    && "getRootNode" in value;
}

export function useModalBoundary({
  backdropRef,
  fallbackFocusRef,
  initialFocusRef,
  onDismiss,
  open,
  panelRef,
  returnFocusRef,
}: {
  backdropRef?: RefObject<HTMLElement | null>;
  fallbackFocusRef?: RefObject<HTMLElement | null>;
  initialFocusRef?: RefObject<HTMLElement | null>;
  onDismiss(): void;
  open: boolean;
  panelRef: RefObject<HTMLElement | null>;
  returnFocusRef?: RefObject<HTMLElement | null>;
}) {
  useLayoutEffect(() => {
    if (!open) return;
    const panel = panelRef.current;
    if (!panel) return;
    const document = panel.ownerDocument;
    const window = document.defaultView;
    if (!window) return;

    const restoreScroll = lockDocumentScroll(
      document.documentElement,
      document.body,
    );
    const backdrop = backdropRef?.current;
    const inertOwner = createOutsideInertOwner(
      panel,
      document.body,
      backdrop ? [backdrop] : [],
    );
    const observer = new MutationObserver(inertOwner.refresh);
    observer.observe(document.body, { childList: true, subtree: true });
    const focusFrame = window.requestAnimationFrame(() => {
      focusModal(panel, initialFocusRef?.current);
    });
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onDismiss();
        return;
      }
      containModalFocus(event, panel);
    };
    const onFocusIn = (event: FocusEvent) => {
      if (!modalContainsFocus(panel, event)) {
        focusModal(panel, initialFocusRef?.current);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn, true);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn, true);
      observer.disconnect();
      inertOwner.restore();
      restoreScroll();
      restoreModalFocus(returnFocusRef?.current, fallbackFocusRef?.current);
    };
  }, [
    backdropRef,
    fallbackFocusRef,
    initialFocusRef,
    onDismiss,
    open,
    panelRef,
    returnFocusRef,
  ]);
}
