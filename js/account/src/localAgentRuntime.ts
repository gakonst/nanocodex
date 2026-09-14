import type { AgentEvent, AgentSessionContext } from "nanocodex";
import type { Agent, AgentTurn } from "nanocodex-react/agent";
import {
  createLocalTranscriptJournal,
  type LocalTranscriptJournal,
  type LocalTranscriptSteer,
  type LocalTranscriptTransition,
  type LocalTranscriptTurn,
  type LocalTranscriptTurnStatus,
} from "./localTranscriptJournal.ts";

const DEFAULT_RECOVERY_TIMEOUT_MS = 30_000;
const browserJournal = createLocalTranscriptJournal();

export type LocalTranscriptActivity = Readonly<{
  watch(listener: () => void): () => void;
}>;

const browserTranscriptActivity: LocalTranscriptActivity = Object.freeze({
  watch(listener) {
    const document = globalThis.document;
    const window = globalThis.window;
    if (!document || !window) return () => {};
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") listener();
    };
    document.addEventListener("visibilitychange", refreshWhenVisible);
    window.addEventListener("focus", refreshWhenVisible);
    window.addEventListener("pageshow", refreshWhenVisible);
    return () => {
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      window.removeEventListener("focus", refreshWhenVisible);
      window.removeEventListener("pageshow", refreshWhenVisible);
    };
  },
});

type LocalSessionAgent = Agent & Readonly<{
  session: Readonly<{ context(): Promise<AgentSessionContext> }>;
  turn: Readonly<{ prompt(options: { input: string; id?: string }): AgentTurn }>;
}>;

