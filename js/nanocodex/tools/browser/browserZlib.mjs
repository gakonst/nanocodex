import { Buffer } from "./browserBuffer.mjs";
import { Deflate, Inflate, constants as pakoConstants } from "pako";

const DEFAULT_CHUNK_BYTES = 64 * 1024;

export const constants = Object.freeze({
  Z_BEST_COMPRESSION: pakoConstants.Z_BEST_COMPRESSION,
  Z_BEST_SPEED: pakoConstants.Z_BEST_SPEED,
  Z_DEFAULT_COMPRESSION: pakoConstants.Z_DEFAULT_COMPRESSION,
});

export const {
  Z_BEST_COMPRESSION,
  Z_BEST_SPEED,
  Z_DEFAULT_COMPRESSION,
} = constants;

export function gzipSync(input, options = undefined) {
  const bytes = inputBytes(input);
  const normalized = compressionOptions(options);
  return transform(
    new Deflate({ ...normalized.pako, gzip: true, chunkSize: normalized.chunkSize }),
    bytes,
    normalized.maxOutputLength,
  );
}

export function gunzipSync(input, options = undefined) {
  const bytes = inputBytes(input);
  const normalized = compressionOptions(options);
  return transform(
    new Inflate({ ...normalized.pako, chunkSize: normalized.chunkSize }),
    bytes,
    normalized.maxOutputLength,
  );
}

function transform(stream, input, maxOutputLength) {
  const chunks = [];
  let outputLength = 0;
  stream.onData = (chunk) => {
    const nextLength = outputLength + chunk.byteLength;
    if (!Number.isSafeInteger(nextLength) || nextLength > maxOutputLength) {
      throw bufferTooLarge(maxOutputLength);
    }
    chunks.push(chunk);
    outputLength = nextLength;
  };
  stream.push(input, true);
  if (stream.err) throw zlibError(stream.err, stream.msg);
  if (chunks.length === 0) return Buffer.alloc(0);
  if (chunks.length === 1) {
    const [chunk] = chunks;
    return Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
  }
  return Buffer.concat(
    chunks.map((chunk) => Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength)),
    outputLength,
  );
}

function compressionOptions(options) {
  if (options !== undefined && (options === null || typeof options !== "object")) {
    throw invalidOptions(options);
  }
  const { maxOutputLength, ...pako } = options ?? {};
  const maximum = validateMaxOutputLength(maxOutputLength);
  return {
    maxOutputLength: maximum,
    // pako emits incrementally at this boundary. For a very small caller limit,
    // ask it to stop at the first byte beyond that limit instead of materializing
    // the complete decompressed payload before the compatibility layer sees it.
    chunkSize: Math.max(2, Math.min(DEFAULT_CHUNK_BYTES, Math.floor(maximum) + 1)),
    pako,
  };
}

function validateMaxOutputLength(value) {
  if (value === undefined || Number.isNaN(value)) return Number.MAX_SAFE_INTEGER;
  if (typeof value !== "number") {
    const error = new TypeError(
      `The "options.maxOutputLength" property must be of type number. Received type ${typeof value}`,
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  if (!Number.isFinite(value) || value < 1 || value > Number.MAX_SAFE_INTEGER) {
    const error = new RangeError(
      `The value of "options.maxOutputLength" is out of range. It must be >= 1 and <= ${Number.MAX_SAFE_INTEGER}. Received ${value}`,
    );
    error.code = "ERR_OUT_OF_RANGE";
    throw error;
  }
  return value;
}

function inputBytes(input) {
  if (typeof input === "string") return new TextEncoder().encode(input);
  if (ArrayBuffer.isView(input)) {
    return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  }
  if (input instanceof ArrayBuffer || isSharedArrayBuffer(input)) {
    return new Uint8Array(input);
  }
  const received = input === null ? "null" : typeof input;
  const error = new TypeError(
    `The "buffer" argument must be of type string or an instance of Buffer, TypedArray, DataView, or ArrayBuffer. Received ${received}`,
  );
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
}

function isSharedArrayBuffer(input) {
  return typeof SharedArrayBuffer !== "undefined" && input instanceof SharedArrayBuffer;
}

function invalidOptions(options) {
  const error = new TypeError(
    `The "options" argument must be of type object. Received ${options === null ? "null" : typeof options}`,
  );
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}

function bufferTooLarge(maximum) {
  const error = new RangeError(`Cannot create a Buffer larger than ${maximum} bytes`);
  error.code = "ERR_BUFFER_TOO_LARGE";
  return error;
}

function zlibError(errno, message) {
  const error = new Error(message || "zlib operation failed");
  error.code = errno === pakoConstants.Z_DATA_ERROR ? "Z_DATA_ERROR" : "Z_BUF_ERROR";
  error.errno = errno;
  return error;
}
