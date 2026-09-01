import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const packageJson = JSON.parse(await readFile(
  new URL("./package.json", import.meta.url),
  "utf8",
));

test("web development prepares the managed QuickJS WASM asset before Vite", () => {
  assert.match(
    packageJson.scripts["predev:app"] ?? "",
    /prepare:code-evaluator/,
  );
  assert.match(packageJson.scripts["dev:app"] ?? "", /vite/);
});
