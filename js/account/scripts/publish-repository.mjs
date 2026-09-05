import { execFile, spawn } from "node:child_process";
import { createReadStream, createWriteStream } from "node:fs";
import {
  mkdtemp,
  open,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(import.meta.url);
const projectRoot = resolve(dirname(scriptPath), "..");
const repositoryPath = resolve(
  process.env.NANOCODEX_REPO ?? resolve(projectRoot, "../.."),
);
const uploadConcurrency = 12;
const repositoryUploadPartBytes = 4 * 1024 * 1024;
const publicationBranch = "master";
const uploadAttemptTimeoutMs = positiveIntegerEnvironment(
  "NANOCODEX_GIT_UPLOAD_TIMEOUT_MS",
  60_000,
);

async function main() {
  const origin = requiredEnvironment("NANOCODEX_GIT_ORIGIN").replace(/\/$/, "");
  const head = await git(["rev-parse", "HEAD"]);
  await requireDeploymentSha(origin, head);
  const token = requiredEnvironment("NANOCODEX_GIT_TOKEN");
  const previous = await readRemoteState(
    origin,
    token,
    process.env.NANOCODEX_REPAIR_INVALID_PUBLICATION === "1",
  );
  if (previous?.publication?.head === head && process.env.NANOCODEX_FORCE_SYNC !== "1") {
    console.log(`Cloudflare repository is current (${head.slice(0, 7)})`);
    return;
  }

  const temporaryDirectory = await mkdtemp(resolve(tmpdir(), "nanocodex-git-"));
  try {
    const dataDirectory = resolve(temporaryDirectory, "data");
    await run(process.execPath, [resolve(projectRoot, "scripts", "sync-nanocodex.mjs")], {
      cwd: projectRoot,
      env: {
        ...process.env,
        NANOCODEX_DATA_DIR: dataDirectory,
        NANOCODEX_EMIT_OBJECTS: "1",
        NANOCODEX_FORCE_SYNC: "1",
        NANOCODEX_PUBLIC_BRANCH: publicationBranch,
        NANOCODEX_REPO: repositoryPath,
      },
    });

    const [
      snapshot,
      commits,
      commitIndex,
      blobNames,
      patchNames,
      commitPageNames,
      commitPatchPageNames,
    ] = await Promise.all([
      readJson(resolve(dataDirectory, "repository.json")),
      readJson(resolve(dataDirectory, "commits.json")),
      readJson(resolve(dataDirectory, "commit-index.json")),
      listObjectNames(resolve(dataDirectory, "blobs"), ".txt"),
      listObjectNames(resolve(dataDirectory, "patches"), ".patch"),
      listObjectNames(resolve(dataDirectory, "commit-pages"), ".json"),
      listObjectNames(resolve(dataDirectory, "commit-patch-pages"), ".diff"),
    ]);
    const commitPatchParts = buildCommitPatchParts(
      head,
      await Promise.all(commitPatchPageNames.map(async (name) => {
        const path = resolve(dataDirectory, "commit-patch-pages", `${name}.diff`);
        return { name, path, size: (await stat(path)).size };
      })),
    );
    const commitPatchSize = commitPatchParts.reduce(
      (total, part) => total + part.size,
      0,
    );
    const refs = [{ name: `refs/heads/${publicationBranch}`, oid: head }];
    const publishedPageSize = snapshot.repository?.commitPageSize;
    if (!Number.isSafeInteger(publishedPageSize) || publishedPageSize <= 0) {
      throw new Error("published commit page size is invalid");
    }
    const expectedCommitPageNames = Array.from(
      {
        length: Math.ceil(
          commits.length / publishedPageSize,
        ),
      },
      (_, page) => String(page).padStart(4, "0"),
    );
    if (
      snapshot.repository?.head !== head ||
      snapshot.repository?.branch !== publicationBranch ||
      snapshot.repository?.indexedCommits !== commits.length ||
      commitIndex.repository?.head !== head ||
      commitIndex.repository?.commitPageSize !== publishedPageSize ||
      JSON.stringify(commitIndex.hashes) !==
        JSON.stringify(commits.map(({ hash }) => hash)) ||
      JSON.stringify(commitPageNames) !== JSON.stringify(expectedCommitPageNames) ||
      JSON.stringify(commitPatchPageNames) !== JSON.stringify(expectedCommitPageNames)
    ) {
      throw new Error("repository changed while its publication was being built");
    }
    const gitArtifacts = await buildGitArtifacts({
      repository: repositoryPath,
      temporaryDirectory,
      head,
      refs,
      previousManifest: previous?.objectManifest,
    });

    const inventory = {
      version: 1,
      head,
      blobs: blobNames,
      patches: patchNames,
    };
    const previousInventory = previous?.inventory ?? { blobs: [], patches: [] };
    const plan = buildUploadPlan(inventory, previousInventory);
    console.log(
      `Uploading ${plan.blobs.length} new blobs and ${plan.patches.length} new patches`,
    );
    await mapConcurrent(
      [
        ...plan.blobs.map((id) => ({
          remote: `blobs/${id}`,
          local: resolve(dataDirectory, "blobs", `${id}.txt`),
        })),
        ...plan.patches.map((id) => ({
          remote: `patches/${id}`,
          local: resolve(dataDirectory, "patches", `${id}.patch`),
        })),
      ],
      uploadConcurrency,
      ({ remote, local }) => uploadFile(origin, token, remote, local),
    );

    const generationPrefix = `generations/${head}`;
    const inventoryPath = resolve(temporaryDirectory, "inventory.json");
    const commitPatchManifestPath = resolve(temporaryDirectory, "commit-patches.json");
    const publicationPath = resolve(temporaryDirectory, "publication.json");
    const commitPatchManifest = {
      version: 1,
      head,
      parts: commitPatchParts.map(({ key, size }) => ({ key, size })),
      size: commitPatchSize,
    };
    const publication = {
      version: 1,
      head,
      branch: publicationBranch,
      refs,
      snapshotKey: `${generationPrefix}/repository.json`,
      commitsKey: `${generationPrefix}/commits.json`,
      commitPatchParts: commitPatchParts.map(({ key, size }) => ({ key, size })),
      commitPatchSize,
      inventoryKey: `${generationPrefix}/inventory.json`,
      packParts: gitArtifacts.packParts.map(({ key, size }) => ({ key, size })),
      packSize: gitArtifacts.packSize,
      objectManifestKey: `${generationPrefix}/objects.json`,
      packHash: gitArtifacts.packHash,
      publishedAt: new Date().toISOString(),
    };
    await Promise.all([
      writeFile(inventoryPath, `${JSON.stringify(inventory)}\n`),
      writeFile(commitPatchManifestPath, `${JSON.stringify(commitPatchManifest)}\n`),
      writeFile(publicationPath, `${JSON.stringify(publication)}\n`),
    ]);
    await Promise.all([
      uploadFile(origin, token, `${generationPrefix}/publication.json`, publicationPath),
      uploadFile(origin, token, `${generationPrefix}/repository.json`, resolve(dataDirectory, "repository.json")),
      uploadFile(origin, token, `${generationPrefix}/commits.json`, resolve(dataDirectory, "commits.json")),
      uploadFile(origin, token, `${generationPrefix}/commit-index.json`, resolve(dataDirectory, "commit-index.json")),
      uploadFile(origin, token, `${generationPrefix}/commit-patches.json`, commitPatchManifestPath),
      mapConcurrent(commitPatchParts, 2, (part) =>
        uploadFile(origin, token, part.key, part.path)
      ),
      uploadFile(origin, token, `${generationPrefix}/inventory.json`, inventoryPath),
      uploadFile(origin, token, `${generationPrefix}/objects.json`, gitArtifacts.manifestPath),
      mapConcurrent(gitArtifacts.shards, uploadConcurrency, (shard) =>
        uploadFile(origin, token, shard.key, shard.path)
      ),
      mapConcurrent(gitArtifacts.packParts, 2, (part) =>
        uploadFile(origin, token, part.key, gitArtifacts.packPath, part)
      ),
      mapConcurrent(commitPageNames, uploadConcurrency, (name) => uploadFile(
          origin,
          token,
          `${generationPrefix}/commits/${name}.json`,
          resolve(dataDirectory, "commit-pages", `${name}.json`),
        )),
    ]);
    const response = await authenticatedFetch(`${origin}/api/git/publish`, token, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        expectedHead: previous?.publication?.head ?? null,
        publication,
        ...(previous?.replaceInvalid === true ? { replaceInvalid: true } : {}),
      }),
    });
    if (!response.ok) throw new Error(await responseError("publish", response));
    console.log(
      `Published ${snapshot.repository.fullName} ${head.slice(0, 7)} (${commits.length} commits, ${gitArtifacts.objectCount} objects, ${gitArtifacts.packParts.length} pack parts, ${gitArtifacts.packHash.slice(0, 7)} pack)`,
    );
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
}

