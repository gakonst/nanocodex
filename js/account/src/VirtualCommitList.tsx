import { useVirtualizer } from "@tanstack/react-virtual";
import { ChevronRight } from "lucide-react";
import { memo, useEffect, useRef } from "react";
import type { HarnessCommit } from "./threadRepositorySnapshot";
import "./Commits.css";

type VirtualCommitListProps = {
  commits: HarnessCommit[];
  hasMore: boolean;
  selectedHash?: string;
  onClearSearch(): void;
  onLoadMore(): void;
  onSelectCommit(commit: HarnessCommit): void;
};

const dateFormatter = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

const relativeFormatter = new Intl.RelativeTimeFormat("en", {
  numeric: "auto",
});

function relativeDate(value: string) {
  const milliseconds = new Date(value).getTime() - Date.now();
  const hours = Math.round(milliseconds / 3_600_000);
  if (Math.abs(hours) < 24) return relativeFormatter.format(hours, "hour");
  const days = Math.round(milliseconds / 86_400_000);
  if (Math.abs(days) < 30) return relativeFormatter.format(days, "day");
  return dateFormatter.format(new Date(value));
}

export const VirtualCommitList = memo(function VirtualCommitList({
  commits,
  hasMore,
  selectedHash,
  onClearSearch,
  onLoadMore,
  onSelectCommit,
}: VirtualCommitListProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const loadMoreRequestedRef = useRef(false);
  const virtualizer = useVirtualizer({
    count: commits.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 80,
    getItemKey: (index) => commits[index]?.hash ?? index,
    overscan: 8,
  });

  useEffect(() => {
    loadMoreRequestedRef.current = false;
  }, [commits.length]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      const list = listRef.current;
      if (
        !hasMore ||
        list == null ||
        loadMoreRequestedRef.current ||
        list.scrollHeight > list.clientHeight + 240
      ) {
        return;
      }
      loadMoreRequestedRef.current = true;
      onLoadMore();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [commits.length, hasMore, onLoadMore]);

  return (
    <div
      className="commit-list"
      ref={listRef}
      onScroll={(event) => {
        const list = event.currentTarget;
        if (
          !hasMore ||
          loadMoreRequestedRef.current ||
          list.scrollTop + list.clientHeight < list.scrollHeight - 240
        ) {
          return;
        }
        loadMoreRequestedRef.current = true;
        onLoadMore();
      }}
    >
      {commits.length ? (
        <div
          style={{
            position: "relative",
            height: `${virtualizer.getTotalSize()}px`,
          }}
        >
          {virtualizer.getVirtualItems().map((virtualRow) => {
            const commit = commits[virtualRow.index];
            if (!commit) return null;
            const isSelected = commit.hash === selectedHash;
            return (
              <button
                className={isSelected ? "commit-row is-selected" : "commit-row"}
                type="button"
                key={commit.hash}
                aria-current={isSelected ? "location" : undefined}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  transform: `translateY(${virtualRow.start}px)`,
                }}
                onClick={() => onSelectCommit(commit)}
              >
                <span className="commit-meta">
                  <span>{commit.shortHash}</span>
                  <span>{relativeDate(commit.authoredAt)}</span>
                </span>
                <strong>{commit.subject}</strong>
                <span className="commit-byline">
                  <span>
                    {commit.author} · {commit.stats.files} file
                    {commit.stats.files === 1 ? "" : "s"}
                  </span>
                  <span className="commit-diff-stats">
                    <span
                      className="commit-additions"
                      aria-label={`${commit.stats.additions} additions`}
                    >
                      +{commit.stats.additions}
                    </span>
                    <span
                      className="commit-deletions"
                      aria-label={`${commit.stats.deletions} deletions`}
                    >
                      −{commit.stats.deletions}
                    </span>
                  </span>
                </span>
                <ChevronRight className="commit-chevron" aria-hidden="true" />
              </button>
            );
          })}
        </div>
      ) : (
        <div className="empty-state">
          <p>No commits match this filter.</p>
          <button type="button" onClick={onClearSearch}>
            Clear search
          </button>
        </div>
      )}
    </div>
  );
});