/** Adds an app-owned durable transcript to the local browser agent. */
export function localTerminalAgent(
  agent: LocalSessionAgent,
  threadId: string,
  journal: LocalTranscriptJournal = browserJournal,
  onInitializationError?: (error: unknown) => void,
  recoveryTimeoutMs = DEFAULT_RECOVERY_TIMEOUT_MS,
  transcriptActivity: LocalTranscriptActivity = browserTranscriptActivity,
  beforeTurn: () => Promise<void> = async () => {},
): Agent {
  let latestHistory: readonly AgentEvent[] | undefined;
  const visibleTurns = new Map<string, LocalTranscriptTurn>();
  let visibleHistoryChanged = true;
  let historyInitialized = false;
  let olderBefore: string | undefined;
  let newestOrder: string | undefined;
  const reopenBarriers = new Set<string>();
  let processorTail: Promise<void> = Promise.resolve();
  const processLocally = <T>(operation: () => Promise<T>): Promise<T> => {
    const result = processorTail.then(operation);
    processorTail = result.then(() => undefined, () => undefined);
    return result;
  };
  const historyListeners = new Set<{
    listener: (events: readonly AgentEvent[]) => void;
    delivered: boolean;
  }>();
  let refreshTail: Promise<void> = Promise.resolve();
  const publishHistory = (publish: boolean): readonly AgentEvent[] => {
    if (!visibleHistoryChanged && latestHistory) return latestHistory;
    const turns = [...visibleTurns.values()].sort((left, right) => {
      const a = left.order ?? "", b = right.order ?? "";
      return a === b ? left.createdAt - right.createdAt : a < b ? -1 : 1;
    });
    const events = localTranscriptEvents(turns, agent.sessionId);
    latestHistory = events;
    visibleHistoryChanged = false;
    if (publish) {
      for (const subscription of historyListeners) {
        if (subscription.delivered) notifyHistoryListener(subscription.listener, events);
      }
    }
    return events;
  };
  const retainPage = (turns: readonly LocalTranscriptTurn[]) => {
    for (const turn of turns) {
      const retained = visibleTurns.get(turn.turnId);
      if (retained && sameTranscriptTurn(retained, turn)) continue;
      visibleTurns.set(turn.turnId, turn);
      visibleHistoryChanged = true;
    }
  };
  const refreshHistory = (publish: boolean): Promise<readonly AgentEvent[]> => {
    const refresh = refreshTail.then(async () => {
      let page = await journal.load(threadId);
      const newest = page.turns.at(-1)?.order;
      const refreshed = new Set(page.turns.map(({ turnId }) => turnId));
      retainPage(page.turns);
      if (!historyInitialized) {
        olderBefore = page.next;
        historyInitialized = true;
      } else {
        // Catch up only the gap since the last observed page, not all saved
        // history. Older terminal rows are immutable; only unfinished rows in
        // the current presentation need individual refreshes.
        while (page.next !== undefined
          && (newestOrder === undefined || page.turns[0]!.order! > newestOrder)) {
          page = await journal.load(threadId, page.next);
          const added = page.turns.filter((turn) => newestOrder === undefined || turn.order! > newestOrder);
          retainPage(added);
          for (const turn of added) refreshed.add(turn.turnId);
        }
        for (const turn of visibleTurns.values()) {
          if (refreshed.has(turn.turnId) || terminalTranscriptStatus(transcriptStatus(turn))) continue;
          const current = await journal.get(threadId, turn.turnId);
          if (current) retainPage([current]);
        }
      }
      newestOrder = newest ?? newestOrder;
      return publishHistory(publish);
    });
    refreshTail = refresh.then(() => undefined, () => undefined);
    return refresh;
  };
  const loadOlder = (): Promise<boolean> => {
    const load = refreshTail.then(async () => {
      if (olderBefore === undefined) return false;
      const page = await journal.load(threadId, olderBefore);
      retainPage(page.turns);
      olderBefore = page.next;
      publishHistory(true);
      return page.turns.length > 0;
    });
    refreshTail = load.then(() => undefined, () => undefined);
    return load;
  };
  let journalWatchers = 0;
  let stopJournalWatch: (() => void) | undefined;
  const acquireJournalWatch = () => {
    journalWatchers += 1;
    if (journalWatchers !== 1) return;
    let stopped = false;
    let pending = false;
    let refreshing = false;
    const refresh = () => {
      pending = true;
      if (refreshing) return;
      refreshing = true;
      // Visibility, focus and pageshow often describe the same tab activation.
      // Coalesce that burst; a notification during a read requests one followup.
      queueMicrotask(async () => {
        try {
          while (pending && !stopped) {
            pending = false;
            await refreshHistory(true);
          }
        } catch (error) { onInitializationError?.(error); }
        finally { refreshing = false; }
      });
    };
    const stopBroadcastWatch = journal.watch(threadId, refresh);
    const stopActivityWatch = transcriptActivity.watch(refresh);
    stopJournalWatch = () => {
      stopped = true;
      stopBroadcastWatch();
      stopActivityWatch();
    };
  };
  const releaseJournalWatch = () => {
    journalWatchers -= 1;
    if (journalWatchers !== 0) return;
    stopJournalWatch?.();
    stopJournalWatch = undefined;
  };
  let initialization: Promise<void> | undefined;
  const initialize = () => {
    if (!initialization) {
      initialization = initializeTranscript(agent, threadId, journal, beforeTurn);
      void initialization.catch((error) => onInitializationError?.(error));
    }
    return initialization;
  };
  let history: Promise<readonly AgentEvent[]> | undefined;
  let activeDurableTurnId: string | undefined;
  const setActiveDurableTurn = (turnId?: string) => {
    activeDurableTurnId = turnId;
  };
  const startHistory = () => {
    if (history) return history;
    history = (async () => {
      await initialize();
      // Establish the presentation snapshot before recovery can emit live
      // events. If recovery completed first, this snapshot already contained
      // its terminal assistant and the buffered raw stream replayed that same
      // assistant a second time after history delivery.
      const initial = await refreshHistory(false);
      void processLocally(async () => {
        await processPendingTurns(
          agent,
          threadId,
          journal,
          reopenBarriers,
          recoveryTimeoutMs,
          undefined,
          undefined,
          () => refreshHistory(true),
          setActiveDurableTurn,
          beforeTurn,
        );
      }).catch((error) => onInitializationError?.(error));
      return initial;
    })();
    void history.catch((error) => onInitializationError?.(error));
    return history;
  };

  return Object.freeze({
    sessionId: agent.sessionId,
    turn: Object.freeze({
      prompt(options: { input: string }) {
        const turnId = crypto.randomUUID();
        const transcript = Object.freeze({
          threadId,
          turnId,
          createdAt: Date.now(),
          prompt: options.input,
        });
        const persisted = (async () => {
          try {
            await journal.recordPrompt(transcript);
          } catch (error) {
            throw localStorageError("Could not save this prompt; it was not submitted to the agent", error);
          }
          try {
            await initialize();
          } catch (error) {
            throw localStorageError(
              "The prompt was saved, but local transcript initialization failed; reload to recover it",
              error,
            );
          }
        })();
        return deferredTurn(async (admitted) => {
          await persisted;
          return processLocally(async () => {
            const processed = await processPendingTurns(
              agent,
              threadId,
              journal,
              reopenBarriers,
              recoveryTimeoutMs,
              transcript,
              admitted,
              () => refreshHistory(true),
              setActiveDurableTurn,
              beforeTurn,
            );
            if (processed instanceof Error) throw processed;
            if (processed) return processed;
            throw localStorageError(
              `Saved prompt ${turnId} disappeared before it could be processed`,
              new Error("persisted local transcript turn is missing"),
            );
          });
        }, `managed-user-${turnId}`, async (input) => {
          await persisted;
          const steer = Object.freeze({
            id: crypto.randomUUID(),
            text: input,
            status: "pending" as const,
          });
          try {
            await journal.appendSteer(transcript, steer);
          } catch (error) {
            throw localStorageError(
              "Could not reserve durable steering input; it was not submitted to the agent",
              error,
            );
          }
          await refreshHistory(true).catch((error) => onInitializationError?.(error));
          return steer;
        }, async (steer, status, error) => {
          try {
            await journal.updateSteer(transcript, steer.id, {
              status,
              ...(error === undefined ? {} : { error: errorMessage(error) }),
            });
          } catch (storageError) {
            throw localStorageError(
              `The steering input is durably retained as pending, but saving its ${status} status failed; reload before sending more input`,
              storageError,
            );
          }
          await refreshHistory(true).catch((refreshError) => onInitializationError?.(refreshError));
        }, async () => {
          await persisted;
          let transition: LocalTranscriptTransition;
          try {
            transition = await journal.requestCancel(transcript);
          } catch (error) {
            throw localStorageError(
              "Could not save the cancellation request; the durable prompt remains active",
              error,
            );
          }
          await refreshHistory(true).catch((error) => onInitializationError?.(error));
          return transition;
        });
      },
    }),
    events: Object.freeze({
      watch() {
        acquireJournalWatch();
        const live = agent.events.watch();
        const eventListeners = new Set<(event: AgentEvent) => void>();
        const ownedHistoryListeners = new Set<{
          listener: (events: readonly AgentEvent[]) => void;
          delivered: boolean;
        }>();
        const pendingLiveEvents: AgentEvent[] = [];
        let historyRequested = false;
        let initialHistoryDelivered = false;
        let disposed = false;
        const offLive = live.onEvent((event) => {
          if (disposed) return;
          // The browser Agent emits the complete protocol event. The public
          // controller contract intentionally requires only its projection fields.
          const sourceEvent = event as AgentEvent;
          const ownedEvent = activeDurableTurnId === undefined
            ? sourceEvent
            : {
                ...sourceEvent,
                payload: { ...sourceEvent.payload, turn_id: activeDurableTurnId },
              };
          // The independently awaited Turn result owns the typed durability
          // disposition and authoritative transcript transition. Raw Rust
          // policy diagnostics are tracing detail; projecting them here races
          // winner absorption and leaks stale-owner/ambiguous errors after the
          // saved turn has already recovered correctly.
          if (rawDurabilityPolicyDiagnostic(ownedEvent)) return;
          if (!initialHistoryDelivered) {
            if (historyRequested) pendingLiveEvents.push(ownedEvent);
            return;
          }
          for (const listener of eventListeners) listener(ownedEvent);
        });
        return Object.freeze({
          onEvent(listener: (event: AgentEvent) => void) {
            eventListeners.add(listener);
            return () => eventListeners.delete(listener);
          },
          onHistory(listener: (events: readonly AgentEvent[]) => void) {
            historyRequested = true;
            const subscription = { listener, delivered: false };
            historyListeners.add(subscription);
            ownedHistoryListeners.add(subscription);
            const loaded = history === undefined ? startHistory() : startHistory().then(() => refreshHistory(false));
            void loaded.then((events) => {
              if (!disposed && historyListeners.has(subscription)) {
                notifyHistoryListener(listener, latestHistory ?? events);
                subscription.delivered = true;
                if (!initialHistoryDelivered) {
                  initialHistoryDelivered = true;
                  for (const event of pendingLiveEvents.splice(0)) {
                    for (const eventListener of eventListeners) eventListener(event);
                  }
                }
              }
            }, () => {});
            return () => {
              historyListeners.delete(subscription);
              ownedHistoryListeners.delete(subscription);
            };
          },
          async loadOlder() {
            await startHistory();
            if (disposed) return false;
            return loadOlder();
          },
          off() {
            if (disposed) return;
            disposed = true;
            offLive();
            eventListeners.clear();
            pendingLiveEvents.length = 0;
            for (const subscription of ownedHistoryListeners) historyListeners.delete(subscription);
            ownedHistoryListeners.clear();
            live.off();
            releaseJournalWatch();
          },
        });
      },
    }),
  });
}

