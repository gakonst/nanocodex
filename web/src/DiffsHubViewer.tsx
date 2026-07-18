import {
  DEFAULT_THEMES,
  type CodeViewItem,
  type CodeViewOptions,
  type DiffIndicators,
} from "@pierre/diffs";
import {
  CodeView,
  type CodeViewHandle,
  useStableCallback,
} from "@pierre/diffs/react";
import { ChevronDown } from "lucide-react";
import { memo, type RefObject, useMemo } from "react";
import { CODE_VIEW_CUSTOM_CSS, CODE_VIEW_LAYOUT } from "./pierreCodeView";
import type { Theme } from "./Xedoc";

// Behavioral viewer port from pierrecomputer/pierre@4f94a5e765195b27e1e4188b943aab2ae44613cb
// apps/diffshub/components/DiffsHubViewer.tsx. Commit weaving belongs entirely
// to the item loader; this component deliberately has no commit-specific UI.
interface DiffsHubViewerProps {
  diffIndicators: DiffIndicators;
  diffStyle: "split" | "unified";
  disableWorkerPool: boolean;
  initialItems: CodeViewItem<undefined>[];
  lineNumbers: boolean;
  overflow: "wrap" | "scroll";
  scrollRef: RefObject<HTMLDivElement | null>;
  showBackgrounds: boolean;
  theme: Theme;
  viewerRef: RefObject<CodeViewHandle<undefined> | null>;
}

export const DiffsHubViewer = memo(function DiffsHubViewer({
  diffIndicators,
  diffStyle,
  disableWorkerPool,
  initialItems,
  lineNumbers,
  overflow,
  scrollRef,
  showBackgrounds,
  theme,
  viewerRef,
}: DiffsHubViewerProps) {
  const handleViewerRef = useStableCallback(
    (viewer: CodeViewHandle<undefined> | null) => {
      viewerRef.current = viewer;
    },
  );

  const handleToggleItemCollapsed = useStableCallback((itemId: string) => {
    const { current: viewerHandle } = viewerRef;
    const viewer = viewerHandle?.getInstance();
    const item = viewerHandle?.getItem(itemId);
    if (viewerHandle == null || viewer == null || item == null) return;

    const itemTop = viewer.getTopForItem(itemId);
    item.collapsed = item.collapsed !== true;
    item.version = getNextItemVersion(item);
    if (!viewerHandle.updateItem(item)) return;

    if (itemTop != null && itemTop < viewer.getScrollTop()) {
      viewer.scrollTo({ type: "item", id: item.id, align: "start" });
    }
  });

  const renderHeaderPrefix = useStableCallback(
    (item: CodeViewItem<undefined>) => {
      if (item.type !== "diff") return null;
      return (
        <CollapseDiffButton
          disabled={
            item.fileDiff.splitLineCount === 0 &&
            item.fileDiff.unifiedLineCount === 0
          }
          collapsed={item.collapsed}
          onToggle={() => handleToggleItemCollapsed(item.id)}
        />
      );
    },
  );

  const options: CodeViewOptions<undefined> = useMemo(
    () =>
      ({
        layout: CODE_VIEW_LAYOUT,
        theme: DEFAULT_THEMES,
        themeType: theme,
        diffStyle,
        diffIndicators,
        overflow,
        disableBackground: !showBackgrounds,
        disableLineNumbers: !lineNumbers,
        lineHoverHighlight: "number",
        enableLineSelection: true,
        enableGutterUtility: true,
        stickyHeaders: true,
        unsafeCSS: CODE_VIEW_CUSTOM_CSS,
      }) satisfies CodeViewOptions<undefined>,
    [
      diffIndicators,
      diffStyle,
      lineNumbers,
      overflow,
      showBackgrounds,
      theme,
    ],
  );

  return (
    <CodeView
      ref={handleViewerRef}
      containerRef={scrollRef}
      initialItems={initialItems}
      className="commit-stream code-view cv-scrollbar"
      disableWorkerPool={disableWorkerPool}
      options={options}
      renderHeaderPrefix={renderHeaderPrefix}
    />
  );
});

function getNextItemVersion(item: CodeViewItem<undefined>): number {
  return typeof item.version === "number" ? item.version + 1 : 1;
}

interface CollapseDiffButtonProps {
  disabled?: boolean;
  collapsed?: boolean;
  onToggle(): void;
}

function CollapseDiffButton({
  disabled = false,
  collapsed = false,
  onToggle,
}: CollapseDiffButtonProps) {
  return (
    <button
      type="button"
      disabled={disabled}
      aria-expanded={!disabled && !collapsed}
      aria-hidden={disabled}
      aria-label={
        disabled ? undefined : collapsed ? "Expand diff" : "Collapse diff"
      }
      className="diff-collapse-button"
      onClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        onToggle();
      }}
    >
      <ChevronDown
        aria-hidden="true"
        className={disabled || collapsed ? "is-collapsed" : ""}
      />
    </button>
  );
}
