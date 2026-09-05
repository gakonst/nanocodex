import { createTwoFilesPatch, diffLines } from "diff";
import git, { type ReadCommitResult, type WalkerEntry } from "isomorphic-git";

import {
  getBrowserThread,
  inspectThreadGit,
  type OpfsGitFs,
} from "nanocodex/tools/browser";

const directory = "/workspace";
const textDecoder = new TextDecoder("utf-8", { fatal: true });
export const MAX_COMMIT_HISTORY = 200;
export const MAX_DIFF_FILE_BYTES = 1024 * 1024;
export const MAX_COMMIT_PATCH_BYTES = 4 * 1024 * 1024;
const MAX_CACHED_DIFF_BLOBS = 32;
const PATCH_TRUNCATION_NOTICE = "# Patch output truncated at 4 MiB.\n";

export type RepositoryFile = {
  path: string;
  mode: string;
  objectId: string;
  size: number | null;
};

export type ChangedFile = {
  path: string;
  previousPath: string | null;
  status: string;
  binary: boolean;
  additions: number | null;
  deletions: number | null;
};

export type HarnessCommit = {
  hash: string;
  shortHash: string;
  parents: string[];
  author: string;
  authoredAt: string;
  refs: string[];
  subject: string;
  body: string;
  files: ChangedFile[];
  stats: {
    files: number;
    additions: number;
    deletions: number;
  };
};

export type RepositorySnapshot = {
  repository: {
    fullName: string;
    branch: string;
    head: string;
    totalCommits: number;
    dirty: boolean;
    dirtyCount: number;
  };
  generatedAt: string;
  historyLoaded: boolean;
  commitPatchUrl: string | null;
  tree: RepositoryFile[];
  readFile(file: RepositoryFile): Promise<string>;
  commits: HarnessCommit[];
  release(): void;
};

export async function loadThreadRepositorySnapshot(
  includeHistory = true,
): Promise<RepositorySnapshot> {
  const thread = getBrowserThread();
  const snapshot = await inspectThreadGit(thread, (fs) => buildThreadRepositorySnapshot(
    fs,
    thread.repositoryName,
    thread.branch,
    { includeHistory },
  ));
  const head = snapshot.repository.head === "unborn"
    ? undefined
    : snapshot.repository.head;
  return {
    ...snapshot,
    readFile: (file) => inspectThreadGit(
      thread,
      (fs) => readRepositoryFile(fs, head, file),
    ),
  };
}

export async function buildThreadRepositorySnapshot(
  fs: OpfsGitFs,
  repositoryName: string,
  branch: "nanocodex",
  { includeHistory = true }: { includeHistory?: boolean } = {},
): Promise<RepositorySnapshot> {
  const head = await git.resolveRef({ fs, dir: directory, ref: "HEAD" })
    .catch(() => undefined);
  const resourceUrls: string[] = [];
  const tree = head
    ? await readHeadFiles(fs, head)
    : await readWorktreeFiles(fs);
  const log = head && includeHistory
    ? await git.log({
        fs,
        dir: directory,
        ref: head,
        depth: MAX_COMMIT_HISTORY,
        includeChanges: true,
      })
    : [];
  const { commits, patch } = includeHistory
    ? await readCommits(fs, log, head)
    : { commits: [], patch: "" };
  const commitPatchUrl = includeHistory
    ? resourceUrl(new Blob([patch], { type: "text/x-diff" }), resourceUrls)
    : null;
  const dirtyCount = (await git.statusMatrix({ fs, dir: directory }))
    .filter(([, headStatus, workdirStatus, stageStatus]) =>
      headStatus !== workdirStatus || headStatus !== stageStatus).length;

  return {
    repository: {
      fullName: repositoryName,
      branch,
      head: head ?? "unborn",
      totalCommits: commits.length,
      dirty: dirtyCount > 0,
      dirtyCount,
    },
    generatedAt: new Date().toISOString(),
    historyLoaded: includeHistory,
    commitPatchUrl,
    tree,
    readFile: (file) => readRepositoryFile(fs, head, file),
    commits,
    release() {
      for (const url of resourceUrls) URL.revokeObjectURL(url);
      resourceUrls.length = 0;
    },
  };
}