function deferredTurn(
  start: (admitted: (turn: AgentTurn) => void) => Promise<Awaited<ReturnType<AgentTurn["result"]>>>,
  historyEntryId?: string,
  reserveSteer: (input: string) => Promise<LocalTranscriptSteer> = async (input) => ({
    id: crypto.randomUUID(), text: input, status: "pending",
  }),
  settleSteer: (
    steer: LocalTranscriptSteer,
    status: "accepted" | "rejected",
    error?: unknown,
  ) => Promise<void> = async () => {},
  persistCancel: () => Promise<LocalTranscriptTransition | undefined> = async () => undefined,
): AgentTurn {
  let turn: AgentTurn | undefined;
  let disposed = false;
  let settled = false;
  let underlyingDisposed = false;
  let cancellation: Promise<unknown> | undefined;
  let resolveAdmission!: (turn: AgentTurn) => void;
  let rejectAdmission!: (error: unknown) => void;
  const admission = new Promise<AgentTurn>((resolve, reject) => {
    resolveAdmission = resolve;
    rejectAdmission = reject;
  });
  void admission.catch(() => {});
  const disposeUnderlying = () => {
    if (!disposed || !settled || !turn || underlyingDisposed) return;
    underlyingDisposed = true;
    try {
      turn.dispose();
    } catch (error) {
      reportObserverError(error);
    }
  };
  const result = start((admitted) => {
    turn = admitted;
    resolveAdmission(admitted);
  });
  void result.then(() => {
    settled = true;
    if (!turn) {
      rejectAdmission(codedError(
        "conflict",
        "This saved turn was completed by another browser context; it no longer accepts steering or cancellation",
      ));
    }
    disposeUnderlying();
  }, (error) => {
    settled = true;
    rejectAdmission(error);
    disposeUnderlying();
  });
  return Object.freeze({
    historyEntryId,
    async steer(options: { input: string }) {
      const steer = await reserveSteer(options.input);
      let admitted: AgentTurn;
      try {
        admitted = await admission;
      } catch (error) {
        await settleSteer(steer, "rejected", error);
        throw error;
      }
      try {
        await admitted.steer(options);
      } catch (error) {
        await settleSteer(steer, "rejected", error);
        throw error;
      }
      await settleSteer(steer, "accepted");
    },
    cancel() {
      if (!cancellation) cancellation = persistCancel().then(async (transition) => {
        const status = transition && transcriptStatus(transition.turn);
        if (status === "cancelled") return;
        if (status === "completed" || status === "failed") {
          throw codedError("conflict", `This saved turn is already ${status}`);
        }
        try {
          const admitted = await admission;
          await admitted.cancel();
        } catch (error) {
          if (errorCode(error) === "cancelled") return;
          throw error;
        }
      });
      return cancellation;
    },
    result: () => result,
    dispose() {
      disposed = true;
      disposeUnderlying();
    },
  });
}

