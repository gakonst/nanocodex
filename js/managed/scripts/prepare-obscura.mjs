import { build } from "esbuild";
import { copyFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url),
  root = new URL("../", import.meta.url);
await build({
  entryPoints: [fileURLToPath(new URL("obscura/runtime/worker.mjs", root))],
  outfile: fileURLToPath(new URL("src/obscura-assets/worker.txt", root)),
  bundle: true,
  platform: "browser",
  format: "esm",
  conditions: ["workerd"],
  external: ["quickjs.wasm", "dom.wasm", "bootstrap-source"],
});
await copyFile(
  require.resolve("@jitl/quickjs-wasmfile-release-sync/wasm"),
  new URL("src/obscura-assets/quickjs.bin", root),
);
