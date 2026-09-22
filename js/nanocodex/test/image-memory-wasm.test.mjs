import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { deflateSync } from "node:zlib";
import { Agent, Transport } from "../host/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { viewImage } from "../tools/standard.mjs";

function png(width, height, paddedBytes = 0, alpha = false) {
  function crc32(bytes) {
    let crc = 0xffffffff;
    for (const byte of bytes) {
      crc ^= byte;
      for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
    return (crc ^ 0xffffffff) >>> 0;
  }
  function chunk(type, body) {
    const payload = Buffer.concat([Buffer.from(type), body]);
    const length = Buffer.alloc(4), checksum = Buffer.alloc(4);
    length.writeUInt32BE(body.length);
    checksum.writeUInt32BE(crc32(payload));
    return Buffer.concat([length, payload, checksum]);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = alpha ? 6 : 2;
  const encoded = Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.alloc((width * (alpha ? 4 : 3) + 1) * height))), chunk("IEND", Buffer.alloc(0)),
  ]);
  // Valid trailing padding exercises ordinary photo-sized transfer/JSON copies.
  return Buffer.concat([encoded, Buffer.alloc(Math.max(0, paddedBytes - encoded.length))]);
}

async function inspect(bytes) {
  let calls = 0;
  let imageOutput;
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
    close() { this.readyState = 3; }
    send(encoded) {
      const request = JSON.parse(encoded);
      calls++;
      if (calls === 2) imageOutput = request.input.find(item => item.type === "function_call_output").output;
      const output = calls === 1 ? [{ type: "function_call", call_id: "fixture-image",
        name: "view_image", arguments: '{"path":"/brain/fixture.png","detail":"original"}' }]
        : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "finished" }] }];
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed", response: { id: `image-${calls}`, status: "completed",
          end_turn: calls > 1, output, usage: { input_tokens: 100, output_tokens: 5, total_tokens: 105 } },
      }) })));
    }
  }
  const agent = await Agent.create({ harness: false, rawApiEvents: false,
    tools: [viewImage({ workspace: { readFile: async () => bytes } })],
    transport: Transport.openAi({ apiKey: "fixture", websocketWarmup: false,
      createWebSocket: () => ({ socket: new ModelSocket(), reasoningIncluded: true }) }),
  });
  let resultEvents = 0;
  let resultDetail;
  const stop = agent.events.watch().onEvent(event => {
    if (event.type === "tool.result") {
      resultEvents++;
      resultDetail = event.payload.structured_result?.detail;
    }
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "Inspect the fixture." }).result()).finalMessage, "finished");
    assert.equal(calls, 2);
    assert.equal(resultEvents, 1);
    assert.equal(resultDetail, "original");
    return imageOutput;
  } finally { stop(); await agent.session.shutdown(); }
}

test("real WASM bounds image preparation and completes a normal 12 MP original-detail tool", { timeout: 60_000 }, async t => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const wasm = await initializeBrowserEngine({ module });
  const oversized = await inspect(png(8064, 6048));
  assert.deepEqual(oversized.map(item => item.type), ["input_text"]);
  assert.match(oversized[0].text, /image content omitted/);
  const rejectedBytes = wasm.memory.buffer.byteLength;
  assert.ok(rejectedBytes < 32 * 1024 * 1024, `oversized image allocated ${rejectedBytes} WASM bytes`);

  const output = await inspect(png(4032, 3024, 3_600_000));
  assert.equal(output[0].type, "input_image");
  const prepared = Buffer.from(output[0].image_url.split(",")[1], "base64");
  assert.equal(prepared.readUInt32BE(16), 3669);
  assert.equal(prepared.readUInt32BE(20), 2752);
  assert.equal(prepared[25], 2);
  const preparedBytes = wasm.memory.buffer.byteLength;
  assert.ok(preparedBytes < 128 * 1024 * 1024,
    `12 MP RGB image allocated ${preparedBytes} WASM bytes`);

  // The same dimensions in RGBA require a larger source pixel buffer. Reject
  // before decoding, including after a previous image has grown the allocator.
  const rgba = await inspect(png(4032, 3024, 3_600_000, true));
  assert.deepEqual(rgba.map(item => item.type), ["input_text"]);
  assert.match(rgba[0].text, /image content omitted/);
  const rgbaRejectedBytes = wasm.memory.buffer.byteLength;
  assert.equal(rgbaRejectedBytes, preparedBytes,
    "over-budget RGBA decode must not grow WASM after the RGB image");

  // This guards WASM allocation, not the total Worker isolate working set.
  t.diagnostic(JSON.stringify({ rejectedBytes, preparedBytes, rgbaRejectedBytes }));
});