async function finishDurableTurn(
  turn: AgentTurn,
  transcript: LocalTranscriptTurn,
  journal: LocalTranscriptJournal,
  refreshed: () => Promise<unknown>,
): Promise<Awaited<ReturnType<AgentTurn["result"]>>> {
  try {
    const completed = await turn.result();
    let transition: LocalTranscriptTransition;
    try {
      transition = await journal.completeTurn({
        ...transcript,
        assistant: completed.finalMessage,
      });
      await refreshed();
    } catch (error) {
      completed.dispose();
      throw localStorageError("The answer completed, but saving it failed; reload to recover it", error);
    }
    const status = transcriptStatus(transition.turn);
    if (status === "cancelled") {
      completed.dispose();
      throw retainedCancellation(transition.turn);
    }
    if (status === "failed") {
      completed.dispose();
      throw retainedTerminalFailure(transition.turn);
    }
    if (!transition.applied && transition.turn.assistant !== completed.finalMessage) {
      completed.dispose();
      return retainedCompletion(transition.turn);
    }
    return completed;
  } catch (error) {
    if (errorCode(error) === "local_storage") throw error;
    const update = retainedFailure(error, transcript);
    let transition: LocalTranscriptTransition;
    try {
      transition = await journal.updateTurn(transcript, update);
      await refreshed();
    } catch (storageError) {
      throw localStorageError("Saving the durable turn state failed; reload before submitting more work", storageError);
    }
    const status = transcriptStatus(transition.turn);
    if (status === "completed") return retainedCompletion(transition.turn);
    if (status === "cancelled") throw retainedCancellation(transition.turn);
    if (status === "failed") {
      if (transition.applied) throw error;
      throw retainedTerminalFailure(transition.turn);
    }
    throw retainedBarrier(transition.turn, status);
  }
}

