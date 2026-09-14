export const DEFAULT_USER_DATA_PAGE_SIZE = 100;
export const MAX_USER_DATA_PAGE_SIZE = 1_000;
export const MAX_USER_DATA_POINTS_PER_WRITE = 5_000;

const DATA_KEY = /^[A-Za-z0-9](?:[A-Za-z0-9._:/-]{0,510}[A-Za-z0-9])?$/u;
const SERIES = /^[A-Za-z0-9](?:[A-Za-z0-9._:/-]{0,254}[A-Za-z0-9])?$/u;
const CONTENT_TYPE = /^[\x21-\x7e]{1,255}$/u;
const SHA256 = /^[a-f0-9]{64}$/u;

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | readonly JsonValue[] | { readonly [key: string]: JsonValue };

export type UserDataDocumentPut = Readonly<{
  operation: "document_put";
  key: string;
  value: JsonValue;
  if_version?: number;
}>;

export type UserDataDocumentGet = Readonly<{
  operation: "document_get";
  key: string;
}>;

export type UserDataDocumentDelete = Readonly<{
  operation: "document_delete";
  key: string;
  if_version?: number;
}>;

export type UserDataDocumentList = Readonly<{
  operation: "document_list";
  prefix?: string;
  cursor?: string;
  limit: number;
}>;

export type UserDataPoint = Readonly<{
  timestamp_ms: number;
  value: number;
  fields?: Readonly<Record<string, JsonValue>>;
}>;

export type UserDataTimeseriesWrite = Readonly<{
  operation: "timeseries_write";
  series: string;
  points: readonly UserDataPoint[];
  conflict: "error" | "replace";
}>;

export type UserDataTimeseriesList = Readonly<{
  operation: "timeseries_list";
  prefix?: string;
  cursor?: string;
  limit: number;
}>;

export type UserDataTimeseriesQuery = Readonly<{
  operation: "timeseries_query";
  series: string;
  start_ms?: number;
  end_ms?: number;
  order: "asc" | "desc";
  cursor?: string;
  limit: number;
}>;

export type UserDataTimeseriesAggregate = Readonly<{
  operation: "timeseries_aggregate";
  series: string;
  start_ms: number;
  end_ms: number;
  bucket_ms: number;
  aggregation: "avg" | "min" | "max" | "sum" | "count";
}>;

export type UserDataObjectPut = Readonly<{
  operation: "object_put";
  key: string;
  content: string;
  encoding: "utf8" | "base64";
  content_type: string;
  metadata?: Readonly<Record<string, JsonValue>>;
  if_version?: number;
  sha256?: string;
}>;

export type UserDataObjectGet = Readonly<{
  operation: "object_get";
  key: string;
  encoding: "utf8" | "base64";
}>;

export type UserDataObjectDelete = Readonly<{
  operation: "object_delete";
  key: string;
  if_version?: number;
}>;

export type UserDataObjectList = Readonly<{
  operation: "object_list";
  prefix?: string;
  cursor?: string;
  limit: number;
}>;

export type UserDataOperation =
  | UserDataDocumentPut
  | UserDataDocumentGet
  | UserDataDocumentDelete
  | UserDataDocumentList
  | UserDataTimeseriesList
  | UserDataTimeseriesWrite
  | UserDataTimeseriesQuery
  | UserDataTimeseriesAggregate
  | UserDataObjectPut
  | UserDataObjectGet
  | UserDataObjectDelete
  | UserDataObjectList;

export const USER_DATA_TOOL_DESCRIPTION = [
  "Read and write the current user's durable private data store.",
  "Documents hold structured JSON, objects hold opaque UTF-8 or base64 content, and time series hold timestamped numeric measurements with optional JSON fields.",
  "Use reverse-domain or integration-prefixed keys and series, page large reads with returned cursors, and use if_version when updating data you previously read.",
].join(" ");

