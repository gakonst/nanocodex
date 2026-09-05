import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";

const exec = promisify(execFile);
const packageRoot = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", packageRoot), "utf8"));

test("exposes one typed ESM protocol entry point", async () => {
  assert.equal(packageJson.name, "nanocodex-connect-protocol");
  assert.equal(packageJson.type, "module");
  assert.deepEqual(packageJson.exports, {
    ".": {
      types: "./dist/index.d.mts",
      import: "./dist/index.mjs",
    },
  });

  for (const target of Object.values(packageJson.exports["."])) {
    assert.equal((await stat(new URL(target, packageRoot))).isFile(), true);
  }
});

test("the packed package contains its public runtime and types but no tests", async () => {
  const temporary = await mkdtemp(join(tmpdir(), "nanocodex-connect-protocol-"));
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
    assert.equal(packedFiles.has("dist/index.mjs"), true);
    assert.equal(packedFiles.has("dist/index.d.mts"), true);
    assert.equal([...packedFiles].some((path) => path.startsWith("test/")), false);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
});
