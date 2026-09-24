import assert from "node:assert/strict";
import { gzipSync as nodeGzipSync } from "node:zlib";
import { test } from "node:test";
import {
  gunzipSync,
} from "../tools/browser/browserZlib.mjs";

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
