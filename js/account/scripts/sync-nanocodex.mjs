import { execFile, spawn } from "node:child_process";
import {
  access,
  mkdir,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { redactPublicText } from "./public-redaction.mjs";

const execFileAsync = promisify(execFile);
const generatorVersion = 8;
const commitPageSize = 32;
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryPath = resolve(
  process.env.NANOCODEX_REPO ?? resolve(projectRoot, "../.."),
);
const generatedDataDirectory = resolve(
  process.env.NANOCODEX_DATA_DIR ?? resolve(projectRoot, ".repository-data"),
);
const outputPath = resolve(generatedDataDirectory, "repository.json");
const commitsOutputPath = resolve(generatedDataDirectory, "commits.json");
const commitIndexOutputPath = resolve(generatedDataDirectory, "commit-index.json");
const commitPagesDirectory = resolve(generatedDataDirectory, "commit-pages");
const commitPatchPagesDirectory = resolve(
  generatedDataDirectory,
  "commit-patch-pages",
);
const commitLimit = parseCommitLimit(process.env.NANOCODEX_COMMIT_LIMIT);
const forceSync = process.env.NANOCODEX_FORCE_SYNC === "1";
const emitObjects = process.env.NANOCODEX_EMIT_OBJECTS === "1";
const projectPath = relative(repositoryPath, projectRoot).replaceAll("\\", "/");
const projectIsInRepository =
  projectPath === "" ||
  (projectPath !== ".." && !projectPath.startsWith("../"));
const projectPrefix = projectPath === "" ? "" : `${projectPath}/`;
const generatedDataPrefixes = projectIsInRepository
  ? [
      `${projectPrefix}public/data/`,
      `${projectPrefix}src/data/`,
      `${projectPrefix}.repository-data/`,
    ]
  : [];
const sourcePathspec = [
  ".",
  ...generatedDataPrefixes.map((prefix) => `:(exclude)${prefix}**`),
];
const logFormat = "%x1e%H%x00%h%x00%P%x00%an%x00%aI%x00%D%x00%s%x00%b%x00";

const [head, branch, remote] = await Promise.all([
  git(["rev-parse", "HEAD"]),
  git(["branch", "--show-current"]),
  git(["remote", "get-url", "origin"], { optional: true }),
]);
const identity = parseRepositoryIdentity(remote);
const publicBranch = process.env.NANOCODEX_PUBLIC_BRANCH?.trim();
if (
  publicBranch !== undefined &&
  !/^[A-Za-z0-9][A-Za-z0-9._\/-]*$/.test(publicBranch)
) {
  throw new Error("NANOCODEX_PUBLIC_BRANCH is invalid");
}
const repositoryBranch = publicBranch ?? (branch || "detached");

if (
  !forceSync &&
  await generatedSnapshotIsCurrent({
    branch: repositoryBranch,
    fullName: identity.fullName,
    head,
  })
) {
  console.log(`Repository assets are current (${head.slice(0, 7)})`);
  process.exit(0);
}

const historyPrefix = ["log", ...(commitLimit == null ? [] : [`-${commitLimit}`])];
const [
  numstatLog,
  statusLog,
  rawCommitPatches,
  rawTree,
  totalCommits,
  generatedAt,
] = await Promise.all([
  git([...historyPrefix, `--format=${logFormat}`, "--numstat", "-z", "--find-renames"]),
  git([...historyPrefix, `--format=${logFormat}`, "--name-status", "-z", "--find-renames"]),
  emitObjects
    ? git([
        ...historyPrefix,
        "--format=From %H Mon Sep 17 00:00:00 2001",
        "-p",
        "--no-ext-diff",
        "--no-color",
        "--find-renames",
        "--find-copies",
        "--unified=3",
        "--",
        ...sourcePathspec,
      ])
    : Promise.resolve(""),
  git(["ls-tree", "-r", "-z", "-l", head]),
  git(["rev-list", "--count", head]),
  git(["show", "-s", "--format=%cI", head]),
]);

const commits = projectCommitRefs(
  combineCommitLogs(numstatLog, statusLog),
  head,
  publicBranch,
);
const treeEntries = parseTree(rawTree).filter(
  ({ path }) => !generatedDataPrefixes.some((prefix) => path.startsWith(prefix)),
);
const objects = await readGitObjects(treeEntries.map(({ objectId }) => objectId));
const blobFiles = [];
const tree = treeEntries.map(({ mode, objectId, rawSize, path }) => {
  const contents = objects.get(objectId);
  if (contents == null) throw new Error(`Git did not return blob ${objectId}`);
  const viewable = isText(contents);
  if (viewable) {
    blobFiles.push({
      objectId,
      contents: Buffer.from(redactPublicText(contents.toString("utf8")), "utf8"),
    });
  }
  return {
    path,
    mode,
    objectId,
    size: rawSize === "-" ? null : Number(rawSize),
    contentUrl: viewable ? `/api/repository/blob/${objectId}` : null,
  };
});
const snapshot = {
  generatorVersion,
  commitLimit,
  repository: {
    ...identity,
    branch: repositoryBranch,
    head,
    totalCommits: Number(totalCommits),
    indexedCommits: commits.length,
    commitPageSize,
    dirty: false,
    dirtyCount: 0,
  },
  generatedAt,
  tree,
};
const commitIndex = {
  version: 1,
  repository: snapshot.repository,
  generatedAt,
  hashes: commits.map(({ hash }) => hash),
  scopeCounts: buildCommitScopeCounts(commits),
};

await rm(generatedDataDirectory, { recursive: true, force: true });
await mkdir(generatedDataDirectory, { recursive: true });
await mkdir(commitPagesDirectory, { recursive: true });
const writes = [
  writeIfChanged(outputPath, Buffer.from(`${JSON.stringify(snapshot)}\n`, "utf8")),
  writeIfChanged(commitsOutputPath, Buffer.from(`${JSON.stringify(commits)}\n`, "utf8")),
  writeIfChanged(
    commitIndexOutputPath,
    Buffer.from(`${JSON.stringify(commitIndex)}\n`, "utf8"),
  ),
  ...Array.from(
    { length: Math.ceil(commits.length / commitPageSize) },
    (_, page) => writeIfChanged(
      resolve(commitPagesDirectory, `${String(page).padStart(4, "0")}.json`),
      Buffer.from(
        `${JSON.stringify(commits.slice(page * commitPageSize, (page + 1) * commitPageSize))}\n`,
        "utf8",
      ),
    ),
  ),
];
if (emitObjects) {
  const patches = splitCommitPatches(commits, rawCommitPatches).map(
    ({ hash, contents }) => ({ hash, contents: redactPublicText(contents) }),
  );
  await Promise.all([
    mkdir(resolve(generatedDataDirectory, "blobs"), { recursive: true }),
    mkdir(resolve(generatedDataDirectory, "patches"), { recursive: true }),
    mkdir(commitPatchPagesDirectory, { recursive: true }),
  ]);
  writes.push(
    ...Array.from(
      { length: Math.ceil(patches.length / commitPageSize) },
      (_, page) => writeIfChanged(
        resolve(commitPatchPagesDirectory, `${String(page).padStart(4, "0")}.diff`),
        Buffer.from(
          patches
            .slice(page * commitPageSize, (page + 1) * commitPageSize)
            .map(({ contents }) => contents)
            .join("\n"),
          "utf8",
        ),
      ),
    ),
    ...blobFiles.map(({ objectId, contents }) =>
      writeIfChanged(resolve(generatedDataDirectory, "blobs", `${objectId}.txt`), contents),
    ),
    ...patches.map(({ hash, contents }) =>
      writeIfChanged(
        resolve(generatedDataDirectory, "patches", `${hash}.patch`),
        Buffer.from(contents, "utf8"),
      ),
    ),
  );
}
await Promise.all(writes);
console.log(
  `Synced ${tree.length} file and ${commits.length} commit indexes from ${identity.fullName} (${head.slice(0, 7)})`,
);

async function generatedSnapshotIsCurrent(repository) {
  let snapshot;
  try {
    snapshot = JSON.parse(await readFile(outputPath, "utf8"));
  } catch (error) {
    if (isMissing(error)) return false;
    throw error;
  }
  if (
    snapshot.generatorVersion !== generatorVersion ||
    snapshot.commitLimit !== commitLimit ||
    snapshot.repository?.branch !== repository.branch ||
    snapshot.repository?.fullName !== repository.fullName ||
    snapshot.repository?.head !== repository.head ||
    !Array.isArray(snapshot.tree)
  ) {
    return false;
  }
  try {
    const pageCount = Math.ceil(snapshot.repository.indexedCommits / commitPageSize);
    const pageNames = Array.from(
      { length: pageCount },
      (_, page) => String(page).padStart(4, "0"),
    );
    await Promise.all([
      access(outputPath),
      access(commitsOutputPath),
      access(commitIndexOutputPath),
      ...pageNames.map((name) =>
        access(resolve(commitPagesDirectory, `${name}.json`))
      ),
      ...(emitObjects
        ? pageNames.map((name) =>
          access(resolve(commitPatchPagesDirectory, `${name}.diff`))
        )
        : []),
    ]);
    return true;
  } catch (error) {
    if (isMissing(error)) return false;
    throw error;
  }
}

function combineCommitLogs(numstatLog, statusLog) {
  const statuses = new Map(
    parseLogRecords(statusLog).map(({ header, payload }) => [
      header.hash,
      parseStatuses(payload),
    ]),
  );
  return parseLogRecords(numstatLog).map(({ header, payload }) => {
    const statusByPath = statuses.get(header.hash) ?? new Map();
    const files = parseNumstat(payload).map((file) => ({
      ...file,
      status: statusByPath.get(file.path)?.status ?? "M",
      previousPath: statusByPath.get(file.path)?.previousPath ?? file.previousPath,
    }));
    return {
      ...header,
      subject: redactPublicText(header.subject),
      body: redactPublicText(header.body),
      files,
      stats: {
        files: files.length,
        additions: files.reduce((total, file) => total + (file.additions ?? 0), 0),
        deletions: files.reduce((total, file) => total + (file.deletions ?? 0), 0),
      },
    };
  });
}

function projectCommitRefs(commits, head, publicBranch) {
  if (publicBranch == null) return commits;
  return commits.map((commit) => ({
    ...commit,
    refs: commit.hash === head ? [`HEAD -> ${publicBranch}`] : [],
  }));
}

function buildCommitScopeCounts(commits) {
  const counts = { all: commits.length, eval: 0, fix: 0, docs: 0, perf: 0 };
  for (const { subject } of commits) {
    const scope = subject.match(/^([a-z]+)(?:\([^)]*\))?:/i)?.[1]?.toLowerCase();
    if (scope != null && Object.hasOwn(counts, scope) && scope !== "all") counts[scope]++;
  }
  return counts;
}

