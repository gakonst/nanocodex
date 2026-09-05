import {
  DATASET_DESCRIPTION,
  MAX_QUERY_BYTES,
  datasetParameters
} from "./datasetContract.mjs";
const MAX_OPEN_DATASETS = 8;
const MAX_TOTAL_OPEN_DATASETS = 16;
const DEFAULT_QUERY_LIMIT = 20;
const DEFAULT_QUERY_BYTES = 32 * 1024 * 1024;
const OPEN_METADATA_BYTES = 8 * 1024 * 1024;
const JSONL_SCHEMA_BYTES = 1024 * 1024;
const JSONL_SCHEMA_ROWS = 32;
const MAX_JSONL_LINE_BYTES = 2 * 1024 * 1024;
const MAX_API_RESPONSE_BYTES = 4 * 1024 * 1024;
const MAX_TOOL_OUTPUT_BYTES = 256 * 1024;
const MAX_CELL_STRING_LENGTH = 16 * 1024;
const FETCH_TIMEOUT_MS = 2e4;
const RANGE_CACHE_BYTES = 32 * 1024 * 1024;
const MAX_CACHED_RANGE_BYTES = 4 * 1024 * 1024;
const QUERY_CACHE_BYTES = 2 * 1024 * 1024;
const MAX_CACHED_QUERIES = 16;
const MAX_BLOOM_FILTER_GROUPS = 8;
const PARQUET_SCAN_ROWS = 2048;
const CURSOR_VERSION = 1;
function createDatasetTool(options = {}) {
  const providedFetch = options.fetch;
  const fetchImpl = providedFetch === undefined
    ? globalThis.fetch.bind(globalThis)
    : (input, init) => providedFetch(input, init);
  const randomId = options.randomId ?? (() => crypto.randomUUID());
  const loadParquet = options.loadParquet ?? (() => import("hyparquet"));
  const loadCompressors = options.loadCompressors ?? (() => import("hyparquet-compressors"));
  const sessions = /* @__PURE__ */ new Map();
  const rangeCache = new RangeCache(RANGE_CACHE_BYTES, MAX_CACHED_RANGE_BYTES);
  const pendingOpens = /* @__PURE__ */ new Map();
  return {
    description: DATASET_DESCRIPTION,
    parameters: datasetParameters,
    async handler(input, context) {
      context.signal?.throwIfAborted();
      const args = requireObject(input, "dataset");
      const operation = requireString(args.operation, "dataset.operation");
      const runtime = { fetch: fetchImpl, rangeCache, signal: context.signal };
      if (operation === "open") {
        const sessionPending = pendingOpens.get(context.sessionId) ?? 0;
        if ((sessions.get(context.sessionId)?.size ?? 0) + sessionPending >= MAX_OPEN_DATASETS) {
          throw new Error(`dataset session already has ${MAX_OPEN_DATASETS} open datasets; close one first`);
        }
        if (openDatasetCount(sessions) + pendingOpenCount(pendingOpens) >= MAX_TOTAL_OPEN_DATASETS) {
          throw new Error(`dataset tool already has ${MAX_TOTAL_OPEN_DATASETS} open datasets; close one first`);
        }
        pendingOpens.set(context.sessionId, sessionPending + 1);
        let dataset;
        try {
          dataset = await openDataset({
            args,
            id: randomId(),
            loadParquet,
            runtime
          });
        } finally {
          const remaining = (pendingOpens.get(context.sessionId) ?? 1) - 1;
          if (remaining > 0) pendingOpens.set(context.sessionId, remaining);
          else pendingOpens.delete(context.sessionId);
        }
        const datasets = sessionDatasets(sessions, context.sessionId);
        if (datasets.size >= MAX_OPEN_DATASETS) {
          throw new Error(`dataset session already has ${MAX_OPEN_DATASETS} open datasets; close one first`);
        }
        if (openDatasetCount(sessions) >= MAX_TOTAL_OPEN_DATASETS) {
          throw new Error(`dataset tool already has ${MAX_TOTAL_OPEN_DATASETS} open datasets; close one first`);
        }
        datasets.set(dataset.id, dataset);
        return publicDataset(dataset);
      }
      if (operation === "query") {
        const dataset = requireDataset(sessions, context.sessionId, args.dataset_id);
        const query = parseQuery(args, dataset);
        const cacheKey = queryCacheKey(query);
        const cached = dataset.queryCache.get(cacheKey);
        if (cached) return { ...cached, bytesRead: 0, cacheHit: true };
        const budget = new ByteBudget(query.maxBytes);
        const progress = dataset.format === "parquet" ? await queryParquet(dataset, query, budget, runtime, loadParquet, loadCompressors) : await queryJsonl(dataset, query, budget, runtime);
        const result = queryResult(dataset, query, budget, progress);
        dataset.queryCache.set(cacheKey, result);
        return { ...result, cacheHit: false };
      }
      if (operation === "close") {
        const id = requireString(args.dataset_id, "dataset.dataset_id");
        const removed = sessions.get(context.sessionId)?.delete(id) ?? false;
        if (sessions.get(context.sessionId)?.size === 0) sessions.delete(context.sessionId);
        if (removed) rangeCache.deleteDataset(id);
        return { datasetId: id, closed: removed };
      }
      throw new Error("dataset.operation must be open, query, or close");
    },
    releaseSession(sessionId) {
      for (const dataset of sessions.get(sessionId)?.values() ?? []) {
        rangeCache.deleteDataset(dataset.id);
      }
      sessions.delete(sessionId);
      pendingOpens.delete(sessionId);
    },
    dispose() {
      sessions.clear();
      pendingOpens.clear();
      rangeCache.clear();
    }
  };
}
async function openDataset({
  args,
  id,
  loadParquet,
  runtime
}) {
  const source = requireObject(args.source, "dataset.source");
  const kind = requireString(source.kind, "dataset.source.kind");
  if (kind === "huggingface") {
    return openHuggingFace(source, id, runtime, loadParquet);
  }
  if (kind !== "url") throw new Error("dataset.source.kind must be url or huggingface");
  const url = publicHttpsUrl(requireString(source.url, "dataset.source.url"));
  const format = parseFormat(source.format, url);
  if (format === "parquet") {
    const parquetPromise = loadParquet();
    const byteLength = await remoteByteLength(url, runtime);
    if (byteLength == null) {
      throw new Error("Parquet URL must expose Content-Length or support a one-byte range request");
    }
    const shard = {
      url: url.href,
      byteLength,
      cacheKey: id,
      allowRedirects: false
    };
    const budget2 = new ByteBudget(OPEN_METADATA_BYTES);
    const parquet = await parquetPromise;
    shard.metadata = await parquet.parquetMetadataAsync(remoteBuffer(shard, budget2, runtime));
    const schema = parquet.parquetSchema(shard.metadata);
    return {
      id,
      format,
      source: { kind: "url", url: url.href },
      byteLength,
      rowCount: safeCount(shard.metadata.num_rows),
      schema: parquetColumns(schema),
      queryCache: new QueryResultCache(QUERY_CACHE_BYTES, MAX_CACHED_QUERIES),
      parquet: { shards: [shard], filterKinds: parquetFilterKinds(schema) }
    };
  }
  const dataset = {
    id,
    format,
    source: { kind: "url", url: url.href },
    byteLength: null,
    rowCount: null,
    schema: [],
    queryCache: new QueryResultCache(QUERY_CACHE_BYTES, MAX_CACHED_QUERIES),
    jsonl: {
      object: { url: url.href, byteLength: null, cacheKey: id, allowRedirects: false },
      preview: []
    }
  };
  const budget = new ByteBudget(JSONL_SCHEMA_BYTES);
  const inferred = await scanJsonl(dataset, {
    datasetId: id,
    filters: [],
    offset: 0,
    limit: JSONL_SCHEMA_ROWS,
    maxBytes: JSONL_SCHEMA_BYTES
  }, budget, runtime);
  dataset.schema = inferJsonlSchema(inferred.rows);
  dataset.jsonl.preview = inferred.rows.slice(0, 10);
  return dataset;
}
async function openHuggingFace(source, id, runtime, loadParquet) {
  const datasetName = requireString(source.dataset, "dataset.source.dataset");
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(datasetName) || datasetName.length > 200) {
    throw new Error("Hugging Face dataset must be owner/name");
  }
  const encoded = encodeURIComponent(datasetName);
  const parquetPromise = loadParquet();
  const [parquetResponse, sizeResponse] = await Promise.all([
    fetchJson(
      `https://datasets-server.huggingface.co/parquet?dataset=${encoded}`,
      runtime
    ),
    fetchJson(
      `https://datasets-server.huggingface.co/size?dataset=${encoded}`,
      runtime
    ).catch(() => ({}))
  ]);
  const allFiles = parquetResponse.parquet_files ?? [];
  if (!allFiles.length) {
    const pending = parquetResponse.pending?.length ?? 0;
    const failed = parquetResponse.failed?.length ?? 0;
    throw new Error(`Hugging Face has no Parquet shards for ${datasetName} (${pending} pending, ${failed} failed)`);
  }
  const requestedConfig = optionalString(source.config, "dataset.source.config");
  const configs = unique(allFiles.map((file) => file.config));
  const config = chooseDatasetPart(requestedConfig, configs, "default", "config");
  const configFiles = allFiles.filter((file) => file.config === config);
  const requestedSplit = optionalString(source.split, "dataset.source.split");
  const splits = unique(configFiles.map((file) => file.split));
  const split = chooseDatasetPart(requestedSplit, splits, "train", "split");
  const selected = configFiles.filter((file) => file.split === split);
  const shards = selected.map((file, index) => {
    const url = publicHttpsUrl(file.url);
    return {
      url: url.href,
      byteLength: boundedNonnegativeInteger(file.size, "Hugging Face shard size"),
      cacheKey: `${id}:${index}`,
      allowRedirects: huggingFaceRedirectUrl(url)
    };
  });
  const budget = new ByteBudget(OPEN_METADATA_BYTES);
  const parquet = await parquetPromise;
  shards[0].metadata = await parquet.parquetMetadataAsync(remoteBuffer(shards[0], budget, runtime));
  const schema = parquet.parquetSchema(shards[0].metadata);
  const splitSize = sizeResponse.size?.splits?.find(
    (entry) => entry.config === config && entry.split === split
  );
  const byteLength = shards.reduce((total, shard) => total + (shard.byteLength ?? 0), 0);
  return {
    id,
    format: "parquet",
    source: {
      kind: "huggingface",
      dataset: datasetName,
      config,
      split,
      partial: parquetResponse.partial === true
    },
    byteLength: splitSize?.num_bytes_parquet_files ?? byteLength,
    rowCount: splitSize?.num_rows ?? null,
    schema: parquetColumns(schema),
    queryCache: new QueryResultCache(QUERY_CACHE_BYTES, MAX_CACHED_QUERIES),
    parquet: { shards, filterKinds: parquetFilterKinds(schema) }
  };
}
async function queryParquet(dataset, query, budget, runtime, loadParquet, loadCompressors) {
  const parquet = await loadParquet();
  const shards = dataset.parquet?.shards;
  if (!shards) throw new Error("Parquet dataset is unavailable");
  const pushdownFilters = query.filters.filter((filter) => filter.op !== "contains");
  const postFilters = query.filters.filter((filter) => filter.op === "contains");
  const filtered = pushdownFilters.length > 0 || postFilters.length > 0;
  const rows = new QueryRows(query.limit);
  let position = query.position ?? { format: "parquet", shard: 0, row: 0, match: 0 };
  let skip = query.skip;
  let rowsScanned = 0;
  let compressors;
  try {
    for (let shardIndex = position.shard; shardIndex < shards.length; shardIndex++) {
      const shard = shards[shardIndex];
      const metadata = await parquetMetadata(shard, budget, runtime, parquet);
      const file = remoteBuffer(shard, budget, runtime);
      if (!compressors && needsCustomCompressors(metadata, query)) {
        compressors = (await loadCompressors()).compressors;
      }
      const useBloomFilters = pushdownFilters.length > 0 && metadata.row_groups.length <= MAX_BLOOM_FILTER_GROUPS;
      const shardRows = safeCount(metadata.num_rows);
      let rowStart = shardIndex === position.shard ? position.row : 0;
      let matchStart = shardIndex === position.shard ? position.match : 0;
      if (!filtered && skip > 0) {
        const available = shardRows - rowStart;
        if (skip >= available) {
          skip -= available;
          position = { format: "parquet", shard: shardIndex + 1, row: 0, match: 0 };
          continue;
        }
        rowStart += skip;
        skip = 0;
      }
      while (rowStart < shardRows) {
        const rowEnd = Math.min(shardRows, rowStart + PARQUET_SCAN_ROWS);
        position = { format: "parquet", shard: shardIndex, row: rowStart, match: matchStart };
        const found = await parquet.parquetReadObjects({
          file,
          metadata,
          columns: postFilters.length ? columnsForPostFilter(query.columns, postFilters) : query.columns,
          filter: pushdownFilters.length ? parquetFilter(pushdownFilters) : void 0,
          rowStart,
          rowEnd,
          compressors,
          useBloomFilters,
          usePageIndex: pushdownFilters.length > 0,
          useOffsetIndex: true
        });
        rowsScanned += rowEnd - rowStart;
        const candidates = postFilters.length
          ? found.filter((candidate) => matchesFilters(candidate, postFilters)).map((candidate) => projectRow(candidate, query.columns))
          : found;
        for (let match = matchStart; match < candidates.length; match++) {
          const nextPosition = match + 1 < candidates.length
            ? { format: "parquet", shard: shardIndex, row: rowStart, match: match + 1 }
            : { format: "parquet", shard: shardIndex, row: rowEnd, match: 0 };
          if (skip > 0) {
            skip--;
            position = nextPosition;
            continue;
          }
          const status = rows.add(candidates[match]);
          if (status === "output_limit") {
            return partialQuery(rows, rowsScanned, "output_limit", position, skip);
          }
          position = nextPosition;
          if (status === "limit") {
            return partialQuery(rows, rowsScanned, "limit", position, skip);
          }
        }
        rowStart = rowEnd;
        matchStart = 0;
        position = { format: "parquet", shard: shardIndex, row: rowStart, match: 0 };
      }
      position = { format: "parquet", shard: shardIndex + 1, row: 0, match: 0 };
    }
  } catch (error) {
    if (!(error instanceof ByteLimitError)) throw error;
    return partialQuery(rows, rowsScanned, "byte_limit", position, skip);
  }
  if (dataset.source.kind === "huggingface" && dataset.source.partial) {
    return { rows: rows.rows, rowsScanned, complete: false, truncatedReason: "source_partial" };
  }
  return { rows: rows.rows, rowsScanned, complete: true };
}
async function queryJsonl(dataset, query, budget, runtime) {
  return scanJsonl(dataset, query, budget, runtime);
}
function needsCustomCompressors(metadata, query) {
  const selected = query.columns ? /* @__PURE__ */ new Set([...query.columns, ...query.filters.map((filter) => filter.path[0])]) : void 0;
  return metadata.row_groups.some((group) => group.columns.some((column) => {
    const meta = column.meta_data;
    const name = meta?.path_in_schema[0];
    return name && (!selected || selected.has(name)) && meta.codec !== "UNCOMPRESSED" && meta.codec !== "SNAPPY";
  }));
}
async function scanJsonl(dataset, query, budget, runtime) {
  const object = dataset.jsonl?.object;
  if (!object) throw new Error("JSONL dataset is unavailable");
  const rows = new QueryRows(query.limit);
  const startByte = query.position?.byte ?? 0;
  if (object.byteLength != null && startByte >= object.byteLength) {
    return { rows: rows.rows, rowsScanned: 0, complete: true };
  }
  const headers = startByte > 0
    ? { range: `bytes=${startByte}-${object.byteLength == null ? "" : object.byteLength - 1}` }
    : void 0;
  const response = await safeFetch(object.url, runtime, { method: "GET", headers }, object.allowRedirects);
  if (!response.ok || !response.body) {
    await response.body?.cancel().catch(() => void 0);
    throw new Error(`JSONL fetch failed with HTTP ${response.status}`);
  }
  if (startByte > 0 && response.status !== 206) {
    await response.body.cancel().catch(() => void 0);
    throw new Error("JSONL server does not support byte-range continuation");
  }
  if (response.status === 206) {
    const range = parseContentRange(response.headers.get("content-range"));
    if (!range || range.start !== startByte) {
      await response.body.cancel().catch(() => void 0);
      throw new Error("JSONL server returned an invalid Content-Range");
    }
    if (range.total != null) {
      object.byteLength = range.total;
      dataset.byteLength = range.total;
    }
  } else if (object.byteLength == null) {
    object.byteLength = contentLength(response.headers.get("content-length"));
    dataset.byteLength = object.byteLength;
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  const encoder = new TextEncoder();
  let buffer = "";
  let networkOffset = startByte;
  let position = { format: "jsonl", byte: startByte };
  let rowsScanned = 0;
  let skip = query.skip;
  const processLine = (line, newlineBytes) => {
    const lineStart = position.byte;
    const lineEnd = lineStart + encoder.encode(line).byteLength + newlineBytes;
    position = { format: "jsonl", byte: lineEnd };
    const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line;
    if (!trimmed) return "accepted";
    if (trimmed.length > MAX_JSONL_LINE_BYTES) {
      throw new Error(`JSONL row exceeds ${MAX_JSONL_LINE_BYTES} characters`);
    }
    let value;
    try {
      value = JSON.parse(trimmed);
    } catch (error) {
      throw new Error(`JSONL row ${rowsScanned + 1} is invalid: ${errorMessage(error)}`);
    }
    rowsScanned++;
    if (!isRecord(value) || !matchesFilters(value, query.filters)) return "accepted";
    if (skip > 0) {
      skip--;
      return "accepted";
    }
    const status = rows.add(projectRow(value, query.columns));
    if (status === "output_limit") {
      position = { format: "jsonl", byte: lineStart };
    }
    return status;
  };
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      const acceptedBytes = Math.min(chunk.value.byteLength, budget.remaining);
      if (acceptedBytes === 0) throw new ByteLimitError("dataset byte limit reached");
      budget.consume(acceptedBytes);
      buffer += decoder.decode(chunk.value.subarray(0, acceptedBytes), { stream: true });
      networkOffset += acceptedBytes;
      if (buffer.length > MAX_JSONL_LINE_BYTES && !buffer.includes("\n")) {
        throw new Error(`JSONL row exceeds ${MAX_JSONL_LINE_BYTES} characters`);
      }
      for (let newline = buffer.indexOf("\n"); newline >= 0; newline = buffer.indexOf("\n")) {
        const status = processLine(buffer.slice(0, newline), 1);
        buffer = buffer.slice(newline + 1);
        if (status === "limit" || status === "output_limit") {
          return partialQuery(rows, rowsScanned, status, position, skip);
        }
      }
      if (acceptedBytes < chunk.value.byteLength || budget.remaining === 0 && (object.byteLength == null || networkOffset < object.byteLength)) {
        return partialQuery(rows, rowsScanned, "byte_limit", position, skip);
      }
    }
    buffer += decoder.decode();
    if (buffer) {
      const status = processLine(buffer, 0);
      if (status === "limit" || status === "output_limit") {
        return partialQuery(rows, rowsScanned, status, position, skip);
      }
    }
  } catch (error) {
    if (!(error instanceof ByteLimitError)) throw error;
    return partialQuery(rows, rowsScanned, "byte_limit", position, skip);
  } finally {
    await reader.cancel().catch(() => void 0);
  }
  return { rows: rows.rows, rowsScanned, complete: true };
}
async function parquetMetadata(shard, budget, runtime, parquet) {
  shard.metadata ??= await parquet.parquetMetadataAsync(remoteBuffer(shard, budget, runtime));
  return shard.metadata;
}
function remoteBuffer(object, budget, runtime) {
  if (object.byteLength == null) throw new Error("remote object size is unknown");
  return {
    byteLength: object.byteLength,
    async slice(start, end = object.byteLength) {
      const normalizedStart = Math.max(0, start);
      const normalizedEnd = Math.min(object.byteLength, end);
      if (!Number.isSafeInteger(normalizedStart) || !Number.isSafeInteger(normalizedEnd) || normalizedStart > normalizedEnd) {
        throw new Error(`invalid dataset byte range ${start}-${end}`);
      }
      const length = normalizedEnd - normalizedStart;
      budget.consume(length);
      if (length === 0) return new ArrayBuffer(0);
      const key = `${object.cacheKey}
${normalizedStart}-${normalizedEnd}`;
      return runtime.rangeCache.read(key, length, async () => {
        const response = await safeFetch(object.url, runtime, {
          method: "GET",
          headers: { range: `bytes=${normalizedStart}-${normalizedEnd - 1}` }
        }, object.allowRedirects);
        if (response.status !== 206) {
          await response.body?.cancel().catch(() => void 0);
          throw new Error(`dataset server ignored byte range and returned HTTP ${response.status}`);
        }
        const contentRange = response.headers.get("content-range");
        if (contentRange !== `bytes ${normalizedStart}-${normalizedEnd - 1}/${object.byteLength}`) {
          await response.body?.cancel().catch(() => void 0);
          throw new Error("dataset server returned an invalid Content-Range");
        }
        return readExactResponse(response, length);
      });
    }
  };
}
async function remoteByteLength(url, runtime) {
  const head = await safeFetch(url.href, runtime, { method: "HEAD" }).catch(() => void 0);
  if (head?.ok) {
    const length = contentLength(head.headers.get("content-length"));
    if (length != null) return length;
  }
  const response = await safeFetch(url.href, runtime, {
    method: "GET",
    headers: { range: "bytes=0-0" }
  });
  const match = response.status === 206 ? response.headers.get("content-range")?.match(/^bytes 0-0\/(\d+)$/) : void 0;
  await response.body?.cancel().catch(() => void 0);
  return match ? contentLength(match[1]) : null;
}
async function fetchJson(url, runtime) {
  const response = await safeFetch(url, runtime, { method: "GET" });
  if (!response.ok) {
    await response.body?.cancel().catch(() => void 0);
    throw new Error(`dataset metadata request failed with HTTP ${response.status}`);
  }
  const declared = contentLength(response.headers.get("content-length"));
  if (declared != null && declared > MAX_API_RESPONSE_BYTES) {
    throw new Error("dataset metadata response is too large");
  }
  const body = await readBoundedText(response, MAX_API_RESPONSE_BYTES);
  try {
    return JSON.parse(body);
  } catch (error) {
    throw new Error(`dataset metadata response is invalid JSON: ${errorMessage(error)}`);
  }
}
async function readExactResponse(response, expectedBytes) {
  const declared = contentLength(response.headers.get("content-length"));
  if (declared != null && declared !== expectedBytes) {
    await response.body?.cancel().catch(() => void 0);
    throw new Error(`dataset byte range declared ${declared} bytes; expected ${expectedBytes}`);
  }
  if (!response.body) throw new Error("dataset byte range has no response body");
  const reader = response.body.getReader();
  const output = new Uint8Array(expectedBytes);
  let offset = 0;
  let finished = false;
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      if (offset + chunk.value.byteLength > expectedBytes) {
        throw new Error(`dataset byte range exceeds expected ${expectedBytes} bytes`);
      }
      output.set(chunk.value, offset);
      offset += chunk.value.byteLength;
    }
    finished = offset === expectedBytes;
  } finally {
    if (!finished) await reader.cancel().catch(() => void 0);
  }
  if (offset !== expectedBytes) {
    throw new Error(`dataset byte range returned ${offset} bytes; expected ${expectedBytes}`);
  }
  return output.buffer;
}
async function readBoundedText(response, maxBytes) {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let bytes = 0;
  let text = "";
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > maxBytes) throw new Error("dataset metadata response is too large");
      text += decoder.decode(chunk.value, { stream: true });
    }
    return text + decoder.decode();
  } finally {
    await reader.cancel().catch(() => void 0);
  }
}
async function safeFetch(input, runtime, init, allowRedirects = false) {
  publicHttpsUrl(input);
  const timeout = AbortSignal.timeout(FETCH_TIMEOUT_MS);
  const signal = runtime.signal ? AbortSignal.any([runtime.signal, timeout]) : timeout;
  const response = await runtime.fetch(input, {
    ...init,
    credentials: "omit",
    mode: "cors",
    redirect: allowRedirects ? "follow" : "error",
    referrerPolicy: "no-referrer",
    signal
  });
  if (response.url) publicHttpsUrl(response.url);
  return response;
}
function parseQuery(args, dataset) {
  const datasetId = requireString(args.dataset_id, "dataset.dataset_id");
  const cursor = args.cursor === void 0 ? void 0 : requireString(args.cursor, "dataset.cursor");
  let source = args;
  let position;
  let offset;
  let skip;
  if (cursor !== void 0) {
    if (args.columns !== void 0 || args.filters !== void 0 || args.offset !== void 0) {
      throw new Error("dataset.cursor retains columns, filters, and position; do not combine them with cursor");
    }
    const state = decodeQueryCursor(cursor);
    if (state.v !== CURSOR_VERSION || state.d !== dataset.id || !isRecord(state.q)) {
      throw new Error("dataset.cursor is unavailable for this dataset handle");
    }
    source = state.q;
    position = parseCursorPosition(state.p, dataset.format);
    offset = nonnegativeSafeInteger(state.o, "dataset.cursor offset");
    skip = nonnegativeSafeInteger(state.s, "dataset.cursor remaining offset");
  } else {
    offset = optionalIntegerAtLeast(args.offset, 0, 0, "dataset.offset");
    skip = offset;
  }
  const columns = optionalStringArray(source.columns, "dataset.columns");
  if (columns && dataset.format === "parquet") {
    const known = new Set(dataset.schema.map((column) => column.name));
    const unknown = columns.filter((column) => !known.has(column));
    if (unknown.length) throw new Error(`dataset columns not found: ${unknown.join(", ")}`);
  }
  let filters = parseFilters(source.filters);
  if (dataset.format === "parquet") {
    const known = new Set(dataset.schema.map((column) => column.name));
    const unknown = filters.map((filter) => filter.column.split(".")[0]).filter((column) => !known.has(column));
    if (unknown.length) throw new Error(`dataset filter columns not found: ${unique(unknown).join(", ")}`);
    filters = coerceParquetFilters(filters, dataset.parquet?.filterKinds ?? /* @__PURE__ */ new Map());
  }
  const limit = optionalIntegerAtLeast(args.limit, 1, DEFAULT_QUERY_LIMIT, "dataset.limit");
  const maxBytes = optionalBoundedInteger(
    args.max_bytes,
    1024,
    MAX_QUERY_BYTES,
    DEFAULT_QUERY_BYTES,
    "dataset.max_bytes"
  );
  return {
    datasetId,
    columns,
    filters,
    offset,
    limit,
    maxBytes,
    position,
    skip,
    cursorQuery: {
      ...(columns === void 0 ? {} : { columns }),
      ...(source.filters === void 0 ? {} : { filters: source.filters })
    }
  };
}
function coerceParquetFilters(filters, kinds) {
  return filters.map((filter) => {
    const kind = kinds.get(filter.column);
    if (!kind) return filter;
    if (filter.op === "contains") {
      throw new Error(`dataset filter ${filter.column} does not support contains`);
    }
    const value = filter.op === "in" ? filter.value.map((entry) => coerceParquetFilterValue(entry, kind, filter.column)) : coerceParquetFilterValue(filter.value, kind, filter.column);
    return { ...filter, value };
  });
}
function coerceParquetFilterValue(value, kind, column) {
  if (value === null) return null;
  if (kind === "bigint") {
    if (typeof value === "bigint") return value;
    if (typeof value === "number" && Number.isSafeInteger(value)) return BigInt(value);
    if (typeof value === "string" && /^-?\d+$/.test(value)) return BigInt(value);
    throw new Error(`dataset filter ${column} requires a safe integer or integer string`);
  }
  if (value instanceof Date && Number.isFinite(value.getTime())) return value;
  if (typeof value === "string" || typeof value === "number") {
    const date = new Date(value);
    if (Number.isFinite(date.getTime())) return date;
  }
  throw new Error(`dataset filter ${column} requires an ISO date or timestamp`);
}
function parseFilters(value) {
  if (value === void 0) return [];
  if (!Array.isArray(value)) throw new Error("dataset.filters must be an array");
  if (value.length > 8) throw new Error("dataset.filters supports at most 8 filters");
  return value.map((entry, index) => {
    const filter = requireObject(entry, `dataset.filters[${index}]`);
    const column = requireString(filter.column, `dataset.filters[${index}].column`);
    const op = requireString(filter.op, `dataset.filters[${index}].op`);
    if (!["eq", "ne", "gt", "gte", "lt", "lte", "in", "contains"].includes(op)) {
      throw new Error(`dataset.filters[${index}].op is unsupported`);
    }
    if (op === "in" && (!Array.isArray(filter.value) || filter.value.length > 100)) {
      throw new Error(`dataset.filters[${index}].value must be an array with at most 100 values`);
    }
    if (op === "contains" && typeof filter.value !== "string") {
      throw new Error(`dataset.filters[${index}].value must be a string for contains`);
    }
    return { column, path: column.split("."), op, value: filter.value };
  });
}
function parquetFilter(filters) {
  if (!filters.length) return void 0;
  const clauses = filters.map((filter) => ({
    [filter.column]: { [`$${filter.op}`]: filter.value }
  }));
  return clauses.length === 1 ? clauses[0] : { $and: clauses };
}
function matchesFilters(row, filters) {
  return filters.every((filter) => {
    const actual = pathPartsValue(row, filter.path);
    switch (filter.op) {
      case "eq":
        return actual === filter.value;
      case "ne":
        return actual !== filter.value;
      case "gt":
        return comparable(actual, filter.value) > 0;
      case "gte":
        return comparable(actual, filter.value) >= 0;
      case "lt":
        return comparable(actual, filter.value) < 0;
      case "lte":
        return comparable(actual, filter.value) <= 0;
      case "in":
        return filter.value.some((value) => value === actual);
      case "contains":
        return typeof actual === "string" && actual.includes(filter.value);
    }
  });
}
function comparable(left, right) {
  if (typeof left === "number" && typeof right === "number") return left - right;
  if (typeof left === "string" && typeof right === "string") return left.localeCompare(right);
  return Number.NaN;
}
function pathValue(row, path) {
  return pathPartsValue(row, path.split("."));
}
function pathPartsValue(row, path) {
  let value = row;
  for (const part of path) {
    if (!isRecord(value)) return void 0;
    value = value[part];
  }
  return value;
}
function projectRow(row, columns) {
  if (!columns) return row;
  return Object.fromEntries(columns.map((column) => [column, pathValue(row, column)]));
}
function columnsForPostFilter(columns, filters) {
  if (!columns) return void 0;
  return unique([...columns, ...filters.map((filter) => filter.column.split(".")[0])]);
}
function parquetColumns(schema) {
  return schema.children.map((column) => ({
    name: column.element.name,
    type: column.element.logical_type?.type ?? column.element.converted_type ?? column.element.type ?? (column.children.length ? "STRUCT" : "UNKNOWN"),
    nullable: column.element.repetition_type !== "REQUIRED"
  }));
}
function parquetFilterKinds(schema) {
  const kinds = /* @__PURE__ */ new Map();
  const visit = (node, prefix) => {
    for (const child of node.children) {
      const path = [...prefix, child.element.name];
      if (child.children.length) {
        visit(child, path);
        continue;
      }
      const element = child.element;
      const logical = element.logical_type?.type;
      const converted = element.converted_type;
      if (logical === "DATE" || logical === "TIMESTAMP" || converted === "DATE" || converted === "TIMESTAMP_MILLIS" || converted === "TIMESTAMP_MICROS" || element.type === "INT96") {
        kinds.set(path.join("."), "date");
      } else if (element.type === "INT64" && logical !== "DECIMAL" && converted !== "DECIMAL") {
        kinds.set(path.join("."), "bigint");
      }
    }
  };
  visit(schema, []);
  return kinds;
}
function inferJsonlSchema(rows) {
  const types = /* @__PURE__ */ new Map();
  for (const row of rows) {
    for (const [key, value] of Object.entries(row)) {
      let seen = types.get(key);
      if (!seen) {
        seen = /* @__PURE__ */ new Set();
        types.set(key, seen);
      }
      seen.add(valueType(value));
    }
  }
  return [...types].map(([name, seen]) => ({ name, type: [...seen].sort().join(" | ") }));
}
function valueType(value) {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value === "object" ? "object" : typeof value;
}
function queryResult(dataset, query, budget, progress) {
  const nextCursor = progress.nextPosition === void 0 ? void 0 : encodeQueryCursor({
    v: CURSOR_VERSION,
    d: dataset.id,
    q: query.cursorQuery,
    p: progress.nextPosition,
    o: safeIntegerSum(query.offset, progress.rows.length, "dataset cursor offset"),
    s: progress.nextSkip
  });
  return {
    datasetId: dataset.id,
    format: dataset.format,
    rows: progress.rows,
    returnedRows: progress.rows.length,
    rowsScanned: progress.rowsScanned,
    bytesRead: budget.used,
    complete: progress.complete,
    truncatedReason: progress.truncatedReason,
    offset: query.offset,
    limit: query.limit,
    ...(nextCursor === void 0 ? {} : { nextCursor })
  };
}
function queryCacheKey(query) {
  return JSON.stringify({
    cursorQuery: query.cursorQuery,
    position: query.position,
    offset: query.offset,
    skip: query.skip,
    limit: query.limit,
    maxBytes: query.maxBytes
  }, (_key, value) => typeof value === "bigint" ? { $bigint: value.toString() } : value);
}
function partialQuery(rows, rowsScanned, truncatedReason, nextPosition, nextSkip) {
  return {
    rows: rows.rows,
    rowsScanned,
    complete: false,
    truncatedReason,
    nextPosition,
    nextSkip
  };
}
function encodeQueryCursor(state) {
  return encodeURIComponent(JSON.stringify(state));
}
function decodeQueryCursor(cursor) {
  try {
    const state = JSON.parse(decodeURIComponent(cursor));
    if (!isRecord(state)) throw new Error("cursor state must be an object");
    return state;
  } catch {
    throw new Error("dataset.cursor is invalid");
  }
}
function parseCursorPosition(value, format) {
  if (!isRecord(value) || value.format !== format) {
    throw new Error("dataset.cursor is unavailable for this dataset format");
  }
  if (format === "parquet") {
    return {
      format,
      shard: nonnegativeSafeInteger(value.shard, "dataset.cursor shard"),
      row: nonnegativeSafeInteger(value.row, "dataset.cursor row"),
      match: nonnegativeSafeInteger(value.match, "dataset.cursor match")
    };
  }
  return {
    format,
    byte: nonnegativeSafeInteger(value.byte, "dataset.cursor byte")
  };
}
function parseContentRange(value) {
  const match = value?.match(/^bytes (\d+)-(\d+)\/(\d+|\*)$/);
  if (!match) return null;
  const start = contentLength(match[1]);
  const end = contentLength(match[2]);
  const total = match[3] === "*" ? null : contentLength(match[3]);
  if (start == null || end == null || end < start || total != null && end >= total) return null;
  return { start, end, total };
}
function normalizeValue(value, seen = /* @__PURE__ */ new WeakSet()) {
  if (value === void 0) return null;
  if (typeof value === "bigint") {
    const number = Number(value);
    return Number.isSafeInteger(number) ? number : value.toString();
  }
  if (typeof value === "string") {
    return value.length <= MAX_CELL_STRING_LENGTH ? value : `${value.slice(0, MAX_CELL_STRING_LENGTH)}\u2026[truncated ${value.length - MAX_CELL_STRING_LENGTH} chars]`;
  }
  if (value instanceof Date) return value.toISOString();
  if (value instanceof Uint8Array) {
    return { binaryBytes: value.byteLength, previewHex: hex(value.subarray(0, 32)) };
  }
  if (Array.isArray(value)) return value.map((entry) => normalizeValue(entry, seen));
  if (isRecord(value)) {
    if (seen.has(value)) return "[circular]";
    seen.add(value);
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, normalizeValue(entry, seen)]));
  }
  return value;
}
function publicDataset(dataset) {
  return {
    datasetId: dataset.id,
    format: dataset.format,
    source: dataset.source,
    byteLength: dataset.byteLength,
    rowCount: dataset.rowCount,
    schema: dataset.schema,
    shardCount: dataset.parquet?.shards.length ?? 1,
    previewRows: dataset.jsonl?.preview.map((row) => normalizeValue(row))
  };
}
function requireDataset(sessions, sessionId, value) {
  const id = requireString(value, "dataset.dataset_id");
  const dataset = sessions.get(sessionId)?.get(id);
  if (!dataset) throw new Error("dataset handle is unavailable in this agent session");
  return dataset;
}
function sessionDatasets(sessions, sessionId) {
  let datasets = sessions.get(sessionId);
  if (!datasets) {
    datasets = /* @__PURE__ */ new Map();
    sessions.set(sessionId, datasets);
  }
  return datasets;
}
function openDatasetCount(sessions) {
  let count = 0;
  for (const datasets of sessions.values()) count += datasets.size;
  return count;
}
function pendingOpenCount(pending) {
  let count = 0;
  for (const value of pending.values()) count += value;
  return count;
}
function parseFormat(value, url) {
  if (value !== void 0) {
    if (value === "parquet" || value === "jsonl") return value;
    throw new Error("dataset.source.format must be parquet or jsonl");
  }
  const path = url.pathname.toLowerCase();
  if (path.endsWith(".parquet")) return "parquet";
  if (path.endsWith(".jsonl") || path.endsWith(".ndjson")) return "jsonl";
  if (path.endsWith(".jsonl.gz") || path.endsWith(".ndjson.gz") || path.endsWith(".jsonl.zst")) {
    throw new Error("compressed JSONL requires a converted Parquet source or an uncompressed streaming URL");
  }
  throw new Error("dataset.source.format is required when the URL extension is not .parquet, .jsonl, or .ndjson");
}
function publicHttpsUrl(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("dataset URL is invalid");
  }
  if (url.protocol !== "https:" || url.username || url.password) {
    throw new Error("dataset URL must be credential-free HTTPS");
  }
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  if (unsafeHostname(hostname)) throw new Error("dataset URL must use a public hostname");
  return url;
}
function huggingFaceRedirectUrl(url) {
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  return hostname === "huggingface.co" || hostname.endsWith(".huggingface.co");
}
function unsafeHostname(hostname) {
  if (hostname === "localhost" || hostname.endsWith(".localhost") || hostname.endsWith(".local") || hostname.endsWith(".internal") || hostname.includes(":")) return true;
  const parts = hostname.split(".");
  if (parts.length !== 4 || parts.some((part) => !/^\d+$/.test(part))) return false;
  const octets = parts.map(Number);
  if (octets.some((part) => part < 0 || part > 255)) return true;
  const [a, b] = octets;
  return a === 0 || a === 10 || a === 127 || a >= 224 || a === 100 && b >= 64 && b <= 127 || a === 169 && b === 254 || a === 172 && b >= 16 && b <= 31 || a === 192 && b === 168;
}
function chooseDatasetPart(requested, available, preferred, label) {
  if (requested) {
    if (!available.includes(requested)) {
      throw new Error(`Hugging Face ${label} ${requested} was not found; available: ${available.join(", ")}`);
    }
    return requested;
  }
  if (available.includes(preferred)) return preferred;
  if (available.length === 1) return available[0];
  throw new Error(`Hugging Face ${label} is required; available: ${available.join(", ")}`);
}
function requireObject(value, name) {
  if (!isRecord(value)) throw new Error(`${name} requires an object`);
  return value;
}
function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function requireString(value, name) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${name} must be a non-empty string`);
  return value;
}
function optionalString(value, name) {
  return value === void 0 ? void 0 : requireString(value, name);
}
function optionalStringArray(value, name) {
  if (value === void 0) return void 0;
  if (!Array.isArray(value) || value.length > 64 || value.some((entry) => typeof entry !== "string" || !entry)) {
    throw new Error(`${name} must be an array of at most 64 non-empty strings`);
  }
  return unique(value);
}
function optionalBoundedInteger(value, minimum, maximum, fallback, name) {
  if (value === void 0) return fallback;
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return value;
}
function optionalIntegerAtLeast(value, minimum, fallback, name) {
  if (value === void 0) return fallback;
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`${name} must be a safe integer greater than or equal to ${minimum}`);
  }
  return value;
}
function nonnegativeSafeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a nonnegative safe integer`);
  }
  return value;
}
function safeIntegerSum(left, right, name) {
  const sum = left + right;
  if (!Number.isSafeInteger(sum)) throw new Error(`${name} exceeds JavaScript safe integers`);
  return sum;
}
function boundedNonnegativeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${name} is invalid`);
  return value;
}
function contentLength(value) {
  if (value == null || !/^\d+$/.test(value)) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : null;
}
function safeCount(value) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error("dataset row count exceeds JavaScript safe integers");
  return number;
}
function unique(values) {
  return [...new Set(values)];
}
function hex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
class ByteLimitError extends Error {
}
class ByteBudget {
  used = 0;
  limit;
  constructor(limit) {
    this.limit = limit;
  }
  get remaining() {
    return this.limit - this.used;
  }
  consume(bytes) {
    if (!Number.isSafeInteger(bytes) || bytes < 0) throw new Error("dataset byte count is invalid");
    if (this.used + bytes > this.limit) throw new ByteLimitError("dataset byte limit reached");
    this.used += bytes;
  }
}
class QueryRows {
  rows = [];
  bytes = 0;
  limit;
  encoder = new TextEncoder();
  constructor(limit) {
    this.limit = limit;
  }
  add(value) {
    if (this.rows.length >= this.limit) return "limit";
    const normalized = normalizeValue(value);
    const bytes = this.encoder.encode(JSON.stringify(normalized)).byteLength;
    if (bytes > MAX_TOOL_OUTPUT_BYTES && this.rows.length === 0) {
      throw new Error("dataset row exceeds the tool output budget; select fewer columns");
    }
    if (this.bytes + bytes > MAX_TOOL_OUTPUT_BYTES) return "output_limit";
    this.rows.push(normalized);
    this.bytes += bytes;
    return this.rows.length >= this.limit ? "limit" : "accepted";
  }
}
class RangeCache {
  entries = /* @__PURE__ */ new Map();
  maxBytes;
  maxEntryBytes;
  bytes = 0;
  constructor(maxBytes, maxEntryBytes) {
    this.maxBytes = maxBytes;
    this.maxEntryBytes = maxEntryBytes;
  }
  async read(key, length, load) {
    const cached = this.entries.get(key);
    if (cached) {
      this.entries.delete(key);
      this.entries.set(key, cached);
      return cached;
    }
    const buffer = await load();
    if (length <= this.maxEntryBytes) this.insert(key, buffer);
    return buffer;
  }
  insert(key, buffer) {
    const existing = this.entries.get(key);
    if (existing) {
      this.entries.delete(key);
      this.bytes -= existing.byteLength;
    }
    while (this.bytes + buffer.byteLength > this.maxBytes && this.entries.size) {
      const oldest = this.entries.keys().next().value;
      const removed = this.entries.get(oldest);
      this.entries.delete(oldest);
      this.bytes -= removed.byteLength;
    }
    if (buffer.byteLength > this.maxBytes) return;
    this.entries.set(key, buffer);
    this.bytes += buffer.byteLength;
  }
  deleteDataset(datasetId) {
    for (const [key, buffer] of this.entries) {
      if (!key.startsWith(`${datasetId}\n`) && !key.startsWith(`${datasetId}:`)) continue;
      this.entries.delete(key);
      this.bytes -= buffer.byteLength;
    }
  }
  clear() {
    this.entries.clear();
    this.bytes = 0;
  }
}
class QueryResultCache {
  entries = /* @__PURE__ */ new Map();
  maxBytes;
  maxEntries;
  bytes = 0;
  constructor(maxBytes, maxEntries) {
    this.maxBytes = maxBytes;
    this.maxEntries = maxEntries;
  }
  get(key) {
    const entry = this.entries.get(key);
    if (!entry) return void 0;
    this.entries.delete(key);
    this.entries.set(key, entry);
    return structuredClone(entry.result);
  }
  set(key, result) {
    const copy = structuredClone(result);
    const bytes = new TextEncoder().encode(JSON.stringify(copy)).byteLength;
    if (bytes > this.maxBytes) return;
    const existing = this.entries.get(key);
    if (existing) {
      this.entries.delete(key);
      this.bytes -= existing.bytes;
    }
    while ((this.bytes + bytes > this.maxBytes || this.entries.size >= this.maxEntries) && this.entries.size) {
      const oldest = this.entries.keys().next().value;
      const removed = this.entries.get(oldest);
      this.entries.delete(oldest);
      this.bytes -= removed.bytes;
    }
    this.entries.set(key, { bytes, result: copy });
    this.bytes += bytes;
  }
}
export {
  createDatasetTool
};
