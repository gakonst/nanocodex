// The JS package is canonical; keep local assets for independently packed crates.
import { readFile, writeFile } from "node:fs/promises";
for (const name of ["code-tools.mjs", "code-values.mjs"]) {
  const canonical = new URL(`../runtime/${name}`, import.meta.url);
  const generated = new URL(`../../../crates/nanocodex-tools/src/code_mode/${name}`, import.meta.url);
  const source = await readFile(canonical, "utf8");
  if (process.argv.includes("--check")) {
    if (await readFile(generated, "utf8") !== source) {
      throw new Error(`Rust Code Mode ${name} is stale. Run node js/nanocodex-tools/scripts/sync-code-tools.mjs`);
    }
  } else {
    await writeFile(generated, source);
  }
}
