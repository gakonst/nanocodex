import { createCodeTools } from "./code-tools.mjs";
import { stringify, storeSnapshot, normalizeImage, normalizeAudio, generatedImageItems } from "./code-values.mjs";
import { limitCodeOutput } from "./code-output.mjs";
import {
  providerSource,
  ToolRouter,
  toolMapSource,
  toolRouterBrand,
  toolRouterRuntime,
} from "./tool-router.mjs";

const CANCELLATION_MESSAGE = "Code Mode execution was cancelled";

// Trusted instrumentation must preserve the handler's outcome even if span
// setup/annotation fails, returns another value, or invokes its callback twice.
export async function traceToolInvocation(traceTool, name, context, run) {
  let runPromise;
  const invoke = () => {
    if (runPromise === undefined) {
      let resolveRun, rejectRun;
      runPromise = new Promise((resolve, reject) => { resolveRun = resolve; rejectRun = reject; });
      // A broken hook may discard this promise before its own setup settles.
      void runPromise.catch(() => undefined);
      // Code Mode registers its observation queue before Rust reads it. Invoke
      // synchronously while assigning the memoized promise before any reentry.
      try { resolveRun(run()); }
      catch (error) { rejectRun(error); }
    }
    return runPromise;
  };
  try {
    const tracing = traceTool(name, context, invoke);
    // Also preserve synchronous host setup when a hook postpones its callback.
    invoke();
    if (tracing?.then) void Promise.resolve(tracing).catch(() => undefined);
  }
  catch { /* Instrumentation cannot replace a real tool failure or result. */ }
  return invoke();
}

