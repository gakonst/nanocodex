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
  const cells = new Map();
  const turns = new Map();
  const cellGeneration = globalThis.crypto.randomUUID();
  let nextCellId = 1;
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
    const execution = { callId, controller, sessionId, turn: turns.get(sessionId) ?? 0 };
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

  async function executeCode(source, sessionId = "default", parentCallId = "exec", model = "unknown", observer, cell) {
    if (typeof model === "function" && observer === undefined) {
      observer = model;
      model = "unknown";
    }
    const startedAt = performance.now();
    const content = cell?.content ?? [];
    const stored = stores.get(sessionId) || new Map();
    stores.set(sessionId, stored);
    const nestedCalls = [];
    const controller = cell?.controller ?? new AbortController();
    const execution = { callId: parentCallId, controller, sessionId, cell, turn: turns.get(sessionId) ?? 0 };
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
    const normalized = availableTools.map(({ name }) => normalizeIdentifier(name));
    if (new Set(normalized).size !== normalized.length) {
      admission.release();
      activeExecutions.delete(execution);
      return JSON.stringify({ output: "Script failed\nOutput:\nCode Mode tool names collide after normalization", success: false, nested_calls: [] });
    }
    for (const { name } of availableTools) {
      const normalizedName = normalizeIdentifier(name);
      tools[normalizedName] = (input) => {
        const invocation = executeNestedTool(input);
        // Attach a rejection handler immediately so a discarded guest Promise
        // cannot become an unhandled rejection before the cell reaches its
        // quiescence boundary.
        nestedInvocations.push(invocation.then(() => undefined, () => undefined));
        return invocation;
      };
      // Preserve existing SDK bracket access while advertising Codex's
      // normalized identifiers to newly generated cells.
      if (name !== normalizedName) tools[name] = tools[normalizedName];

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
      controller.signal.throwIfAborted();
      content.push({ type: "input_text", text: stringify(value) });
    }
    function image(value, detail) {
      controller.signal.throwIfAborted();
      const url = typeof value === "string" ? value
        : value?.type === "image" ? `data:${value.mimeType};base64,${value.data}`
        : value?.image_url;
      if (typeof url !== "string" || !url.startsWith("data:image/")) {
        throw new TypeError("image() requires a base64 data URL or MCP image block");
      }
      const selected = detail ?? value?._meta?.["codex/imageDetail"] ?? value?.detail ?? "auto";
      if (!["auto", "low", "high", "original"].includes(selected)) throw new TypeError("invalid image detail");
      content.push({ type: "input_image", image_url: url, detail: selected });
    }
    function audio(value) {
      controller.signal.throwIfAborted();
      const url = typeof value === "string" ? value
        : value?.type === "audio" ? `data:${value.mimeType};base64,${value.data}`
        : value?.audio_url;
      if (typeof url !== "string" || !url.startsWith("data:audio/")) {
        throw new TypeError("audio() requires a base64 data URL or MCP audio block");
      }
      content.push({ type: "input_audio", audio_url: url });
    }
    function generatedImage(result) {
      if (!result || typeof result !== "object" || typeof result.image_url !== "string") {
        throw new TypeError("generatedImage() requires an image generation result");
      }
      image(result.image_url, "high");
      if (typeof result.output_hint === "string" && result.output_hint) text(result.output_hint);
    }
    function notify(value) {
      controller.signal.throwIfAborted();
      if (cell) {
        cell.notifications.push({ call_id: parentCallId, text: stringify(value) });
        cell.wake?.();
      } else text(value);
    }
    function yield_control() {
      if (cell) { cell.yieldRequested = true; cell.wake?.(); }
    }
    const timers = new Map();
    let nextTimer = 1;
    function schedule(callback, delay = 0) {
      controller.signal.throwIfAborted();
      const id = nextTimer++;
      const timer = setTimeout(() => {
        timers.delete(id);
        if (!controller.signal.aborted) {
          try { Promise.resolve(callback()).catch((error) => controller.abort(error)); }
          catch (error) { controller.abort(error); }
        }
      }, delay);
      timers.set(id, timer);
      return id;
    }
    function unschedule(id) {
      clearTimeout(timers.get(id));
      timers.delete(id);
    }
    function store(key, value) {
      controller.signal.throwIfAborted();
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
              audio,
              notify,
              yield_control,
              setTimeout: schedule,
              clearTimeout: unschedule,
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
      for (const timer of timers.values()) clearTimeout(timer);
      admission.release();
      activeExecutions.delete(execution);
    }
  }

  function executeCodeObserved(source, sessionId = "default", parentCallId = "exec", model = "unknown") {
    return observeOperation(sessionId, parentCallId, (observation) => {
      const options = parseExec(source);
      const cell = {
        id: `${cellGeneration}:${nextCellId++}`, sessionId, controller: new AbortController(),
        content: [], updates: [], completedCalls: [], notifications: [], turn: turns.get(sessionId) ?? 0,
        budget: options.max_output_tokens ?? 10_000, result: undefined, observing: false,
      };
      cells.set(cell.id, cell);
      cell.completion = executeCode(options.source, sessionId, parentCallId, model, (update) => {
        // Keep queued completions immutable; the invocation record is mutable
        // until the nested call finishes. Original call IDs survive every wait.
        const encoded = JSON.stringify(update);
        if (cell.observation) cell.observation.push(encoded);
        else cell.updates.push(encoded);
        if (update.type === "nested_call_completed") cell.completedCalls.push(update.call);
      }, cell).then((result) => {
        const completed = JSON.parse(result);
        if (!completed.success && typeof completed.output === "string") {
          cell.content.push({ type: "input_text", text: completed.output.split("Output:\n").slice(1).join("Output:\n") || completed.output });
        }
        cell.result = { success: completed.success };
        cell.wake?.();
      }, (error) => {
        cell.content.push({ type: "input_text", text: errorMessage(error) });
        cell.result = { success: false };
        cell.wake?.();
      });
      return observeCell(cell, observation, options.yield_time_ms ?? 10_000, cell.budget);
    });
  }

  function waitCodeObserved(input, sessionId = "default", callId = "wait") {
    return observeOperation(sessionId, callId, (observation) => {
      const options = parseCellOptions(JSON.parse(input), ["cell_id", "yield_time_ms", "max_tokens", "terminate"]);
      if (typeof options.cell_id !== "string") throw new TypeError("wait requires a string cell_id");
      if (options.terminate !== undefined && typeof options.terminate !== "boolean") throw new TypeError("terminate must be boolean");
      const cell = cells.get(options.cell_id);
      if (!cell || cell.sessionId !== sessionId) throw new Error(`exec cell ${options.cell_id} not found`);
      if (cell.observing) throw new Error(`exec cell ${cell.id} already has an active observer`);
      cell.turn = turns.get(sessionId) ?? 0;
      if (options.terminate) {
        cell.terminated = true;
        cell.controller.abort(new Error(CANCELLATION_MESSAGE));
      }
      return observeCell(cell, observation, options.yield_time_ms ?? 10_000, options.max_tokens ?? cell.budget);
    });
  }

  function observeOperation(sessionId, callId, operation) {
    const key = codeObservationKey(sessionId, callId);
    const observation = createCodeObservation(sessionId, turns.get(sessionId) ?? 0);
    codeObservations.get(key)?.close();
    codeObservations.set(key, observation);
    return Promise.resolve().then(() => operation(observation)).catch((error) => JSON.stringify({
      output: `Script failed\nOutput:\n${errorMessage(error)}`, success: false, nested_calls: [],
    })).finally(() => observation.close());
  }

  async function observeCell(cell, observation, yieldTime, budget) {
    const startedAt = performance.now();
    cell.observing = true;
    cell.observation = observation;
    for (const update of cell.updates.splice(0)) observation.push(update);
    let timer;
    try {
      if (cell.terminated) await cell.completion;
      else if (!cell.result && !cell.yieldRequested && cell.notifications.length === 0) {
        await new Promise((resolve) => {
          cell.wake = resolve;
          // JS timer APIs overflow past this boundary; clamp instead of
          // accidentally turning a large valid duration into a 1 ms wait.
          timer = setTimeout(resolve, Math.min(yieldTime, 2_147_483_647));
        });
      }
      cell.yieldRequested = false;
      const result = cell.result;
      const status = cell.terminated ? "Script terminated"
        : result ? (result.success ? "Script completed" : "Script failed")
        : `Script running with cell ID ${cell.id}`;
      const output = withStatus(status, startedAt, cell.content.splice(0));
      if (result) cells.delete(cell.id);
      let limited = limitCodeOutput(output, Math.max(1, budget));
      if (result?.success === false && Array.isArray(limited) && limited.every((item) => item.type === "input_text")) {
        limited = limited.map((item) => item.text).join("");
      }
      return JSON.stringify({
        output: limited,
        success: cell.terminated || (result?.success ?? true),
        nested_calls: cell.completedCalls.splice(0),
        notifications: cell.notifications.splice(0),
      });
    } finally {
      clearTimeout(timer);
      cell.wake = undefined;
      cell.observation = undefined;
      cell.observing = false;
    }
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

  function closeCodeObservations(sessionId, turn) {
    for (const [key, observation] of codeObservations) {
      if (sessionId !== undefined && observation.sessionId !== sessionId) continue;
      if (turn !== undefined && observation.turn !== turn) continue;
      codeObservations.delete(key);
      observation.close();
    }
  }

  function cancel(sessionId, turn) {
    for (const execution of activeExecutions) {
      if ((sessionId === undefined || execution.sessionId === sessionId)
        && (turn === undefined || (execution.cell?.turn ?? execution.turn) === turn)) {
        execution.controller.abort(new Error(CANCELLATION_MESSAGE));
      }
    }
    for (const [id, cell] of cells) {
      if ((sessionId === undefined || cell.sessionId === sessionId)
        && (turn === undefined || cell.turn === turn)) cells.delete(id);
    }
    closeCodeObservations(sessionId, turn);
  }

  function releaseSession(sessionId) {
    cancel(sessionId);
    const binding = subagentBindingsBySession.get(sessionId);
    if (binding !== undefined) {
      subagentSessions?.release?.(sessionId, binding.hostContextRef);
    }
    turns.delete(sessionId);
    stores.delete(sessionId);
    subagentBindingsBySession.delete(sessionId);
    router.releaseSession(sessionId);
    closeCodeObservations(sessionId);
  }

  function reset() {
    for (const execution of activeExecutions) {
      execution.controller.abort(new Error(CANCELLATION_MESSAGE));
    }
    cells.clear();
    turns.clear();
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
    waitCodeObserved,
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
    beginTurn(sessionId) { turns.set(sessionId, (turns.get(sessionId) ?? 0) + 1); },
    cancelTurn(sessionId) { cancel(sessionId, turns.get(sessionId) ?? 0); },
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
    "audio",
    "notify",
    "yield_control",
    "setTimeout",
    "clearTimeout",
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
    environment.audio,
    environment.notify,
    environment.yield_control,
    environment.setTimeout,
    environment.clearTimeout,
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

function createCodeObservation(sessionId, turn) {
  const queued = [];
  const waiters = [];
  let closed = false;
  return Object.freeze({
    sessionId,
    turn,
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

function normalizeIdentifier(name) {
  return [...name].map((character, index) => (index === 0 ? /[A-Za-z_$]/ : /[A-Za-z0-9_$]/).test(character) ? character : "_").join("") || "_";
}

function parseExec(source) {
  const [line] = source.split(/\r?\n/, 1);
  if (!line.trimStart().startsWith("// @exec:")) return { source };
  const rest = source.slice(line.length).replace(/^\r?\n/, "");
  if (!rest) throw new TypeError("exec pragma must be followed by JavaScript source on subsequent lines");
  return { ...parseCellOptions(JSON.parse(line.trimStart().slice("// @exec:".length)), ["yield_time_ms", "max_output_tokens"]), source: rest };
}

function parseCellOptions(value, allowed) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("cell options must be a JSON object");
  for (const [key, field] of Object.entries(value)) {
    if (!allowed.includes(key)) throw new TypeError(`unknown cell option: ${key}`);
    if (key !== "cell_id" && key !== "terminate" && (!Number.isSafeInteger(field) || field < 0)) {
      throw new TypeError(`${key} must be a non-negative safe integer`);
    }
  }
  return value;
}

function limitCodeOutput(output, budget) {
  let remaining = budget * 4;
  const limit = (text) => {
    const bytes = new TextEncoder().encode(text);
    if (bytes.length <= remaining) { remaining -= bytes.length; return text; }
    const half = Math.floor(remaining / 2);
    remaining = 0;
    // Decode complete code points only, including for non-ASCII tool output.
    const decoder = new TextDecoder("utf-8", { fatal: true });
    const slice = (start, end) => {
      for (;;) {
        try { return decoder.decode(bytes.subarray(start, end)); }
        catch { if (start === 0) end--; else start++; }
      }
    };
    return `${slice(0, half)}…output truncated…${half ? slice(bytes.length - half, bytes.length) : ""}`;
  };
  if (typeof output === "string") {
    const split = output.indexOf("Output:\n");
    return split < 0 ? output : output.slice(0, split + 8) + limit(output.slice(split + 8));
  }
  return output.map((item, index) =>
    item.type === "input_text" ? { ...item, text: index === 0 ? item.text : limit(item.text) } : item);
}