function parseLogRecords(output) {
  return output
    .split("\x1e")
    .filter(Boolean)
    .map((record) => {
      const fields = [];
      let cursor = 0;
      for (let index = 0; index < 8; index += 1) {
        const end = record.indexOf("\0", cursor);
        if (end < 0) throw new Error("Could not parse Git log metadata");
        fields.push(record.slice(cursor, end));
        cursor = end + 1;
      }
      const [hash, shortHash, parentField, author, authoredAt, refField, subject, body] = fields;
      return {
        header: {
          hash,
          shortHash,
          parents: parentField.split(" ").filter(Boolean),
          author,
          authoredAt,
          refs: refField.split(",").map((ref) => ref.trim()).filter(Boolean),
          subject,
          body: body.trim(),
        },
        payload: record.slice(cursor).replace(/^[\0\r\n]+/, ""),
      };
    });
}

function splitCommitPatches(commits, rawPatches) {
  const sections = rawPatches
    .split(/(?=^From [a-f0-9]{40} Mon Sep 17 00:00:00 2001$)/m)
    .filter((section) => section.startsWith("From "));
  const sectionByHash = new Map(
    sections.map((section) => [section.slice(5, 45), section]),
  );
  return commits.map(({ hash }) => ({
    hash,
    contents: sectionByHash.get(hash) ??
      `From ${hash} Mon Sep 17 00:00:00 2001\n`,
  }));
}

