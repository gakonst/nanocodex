const MAX_CONCURRENT_NESTED_CALLS = 128;
const CANCELLATION_MESSAGE = "Code Mode execution was cancelled";

export function createCodeRuntime(toolConfiguration = {}, extras = {}) {
  const activeExecutions = new Set();
  const codeObservations = new Map();
  const stores = new Map();
  const providers = [];
  let nextCallId = 1;
  const definitions = [];
  const configuredTools = [];
  const toolByName = new Map();
  const subagentsBySession = new Map();

  function addTools(configuration = {}) {
    for (const [name, tool] of Object.entries(configuration)) {
      if (toolByName.has(name)) {
        throw new Error(`tool is already configured: ${name}`);
      }
      addTool(name, tool, { configuredTools, definitions, toolByName });
    }
  }
  addTools(toolConfiguration);

  function currentDefinitions() {
    return [
      ...definitions,
      ...providers.flatMap((provider) => provider.definitions()),
    ];
  }

  function currentCodeDefinitions() {
    return currentDefinitions().map((definition) => definition.type === "tool_search"
      ? deepFreeze({
          type: "function",
          name: "tool_search",
          description: definition.description,
          strict: false,
          parameters: jsonSnapshot(definition.parameters, "tool_search parameters"),
        })
      : definition);
  }

  function currentTools() {
    const tools = [...configuredTools];
    for (const provider of providers) {
      for (const definition of provider.definitions()) {
        const name = definition.type === "tool_search" ? "tool_search" : definition.name;
        const tool = provider.resolve(name);
        if (tool) tools.push(tool);
      }
    }
    return tools;
  }

  function resolveTool(name) {
    const configured = toolByName.get(name);
    if (configured) return configured;
    for (const provider of providers) {
      const tool = provider.resolve(name);
      if (tool) return tool;
    }
  }

  async function executeTool(name, encodedInput, sessionId = "default", callId = "tool") {
    const tool = resolveTool(name);
    if (!tool) return encodeToolOutput(`unknown application tool: ${name}`, false, null);
    let input;
    try {
      input = JSON.parse(encodedInput);
    } catch (error) {
      return encodeToolOutput(`invalid tool input: ${errorMessage(error)}`, false, null);
    }
    const controller = new AbortController();
    const execution = { callId, controller, sessionId };
    activeExecutions.add(execution);
    try {
      const result = await tool.handler(input, {
        sessionId,
        parentCallId: "",
        callId,
        signal: controller.signal,
        subagent: subagentsBySession.get(sessionId),
      });
      return encodeToolOutput(
        outputBody(result),
        toolSucceeded(result),
        structuredResult(result, `tool ${name} result`),
        toolMetadata(result, `tool ${name} metadata`),
      );
    } catch (error) {
      return encodeToolOutput(errorMessage(error), false, null);
    } finally {
      activeExecutions.delete(execution);
    }
  }

  async function executeCode(source, sessionId = "default", parentCallId = "exec", observer) {
    const startedAt = performance.now();
    const content = [];
    const stored = stores.get(sessionId) || new Map();
    stores.set(sessionId, stored);
    const nestedCalls = [];
    const controller = new AbortController();
    const execution = { callId: parentCallId, controller, sessionId };
    activeExecutions.add(execution);
    const tools = Object.create(null);
    const availableTools = currentTools();
    const availableDefinitions = currentCodeDefinitions();
    const nestedInvocations = [];
    const nestedCallPermits = new AsyncSemaphore(MAX_CONCURRENT_NESTED_CALLS);
    const parallelExecution = new AsyncReadWriteGate();
    for (const { handler, name, parallelSafe } of availableTools) {
      tools[name] = (input) => {
        const invocation = executeNestedTool(input);
        // Attach a rejection handler immediately so a discarded guest Promise
        // cannot become an unhandled rejection before the cell reaches its
        // quiescence boundary.
        nestedInvocations.push(invocation.then(() => undefined, () => undefined));
        return invocation;
      };

      async function executeNestedTool(input) {
        const callId = `${parentCallId}/code-${nextCallId++}`;
        const toolStartedAt = performance.now();
        const startedAfterNs = Math.max(
          0,
          Math.round((toolStartedAt - startedAt) * 1_000_000),
        );
        const recordedInput = clone(input) ?? null;
        const recordedCall = {
          call_id: callId,
          name,
          input: recordedInput,
          output: "",
          structured_result: null,
          success: false,
          started_after_ns: startedAfterNs,
          duration_ns: 0,
          metadata: null,
        };
        // Rust records nested calls in invocation order even when parallel
        // siblings finish out of order. Reserve the slot before dispatch.
        nestedCalls.push(recordedCall);
        observer?.({
          type: "nested_call_started",
          call_id: callId,
          name,
          input: recordedInput,
        });
        let result;
        try {
          controller.signal.throwIfAborted();
          const releasePermit = await nestedCallPermits.acquire(controller.signal);
          let releaseExecution;
          try {
            releaseExecution = await parallelExecution.acquire(parallelSafe, controller.signal);
            result = await handler(input, {
              sessionId,
              parentCallId,
              callId,
              signal: controller.signal,
              subagent: subagentsBySession.get(sessionId),
            });
          } finally {
            releaseExecution?.();
            releasePermit();
          }
        } catch (error) {
          const message = errorMessage(error);
          Object.assign(recordedCall, {
            output: message,
            structured_result: message,
            success: false,
            duration_ns: elapsedNs(toolStartedAt),
          });
          observer?.({ type: "nested_call_completed", call: recordedCall });
          throw error;
        }
        let structured;
        let output;
        let metadata;
        let success;
        try {
          structured = structuredResult(result, `tool ${name} result`);
          output = outputBody(result);
          metadata = toolMetadata(result, `tool ${name} metadata`);
          success = toolSucceeded(result);
        } catch (error) {
          const message = errorMessage(error);
          Object.assign(recordedCall, {
            output: message,
            structured_result: message,
            success: false,
            duration_ns: elapsedNs(toolStartedAt),
          });
          observer?.({ type: "nested_call_completed", call: recordedCall });
          throw error;
        }
        Object.assign(recordedCall, {
          output,
          structured_result: structured,
          success,
          duration_ns: elapsedNs(toolStartedAt),
          metadata,
        });
        observer?.({ type: "nested_call_completed", call: recordedCall });
        if (!success) throw toolValue(result);
        return toolValue(result);
      }
    }
    Object.freeze(tools);
    const EXIT = Symbol("exit");

    function text(value) {
      content.push({ type: "input_text", text: stringify(value) });
    }
    function image(value, detail = "auto") {
      if (typeof value === "string") {
        content.push({ type: "input_image", image_url: value, detail });
        return;
      }
      if (!value || typeof value !== "object" || typeof value.image_url !== "string") {
        throw new TypeError("image() requires an image URL or image item");
      }
      content.push({
        type: "input_image",
        image_url: value.image_url,
        detail: value.detail == null ? detail : value.detail,
      });
    }
    function generatedImage(result) {
      if (!result || typeof result !== "object" || typeof result.image_url !== "string") {
        throw new TypeError("generatedImage() requires an image generation result");
      }
      image(result.image_url, "high");
      if (typeof result.output_hint === "string" && result.output_hint) text(result.output_hint);
    }
    function store(key, value) {
      if (typeof key !== "string") throw new TypeError("store key must be a string");
      stored.set(key, clone(value));
    }
    function load(key) {
      return stored.has(key) ? clone(stored.get(key)) : undefined;
    }
    function exit() {
      throw EXIT;
    }

    try {
      try {
        await abortableEvaluation((async () => {
          try {
            await (extras.evaluate || evaluateNative)(source, {
              tools,
              toolDefinitions: availableDefinitions,
              text,
              image,
              generatedImage,
              store,
              load,
              exit,
              require: extras.require,
              console: extras.console || console,
              signal: controller.signal,
              storedEntries: [...stored],
            });
          } finally {
            // A guest may discard a tool Promise or call exit(). The cell still
            // owns that work: do not report completion or drop its cancellation
            // controller until every invocation reaches a terminal boundary.
            await Promise.allSettled(nestedInvocations);
          }
        })(), controller.signal);
      } catch (error) {
        if (error !== EXIT) throw error;
      }
      return JSON.stringify({
        output: withStatus("Script completed", startedAt, content),
        success: true,
        nested_calls: nestedCalls,
      });
    } catch (error) {
      return JSON.stringify({
        output: `Script failed\nWall time ${wallTime(startedAt)} seconds\nOutput:\n${errorMessage(error)}`,
        success: false,
        nested_calls: nestedCalls,
      });
    } finally {
      activeExecutions.delete(execution);
    }
  }

  function executeCodeObserved(source, sessionId = "default", parentCallId = "exec") {
    const key = codeObservationKey(sessionId, parentCallId);
    codeObservations.get(key)?.close();
    const observation = createCodeObservation(sessionId);
    codeObservations.set(key, observation);
    let execution;
    try {
      execution = executeCode(
        source,
        sessionId,
        parentCallId,
        (update) => observation.push(JSON.stringify(update)),
      );
    } catch (error) {
      observation.close();
      throw error;
    }
    void Promise.resolve(execution).then(
      () => observation.close(),
      () => observation.close(),
    );
    return execution;
  }

  async function nextCodeUpdate(sessionId, parentCallId) {
    const key = codeObservationKey(sessionId, parentCallId);
    const observation = codeObservations.get(key);
    if (!observation) throw new Error(`unknown Code Mode observation: ${parentCallId}`);
    const update = await observation.next();
    if (update === null && codeObservations.get(key) === observation) {
      codeObservations.delete(key);
    }
    return update;
  }

  function closeCodeObservations(sessionId) {
    for (const [key, observation] of codeObservations) {
      if (sessionId !== undefined && observation.sessionId !== sessionId) continue;
      codeObservations.delete(key);
      observation.close();
    }
  }

  function cancel(sessionId) {
    for (const execution of activeExecutions) {
      if (sessionId === undefined || execution.sessionId === sessionId) {
        execution.controller.abort(new Error(CANCELLATION_MESSAGE));
      }
    }
    closeCodeObservations(sessionId);
  }

  function releaseSession(sessionId) {
    stores.delete(sessionId);
    subagentsBySession.delete(sessionId);
    for (const tool of configuredTools) tool.releaseSession?.(sessionId);
    closeCodeObservations(sessionId);
  }

  function reset() {
    for (const execution of activeExecutions) {
      execution.controller.abort(new Error(CANCELLATION_MESSAGE));
    }
    stores.clear();
    subagentsBySession.clear();
    for (const tool of configuredTools) tool.dispose?.();
    closeCodeObservations();
  }

  return Object.freeze({
    addTools,
    addProvider(provider) {
      if (!provider || typeof provider.definitions !== "function" || typeof provider.resolve !== "function") {
        throw new TypeError("a Code Mode tool provider requires definitions() and resolve(name)");
      }
      providers.push(provider);
    },
    executeCode,
    executeCodeObserved,
    executeTool,
    bindSubagentSession(sessionId, context) {
      subagentsBySession.set(sessionId, Object.freeze({ ...context }));
    },
    nextCodeUpdate,
    cancel,
    toolDefinitions: () => JSON.stringify(currentDefinitions()),
    releaseSession,
    reset,
  });
}

