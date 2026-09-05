"use client";

import {
  createElement,
  useEffect,
  useInsertionEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import {
  applyAgentEvents,
  historyEntryKeys,
  initialState,
  mergeHistoryEntries,
  queuePrompt,
  queueSteer,
  requeueSteerAsPrompt,
  steerAdmitted,
  steerFailed,
  turnFinished,
} from "./transcript.mjs";

const DEFAULT_MAX_ENTRIES = 200;

/** Owns one Agent's semantic conversation lifecycle and returns a render-ready snapshot. */
export function useAgentController(agent, options = {}) {
  const committed = useRef({ onEvent: options.onEvent });
  useInsertionEffect(() => {
    const callbacks = { onEvent: options.onEvent };
    committed.current = callbacks;
    return () => {
      if (committed.current === callbacks) committed.current = {};
    };
  }, [options.onEvent]);

  const maxEntries = positiveInteger(options.maxEntries, DEFAULT_MAX_ENTRIES);
  const controller = useMemo(() => agent
    ? createController(agent, {
        maxEntries,
        onEvent(event) {
          try { committed.current.onEvent?.(event); } catch { /* Observers cannot break ownership. */ }
        },
        visible: options.visible ?? true,
      })
    : IDLE_CONTROLLER, [agent, maxEntries]);

  useEffect(() => controller.attach(), [controller]);
  useEffect(() => {
    controller.setVisible(options.visible ?? true);
  }, [controller, options.visible]);

  return useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );
}

/** Render-prop form of useAgentController for component-first consumers. */
export function AgentController({ agent, children, ...options }) {
  if (typeof children !== "function") {
    throw new TypeError("AgentController children must be a function");
  }
  return createElement(RenderController, { agent, children, options });
}

function RenderController({ agent, children, options }) {
  return children(useAgentController(agent, options));
}