function parseNumstat(payload) {
  const tokens = payload.split("\0");
  const files = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index].replace(/^[\r\n]+/, "");
    const match = token.match(/^([^\t]+)\t([^\t]+)\t([\s\S]*)$/);
    if (!match) continue;
    const [, rawAdditions, rawDeletions, inlinePath] = match;
    const previousPath = inlinePath === "" ? tokens[++index] ?? null : null;
    const path = inlinePath === "" ? tokens[++index] ?? "" : inlinePath;
    if (!path || isGeneratedData(path)) continue;
    files.push({
      path,
      previousPath,
      additions: rawAdditions === "-" ? null : Number(rawAdditions),
      deletions: rawDeletions === "-" ? null : Number(rawDeletions),
    });
  }
  return files;
}

function parseStatuses(payload) {
  const tokens = payload.split("\0");
  const statuses = new Map();
  for (let index = 0; index < tokens.length; index += 1) {
    const status = tokens[index].replace(/^[\r\n]+/, "");
    if (!/^[A-Z]/.test(status)) continue;
    const previousPath = status.startsWith("R") || status.startsWith("C")
      ? tokens[++index] ?? null
      : null;
    const path = tokens[++index] ?? "";
    if (!path || isGeneratedData(path)) continue;
    statuses.set(path, { status: status[0], previousPath });
  }
  return statuses;
}