async function readWorktreeFiles(fs: OpfsGitFs): Promise<RepositoryFile[]> {
  const files: RepositoryFile[] = [];
  const visit = async (relativeDirectory: string): Promise<void> => {
    const absoluteDirectory = relativeDirectory
      ? `${directory}/${relativeDirectory}`
      : directory;
    const names = await fs.promises.readdir(absoluteDirectory);
    for (const name of names.sort()) {
      if (!relativeDirectory && name === ".git") continue;
      const path = relativeDirectory ? `${relativeDirectory}/${name}` : name;
      const stat = await fs.promises.stat(`${directory}/${path}`);
      if (stat.isDirectory()) {
        await visit(path);
        continue;
      }
      files.push({
        path,
        mode: "100644",
        objectId: `worktree:${path}:${stat.size}:${stat.mtimeMs}`,
        size: stat.size,
      });
    }
  };
  await visit("");
  return files;
}

async function readHeadFiles(
  fs: OpfsGitFs,
  head: string,
): Promise<RepositoryFile[]> {
  const files = await git.walk({
    fs,
    dir: directory,
    trees: [git.TREE({ ref: head })],
    map: async (path: string, entries: Array<WalkerEntry | null>) => {
      const entry = entries[0];
      if (path === "." || !entry || await entry.type() !== "blob") return [];
      return [{
        path,
        mode: (await entry.mode()).toString(8).padStart(6, "0"),
        objectId: await entry.oid(),
        size: null,
      } satisfies RepositoryFile];
    },
    reduce: async (parent: RepositoryFile[], children: RepositoryFile[][]) => [
      ...parent,
      ...children.flat(),
    ],
  }) as RepositoryFile[];
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

async function readRepositoryFile(
  fs: OpfsGitFs,
  head: string | undefined,
  file: RepositoryFile,
): Promise<string> {
  const bytes = head
    ? (await git.readBlob({ fs, dir: directory, oid: file.objectId })).blob
    : await fs.promises.readFile(`${directory}/${file.path}`);
  if (typeof bytes === "string") return bytes;
  const text = decodeText(bytes);
  if (text === undefined) throw new Error(`${file.path} is not a text file`);
  return text;
}

async function readCommits(
  fs: OpfsGitFs,
  log: ReadCommitResult[],
  head: string | undefined,
): Promise<{ commits: HarnessCommit[]; patch: string }> {
  const commits: HarnessCommit[] = [];
  const patches: string[] = [];
  const blobCache = new Map<string, { binary: boolean; text: string }>();
  let patchLength = 0;
  let patchTruncated = false;
  for (const entry of log) {
    const files: ChangedFile[] = [];
    const filePatches: string[] = [];
    const commitPatchPrefix = `From ${entry.oid} Mon Sep 17 00:00:00 2001\n`;
    const commitSeparatorLength = patches.length ? 1 : 0;
    const wasPatchOpen = !patchTruncated &&
      patchLength + commitSeparatorLength + commitPatchPrefix.length <=
        MAX_COMMIT_PATCH_BYTES;
    if (!wasPatchOpen) patchTruncated = true;
    let commitPatchLength = commitPatchPrefix.length;
    for (const change of entry.commit.changes ?? []) {
      const [newOid, oldOid, path] = change;
      if (typeof path !== "string") continue;
      const oldBlob = await readTextBlob(fs, oldOid, blobCache);
      const newBlob = await readTextBlob(fs, newOid, blobCache);
      const binary = oldBlob.binary || newBlob.binary;
      const stats = binary
        ? { additions: null, deletions: null }
        : lineStats(oldBlob.text, newBlob.text);
      files.push({
        path,
        previousPath: null,
        status: oldOid == null ? "A" : newOid == null ? "D" : "M",
        binary,
        ...stats,
      });
      if (!patchTruncated) {
        const nextPatch = filePatch(
          path,
          oldOid,
          newOid,
          oldBlob.text,
          newBlob.text,
          binary,
        );
        const fileSeparatorLength = filePatches.length ? 1 : 0;
        if (
          patchLength + commitSeparatorLength + commitPatchLength +
            fileSeparatorLength + nextPatch.length <= MAX_COMMIT_PATCH_BYTES
        ) {
          filePatches.push(nextPatch);
          commitPatchLength += fileSeparatorLength + nextPatch.length;
        } else {
          const noticeSeparatorLength = filePatches.length ? 1 : 0;
          if (
            patchLength + commitSeparatorLength + commitPatchLength +
              noticeSeparatorLength + PATCH_TRUNCATION_NOTICE.length <=
                MAX_COMMIT_PATCH_BYTES
          ) {
            filePatches.push(PATCH_TRUNCATION_NOTICE);
            commitPatchLength += noticeSeparatorLength + PATCH_TRUNCATION_NOTICE.length;
          }
          patchTruncated = true;
        }
      }
    }
    const [subject = "Untitled commit", ...bodyLines] = entry.commit.message.trimEnd().split("\n");
    const additions = files.reduce((sum, file) => sum + (file.additions ?? 0), 0);
    const deletions = files.reduce((sum, file) => sum + (file.deletions ?? 0), 0);
    commits.push({
      hash: entry.oid,
      shortHash: entry.oid.slice(0, 7),
      parents: entry.commit.parent,
      author: entry.commit.author.name,
      authoredAt: new Date(entry.commit.author.timestamp * 1_000).toISOString(),
      refs: entry.oid === head ? ["HEAD -> nanocodex"] : [],
      subject,
      body: bodyLines.join("\n").trim(),
      files,
      stats: { files: files.length, additions, deletions },
    });
    if (wasPatchOpen) {
      const commitPatch = `${commitPatchPrefix}${filePatches.join("\n")}`;
      patches.push(commitPatch);
      patchLength += commitSeparatorLength + commitPatch.length;
    }
  }
  return { commits, patch: patches.join("\n") };
}

async function readTextBlob(
  fs: OpfsGitFs,
  oid: string | null,
  cache: Map<string, { binary: boolean; text: string }>,
): Promise<{ binary: boolean; text: string }> {
  if (oid == null) return { binary: false, text: "" };
  const cached = cache.get(oid);
  if (cached) {
    cache.delete(oid);
    cache.set(oid, cached);
    return cached;
  }
  const bytes = (await git.readBlob({ fs, dir: directory, oid })).blob;
  if (bytes.byteLength > MAX_DIFF_FILE_BYTES || bytes.includes(0)) {
    const blob = { binary: true, text: "" };
    cacheDiffBlob(cache, oid, blob);
    return blob;
  }
  try {
    const blob = { binary: false, text: textDecoder.decode(bytes) };
    cacheDiffBlob(cache, oid, blob);
    return blob;
  } catch {
    const blob = { binary: true, text: "" };
    cacheDiffBlob(cache, oid, blob);
    return blob;
  }
}

function cacheDiffBlob(
  cache: Map<string, { binary: boolean; text: string }>,
  oid: string,
  blob: { binary: boolean; text: string },
): void {
  if (cache.size >= MAX_CACHED_DIFF_BLOBS) {
    const oldest = cache.keys().next().value;
    if (oldest) cache.delete(oldest);
  }
  cache.set(oid, blob);
}

function decodeText(bytes: Uint8Array): string | undefined {
  if (bytes.includes(0)) return undefined;
  try {
    return textDecoder.decode(bytes);
  } catch {
    return undefined;
  }
}

function lineStats(oldText: string, newText: string) {
  let additions = 0;
  let deletions = 0;
  for (const change of diffLines(oldText, newText)) {
    if (change.added) additions += change.count ?? 0;
    if (change.removed) deletions += change.count ?? 0;
  }
  return { additions, deletions };
}

function filePatch(
  path: string,
  oldOid: string | null,
  newOid: string | null,
  oldText: string,
  newText: string,
  binary: boolean,
): string {
  const oldPath = oldOid == null ? "/dev/null" : `a/${path}`;
  const newPath = newOid == null ? "/dev/null" : `b/${path}`;
  const header = [
    `diff --git a/${path} b/${path}`,
    oldOid == null ? "new file mode 100644" : newOid == null ? "deleted file mode 100644" : "",
    oldOid && newOid ? `index ${oldOid.slice(0, 7)}..${newOid.slice(0, 7)} 100644` : "",
  ].filter(Boolean).join("\n");
  if (binary) return `${header}\nBinary files ${oldPath} and ${newPath} differ\n`;
  const unified = createTwoFilesPatch(oldPath, newPath, oldText, newText, "", "", { context: 3 });
  return `${header}\n${unified.replace(/^={3,}\n/, "")}`;
}

function resourceUrl(blob: Blob, urls: string[]): string {
  const url = URL.createObjectURL(blob);
  urls.push(url);
  return url;
}
