import assert from "node:assert/strict";
import { test } from "node:test";
import { parquetWriteBuffer } from "hyparquet-writer";
import { dataset } from "../tools/index.mjs";
import { createDatasetTool } from "../tools/datasetEngine.mjs";
const context = { sessionId: "session-a" };
test("the public dataset factory advertises the lazy dataset capability", () => {
  const tool = dataset();
  assert.equal(tool.name, "dataset");
  assert.ok(Object.isFrozen(tool));
  assert.match(tool.description, /Parquet, uncompressed JSONL, or Hugging Face/);
  assert.deepEqual(
    tool.parameters.properties.operation.enum,
    ["open", "query", "close"]
  );
  assert.equal(tool.parameters.properties.limit.maximum, undefined);
  assert.equal(tool.parameters.properties.offset.maximum, undefined);
  assert.equal(tool.parameters.properties.cursor.type, "string");
});

test("the default dataset fetch keeps the browser global receiver", async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = function () {
    if (this !== globalThis) throw new TypeError("Illegal invocation");
    return Promise.resolve(new Response('{"id":1}\n', { status: 200 }));
  };
  try {
    const tool = createDatasetTool({ randomId: () => "browser-fetch" });
    const opened = await tool.handler({
      operation: "open",
      source: { kind: "url", url: "https://data.example/browser.jsonl" },
    }, context);
    assert.equal(opened.datasetId, "browser-fetch");
    assert.deepEqual(opened.previewRows, [{ id: 1 }]);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("the public dataset factory releases session handles and rejects work after disposal", async () => {
  const url = "https://data.example/release.jsonl";
  const tool = dataset({
    fetch: objectFetch(new Map([[url, new TextEncoder().encode('{"id":1}\n')]])),
  });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url, format: "jsonl" },
  }, context);
  tool.releaseSession(context.sessionId);
  await assert.rejects(
    tool.handler({ operation: "query", dataset_id: opened.datasetId }, context),
    /unavailable in this agent session/,
  );
  tool.dispose();
  assert.throws(
    () => tool.handler({ operation: "close", dataset_id: opened.datasetId }, context),
    /disposed/,
  );
});
test("Parquet datasets expose metadata and bounded projected queries", async () => {
  const parquet = parquetWriteBuffer({
    columnData: [
      { name: "name", data: ["Ada", "Grace", "Linus", "Margaret"], type: "STRING" },
      { name: "score", data: [10, 20, 30, 40], type: "INT32" },
      { name: "bio", data: ["math", "compiler pioneer", "kernel", "compiler lead"], type: "STRING" }
    ],
    rowGroupSize: 2
  });
  const fetch = objectFetch(/* @__PURE__ */ new Map([
    ["https://data.example/people.parquet", new Uint8Array(parquet)]
  ]));
  const tool = createDatasetTool({ fetch, randomId: () => "dataset-1" });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/people.parquet" }
  }, context);
  assert.equal(opened.datasetId, "dataset-1");
  assert.equal(opened.format, "parquet");
  assert.equal(opened.rowCount, 4);
  assert.deepEqual(opened.schema.map((column) => column.name), ["name", "score", "bio"]);
  const filtered = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["name", "score"],
    filters: [{ column: "score", op: "gte", value: 20 }],
    limit: 2
  }, context);
  assert.deepEqual(filtered.rows, [
    { name: "Grace", score: 20 },
    { name: "Linus", score: 30 }
  ]);
  assert.equal(filtered.complete, false);
  assert.equal(filtered.truncatedReason, "limit");
  assert.ok(filtered.bytesRead > 0);
  const searched = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["name"],
    filters: [{ column: "bio", op: "contains", value: "compiler" }],
    limit: 10
  }, context);
  assert.deepEqual(searched.rows, [{ name: "Grace" }, { name: "Margaret" }]);
  assert.equal(searched.complete, true);
});
test("JSONL datasets stream, filter, project, and report partial scans", async () => {
  const records = [
    { language: "rust", text: "ownership and borrowing", score: 5 },
    { language: "python", text: "dataframes", score: 2 },
    { language: "rust", text: "async runtimes", score: 8 },
    { language: "rust", text: "unsafe internals", score: 9 }
  ];
  const bytes = new TextEncoder().encode(`${records.map(JSON.stringify).join("\n")}
`);
  const fetch = objectFetch(/* @__PURE__ */ new Map([["https://data.example/train.jsonl", bytes]]), 37);
  const tool = createDatasetTool({ fetch, randomId: () => "jsonl-1" });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/train.jsonl" }
  }, context);
  assert.deepEqual(opened.schema.map((column) => column.name), ["language", "text", "score"]);
  assert.deepEqual(opened.previewRows.slice(0, 2), records.slice(0, 2));
  const queried = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["text", "score"],
    filters: [
      { column: "language", op: "eq", value: "rust" },
      { column: "score", op: "gte", value: 8 }
    ],
    offset: 1,
    limit: 1
  }, context);
  assert.deepEqual(queried.rows, [{ text: "unsafe internals", score: 9 }]);
  assert.equal(queried.rowsScanned, 4);
  assert.equal(queried.complete, false);
  assert.equal(queried.truncatedReason, "limit");
  const longRows = Array.from({ length: 30 }, (_, index) => ({
    index,
    text: "x".repeat(90)
  }));
  const longBytes = new TextEncoder().encode(`${longRows.map(JSON.stringify).join("\n")}
`);
  const boundedTool = createDatasetTool({
    fetch: objectFetch(/* @__PURE__ */ new Map([["https://data.example/long.jsonl", longBytes]])),
    randomId: () => "jsonl-bounded"
  });
  const bounded = await boundedTool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/long.jsonl" }
  }, context);
  const partial = await boundedTool.handler({
    operation: "query",
    dataset_id: bounded.datasetId,
    limit: 100,
    max_bytes: 1024
  }, context);
  assert.ok(partial.rows.length > 0);
  assert.equal(partial.complete, false);
  assert.equal(partial.truncatedReason, "byte_limit");
  assert.equal(partial.bytesRead, 1024);
  let deepPage = await boundedTool.handler({
    operation: "query",
    dataset_id: bounded.datasetId,
    columns: ["index"],
    offset: 20,
    limit: 1,
    max_bytes: 1024
  }, context);
  for (let page = 0; deepPage.rows.length === 0 && page < 10; page++) {
    deepPage = await boundedTool.handler({
      operation: "query",
      dataset_id: bounded.datasetId,
      cursor: deepPage.nextCursor,
      limit: 1,
      max_bytes: 1024
    }, context);
  }
  assert.deepEqual(deepPage.rows, [{ index: 20 }]);
  const sparseRows = Array.from(
    { length: 40 },
    (_, index) => index === 35 ? { index, late: "discovered during query" } : { index }
  );
  const sparseBytes = new TextEncoder().encode(`${sparseRows.map(JSON.stringify).join("\n")}
`);
  const sparseTool = createDatasetTool({
    fetch: objectFetch(/* @__PURE__ */ new Map([["https://data.example/sparse.jsonl", sparseBytes]]), 64),
    randomId: () => "jsonl-sparse"
  });
  const sparse = await sparseTool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/sparse.jsonl" }
  }, context);
  assert.equal(sparse.schema.some((column) => column.name === "late"), false);
  const late = await sparseTool.handler({
    operation: "query",
    dataset_id: sparse.datasetId,
    columns: ["late"],
    offset: 35,
    limit: 1
  }, context);
  assert.deepEqual(late.rows, [{ late: "discovered during query" }]);
});
test("queries allow deep offsets and continue JSONL scans from byte cursors", async () => {
  const records = Array.from({ length: 12_050 }, (_, id) => ({ id, tag: id % 2 ? "é" : "雪" }));
  const url = "https://data.example/deep.jsonl";
  const bytes = new TextEncoder().encode(`${records.map(JSON.stringify).join("\n")}\n`);
  const baseFetch = objectFetch(new Map([[url, bytes]]), 4096);
  const ranges = [];
  const tool = createDatasetTool({
    fetch: (input, init) => {
      ranges.push(new Headers(init?.headers).get("range"));
      return baseFetch(input, init);
    },
    randomId: () => "deep-jsonl"
  });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url }
  }, context);
  const first = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["id"],
    filters: [{ column: "tag", op: "eq", value: "雪" }],
    offset: 5_500,
    limit: 2
  }, context);
  assert.deepEqual(first.rows, [{ id: 11_000 }, { id: 11_002 }]);
  assert.ok(first.nextCursor);
  const second = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    cursor: first.nextCursor,
    limit: 2
  }, context);
  assert.deepEqual(second.rows, [{ id: 11_004 }, { id: 11_006 }]);
  assert.equal(second.offset, 5_502);
  assert.equal(second.rowsScanned, 4);
  assert.match(ranges.at(-1), /^bytes=\d+-\d+$/);
  await assert.rejects(
    tool.handler({
      operation: "query",
      dataset_id: opened.datasetId,
      cursor: first.nextCursor,
      offset: 0
    }, context),
    /do not combine them with cursor/
  );
});
test("Parquet queries accept limits above 100 and continue from physical cursors", async () => {
  const ids = Array.from({ length: 12_500 }, (_, id) => id);
  const parquet = new Uint8Array(parquetWriteBuffer({
    columnData: [{ name: "id", data: ids, type: "INT32" }],
    rowGroupSize: 1000
  }));
  const url = "https://data.example/deep.parquet";
  const tool = createDatasetTool({
    fetch: objectFetch(new Map([[url, parquet]])),
    randomId: () => "deep-parquet"
  });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url }
  }, context);
  const first = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    columns: ["id"],
    offset: 11_000,
    limit: 150
  }, context);
  assert.equal(first.returnedRows, 150);
  assert.equal(first.rows[0].id, 11_000);
  assert.equal(first.rows.at(-1).id, 11_149);
  assert.ok(first.nextCursor);
  const second = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    cursor: first.nextCursor,
    limit: 150
  }, context);
  assert.equal(second.rows[0].id, 11_150);
  assert.equal(second.rows.at(-1).id, 11_299);
  assert.equal(second.offset, 11_150);
});
test("Hugging Face sources resolve the default training split to Parquet shards", async () => {
  const parquet = new Uint8Array(parquetWriteBuffer({
    columnData: [
      { name: "text", data: ["one", "two"], type: "STRING" },
      { name: "label", data: [0, 1], type: "INT32" }
    ]
  }));
  const shardUrl = "https://huggingface.co/datasets/acme/demo/resolve/main/train.parquet";
  const objects = /* @__PURE__ */ new Map([[shardUrl, parquet]]);
  const baseFetch = objectFetch(objects);
  let followedHuggingFaceRedirect = false;
  const fetch = async (input, init) => {
    const url = String(input);
    if (url.startsWith("https://datasets-server.huggingface.co/parquet?")) {
      return Response.json({
        parquet_files: [
          { config: "default", split: "test", url: shardUrl, filename: "test.parquet", size: parquet.byteLength },
          { config: "default", split: "train", url: shardUrl, filename: "train.parquet", size: parquet.byteLength }
        ],
        pending: [],
        failed: [],
        partial: true
      });
    }
    if (url.startsWith("https://datasets-server.huggingface.co/size?")) {
      return Response.json({
        size: {
          splits: [{
            config: "default",
            split: "train",
            num_rows: 2,
            num_bytes_parquet_files: parquet.byteLength
          }]
        }
      });
    }
    if (url === shardUrl) {
      assert.equal(init?.redirect, "follow");
      followedHuggingFaceRedirect = true;
    }
    return baseFetch(input, init);
  };
  const tool = createDatasetTool({ fetch, randomId: () => "hf-1" });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "huggingface", dataset: "acme/demo" }
  }, context);
  assert.deepEqual(opened.source, {
    kind: "huggingface",
    dataset: "acme/demo",
    config: "default",
    split: "train",
    partial: true
  });
  assert.equal(opened.rowCount, 2);
  assert.equal(opened.shardCount, 1);
  assert.equal(followedHuggingFaceRedirect, true);
  const queried = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    limit: 10
  }, context);
  assert.equal(queried.complete, false);
  assert.equal(queried.truncatedReason, "source_partial");
});
test("Parquet filters coerce JSON-safe INT64 values without losing precision", async () => {
  const parquet = new Uint8Array(parquetWriteBuffer({
    columnData: [{
      name: "value",
      data: [1n, 2n, 9007199254740993n],
      type: "INT64"
    }]
  }));
  const tool = createDatasetTool({
    fetch: objectFetch(/* @__PURE__ */ new Map([["https://data.example/int64.parquet", parquet]])),
    randomId: () => "int64"
  });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/int64.parquet" }
  }, context);
  const safe = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    filters: [{ column: "value", op: "eq", value: 2 }],
    limit: 10
  }, context);
  assert.deepEqual(safe.rows, [{ value: 2 }]);
  const precise = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    filters: [{ column: "value", op: "eq", value: "9007199254740993" }],
    limit: 10
  }, context);
  assert.deepEqual(precise.rows, [{ value: "9007199254740993" }]);
  const beyondDataset = await tool.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    offset: 10001
  }, context);
  assert.deepEqual(beyondDataset.rows, []);
  assert.equal(beyondDataset.complete, true);
});
test("dataset handles are session-scoped and arbitrary URLs reject local targets", async () => {
  const bytes = new TextEncoder().encode('{"value":1}\n');
  const baseFetch = objectFetch(/* @__PURE__ */ new Map([["https://data.example/one.jsonl", bytes]]));
  const redirectModes = [];
  const tool = createDatasetTool({
    fetch: async (input, init) => {
      redirectModes.push(init?.redirect);
      return baseFetch(input, init);
    },
    randomId: () => "private-1"
  });
  const opened = await tool.handler({
    operation: "open",
    source: { kind: "url", url: "https://data.example/one.jsonl" }
  }, context);
  assert.deepEqual(redirectModes, ["error"]);
  await assert.rejects(
    tool.handler({ operation: "query", dataset_id: opened.datasetId }, { sessionId: "session-b" }),
    /unavailable in this agent session/
  );
  await assert.rejects(
    tool.handler({
      operation: "open",
      source: { kind: "url", url: "https://127.0.0.1/private.jsonl" }
    }, context),
    /public hostname/
  );
  await assert.rejects(
    tool.handler({
      operation: "open",
      source: { kind: "url", url: "https://localhost./private.jsonl" }
    }, context),
    /public hostname/
  );
  assert.deepEqual(
    await tool.handler({ operation: "close", dataset_id: opened.datasetId }, context),
    { datasetId: opened.datasetId, closed: true }
  );
});
function objectFetch(objects, chunkSize) {
  return async (input, init) => {
    const url = String(input);
    const bytes = objects.get(url);
    if (!bytes) return new Response("not found", { status: 404 });
    const method = init?.method ?? "GET";
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
    return new Response(chunkSize ? chunked(bytes, chunkSize) : bytes, {
      status: 200,
      headers: { "content-length": String(bytes.byteLength) }
    });
  };
}
function chunked(bytes, size) {
  let offset = 0;
  return new ReadableStream({
    pull(controller) {
      if (offset >= bytes.byteLength) {
        controller.close();
        return;
      }
      const end = Math.min(offset + size, bytes.byteLength);
      controller.enqueue(bytes.slice(offset, end));
      offset = end;
    }
  });
}
