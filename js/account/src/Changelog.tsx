import {
  startTransition,
  use,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  loadNightlyChangelog,
  type ChangelogCategory,
  type NightlyChangelog,
} from "./changelogData";
import { pathForCommit } from "./navigation";
import "./Changelog.css";

const categories: readonly ChangelogCategory[] = [
  "New Features",
  "Improvements",
  "Bug Fixes",
];

type ChangelogResult =
  | { changelog: NightlyChangelog; state: "ready" }
  | { state: "failed" };

let changelogRequest: Promise<ChangelogResult> | undefined;

export function preloadChangelog(): Promise<ChangelogResult> {
  return changelogRequest ??= loadNightlyChangelog()
    .then((changelog) => ({ changelog, state: "ready" as const }))
    .catch(() => ({ state: "failed" as const }));
}

export function Changelog({
  onCommitClick,
}: {
  onCommitClick(event: ReactMouseEvent<HTMLAnchorElement>, hash: string): void;
}) {
  const [request, setRequest] = useState(preloadChangelog);
  const result = use(request);

  if (result.state === "failed") {
    return (
      <section className="changelog-error" role="alert">
        <h1>Changelog unavailable.</h1>
        <p>The immutable nightly commit record could not be loaded.</p>
        <button
          type="button"
          onClick={() => {
            changelogRequest = undefined;
            startTransition(() => setRequest(preloadChangelog()));
          }}
        >
          Try again
        </button>
      </section>
    );
  }

  const { changelog } = result;
  return (
    <div className="changelog-page">
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