export function createCodeRuntime(toolConfiguration = {}, extras = {}) {
  const traceTool = extras.traceTool;
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

  async function executeTool(name, encodedInput, sessionId = "default", callId = "tool", model = "unknown", turnId) {
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
      if (extras.effectJournal) return await executeJournalledTool(name, input, execution, model, turnId);
      const context = {
        sessionId,
        parentCallId: "",
        callId,
        model,
        ...(turnId == null ? {} : { turnId }),
        signal: controller.signal,
        subagent: subagentBindingsBySession.get(sessionId)?.descriptor,
      };
      const result = await (traceTool === undefined
        ? router.execute(name, input, context)
        : traceToolInvocation(traceTool, name, {
          sessionId, callId, ...(turnId == null ? {} : { turnId }),
        }, () => router.execute(name, input, context)));
      return encodeToolOutput(
        outputBody(result),
        toolSucceeded(result),
        structuredResult(result, `tool ${name} result`),
        toolMetadata(result, `tool ${name} metadata`),
      );
    } catch (error) {
      if (error?.code === "host_interrupted") throw error;
      return encodeToolOutput(errorMessage(error), false, error?.code === "CODE_EFFECT_UNKNOWN"
        ? { error: errorMessage(error), code: error.code, outcome: "unknown" } : null);
    } finally {
      activeExecutions.delete(execution);
    }
  }

  async function executeJournalledTool(name, input, execution, model, turnId) {
    const { sessionId, callId, controller } = execution;
    const journal = extras.effectJournal;
    function interrupt(cause) {
      if (cause?.code === "CODE_EFFECT_UNKNOWN") throw cause;
      const error = Object.assign(new Error("Application tool effect journal interrupted", { cause }),
        { code: "host_interrupted" });
      execution.interruption = error;
      controller.abort(error);
      throw error;
    }
    let recordedInput;
    try { recordedInput = boundedEffectSnapshot(input, "direct effect input"); }
    catch (cause) { interrupt(cause); }
    let identity;
    try { identity = await extras.effectIdentity?.(sessionId, callId, turnId, controller.signal) ?? {}; }
    catch (cause) { interrupt(cause); }
    const effectContext = { ...identity, sessionId, parentCallId: callId, callId, name,
      source: "host-tool:" + name, input: recordedInput, ...(turnId == null ? {} : { turnId }) };
    controller.signal.throwIfAborted();
    let decision;
    try { decision = await journal.begin(effectContext); }
    catch (cause) { interrupt(cause); }
    controller.signal.throwIfAborted();
    if (decision?.status === "unknown") {
      const message = "Application tool effect has a retained dispatch intent but no completed receipt; outcome unknown. Reconcile the original operation before retrying.";
      return encodeToolOutput(message, false, { error: message, code: "TOOL_CALL_INTERRUPTED", outcome: "unknown" });
    }
    if (decision?.status === "replay") {
      let receipt;
      try {
        receipt = boundedEffectSnapshot(decision.receipt, "direct effect replay");
        if (!receipt || Array.isArray(receipt) || typeof receipt.success !== "boolean"
          || receipt.thrown !== false || receipt.value !== null
          || ["output", "structured_result", "metadata", "value"].some(key => !Object.hasOwn(receipt, key))
          || receipt.valueRef !== undefined || receipt.valueUndefined !== undefined
          || (receipt.structuredResultRef !== undefined && receipt.structuredResultRef !== "output")
          || (receipt.outputJsonRef !== undefined && receipt.outputJsonRef !== "structured_result")
          || (receipt.outputJsonRef !== undefined && receipt.structuredResultRef !== undefined)) {
          throw new Error("invalid direct effect receipt");
        }
        if (receipt.outputJsonRef === "structured_result") receipt.output = JSON.stringify(receipt.structured_result);
        if (receipt.structuredResultRef === "output") receipt.structured_result = receipt.output;
      } catch (cause) { interrupt(effectUnknown(cause)); }
      return encodeToolOutput(receipt.output, receipt.success, receipt.structured_result, receipt.metadata);
    }
    if (decision?.status !== "execute") interrupt(effectUnknown(new Error("invalid direct effect admission")));
    let result, receipt;
    try {
      controller.signal.throwIfAborted();
      const context = {
        sessionId, parentCallId: callId, callId, model,
        ...(turnId == null ? {} : { turnId }), signal: controller.signal,
        subagent: subagentBindingsBySession.get(sessionId)?.descriptor,
      };
      result = await (traceTool === undefined
        ? router.execute(name, input, context)
        : traceToolInvocation(traceTool, name, {
          sessionId, callId, parentCallId: callId, ...(turnId == null ? {} : { turnId }),
        }, () => router.execute(name, input, context)));
    } catch (error) {
      if (error?.code === "host_interrupted") interrupt(error);
      // An abort is not proof that the dispatched operation did not write.
      // Leave its retained intent pending, even if the handler rejects or ignores abort.
      if (controller.signal.aborted) throw error;
      receipt = { output: errorMessage(error), success: false, structured_result: null,
        metadata: null, value: null, thrown: false };
    }
    controller.signal.throwIfAborted();
    let wire;
    try {
      if (!receipt) {
        // No guest value is exposed by executeTool. Bound unique output/structured
        // payloads BEFORE outputBody or structuredResult clone/stringify them.
        const raw = isToolResult(result) ? { output: result.output, metadata: result.metadata } : { result };
        if (isToolResult(result) && result.structuredResult !== result.output) raw.structured_result = result.structuredResult;
        boundedEffectSnapshot(raw, "direct effect result");
        receipt = { output: outputBody(result), success: toolSucceeded(result),
          structured_result: structuredResult(result, `tool ${name} result`),
          metadata: toolMetadata(result, `tool ${name} metadata`), value: null, thrown: false };
      }
      wire = { ...receipt };
      const rawOutput = isToolResult(result) ? result.output : result;
      if (result !== undefined && typeof receipt.output === "string" && typeof rawOutput !== "string"
        && (!isToolResult(result) || result.output === result.structuredResult)) {
        wire.output = null; wire.outputJsonRef = "structured_result";
      } else if (receipt.structured_result === receipt.output
        || (Array.isArray(receipt.output) && (!isToolResult(result) || result.output === result.structuredResult))) {
        wire.structured_result = null; wire.structuredResultRef = "output";
      }
      wire = boundedEffectSnapshot(wire, "direct effect receipt");
    } catch (cause) { interrupt(cause); }
    controller.signal.throwIfAborted();
    try { await journal.complete(effectContext, wire); }
    catch (cause) { interrupt(cause); }
    controller.signal.throwIfAborted();
    return encodeToolOutput(receipt.output, receipt.success, receipt.structured_result, receipt.metadata);
  }

  async function executeCode(source, sessionId = "default", parentCallId = "exec", model = "unknown", observer, cell, turnId, localDefinitions, executeLocalTool) {
    if (typeof model === "function" && observer === undefined) {
      observer = model;
      model = "unknown";
    }
    const startedAt = performance.now();
    const content = cell?.content ?? [];
    const sessionStore = stores.get(sessionId) || new Map();
    stores.set(sessionId, sessionStore);
    const stored = new Map([...sessionStore].map(([key, value]) => [key, jsonSnapshot(value, "stored value")]));
    const storedWrites = new Map();
    const nestedCalls = [];
    const notifications = [];
    const controller = cell?.controller ?? new AbortController();
    const execution = { callId: parentCallId, controller, sessionId, cell, turn: turns.get(sessionId) ?? 0 };
    activeExecutions.add(execution);
    let finished = false;
    let admission;
    try {
      admission = await router.admit(controller.signal);
      if (localDefinitions && localDefinitions !== "[]") {
        const configured = Object.fromEntries(JSON.parse(localDefinitions).map(definition => [definition.name, {
          definition,
          async handler(input, context) {
            context.signal.throwIfAborted();
            const pending = executeLocalTool(definition.name, JSON.stringify(input), context.callId);
            const cancel = () => pending.cancel?.();
            context.signal.addEventListener("abort", cancel, { once: true });
            try {
              if (context.signal.aborted) cancel();
              const receipt = JSON.parse(await pending);
              return toolResult(receipt.output, receipt.structured_result, {
                success: receipt.success, metadata: receipt.metadata,
                value: receipt.structured_result ?? receipt.output,
              });
            } finally {
              context.signal.removeEventListener("abort", cancel);
            }
          },
        }]));
        const local = new ToolRouter([toolMapSource("rust-cell", configured)]).snapshot();
        const application = admission;
        admission = {
          definitions: [...application.definitions, ...local.definitions],
          tools: new Map([...application.tools, ...local.tools]),
          invoke: (name, input, context) => (local.tools.has(name) ? local : application).invoke(name, input, context),
          release() { local.release(); application.release(); },
        };
        if (new Set(admission.definitions.map(definition => normalizeIdentifier(definition.type === "tool_search" ? "tool_search" : definition.name))).size !== admission.definitions.length) {
          throw new Error("Code Mode local tool names collide after normalization");
        }
      }
    } catch (error) {
      admission?.release();
      activeExecutions.delete(execution);
      return JSON.stringify({
        output: `Script failed\nWall time ${wallTime(startedAt)} seconds\nOutput:\n${errorMessage(error)}`,
        success: false,
        nested_calls: nestedCalls,
      });
    }
    const declaredTools = Object.create(null);
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
    const pendingCalls = new Map();
    // A journalled replay must address the same effects regardless of earlier cells.
    let nextJournalCallId = 1;
    const journal = extras.effectJournal;
    let canonicalIdentity;
    let cellContext;
    function journalFailure(cause) {
      const error = cause?.code === "CODE_EFFECT_UNKNOWN" ? cause
        : Object.assign(new Error("Code Mode effect journal interrupted", { cause }), { code: "host_interrupted" });
      if (error.code === "host_interrupted") execution.interruption = error;
      else execution.recoveryFailure = error;
      controller.abort(error);
      throw error;
    }
    function closePendingCalls() {
      // Guest completion still ends the cell immediately, as in Codex. Host
      // receipts outlive guest promises: every observed start needs a terminal
      // result even when an invocation ignores cancellation or settles later.
      for (const finish of pendingCalls.values()) finish({
        output: "Code Mode cell ended before the tool returned; execution outcome unknown",
        structured_result: {
          error: "Code Mode cell ended before the tool returned; execution outcome unknown",
          code: "CODE_MODE_CALL_INTERRUPTED",
          outcome: "unknown",
        },
        success: false,
      });
    }
    const normalized = availableTools.map(({ name }) => normalizeIdentifier(name));
    if (new Set(normalized).size !== normalized.length) {
      admission.release();
      activeExecutions.delete(execution);
      return JSON.stringify({ output: "Script failed\nOutput:\nCode Mode tool names collide after normalization", success: false, nested_calls: [] });
    }
    for (const { name } of availableTools) {
      const normalizedName = normalizeIdentifier(name);
      declaredTools[normalizedName] = (input) => {
        const invocation = executeNestedTool(input);
        // Attach a rejection handler immediately so a discarded guest Promise
        // cannot become an unhandled rejection before the cell reaches its
        // quiescence boundary.
        void invocation.catch(() => undefined);
        return invocation;
      };
      // Preserve existing SDK bracket access while advertising Codex's
      // normalized identifiers to newly generated cells.
      if (name !== normalizedName) declaredTools[name] = declaredTools[normalizedName];

      async function executeNestedTool(input) {
        // A native guest continuation can survive its cell if a host promise
        // ignores abort. Reject before creating telemetry for a closed cell.
        controller.signal.throwIfAborted();
        if (finished) throw new Error(CANCELLATION_MESSAGE);
        const callId = `${parentCallId}/code-${journal ? nextJournalCallId++ : nextCallId++}`;
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
        function complete(fields) {
          if (!pendingCalls.delete(callId)) return;
          Object.assign(recordedCall, fields, { duration_ns: elapsedNs(toolStartedAt) });
          observer?.({ type: "nested_call_completed", call: recordedCall });
        }
        pendingCalls.set(callId, complete);
        if (!finished) observer?.({
          type: "nested_call_started",
          call_id: callId,
          name,
          input: recordedInput,
        });
        let resolvedIdentity;
        try {
          canonicalIdentity ??= Promise.resolve(journal ? extras.effectIdentity?.(sessionId, parentCallId, turnId, controller.signal) ?? {} : {});
          resolvedIdentity = await canonicalIdentity;
          controller.signal.throwIfAborted();
        }
        catch (cause) { interrupt(cause); }
        const effectContext = { ...resolvedIdentity, sessionId, parentCallId, callId, name, source, input: recordedInput,
          ...(turnId == null ? {} : { turnId }) };
        // Never resolve a guest promise before its durable outcome is acknowledged.
        async function retain(receipt, valueRef, outputJsonRef) {
          if (!journal) return;
          try {
            // JSON receipts need not repeat identical output/structured/value data.
            // Keep references inside the receipt, never to mutable external state.
            const wire = { ...receipt };
            if (outputJsonRef) { wire.output = null; wire.outputJsonRef = outputJsonRef; }
            if (receipt.structured_result === receipt.output) {
              wire.structured_result = null;
              wire.structuredResultRef = "output";
            }
            if (valueRef) { wire.value = null; wire.valueRef = valueRef; }
            await journal.complete(effectContext, boundedEffectSnapshot(wire, "nested effect receipt"));
          }
          catch (error) { interrupt(error); }
        }
        function interrupt(cause) { journalFailure(cause); }
        let decision;
        if (journal) {
          try { decision = await journal.begin(effectContext); }
          catch (error) { interrupt(error); }
          if (decision?.status === "unknown") {
            const message = "Code Mode nested effect has a retained dispatch intent but no completed receipt; outcome unknown. Reconcile the original operation before retrying.";
            const error = Object.assign(new Error(message), { code: "CODE_MODE_CALL_INTERRUPTED", outcome: "unknown" });
            complete({ output: message, structured_result: { error: message, code: error.code, outcome: "unknown" }, success: false });
            // Guest catch blocks cannot turn an uncertain write into a new retry.
            execution.recoveryFailure = error;
            controller.abort(error);
            throw error;
          }
          if (decision?.status === "replay") {
            let receipt;
            try {
              receipt = boundedEffectSnapshot(decision.receipt, "nested effect replay");
              if (!receipt || Array.isArray(receipt)
                || typeof receipt.success !== "boolean" || typeof receipt.thrown !== "boolean"
                || ["output", "structured_result", "metadata", "value"].some(key => !Object.hasOwn(receipt, key))
                || (receipt.valueUndefined !== undefined && typeof receipt.valueUndefined !== "boolean")
                || (receipt.structuredResultRef !== undefined && receipt.structuredResultRef !== "output")
                || (receipt.outputJsonRef !== undefined && receipt.outputJsonRef !== "structured_result")
                || (receipt.outputJsonRef !== undefined && receipt.structuredResultRef !== undefined)
                || (receipt.valueRef !== undefined && !["output", "structured_result"].includes(receipt.valueRef))
                || (receipt.thrown && (receipt.success || !validEffectFailure(receipt.failure)))) {
                throw new Error("invalid nested effect receipt");
              }
            } catch (cause) { interrupt(effectUnknown(cause)); }
            if (receipt.outputJsonRef === "structured_result") receipt.output = JSON.stringify(receipt.structured_result);
            if (receipt.structuredResultRef === "output") receipt.structured_result = receipt.output;
            if (receipt.valueRef) receipt.value = receipt[receipt.valueRef];
            complete({ output: receipt.output, structured_result: receipt.structured_result,
              success: receipt.success, metadata: receipt.metadata });
            if (receipt.thrown) throw restoreEffectFailure(receipt.failure);
            const value = receipt.valueUndefined ? undefined : receipt.value;
            if (!receipt.success) throw value;
            return value;
          }
          if (decision?.status !== "execute") interrupt(effectUnknown(new Error("invalid nested effect admission")));
        }
        function retainedFailure(error) {
          try { return effectFailure(error); }
          catch (cause) { interrupt(cause); }
        }
        let result;
        try {
          controller.signal.throwIfAborted();
          const context = {
            sessionId,
            parentCallId,
            callId,
            model,
            ...(turnId == null ? {} : { turnId }),
            signal: controller.signal,
            subagent: subagentBindingsBySession.get(sessionId)?.descriptor,
          };
          result = await (traceTool === undefined
            ? admission.invoke(name, input, context)
            : traceToolInvocation(traceTool, name, {
              sessionId, callId, parentCallId, ...(turnId == null ? {} : { turnId }),
            }, () => admission.invoke(name, input, context)));
        } catch (error) {
          if (error?.code === "host_interrupted") {
            execution.interruption = error;
            controller.abort(error);
            throw error;
          }
          // Cancellation is not evidence that an already dispatched effect failed
          // before writing. Retain the pending intent, never a replayable failure
          // that a guest catch block could use to retry the cancelled operation.
          if (controller.signal.aborted) throw error;
          const message = errorMessage(error);
          const receipt = { output: message, structured_result: message, success: false,
            metadata: null, thrown: true, failure: journal ? retainedFailure(error) : null, value: null };
          await retain(receipt);
          complete({ output: receipt.output, structured_result: receipt.structured_result,
              success: receipt.success, metadata: receipt.metadata });
          throw error;
        }
        // Bound host data before outputBody/structuredResult clone or stringify it.
        // A write already happened: an unretainable receipt leaves its intent unknown,
        // not a catchable formatting error that could authorize another dispatch.
        if (journal) {
          try {
            // Before cloning/formatting, bound each unique original payload;
            // ToolResult commonly aliases all three fields to one media object.
            const raw = isToolResult(result) ? { output: result.output, metadata: result.metadata } : { result };
            if (isToolResult(result)) {
              if (result.structuredResult !== result.output) raw.structured_result = result.structuredResult;
              if (result.value !== result.output && result.value !== result.structuredResult) raw.value = result.value;
            }
            boundedEffectSnapshot(raw, "nested effect result");
          } catch (cause) { interrupt(cause); }
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
          const receipt = { output: message, structured_result: message, success: false,
            metadata: null, thrown: true, failure: journal ? retainedFailure(error) : null, value: null };
          await retain(receipt);
          complete({ output: receipt.output, structured_result: receipt.structured_result,
              success: receipt.success, metadata: receipt.metadata });
          throw error;
        }
        const receipt = { output, structured_result: structured, success, metadata,
          thrown: false, value: toolValue(result) ?? null,
          ...(toolValue(result) === undefined ? { valueUndefined: true } : {}) };
        const valueRef = toolValue(result) === undefined ? undefined
          : !isToolResult(result) || result.value === result.structuredResult ? "structured_result"
          : result.value === output ? "output" : undefined;
        const rawOutput = isToolResult(result) ? result.output : result;
        const outputJsonRef = typeof output === "string" && rawOutput !== undefined && typeof rawOutput !== "string"
          && (!isToolResult(result) || result.output === result.structuredResult) ? "structured_result" : undefined;
        await retain(receipt, valueRef, outputJsonRef);
        complete({ output: receipt.output, structured_result: receipt.structured_result,
              success: receipt.success, metadata: receipt.metadata });
        if (!success) throw toolValue(result);
        return toolValue(result);
      }
    }
    const tools = createCodeTools(Object.keys(declaredTools), (name, input) => declaredTools[name](input));
    const EXIT = Symbol("exit");

    function text(value) {
      controller.signal.throwIfAborted();
      content.push({ type: "input_text", text: stringify(value) });
    }
    function image(value, detail) {
      controller.signal.throwIfAborted();
      content.push(normalizeImage(value, detail));
    }
    function audio(value) {
      controller.signal.throwIfAborted();
      content.push(normalizeAudio(value));
    }
    function generatedImage(value) {
      controller.signal.throwIfAborted();
      content.push(...generatedImageItems(value));
    }
    function notify(value) {
      controller.signal.throwIfAborted();
      const notification = stringify(value);
      if (!notification.trim()) throw new TypeError("notify expects non-empty text");
      observer?.({ type: "notification", call_id: parentCallId, text: notification });
      extras.notify?.({ sessionId, callId: parentCallId, text: notification });
      if (!observer && !extras.notify) notifications.push({ call_id: parentCallId, text: notification });
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
      const entry = storeSnapshot(key, value);
      key = entry[0];
      const snapshot = entry[1];
      stored.set(key, snapshot);
      storedWrites.set(key, snapshot);
    }
    function load(key) {
      controller.signal.throwIfAborted();
      key = `${key}`;
      return stored.has(key) ? jsonSnapshot(stored.get(key), "stored value") : undefined;
    }
    function exit() {
      throw EXIT;
    }

    try {
      if (journal?.beginCell || journal?.commitStore) {
        try {
          if (!journal.beginCell || !journal.commitStore) throw effectUnknown(new Error("incomplete durable cell store protocol"));
          canonicalIdentity ??= Promise.resolve(extras.effectIdentity?.(sessionId, parentCallId, turnId, controller.signal) ?? {});
          cellContext = { ...await canonicalIdentity, sessionId, parentCallId, callId: parentCallId,
            name: "code-cell", source, input: null, ...(turnId == null ? {} : { turnId }) };
          const entries = await journal.beginCell(cellContext);
          let snapshot;
          try {
            snapshot = boundedEffectSnapshot(entries, "cell starting store");
            if (!Array.isArray(snapshot) || snapshot.some(entry => !Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== "string")
              || new Set(snapshot.map(entry => entry[0])).size !== snapshot.length) throw new Error("invalid cell store snapshot");
          } catch (cause) { throw effectUnknown(cause); }
          stored.clear();
          for (const [key, value] of snapshot) stored.set(key, value);
        } catch (cause) { journalFailure(cause); }
      }
      try {
        await abortableEvaluation((async () => {
          try {
            await (extras.evaluate || evaluateNative)(source, {
              sessionId,
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
            // Root completion defines the isolate lifetime; pending nested work
            // is cancelled below without delaying the completed result.
          }
        })(), controller.signal);
      } catch (error) {
        if (error !== EXIT) throw error;
      }
      if (execution.interruption) throw execution.interruption;
      if (execution.recoveryFailure) throw execution.recoveryFailure;
      closePendingCalls();
      return JSON.stringify({
        output: withStatus("Script completed", startedAt, content),
        success: true,
        nested_calls: nestedCalls,
        notifications,
      });
    } catch (error) {
      if (execution.interruption) throw execution.interruption;
      if (error?.code === "host_interrupted") throw error;
      closePendingCalls();
      return JSON.stringify({
        output: `Script failed\nWall time ${wallTime(startedAt)} seconds\nOutput:\n${errorMessage(error)}`,
        success: false,
        nested_calls: nestedCalls,
      });
    } finally {
      closePendingCalls();
      finished = true;
      const commitWrites = !controller.signal.aborted;
      // End the isolate lifetime before asynchronous durability acknowledgement:
      // timers/detached continuations cannot mutate an already captured delta.
      controller.abort(new Error(CANCELLATION_MESSAGE));
      for (const timer of timers.values()) clearTimeout(timer);
      try {
        if (commitWrites) {
          // Failed scripts are completed results too. The durable adapter merges
          // a delta once, never replacing newer writes on a completed-cell replay.
          if (cellContext) {
            try { await journal.commitStore(cellContext, boundedEffectSnapshot([...storedWrites], "cell store writes")); }
            catch (cause) { journalFailure(cause); }
          }
          for (const [key, value] of storedWrites) sessionStore.set(key, value);
          if (cell) cell.finished = true;
        }
      } catch (error) {
        if (error?.code !== "CODE_EFFECT_UNKNOWN") throw error;
        return JSON.stringify({ output: `Script failed\nOutput:\n${errorMessage(error)}`, success: false, nested_calls: nestedCalls });
      } finally {
        controller.abort(new Error(CANCELLATION_MESSAGE));
        for (const timer of timers.values()) clearTimeout(timer);
        admission.release();
        activeExecutions.delete(execution);
      }
    }
  }

  function executeCodeObserved(source, sessionId = "default", parentCallId = "exec", model = "unknown", turnId, localDefinitions, executeLocalTool) {
    return observeOperation(sessionId, parentCallId, (observation) => {
      const options = parseExec(source);
      const cell = {
        id: `${cellGeneration}:${nextCellId++}`, sessionId, parentCallId, controller: new AbortController(),
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
      }, cell, turnId, localDefinitions, executeLocalTool).then((result) => {
        const completed = JSON.parse(result);
        if (!completed.success && typeof completed.output === "string") {
          cell.content.push({ type: "input_text", text: completed.output.split("Output:\n").slice(1).join("Output:\n") || completed.output });
        }
        cell.result = { success: completed.success };
        cell.wake?.();
      }, (error) => {
        if (error?.code === "host_interrupted") cell.interruption = error;
        cell.content.push({ type: "input_text", text: errorMessage(error) });
        cell.result = { success: false };
        cell.wake?.();
      });
      return observeCell(cell, observation, options.yield_time_ms ?? 10_000, cell.budget);
    });
  }

  function waitCodeObserved(input, sessionId = "default", callId = "wait") {
    return observeOperation(sessionId, callId, (observation) => {
      const options = parseCellOptions(input, ["cell_id", "yield_time_ms", "max_tokens", "terminate"], ["max_tokens"], true);
      if (typeof options.cell_id !== "string") throw new TypeError("wait requires a string cell_id");
      if (options.terminate !== undefined && typeof options.terminate !== "boolean") throw new TypeError("terminate must be boolean");
      const cell = cells.get(options.cell_id);
      if (!cell || cell.sessionId !== sessionId) throw new Error(`exec cell ${options.cell_id} not found`);
      if (cell.observing) throw new Error(`exec cell ${cell.id} already has an active observer`);
      cell.turn = turns.get(sessionId) ?? 0;
      if (options.terminate && !cell.finished && !cell.result) {
        cell.terminated = true;
        cell.controller.abort(new Error(CANCELLATION_MESSAGE));
      }
      return observeCell(cell, observation, options.yield_time_ms ?? 10_000, options.max_tokens ?? 10_000);
    });
  }

  function observeOperation(sessionId, callId, operation) {
    const key = codeObservationKey(sessionId, callId);
    const observation = createCodeObservation(sessionId, turns.get(sessionId) ?? 0);
    codeObservations.get(key)?.close();
    codeObservations.set(key, observation);
    return Promise.resolve().then(() => operation(observation)).catch((error) => {
      if (error?.code === "host_interrupted") throw error;
      return JSON.stringify({
        output: `Script failed\nOutput:\n${errorMessage(error)}`, success: false, nested_calls: [],
      });
    }).finally(() => observation.close());
  }

  async function observeCell(cell, observation, yieldTime, budget) {
    const startedAt = performance.now();
    cell.observing = true;
    cell.observation = observation;
    for (const update of cell.updates.splice(0)) observation.push(update);
    let timer;
    let stopPreemptWake;
    try {
      if (cell.terminated) await cell.completion;
      else if (!cell.result && !cell.yieldRequested && !observation.preempted) {
        await new Promise((resolve) => {
          cell.wake = resolve;
          stopPreemptWake = observation.onPreempt(resolve);
          // JS timer APIs overflow past this boundary; clamp instead of
          // accidentally turning a large valid duration into a 1 ms wait.
          timer = setTimeout(resolve, Math.min(yieldTime + (yieldTime >= 10_000 ? 1_000 : 0), 2_147_483_647));
        });
      }
      if (cell.interruption) throw cell.interruption;
      cell.yieldRequested = false;
      const result = cell.result;
      const status = cell.terminated ? "Script terminated"
        : result ? (result.success ? "Script completed" : "Script failed")
        : `Script running with cell ID ${cell.id}`;
      const output = withStatus(status, startedAt, cell.content.splice(0));
      if (result) cells.delete(cell.id);
      let limited = limitCodeOutput(output, budget);
      if (result?.success === false && Array.isArray(limited) && limited.every((item) => item.type === "input_text")) {
        limited = limited.map((item) => item.text).join("");
      }
      return JSON.stringify({
        output: limited,
        success: cell.terminated || (result?.success ?? true),
        cell: { origin_call_id: cell.parentCallId, running: !result },
        nested_calls: cell.completedCalls.splice(0),
        notifications: cell.notifications.splice(0),
      });
    } finally {
      clearTimeout(timer);
      stopPreemptWake?.();
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

  // Preempt only a foreground observation, never the evaluator or nested
  // effects. A signal is scoped to an already admitted exec/wait call ID;
  // a missing/closed observation does not arm future cells or waits.
  // This requires observer event-loop progress. A synchronous guest sharing
  // that event loop cannot be preempted until it yields; evaluator interruption
  // is NOT a safe substitute because it can destroy pending guest promises.
  // Child-Worker evaluation keeps the observer reachable while guest JS is busy.
  function preempt(sessionId, callId) {
    if (typeof sessionId !== "string" || typeof callId !== "string") {
      throw new TypeError("preempt requires session and observation call IDs");
    }
    return codeObservations.get(codeObservationKey(sessionId, callId))?.preempt() ?? false;
  }

  function preemptTurn(sessionId) {
    const turn = turns.get(sessionId) ?? 0;
    let count = 0;
    for (const observation of codeObservations.values()) {
      if (observation.sessionId === sessionId && observation.turn === turn
        && observation.preempt()) count++;
    }
    return count;
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
      const bindingContext = subagentSessions?.bindingDescriptor?.(sessionId, context, hostContextRef) ?? context;
      const descriptor = Object.freeze({
        agentId: bindingContext.agentId,
        parentAgentId: bindingContext.parentAgentId,
        sessionId: bindingContext.sessionId,
        role: bindingContext.role,
        task: bindingContext.task,
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
    preempt,
    preemptTurn,
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
  let preempted = false;
  let preemptWake;
  return Object.freeze({
    sessionId,
    turn,
    get preempted() { return preempted; },
    preempt() {
      if (closed || preempted) return false;
      preempted = true;
      preemptWake?.();
      return true;
    },
    onPreempt(wake) {
      preemptWake = wake;
      if (preempted) wake();
      return () => { if (preemptWake === wake) preemptWake = undefined; };
    },
    push(update) {
      if (closed) return;
      const resolve = waiters.shift();
      if (resolve) resolve(update);
      else queued.push(update);
    },
    close() {
      if (closed) return;
      closed = true;
      preemptWake = undefined;
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
  if (typeof source !== "string" || !source.trim()) {
    throw new TypeError('exec expects raw JavaScript source text (non-empty). Provide JS only, optionally with first-line `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`.');
  }
  const [line] = source.split(/\r?\n/, 1);
  if (!line.trimStart().startsWith("// @exec:")) return { source };
  const rest = source.slice(line.length).replace(/^\r?\n/, "");
  if (!rest.trim()) throw new TypeError("exec pragma must be followed by JavaScript source on subsequent lines");
  return { ...parseCellOptions(line.trimStart().slice("// @exec:".length), ["yield_time_ms", "max_output_tokens"], ["yield_time_ms", "max_output_tokens"]), source: rest };
}

// Match serde u64 parsing without losing integer spelling to IEEE-754
// rounding. Fractional/exponent JSON numbers are not integer arguments.
function parseCellOptions(encoded, allowed, nullable = [], ignoreUnknown = false) {
  const value = JSON.parse(encoded);
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("cell options must be a JSON object");
  const tokens = encoded.match(/"(?:\\.|[^"\\])*"|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null|[{}\[\]:,]/g);
  let depth = 0;
  const rawNumbers = new Map();
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token === "{" || token === "[") depth++;
    else if (token === "}" || token === "]") depth--;
    else if (depth === 1 && token.startsWith('"') && tokens[i + 1] === ":") {
      const key = JSON.parse(token);
      if (rawNumbers.has(key) && allowed.includes(key)) throw new TypeError(`duplicate field ${key}`);
      rawNumbers.set(key, tokens[i + 2]);
    }
  }
  for (const [key, field] of Object.entries(value)) {
    if (!allowed.includes(key)) {
      if (ignoreUnknown) continue;
      throw new TypeError(`unknown cell option: ${key}`);
    }
    if (field === null && nullable.includes(key)) continue;
    if (key !== "cell_id" && key !== "terminate") {
      const raw = rawNumbers.get(key);
      if (typeof field !== "number" || !/^\d+$/.test(raw) || BigInt(raw) > 18_446_744_073_709_551_615n) {
        throw new TypeError(`${key} must be a non-negative u64 integer`);
      }
    }
  }
  return value;
}

// Preserve guest-observable failure kind/code when replaying a failed handler.
function effectFailure(error) {
  if (!(error instanceof Error)) return { kind: "value", value: error ?? null,
    ...(error === undefined ? { undefined: true } : {}) };
  const value = { message: error.message, name: error.name, stack: error.stack };
  for (const key of ["code", "details"]) {
    if (error[key] !== undefined) value[key] = error[key];
  }
  return { kind: "error", value };
}
function validEffectFailure(failure) {
  return failure?.kind === "value" ? Object.hasOwn(failure, "value")
    : failure?.kind === "error" && typeof failure.value?.message === "string";
}
function restoreEffectFailure(failure) {
  if (failure?.kind === "value") return failure.undefined ? undefined : failure.value;
  if (failure?.kind !== "error" || typeof failure.value?.message !== "string") {
    return new Error("invalid retained nested failure");
  }
  const constructors = { Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError };
  const Constructor = Object.hasOwn(constructors, failure.value.name) ? constructors[failure.value.name] : Error;
  return Object.assign(new Constructor(failure.value.message), failure.value);
}

// Receipt serialization is bounded BEFORE allocating JSON clones or multiplying
// output/structured/value representations. The managed adapter may encode this
// snapshot once more, but can never receive an unbounded post-effect payload.
// Reject rather than truncate: truncated data is not an exact replay receipt.
const MAX_EFFECT_RECEIPT_BYTES = 8 * 1024 * 1024;
function boundedEffectSnapshot(value, label) {
  let budget = MAX_EFFECT_RECEIPT_BYTES;
  let nodes = 0;
  try {
    const encoded = JSON.stringify(value, (key, item) => {
      if (++nodes > 32768) throw new RangeError("too many receipt entries");
      if (typeof item === "function" || typeof item === "symbol"
        || (typeof item === "number" && !Number.isFinite(item))) {
        throw new TypeError("receipt contains a non-JSON value");
      }
      budget -= key.length * 3 + 4;
      if (typeof item === "string") {
        // UTF-8 and escaping never require less space than the UTF-16 length.
        // Check first so JSON.stringify(item) cannot allocate a giant string.
        if (item.length > budget) throw new RangeError("receipt too large");
        budget -= new TextEncoder().encode(JSON.stringify(item)).byteLength;
      } else budget -= 24;
      if (budget < 0) throw new RangeError("receipt too large");
      return item;
    });
    if (encoded === undefined || new TextEncoder().encode(encoded).byteLength > MAX_EFFECT_RECEIPT_BYTES) {
      throw new RangeError("receipt too large or not serializable");
    }
    return JSON.parse(encoded);
  } catch (cause) {
    throw effectUnknown(new TypeError(`${label} must be JSON-serializable within the 8 MiB receipt limit; execution outcome unknown`, { cause }));
  }
}

function effectUnknown(cause) {
  return Object.assign(new Error(`Code Mode recovery failed: ${errorMessage(cause)}; execution outcome unknown`, { cause }),
    { code: "CODE_EFFECT_UNKNOWN", outcome: "unknown" });
}
