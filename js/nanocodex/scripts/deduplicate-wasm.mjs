import assert from "node:assert/strict";
import { readFile, rm, writeFile } from "node:fs/promises";

const nodeGlue = new URL("../pkg-node/nanocodex.js", import.meta.url);
const nodeWasm = new URL("../pkg-node/nanocodex_bg.wasm", import.meta.url);
const localPath = "`${__dirname}/nanocodex_bg.wasm`";
const sharedPath = "`${__dirname}/../pkg-web/nanocodex_bg.wasm`";
const source = await readFile(nodeGlue, "utf8");

assert.equal(
  source.split(localPath).length,
  2,
  "wasm-bindgen Node glue must contain exactly one local WASM path",
);
await writeFile(nodeGlue, source.replace(localPath, sharedPath));
await rm(nodeWasm);
