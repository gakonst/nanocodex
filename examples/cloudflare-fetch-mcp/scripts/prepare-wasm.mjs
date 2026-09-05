import { copyFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const wasmSource = fileURLToPath(import.meta.resolve("nanocodex/wasm"));
const wasmTarget = resolve(dirname(fileURLToPath(import.meta.url)), "../src/nanocodex.wasm");
const quickJsSource = fileURLToPath(import.meta.resolve("@jitl/quickjs-wasmfile-release-asyncify/wasm"));
const quickJsTarget = resolve(dirname(fileURLToPath(import.meta.url)), "../src/quickjs.wasm");

await mkdir(dirname(wasmTarget), { recursive: true });
await Promise.all([
  copyFile(wasmSource, wasmTarget),
  copyFile(quickJsSource, quickJsTarget),
]);
