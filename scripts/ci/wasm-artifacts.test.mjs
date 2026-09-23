import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { artifactName, findArtifact, githubRequest, outputPaths, prepareSave, restoreOutputs, verifyOutputs } from "./wasm-artifacts.mjs";
import { check, fingerprintInputs, save } from "../../js/nanocodex-vite/scripts/wasm-output-cache.mjs";
import { hashManagedWasmArtifacts, managedWasmArtifactNames } from "../../js/nanocodex/scripts/check-managed-wasm.mjs";

const now = Date.parse("2026-01-01T00:00:00Z");
const key = `wasm-outputs-v1-Linux-X64-${"a".repeat(64)}`;
const context = { key, repository: "fixture/project", repositoryId: "123", now };
const sha = "b".repeat(40);
const artifact = (overrides = {}) => ({
  id: 50, name: artifactName(key), size_in_bytes: 1024, expired: false,
  created_at: "2025-12-30T00:00:00Z", expires_at: "2026-03-01T00:00:00Z",
  workflow_run: { id: 7, repository_id: 123, head_repository_id: 123, head_branch: "master", head_sha: sha },
  ...overrides,
});
const run = (overrides = {}) => ({
  id: 7, repository: { id: 123, full_name: "fixture/project" },
  head_repository: { id: 123, full_name: "fixture/project" }, head_branch: "master", head_sha: sha,
  event: "push", path: ".github/workflows/cloudflare.yml", status: "in_progress", conclusion: null,
  ...overrides,
});
function api(artifacts, runs = new Map([[7, run()]])) {
  const calls = [];
  const request = async (path) => {
    calls.push(path);
    if (path.includes("/actions/artifacts?")) return { total_count: artifacts.length, artifacts };
    const result = runs.get(Number(path.split("/").at(-1)));
    assert.ok(result, `unexpected API request ${path}`);
    return result;
  };
  return { request, calls };
}

test("exact-name query selects a trusted master artifact while other producer jobs run", async () => {
  const mock = api([artifact()]);
  assert.equal((await findArtifact({ ...context, request: mock.request })).id, 50);
  assert.equal(mock.calls[0], `/repos/fixture/project/actions/artifacts?name=${artifactName(key)}&per_page=100&page=1`);
  assert.equal(mock.calls.length, 2);
  for (const [event, path] of [["workflow_dispatch", ".github/workflows/cloudflare.yml"], ["schedule", ".github/workflows/ci.yml"]]) {
    assert.equal((await findArtifact({ ...context, request: api([artifact()], new Map([[7, run({ event, path })]])).request })).id, 50);
  }
});

test("selection rejects wrong keys, expiration, missing provenance, forks and branch artifacts before fetching runs", async () => {
  const original = artifact();
  for (const overrides of [
    { name: `${original.name}-other` }, { expired: true }, { expires_at: "2025-12-31T23:59:59Z" },
    { expires_at: "invalid" }, { size_in_bytes: 0 }, { id: "50\ninjected=true" }, { workflow_run: undefined },
    ...[{ head_branch: "feature" }, { repository_id: 456 }, { head_repository_id: 456 }, { head_sha: "abc" }]
      .map((change) => ({ workflow_run: { ...original.workflow_run, ...change } })),
  ]) {
    const mock = api([artifact(overrides)]);
    assert.equal(await findArtifact({ ...context, request: mock.request }), null, JSON.stringify(overrides));
    assert.equal(mock.calls.length, 1);
  }
});

test("run provenance rejects PR and PR-target events even on same-repo master", async () => {
  for (const overrides of [
    { id: 8 }, { head_branch: "feature" }, { head_sha: "c".repeat(40) },
    { event: "pull_request" }, { event: "pull_request_target" }, { event: "workflow_run" },
    { path: ".github/workflows/untrusted.yml" }, { path: ".github/workflows/ci.yml@refs/pull/1/merge" },
    { repository: { id: 456, full_name: "fixture/project" } },
    { head_repository: { id: 456, full_name: "fork/project" } },
    { repository: { id: 123, full_name: "other/project" } },
    { head_repository: { id: 123, full_name: "other/project" } },
  ]) {
    const mock = api([artifact()], new Map([[7, run(overrides)]]));
    assert.equal(await findArtifact({ ...context, request: mock.request }), null, JSON.stringify(overrides));
  }
});