async function requireDeploymentSha(origin, expected) {
  const response = await fetch(`${origin}/api/health`, {
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(await responseError("read deployment health", response));
  }
  let health;
  try {
    health = await response.json();
  } catch {
    throw new Error("Cloudflare Worker health returned invalid JSON");
  }
  const observed = typeof health?.deployment_sha === "string"
    ? health.deployment_sha
    : "unattested";
  if (observed !== expected) {
    throw new Error(
      `Cloudflare Worker revision ${observed} does not match repository ${expected}; deploy the Worker before publishing`,
    );
  }
}

export function buildUploadPlan(inventory, previousInventory) {
  const previousBlobs = new Set(previousInventory.blobs ?? []);
  const previousPatches = new Set(previousInventory.patches ?? []);
  return {
    blobs: inventory.blobs.filter((id) => !previousBlobs.has(id)),
    patches: inventory.patches.filter((id) => !previousPatches.has(id)),
  };
}

export async function readRemoteState(origin, token, repairInvalid = false) {
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    let response;
    try {
      response = await authenticatedFetch(`${origin}/api/git/state`, token);
    } catch (error) {
      if (!isRetriableUploadError(error) || attempt === 6) throw error;
      await delay(250 * (2 ** (attempt - 1)));
      continue;
    }
    if (response.status === 404) return null;
    if (response.status === 503) {
      let failure;
      try {
        failure = await response.clone().json();
      } catch {
        // The normal response error below retains the bounded raw response body.
      }
      if (failure?.error === "repository_not_published") return null;
      if (failure?.error === "repository publication is invalid") {
        if (!repairInvalid) {
          throw new Error(
            "Cloudflare repository publication is invalid; set NANOCODEX_REPAIR_INVALID_PUBLICATION=1 to atomically replace it with the current format",
          );
        }
        console.log("Replacing invalid Cloudflare repository publication with the current format");
        return { replaceInvalid: true };
      }
    }
    if (response.ok) return response.json();
    if (!isRetriableUploadStatus(response.status) || attempt === 6) {
      throw new Error(await responseError("read state", response));
    }
    if (response.body) await response.body.cancel().catch(() => undefined);
    await delay(250 * (2 ** (attempt - 1)));
  }
  throw new Error("read state exhausted its retry policy");
}

