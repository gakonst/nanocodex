const HISTORY_PAGE_SIZE = 128;
const MAX_LIVE_ENVELOPES = HISTORY_PAGE_SIZE * 2;
const HISTORY_ATTEMPTS = 3;
const HISTORY_ATTEMPT_TIMEOUT_MS = 10_000;
const HISTORY_RETRY_INITIAL_MS = 1_000;
const HISTORY_RETRY_MAX_MS = 30_000;
const TAIL_RETRY_INITIAL_MS = 250;
const TAIL_RETRY_MAX_MS = 5_000;
const TURN_RETRY_INITIAL_MS = 250;
const TURN_RETRY_MAX_MS = 5_000;

/** Normalizes one capability-bound Connect agent for nanocodex-react/agent. */
export function createConnectAgentSource(connectAgent, options) {
  validateConnectAgent(connectAgent);
  if (!options || typeof options.history !== "boolean") {
    throw new TypeError("createConnectAgentSource requires an explicit history boolean");
  }

  const submitted = options.history ? undefined : new Set();
  return Object.freeze({
    sessionId: connectAgent.sessionId,
    voiceSource: connectAgent,
    events: Object.freeze({
      watch: () => connectEventWatcher(connectAgent, submitted, options.history),
    }),
    turn: Object.freeze({
      prompt: ({ input }) => {
        const id = globalThis.crypto.randomUUID();
        submitted?.add(id);
        return connectTurn(connectAgent, id, input);
      },
    }),
  });
}

function connectTurn(connectAgent, turnId, input) {
  const controller = new AbortController();
  let turn = startConnectTurn(connectAgent, turnId, input);
  return Object.freeze({
    historyEntryId: `managed-user-${turnId}`,
    steer: ({ input: steerInput }) => turn.steer({ input: steerInput }),
    cancel: () => turn.cancel(),
    async result() {
      let retryDelay = TURN_RETRY_INITIAL_MS;
      while (!controller.signal.aborted) {
        try {
          const result = await turn.result({ signal: controller.signal });
          return Object.freeze({ finalMessage: result.finalMessage, dispose() {} });
        } catch (error) {
          if (controller.signal.aborted || !isRetryableTurnError(error)) throw error;
          await retryDelayUnlessAborted(retryDelay, controller.signal);
          if (controller.signal.aborted) throw controller.signal.reason;
          retryDelay = Math.min(retryDelay * 2, TURN_RETRY_MAX_MS);
          turn = startConnectTurn(connectAgent, turnId, input);
        }
      }
      throw controller.signal.reason ?? new DOMException("Connect turn detached", "AbortError");
    },
    dispose() {
      if (!controller.signal.aborted) controller.abort();
    },
  });
}

function startConnectTurn(connectAgent, turnId, input) {
  return connectAgent.turn.prompt({ id: turnId, idempotencyKey: turnId, input });
}

function isRetryableTurnError(error) {
  return error && typeof error === "object" && (
    error.code === "network_error"
    || error.code === "event_stream_ended"
    || error.code === "event_stream_inactive"
  );
}