function addTool(name, tool, collection) {
  if (!tool || typeof tool.handler !== "function") {
    throw new TypeError(`tool ${name} requires a handler function`);
  }
  const configured = Object.freeze({
    dispose: typeof tool.dispose === "function" ? tool.dispose : undefined,
    handler: tool.handler,
    name,
    parallelSafe: tool.supportsParallelToolCalls === true,
    releaseSession: typeof tool.releaseSession === "function" ? tool.releaseSession : undefined,
  });
  collection.configuredTools.push(configured);
  collection.toolByName.set(name, configured);
  const definition = {
    type: "function",
    name,
    description: tool.description || "Application-defined tool.",
    strict: false,
    parameters: jsonSnapshot(tool.parameters || {
      type: "object",
      additionalProperties: true,
    }, `tool ${name} parameters`),
  };
  if (tool.outputSchema !== undefined) {
    definition.output_schema = jsonSnapshot(tool.outputSchema, "tool output schema");
  }
  collection.definitions.push(deepFreeze(definition));
}

async function evaluateNative(source, environment) {
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
  await script(
    environment.tools,
    environment.toolDefinitions,
    environment.text,
    environment.image,
    environment.generatedImage,
    environment.store,
    environment.load,
    environment.exit,
    environment.require,
    environment.console,
  );
}