function createController(agent, options) {
  validateAgent(agent);
  let state = initialState();
  let projectedHistoryEntryKeys = new Set();
  let visible = options.visible;
  let dirty = false;
  let scheduled = false;
  let cancelScheduled;
  let attached = false;
  let disposed = false;
  let watcher;
  let releases = [];
  let nextPromptId = 1;
  let loadingOlder = false;
  let hasOlder;
  const listeners = new Set();
  const activeTurns = new Set();

  const controls = Object.freeze({
    submit,
    steer(input, submitOptions = {}) {
      return submit(input, { ...submitOptions, intent: "steer" });
    },
    cancel,
    clear,
    loadOlder,
    dispose,
    setVisible,
  });
  let snapshot = makeSnapshot();

  function makeSnapshot() {
    return Object.freeze({
      entries: state.entries,
      running: state.running,
      status: state.status,
      pendingTurns: state.pendingTurns,
      isLoadingOlder: loadingOlder,
      canLoadOlder: Boolean(watcher?.loadOlder) && hasOlder !== false,
      hasOlder,
      visible,
      ...controls,
    });
  }

  function subscribe(listener) {
    if (disposed) return () => {};
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  }

  function getSnapshot() {
    return snapshot;
  }

  function publish() {
    if (disposed) return;
    dirty = true;
    if (!visible || scheduled) return;
    scheduled = true;
    cancelScheduled = scheduleFrame(flush);
  }

  function flush() {
    scheduled = false;
    cancelScheduled = undefined;
    if (disposed || !dirty || !visible) return;
    dirty = false;
    snapshot = makeSnapshot();
    for (const listener of listeners) listener();
  }

  function setVisible(nextVisible) {
    if (disposed) return;
    const next = Boolean(nextVisible);
    if (visible === next) return;
    visible = next;
    dirty = true;
    if (!visible) {
      cancelScheduled?.();
      cancelScheduled = undefined;
      scheduled = false;
    } else {
      publish();
    }
  }

  function emit(type, detail = {}) {
    options.onEvent(Object.freeze({
      type,
      timestamp: globalThis.performance?.now?.() ?? Date.now(),
      ...detail,
    }));
  }

  function attach() {
    if (disposed || attached) return () => detach();
    attached = true;
    watcher = agent.events.watch();
    if (!watcher || typeof watcher.onEvent !== "function" || typeof watcher.off !== "function") {
      attached = false;
      throw new TypeError("agent.events.watch() must provide onEvent and off");
    }
    releases.push(watcher.onEvent((event) => {
      if (disposed || !attached || event.request_id !== agent.sessionId) return;
      emit("agent.event", { event });
      const wasRunning = state.running;
      state = boundedState(applyAgentEvents(state, [event]), options.maxEntries);
      if (state.running !== wasRunning) emit("controller.running_changed", { running: state.running });
      publish();
    }));
    if (typeof watcher.onHistory === "function") {
      releases.push(watcher.onHistory((events) => {
        if (disposed || !attached) return;
        emit("agent.history", { events });
        const historical = applyAgentEvents(initialState(), events);
        const historicalKeys = historyEntryKeys(historical.entries);
        const entries = mergeHistoryEntries(
          state.entries,
          historical.entries,
          projectedHistoryEntryKeys,
        );
        projectedHistoryEntryKeys = historicalKeys;
        state = boundedState({ ...state, entries }, options.maxEntries);
        publish();
      }));
    }
    dirty = true;
    publish();
    emit("controller.attached", { sessionId: agent.sessionId });
    return detach;
  }

  function detach() {
    if (!attached) return;
    attached = false;
    try { watcher?.off(); } catch (error) { emit("controller.cleanup_error", { error }); }
    for (const release of releases.splice(0)) {
      try { release(); } catch (error) { emit("controller.cleanup_error", { error }); }
    }
    watcher = undefined;
    for (const record of activeTurns) disposeTurn(record);
    activeTurns.clear();
    loadingOlder = false;
    emit("controller.detached", { sessionId: agent.sessionId });
  }

  async function submit(value, submitOptions = {}) {
    const input = String(value).trim();
    if (!input || disposed) return undefined;
    if (input === "/clear") {
      clear();
      return undefined;
    }
    if (input === "/cancel") {
      await cancel();
      return undefined;
    }
    if (input === "/exit") {
      dispose();
      return undefined;
    }
    const id = nextPromptId++;
    const current = latestActiveTurn();
    if (submitOptions.intent !== "queue" && current) {
      state = boundedState(queueSteer(state, id, input), options.maxEntries);
      publish();
      try {
        await current.turn.steer({ input });
        if (disposed) return current.turn;
        state = boundedState(steerAdmitted(state, id), options.maxEntries);
        emit("prompt.steered", { id, input });
      } catch (error) {
        if (isCompletedSteerRace(error)) {
          return startRootTurn(id, input, true);
        }
        if (!disposed) {
          state = boundedState(steerFailed(state, id, errorMessage(error)), options.maxEntries);
          emit("prompt.steer_error", { error, id, input });
        }
      }
      publish();
      return current.turn;
    }
    return startRootTurn(id, input, false);
  }

  function startRootTurn(id, input, requeuedSteer) {
    let turn;
    try {
      turn = agent.turn.prompt({ input });
    } catch (error) {
      state = requeuedSteer
        ? steerFailed(state, id, errorMessage(error))
        : appendLocalError(state, errorMessage(error));
      state = boundedState(state, options.maxEntries);
      emit("prompt.rejected", { error, id, input });
      publish();
      return undefined;
    }
    state = boundedState(
      requeuedSteer
        ? requeueSteerAsPrompt(state, id, input, turn.historyEntryId)
        : queuePrompt(state, id, input, turn.historyEntryId),
      options.maxEntries,
    );
    const record = { disposed: false, id, turn };
    activeTurns.add(record);
    emit("prompt.accepted", { id, input, sessionId: agent.sessionId });
    publish();
    void finishTurn(record);
    return turn;
  }

  async function finishTurn(record) {
    let result;
    try {
      result = await record.turn.result();
      if (disposed || record.disposed) return;
      state = boundedState(turnFinished(
        state,
        undefined,
        result.finalMessage,
        record.id,
        record.turn.historyEntryId,
      ), options.maxEntries);
      emit("prompt.completed", { id: record.id, finalMessage: result.finalMessage });
    } catch (error) {
      if (disposed || record.disposed) return;
      state = boundedState(turnFinished(
        state,
        errorMessage(error),
        undefined,
        record.id,
        record.turn.historyEntryId,
      ), options.maxEntries);
      emit("prompt.failed", { error, id: record.id });
    } finally {
      if (result) {
        try { result.dispose(); } catch (error) { emit("controller.cleanup_error", { error }); }
      }
      activeTurns.delete(record);
      disposeTurn(record);
      publish();
    }
  }

  async function cancel() {
    const current = latestActiveTurn();
    if (!current || disposed) return false;
    try {
      await current.turn.cancel();
      emit("prompt.cancelled", { id: current.id });
      return true;
    } catch (error) {
      state = boundedState(appendLocalError(state, errorMessage(error)), options.maxEntries);
      emit("prompt.cancel_error", { error, id: current.id });
      publish();
      return false;
    }
  }

  async function loadOlder() {
    if (disposed || loadingOlder || typeof watcher?.loadOlder !== "function" || hasOlder === false) {
      return false;
    }
    loadingOlder = true;
    publish();
    try {
      const loaded = await watcher.loadOlder();
      hasOlder = Boolean(loaded);
      emit("history.loaded", { loaded: hasOlder });
      return hasOlder;
    } catch (error) {
      emit("history.error", { error });
      throw error;
    } finally {
      loadingOlder = false;
      publish();
    }
  }

  function clear() {
    if (disposed) return;
    state = { ...state, entries: [] };
    projectedHistoryEntryKeys = new Set();
    emit("controller.cleared");
    publish();
  }

  function latestActiveTurn() {
    let latest;
    for (const record of activeTurns) latest = record;
    return latest;
  }

  function disposeTurn(record) {
    if (record.disposed) return;
    record.disposed = true;
    try { record.turn.dispose(); } catch (error) { emit("controller.cleanup_error", { error }); }
  }

  function dispose() {
    if (disposed) return;
    detach();
    disposed = true;
    cancelScheduled?.();
    cancelScheduled = undefined;
    scheduled = false;
    listeners.clear();
    emit("controller.disposed", { sessionId: agent.sessionId });
  }

  return Object.freeze({
    attach,
    cancel,
    clear,
    dispose,
    getSnapshot,
    loadOlder,
    setVisible,
    steer: controls.steer,
    submit,
    subscribe,
  });
}