const objectShardCompactionThreshold = 128;
const gitObjectTypes = { commit: 1, tree: 2, blob: 3, tag: 4 };

export async function buildGitArtifacts({
  repository,
  temporaryDirectory,
  head,
  refs,
  previousManifest,
}) {
  const revisionOids = [...new Set(refs.map((ref) => ref.oid))];
  if (revisionOids.length === 0 || !revisionOids.includes(head)) {
    throw new Error("published Git refs do not contain HEAD");
  }

  const packPath = resolve(temporaryDirectory, "repository.pack");
  const indexPath = resolve(temporaryDirectory, "repository.idx");
  await writePack(packPath, revisionOids, repository, true);
  const packHash = await indexAndVerifyPack(packPath, indexPath, repository);
  const packSize = (await stat(packPath)).size;
  const packParts = buildRepositoryPackParts(head, packHash, packSize);
  const reachableOids = await listReachableOids(revisionOids, repository);

  const reusePrevious = isReusableManifest(previousManifest) &&
    previousManifest.shards.length < objectShardCompactionThreshold;
  const shards = reusePrevious ? previousManifest.shards.map((shard) => ({ ...shard })) : [];
  const objects = {};
  const newOids = [];
  for (const oid of reachableOids) {
    const previous = reusePrevious ? previousManifest.objects[oid] : undefined;
    if (isReusableObjectRecord(previous, shards)) objects[oid] = previous;
    else newOids.push(oid);
  }

  const newShards = newOids.length === 0
    ? []
    : await buildObjectShards({
        repository,
        temporaryDirectory,
        head,
        objectIds: newOids,
        firstShardIndex: shards.length,
        objects,
      });
  shards.push(...newShards.map(({ key, size }) => ({ key, size })));

  for (const [oid, record] of Object.entries(objects)) {
    for (const dependency of record[4]) {
      if (objects[dependency] == null) {
        throw new Error(`Git object ${oid} depends on unpublished object ${dependency}`);
      }
    }
  }
  if (objects[head] == null || Object.keys(objects).length !== reachableOids.length) {
    throw new Error("Git object manifest is incomplete");
  }

  const manifest = { version: 1, head, shards, objects };
  const manifestPath = resolve(temporaryDirectory, "objects.json");
  await writeFile(manifestPath, `${JSON.stringify(manifest)}\n`);
  return {
    packPath,
    packHash,
    packParts,
    packSize,
    manifest,
    manifestPath,
    shards: newShards.map(({ key, path }) => ({ key, path })),
    objectCount: reachableOids.length,
  };
}