export class UserDataError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export function isUserDataMutation(operation: UserDataOperation): boolean {
  return operation.operation === "document_put"
    || operation.operation === "document_delete"
    || operation.operation === "timeseries_write"
    || operation.operation === "object_put"
    || operation.operation === "object_delete";
}

export function parseUserDataOperation(input: unknown): UserDataOperation {
  if (!isRecord(input) || typeof input.operation !== "string") {
    throw new UserDataError("invalid_request", "user data input must include an operation");
  }
  switch (input.operation) {
    case "document_put": {
      fields(input, ["operation", "key", "value", "if_version"]);
      const operation: UserDataDocumentPut = {
        operation: "document_put",
        key: dataKey(input.key),
        value: jsonValue(input.value),
        ...(input.if_version === undefined ? {} : { if_version: version(input.if_version) }),
      };
      return operation;
    }
    case "document_get":
      fields(input, ["operation", "key"]);
      return { operation: "document_get", key: dataKey(input.key) };
    case "document_delete":
      fields(input, ["operation", "key", "if_version"]);
      return {
        operation: "document_delete",
        key: dataKey(input.key),
        ...(input.if_version === undefined ? {} : { if_version: version(input.if_version) }),
      };
    case "document_list":
      fields(input, ["operation", "prefix", "cursor", "limit"]);
      return {
        operation: "document_list",
        ...(input.prefix === undefined ? {} : { prefix: dataPrefix(input.prefix) }),
        ...(input.cursor === undefined ? {} : { cursor: dataKey(input.cursor) }),
        limit: pageSize(input.limit),
      };
    case "timeseries_list":
      fields(input, ["operation", "prefix", "cursor", "limit"]);
      return {
        operation: "timeseries_list",
        ...(input.prefix === undefined ? {} : { prefix: seriesPrefix(input.prefix) }),
        ...(input.cursor === undefined ? {} : { cursor: seriesName(input.cursor) }),
        limit: pageSize(input.limit),
      };
    case "timeseries_write": {
      fields(input, ["operation", "series", "points", "conflict"]);
      if (!Array.isArray(input.points) || input.points.length === 0
        || input.points.length > MAX_USER_DATA_POINTS_PER_WRITE) {
        throw new UserDataError(
          "invalid_points",
          `timeseries_write requires 1-${MAX_USER_DATA_POINTS_PER_WRITE} points`,
        );
      }
      return {
        operation: "timeseries_write",
        series: seriesName(input.series),
        points: input.points.map(point),
        conflict: input.conflict === undefined ? "error" : conflictMode(input.conflict),
      };
    }
    case "timeseries_query": {
      fields(input, ["operation", "series", "start_ms", "end_ms", "order", "cursor", "limit"]);
      const start = optionalTimestamp(input.start_ms, "start_ms");
      const end = optionalTimestamp(input.end_ms, "end_ms");
      if (start !== undefined && end !== undefined && end < start) {
        throw new UserDataError("invalid_range", "end_ms must be greater than or equal to start_ms");
      }
      return {
        operation: "timeseries_query",
        series: seriesName(input.series),
        ...(start === undefined ? {} : { start_ms: start }),
        ...(end === undefined ? {} : { end_ms: end }),
        order: input.order === undefined ? "asc" : order(input.order),
        ...(input.cursor === undefined ? {} : { cursor: timestampCursor(input.cursor) }),
        limit: pageSize(input.limit),
      };
    }
    case "timeseries_aggregate": {
      fields(input, ["operation", "series", "start_ms", "end_ms", "bucket_ms", "aggregation"]);
      const start = timestamp(input.start_ms, "start_ms");
      const end = timestamp(input.end_ms, "end_ms");
      if (end < start) throw new UserDataError("invalid_range", "end_ms must be greater than or equal to start_ms");
      if (!Number.isSafeInteger(input.bucket_ms) || Number(input.bucket_ms) < 1) {
        throw new UserDataError("invalid_bucket", "bucket_ms must be a positive safe integer");
      }
      return {
        operation: "timeseries_aggregate",
        series: seriesName(input.series),
        start_ms: start,
        end_ms: end,
        bucket_ms: Number(input.bucket_ms),
        aggregation: aggregation(input.aggregation),
      };
    }
    case "object_put":
      fields(input, ["operation", "key", "content", "encoding", "content_type", "metadata", "if_version", "sha256"]);
      if (typeof input.content !== "string") throw new UserDataError("invalid_content", "object content must be a string");
      if (input.encoding !== "utf8" && input.encoding !== "base64") {
        throw new UserDataError("invalid_encoding", "object encoding must be utf8 or base64");
      }
      if (typeof input.content_type !== "string" || !CONTENT_TYPE.test(input.content_type)) {
        throw new UserDataError("invalid_content_type", "object content_type must be a visible ASCII media type");
      }
      if (input.sha256 !== undefined && (typeof input.sha256 !== "string" || !SHA256.test(input.sha256))) {
        throw new UserDataError("invalid_sha256", "object sha256 must be a lowercase hexadecimal SHA-256 digest");
      }
      return {
        operation: "object_put",
        key: dataKey(input.key),
        content: input.content,
        encoding: input.encoding,
        content_type: input.content_type,
        ...(input.metadata === undefined ? {} : { metadata: jsonRecord(input.metadata, "metadata") }),
        ...(input.if_version === undefined ? {} : { if_version: version(input.if_version) }),
        ...(input.sha256 === undefined ? {} : { sha256: input.sha256 }),
      };
    case "object_get":
      fields(input, ["operation", "key", "encoding"]);
      if (input.encoding !== undefined && input.encoding !== "utf8" && input.encoding !== "base64") {
        throw new UserDataError("invalid_encoding", "object encoding must be utf8 or base64");
      }
      return {
        operation: "object_get",
        key: dataKey(input.key),
        encoding: input.encoding === undefined ? "utf8" : input.encoding,
      };
    case "object_delete":
      fields(input, ["operation", "key", "if_version"]);
      return {
        operation: "object_delete",
        key: dataKey(input.key),
        ...(input.if_version === undefined ? {} : { if_version: version(input.if_version) }),
      };
    case "object_list":
      fields(input, ["operation", "prefix", "cursor", "limit"]);
      return {
        operation: "object_list",
        ...(input.prefix === undefined ? {} : { prefix: dataPrefix(input.prefix) }),
        ...(input.cursor === undefined ? {} : { cursor: dataKey(input.cursor) }),
        limit: pageSize(input.limit),
      };
    default:
      throw new UserDataError("invalid_operation", "unsupported user data operation");
  }
}