function boundedState(state, maxEntries) {
  let entries = state.entries.length > maxEntries ? state.entries.slice(-maxEntries) : state.entries;
  let changed = entries !== state.entries;
  for (let index = 0; index < entries.length; index += 1) {
    const entry = entries[index];
    if (!("text" in entry) || entry.text.length <= 8_001) continue;
    if (!changed) {
      entries = entries.slice();
      changed = true;
    }
    entries[index] = { ...entry, text: entry.text.slice(0, 8_001) };
  }
  return changed ? { ...state, entries } : state;
}

function appendLocalError(state, text) {
  const syntheticId = state.syntheticId + 1;
  return {
    ...state, syntheticId,
    entries: [...state.entries, { id: `controller-error-${syntheticId}`, kind: "error", text }],
  };
}

function scheduleFrame(callback) {
  let active = true;
  if (typeof globalThis.requestAnimationFrame === "function") {
    try {
      const frame = globalThis.requestAnimationFrame(() => {
        if (!active) return;
        active = false;
        callback();
      });
      return () => {
        if (!active) return;
        active = false;
        try { globalThis.cancelAnimationFrame?.(frame); } catch { /* Guarded above. */ }
      };
    } catch { /* Non-window shims use a microtask. */ }
  }
  queueMicrotask(() => {
    if (!active) return;
    active = false;
    callback();
  });
  return () => { active = false; };
}

function validateAgent(agent) {
  if (!agent || typeof agent.sessionId !== "string"
    || typeof agent.turn?.prompt !== "function"
    || typeof agent.events?.watch !== "function") {
    throw new TypeError("agent must provide sessionId, turn.prompt, and events.watch");
  }
}

function positiveInteger(value, fallback) {
  return Number.isSafeInteger(value) && value > 0 ? value : fallback;
}

function errorMessage(error) {
  const message = error instanceof Error ? error.message : String(error);
  return /Responses WebSocket handshake failed|WebSocket connection failed/.test(message)
    ? "Could not connect to the agent. Try again."
    : message;
}

function isCompletedSteerRace(error) {
  return error && typeof error === "object" && error.status === 409
    && (error.code === "turn_not_active" || error.code === "turn_not_steerable");
}

const idleControls = Object.freeze({
  async submit() { return undefined; },
  async steer() { return undefined; },
  async cancel() { return false; },
  clear() {},
  async loadOlder() { return false; },
  dispose() {},
  setVisible() {},
});
const IDLE_SNAPSHOT = Object.freeze({
  entries: Object.freeze([]), running: false, status: "Idle", pendingTurns: 0,
  isLoadingOlder: false, canLoadOlder: false, hasOlder: undefined, visible: true,
  ...idleControls,
});
const IDLE_CONTROLLER = Object.freeze({
  attach() { return () => {}; },
  getSnapshot() { return IDLE_SNAPSHOT; },
  subscribe() { return () => {}; },
  ...idleControls,
});
