import assert from "node:assert/strict";
import { gzipSync as nodeGzipSync } from "node:zlib";
import { test } from "node:test";
import {
  constants,
  gunzipSync,
  gzipSync,
} from "../tools/browser/browserZlib.mjs";

test("browser zlib roundtrips Node-compatible string and view inputs", () => {
  const source = "nanocodex browser gzip 🦀".repeat(64);
  const compressed = gzipSync(source, { level: constants.Z_BEST_COMPRESSION });
  assert(compressed instanceof Uint8Array);
  assert.equal(new TextDecoder().decode(gunzipSync(compressed)), source);

  const storage = Uint8Array.from([0, 1, 2, 3, 4, 5]);
  assert.deepEqual(
    [...gunzipSync(gzipSync(storage.subarray(1, 5)))],
    [1, 2, 3, 4],
  );
  const view = new DataView(storage.buffer, 2, 3);
  assert.deepEqual([...gunzipSync(gzipSync(view))], [2, 3, 4]);
});

test("browser gunzip enforces maxOutputLength while inflating", () => {
  const compressedBomb = nodeGzipSync(Buffer.alloc(2 * 1024 * 1024, 0x61));
  assert.throws(
    () => gunzipSync(compressedBomb, { maxOutputLength: 1024 }),
    (error) => error instanceof RangeError && error.code === "ERR_BUFFER_TOO_LARGE",
  );
  assert.equal(
    gunzipSync(compressedBomb, { maxOutputLength: 2 * 1024 * 1024 }).byteLength,
    2 * 1024 * 1024,
  );
});

test("browser zlib preserves relevant Node argument and data errors", () => {
  assert.throws(
    () => gzipSync(123),
    (error) => error instanceof TypeError && error.code === "ERR_INVALID_ARG_TYPE",
  );
  assert.throws(
    () => gunzipSync(new Uint8Array(), { maxOutputLength: 0 }),
    (error) => error instanceof RangeError && error.code === "ERR_OUT_OF_RANGE",
  );
  assert.throws(
    () => gunzipSync(new TextEncoder().encode("not gzip")),
    (error) => error.code === "Z_DATA_ERROR",
  );
});
