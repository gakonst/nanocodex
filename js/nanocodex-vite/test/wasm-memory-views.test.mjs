import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { test } from "node:test";
import { rewriteWasmMemoryViews } from "../scripts/wasm-memory-views.mjs";

const LIMIT = 128 * 1024 * 1024;

for (const target of ["pkg-web/nanocodex.js", "pkg-web/nanocodex_bg.js", "pkg-node/nanocodex.js"]) {
  test(`${target} transfers strings and bytes above the Worker subarray offset ceiling`, async () => {
    const source = await readFile(new URL(`../../nanocodex/${target}`, import.meta.url), "utf8");
    const memory = new WebAssembly.Memory({ initial: 1 });
    memory.grow(2_304);
    // Wrangler remote preview reproduces this V8 embedder limit. Local
    // workerd/Node do not configure it, so apply that constraint in this realm.
    class WorkerUint8Array extends Uint8Array {
      subarray(begin, end) {
        if (begin > LIMIT) throw new RangeError("Invalid array buffer length");
        return super.subarray(begin, end);
      }
    }
    assert.throws(() => new WorkerUint8Array(memory.buffer).subarray(LIMIT + 32, LIMIT + 33), RangeError);
    let cursor = LIMIT + 32;
    const malloc = (size) => { const ptr = cursor; cursor += Math.max(8, size); return ptr; };
    const realloc = (ptr, oldSize, newSize) => {
      const next = malloc(newSize);
      new Uint8Array(memory.buffer, next, Math.min(oldSize, newSize))
        .set(new Uint8Array(memory.buffer, ptr, Math.min(oldSize, newSize)));
      return next;
    };
    const helpers = ["getArrayU8FromWasm0", "getStringFromWasm0", "getUint8ArrayMemory0", "decodeText", "passStringToWasm0"]
      .map((name) => generatedFunction(source, name)).join("\n");
    const transfer = runInNewContext(`
      let cachedUint8ArrayMemory0 = null;
      let cachedTextDecoder = new TextDecoder("utf-8", { ignoreBOM: true, fatal: true });
      const cachedTextEncoder = new TextEncoder();
      const MAX_SAFARI_DECODE_BYTES = 2146435072;
      let numBytesDecoded = 0;
      let WASM_VECTOR_LEN = 0;
      ${helpers}
      (value, useRealloc) => {
        const ptr = passStringToWasm0(value, malloc, useRealloc ? realloc : undefined);
        return { ptr, value: getStringFromWasm0(ptr, WASM_VECTOR_LEN), bytes: getArrayU8FromWasm0(ptr, WASM_VECTOR_LEN) };
      }
    `, { wasm: { memory }, Uint8Array: WorkerUint8Array, TextEncoder, TextDecoder, malloc, realloc });
    for (const value of ["", "durability checkpoint", "ASCII then Ελληνικά 😀", "\uFEFFpreserve BOM", "\0embedded NUL"]) {
      for (const useRealloc of [false, true]) {
        const result = transfer(value, useRealloc);
        assert.ok(result.ptr > LIMIT);
        assert.equal(result.value, value);
        assert.deepEqual([...result.bytes], [...new TextEncoder().encode(value)]);
      }
    }
    // A grown memory detaches the prior cache. The next transfer must use the
    // current backing buffer, including the ASCII prefix/realloc path.
    memory.grow(1);
    assert.equal(transfer("after growth 😀", true).value, "after growth 😀");
  });
}

test("the build rejects an unknown wasm-bindgen memory-view ABI", () => {
  assert.throws(() => rewriteWasmMemoryViews("unknown generated glue"), /unsupported wasm-bindgen memory view/);
});

function generatedFunction(source, name) {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `${name} must be present in generated glue`);
  const end = source.indexOf("\n}", start);
  assert.notEqual(end, -1);
  return source.slice(start, end + 2);
}
