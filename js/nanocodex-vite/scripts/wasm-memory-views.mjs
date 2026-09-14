import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** Normalize wasm-bindgen's byte views without changing the Rust/WASM ABI. */
export function rewriteWasmMemoryViews(source) {
  // V8 subarray() checks its begin offset against the embedder's maximum
  // ArrayBuffer allocation (128 MiB on Cloudflare). WASM memory.grow can exceed
  // that address, so even a tiny, valid string then throws RangeError. A direct
  // view checks the requested length and the actual memory bounds instead.
  const replacements = [
    ["getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len)",
      "new Uint8Array(wasm.memory.buffer, ptr, len)"],
    ["getUint8ArrayMemory0().subarray(ptr, ptr + len)",
      "new Uint8Array(wasm.memory.buffer, ptr, len)"],
    ["getUint8ArrayMemory0().subarray(ptr, ptr + buf.length)",
      "new Uint8Array(wasm.memory.buffer, ptr, buf.length)"],
    ["getUint8ArrayMemory0().subarray(ptr + offset, ptr + len)",
      "new Uint8Array(wasm.memory.buffer, ptr + offset, len - offset)"],
  ];
  for (const [before, after] of replacements) {
    if (source.split(before).length !== 2) {
      throw new Error(`unsupported wasm-bindgen memory view: expected exactly one ${before}`);
    }
    source = source.replace(before, after);
  }
  if (source.includes("getUint8ArrayMemory0().subarray(")) {
    throw new Error("unsupported wasm-bindgen memory view remains");
  }
  return source;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const paths = process.argv.slice(2);
  if (paths.length === 0) throw new Error("expected generated wasm-bindgen JavaScript paths");
  // Validate all targets before writing any, so generator drift fails the build.
  const outputs = await Promise.all(paths.map(async (path) => [path, rewriteWasmMemoryViews(await readFile(path, "utf8"))]));
  await Promise.all(outputs.map(([path, source]) => writeFile(path, source)));
}
