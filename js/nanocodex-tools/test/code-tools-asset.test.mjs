import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

for (const name of ["code-tools.mjs", "code-values.mjs"]) {
  test(`Rust packaged ${name} matches canonical JavaScript source`, async () => {
    const [canonical, generated] = await Promise.all([
      readFile(new URL(`../runtime/${name}`, import.meta.url), "utf8"),
      readFile(new URL(`../../../crates/nanocodex-tools/src/code_mode/${name}`, import.meta.url), "utf8"),
    ]);
    assert.equal(generated, canonical, "Run node js/nanocodex-tools/scripts/sync-code-tools.mjs");
  });
}