async function initializeTranscript(
  agent: LocalSessionAgent,
  threadId: string,
  journal: LocalTranscriptJournal,
  beforeTurn: () => Promise<void>,
): Promise<void> {
  const retained = await journal.load(threadId);
  if (retained.initialized) return;
  await beforeTurn();
  const context = await agent.session.context();
  const bootstrap = localContextTurns(context.history, threadId);
  await journal.bootstrap(threadId, bootstrap);
}

async function processPendingTurns(
  agent: LocalSessionAgent,
  threadId: string,
  journal: LocalTranscriptJournal,
  reopenBarriers: Set<string>,
  recoveryTimeoutMs: number,
  target?: LocalTranscriptTurn,
  admitted?: (turn: AgentTurn) => void,
  refreshed: () => Promise<unknown> = async () => {},
  setActiveTurn: (turnId?: string) => void = () => {},
  beforeTurn: () => Promise<void> = async () => {},
): Promise<Awaited<ReturnType<AgentTurn["result"]>> | Error | undefined> {
  const settledTarget = async () => {
    if (!target) return undefined;
    const retained = await journal.get(threadId, target.turnId);
    if (!retained) return undefined;
    const status = transcriptStatus(retained);
    if (status === "completed") return retainedCompletion(retained);
    if (status === "cancelled") return retainedCancellation(retained);
    if (status === "failed") return retainedTerminalFailure(retained);
    return undefined;
  };
  const alreadySettled = await settledTarget();
  if (alreadySettled !== undefined) return alreadySettled;
  for await (const transcript of journal.pending(threadId)) {
    const status = transcriptStatus(transcript);
    if (status === "completed") {
      if (transcript.turnId === target?.turnId) return retainedCompletion(transcript);
      continue;
    }
    const unresolvedSteer = transcript.steers?.find(({ status: steerStatus }) => steerStatus === "pending");
    if (unresolvedSteer && transcript.turnId !== target?.turnId) {
      const transition = await journal.updateTurn(transcript, {
        status: "blocked",
        error: unresolvedSteerMessage(transcript, unresolvedSteer),
      });
      await refreshed();
      const currentStatus = transcriptStatus(transition.turn);
      if (currentStatus === "completed") {
        if (transcript.turnId === target?.turnId) return retainedCompletion(transition.turn);
        continue;
      }
      if (currentStatus === "cancelled") {
        if (transcript.turnId === target?.turnId) return retainedCancellation(transition.turn);
        continue;
      }
      if (currentStatus === "failed") {
        if (transcript.turnId === target?.turnId) return retainedTerminalFailure(transition.turn);
        continue;
      }
      return retainedBarrier(transition.turn, currentStatus);
    }
    if (transcript.cancelRequested) {
      const transition = await journal.updateTurn(transcript, {
        status: "cancelled",
        error: "the turn was cancelled",
      });
      await refreshed();
      const currentStatus = transcriptStatus(transition.turn);
      if (currentStatus === "completed") {
        if (transcript.turnId === target?.turnId) return retainedCompletion(transition.turn);
        continue;
      }
      if (currentStatus === "failed") {
        if (transcript.turnId === target?.turnId) return retainedTerminalFailure(transition.turn);
        continue;
      }
      if (transcript.turnId === target?.turnId) return retainedCancellation(transition.turn);
      continue;
    }
    if (status === "cancelled") {
      if (transcript.turnId === target?.turnId) return retainedCancellation(transcript);
      continue;
    }
    if (status === "failed") {
      if (transcript.turnId === target?.turnId) return retainedTerminalFailure(transcript);
      continue;
    }
    if (status === "blocked" || (status === "reopen_required" && reopenBarriers.has(transcript.turnId))) {
      const transition = await journal.updateTurn(transcript, { status, error: transcript.error });
      const currentStatus = transcriptStatus(transition.turn);
      if (currentStatus === "completed") {
        if (transcript.turnId === target?.turnId) return retainedCompletion(transition.turn);
        continue;
      }
      if (currentStatus === "cancelled") {
        if (transcript.turnId === target?.turnId) return retainedCancellation(transition.turn);
        continue;
      }
      if (currentStatus === "failed") {
        if (transcript.turnId === target?.turnId) return retainedTerminalFailure(transition.turn);
        continue;
      }
      return retainedBarrier(transition.turn, currentStatus);
    }
    if (!transcript.prompt) {
      const error = codedError(
        "blocked",
        `Durable turn ${transcript.turnId} has no recoverable prompt; reopen or replace this local thread`,
      );
      const transition = await journal.updateTurn(transcript, { status: "blocked", error: error.message });
      const currentStatus = transcriptStatus(transition.turn);
      if (currentStatus === "completed") {
        if (transcript.turnId === target?.turnId) return retainedCompletion(transition.turn);
        continue;
      }
      if (currentStatus === "cancelled") {
        if (transcript.turnId === target?.turnId) return retainedCancellation(transition.turn);
        continue;
      }
      if (currentStatus === "failed") {
        if (transcript.turnId === target?.turnId) return retainedTerminalFailure(transition.turn);
        continue;
      }
      return retainedBarrier(transition.turn, currentStatus);
    }
    if (transcript.turnId === target?.turnId) {
      await beforeTurn();
      setActiveTurn(transcript.turnId);
      try {
        const turn = agent.turn.prompt({ input: transcript.prompt, id: transcript.turnId });
        admitted?.(turn);
        return await finishDurableTurn(turn, transcript, journal, refreshed);
      } finally {
        setActiveTurn();
      }
    }
    let turn: AgentTurn | undefined;
    let completed: Awaited<ReturnType<AgentTurn["result"]>> | undefined;
    try {
      await beforeTurn();
      setActiveTurn(transcript.turnId);
      turn = agent.turn.prompt({ input: transcript.prompt, id: transcript.turnId });
      completed = await boundedRecoveryResult(turn, transcript, recoveryTimeoutMs);
      try {
        await journal.completeTurn({ ...transcript, assistant: completed.finalMessage });
        await refreshed();
      } catch (error) {
        throw localStorageError(
          "The recovered answer could not be saved and published; reload before submitting more work",
          error,
        );
      }
    } catch (error) {
      if (errorCode(error) === "local_storage") throw error;
      if (errorCode(error) === "recovery_timeout" && turn) {
        // Relinquish only this observer. The durable operation remains
        // nonterminal so a fresh owner can recover its exact ID.
        turn = undefined;
      }
      const update = retainedFailure(error, transcript);
      const transition = await journal.updateTurn(transcript, update);
      await refreshed();
      const currentStatus = transcriptStatus(transition.turn);
      if (currentStatus === "completed" || currentStatus === "cancelled" || currentStatus === "failed") continue;
      if (currentStatus === "reopen_required") reopenBarriers.add(transcript.turnId);
      return retainedBarrier(transition.turn, currentStatus);
    } finally {
      setActiveTurn();
      completed?.dispose();
      turn?.dispose();
    }
  }
  // Another tab may settle the target after the first read, removing it
  // from the pending index before our cursor reaches it. Absorb that winner.
  return settledTarget();
}

