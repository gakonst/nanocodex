import { Console } from "node:console";
import { createRequire } from "node:module";
import readline from "node:readline";

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const executions = [];
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
  if (message.type !== "tool_result" || currentCell?.cellId !== message.cell_id) {
    return;
  }

  const entry = currentCell.pending.get(message.id);
  if (!entry) return;
  currentCell.pending.delete(message.id);
  entry.resolve(message.value);
});

function stringify(value) {
  if (typeof value === "string") return value;
  if (value === undefined) return "undefined";
  try { return JSON.stringify(value); } catch { return String(value); }
}

const require = createRequire(import.meta.url);
const scriptConsole = new Console({ stdout: process.stderr, stderr: process.stderr });
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

async function runCell(init) {
  const pending = new Map();
  const content = [];
  const stored = new Map(Object.entries(init.stored));
  let nextId = 1;
  currentCell = { cellId: init.cell_id, pending };

  const tools = Object.create(null);
  for (const definition of init.tools) {
    tools[definition.name] = (input) => new Promise((resolve) => {
      const id = nextId++;
      pending.set(id, { resolve });
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

  function image(value, detail = "auto") {
    if (typeof value === "string") {
      content.push({ type: "input_image", image_url: value, detail });
      return;
    }
    if (!value || typeof value !== "object" || typeof value.image_url !== "string") {
      throw new TypeError("image() requires a data URL or image item");
    }
    content.push({
      type: "input_image",
      image_url: value.image_url,
      detail: value.detail ?? detail ?? "auto",
    });
  }

  function store(key, value) {
    if (typeof key !== "string") throw new TypeError("store key must be a string");
    stored.set(key, structuredClone(value));
  }

  function load(key) {
    return stored.has(key) ? structuredClone(stored.get(key)) : undefined;
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
      "store",
      "load",
      "exit",
      "require",
      "console",
      init.source,
    );
    try {
      await script(tools, allTools, text, image, store, load, exit, require, scriptConsole);
    } catch (error) {
      if (error !== EXIT) throw error;
    }
    send({
      type: "done",
      cell_id: init.cell_id,
      content,
      stored: Object.fromEntries(stored),
    });
  } catch (error) {
    const message = error?.stack || error?.message || String(error);
    send({
      type: "error",
      cell_id: init.cell_id,
      message,
      stored: Object.fromEntries(stored),
    });
  } finally {
    currentCell = undefined;
  }
}

for (;;) {
  await runCell(await nextExecution());
}
