import { execFileSync } from "node:child_process";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { prepareFileTreeInput } from "@pierre/trees";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryPath = resolve(
  process.env.NANOCODEX_REPO ?? resolve(projectRoot, ".."),
);
const outputPath = resolve(projectRoot, "src", "data", "harness-repository.json");
const commitPatchPath = resolve(projectRoot, "public", "data", "commits.diff");
const legacyPatchDirectory = resolve(projectRoot, "public", "data", "patches");
const blobDirectory = resolve(projectRoot, "public", "data", "blobs");
const requestedCommitLimit = process.env.NANOCODEX_COMMIT_LIMIT;
const projectPath = relative(repositoryPath, projectRoot).replaceAll("\\", "/");
const projectIsInRepository =
  projectPath === "" ||
  (projectPath !== ".." && !projectPath.startsWith("../"));
const projectPrefix = projectPath === "" ? "" : `${projectPath}/`;
const generatedDataPrefixes = projectIsInRepository
  ? [`${projectPrefix}public/data/`, `${projectPrefix}src/data/`]
  : [];
const sourcePathspec = [
  ".",
  ...generatedDataPrefixes.map((prefix) => `:(exclude)${prefix}**`),
];

function git(args, { optional = false } = {}) {
  try {
    return execFileSync("git", args, {
      cwd: repositoryPath,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", optional ? "ignore" : "inherit"],
    }).trimEnd();
  } catch (error) {
    if (optional) return "";
    throw error;
  }
}

function gitBuffer(args) {
  return execFileSync("git", args, {
    cwd: repositoryPath,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "inherit"],
  });
}

function isText(buffer) {
  if (buffer.includes(0)) return false;
  const sample = buffer.subarray(0, Math.min(buffer.length, 8_192));
  const controlBytes = sample.reduce(
    (count, byte) => count + (byte < 32 && byte !== 9 && byte !== 10 && byte !== 13 ? 1 : 0),
    0,
  );
  return sample.length === 0 || controlBytes / sample.length < 0.02;
}

function parseRepositoryIdentity(remote) {
  const match = remote.match(/[:/]([^/:]+)\/([^/]+?)(?:\.git)?$/);
  return { fullName: match ? `${match[1]}/${match[2]}` : "gakonst/nanocodex" };
}

function parseChangedFiles(hash) {
  const statuses = new Map();
  const statusRows = git([
    "diff-tree",
    "--root",
    "--no-commit-id",
    "--name-status",
    "-r",
    "-M",
    hash,
    "--",
    ...sourcePathspec,
  ]);

  for (const row of statusRows.split("\n").filter(Boolean)) {
    const fields = row.split("\t");
    const status = fields[0];
    const path = fields.at(-1);
    if (!path) continue;
    statuses.set(path, {
      status: status[0],
      previousPath: fields.length === 3 ? fields[1] : null,
    });
  }

  const numstatRows = git([
    "diff-tree",
    "--root",
    "--no-commit-id",
    "--numstat",
    "-r",
    "-M",
    hash,
    "--",
    ...sourcePathspec,
  ]);

  return numstatRows
    .split("\n")
    .filter(Boolean)
    .map((row) => {
      const [rawAdditions, rawDeletions, ...pathParts] = row.split("\t");
      const path = pathParts.at(-1) ?? "unknown";
      const status = statuses.get(path);
      return {
        path,
        previousPath: status?.previousPath ?? null,
        status: status?.status ?? "M",
        additions: rawAdditions === "-" ? null : Number(rawAdditions),
        deletions: rawDeletions === "-" ? null : Number(rawDeletions),
      };
    });
}