function boundedRecoveryResult(
  turn: AgentTurn,
  transcript: LocalTranscriptTurn,
  timeoutMs: number,
): Promise<Awaited<ReturnType<AgentTurn["result"]>>> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(codedError(
        "recovery_timeout",
        `Saved turn ${transcript.turnId} did not settle during recovery. Reload to retry this exact saved prompt; newer prompts remain blocked.`,
      ));
    }, Math.max(0, timeoutMs));
    let result: ReturnType<AgentTurn["result"]>;
    try {
      result = turn.result();
    } catch (error) {
      clearTimeout(timeout);
      reject(error);
      return;
    }
    void result.then((completed) => {
      if (settled) {
        disposeCompletedResult(completed);
        disposeTerminalTurn(turn);
        return;
      }
      settled = true;
      clearTimeout(timeout);
      resolve(completed);
    }, (error) => {
      if (settled) {
        disposeTerminalTurn(turn);
        return;
      }
      settled = true;
      clearTimeout(timeout);
      reject(error);
    });
  });
}

function disposeTerminalTurn(turn: AgentTurn): void {
  try {
    turn.dispose();
  } catch (error) {
    reportObserverError(error);
  }
}

function disposeCompletedResult(completed: Awaited<ReturnType<AgentTurn["result"]>>): void {
  try {
    completed.dispose();
  } catch (error) {
    reportObserverError(error);
  }
}

export function localContextTurns(
  history: readonly Record<string, unknown>[],
  threadId: string,
): readonly LocalTranscriptTurn[] {
  const turns: Array<{ prompt?: string; assistant?: string; turnId: string }> = [];
  for (const item of history) {
    if (item.type !== "message") continue;
    if (item.role === "user") {
      const prompt = messageText(item.content, "input_text");
      if (!prompt || adapterContextMessage(prompt)) continue;
      turns.push({ prompt, turnId: messageId(item, turns.length + 1) });
      continue;
    }
    if (item.role !== "assistant" || item.phase === "commentary") continue;
    const assistant = messageText(item.content, "output_text");
    if (!assistant) continue;
    const pending = turns.at(-1);
    if (pending && pending.assistant === undefined) pending.assistant = assistant;
    else turns.push({ assistant, turnId: `bootstrap-${turns.length}-assistant` });
  }
  const start = Date.now() - turns.length;
  return Object.freeze(turns.map((turn, index) => Object.freeze({
    ...turn,
    threadId,
    createdAt: start + index,
  })));
}