test("selection prefers newest trusted candidate and ignores rejected or nearly expired artifacts", async () => {
  const artifacts = [
    artifact({ id: 51, created_at: "2025-12-31T00:00:00Z", workflow_run: { ...artifact().workflow_run, id: 8 } }),
    artifact({ id: 52, created_at: "2025-12-31T12:00:00Z", workflow_run: { ...artifact().workflow_run, id: 9 } }),
    artifact(),
  ];
  const mock = api(artifacts, new Map([[7, run()], [8, run({ id: 8 })], [9, run({ id: 9, event: "pull_request_target" })]]));
  assert.equal((await findArtifact({ ...context, request: mock.request })).id, 51);
  assert.equal((await findArtifact({ ...context, request: mock.request, excludedIds: [51] })).id, 50);
  assert.equal(await findArtifact({ ...context, request: api([artifact({ expires_at: "2026-01-03T00:00:00Z" })]).request, minRemainingMs: 7 * 86400_000 }), null);
});

test("pagination reaches a trusted artifact after a full page of ineligible results", async () => {
  const calls = [];
  const request = async (path) => {
    calls.push(path);
    if (path.endsWith("page=1")) return { total_count: 101, artifacts: Array.from({ length: 100 }, (_, id) => artifact({ id: id + 100, expired: true })) };
    if (path.endsWith("page=2")) return { total_count: 101, artifacts: [artifact()] };
    return run();
  };
  assert.equal((await findArtifact({ ...context, request })).id, 50);
  assert.equal(calls.length, 3);
});

test("API read failures propagate without retries or response-body disclosure", async () => {
  let calls = 0;
  const request = githubRequest({ token: "fixture-token", fetchImpl: async (_url, options) => {
    calls++;
    assert.equal(options.headers.Authorization, "Bearer fixture-token");
    assert.equal(options.redirect, "error");
    return { ok: false, status: 403, json: () => { throw new Error("response body must not be read"); } };
  } });
  await assert.rejects(findArtifact({ ...context, request }), /HTTP 403/);
  assert.equal(calls, 1);
});

async function outputFixture(t) {
  const temporary = await mkdtemp(resolve(tmpdir(), "nanocodex-wasm-artifacts-"));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const repository = resolve(temporary, "checkout");
  const directory = resolve(temporary, "download");
  const put = async (path, value) => {
    await mkdir(resolve(repository, path, ".."), { recursive: true });
    await writeFile(resolve(repository, path), value);
  };
  await put("Cargo.toml", "[workspace]\n");
  await put("Cargo.lock", "locked");
  await put("js/nanocodex/Cargo.toml", '[package]\nname = "fixture"\n');
  await put("js/nanocodex/src/lib.rs", "fixture Rust input");
  await put("js/nanocodex/package.json", '{"devDependencies":{"binaryen":"132.0.0"}}');
  for (const path of ["js/nanocodex-vite/scripts/build-js-package.sh", "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs", "js/nanocodex/scripts/deduplicate-wasm.mjs", "js/nanocodex/scripts/write-package-types.mjs", "js/nanocodex/scripts/write-wasm-attestation.mjs", "js/nanocodex/scripts/check-managed-wasm.mjs"]) await put(path, "fixture build policy");
  for (const name of managedWasmArtifactNames) await put(`js/nanocodex/pkg-web/${name}`, `web ${name}`);
  for (const name of ["nanocodex.js", "nanocodex.d.ts", "package.json"]) await put(`js/nanocodex/pkg-node/${name}`, `node ${name}`);
  await put("raw.wasm", "raw WASM fixture");
  await put("js/nanocodex/pkg-web/nanocodex-build.json", JSON.stringify({
    schema: 1, revision: sha, dirty: false,
    sourceWasmSha256: createHash("sha256").update("raw WASM fixture").digest("hex"),
    artifacts: await hashManagedWasmArtifacts(pathToFileURL(`${resolve(repository, "js/nanocodex/pkg-web")}/`)),
  }));
  await save(repository, "release", resolve(repository, "raw.wasm"));
  const fixtureKey = `wasm-outputs-v1-Linux-X64-${await fingerprintInputs(repository)}`;
  for (const path of outputPaths) {
    await mkdir(resolve(directory, path, ".."), { recursive: true });
    await cp(resolve(repository, path), resolve(directory, path), { recursive: true });
  }
  return { repository, directory, key: fixtureKey, put };
}