const format = "%H%x00%h%x00%P%x00%an%x00%aI%x00%D%x00%s%x00%b%x1e";
const logArgs = ["log"];
if (requestedCommitLimit) {
  logArgs.push(`-${Number.parseInt(requestedCommitLimit, 10)}`);
}
logArgs.push(`--format=${format}`);
const rawLog = git(logArgs);
const commitPatches = [];
const commits = rawLog
  .split("\x1e")
  .map((record) => record.replace(/^\n+|\n+$/g, ""))
  .filter(Boolean)
  .map((record) => {
    const [hash, shortHash, parentField, author, authoredAt, refField, subject, body = ""] =
      record.split("\x00");
    const files = parseChangedFiles(hash);
    const additions = files.reduce((total, file) => total + (file.additions ?? 0), 0);
    const deletions = files.reduce((total, file) => total + (file.deletions ?? 0), 0);
    const patch = git([
      "show",
      "--format=",
      "--no-ext-diff",
      "--no-color",
      "--find-renames",
      "--find-copies",
      "--unified=3",
      hash,
      "--",
      ...sourcePathspec,
    ]);
    commitPatches.push(
      `From ${hash} Mon Sep 17 00:00:00 2001\n${patch}\n`,
    );

    return {
      hash,
      shortHash,
      parents: parentField.split(" ").filter(Boolean),
      author,
      authoredAt,
      refs: refField
        .split(",")
        .map((ref) => ref.trim())
        .filter(Boolean),
      subject,
      body: body.trim(),
      files,
      stats: { files: files.length, additions, deletions },
    };
  });

const remote = git(["remote", "get-url", "origin"], { optional: true });
const identity = parseRepositoryIdentity(remote);
const head = git(["rev-parse", "HEAD"]);
const dirtyRows = git(["status", "--porcelain"], { optional: true })
  .split("\n")
  .filter(Boolean);
const blobFiles = [];
const tree = git(["ls-tree", "-r", "-z", "-l", head])
  .split("\0")
  .filter(Boolean)
  .map((row) => {
    const match = row.match(/^(\d+) blob ([0-9a-f]+)\s+(\d+|-)\t([\s\S]+)$/);
    if (!match) throw new Error(`Could not parse git tree row: ${row}`);
    const [, mode, objectId, rawSize, path] = match;
    return { mode, objectId, rawSize, path };
  })
  .filter(
    ({ path }) =>
      !generatedDataPrefixes.some((prefix) => path.startsWith(prefix)),
  )
  .map(({ mode, objectId, rawSize, path }) => {
    const contents = gitBuffer(["cat-file", "blob", objectId]);
    const viewable = isText(contents);
    if (viewable) blobFiles.push({ objectId, contents });
    return {
      path,
      mode,
      objectId,
      size: rawSize === "-" ? null : Number(rawSize),
      contentUrl: viewable ? `/data/blobs/${objectId}.txt` : null,
    };
  });
const treeInput = prepareFileTreeInput(
  tree.map((file) => file.path),
  { flattenEmptyDirectories: true },
);

const snapshot = {
  repository: {
    ...identity,
    branch: git(["branch", "--show-current"]) || "detached",
    head,
    totalCommits: Number(git(["rev-list", "--count", "HEAD"])),
    dirty: dirtyRows.length > 0,
    dirtyCount: dirtyRows.length,
  },
  generatedAt: new Date().toISOString(),
  commitPatchUrl: "/data/commits.diff",
  tree,
  treeInput,
  commits,
};

await mkdir(dirname(outputPath), { recursive: true });
await mkdir(dirname(commitPatchPath), { recursive: true });
await rm(legacyPatchDirectory, { recursive: true, force: true });
await rm(blobDirectory, { recursive: true, force: true });
await mkdir(blobDirectory, { recursive: true });
await Promise.all(
  [
    writeFile(commitPatchPath, commitPatches.join(""), "utf8"),
    ...blobFiles.map(({ objectId, contents }) =>
      writeFile(resolve(blobDirectory, `${objectId}.txt`), contents),
    ),
  ],
);
await writeFile(outputPath, `${JSON.stringify(snapshot)}\n`, "utf8");
console.log(
  `Synced ${tree.length} files and ${commits.length} commits from ${identity.fullName} (${snapshot.repository.head.slice(0, 7)})`,
);