export function userDataToolInputSchema() {
  const key = { type: "string", minLength: 1, maxLength: 512, pattern: DATA_KEY.source } as const;
  const series = { type: "string", minLength: 1, maxLength: 256, pattern: SERIES.source } as const;
  const limit = { type: "integer", minimum: 1, maximum: MAX_USER_DATA_PAGE_SIZE, default: DEFAULT_USER_DATA_PAGE_SIZE } as const;
  const page = {
    prefix: { type: "string", maxLength: 512 }, cursor: key, limit,
  } as const;
  const operations = [
    objectSchema("document_put", { key, value: {}, if_version: positiveInteger() }, ["key", "value"]),
    objectSchema("document_get", { key }, ["key"]),
    objectSchema("document_delete", { key, if_version: positiveInteger() }, ["key"]),
    objectSchema("document_list", page, []),
    objectSchema("timeseries_list", {
      prefix: { type: "string", maxLength: 256 }, cursor: series, limit,
    }, []),
    objectSchema("timeseries_write", {
      series,
      points: {
        type: "array", minItems: 1, maxItems: MAX_USER_DATA_POINTS_PER_WRITE,
        items: {
          type: "object",
          properties: {
            timestamp_ms: { type: "integer", minimum: 0 },
            value: { type: "number" },
            fields: { type: "object", additionalProperties: true },
          },
          required: ["timestamp_ms", "value"], additionalProperties: false,
        },
      },
      conflict: { type: "string", enum: ["error", "replace"], default: "error" },
    }, ["series", "points"]),
    objectSchema("timeseries_query", {
      series,
      start_ms: timestampSchema(), end_ms: timestampSchema(),
      order: { type: "string", enum: ["asc", "desc"], default: "asc" },
      cursor: { type: "string", pattern: "^[0-9]+$" }, limit,
    }, ["series"]),
    objectSchema("timeseries_aggregate", {
      series, start_ms: timestampSchema(), end_ms: timestampSchema(),
      bucket_ms: positiveInteger(), aggregation: { type: "string", enum: ["avg", "min", "max", "sum", "count"] },
    }, ["series", "start_ms", "end_ms", "bucket_ms", "aggregation"]),
    objectSchema("object_put", {
      key, content: { type: "string" }, encoding: { type: "string", enum: ["utf8", "base64"] },
      content_type: { type: "string", minLength: 1, maxLength: 255 },
      metadata: { type: "object", additionalProperties: true }, if_version: positiveInteger(),
      sha256: { type: "string", pattern: "^[a-f0-9]{64}$" },
    }, ["key", "content", "encoding", "content_type"]),
    objectSchema("object_get", { key, encoding: { type: "string", enum: ["utf8", "base64"], default: "utf8" } }, ["key"]),
    objectSchema("object_delete", { key, if_version: positiveInteger() }, ["key"]),
    objectSchema("object_list", page, []),
  ];
  return { oneOf: operations } as const;
}

