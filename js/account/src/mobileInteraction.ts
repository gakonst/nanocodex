export const COARSE_POINTER_QUERY = "(pointer: coarse), (any-pointer: coarse)";
export const MINIMUM_COARSE_TARGET_SIZE = 44;
export const COMPACT_SOURCE_TREE_ITEM_HEIGHT = 24;
export const TERMINAL_COMPOSER_BASE_HEIGHT = 62;

export function visualViewportKeyboardInset({
  baselineHeight,
  viewportHeight,
  viewportOffsetTop,
}: {
  baselineHeight: number;
  viewportHeight: number;
  viewportOffsetTop: number;
}): number {
  return Math.max(
    0,
    finiteNonNegative(baselineHeight)
      - finiteNonNegative(viewportHeight)
      - finiteNonNegative(viewportOffsetTop),
  );
}

export function terminalComposerAction(running: boolean, draft: string): "send" | "stop" {
  return running && !draft.trim() ? "stop" : "send";
}

type MediaQueryMatchSource = Pick<
  MediaQueryList,
  "addEventListener" | "matches" | "removeEventListener"
>;

export function observeMediaQueryMatch(
  query: MediaQueryMatchSource,
  onChange: (matches: boolean) => void,
): () => void {
  const update = () => onChange(query.matches);
  update();
  query.addEventListener("change", update);
  return () => query.removeEventListener("change", update);
}

export function sourceTreeItemHeight(coarsePointer: boolean): number {
  return coarsePointer
    ? MINIMUM_COARSE_TARGET_SIZE
    : COMPACT_SOURCE_TREE_ITEM_HEIGHT;
}

type SourceTreeStateReader = {
  getFocusedPath(): string | null;
  getItem(path: string): {
    isDirectory(): boolean;
    isExpanded?(): boolean;
  } | null;
  getSearchValue(): string;
  getSelectedPaths(): readonly string[];
  isSearchOpen(): boolean;
};

export function retainedSourceTreeState(
  model: SourceTreeStateReader,
  directoryPaths: readonly string[],
) {
  return {
    expandedPaths: directoryPaths.filter((path) => {
      const item = model.getItem(path);
      return item?.isDirectory() === true && item.isExpanded?.() === true;
    }),
    focusedPath: model.getFocusedPath(),
    searchQuery: model.isSearchOpen() ? model.getSearchValue() : null,
    selectedPaths: [...model.getSelectedPaths()],
  };
}

export function scaledSourceTreeScrollTop({
  itemHeight,
  nextItemHeight,
  scrollTop,
}: {
  itemHeight: number;
  nextItemHeight: number;
  scrollTop: number;
}): number {
  const current = finiteNonNegative(itemHeight);
  const next = finiteNonNegative(nextItemHeight);
  const scroll = finiteNonNegative(scrollTop);
  return current > 0 ? scroll / current * next : scroll;
}

export function terminalComposerMinimumHeight({
  measuredComposerHeight,
  safeAreaInsetBottom,
}: {
  measuredComposerHeight: number;
  safeAreaInsetBottom: number;
}): number {
  const measured = finiteNonNegative(measuredComposerHeight);
  const safeArea = finiteNonNegative(safeAreaInsetBottom);
  return Math.ceil(Math.max(
    measured,
    TERMINAL_COMPOSER_BASE_HEIGHT + safeArea,
  ));
}

export function cssPixelValue(value: string): number {
  const parsed = Number.parseFloat(value);
  return finiteNonNegative(parsed);
}

function finiteNonNegative(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}