function connectEventWatcher(connectAgent, submitted, historyEnabled) {
  const controller = new AbortController();
  const listeners = new Set();
  const historyListeners = new Set();
  const envelopes = [];
  const seen = new Set();
  let sequence = 0;
  let hasOlder = false;
  let historyLoaded = false;
  let loadingOlder;
  let loadingInitial;
  let historyPageInFlight;
  let tailStarted = false;
  let outageReported = false;
  let historyRetryDelay = HISTORY_RETRY_INITIAL_MS;
  let historyRetryTimer;
  let latestLiveCursor;
  let historySnapshot = Object.freeze([]);

  const projectedHistory = () => connectHistoryEvents(envelopes, connectAgent.sessionId);
  const emitHistory = () => {
    const events = projectedHistory();
    historySnapshot = events;
    sequence = Math.max(sequence, events.length);
    for (const listener of historyListeners) listener(events);
  };
  const retain = (envelope) => {
    if (seen.has(envelope.cursor)) return false;
    seen.add(envelope.cursor);
    envelopes.push(envelope);
    return seen.has(envelope.cursor);
  };
  const requestHistoryPage = (pageOptions) => historyPageAttempt((signal) => {
    if (historyPageInFlight) {
      throw new Error("the previous Connect history request is still settling");
    }
    const request = connectAgent.events.page({ ...pageOptions, signal });
    historyPageInFlight = request;
    const clear = () => {
      if (historyPageInFlight === request) historyPageInFlight = undefined;
    };
    void request.then(clear, clear);
    return request;
  }, controller.signal);
  const reportHistoryOutage = (error) => {
    if (outageReported) return;
    outageReported = true;
    console.warn("nanocodex:connect.history_unavailable", {
      agentId: connectAgent.id,
      error: error instanceof Error ? error.message : String(error),
      retrying: true,
    });
  };
  const scheduleHistoryRetry = () => {
    if (controller.signal.aborted || historyLoaded || historyRetryTimer !== undefined) return;
    const delay = historyRetryDelay;
    historyRetryDelay = Math.min(historyRetryDelay * 2, HISTORY_RETRY_MAX_MS);
    historyRetryTimer = setTimeout(() => {
      historyRetryTimer = undefined;
      void loadInitial();
    }, delay);
  };
  const startTail = (cursor) => {
    if (tailStarted || controller.signal.aborted) return;
    tailStarted = true;
    void (async () => {
      let retryDelay = TAIL_RETRY_INITIAL_MS;
      while (!controller.signal.aborted) {
        try {
          for await (const envelope of connectAgent.events.watch({
            cursor: latestLiveCursor ?? cursor,
            signal: controller.signal,
          })) {
            if (controller.signal.aborted) return;
            if (latestLiveCursor !== undefined
              && compareCursor(envelope.cursor, latestLiveCursor) <= 0) continue;
            latestLiveCursor = envelope.cursor;
            retryDelay = TAIL_RETRY_INITIAL_MS;
            const turnId = envelopeTurnId(envelope);
            if (!historyEnabled && !submitted?.has(turnId ?? "")) continue;
            if (!retain(envelope)) continue;
            const projected = envelopeEvents(
              envelope,
              rawAssistantMessageTurns(envelopes),
              connectAgent.sessionId,
              sequence + 1,
            );
            sequence += projected.length;
            if (historyEnabled && projected.length > 0) {
              historySnapshot = Object.freeze([...historySnapshot, ...projected]);
            }
            for (const event of projected) {
              for (const listener of listeners) listener(event);
            }
            if (turnId && isOuterTerminal(envelope)) submitted?.delete(turnId);
            if (historyLoaded && !hasOlder) compactEnvelopeRetention(envelopes, seen);
          }
        } catch (error) {
          if (controller.signal.aborted) return;
          if (!isRetryableTailError(error)) {
            for (const event of [
              historyEvent(connectAgent.sessionId, ++sequence, "run.error", {
                message: error instanceof Error ? error.message : String(error),
              }),
              historyEvent(connectAgent.sessionId, ++sequence, "run.failed", { status: "failed" }),
            ]) {
              for (const listener of listeners) listener(event);
            }
            return;
          }
        }
        await retryDelayUnlessAborted(retryDelay, controller.signal);
        retryDelay = Math.min(retryDelay * 2, TAIL_RETRY_MAX_MS);
      }
    })();
  };
  const loadInitial = () => {
    if (historyLoaded) return Promise.resolve(true);
    if (controller.signal.aborted) return Promise.resolve(false);
    if (loadingInitial) return loadingInitial;
    loadingInitial = (async () => {
      let initial;
      let historyError;
      const attempts = outageReported ? 1 : HISTORY_ATTEMPTS;
      for (let attempt = 1; attempt <= attempts; attempt += 1) {
        try {
          initial = await requestHistoryPage({ limit: HISTORY_PAGE_SIZE });
          break;
        } catch (error) {
          historyError = error;
          if (controller.signal.aborted) return false;
          if (historyPageInFlight) break;
        }
      }
      if (!initial || controller.signal.aborted) {
        reportHistoryOutage(historyError);
        startTail("latest");
        scheduleHistoryRetry();
        return false;
      }
      for (const envelope of initial.data) retain(envelope);
      envelopes.sort((left, right) => compareCursor(left.cursor, right.cursor));
      hasOlder = initial.hasMore;
      historyLoaded = true;
      latestLiveCursor = initial.latestCursor;
      outageReported = false;
      historyRetryDelay = HISTORY_RETRY_INITIAL_MS;
      if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
      historyRetryTimer = undefined;
      emitHistory();
      startTail(initial.latestCursor);
      if (hasOlder) void loadRemainingHistory();
      else compactEnvelopeRetention(envelopes, seen);
      return true;
    })().finally(() => { loadingInitial = undefined; });
    return loadingInitial;
  };
  const retryWhenOnline = () => {
    if (controller.signal.aborted || historyLoaded) return;
    if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
    historyRetryTimer = undefined;
    void loadInitial();
  };
  const loadOlderPage = () => {
    if (!historyEnabled || !historyLoaded || !hasOlder || controller.signal.aborted) {
      return Promise.resolve(false);
    }
    if (loadingOlder) return loadingOlder;
    const before = envelopes[0]?.cursor;
    if (!before) return Promise.resolve(false);
    loadingOlder = requestHistoryPage({ before, limit: HISTORY_PAGE_SIZE }).then((page) => {
      let added = false;
      for (const envelope of page.data) added = retain(envelope) || added;
      if (added) envelopes.sort((left, right) => compareCursor(left.cursor, right.cursor));
      hasOlder = page.hasMore;
      if (added) emitHistory();
      return added;
    }).finally(() => { loadingOlder = undefined; });
    return loadingOlder;
  };
  const loadRemainingHistory = async () => {
    try {
      while (hasOlder && !controller.signal.aborted) {
        const added = await loadOlderPage();
        if (!added) break;
      }
    } catch (error) {
      reportHistoryOutage(error);
    } finally {
      if (!hasOlder) compactEnvelopeRetention(envelopes, seen);
    }
  };

  if (historyEnabled) {
    globalThis.addEventListener?.("online", retryWhenOnline);
    void loadInitial();
  } else {
    historyLoaded = true;
    startTail("latest");
  }

  return Object.freeze({
    onEvent(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    onHistory(listener) {
      historyListeners.add(listener);
      if (historyLoaded) listener(historySnapshot);
      return () => historyListeners.delete(listener);
    },
    loadOlder() {
      if (!historyEnabled) return Promise.resolve(false);
      if (!historyLoaded) return loadInitial();
      return loadOlderPage();
    },
    off() {
      if (controller.signal.aborted) return;
      controller.abort();
      if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
      globalThis.removeEventListener?.("online", retryWhenOnline);
      listeners.clear();
      historyListeners.clear();
    },
  });
}

async function historyPageAttempt(load, lifetimeSignal) {
  if (lifetimeSignal.aborted) {
    throw lifetimeSignal.reason ?? new Error("Connect history detached");
  }
  const attempt = new AbortController();
  let rejectBoundary;
  const boundary = new Promise((_, reject) => { rejectBoundary = reject; });
  const abort = (reason) => {
    if (attempt.signal.aborted) return;
    attempt.abort(reason);
    rejectBoundary(reason);
  };
  const lifetimeAborted = () => abort(
    lifetimeSignal.reason ?? new Error("Connect history detached"),
  );
  const timeout = setTimeout(
    () => abort(new Error(`Connect history request exceeded ${HISTORY_ATTEMPT_TIMEOUT_MS}ms`)),
    HISTORY_ATTEMPT_TIMEOUT_MS,
  );
  lifetimeSignal.addEventListener("abort", lifetimeAborted, { once: true });
  try {
    const result = await Promise.race([load(attempt.signal), boundary]);
    if (attempt.signal.aborted) throw attempt.signal.reason;
    return result;
  } catch (error) {
    if (attempt.signal.aborted) throw attempt.signal.reason;
    throw error;
  } finally {
    clearTimeout(timeout);
    lifetimeSignal.removeEventListener("abort", lifetimeAborted);
  }
}

function compactEnvelopeRetention(envelopes, seen) {
  while (envelopes.length > MAX_LIVE_ENVELOPES) {
    const groups = envelopeGroups(envelopes);
    const oversizedComplete = [...groups.values()].find((group) =>
      group.complete && group.envelopes.length > MAX_LIVE_ENVELOPES
    );
    if (oversizedComplete) {
      removeEnvelopes(
        envelopes,
        seen,
        new Set(oversizedComplete.envelopes.filter((envelope) =>
          !oversizedComplete.mandatory.has(envelope)
        )),
      );
      continue;
    }
    const complete = [...groups.values()]
      .filter((group) => group.complete && groups.size > 1)
      .sort((left, right) => compareCursor(left.oldestCursor, right.oldestCursor))[0];
    if (complete) {
      removeEnvelopes(envelopes, seen, new Set(complete.envelopes));
      continue;
    }
    const removable = [...groups.values()]
      .flatMap((group) => group.envelopes.filter((envelope) => !group.mandatory.has(envelope)))
      .sort((left, right) => compareCursor(left.cursor, right.cursor))[0];
    if (!removable) return;
    removeEnvelopes(envelopes, seen, new Set([removable]));
  }
}

function envelopeGroups(envelopes) {
  const grouped = new Map();
  for (const envelope of envelopes) {
    const group = envelopeGroup(envelope);
    const retained = grouped.get(group) ?? [];
    retained.push(envelope);
    grouped.set(group, retained);
  }
  return new Map([...grouped].map(([group, retained]) => {
    retained.sort((left, right) => compareCursor(left.cursor, right.cursor));
    const prompt = retained.find((envelope) => envelope.data.type === "turn_accepted");
    const terminal = [...retained].reverse().find(isOuterTerminal);
    const mandatory = new Set();
    if (prompt) mandatory.add(prompt);
    if (terminal) mandatory.add(terminal);
    return [group, Object.freeze({
      envelopes: retained,
      mandatory,
      complete: terminal !== undefined,
      oldestCursor: retained[0].cursor,
    })];
  }));
}

function isOuterTerminal(envelope) {
  return envelope.data.type === "turn_completed"
    || envelope.data.type === "turn_cancelled"
    || envelope.data.type === "turn_failed"
    || envelope.data.type === "stream_failed";
}

function removeEnvelopes(envelopes, seen, removed) {
  for (let index = envelopes.length - 1; index >= 0; index -= 1) {
    const envelope = envelopes[index];
    if (!removed.has(envelope)) continue;
    envelopes.splice(index, 1);
    seen.delete(envelope.cursor);
  }
}

function envelopeGroup(envelope) {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return envelope.turnId ?? (typeof id === "string" ? id : `cursor:${envelope.cursor}`);
}

function envelopeTurnId(envelope) {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return envelope.turnId ?? (typeof id === "string" ? id : undefined);
}

function compareCursor(left, right) {
  if (left.length !== right.length) return left.length - right.length;
  return left < right ? -1 : left > right ? 1 : 0;
}

function connectHistoryEvents(envelopes, sessionId) {
  const events = [];
  const assistantTurns = rawAssistantMessageTurns(envelopes);
  for (const envelope of envelopes) {
    events.push(...envelopeEvents(
      envelope,
      assistantTurns,
      sessionId,
      events.length + 1,
    ));
  }
  return Object.freeze(events);
}

function envelopeEvents(envelope, rawAssistantTurns, sessionId, firstSequence) {
  if (envelope.data.type === "event") {
    const projected = rawEvent(envelope, sessionId, firstSequence);
    if (!projected || RAW_RUN_TERMINALS.has(projected.type)) return [];
    return [projected];
  }
  if (envelope.data.type === "turn_accepted") {
    return [historyEvent(sessionId, firstSequence, "managed.prompt", {
      text: promptText(envelope.data.input),
      turn_id: envelope.data.id,
    })];
  }

  const turnId = terminalTurnId(envelope);
  if (envelope.data.type === "turn_completed") {
    const projected = [];
    if (!rawAssistantTurns.has(turnId)) {
      projected.push(historyEvent(sessionId, firstSequence, "assistant.message", {
        text: envelope.data.final_message,
        turn_id: turnId,
      }));
    }
    projected.push(historyEvent(sessionId, firstSequence + projected.length, "run.completed", {
      status: "completed",
      disposition: "completed",
      turn_id: turnId,
    }));
    return projected;
  }
  if (envelope.data.type === "turn_cancelled") {
    return [historyEvent(sessionId, firstSequence, "run.failed", {
      status: "cancelled",
      disposition: "cancelled",
      turn_id: turnId,
    })];
  }
  if (envelope.data.type === "turn_failed") {
    return [
      historyEvent(sessionId, firstSequence, "run.error", {
        message: envelope.data.error,
        turn_id: turnId,
      }),
      historyEvent(sessionId, firstSequence + 1, "run.failed", {
        status: "failed",
        disposition: "failed",
        turn_id: turnId,
      }),
    ];
  }
  if (envelope.data.type === "turn_retryable") {
    return [historyEvent(sessionId, firstSequence, "run.error", {
      message: envelope.data.error,
      disposition: "retryable",
      turn_id: turnId,
    })];
  }
  if (envelope.data.type === "stream_failed") {
    return [
      historyEvent(sessionId, firstSequence, "run.error", { message: envelope.data.error }),
      historyEvent(sessionId, firstSequence + 1, "run.failed", {
        status: "failed",
        disposition: "stream_failed",
      }),
    ];
  }
  return [];
}

function rawEvent(envelope, sessionId, sequence) {
  const value = envelope.data.event;
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  if (typeof value.type !== "string"
    || !value.payload || typeof value.payload !== "object" || Array.isArray(value.payload)) {
    return undefined;
  }
  return {
    ...value,
    request_id: sessionId,
    seq: sequence,
    payload: {
      ...value.payload,
      managed_event_cursor: envelope.cursor,
      ...(envelope.turnId ? { turn_id: envelope.turnId } : {}),
    },
  };
}

const RAW_RUN_TERMINALS = new Set(["run.error", "run.completed", "run.failed"]);

function terminalTurnId(envelope) {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return typeof id === "string" ? id : envelope.turnId ?? "unknown";
}

function rawAssistantMessageTurns(envelopes) {
  const turns = new Set();
  for (const envelope of envelopes) {
    if (!envelope.turnId || envelope.data.type !== "event") continue;
    const event = envelope.data.event;
    if (event && typeof event === "object" && !Array.isArray(event)
      && event.type === "assistant.message") {
      turns.add(envelope.turnId);
    }
  }
  return turns;
}

function historyEvent(sessionId, seq, type, payload) {
  return { protocol_version: 1, request_id: sessionId, seq, type, payload };
}

function promptText(input) {
  if (typeof input === "string") return input;
  if (!Array.isArray(input)) return "[prompt]";
  return input.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    return item.type === "text" && typeof item.text === "string"
      ? [item.text]
      : item.type === "image"
        ? ["[image]"]
        : item.type === "audio"
          ? ["[audio]"]
          : [];
  }).join("\n");
}

function validateConnectAgent(connectAgent) {
  if (!connectAgent || typeof connectAgent.sessionId !== "string"
    || typeof connectAgent.turn?.prompt !== "function"
    || typeof connectAgent.events?.page !== "function"
    || typeof connectAgent.events?.watch !== "function") {
    throw new TypeError("connectAgent must provide sessionId, turn.prompt, events.page, and events.watch");
  }
}

function isRetryableTailError(error) {
  return error && typeof error === "object" && (
    error.code === "network_error"
    || error.code === "event_stream_ended"
    || error.code === "event_stream_inactive"
  );
}

function retryDelayUnlessAborted(milliseconds, signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const timeout = setTimeout(done, milliseconds);
    signal.addEventListener("abort", done, { once: true });
    function done() {
      clearTimeout(timeout);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}
