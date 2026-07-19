import { Console } from "node:console";
import { createRequire } from "node:module";
import readline from "node:readline";

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const executions = [];
const imageDetails = new Set(["auto", "low", "high", "original"]);
const imageHelperExpects =
  "image expects a non-empty image URL string or an object with image_url and optional detail";
const invalidImageOutput =
  "Tool call failed: invalid image output. Pass a base64 data URI instead";
const remoteImageOutput =
  "Tool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead";
const jsonParse = JSON.parse;
const jsonStringify = JSON.stringify;
let wakeExecution;
let currentCell;

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function enqueueExecution(message) {
  if (wakeExecution) {
    const resolve = wakeExecution;
    wakeExecution = undefined;
    resolve(message);
  } else {
    executions.push(message);
  }
}

function nextExecution() {
  if (executions.length) return Promise.resolve(executions.shift());
  return new Promise((resolve) => { wakeExecution = resolve; });
}

lines.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    console.error("invalid code-mode host message", error);
    return;
  }

  if (message.type === "execute") {
    enqueueExecution(message);
    return;
  }
  if (
    message.type !== "tool_result" ||
    !currentCell ||
    currentCell.cellId !== message.cell_id
  ) {
    return;
  }

  const entry = currentCell.pending.get(message.id);
  if (!entry) return;
  currentCell.pending.delete(message.id);
  if (message.success) {
    entry.resolve(message.value);
  } else {
    entry.reject(message.value);
  }
});

function stringify(value) {
  if (typeof value === "string") return value;
  if (value === undefined) return "undefined";
  try { return JSON.stringify(value); } catch { return String(value); }
}

function errorText(error) {
  return error && (error.stack || error.message) || String(error);
}

function storageKey(value, helper) {
  try {
    return `${value}`;
  } catch {
    throw `${helper} key must be a string`;
  }
}

function storedValue(key, value) {
  let encoded;
  try {
    encoded = jsonStringify(value);
  } catch (error) {
    throw errorText(error);
  }
  if (encoded === undefined) {
    throw `Unable to store ${jsonStringify(key)}. Only plain serializable objects can be stored.`;
  }
  return jsonParse(encoded);
}

// Node 12 leaves import.meta.url undefined for --eval modules. Anchor require
// to the agent's working directory, which is also where model cells execute.
const require = createRequire(`${process.cwd()}/`);
const scriptConsole = new Console({ stdout: process.stderr, stderr: process.stderr });
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const cloneValue = typeof globalThis.structuredClone === "function"
  ? globalThis.structuredClone
  : (value) => value === undefined
    ? undefined
    : JSON.parse(JSON.stringify(value));

async function runCell(init) {
  const pending = new Map();
  const content = [];
  const stored = new Map(Object.entries(init.stored));
  const storedWrites = new Map();
  let nextId = 1;
  currentCell = { cellId: init.cell_id, pending };

  const tools = Object.create(null);
  for (const definition of init.tools) {
    tools[definition.name] = (input) => new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      send({
        type: "tool_call",
        cell_id: init.cell_id,
        id,
        name: definition.name,
        input,
      });
    });
  }
  Object.freeze(tools);

  function text(value) {
    content.push({ type: "input_text", text: stringify(value) });
  }

  function notify(value) {
    const notification = stringify(value);
    if (!notification.trim()) throw "notify expects non-empty text";
    send({
      type: "notify",
      cell_id: init.cell_id,
      text: notification,
    });
  }

  function image(value, detail) {
    let imageUrl;
    let embeddedDetail;
    if (typeof value === "string") {
      imageUrl = value;
    } else if (
      value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      typeof value.image_url === "string"
    ) {
      imageUrl = value.image_url;
      embeddedDetail = value.detail;
    } else {
      throw imageHelperExpects;
    }

    if (!imageUrl) {
      throw imageHelperExpects;
    }
    const separator = imageUrl.indexOf(":");
    if (separator < 0) {
      throw invalidImageOutput;
    }
    const scheme = imageUrl.slice(0, separator).toLowerCase();
    if (scheme === "http" || scheme === "https") {
      throw remoteImageOutput;
    }
    if (scheme !== "data") {
      throw invalidImageOutput;
    }

    const selectedDetail = detail != null
      ? detail
      : embeddedDetail != null
        ? embeddedDetail
        : "high";
    if (typeof selectedDetail !== "string") {
      throw "image detail must be one of: auto, low, high, original";
    }
    const normalizedDetail = selectedDetail.toLowerCase();
    if (!imageDetails.has(normalizedDetail)) {
      throw "image detail must be one of: auto, low, high, original";
    }

    content.push({
      type: "input_image",
      image_url: imageUrl,
      detail: normalizedDetail,
    });
  }

  function generatedImage(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw "generatedImage expects an image generation result object";
    }
    const outputHint = value.output_hint;
    if (outputHint !== undefined && typeof outputHint !== "string") {
      throw "generatedImage output_hint must be a string when provided";
    }
    image(value);
    if (outputHint !== undefined) {
      content.push({ type: "input_text", text: outputHint });
    }
  }

  function store(key, value) {
    const normalizedKey = storageKey(key, "store");
    const normalizedValue = storedValue(normalizedKey, value);
    stored.set(normalizedKey, normalizedValue);
    storedWrites.set(normalizedKey, normalizedValue);
  }

  function load(key) {
    const normalizedKey = storageKey(key, "load");
    return stored.has(normalizedKey) ? cloneValue(stored.get(normalizedKey)) : undefined;
  }

  async function yield_control() {
    if (pending.size) {
      throw new Error("yield_control() cannot run while nested tool calls are pending");
    }
    send({
      type: "yielded",
      cell_id: init.cell_id,
      content: content.splice(0),
      stored: Object.fromEntries(stored),
    });
    await Promise.resolve();
  }

  const EXIT = Symbol("exit");
  function exit() { throw EXIT; }

  const allTools = Object.freeze(init.tools.map((tool) => Object.freeze({ ...tool })));
  try {
    const script = new AsyncFunction(
      "tools",
      "ALL_TOOLS",
      "text",
      "image",
      "generatedImage",
      "notify",
      "store",
      "load",
      "yield_control",
      "exit",
      "require",
      "console",
      init.source,
    );
    try {
      await script(
        tools,
        allTools,
        text,
        image,
        generatedImage,
        notify,
        store,
        load,
        yield_control,
        exit,
        require,
        scriptConsole,
      );
    } catch (error) {
      if (error !== EXIT) throw error;
    }
    send({
      type: "done",
      cell_id: init.cell_id,
      content,
      stored: Object.fromEntries(storedWrites),
    });
  } catch (error) {
    const message = errorText(error);
    send({
      type: "error",
      cell_id: init.cell_id,
      message,
      content,
      stored: Object.fromEntries(storedWrites),
    });
  } finally {
    currentCell = undefined;
  }
}

async function main() {
  for (;;) {
    await runCell(await nextExecution());
  }
}

main().catch((error) => {
  console.error(error && (error.stack || error.message) || String(error));
  process.exitCode = 1;
});
