import assert from "node:assert/strict";
import { test } from "node:test";
import { parquetWriteBuffer } from "hyparquet-writer";
import { createDatasetTool } from "../tools/datasetEngine.mjs";
const PARQUET_URL = "https://performance.example/records.parquet";
const JSONL_URL = "https://performance.example/records.jsonl";
const parquetBytes = new Uint8Array(parquetWriteBuffer({
  columnData: [
    { name: "id", data: [1, 2, 3, 4], type: "INT32" },
    { name: "label", data: ["one", "two", "three", "four"], type: "STRING" }
  ],
  codec: "SNAPPY",
  rowGroupSize: 2
}));
const jsonlBytes = new TextEncoder().encode([
  { id: 1, label: "one" },
  { id: 2, label: "two" },
  { id: 3, label: "three" },
  { id: 4, label: "four" }
].map(JSON.stringify).join("\n") + "\n");
test("an identical Parquet query is served from the result cache without fetching", async () => {
  const remote = objectFetch(/* @__PURE__ */ new Map([[PARQUET_URL, parquetBytes]]));
  const tool = createDatasetTool({ fetch: remote.fetch, randomId: () => "parquet-cache" });
  const context = { sessionId: "parquet-cache-session" };
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: PARQUET_URL }
  }, context);
  const query = {
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["id", "label"],
    filters: [{ column: "id", op: "gte", value: 2 }],
    limit: 2
  };
  const cold = await tool.handler(query, context);
  const fetchesAfterColdQuery = remote.calls.length;
  const warm = await tool.handler(query, context);
  assert.equal(cold.cacheHit, false);
  assert.equal(warm.cacheHit, true);
  assert.equal(warm.bytesRead, 0);
  assert.deepEqual(warm.rows, cold.rows);
  assert.equal(remote.calls.length, fetchesAfterColdQuery);
});
test("an identical JSONL query is served from the result cache without fetching", async () => {
  const remote = objectFetch(/* @__PURE__ */ new Map([[JSONL_URL, jsonlBytes]]));
  const tool = createDatasetTool({ fetch: remote.fetch, randomId: () => "jsonl-cache" });
  const context = { sessionId: "jsonl-cache-session" };
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: JSONL_URL }
  }, context);
  const query = {
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["label"],
    filters: [{ column: "id", op: "gte", value: 2 }],
    limit: 2
  };
  const cold = await tool.handler(query, context);
  const fetchesAfterColdQuery = remote.calls.length;
  const warm = await tool.handler(query, context);
  assert.equal(cold.cacheHit, false);
  assert.equal(warm.cacheHit, true);
  assert.equal(warm.bytesRead, 0);
  assert.deepEqual(warm.rows, cold.rows);
  assert.equal(remote.calls.length, fetchesAfterColdQuery);
});
test("Snappy Parquet uses hyparquet's built-in decoder without loading compressors", async () => {
  const remote = objectFetch(/* @__PURE__ */ new Map([[PARQUET_URL, parquetBytes]]));
  let compressorLoads = 0;
  const tool = createDatasetTool({
    fetch: remote.fetch,
    randomId: () => "snappy",
    loadCompressors: async () => {
      compressorLoads++;
      throw new Error("hyparquet-compressors must not load for Snappy");
    }
  });
  const context = { sessionId: "snappy-session" };
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: PARQUET_URL }
  }, context);
  const queried = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["label"],
    limit: 4
  }, context);
  assert.deepEqual(queried.rows, [
    { label: "one" },
    { label: "two" },
    { label: "three" },
    { label: "four" }
  ]);
  assert.equal(compressorLoads, 0);
});
test("caller cancellation reaches an in-flight dataset fetch", async () => {
  const controller = new AbortController();
  let receivedSignal;
  let signalReceived;
  const started = new Promise((resolve) => signalReceived = resolve);
  const fetch = async (_input, init) => {
    receivedSignal = init?.signal ?? void 0;
    assert.ok(receivedSignal);
    signalReceived();
    return await new Promise((_resolve, reject) => {
      const rejectAborted = () => reject(receivedSignal.reason);
      if (receivedSignal.aborted) rejectAborted();
      else receivedSignal.addEventListener("abort", rejectAborted, { once: true });
    });
  };
  const tool = createDatasetTool({ fetch, randomId: () => "cancelled" });
  const pending = tool.handler({
    operation: "open",
    source: { kind: "url", url: JSONL_URL }
  }, { sessionId: "cancellation-session", signal: controller.signal });
  await started;
  controller.abort();
  await assert.rejects(pending, (error) => error?.name === "AbortError");
  assert.equal(receivedSignal?.aborted, true);
});
test("opening JSONL performs one GET and no HEAD request", async () => {
  const remote = objectFetch(/* @__PURE__ */ new Map([[JSONL_URL, jsonlBytes]]));
  const tool = createDatasetTool({ fetch: remote.fetch, randomId: () => "jsonl-open" });
  await tool.handler({
    operation: "open",
    source: { kind: "url", url: JSONL_URL }
  }, { sessionId: "jsonl-open-session" });
  assert.deepEqual(remote.calls, [{ method: "GET", url: JSONL_URL }]);
});
test("Parquet range responses cannot buffer past the requested length", async () => {
  const fetch = async (_input, init) => {
    if (init?.method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: { "content-length": String(parquetBytes.byteLength) }
      });
    }
    const range = new Headers(init?.headers).get("range");
    const match = range.match(/^bytes=(\d+)-(\d+)$/);
    const start = Number(match[1]);
    const end = Number(match[2]);
    const expected = end - start + 1;
    return new Response(new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array(expected + 1));
        controller.close();
      }
    }), {
      status: 206,
      headers: { "content-range": `bytes ${start}-${end}/${parquetBytes.byteLength}` }
    });
  };
  const tool = createDatasetTool({ fetch, randomId: () => "oversized-range" });
  await assert.rejects(tool.handler({
    operation: "open",
    source: { kind: "url", url: PARQUET_URL }
  }, { sessionId: "oversized-range-session" }), /exceeds expected/);
});
function objectFetch(objects) {
  const calls = [];
  const fetch = async (input, init) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    calls.push({ method, url });
    const bytes = objects.get(url);
    if (!bytes) return new Response("not found", { status: 404 });
    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: { "content-length": String(bytes.byteLength) }
      });
    }
    const range = new Headers(init?.headers).get("range");
    if (range) {
      const match = range.match(/^bytes=(\d+)-(\d+)$/);
      if (!match) return new Response("bad range", { status: 416 });
      const start = Number(match[1]);
      const end = Number(match[2]);
      const body = bytes.slice(start, end + 1);
      return new Response(body, {
        status: 206,
        headers: {
          "content-length": String(body.byteLength),
          "content-range": `bytes ${start}-${end}/${bytes.byteLength}`
        }
      });
    }
    return new Response(bytes, {
      status: 200,
      headers: { "content-length": String(bytes.byteLength) }
    });
  };
  return { calls, fetch };
}
