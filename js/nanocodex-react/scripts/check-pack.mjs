import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

const packed = spawnSync(
  process.platform === "win32" ? "npm.cmd" : "npm",
  ["pack", "--ignore-scripts", "--dry-run", "--json"],
  { encoding: "utf8" },
);
assert.equal(packed.status, 0, packed.stderr);
const [manifest] = JSON.parse(packed.stdout);
const files = manifest.files.map(({ path }) => path);

assert(files.includes("index.mjs"));
assert(files.includes("index.d.mts"));
assert(files.includes("agent/index.mjs"));
assert(files.includes("agent/index.d.mts"));
assert(files.includes("agent/transcript.mjs"));
assert(files.includes("cloud/index.mjs"));
assert(files.includes("cloud/index.d.mts"));
assert(files.includes("cloud/connectAgentSource.mjs"));
assert(!files.some((path) => path.startsWith("scripts/")));
assert(!files.some(isRawTypeScript));

function isRawTypeScript(path) {
  return /\.(?:ts|tsx|mts|cts)$/.test(path)
    && !/\.d\.(?:ts|mts|cts)$/.test(path);
}