function parseTree(output) {
  return output
    .split("\0")
    .filter(Boolean)
    .map((row) => {
      const match = row.match(/^(\d+) blob ([0-9a-f]+)\s+(\d+|-)\t([\s\S]+)$/);
      if (!match) throw new Error(`Could not parse git tree row: ${row}`);
      const [, mode, objectId, rawSize, path] = match;
      return { mode, objectId, rawSize, path };
    });
}

async function readGitObjects(objectIds) {
  const uniqueObjectIds = [...new Set(objectIds)];
  const child = spawn("git", ["cat-file", "--batch"], {
    cwd: repositoryPath,
    stdio: ["pipe", "pipe", "pipe"],
  });
  const stdout = [];
  let stderr = "";
  child.stdout.on("data", (chunk) => stdout.push(chunk));
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.stdin.end(`${uniqueObjectIds.join("\n")}\n`);
  const code = await new Promise((resolveExit, reject) => {
    child.once("error", reject);
    child.once("close", resolveExit);
  });
  if (code !== 0) throw new Error(stderr.trim() || `git cat-file exited with ${code}`);

  const output = Buffer.concat(stdout);
  const objects = new Map();
  let cursor = 0;
  for (const expectedObjectId of uniqueObjectIds) {
    const headerEnd = output.indexOf(10, cursor);
    if (headerEnd < 0) throw new Error(`Missing cat-file header for ${expectedObjectId}`);
    const [objectId, type, rawSize] = output.toString("utf8", cursor, headerEnd).split(" ");
    const size = Number(rawSize);
    if (objectId !== expectedObjectId || type !== "blob" || !Number.isSafeInteger(size)) {
      throw new Error(`Unexpected cat-file header for ${expectedObjectId}`);
    }
    const contentStart = headerEnd + 1;
    const contentEnd = contentStart + size;
    if (contentEnd >= output.length || output[contentEnd] !== 10) {
      throw new Error(`Truncated cat-file contents for ${expectedObjectId}`);
    }
    objects.set(objectId, output.subarray(contentStart, contentEnd));
    cursor = contentEnd + 1;
  }
  return objects;
}

async function writeIfChanged(path, contents) {
  try {
    const current = await readFile(path);
    if (current.equals(contents)) return;
  } catch (error) {
    if (!isMissing(error)) throw error;
  }
  await writeFile(path, contents);
}

function parseCommitLimit(value) {
  if (value == null || value === "") return null;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error("NANOCODEX_COMMIT_LIMIT must be a positive integer");
  }
  return parsed;
}

function isText(buffer) {
  if (buffer.includes(0)) return false;
  const sample = buffer.subarray(0, Math.min(buffer.length, 8_192));
  let controlBytes = 0;
  for (const byte of sample) {
    if (byte < 32 && byte !== 9 && byte !== 10 && byte !== 13) controlBytes += 1;
  }
  return sample.length === 0 || controlBytes / sample.length < 0.02;
}

function isGeneratedData(path) {
  return generatedDataPrefixes.some((prefix) => path.startsWith(prefix));
}

function parseRepositoryIdentity(value) {
  const match = value.match(/[:/]([^/:]+)\/([^/]+?)(?:\.git)?$/);
  return { fullName: match ? `${match[1]}/${match[2]}` : "gakonst/nanocodex" };
}

function isMissing(error) {
  return error != null && typeof error === "object" && error.code === "ENOENT";
}

async function git(args, { optional = false } = {}) {
  try {
    const { stdout } = await execFileAsync("git", args, {
      cwd: repositoryPath,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
    });
    return stdout.trimEnd();
  } catch (error) {
    if (optional) return "";
    throw error;
  }
}
