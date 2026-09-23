// Retained artifacts survive Actions cache LRU eviction. Only master producers
// from these workflows are trusted; integrity still uses the existing WASM key.
import assert from "node:assert/strict";
import { appendFile, cp, lstat, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { check } from "../../js/nanocodex-vite/scripts/wasm-output-cache.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
export const outputPaths = ["js/nanocodex/pkg-web", "js/nanocodex/pkg-node", ".ci-wasm-cache"];
const trustedWorkflows = new Set([".github/workflows/ci.yml", ".github/workflows/cloudflare.yml"]);
const trustedEvents = new Set(["push", "workflow_dispatch", "schedule"]);
const day = 24 * 60 * 60 * 1000;

export function artifactName(key) {
  assert.match(key, /^wasm-outputs-v1-(Linux|Windows|macOS)-(X64|ARM64|X86)-[a-f0-9]{64}$/, "invalid WASM output key");
  return `retained-${key}`;
}

function sameId(left, right) {
  return /^[1-9][0-9]*$/.test(String(left)) && String(left) === String(right);
}

function candidate(artifact, { key, repositoryId, now, minRemainingMs, excludedIds }) {
  const run = artifact.workflow_run;
  return artifact.name === artifactName(key) && !excludedIds.has(String(artifact.id)) &&
    /^[1-9][0-9]*$/.test(String(artifact.id)) && artifact.expired === false &&
    artifact.size_in_bytes > 0 && Date.parse(artifact.expires_at) > now + minRemainingMs &&
    run?.head_branch === "master" && sameId(run.repository_id, repositoryId) &&
    sameId(run.head_repository_id, repositoryId) && /^[a-f0-9]{40}$/.test(run.head_sha ?? "") &&
    /^[1-9][0-9]*$/.test(String(run.id));
}

export function isTrustedRun(run, artifact, { repository, repositoryId }) {
  // Checking the branch alone would admit forks named master or PR-target runs.
  // A verified artifact can be reused while its producer's unrelated jobs run.
  return sameId(run.id, artifact.workflow_run.id) &&
    sameId(run.repository?.id, repositoryId) && sameId(run.head_repository?.id, repositoryId) &&
    run.repository?.full_name?.toLowerCase() === repository.toLowerCase() &&
    run.head_repository?.full_name?.toLowerCase() === repository.toLowerCase() &&
    run.head_branch === "master" && run.head_sha === artifact.workflow_run.head_sha &&
    trustedEvents.has(run.event) && trustedWorkflows.has(run.path);
}

export function githubRequest({ token = process.env.GITHUB_TOKEN, apiUrl = process.env.GITHUB_API_URL || "https://api.github.com", fetchImpl = fetch } = {}) {
  return async (path) => {
    const response = await fetchImpl(`${apiUrl}${path}`, {
      headers: {
        Accept: "application/vnd.github+json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        "X-GitHub-Api-Version": "2022-11-28",
      },
      signal: AbortSignal.timeout(10_000),
      redirect: "error",
    });
    // Do not print response bodies or retry: this helper performs reads only.
    if (!response.ok) throw new Error(`Actions artifact lookup returned HTTP ${response.status}`);
    return response.json();
  };
}

export async function findArtifact({
  key, repository = process.env.GITHUB_REPOSITORY, repositoryId = process.env.GITHUB_REPOSITORY_ID,
  request = githubRequest(), now = Date.now(), minRemainingMs = 0, excludedIds = [],
}) {
  const name = artifactName(key);
  assert.match(repository ?? "", /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/, "repository context is required");
  assert.match(String(repositoryId), /^[1-9][0-9]*$/, "repository ID context is required");
  const context = { key, repository, repositoryId, now, minRemainingMs, excludedIds: new Set(excludedIds.map(String)) };
  const runs = new Map();
  // Exact-name lookup normally returns one artifact. Bound worst-case API work.
  for (let page = 1; page <= 5; page++) {
    const result = await request(`/repos/${repository}/actions/artifacts?name=${encodeURIComponent(name)}&per_page=100&page=${page}`);
    assert.ok(Array.isArray(result.artifacts), "invalid Actions artifact response");
    const artifacts = result.artifacts.filter((artifact) => candidate(artifact, context))
      .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at) || Number(b.id) - Number(a.id));
    for (const artifact of artifacts) {
      const id = artifact.workflow_run.id;
      if (!runs.has(id)) runs.set(id, await request(`/repos/${repository}/actions/runs/${id}`));
      if (isTrustedRun(runs.get(id), artifact, context)) return artifact;
    }
    if (result.artifacts.length < 100 || page * 100 >= result.total_count) break;
  }
  return null;
}