function objectSchema(
  operation: UserDataOperation["operation"],
  properties: Readonly<Record<string, unknown>>,
  required: readonly string[],
) {
  return {
    type: "object",
    properties: { operation: { type: "string", const: operation }, ...properties },
    required: ["operation", ...required],
    additionalProperties: false,
  } as const;
}

function timestampSchema() {
  return { type: "integer", minimum: 0 } as const;
}

function positiveInteger() {
  return { type: "integer", minimum: 1 } as const;
}

function dataKey(input: unknown): string {
  if (typeof input !== "string" || !DATA_KEY.test(input) || input.includes("//")
    || input.split("/").some((part) => part === "." || part === "..")) {
    throw new UserDataError("invalid_key", "data key must be a bounded relative identifier");
  }
  return input;
}

function dataPrefix(input: unknown): string {
  if (typeof input !== "string" || input.length > 512 || /[\u0000-\u001f\u007f]/u.test(input)
    || input.startsWith("/") || input.includes("\\")) {
    throw new UserDataError("invalid_prefix", "data prefix must be a bounded relative prefix");
  }
  return input;
}

function seriesName(input: unknown): string {
  if (typeof input !== "string" || !SERIES.test(input) || input.includes("//")) {
    throw new UserDataError("invalid_series", "series must be a bounded identifier");
  }
  return input;
}

function seriesPrefix(input: unknown): string {
  if (typeof input !== "string" || input.length > 256 || /[\u0000-\u001f\u007f]/u.test(input)
    || input.startsWith("/") || input.includes("\\")) {
    throw new UserDataError("invalid_prefix", "series prefix must be a bounded relative prefix");
  }
  return input;
}

function version(input: unknown): number {
  if (!Number.isSafeInteger(input) || Number(input) < 1) {
    throw new UserDataError("invalid_version", "if_version must be a positive safe integer");
  }
  return Number(input);
}

function timestamp(input: unknown, name: string): number {
  if (!Number.isSafeInteger(input) || Number(input) < 0) {
    throw new UserDataError("invalid_timestamp", `${name} must be a non-negative safe integer`);
  }
  return Number(input);
}