function encodeToolOutput(output, success, structuredResult, metadata = null) {
  return JSON.stringify({
    output,
    success,
    structured_result: structuredResult,
    metadata,
    process_trace: null,
  });
}

function outputBody(value) {
  if (isToolResult(value)) return outputBody(value.output);
  if (Array.isArray(value) && value.every((item) => item?.type === "input_text" || item?.type === "input_image")) {
    return clone(value);
  }
  return stringify(value);
}

function stringify(value) {
  if (typeof value === "string") return value;
  if (value === undefined) return "undefined";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function clone(value) {
  if (typeof globalThis.structuredClone === "function") return structuredClone(value);
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

function jsonSnapshot(value, label) {
  try {
    return JSON.parse(JSON.stringify(value));
  } catch (error) {
    throw new TypeError(`${label} must be JSON-serializable`, { cause: error });
  }
}

function structuredResult(value, label) {
  if (isToolResult(value)) return jsonSnapshot(value.structuredResult, label);
  return value === undefined ? null : jsonSnapshot(value, label);
}

function toolMetadata(value, label) {
  if (!isToolResult(value) || value.metadata == null) return null;
  return jsonSnapshot(value.metadata, label);
}

function toolSucceeded(value) {
  return !isToolResult(value) || value.success;
}

function toolValue(value) {
  return isToolResult(value) ? value.value : value;
}

const TOOL_RESULT = Symbol("nanocodex.toolResult");

export function toolResult(output, structuredResult = output, options = {}) {
  const success = options.success ?? true;
  if (typeof success !== "boolean") throw new TypeError("tool result success must be boolean");
  const value = Object.prototype.hasOwnProperty.call(options, "value")
    ? options.value
    : output;
  return Object.freeze({
    [TOOL_RESULT]: true,
    metadata: options.metadata ?? null,
    output,
    structuredResult,
    success,
    value,
  });
}

function isToolResult(value) {
  return Boolean(value?.[TOOL_RESULT]);
}

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}

function errorMessage(error) {
  if (error && (error.stack || error.message)) return error.stack || error.message;
  return String(error);
}

function elapsedNs(startedAt) {
  return Math.max(0, Math.round((performance.now() - startedAt) * 1_000_000));
}

function wallTime(startedAt) {
  return ((performance.now() - startedAt) / 1_000).toFixed(1);
}

function withStatus(status, startedAt, content) {
  const heading = `${status}\nWall time ${wallTime(startedAt)} seconds\nOutput:\n`;
  if (!content.length) return heading;
  return [{ type: "input_text", text: heading }, ...content];
}

function abortableEvaluation(evaluation, signal) {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const onAbort = () => reject(signal.reason ?? new Error(CANCELLATION_MESSAGE));
    const settle = (callback, value) => {
      signal.removeEventListener("abort", onAbort);
      callback(value);
    };
    signal.addEventListener("abort", onAbort, { once: true });
    Promise.resolve(evaluation).then(
      (value) => settle(resolve, value),
      (error) => settle(reject, error),
    );
  });
}

