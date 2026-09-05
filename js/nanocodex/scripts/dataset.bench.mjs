import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { parquetWriteBuffer } from "hyparquet-writer";
import { createDatasetTool } from "../tools/datasetEngine.mjs";
const ROWS = 1e5;
const PARQUET_URL = "https://bench.example/train.parquet";
const JSONL_URL = "https://bench.example/train.jsonl";
const context = { sessionId: "dataset-benchmark" };
const ids = Array.from({ length: ROWS }, (_, index) => index);
const languages = ids.map((index) => ["rust", "python", "typescript", "go"][index % 4]);
const scores = ids.map((index) => index % 1e3);
const texts = ids.map((index) => `training example ${index} ${languages[index]}`);
const parquet = new Uint8Array(parquetWriteBuffer({
  columnData: [
    { name: "id", data: ids, type: "INT32", columnIndex: true },
    { name: "language", data: languages, type: "STRING", columnIndex: true },
    { name: "score", data: scores, type: "INT32", columnIndex: true },
    { name: "text", data: texts, type: "STRING" }
  ],
  codec: "SNAPPY",
  pageSize: 64 * 1024,
  rowGroupSize: [1e3, 25e3]
}));
const encoder = new TextEncoder();
const jsonl = encoder.encode(`${ids.map((id) => JSON.stringify({
  id,
  language: languages[id],
  score: scores[id],
  text: texts[id]
})).join("\n")}
`);
const remote = benchmarkFetch(/* @__PURE__ */ new Map([
  [PARQUET_URL, parquet],
  [JSONL_URL, jsonl]
]));
const tool = createDatasetTool({ fetch: remote.fetch, randomId: sequentialIds() });
const parquetOpen = await timed(() => tool.handler({
  operation: "open",
  source: { kind: "url", url: PARQUET_URL }
}, context));
const parquetOpened = parquetOpen.value;
const parquetId = parquetOpened.datasetId;
assert.equal(parquetOpened.format, "parquet");
assert.equal(parquetOpened.rowCount, ROWS);
const parquetOpenIo = remote.take();
const parquetQuery = {
  operation: "query",
  dataset_id: parquetId,
  columns: ["id", "text"],
  filters: [
    { column: "language", op: "eq", value: "rust" },
    { column: "score", op: "gte", value: 900 }
  ],
  offset: 500,
  limit: 100
};
const parquetCold = await timed(() => tool.handler(parquetQuery, context));
const parquetColdResult = parquetCold.value;
const parquetColdIo = remote.take();
const parquetNext = await timed(() => tool.handler({
  operation: "query",
  dataset_id: parquetId,
  cursor: parquetColdResult.nextCursor,
  limit: 100
}, context));
const parquetNextIo = remote.take();
const parquetWarm = await timed(() => tool.handler(parquetQuery, context));
const parquetWarmResult = parquetWarm.value;
const parquetWarmIo = remote.take();
assertQueryPair(parquetColdResult, parquetWarmResult, expectedRows(500, 600));
assertQueryPage(parquetNext.value, expectedRows(600, 700), 600);
assert.ok(parquetColdIo.rangeRequests > 0);
assert.ok(parquetColdIo.rangeBytes > 0);
assertZeroNetwork(parquetWarmIo);
const jsonlOpen = await timed(() => tool.handler({
  operation: "open",
  source: { kind: "url", url: JSONL_URL }
}, context));
const jsonlOpened = jsonlOpen.value;
const jsonlId = jsonlOpened.datasetId;
assert.equal(jsonlOpened.format, "jsonl");
assert.equal(jsonlOpened.rowCount, null);
const jsonlOpenIo = remote.take();
const jsonlQuery = {
  ...parquetQuery,
  dataset_id: jsonlId
};
const jsonlCold = await timed(() => tool.handler(jsonlQuery, context));
const jsonlColdResult = jsonlCold.value;
const jsonlColdIo = remote.take();
const jsonlNext = await timed(() => tool.handler({
  operation: "query",
  dataset_id: jsonlId,
  cursor: jsonlColdResult.nextCursor,
  limit: 100
}, context));
const jsonlNextIo = remote.take();
const jsonlRepeated = await timed(() => tool.handler(jsonlQuery, context));
const jsonlRepeatedResult = jsonlRepeated.value;
const jsonlRepeatedIo = remote.take();
assertQueryPair(jsonlColdResult, jsonlRepeatedResult, expectedRows(500, 600));
assertQueryPage(jsonlNext.value, expectedRows(600, 700), 600);
assert.equal(jsonlColdResult.rowsScanned, 23997);
assert.ok(jsonlColdIo.streamRequests > 0);
assert.ok(jsonlColdIo.streamBytesPulled > 0);
assert.equal(jsonlNextIo.streamRequests, 0);
assert.equal(jsonlNextIo.rangeRequests, 1);
assert.ok(jsonlNextIo.rangeBytes < jsonlColdIo.streamBytesPulled);
assertZeroNetwork(jsonlRepeatedIo);
console.log(JSON.stringify({
  corpus: {
    rows: ROWS,
    parquet: { codec: "SNAPPY", bytes: parquet.byteLength },
    jsonl: { bytes: jsonl.byteLength }
  },
  parquet: {
    open: openMeasurement(parquetOpen, parquetOpenIo),
    coldQuery: queryMeasurement(parquetCold, parquetColdIo),
    cursorContinuation: queryMeasurement(parquetNext, parquetNextIo),
    exactRepeat: queryMeasurement(parquetWarm, parquetWarmIo),
    speedupX: speedup(parquetCold.milliseconds, parquetWarm.milliseconds)
  },
  jsonl: {
    open: openMeasurement(jsonlOpen, jsonlOpenIo),
    coldQuery: queryMeasurement(jsonlCold, jsonlColdIo),
    cursorContinuation: queryMeasurement(jsonlNext, jsonlNextIo),
    exactRepeat: queryMeasurement(jsonlRepeated, jsonlRepeatedIo),
    speedupX: speedup(jsonlCold.milliseconds, jsonlRepeated.milliseconds)
  }
}, null, 2));
function expectedRows(start, end) {
  return ids.filter((id) => languages[id] === "rust" && scores[id] >= 900).slice(start, end).map((id) => ({ id, text: texts[id] }));
}
function assertQueryPair(cold, repeated, expected) {
  assert.deepEqual(cold.rows, expected);
  assert.deepEqual(repeated.rows, expected);
  assert.equal(cold.returnedRows, expected.length);
  assert.equal(repeated.returnedRows, expected.length);
  assert.equal(cold.complete, false);
  assert.equal(repeated.complete, false);
  assert.equal(cold.truncatedReason, "limit");
  assert.equal(repeated.truncatedReason, "limit");
  assert.equal(cold.cacheHit, false);
  assert.equal(repeated.cacheHit, true);
  assert.ok(cold.bytesRead > 0);
  assert.equal(repeated.bytesRead, 0);
  assert.equal(repeated.rowsScanned, cold.rowsScanned);
}
function assertQueryPage(page, expected, offset) {
  assert.deepEqual(page.rows, expected);
  assert.equal(page.returnedRows, expected.length);
  assert.equal(page.offset, offset);
  assert.equal(page.complete, false);
  assert.equal(page.truncatedReason, "limit");
  assert.ok(page.nextCursor);
}
function assertZeroNetwork(io) {
  assert.deepEqual(io, emptyIo());
}
function openMeasurement(measurement, io) {
  return { latencyMs: rounded(measurement.milliseconds), network: io };
}
function queryMeasurement(measurement, io) {
  const value = measurement.value;
  return {
    latencyMs: rounded(measurement.milliseconds),
    returnedRows: value.returnedRows,
    rowsScanned: value.rowsScanned,
    toolBytesRead: value.bytesRead,
    cacheHit: value.cacheHit,
    network: io
  };
}
function speedup(coldMilliseconds, repeatedMilliseconds) {
  return rounded(coldMilliseconds / repeatedMilliseconds);
}
async function timed(run) {
  const started = performance.now();
  const value = await run();
  return { milliseconds: performance.now() - started, value };
}
function benchmarkFetch(objects) {
  let io = emptyIo();
  const fetch = async (input, init) => {
    const data = objects.get(String(input));
    if (!data) return new Response("not found", { status: 404 });
    io.requests++;
    if ((init?.method ?? "GET") === "HEAD") {
      io.headRequests++;
      return new Response(null, {
        status: 200,
        headers: { "content-length": String(data.byteLength) }
      });
    }
    const range = new Headers(init?.headers).get("range");
    if (range) {
      const match = range.match(/^bytes=(\d+)-(\d+)$/);
      if (!match) return new Response("bad range", { status: 416 });
      const start = Number(match[1]);
      const end = Number(match[2]);
      const body = data.slice(start, end + 1);
      io.rangeRequests++;
      return new Response(chunked(body, 64 * 1024, (length) => io.rangeBytes += length), {
        status: 206,
        headers: {
          "content-length": String(body.byteLength),
          "content-range": `bytes ${start}-${end}/${data.byteLength}`
        }
      });
    }
    io.streamRequests++;
    return new Response(chunked(data, 64 * 1024, (length) => io.streamBytesPulled += length), {
      status: 200,
      headers: { "content-length": String(data.byteLength) }
    });
  };
  return {
    fetch,
    take() {
      const snapshot = io;
      io = emptyIo();
      return snapshot;
    }
  };
}
function emptyIo() {
  return {
    requests: 0,
    headRequests: 0,
    rangeRequests: 0,
    streamRequests: 0,
    rangeBytes: 0,
    streamBytesPulled: 0
  };
}
function chunked(bytes, size, onChunk) {
  let offset = 0;
  return new ReadableStream({
    pull(controller) {
      if (offset >= bytes.byteLength) {
        controller.close();
        return;
      }
      const end = Math.min(offset + size, bytes.byteLength);
      const chunk = bytes.slice(offset, end);
      onChunk(chunk.byteLength);
      controller.enqueue(chunk);
      offset = end;
    }
  });
}
function sequentialIds() {
  let id = 0;
  return () => `dataset-${++id}`;
}
function rounded(value) {
  return Math.round(value * 100) / 100;
}
