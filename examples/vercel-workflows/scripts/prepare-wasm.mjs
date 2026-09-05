import { copyFile } from "node:fs/promises";

const source = new URL(import.meta.resolve("nanocodex/wasm"));
const target = new URL("../workflows/nanocodex.wasm", import.meta.url);

await copyFile(source, target);