function optionalTimestamp(input: unknown, name: string): number | undefined {
  return input === undefined ? undefined : timestamp(input, name);
}

function timestampCursor(input: unknown): string {
  if (typeof input !== "string" || !/^(?:0|[1-9][0-9]*)$/u.test(input)
    || !Number.isSafeInteger(Number(input))) {
    throw new UserDataError("invalid_cursor", "time-series cursor must be a safe integer timestamp string");
  }
  return input;
}

function pageSize(input: unknown): number {
  if (input === undefined) return DEFAULT_USER_DATA_PAGE_SIZE;
  if (!Number.isSafeInteger(input) || Number(input) < 1 || Number(input) > MAX_USER_DATA_PAGE_SIZE) {
    throw new UserDataError("invalid_limit", `limit must be between 1 and ${MAX_USER_DATA_PAGE_SIZE}`);
  }
  return Number(input);
}

function point(input: unknown): UserDataPoint {
  if (!isRecord(input)) throw new UserDataError("invalid_point", "each time-series point must be an object");
  fields(input, ["timestamp_ms", "value", "fields"]);
  if (typeof input.value !== "number" || !Number.isFinite(input.value)) {
    throw new UserDataError("invalid_value", "time-series values must be finite numbers");
  }
  return {
    timestamp_ms: timestamp(input.timestamp_ms, "timestamp_ms"),
    value: input.value,
    ...(input.fields === undefined ? {} : { fields: jsonRecord(input.fields, "fields") }),
  };
}

function conflictMode(input: unknown): "error" | "replace" {
  if (input !== "error" && input !== "replace") {
    throw new UserDataError("invalid_conflict_mode", "conflict must be error or replace");
  }
  return input;
}

function order(input: unknown): "asc" | "desc" {
  if (input !== "asc" && input !== "desc") throw new UserDataError("invalid_order", "order must be asc or desc");
  return input;
}

function aggregation(input: unknown): UserDataTimeseriesAggregate["aggregation"] {
  if (input !== "avg" && input !== "min" && input !== "max" && input !== "sum" && input !== "count") {
    throw new UserDataError("invalid_aggregation", "unsupported time-series aggregation");
  }
  return input;
}

function jsonRecord(input: unknown, name: string): Readonly<Record<string, JsonValue>> {
  if (!isRecord(input)) throw new UserDataError(`invalid_${name}`, `${name} must be a JSON object`);
  return jsonValue(input) as Readonly<Record<string, JsonValue>>;
}

function jsonValue(input: unknown): JsonValue {
  const seen = new WeakSet<object>();
  const normalize = (value: unknown): JsonValue => {
    if (value === null || typeof value === "string" || typeof value === "boolean") return value;
    if (typeof value === "number") {
      if (!Number.isFinite(value)) throw new Error("non-finite number");
      return value;
    }
    if (typeof value !== "object") throw new Error("unsupported JSON value");
    if (seen.has(value)) throw new Error("cyclic JSON value");
    seen.add(value);
    try {
      if (Array.isArray(value)) return value.map(normalize);
      const prototype = Object.getPrototypeOf(value);
      if (prototype !== Object.prototype && prototype !== null) throw new Error("non-plain JSON object");
      return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, normalize(entry)]));
    } finally {
      seen.delete(value);
    }
  };
  try {
    return normalize(input);
  } catch {
    throw new UserDataError("invalid_json", "value must be finite, acyclic JSON");
  }
}

function fields(input: Record<string, unknown>, allowed: readonly string[]): void {
  const supported = new Set(allowed);
  const unexpected = Object.keys(input).find((key) => !supported.has(key));
  if (unexpected !== undefined) throw new UserDataError("invalid_request", `unsupported field ${unexpected}`);
}

function isRecord(input: unknown): input is Record<string, unknown> {
  return typeof input === "object" && input !== null && !Array.isArray(input);
}