class AsyncSemaphore {
  constructor(permits) {
    this.permits = permits;
    this.waiters = [];
  }

  acquire(signal) {
    if (signal.aborted) return Promise.reject(signal.reason);
    if (this.permits > 0 && this.waiters.length === 0) {
      this.permits -= 1;
      return Promise.resolve(once(() => this.release()));
    }
    return new Promise((resolve, reject) => {
      const waiter = { resolve, reject, signal };
      waiter.abort = () => {
        const index = this.waiters.indexOf(waiter);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(signal.reason ?? new Error(CANCELLATION_MESSAGE));
      };
      signal.addEventListener("abort", waiter.abort, { once: true });
      this.waiters.push(waiter);
    });
  }

  release() {
    while (this.waiters.length) {
      const waiter = this.waiters.shift();
      waiter.signal.removeEventListener("abort", waiter.abort);
      if (waiter.signal.aborted) continue;
      waiter.resolve(once(() => this.release()));
      return;
    }
    this.permits += 1;
  }
}

class AsyncReadWriteGate {
  constructor() {
    this.readers = 0;
    this.writer = false;
    this.waiters = [];
  }

  acquire(parallelSafe, signal) {
    if (signal.aborted) return Promise.reject(signal.reason);
    if (this.canAcquireImmediately(parallelSafe)) {
      return Promise.resolve(this.grant(parallelSafe));
    }
    return new Promise((resolve, reject) => {
      const waiter = { parallelSafe, resolve, reject, signal };
      waiter.abort = () => {
        const index = this.waiters.indexOf(waiter);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(signal.reason ?? new Error(CANCELLATION_MESSAGE));
        this.drain();
      };
      signal.addEventListener("abort", waiter.abort, { once: true });
      this.waiters.push(waiter);
    });
  }