test("retained outputs survive a completely cold cache and verify with the unchanged helper", async (t) => {
  const fixture = await outputFixture(t);
  for (const path of outputPaths) await rm(resolve(fixture.repository, path), { recursive: true });
  // Only fixed output paths are installed, even if an archive contains extras.
  await writeFile(resolve(fixture.directory, "Cargo.lock"), "untrusted source");
  assert.equal(await restoreOutputs(fixture.repository, fixture.directory, fixture.key), true);
  await check(fixture.repository, "release");
  assert.equal(await readFile(resolve(fixture.repository, "Cargo.lock"), "utf8"), "locked");
});

test("corrupt browser, Node, raw WASM and metadata cause a clean fallback to building", async (t) => {
  for (const path of ["js/nanocodex/pkg-web/nanocodex_bg.wasm", "js/nanocodex/pkg-node/nanocodex.js", ".ci-wasm-cache/source.wasm", ".ci-wasm-cache/outputs.json", "js/nanocodex/pkg-web/nanocodex-build.json"]) {
    const fixture = await outputFixture(t);
    await writeFile(resolve(fixture.directory, path), "corrupt");
    assert.equal(await restoreOutputs(fixture.repository, fixture.directory, fixture.key), false, path);
    for (const output of outputPaths) await assert.rejects(readFile(resolve(fixture.repository, output)), { code: "ENOENT" });
  }
});

test("a partial artifact cannot borrow missing outputs from the cache", async (t) => {
  const fixture = await outputFixture(t);
  await rm(resolve(fixture.directory, "js/nanocodex/pkg-node/nanocodex.js"));
  assert.equal(await restoreOutputs(fixture.repository, fixture.directory, fixture.key), false);
  await assert.rejects(check(fixture.repository));
});

test("stale Rust inputs, wrong keys and symlinks are rejected", async (t) => {
  const fixture = await outputFixture(t);
  await assert.rejects(verifyOutputs(fixture.repository, key), /exact cache key/);
  await fixture.put("js/nanocodex/src/lib.rs", "changed Rust input");
  assert.equal(await restoreOutputs(fixture.repository, fixture.directory, fixture.key), false);
  const linked = await outputFixture(t);
  await symlink(resolve(linked.repository, "raw.wasm"), resolve(linked.directory, "escape"));
  assert.equal(await restoreOutputs(linked.repository, linked.directory, linked.key), false);
});

test("saving seeds durable storage on cache hits, deduplicates and refreshes near expiry", async (t) => {
  const fixture = await outputFixture(t);
  const options = { ...context, key: fixture.key, repositoryDirectory: fixture.repository };
  const retained = artifact({ name: artifactName(fixture.key) });
  assert.equal((await prepareSave({ ...options, request: api([]).request })).upload, true);
  assert.equal((await prepareSave({ ...options, request: api([retained]).request })).upload, false);
  assert.equal((await prepareSave({ ...options, request: api([retained]).request, excludedIds: [retained.id] })).upload, true);
  assert.equal((await prepareSave({ ...options, request: api([{ ...retained, expires_at: "2026-01-04T00:00:00Z" }]).request })).upload, true);
  assert.equal((await prepareSave({ ...options, request: async () => { throw new Error("offline"); } })).upload, true);
  await fixture.put(".ci-wasm-cache/source.wasm", "corrupt");
  await assert.rejects(prepareSave({ ...options, request: api([]).request }));
});

test("a rejected newest artifact forces a fresh upload even when an older trusted artifact exists", async (t) => {
  const fixture = await outputFixture(t);
  const name = artifactName(fixture.key);
  const older = artifact({ id: 49, name, created_at: "2025-12-29T00:00:00Z" });
  const rejected = artifact({ id: 50, name });
  const mock = api([older, rejected]);
  assert.equal((await findArtifact({ ...context, key: fixture.key, request: mock.request })).id, rejected.id);
  const lookupCount = mock.calls.length;
  const prepared = await prepareSave({
    ...context, key: fixture.key, repositoryDirectory: fixture.repository,
    request: mock.request, excludedIds: [rejected.id],
  });
  assert.equal(prepared.upload, true, "an older artifact cannot suppress replacement of the rejected newest artifact");
  assert.equal(prepared.existingId, undefined);
  assert.equal(mock.calls.length, lookupCount, "rejected artifacts bypass deduplication lookup");
  const replacement = artifact({ id: 51, name, created_at: "2025-12-31T00:00:00Z" });
  assert.equal((await findArtifact({ ...context, key: fixture.key, request: api([older, rejected, replacement]).request })).id, replacement.id);
});
