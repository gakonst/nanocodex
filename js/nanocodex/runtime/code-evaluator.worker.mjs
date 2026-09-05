import { installBrowserEgressFetch } from "../tools/browser/browserEgress.mjs";

const WORKER_PROTOCOL = "nanocodex.code-evaluator.v1";
const pendingTools = new Map();
const toolInvocations = [];
let nextToolCall = 1;
let evaluating = false;
let evaluationId;

globalThis.onmessage = ({ data }) => {
  if (data?.protocol !== WORKER_PROTOCOL) return;
  if (data.type === "tool.result") {
    if (data.evaluationId !== evaluationId) return;
    const pending = pendingTools.get(data.id);
    if (!pending) return;
    pendingTools.delete(data.id);
    (data.ok ? pending.resolve : pending.reject)(data.value);
    return;
  }
  if (data.type !== "evaluate" || evaluating) return;
  if (data.egress) installBrowserEgressFetch(data.egress);
  evaluating = true;
  evaluationId = data.evaluationId;
  void evaluate(data).then(
    (storedWrites) => post("completed", { storedWrites }),
    ({ error, storedWrites }) => post("failed", { error: errorMessage(error), storedWrites }),
  );
};

async function evaluate({ source, storedEntries = [], toolDefinitions = [], toolNames = [] }) {
  const stored = new Map(storedEntries);
  const storedWrites = new Map();
  const tools = Object.create(null);
  for (const name of toolNames) tools[name] = (input) => callTool(name, input);
  const callableTools = new Proxy(tools, {
    get(target, property) {
      if (typeof property !== "string") return Reflect.get(target, property);
      return target[property] ?? ((input) => callTool(property, input));
    },
  });
  Object.freeze(tools);

  const text = (value) => post("output", { kind: "text", value });
  const image = (value, detail = "auto") => post("output", {
    kind: "image",
    value,
    detail,
  });
  const generatedImage = (value) => post("output", { kind: "generatedImage", value });
  const store = (key, value) => {
    if (typeof key !== "string") throw new TypeError("store key must be a string");
    const snapshot = structuredClone(value);
    stored.set(key, snapshot);
    storedWrites.set(key, snapshot);
  };
  const load = (key) => stored.has(key) ? structuredClone(stored.get(key)) : undefined;
  const EXIT = Symbol("exit");
  const exit = () => { throw EXIT; };
  const guestConsole = Object.freeze(Object.fromEntries(
    ["debug", "info", "log", "warn", "error"].map((level) => [level, (...values) => {
      post("console", { level, values: values.map(stringify) });
    }]),
  ));

  try {
    const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
    const script = new AsyncFunction(
      "tools",
      "ALL_TOOLS",
      "text",
      "image",
      "generatedImage",
      "store",
      "load",
      "exit",
      "require",
      "console",
      source,
    );
    try {
      await script(
        callableTools,
        Object.freeze(toolDefinitions),
        text,
        image,
        generatedImage,
        store,
        load,
        exit,
        undefined,
        guestConsole,
      );
    } finally {
      // Discarding a tool Promise or calling exit() must not orphan parent-held
      // work. The evaluator remains alive until each invocation receives its
      // terminal response, or its supervisor terminates this cell on cancel.
      await Promise.allSettled(toolInvocations);
    }
    return [...storedWrites];
  } catch (error) {
    if (error === EXIT) return [...storedWrites];
    throw { error, storedWrites: [...storedWrites] };
  }
}

function callTool(name, input) {
  const id = nextToolCall++;
  const invocation = new Promise((resolve, reject) => {
    pendingTools.set(id, { resolve, reject });
    post("tool.call", { id, input: input ?? null, name });
  });
  toolInvocations.push(invocation.then(() => undefined, () => undefined));
  return invocation;
}

function post(type, value = {}) {
  globalThis.postMessage({ protocol: WORKER_PROTOCOL, evaluationId, type, ...value });
}

function stringify(value) {
  if (typeof value === "string") return value;
  if (value === undefined) return "undefined";
  try { return JSON.stringify(value); } catch { return String(value); }
}

function errorMessage(error) {
  return error && (error.stack || error.message) ? error.stack || error.message : String(error);
}
