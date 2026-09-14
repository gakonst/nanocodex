import assert from "node:assert/strict";
import { gzipSync } from "node:zlib";
import test from "node:test";
import { packTar } from "modern-tar";
import { downloadRepositoryArchive } from "../dist/repository-archive.js";

const entry = (name, type = "file", body = "source") => ({
  header: { name, type, size: type === "file" ? Buffer.byteLength(body) : 0 },
  body: type === "file" ? body : undefined,
});
const root = entry("repo/", "directory");

for (const bad of [
  entry("repo/../escape"), entry("/escape"), entry("repo/.git/config"),
  entry("other/file"), entry("repo/link", "symlink"), entry("repo/device", "fifo"),
]) {
  test(`archive rejects ${bad.header.type} ${bad.header.name} before writing it`, async () => {
    const data = gzipSync(await packTar([root, bad]));
    const writes = [];
    const workspace = { mkdir: async () => {}, writeFile: async (path) => { writes.push(path); } };
    await assert.rejects(downloadRepositoryArchive(async () => ({ status: 200, body: data }),
      workspace, "fixture/repo", "HEAD", "/brain/repo"));
    assert.deepEqual(writes, []);
  });
}

test("archive drains outstanding writes before returning a failure", async () => {
  const data = gzipSync(await packTar([root, entry("repo/good"), entry("repo/../bad")]));
  let finished = false;
  const workspace = {
    mkdir: async () => {},
    writeFile: async () => { await new Promise((resolve) => setTimeout(resolve, 10)); finished = true; },
  };
  await assert.rejects(downloadRepositoryArchive(async () => ({ status: 200, body: data }),
    workspace, "fixture/repo", "HEAD", "/brain/repo"));
  assert.equal(finished, true);
});

test("archive cancellation settles in-flight writes before cleanup and stops the transport", async () => {
  const data = gzipSync(await packTar([root, entry("repo/file")]));
  const caller = new AbortController();
  let transportSignal;
  let finished = false;
  const workspace = {
    mkdir: async () => {},
    writeFile: async () => {
      caller.abort(new Error("caller cancelled"));
      await new Promise((resolve) => setTimeout(resolve, 10));
      finished = true;
    },
  };
  await assert.rejects(downloadRepositoryArchive(async (_url, options) => {
    transportSignal = options.signal;
    return { status: 200, body: data };
  }, workspace, "fixture/repo", "HEAD", "/brain/repo", caller.signal), /caller cancelled/);
  assert.equal(finished, true);
  assert.equal(transportSignal.aborted, true);
});

test("archive rejects truncated input instead of reporting a partial checkout as complete", async () => {
  const tar = await packTar([root, entry("repo/file", "file", "source".repeat(1_000))]);
  const workspace = { mkdir: async () => {}, writeFile: async () => {} };
  await assert.rejects(downloadRepositoryArchive(async () => ({ status: 200, body: gzipSync(tar.subarray(0, 1400)) }),
    workspace, "fixture/repo", "HEAD", "/brain/repo"));
});
