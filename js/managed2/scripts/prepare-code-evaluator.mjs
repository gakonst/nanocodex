import { copyFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const source = fileURLToPath(
  import.meta.resolve("@jitl/quickjs-wasmfile-release-asyncify/wasm"),
);
const target = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../src/quickjs.wasm",
);

await copyFile(source, target);

