import type { HarnessCommit } from "./threadRepositorySnapshot";

export const MAX_CHANGELOG_PAGES = 3;
export const MAX_CHANGELOG_COMMITS = 64;
const COMMIT_PAGE_SIZE = 32;
const MAX_SNAPSHOT_BYTES = 512 * 1024;
const MAX_COMMIT_PAGE_BYTES = 256 * 1024;

export type ChangelogCategory =
  | "New Features"
  | "Improvements"
  | "Bug Fixes";

export type ChangelogEntry = {
  category: ChangelogCategory;
  description: string;
  hash: string;
  title: string;
};

export type NightlyChangelog = {
  date: string;
  entries: ChangelogEntry[];
  revision: string;
};

type Fetch = typeof fetch;
type ChangelogCommit = Pick<
  HarnessCommit,
  "authoredAt" | "body" | "hash" | "subject"
>;

export async function loadNightlyChangelog(
  request: Fetch = fetch,
  development = import.meta.env?.DEV ?? false,
): Promise<NightlyChangelog> {
  const base = "/api/repository";
  const snapshotResponse = await request(`${base}/snapshot`, {
    cache: development ? "no-store" : "default",
  });
  if (!snapshotResponse.ok) {
    throw new Error(`Repository revision request failed (${snapshotResponse.status})`);
  }
  const snapshot = await readBoundedJson(
    snapshotResponse,
    MAX_SNAPSHOT_BYTES,
    "Repository revision",
  );
  const revision = repositoryRevision(snapshot);
  const commits: ChangelogCommit[] = [];
  let nightlyDate: string | undefined;

  for (let page = 0; page < MAX_CHANGELOG_PAGES; page += 1) {
    const response = await request(
      `${base}/commits?page=${page}&generation=${revision}`,
      { cache: development ? "no-store" : "force-cache" },
    );
    if (!response.ok) {
      throw new Error(`Changelog page request failed (${response.status})`);
    }
    const responseGeneration = response.headers.get("x-repository-generation");
    if (responseGeneration != null && responseGeneration !== revision) {
      throw new Error("Repository publication changed while loading changelog");
    }
    const value = await readBoundedJson(
      response,
      MAX_COMMIT_PAGE_BYTES,
      "Changelog page",
    );
    if (!Array.isArray(value) || value.length > COMMIT_PAGE_SIZE) {
      throw new Error("Changelog page is invalid");
    }
    const pageCommits = value.map(requireCommit);
    if (pageCommits.length === 0) break;
    nightlyDate ??= commitDate(pageCommits[0]!);

    let reachedEarlierDate = false;
    for (const commit of pageCommits) {
      if (commitDate(commit) !== nightlyDate) {
        reachedEarlierDate = true;
        break;
      }
      if (commits.length < MAX_CHANGELOG_COMMITS) commits.push(commit);
    }
    if (
      reachedEarlierDate ||
      pageCommits.length < COMMIT_PAGE_SIZE ||
      commits.length >= MAX_CHANGELOG_COMMITS
    ) break;
  }

  if (nightlyDate == null || commits.length === 0) {
    throw new Error("No nightly commits are published");
  }

  return {
    date: nightlyDate,
    entries: commits.map(changelogEntry),
    revision,
  };
}

export function changelogEntry(commit: ChangelogCommit): ChangelogEntry {
  const conventional = commit.subject.match(
    /^(feat|fix|perf|docs|refactor|chore|build|ci|test|style|revert)(?:\(([^)]+)\))?!?:\s*(.+)$/i,
  );
  const kind = conventional?.[1]?.toLowerCase();
  const subject = cleanText(conventional?.[3] ?? commit.subject, 180);
  const body = commit.body
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find(Boolean);
  const title = entryTitle(subject);
  const description = body == null
    ? sentence(subject)
    : sentence(cleanText(body, 180));

  return {
    category: kind === "feat"
      ? "New Features"
      : kind === "fix" || kind === "revert"
      ? "Bug Fixes"
      : "Improvements",
    description,
    hash: commit.hash,
    title,
  };
}

function entryTitle(subject: string): string {
  const words = subject.split(/\s+/).filter(Boolean);
  const action = words[0]?.toLowerCase();
  const actionWords = new Set([
    "add", "allow", "avoid", "build", "change", "clean", "expose", "fix",
    "harden", "improve", "keep", "launch", "make", "move", "own", "preserve",
    "publish", "raise", "recover", "reduce", "remove", "replace", "retry",
    "reuse", "stream", "support", "surface", "unify", "update",
  ]);
  const target = action != null && actionWords.has(action)
    ? words.slice(1)
    : words;
  const boundary = target.findIndex((word, index) =>
    index > 0 && /^(across|after|against|as|before|for|from|in|on|through|to|with|without)$/i.test(word)
  );
  const terse = target.slice(0, boundary < 0 ? undefined : boundary).join(" ") || subject;
  return capitalize(cleanText(terse, 80));
}

function repositoryRevision(value: unknown): string {
  if (
    value == null ||
    typeof value !== "object" ||
    !("repository" in value) ||
    value.repository == null ||
    typeof value.repository !== "object" ||
    !("head" in value.repository) ||
    typeof value.repository.head !== "string" ||
    !/^[a-f0-9]{40}$/.test(value.repository.head)
  ) {
    throw new Error("Repository revision is invalid");
  }
  return value.repository.head;
}

function requireCommit(value: unknown): ChangelogCommit {
  if (
    value == null ||
    typeof value !== "object" ||
    !("hash" in value) ||
    typeof value.hash !== "string" ||
    !/^[a-f0-9]{40}$/.test(value.hash) ||
    !("authoredAt" in value) ||
    typeof value.authoredAt !== "string" ||
    !("subject" in value) ||
    typeof value.subject !== "string" ||
    !("body" in value) ||
    typeof value.body !== "string"
  ) {
    throw new Error("Changelog commit is invalid");
  }
  return {
    authoredAt: value.authoredAt,
    body: value.body,
    hash: value.hash,
    subject: value.subject,
  };
}

function commitDate(commit: ChangelogCommit): string {
  const date = new Date(commit.authoredAt);
  if (!Number.isFinite(date.valueOf())) throw new Error("Changelog date is invalid");
  return date.toISOString().slice(0, 10);
}

async function readBoundedJson(
  response: Response,
  maxBytes: number,
  label: string,
): Promise<unknown> {
  const advertised = Number(response.headers.get("content-length"));
  if (Number.isFinite(advertised) && advertised > maxBytes) {
    await response.body?.cancel();
    throw new Error(`${label} exceeds the data limit`);
  }
  const reader = response.body?.getReader();
  if (reader == null) return response.json() as Promise<unknown>;
  const chunks: Uint8Array[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > maxBytes) {
      await reader.cancel();
      throw new Error(`${label} exceeds the data limit`);
    }
    chunks.push(value);
  }
  const body = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    return JSON.parse(new TextDecoder().decode(body)) as unknown;
  } catch {
    throw new Error(`${label} is invalid JSON`);
  }
}

function cleanText(value: string, limit: number): string {
  const clean = value.replace(/\s+/g, " ").trim().replace(/[.!?]+$/, "");
  if (clean.length <= limit) return clean;
  return `${clean.slice(0, limit - 1).trimEnd()}…`;
}

function capitalize(value: string): string {
  return value ? `${value[0]!.toUpperCase()}${value.slice(1)}` : value;
}

function sentence(value: string): string {
  const capitalized = capitalize(value);
  return /[.!?]$/.test(capitalized) ? capitalized : `${capitalized}.`;
}
