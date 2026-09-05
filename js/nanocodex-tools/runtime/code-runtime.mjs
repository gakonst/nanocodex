import {
  providerSource,
  ToolRouter,
  toolMapSource,
  toolRouterBrand,
  toolRouterRuntime,
} from "./tool-router.mjs";

const CANCELLATION_MESSAGE = "Code Mode execution was cancelled";

export function createCodeRuntime(toolConfiguration = {}, extras = {}) {
  const activeExecutions = new Set();
  const codeObservations = new Map();
  const stores = new Map();
  const ownsRouter = !toolConfiguration?.[toolRouterBrand];
  const router = !ownsRouter
    ? (toolConfiguration[toolRouterRuntime] ?? toolConfiguration)
    : new ToolRouter();
  let nextSourceId = 1;
  let nextCallId = 1;
  const toolByName = new Map();
  const subagentBindingsBySession = new Map();
  const subagentSessions = extras.subagentSessions;

  function addTools(configuration = {}) {
    const added = {};
    for (const [name, tool] of Object.entries(configuration)) {
      if (toolByName.has(name)) {
        throw new Error(`tool is already configured: ${name}`);
      }
      added[name] = tool;
    }
    if (Object.keys(added).length) {
      router.addSource(toolMapSource(`cloud:${String(nextSourceId++).padStart(8, "0")}`, added));
      for (const [name, tool] of Object.entries(added)) toolByName.set(name, tool);
    }
  }
  if (!toolConfiguration?.[toolRouterBrand]) addTools(toolConfiguration);

  function callableDefinitions() {
    return router.definitions();
  }

  function resolveTool(name) {
    return router.resolve(name);
  }

  async function executeTool(name, encodedInput, sessionId = "default", callId = "tool", model = "unknown") {
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
      const tool = resolveTool(name);
      if (!tool) return encodeToolOutput(`unknown application tool: ${name}`, false, null);
      const result = await router.execute(name, input, {
        sessionId,
        parentCallId: "",
        callId,
        model,
        signal: controller.signal,
        subagent: subagentBindingsBySession.get(sessionId)?.descriptor,
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

  async function executeCode(source, sessionId = "default", parentCallId = "exec", model = "unknown", observer) {
    if (typeof model === "function" && observer === undefined) {
      observer = model;
      model = "unknown";
    }
    const startedAt = performance.now();
    const content = [];
    const stored = stores.get(sessionId) || new Map();
    stores.set(sessionId, stored);
    const nestedCalls = [];
    const controller = new AbortController();
    const execution = { callId: parentCallId, controller, sessionId };
    activeExecutions.add(execution);
    let admission;
    try {
      admission = await router.admit(controller.signal);
    } catch (error) {
      activeExecutions.delete(execution);
      return JSON.stringify({
        output: `Script failed\nWall time ${wallTime(startedAt)} seconds\nOutput:\n${errorMessage(error)}`,
        success: false,
        nested_calls: nestedCalls,
      });
    }
    const tools = Object.create(null);
    const availableTools = [...admission.tools.values()];
    const availableDefinitions = admission.definitions.map((definition) => definition.type === "tool_search"
      ? deepFreeze({
          type: "function",
          name: "tool_search",
          description: definition.description,
          strict: false,
          parameters: jsonSnapshot(definition.parameters, "tool_search parameters"),
        })
      : definition);
    const nestedInvocations = [];
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
          result = await admission.invoke(name, input, {
            sessionId,
            parentCallId,
            callId,
            model,
            signal: controller.signal,
            subagent: subagentBindingsBySession.get(sessionId)?.descriptor,
          });
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
      admission.release();
      activeExecutions.delete(execution);
    }
  }

  function executeCodeObserved(source, sessionId = "default", parentCallId = "exec", model = "unknown") {
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
        model,
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
    const binding = subagentBindingsBySession.get(sessionId);
    if (binding !== undefined) {
      subagentSessions?.release?.(sessionId, binding.hostContextRef);
    }
    stores.delete(sessionId);
    subagentBindingsBySession.delete(sessionId);
    router.releaseSession(sessionId);
    closeCodeObservations(sessionId);
  }

  function reset() {
    for (const execution of activeExecutions) {
      execution.controller.abort(new Error(CANCELLATION_MESSAGE));
    }
    stores.clear();
    subagentBindingsBySession.clear();
    closeCodeObservations();
    return ownsRouter ? router.reset() : undefined;
  }

  return Object.freeze({
    addTools,
    addProvider(provider, options = {}) {
      if (!provider || typeof provider.definitions !== "function" || typeof provider.resolve !== "function") {
        throw new TypeError("a Code Mode tool provider requires definitions() and resolve(name)");
      }
      const sourceId = options.id ?? provider.sourceId ?? `provider:${String(nextSourceId++).padStart(8, "0")}`;
      router.addSource(providerSource(
        sourceId,
        provider,
        options,
      ));
      return sourceId;
    },
    validateProviderDefinitions(sourceId, candidateDefinitions, options = {}) {
      const definitions = jsonSnapshot(candidateDefinitions, `tool source ${sourceId} candidate definitions`);
      return router.validateSource({
        id: sourceId,
        kind: options.kind ?? "attached",
        mode: options.mode ?? "attached-over-cloud",
        deferred: options.deferred ?? true,
        definitions: () => definitions,
        resolve: (name) => ({ name, parallelSafe: false, handler() {} }),
      });
    },
    router,
    executeCode,
    executeCodeObserved,
    executeTool,
    bindSubagentSession(sessionId, context, hostContextRef) {
      if (hostContextRef !== undefined
        && (typeof hostContextRef !== "string" || hostContextRef.length === 0)) {
        throw new TypeError("subagent host context ref must be a non-empty string when supplied");
      }
      const descriptor = Object.freeze({
        agentId: context.agentId,
        parentAgentId: context.parentAgentId,
        sessionId: context.sessionId,
        role: context.role,
        task: context.task,
      });
      const existing = subagentBindingsBySession.get(sessionId);
      if (sameSubagentBinding(existing, descriptor, hostContextRef)) return;
      subagentSessions?.bind?.(sessionId, descriptor, hostContextRef);
      subagentBindingsBySession.set(sessionId, Object.freeze({
        descriptor,
        hostContextRef,
      }));
    },
    nextCodeUpdate,
    cancel,
    toolDefinitions: () => JSON.stringify(callableDefinitions()),
    releaseSession,
    reset,
  });
}

function sameSubagentBinding(binding, descriptor, hostContextRef) {
  const left = binding?.descriptor;
  return left !== undefined
    && left.agentId === descriptor.agentId
    && left.parentAgentId === descriptor.parentAgentId
    && left.sessionId === descriptor.sessionId
    && left.role === descriptor.role
    && left.task === descriptor.task
    && binding.hostContextRef === hostContextRef;
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

const TOOL_RESULT = Symbol.for("nanocodex.toolResult");

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
