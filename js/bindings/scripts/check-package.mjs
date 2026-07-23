import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", root), "utf8"));

assert.equal(packageJson.name, "nanocodex");
assert.equal(packageJson.type, "module");
assert.equal(packageJson.publishConfig?.access, "public");
assert.equal(packageJson.exports?.["./browser"]?.import, "./browser/index.mjs");
assert.equal(packageJson.exports?.["./node"]?.import, "./node/index.mjs");

const requiredFiles = [
  "browser/index.mjs",
  "browser/index.d.mts",
  "node/index.mjs",
  "node/index.d.mts",
  "pkg-web/nanocodex.js",
  "pkg-web/nanocodex.d.ts",
  "pkg-web/nanocodex_bg.wasm",
  "pkg-node/nanocodex.js",
  "pkg-node/nanocodex.d.ts",
  "pkg-node/nanocodex_bg.wasm",
];

for (const file of requiredFiles) {
  const metadata = await stat(new URL(file, root));
  assert(metadata.isFile(), `${file} must be a file`);
  assert(metadata.size > 0, `${file} must not be empty`);
}

for (const target of ["web", "node"]) {
  const wasm = await readFile(new URL(`pkg-${target}/nanocodex_bg.wasm`, root));
  assert(wasm.byteLength > 100_000, `pkg-${target} WASM is unexpectedly small`);
  assert.deepEqual([...wasm.subarray(0, 4)], [0x00, 0x61, 0x73, 0x6d]);
}

console.log(`nanocodex@${packageJson.version} package artifacts are complete`);