export async function verifyOutputs(repository, key) {
  artifactName(key);
  await check(repository, "release");
  const metadata = JSON.parse(await readFile(resolve(repository, ".ci-wasm-cache/outputs.json"), "utf8"));
  assert.equal(metadata.fingerprint, key.slice(-64), "artifact must match the requested exact cache key");
}

async function plainTree(directory) {
  const entry = await lstat(directory);
  assert.ok(entry.isDirectory() || entry.isFile(), "WASM artifacts must contain only regular files and directories");
  if (entry.isDirectory()) for (const child of await readdir(directory)) await plainTree(resolve(directory, child));
}

async function clearOutputs(repository) {
  for (const path of outputPaths) await rm(resolve(repository, path), { recursive: true, force: true });
}

export async function restoreOutputs(repository, directory, key) {
  assert.notEqual(resolve(repository), resolve(directory), "download artifacts outside the checkout root");
  try {
    await plainTree(directory);
    for (const path of outputPaths) assert.ok((await lstat(resolve(directory, path))).isDirectory(), `missing artifact directory: ${path}`);
    // Never merge a partial artifact with old cache files: all three sets must
    // come from one artifact. Ignore any paths outside this fixed allowlist.
    await clearOutputs(repository);
    for (const path of outputPaths) {
      await mkdir(dirname(resolve(repository, path)), { recursive: true });
      await cp(resolve(directory, path), resolve(repository, path), { recursive: true });
    }
    await verifyOutputs(repository, key);
    return true;
  } catch {
    await clearOutputs(repository);
    return false;
  }
}

export async function prepareSave({ repositoryDirectory = root, key, excludedIds = [], ...options }) {
  await verifyOutputs(repositoryDirectory, key);
  // A rejected newest artifact needs a newer verified replacement. Deduplicating
  // against an older artifact would leave the rejected one first on every future
  // restore, because exclusions are local to this run rather than a blacklist.
  if (excludedIds.length) return { name: artifactName(key), upload: true };
  let existing;
  try {
    // Refresh before expiration instead of indefinitely deduplicating an
    // artifact that is about to vanish. Uploads retain each generation 90 days.
    existing = await findArtifact({ ...options, key, minRemainingMs: 7 * day });
  } catch {
    console.warn("WASM artifact lookup unavailable; retaining these verified outputs in this run");
  }
  return { name: artifactName(key), upload: !existing, existingId: existing?.id };
}

async function output(values) {
  const text = Object.entries(values).map(([name, value]) => `${name}=${value ?? ""}\n`).join("");
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, text);
  else process.stdout.write(text);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, key, argument] = process.argv.slice(2);
  if (command === "find") {
    try {
      const artifact = await findArtifact({ key });
      await output({ "artifact-id": artifact?.id, "run-id": artifact?.workflow_run.id });
    } catch {
      console.warn("Retained WASM artifact unavailable; allowing the ordinary build");
      await output({ "artifact-id": "", "run-id": "" });
    }
  } else if (command === "restore" && argument) {
    const hit = await restoreOutputs(root, argument, key);
    console.log(hit ? "Retained WASM outputs verified" : "Retained WASM outputs invalid; allowing the ordinary build");
    await output({ hit });
  } else if (command === "prepare") {
    const prepared = await prepareSave({ key, excludedIds: argument ? [argument] : [] });
    await output({ name: prepared.name, upload: prepared.upload, "existing-id": prepared.existingId });
  } else throw new Error("usage: wasm-artifacts.mjs find <key> | restore <key> <directory> | prepare <key> [rejected-artifact-id]");
}
