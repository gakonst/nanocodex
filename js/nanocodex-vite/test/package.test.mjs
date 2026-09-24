import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";

import { nanocodex } from "../index.mjs";
import { nanocodexTools } from "../tools.mjs";

const exec = promisify(execFile);
const packageRoot = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", packageRoot), "utf8"));

test("the extracted plugin preserves all browser compatibility aliases", async () => {
  const tools = nanocodexTools();
  assert.match(tools.resolveId("@microsoft/dev-tunnels-ssh"), /devTunnelsSshBrowser\.mjs$/);
  assert.match(tools.resolveId("node-rsa"), /unsupportedNodeRsa\.mjs$/);
  assert.match(tools.resolveId("node:zlib"), /browserZlib\.mjs$/);
  const sprintfCompatibility = tools.resolveId("sprintf-js", "/consumer.js");
  assert.match(sprintfCompatibility, /browserSprintf\.mjs$/);
  assert.equal(tools.resolveId("sprintf-js", sprintfCompatibility), null);

  const plugin = nanocodex({ chatGpt: false });
  assert.match(plugin.resolveId("node:zlib"), /browserZlib\.mjs$/);

  const [{ gzipSync, gunzipSync }, { sprintf }] = await Promise.all([
    import(sprintfCompatibility.replace(/browserSprintf\.mjs$/, "browserZlib.mjs")),
    import(sprintfCompatibility),
  ]);
  assert.equal(new TextDecoder().decode(gunzipSync(gzipSync("package gzip"))), "package gzip");
  assert.equal(sprintf("package %s", "printf"), "package printf");
});

test("the packed package contains every public entry and source-checkout build integration", async () => {
  const temporary = await mkdtemp(join(tmpdir(), "nanocodex-vite-package-"));
  try {
    const { stdout } = await exec("npm", [
      "pack",
      "--json",
      "--ignore-scripts",
      "--pack-destination",
      temporary,
      new URL(".", packageRoot).pathname,
    ]);
    const [packed] = JSON.parse(stdout);
    const packedFiles = new Set(packed.files.map(({ path }) => path));
    assert.equal(packed.name, packageJson.name);
    assert.equal(packedFiles.has("scripts/build-js-package.sh"), true);
    assert.equal([...packedFiles].some((path) => path.startsWith("test/")), false);
    for (const conditions of Object.values(packageJson.exports)) {
      for (const target of Object.values(conditions)) {
        assert.equal(packedFiles.has(target.replace(/^\.\//, "")), true);
      }
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
});
