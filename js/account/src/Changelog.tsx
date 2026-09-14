import { useQuery } from "@tanstack/react-query";
import type { MouseEvent as ReactMouseEvent } from "react";
import { appQueryClient } from "./queryClient";
import { changelogQueryOptions } from "./changelogQueries";
import {
  type ChangelogCategory,
} from "./changelogData";
import { pathForCommit } from "./navigation";
import "./Changelog.css";

const categories: readonly ChangelogCategory[] = [
  "New Features",
  "Improvements",
  "Bug Fixes",
];

export function preloadChangelog(): Promise<void> {
  return appQueryClient.prefetchQuery(changelogQueryOptions());
}

export function Changelog({
  onCommitClick,
}: {
  onCommitClick(event: ReactMouseEvent<HTMLAnchorElement>, hash: string): void;
}) {
  const query = useQuery(changelogQueryOptions());

  if (query.isPending) return <section className="changelog-page" role="status">Loading changelog…</section>;
  if (!query.data) {
    return (
      <section className="changelog-error" role="alert">
        <h1>Changelog unavailable.</h1>
        <p>The immutable nightly commit record could not be loaded.</p>
        <button
          type="button"
          onClick={() => void query.refetch()}
        >
          Try again
        </button>
      </section>
    );
  }

  const changelog = query.data;
  return (
    <div className="changelog-page">
      {query.isRefetchError && <p role="alert">Couldn’t refresh the changelog. <button type="button" onClick={() => void query.refetch()}>Retry</button></p>}
      <header className="changelog-title">
        <h1>Changelog</h1>
      </header>
      <article className="changelog-nightly">
        <header>
          <h2>Nightly</h2>
          <time dateTime={changelog.date}>{formatDate(changelog.date)}</time>
          <a
            href={pathForCommit(changelog.revision)}
            onClick={(event) => onCommitClick(event, changelog.revision)}
          >
            revision {changelog.revision.slice(0, 7)}
          </a>
        </header>
        <div className="changelog-categories">
          {categories.map((category) => {
            const entries = changelog.entries.filter(
              (entry) => entry.category === category,
            );
            return (
              <section key={category}>
                <h3>{category}</h3>
                {entries.length > 0 ? (
                  <ul>
                    {entries.map((entry) => (
                      <li key={entry.hash}>
                        <p>
                          <a
                            href={pathForCommit(entry.hash)}
                            onClick={(event) => onCommitClick(event, entry.hash)}
                          >
                            <strong>{entry.title}:</strong>
                          </a>{" "}
                          {entry.description}
                        </p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="changelog-empty">No entries.</p>
                )}
              </section>
            );
          })}
        </div>
      </article>
    </div>
  );
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat("en-US", {
    day: "numeric",
    month: "short",
    timeZone: "UTC",
    year: "numeric",
  }).format(new Date(`${value}T00:00:00Z`));
}