  canAcquireImmediately(parallelSafe) {
    return this.waiters.length === 0
      && !this.writer
      && (parallelSafe || this.readers === 0);
  }

  grant(parallelSafe) {
    if (parallelSafe) this.readers += 1;
    else this.writer = true;
    return once(() => {
      if (parallelSafe) this.readers -= 1;
      else this.writer = false;
      this.drain();
    });
  }

  drain() {
    if (this.writer || this.waiters.length === 0) return;
    const first = this.waiters[0];
    if (!first.parallelSafe) {
      if (this.readers !== 0) return;
      this.waiters.shift();
      first.signal.removeEventListener("abort", first.abort);
      if (first.signal.aborted) {
        first.reject(first.signal.reason ?? new Error(CANCELLATION_MESSAGE));
        this.drain();
      } else {
        first.resolve(this.grant(false));
      }
      return;
    }
    while (!this.writer && this.waiters[0]?.parallelSafe) {
      const waiter = this.waiters.shift();
      waiter.signal.removeEventListener("abort", waiter.abort);
      if (waiter.signal.aborted) {
        waiter.reject(waiter.signal.reason ?? new Error(CANCELLATION_MESSAGE));
      } else {
        waiter.resolve(this.grant(true));
      }
    }
  }
}

function once(callback) {
  let active = true;
  return () => {
    if (!active) return;
    active = false;
    callback();
  };
}

function codeObservationKey(sessionId, callId) {
  return JSON.stringify([sessionId, callId]);
}

function createCodeObservation(sessionId) {
  const queued = [];
  const waiters = [];
  let closed = false;
  return Object.freeze({
    sessionId,
    push(update) {
      if (closed) return;
      const resolve = waiters.shift();
      if (resolve) resolve(update);
      else queued.push(update);
    },
    close() {
      if (closed) return;
      closed = true;
      while (waiters.length) waiters.shift()(null);
    },
    next() {
      if (queued.length) return Promise.resolve(queued.shift());
      if (closed) return Promise.resolve(null);
      return new Promise((resolve) => waiters.push(resolve));
    },
  });
}
