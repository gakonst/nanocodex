import { build } from "esbuild";
import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

// Wrangler's ESModule rule uploads this file separately instead of evaluating
// the interpreter inside the main worker on every chat-only activation.
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(root, ".shell-module/just-bash-lazy.mjs");
await mkdir(dirname(output), { recursive: true });
await build({
  entryPoints: [resolve(root, "../nanocodex-tools/tools/just-bash-browser.mjs")],
  outfile: output,
  bundle: true,
  format: "esm",
  platform: "node",
  target: "es2022",
  minify: true,
  logLevel: "warning",
});

await writeFile(output.replace(/\.mjs$/, ".d.mts"), `export { Bash, defineCommand } from "just-bash/browser";\n`);