export function localTranscriptEvents(
  turns: readonly LocalTranscriptTurn[],
  sessionId: string,
): readonly AgentEvent[] {
  const events: AgentEvent[] = [];
  for (const turn of turns) {
    const unfinished = !terminalTranscriptStatus(transcriptStatus(turn));
    if (turn.prompt !== undefined || unfinished) {
      events.push(historyEvent(sessionId, events.length + 1, "managed.prompt", {
        text: turn.prompt ?? "",
        turn_id: turn.turnId,
        ...(unfinished ? {
          status: turn.cancelRequested ? "cancelling" : transcriptStatus(turn),
          ...(turn.prompt === undefined ? { input_missing: true } : {}),
        } : {}),
      }));
    }
    for (const steer of turn.steers ?? []) {
      events.push(historyEvent(sessionId, events.length + 1, "managed.steer", {
        text: steer.text,
        steer_id: steer.id,
        steering_status: steer.status,
        ...(steer.error === undefined ? {} : { error: steer.error }),
        turn_id: turn.turnId,
      }));
    }
    const status = transcriptStatus(turn);
    if (status === "completed" && turn.assistant) {
      events.push(historyEvent(sessionId, events.length + 1, "assistant.message", {
        text: turn.assistant,
        turn_id: turn.turnId,
      }));
    } else if (status === "cancelled") {
      events.push(historyEvent(sessionId, events.length + 1, "run.failed", {
        status: "cancelled",
        disposition: "cancelled",
        turn_id: turn.turnId,
      }));
    } else if (status !== "pending" && status !== "completed") {
      events.push(historyEvent(sessionId, events.length + 1, "run.error", {
        message: retainedTurnError(turn, status),
        turn_id: turn.turnId,
      }));
      events.push(historyEvent(sessionId, events.length + 1, "run.failed", {
        status: "failed",
        disposition: status,
        turn_id: turn.turnId,
      }));
    }
  }
  return Object.freeze(events);
}

function terminalTranscriptStatus(
  status: LocalTranscriptTurnStatus,
): status is "completed" | "cancelled" | "failed" {
  return status === "completed" || status === "cancelled" || status === "failed";
}

/** Project model context when initializing a browser-owned transcript. */
export function localHistoryEvents(
  history: readonly Record<string, unknown>[],
  sessionId: string,
): readonly AgentEvent[] {
  return localTranscriptEvents(localContextTurns(history, sessionId), sessionId);
}

function messageText(content: unknown, type: "input_text" | "output_text"): string {
  if (!Array.isArray(content)) return "";
  return content.flatMap((part) => {
    if (!part || typeof part !== "object" || Array.isArray(part)) return [];
    const value = part as Record<string, unknown>;
    return value.type === type && typeof value.text === "string" ? [value.text] : [];
  }).join("\n");
}

function adapterContextMessage(text: string): boolean {
  const trimmed = text.trimStart();
  return trimmed.startsWith("<environment_context>")
    || trimmed.startsWith("<permissions instructions>");
}

function messageId(item: Record<string, unknown>, fallback: number): string {
  const metadata = item.internal_chat_message_metadata_passthrough;
  if (metadata && typeof metadata === "object" && !Array.isArray(metadata)) {
    const turnId = (metadata as Record<string, unknown>).turn_id;
    if (typeof turnId === "string" && turnId) return turnId;
  }
  return typeof item.id === "string" && item.id ? item.id : `local-${fallback}`;
}

function historyEvent(
  sessionId: string,
  seq: number,
  type: "managed.prompt" | "managed.steer" | "assistant.message" | "run.error" | "run.failed",
  payload: Record<string, unknown>,
): AgentEvent {
  return { protocol_version: 1, request_id: sessionId, seq, type, payload };
}

function retainedFailure(error: unknown, turn: LocalTranscriptTurn): Readonly<{
  status: LocalTranscriptTurnStatus;
  error: string;
}> {
  const code = errorCode(error);
  const status: LocalTranscriptTurnStatus = code === "retryable"
    ? "retryable"
    : code === "reopen_required" || code === "recovery_timeout"
      ? "reopen_required"
      : code === "blocked" || code === "conflict"
        ? "blocked"
        : code === "cancelled"
          ? "cancelled"
          : code === "failed" || code === "invalid_request"
            ? "failed"
          : "blocked";
  return Object.freeze({
    status,
    error: (status === "reopen_required" && code !== "recovery_timeout") || status === "blocked"
      ? retainedStatusMessage(turn, status)
      : errorMessage(error),
  });
}

