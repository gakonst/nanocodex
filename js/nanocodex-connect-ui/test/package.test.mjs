import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";
import { test } from "node:test";

const packageRoot = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", packageRoot), "utf8"));

test("the public policy .mjs subpath uses generated runtime and declaration output", async () => {
  assert.deepEqual(packageJson.exports["./connectPolicy.mjs"], {
    types: "./dist/connectPolicy.d.mts",
    import: "./dist/connectPolicy.mjs",
  });
  for (const target of Object.values(packageJson.exports["./connectPolicy.mjs"])) {
    assert.equal((await stat(new URL(target, packageRoot))).isFile(), true);
  }
});
