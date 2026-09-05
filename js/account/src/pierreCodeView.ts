import type { CodeViewLayout, ThemesType } from "@pierre/diffs";

export const CODE_VIEW_LAYOUT: CodeViewLayout = {
  paddingTop: 0,
  gap: 1,
  paddingBottom: 0,
};

export const CODE_VIEW_THEMES = {
  light: "pierre-light",
  dark: "pierre-dark-soft",
} satisfies ThemesType;

export const COMPACT_WORKSPACE_QUERY =
  "(max-width: 740px), (pointer: coarse) and (orientation: landscape) and (max-width: 950px)";

export const CODE_VIEW_FILE_TREE_ITEM_HEIGHT = 24;
export const CODE_VIEW_BATCH_COUNT = 25;
export const CODE_VIEW_BATCH_COUNT_MAX = 96;
export const COMMIT_INITIAL_BATCH_COUNT = 1;

export function getInitialBatchSize(): number {
  const viewportHeight = window.visualViewport?.height ?? window.innerHeight;
  if (!Number.isFinite(viewportHeight) || viewportHeight <= 0) {
    return CODE_VIEW_BATCH_COUNT;
  }

  return Math.min(
    CODE_VIEW_BATCH_COUNT_MAX,
    Math.max(
      CODE_VIEW_BATCH_COUNT,
      Math.ceil(viewportHeight / CODE_VIEW_FILE_TREE_ITEM_HEIGHT),
    ),
  );
}

export function observePierreCodeScrollRegions(
  container: HTMLElement,
  onPublish?: () => void,
): () => void {
  const shadowObservers = new Map<ShadowRoot, MutationObserver>();
  let animationFrame: number | undefined;

  const exposeColumns = (root: ShadowRoot) => {
    for (const column of root.querySelectorAll<HTMLElement>("code[data-code]")) {
      if (column.tabIndex !== 0) column.tabIndex = 0;
    }
  };
  const scheduleScan = () => {
    if (animationFrame !== undefined) return;
    animationFrame = requestAnimationFrame(() => {
      animationFrame = undefined;
      exposeDiffs();
    });
  };
  const exposeDiffs = () => {
    for (const [root, observer] of shadowObservers) {
      if (container.contains(root.host)) continue;
      observer.disconnect();
      shadowObservers.delete(root);
    }
    for (const host of container.querySelectorAll<HTMLElement>("diffs-container")) {
      const root = host.shadowRoot;
      if (!root) {
        scheduleScan();
        continue;
      }
      exposeColumns(root);
      if (shadowObservers.has(root)) continue;
      const observer = new MutationObserver(() => {
        exposeColumns(root);
        onPublish?.();
      });
      observer.observe(root, {
        attributes: true,
        attributeFilter: ["tabindex"],
        childList: true,
        subtree: true,
      });
      shadowObservers.set(root, observer);
    }
    onPublish?.();
  };

  exposeDiffs();
  const containerObserver = new MutationObserver(exposeDiffs);
  containerObserver.observe(container, { childList: true, subtree: true });
  return () => {
    containerObserver.disconnect();
    if (animationFrame !== undefined) cancelAnimationFrame(animationFrame);
    for (const observer of shadowObservers.values()) observer.disconnect();
  };
}

export const CODE_VIEW_CUSTOM_CSS = `
[data-code]:focus-visible {
  outline: none;
  box-shadow: inset 0 0 0 2px var(--blue);
}

[data-diffs-header] {
  container-type: scroll-state;
  container-name: sticky-header;
}

@container sticky-header scroll-state(stuck: top) {
  [data-diffs-header]::after {
    position: absolute;
    bottom: -1px;
    left: 0;
    width: 100%;
    height: 1px;
    content: '';
    background-color: var(--border-soft);
  }
}
`;
