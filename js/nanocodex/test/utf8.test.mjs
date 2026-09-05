import assert from "node:assert/strict";
import { test } from "node:test";

import { utf8ByteLength } from "../runtime/utf8.mjs";

const encoder = new TextEncoder();

test("allocation-free UTF-8 sizing matches the Web Platform encoder", () => {
  for (const value of [
    "",
    "plain ASCII",
    "Καλημέρα κόσμε",
    "你好，世界",
    "agent 🤖✨",
    "paired \ud83e\udd16 and unpaired \ud800 x \udfff",
    "a\0b\r\n",
  ]) {
    assert.equal(utf8ByteLength(value), encoder.encode(value).byteLength, JSON.stringify(value));
  }
});
