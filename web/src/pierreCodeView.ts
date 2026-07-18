import type { CodeViewLayout } from "@pierre/diffs";

export const CODE_VIEW_LAYOUT: CodeViewLayout = {
  paddingTop: 0,
  gap: 1,
  paddingBottom: 0,
};

export const CODE_VIEW_FILE_TREE_ITEM_HEIGHT = 24;
export const CODE_VIEW_BATCH_COUNT = 25;
export const CODE_VIEW_BATCH_COUNT_MAX = 96;

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

export const CODE_VIEW_CUSTOM_CSS = `
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