export function buildRepositoryPackParts(head, packHash, packSize) {
  if (!/^[a-f0-9]{40}$/.test(head)) throw new Error("repository pack head is invalid");
  if (!/^[a-f0-9]{40}$/.test(packHash)) throw new Error("repository pack hash is invalid");
  return buildBoundedParts(
    packSize,
    (index) =>
      `generations/${head}/packs/${packHash}/${String(index).padStart(4, "0")}.pack`,
    "repository pack",
  );
}

export function buildCommitPatchParts(head, pages) {
  if (!/^[a-f0-9]{40}$/.test(head)) throw new Error("commit patch head is invalid");
  if (!Array.isArray(pages) || pages.length === 0 || pages.length > 256) {
    throw new Error("commit patch page count is invalid");
  }
  return pages.map((page, index) => {
    const name = String(index).padStart(4, "0");
    if (
      page?.name !== name ||
      typeof page.path !== "string" ||
      !Number.isSafeInteger(page.size) ||
      page.size <= 0 ||
      page.size > repositoryUploadPartBytes
    ) {
      throw new Error(`commit patch page ${name} is invalid`);
    }
    return {
      key: `generations/${head}/commit-patches/${name}.diff`,
      path: page.path,
      size: page.size,
    };
  });
}

function buildBoundedParts(totalSize, keyAt, description) {
  if (!Number.isSafeInteger(totalSize) || totalSize <= 0) {
    throw new Error(`${description} size is invalid`);
  }
  const count = Math.ceil(totalSize / repositoryUploadPartBytes);
  if (count > 256) throw new Error(`${description} part count exceeds 256`);
  return Array.from({ length: count }, (_, index) => {
    const offset = index * repositoryUploadPartBytes;
    return {
      key: keyAt(index),
      offset,
      size: Math.min(repositoryUploadPartBytes, totalSize - offset),
    };
  });
}

