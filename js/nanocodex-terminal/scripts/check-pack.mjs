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
assert(files.includes("dist/index.js"));
assert(files.includes("dist/index.d.ts"));
assert(files.includes("styles.css"));
assert(!files.some((path) => path.startsWith("scripts/")));
assert(!files.some((path) => /\.(?:ts|tsx|mts|cts)$/.test(path) && !/\.d\.(?:ts|mts|cts)$/.test(path)));