function transcriptStatus(turn: LocalTranscriptTurn): LocalTranscriptTurnStatus {
  if (turn.assistant !== undefined && rawDurabilityPolicyMessage(turn.assistant)) {
    return "blocked";
  }
  return turn.status ?? (turn.assistant === undefined ? "pending" : "completed");
}

function retainedBarrier(
  turn: LocalTranscriptTurn,
  status: LocalTranscriptTurnStatus,
): Error {
  const code = status === "reopen_required" ? "reopen_required" : status === "retryable" ? "retryable" : "blocked";
  return codedError(code, retainedTurnError(turn, status));
}

function retainedCompletion(turn: LocalTranscriptTurn): Readonly<{
  finalMessage: string;
  dispose(): void;
}> {
  if (turn.assistant === undefined) {
    throw codedError("failed", `Durable turn ${turn.turnId} completed without a retained answer`);
  }
  return Object.freeze({ finalMessage: turn.assistant, dispose() {} });
}

function retainedTerminalFailure(turn: LocalTranscriptTurn): Error {
  return codedError("failed", retainedTurnError(turn, "failed"));
}

function retainedCancellation(turn: LocalTranscriptTurn): Error {
  return codedError("cancelled", turn.error ?? "the turn was cancelled");
}

function retainedStatusMessage(turn: LocalTranscriptTurn, status: LocalTranscriptTurnStatus): string {
  if (status === "reopen_required") {
    return "This tab lost local agent ownership. Reload to recover this saved prompt.";
  }
  if (status === "retryable") {
    return `Durable turn ${turn.turnId} is still pending and will be retried before newer work`;
  }
  if (status === "failed") return `Durable turn ${turn.turnId} failed`;
  if (status === "cancelled") return "the turn was cancelled";
  return "This saved turn stopped at a tool whose outcome could not be proved. "
    + "It was not repeated, and newer prompts remain blocked. "
    + "Review the tool's effects, then start a new conversation.";
}

function retainedTurnError(turn: LocalTranscriptTurn, status: LocalTranscriptTurnStatus): string {
  return turn.error !== undefined && !rawDurabilityPolicyMessage(turn.error)
    ? turn.error
    : retainedStatusMessage(turn, status);
}

function unresolvedSteerMessage(turn: LocalTranscriptTurn, steer: LocalTranscriptSteer): string {
  return `Durable turn ${turn.turnId} has retained steering input ${steer.id} whose dispatch outcome cannot be proved. `
    + "The saved prompt was not recovered without it, and the steering input was not repeated. "
    + "Review any possible effects, then replace this local thread before submitting newer work.";
}

function localStorageError(message: string, source: unknown): Error {
  return codedError("local_storage", `${message}: ${errorMessage(source)}`);
}

function codedError(code: string, message: string): Error {
  const error = new Error(message) as Error & { code: string };
  error.code = code;
  return error;
}

function errorCode(error: unknown): string | undefined {
  if (!error || typeof error !== "object") return undefined;
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" ? code : undefined;
}

function rawDurabilityPolicyDiagnostic(event: AgentEvent): boolean {
  if (event.type !== "run.error") return false;
  const message = event.payload?.message;
  return typeof message === "string" && rawDurabilityPolicyMessage(message);
}

function rawDurabilityPolicyMessage(message: string): boolean {
  return message.startsWith("durability execution policy failed:");
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function reportObserverError(error: unknown): void {
  if (typeof globalThis.reportError === "function") {
    globalThis.reportError(error);
    return;
  }
  console.error("local transcript history observer failed", error);
}

function notifyHistoryListener(
  listener: (events: readonly AgentEvent[]) => void,
  events: readonly AgentEvent[],
): void {
  try {
    listener(events);
  } catch (error) {
    reportObserverError(error);
  }
}

function sameTranscriptTurn(left: LocalTranscriptTurn, right: LocalTranscriptTurn): boolean {
  return left.threadId === right.threadId && left.turnId === right.turnId
    && left.order === right.order && left.createdAt === right.createdAt
    && left.prompt === right.prompt && left.assistant === right.assistant
    && left.status === right.status && left.error === right.error
    && left.cancelRequested === right.cancelRequested
    && (left.steers?.length ?? 0) === (right.steers?.length ?? 0)
    && (left.steers ?? []).every((steer, index) => {
      const other = right.steers![index]!;
      return steer.id === other.id && steer.text === other.text
        && steer.status === other.status && steer.error === other.error;
    });
}