async function writePack(path, objectIds, repository, traverseRevisions) {
  const args = ["pack-objects", "--stdout"];
  if (traverseRevisions) args.push("--revs");
  else args.push("--window=0", "--depth=0", "--no-reuse-delta", "--no-reuse-object");
  const child = spawn("git", args, {
    cwd: repository,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.stdin.end(`${objectIds.join("\n")}\n`);
  await Promise.all([
    pipeline(child.stdout, createWriteStream(path, { flags: "wx" })),
    new Promise((resolveExit, reject) => {
      child.once("error", reject);
      child.once("close", (code) => {
        if (code === 0) resolveExit();
        else reject(new Error(stderr.trim() || `git pack-objects exited with ${code}`));
      });
    }),
  ]);
}

async function indexAndVerifyPack(packPath, indexPath, repository) {
  const { stdout } = await execFileAsync(
    "git",
    ["index-pack", "--index-version=2", "-o", indexPath, packPath],
    { cwd: repository, encoding: "utf8" },
  );
  const hash = stdout.trim();
  if (!/^[a-f0-9]{40}$/.test(hash)) {
    throw new Error(`git index-pack returned an invalid hash: ${hash}`);
  }
  await execFileAsync("git", ["verify-pack", "-s", indexPath], {
    cwd: repository,
    encoding: "utf8",
  });
  return hash;
}

async function listReachableOids(revisionOids, repository) {
  const output = await gitWithInput(
    ["rev-list", "--objects", "--no-object-names", "--stdin"],
    `${revisionOids.join("\n")}\n`,
    repository,
  );
  const objectIds = [...new Set(output.toString("utf8").trim().split("\n").filter(Boolean))];
  if (objectIds.some((oid) => !/^[a-f0-9]{40}$/.test(oid))) {
    throw new Error("git rev-list returned an invalid object id");
  }
  return objectIds;
}

async function buildObjectShards({
  repository,
  temporaryDirectory,
  head,
  objectIds,
  firstShardIndex,
  objects,
}) {
  const objectPackPath = resolve(temporaryDirectory, "new-objects.pack");
  const objectIndexPath = resolve(temporaryDirectory, "new-objects.idx");
  await writePack(objectPackPath, objectIds, repository, false);
  await indexAndVerifyPack(objectPackPath, objectIndexPath, repository);
  const entries = await readPackEntries(objectIndexPath, repository);
  if (entries.length !== objectIds.length) {
    throw new Error(`Git object pack contains ${entries.length} objects, expected ${objectIds.length}`);
  }
  const dependencies = await readObjectDependencies(entries, repository);
  const handle = await open(objectPackPath, "r");
  const shards = [];
  try {
    for (let entryIndex = 0; entryIndex < entries.length;) {
      const shardEntries = [];
      let shardSize = 0;
      while (entryIndex < entries.length) {
        const entry = entries[entryIndex];
        if (shardSize > 0 && shardSize + entry.length > repositoryUploadPartBytes) break;
        shardEntries.push(entry);
        shardSize += entry.length;
        entryIndex += 1;
      }
      const number = firstShardIndex + shards.length;
      if (number > 9_999) throw new Error("Git object shard count exceeds 9999");
      const name = `${String(number).padStart(4, "0")}.pack`;
      const path = resolve(temporaryDirectory, `objects-${name}`);
      const contents = Buffer.allocUnsafe(shardSize);
      const { bytesRead } = await handle.read(contents, 0, shardSize, shardEntries[0].offset);
      if (bytesRead !== shardSize) throw new Error("Git object pack ended inside an entry shard");
      await writeFile(path, contents);
      const shardIndex = number;
      let offset = 0;
      for (const entry of shardEntries) {
        objects[entry.oid] = [
          entry.type,
          shardIndex,
          offset,
          entry.length,
          dependencies.get(entry.oid) ?? [],
        ];
        offset += entry.length;
      }
      shards.push({
        key: `generations/${head}/objects/${name}`,
        path,
        size: shardSize,
      });
    }
  } finally {
    await handle.close();
  }
  return shards;
}

async function readPackEntries(indexPath, repository) {
  const { stdout } = await execFileAsync("git", ["verify-pack", "-v", indexPath], {
    cwd: repository,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  const entries = [];
  for (const line of stdout.split("\n")) {
    const fields = line.trim().split(/\s+/);
    if (!/^[a-f0-9]{40}$/.test(fields[0] ?? "")) continue;
    const type = gitObjectTypes[fields[1]];
    const length = Number(fields[3]);
    const offset = Number(fields[4]);
    if (
      type == null ||
      fields.length !== 5 ||
      !Number.isSafeInteger(length) ||
      length <= 0 ||
      !Number.isSafeInteger(offset) ||
      offset < 12
    ) {
      throw new Error(`Git object entry is not a complete non-delta object: ${line}`);
    }
    entries.push({ oid: fields[0], type, length, offset });
  }
  entries.sort((left, right) => left.offset - right.offset);
  for (let index = 1; index < entries.length; index++) {
    if (entries[index - 1].offset + entries[index - 1].length !== entries[index].offset) {
      throw new Error("Git object pack entries are not contiguous");
    }
  }
  return entries;
}

async function readObjectDependencies(entries, repository) {
  const structural = entries.filter((entry) => entry.type !== gitObjectTypes.blob);
  if (structural.length === 0) return new Map();
  const output = await gitWithInput(
    ["cat-file", "--batch"],
    `${structural.map((entry) => entry.oid).join("\n")}\n`,
    repository,
    512 * 1024 * 1024,
  );
  const dependencies = new Map();
  let offset = 0;
  for (const expected of structural) {
    const newline = output.indexOf(0x0a, offset);
    if (newline < 0) throw new Error("git cat-file returned a truncated header");
    const header = output.subarray(offset, newline).toString("utf8").split(" ");
    const size = Number(header[2]);
    if (header[0] !== expected.oid || !Number.isSafeInteger(size) || size < 0) {
      throw new Error("git cat-file returned an unexpected object");
    }
    const start = newline + 1;
    const end = start + size;
    if (end >= output.length) throw new Error("git cat-file returned truncated object data");
    const body = output.subarray(start, end);
    dependencies.set(expected.oid, parseObjectDependencies(expected.type, body));
    offset = end + 1;
  }
  return dependencies;
}

function parseObjectDependencies(type, body) {
  if (type === gitObjectTypes.commit) {
    const dependencies = [];
    for (const line of body.toString("utf8").split("\n")) {
      if (line === "") break;
      if (line.startsWith("tree ")) dependencies.unshift(line.slice(5));
      else if (line.startsWith("parent ")) dependencies.push(line.slice(7));
    }
    return dependencies;
  }
  if (type === gitObjectTypes.tag) {
    const target = body.toString("utf8").match(/^object ([a-f0-9]{40})$/m)?.[1];
    return target == null ? [] : [target];
  }
  if (type !== gitObjectTypes.tree) return [];
  const dependencies = [];
  for (let offset = 0; offset < body.length;) {
    const space = body.indexOf(0x20, offset);
    const nul = body.indexOf(0, space + 1);
    if (space < 0 || nul < 0 || nul + 21 > body.length) {
      throw new Error("Git tree object is malformed");
    }
    const mode = body.subarray(offset, space).toString("ascii");
    if (mode !== "160000") dependencies.push(body.subarray(nul + 1, nul + 21).toString("hex"));
    offset = nul + 21;
  }
  return dependencies;
}

function isReusableManifest(value) {
  return value != null &&
    value.version === 1 &&
    Array.isArray(value.shards) &&
    value.shards.length > 0 &&
    value.shards.every((shard) =>
      shard != null &&
      /^generations\/[a-f0-9]{40}\/objects\/\d{4}\.pack$/.test(shard.key) &&
      Number.isSafeInteger(shard.size) &&
      shard.size > 0
    ) &&
    value.objects != null &&
    typeof value.objects === "object";
}

function isReusableObjectRecord(value, shards) {
  return Array.isArray(value) &&
    value.length === 5 &&
    Object.values(gitObjectTypes).includes(value[0]) &&
    Number.isSafeInteger(value[1]) &&
    value[1] >= 0 &&
    value[1] < shards.length &&
    Number.isSafeInteger(value[2]) &&
    value[2] >= 0 &&
    Number.isSafeInteger(value[3]) &&
    value[3] > 0 &&
    value[2] + value[3] <= shards[value[1]].size &&
    Array.isArray(value[4]) &&
    value[4].every((oid) => /^[a-f0-9]{40}$/.test(oid));
}

async function gitWithInput(args, input, repository, maximumBytes = 64 * 1024 * 1024) {
  const child = spawn("git", args, { cwd: repository, stdio: ["pipe", "pipe", "pipe"] });
  const chunks = [];
  let bytes = 0;
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    bytes += chunk.length;
    if (bytes > maximumBytes) child.kill("SIGKILL");
    else chunks.push(chunk);
  });
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.stdin.end(input);
  const code = await new Promise((resolveExit, reject) => {
    child.once("error", reject);
    child.once("close", resolveExit);
  });
  if (bytes > maximumBytes) throw new Error(`git ${args[0]} output exceeded ${maximumBytes} bytes`);
  if (code !== 0) throw new Error(stderr.trim() || `git ${args[0]} exited with ${code}`);
  return Buffer.concat(chunks, bytes);
}

async function uploadFile(origin, token, remote, local, range) {
  const fileSize = (await stat(local)).size;
  const offset = range?.offset ?? 0;
  const size = range?.size ?? fileSize;
  if (
    !Number.isSafeInteger(offset) ||
    offset < 0 ||
    !Number.isSafeInteger(size) ||
    size <= 0 ||
    offset + size > fileSize
  ) {
    throw new Error(`upload ${basename(local)} has an invalid byte range`);
  }
  let lastError;
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    const headers = new Headers({ "content-length": String(size) });
    const body = Readable.toWeb(createReadStream(local, {
      start: offset,
      end: offset + size - 1,
    }));
    let response;
    try {
      response = await authenticatedFetch(
        `${origin}/api/git/objects/${remote}`,
        token,
        {
          method: "PUT",
          headers,
          body,
          duplex: "half",
          signal: AbortSignal.timeout(uploadAttemptTimeoutMs),
        },
      );
    } catch (error) {
      lastError = new Error(
        `upload ${basename(local)} failed: ${error instanceof Error ? error.message : String(error)}`,
        { cause: error },
      );
      if (!isRetriableUploadError(error) || attempt === 5) break;
      await delay(250 * (2 ** (attempt - 1)));
      continue;
    }
    if (response.ok) {
      let uploaded;
      try {
        uploaded = await response.json();
      } catch {
        throw new Error(`upload ${basename(local)} returned invalid JSON`);
      }
      if (uploaded?.size !== size) {
        throw new Error(
          `upload ${basename(local)} resolved ${uploaded?.size ?? "an unknown number of"} bytes; expected ${size}`,
        );
      }
      return;
    }
    lastError = new Error(await responseError(`upload ${basename(local)}`, response));
    if (!isRetriableUploadStatus(response.status) || attempt === 5) break;
    await delay(250 * (2 ** (attempt - 1)));
  }
  throw lastError;
}

export function isRetriableUploadStatus(status) {
  return status === 401 || status === 408 || status === 425 || status === 429 || status >= 500;
}

export function isRetriableUploadError(error) {
  return error instanceof TypeError || error?.name === "TimeoutError";
}

function delay(milliseconds) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}

function authenticatedFetch(url, token, init = {}) {
  const headers = new Headers(init.headers);
  headers.set("authorization", `Bearer ${token}`);
  return fetch(url, { ...init, headers });
}

async function responseError(operation, response) {
  return `${operation} failed with HTTP ${response.status}: ${(await response.text()).slice(0, 1_000)}`;
}

async function listObjectNames(path, suffix) {
  return (await readdir(path))
    .filter((name) => name.endsWith(suffix))
    .map((name) => name.slice(0, -suffix.length))
    .sort();
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function mapConcurrent(values, concurrency, operation) {
  let nextIndex = 0;
  await Promise.all(Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (nextIndex < values.length) {
      const index = nextIndex++;
      await operation(values[index], index);
    }
  }));
}

async function run(command, args, options) {
  const child = spawn(command, args, { ...options, stdio: "inherit" });
  await new Promise((resolveExit, reject) => {
    child.once("error", reject);
    child.once("close", (code) => {
      if (code === 0) resolveExit();
      else reject(new Error(`${command} exited with ${code}`));
    });
  });
}

async function git(args) {
  const { stdout } = await execFileAsync("git", args, {
    cwd: repositoryPath,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  return stdout.trimEnd();
}

function requiredEnvironment(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function positiveIntegerEnvironment(name, fallback) {
  const value = process.env[name]?.trim();
  if (!value) return fallback;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

if (resolve(process.argv[1] ?? "") === scriptPath) {
  await main();
}
