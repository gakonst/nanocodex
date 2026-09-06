import type { AgentEvent } from "nanocodex";
import {
  Agent,
  type ManagedAgent,
  type ManagedCreateSettings,
  type ManagedEvent,
  type ManagedTurn,
} from "nanocodex/managed";
import type { Agent as ControllerAgent, AgentTurn } from "nanocodex-react/agent";

const MANAGED_HISTORY_PAGE_SIZE = 128;
const MANAGED_HISTORY_INITIAL_ATTEMPTS = 3;
const MANAGED_HISTORY_ATTEMPT_TIMEOUT_MS = 10_000;
const MANAGED_HISTORY_RETRY_INITIAL_MS = 1_000;
const MANAGED_HISTORY_RETRY_MAX_MS = 30_000;
const DEFAULT_MANAGED_CREATE_SETTINGS: ManagedCreateSettings = Object.freeze({
  model: "gpt-6-astra",
  thinking: "low",
  reasoningMode: "standard",
  fastMode: false,
});
export const MAX_MANAGED_RETAINED_ENVELOPES = MANAGED_HISTORY_PAGE_SIZE * 2;
const managedAgents = new Map<string, ManagedAgent>();
const managedLists = new Map<string, Promise<readonly ManagedConversation[]>>();
const managedCreates = new Map<string, Promise<ManagedConversation>>();

export type ManagedConversation = Readonly<{
  id: string;
  title: string;
  updatedAt?: number;
  turnCount?: number;
}>;

export type ManagedConversationSelection = Readonly<{
  conversations: readonly ManagedConversation[];
  selectedId?: string;
  replaceRoute: boolean;
}>;

export type ManagedTerminalSource = Pick<ManagedAgent, "events" | "id" | "turn" | "type">;

export function listManagedConversations(
  accountId = "default",
  options: Readonly<{ refresh?: boolean }> = {},
): Promise<readonly ManagedConversation[]> {
  if (options.refresh) managedLists.delete(accountId);
  const retained = managedLists.get(accountId);
  if (retained) return retained;
  const loading = Agent.list().then((agents) => {
    const conversations = agents.map((agent) => {
      managedAgents.set(agent.id, agent);
      return managedConversation(agent);
    });
    return Object.freeze(conversations.sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0)));
  }).catch((error) => {
    if (managedLists.get(accountId) === loading) managedLists.delete(accountId);
    throw error;
  });
  managedLists.set(accountId, loading);
  return loading;
}

export async function loadManagedConversationSelection(options: Readonly<{
  accountId?: string;
  routeAgentId?: string;
  retainedAgentId?: string;
  hasCredential: boolean;
  createSettings?: ManagedCreateSettings;
  refresh?: boolean;
}>): Promise<ManagedConversationSelection> {
  const accountId = options.accountId ?? "default";
  const listing = listManagedConversations(accountId, { refresh: options.refresh });
  if (options.routeAgentId) {
    const [exact, listed] = await Promise.all([
      getManagedConversation(options.routeAgentId),
      listing.catch((): readonly ManagedConversation[] => Object.freeze([])),
    ]);
    const conversations = listed.some(({ id }) => id === exact.id)
      ? listed
      : Object.freeze([exact, ...listed]);
    if (managedLists.get(accountId) === listing) {
      managedLists.set(accountId, Promise.resolve(conversations));
    }
    return Object.freeze({ conversations, selectedId: exact.id, replaceRoute: false });
  }
  const listed = await listing;
  const conversations = listed.length || !options.hasCredential
    ? listed
    : Object.freeze([await createManagedConversation(accountId, options.createSettings)]);
  const selectedId = conversations.find(({ id }) => id === options.retainedAgentId)?.id
    ?? conversations[0]?.id;
  return Object.freeze({
    conversations,
    ...(selectedId === undefined ? {} : { selectedId }),
    replaceRoute: selectedId !== undefined,
  });
}

export function createManagedConversation(
  accountId = "default",
  settings: ManagedCreateSettings = DEFAULT_MANAGED_CREATE_SETTINGS,
): Promise<ManagedConversation> {
  const creationKey = `${accountId}:${JSON.stringify(settings)}`;
  const retained = managedCreates.get(creationKey);
  if (retained) return retained;
  const creating = Agent.create({ settings }).then((agent) => {
    managedAgents.set(agent.id, agent);
    managedLists.delete(accountId);
    return Object.freeze({
      id: agent.id,
      title: "New conversation",
      updatedAt: Date.now(),
      turnCount: 0,
    });
  }).finally(() => {
    if (managedCreates.get(creationKey) === creating) managedCreates.delete(creationKey);
  });
  managedCreates.set(creationKey, creating);
  return creating;
}

export function openManagedTerminalAgent(agentId: string): ControllerAgent {
  return managedTerminalAgent(openManagedAgent(agentId));
}

export function openManagedAgent(agentId: string): ManagedAgent {
  const managed = managedAgents.get(agentId) ?? Agent.open(agentId);
  managedAgents.set(agentId, managed);
  return managed;
}

async function getManagedConversation(agentId: string): Promise<ManagedConversation> {
  const managed = await Agent.get(agentId);
  managedAgents.set(agentId, managed);
  return managedConversation(managed);
}

function managedConversation(agent: ManagedAgent): ManagedConversation {
  return Object.freeze({
    id: agent.id,
    title: titleFromPrompt(agent.summary?.title ?? "") || `Conversation ${agent.id.slice(0, 8)}`,
    ...(agent.summary === undefined ? {} : {
      updatedAt: agent.summary.updatedAt,
      turnCount: agent.summary.turnCount,
    }),
  });
}

export function managedTerminalAgent(
  managed: ManagedTerminalSource,
  options: Readonly<{ history?: boolean }> = {},
): ControllerAgent {
  const historyEnabled = options.history !== false;
  const submitted = historyEnabled ? undefined : new Set<string>();
  return Object.freeze({
    sessionId: managed.id,
    ...(isManagedAgent(managed) ? { voiceSource: managed } : {}),
    events: Object.freeze({
      watch: () => managedEventWatcher(managed, submitted, historyEnabled),
    }),
    turn: Object.freeze({
      prompt: ({ input }: { input: string }) => {
        const id = crypto.randomUUID();
        submitted?.add(id);
        return managedTerminalTurn(managed, id, input);
      },
    }),
  });
}

function isManagedAgent(source: ManagedTerminalSource): source is ManagedAgent {
  const candidate = source as Partial<ManagedAgent>;
  return typeof candidate.state === "function" && typeof candidate.delete === "function";
}

function managedTerminalTurn(managed: ManagedTerminalSource, turnId: string, input: string): AgentTurn {
  const controller = new AbortController();
  const turn: ManagedTurn = managed.turn.prompt({ id: turnId, input });
  return Object.freeze({
    historyEntryId: `managed-user-${turnId}`,
    steer: ({ input }) => turn.steer({ input }),
    cancel: () => turn.cancel(),
    async result() {
      const result = await turn.result({ signal: controller.signal });
      return Object.freeze({ finalMessage: result.finalMessage, dispose() {} });
    },
    dispose() { controller.abort(); },
  });
}

function managedEventWatcher(
  managed: ManagedTerminalSource,
  submitted: Set<string> | undefined,
  historyEnabled: boolean,
): ReturnType<ControllerAgent["events"]["watch"]> {
  const controller = new AbortController();
  const listeners = new Set<(event: AgentEvent) => void>();
  const historyListeners = new Set<(events: readonly AgentEvent[]) => void>();
  const envelopes: ManagedEvent[] = [];
  const seen = new Set<string>();
  let sequence = 0;
  let hasOlder = false;
  let historyLoaded = false;
  let loadingOlder: Promise<boolean> | undefined;
  let loadingInitial: Promise<boolean> | undefined;
  let historyPageInFlight: Promise<Awaited<ReturnType<typeof managed.events.page>>> | undefined;
  let tailStarted = false;
  let outageReported = false;
  let historyRetryDelay = MANAGED_HISTORY_RETRY_INITIAL_MS;
  let historyRetryTimer: ReturnType<typeof setTimeout> | undefined;
  let latestLiveCursor: string | undefined;
  let olderBeforeCursor: string | undefined;
  let historySnapshot: readonly AgentEvent[] = Object.freeze([]);
  const emit = (event: AgentEvent) => {
    for (const listener of listeners) listener(event);
  };
  const projectedHistory = () => managedHistoryEvents(
    envelopes,
    managed.id,
    submitted,
  );
  const emitHistory = () => {
    const events = projectedHistory();
    historySnapshot = events;
    sequence = Math.max(sequence, events.length);
    for (const listener of historyListeners) listener(events);
  };
  const retain = (envelope: ManagedEvent) => {
    if (seen.has(envelope.cursor)) return false;
    seen.add(envelope.cursor);
    envelopes.push(envelope);
    return true;
  };
  const requestHistoryPage = (
    options: Omit<Parameters<typeof managed.events.page>[0], "signal">,
  ) => managedHistoryPageAttempt((signal) => {
    if (historyPageInFlight) {
      throw new Error("the previous managed history request is still settling");
    }
    const request = managed.events.page({ ...options, signal });
    historyPageInFlight = request;
    const clear = () => {
      if (historyPageInFlight === request) historyPageInFlight = undefined;
    };
    void request.then(clear, clear);
    return request;
  }, controller.signal);
  const reportHistoryOutage = (historyError: unknown) => {
    if (outageReported) return;
    outageReported = true;
    const detail = historyError instanceof Error ? historyError.message : String(historyError);
    console.warn("nanocodex:managed.history_unavailable", {
      agentId: managed.id,
      error: detail,
      retrying: true,
    });
  };
  const stopOlderPagination = (before: string | undefined, reason: string) => {
    hasOlder = false;
    olderBeforeCursor = undefined;
    console.warn("nanocodex:managed.history_pagination_stalled", {
      agentId: managed.id,
      before,
      reason,
    });
  };
  const scheduleHistoryRetry = () => {
    if (controller.signal.aborted
      || (historyLoaded && !hasOlder)
      || historyRetryTimer !== undefined) return;
    const delay = historyRetryDelay;
    historyRetryDelay = Math.min(historyRetryDelay * 2, MANAGED_HISTORY_RETRY_MAX_MS);
    historyRetryTimer = setTimeout(() => {
      historyRetryTimer = undefined;
      void (historyLoaded ? loadRemainingHistory() : loadInitial());
    }, delay);
  };
  const startTail = (cursor: string) => {
    if (tailStarted || controller.signal.aborted) return;
    tailStarted = true;
    void (async () => {
      try {
        for await (const envelope of managed.events.watch({
          cursor,
          signal: controller.signal,
        })) {
          if (controller.signal.aborted) return;
          if (latestLiveCursor !== undefined
            && compareManagedCursor(envelope.cursor, latestLiveCursor) <= 0) continue;
          latestLiveCursor = envelope.cursor;
          const turnId = managedEnvelopeTurnId(envelope);
          if (!historyEnabled && !submitted?.has(turnId ?? "")) continue;
          if (!retain(envelope)) continue;
          const projected = managedEnvelopeEvents(
            envelope,
            rawAssistantMessageTurns(envelopes),
            managed.id,
            submitted,
            sequence + 1,
          );
          sequence += projected.length;
          if (historyEnabled && projected.length > 0) {
            historySnapshot = Object.freeze([...historySnapshot, ...projected]);
          }
          for (const event of projected) emit(event);
          if (turnId && managedOuterTerminal(envelope)) submitted?.delete(turnId);
          if (historyLoaded && !hasOlder) {
            compactManagedEnvelopeRetention(envelopes, seen);
          }
        }
      } catch (error) {
        if (controller.signal.aborted) return;
        emit({
          protocol_version: 1,
          request_id: managed.id,
          seq: ++sequence,
          type: "run.error",
          payload: { message: error instanceof Error ? error.message : String(error) },
        });
        emit({
          protocol_version: 1,
          request_id: managed.id,
          seq: ++sequence,
          type: "run.failed",
          payload: { status: "failed" },
        });
      }
    })();
  };
  const loadInitial = (): Promise<boolean> => {
    if (historyLoaded) return Promise.resolve(true);
    if (controller.signal.aborted) return Promise.resolve(false);
    if (loadingInitial) return loadingInitial;
    loadingInitial = (async () => {
      let initial: Awaited<ReturnType<typeof managed.events.page>> | undefined;
      let historyError: unknown;
      const attempts = outageReported ? 1 : MANAGED_HISTORY_INITIAL_ATTEMPTS;
      for (let attempt = 1; attempt <= attempts; attempt += 1) {
        try {
          initial = await requestHistoryPage({ limit: MANAGED_HISTORY_PAGE_SIZE });
          break;
        } catch (error) {
          historyError = error;
          if (controller.signal.aborted) return false;
          console.warn("nanocodex:managed.history_failed", {
            agentId: managed.id,
            attempt,
            error: error instanceof Error ? error.message : String(error),
          });
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
      envelopes.sort((left, right) => compareManagedCursor(left.cursor, right.cursor));
      hasOlder = initial.hasMore;
      olderBeforeCursor = oldestManagedCursor(initial.data);
      if (hasOlder && olderBeforeCursor === undefined) {
        stopOlderPagination(undefined, "the newest page was empty while hasMore was true");
      }
      historyLoaded = true;
      latestLiveCursor = initial.latestCursor;
      outageReported = false;
      historyRetryDelay = MANAGED_HISTORY_RETRY_INITIAL_MS;
      if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
      historyRetryTimer = undefined;
      emitHistory();
      startTail(initial.latestCursor);
      if (hasOlder) void loadRemainingHistory();
      else compactManagedEnvelopeRetention(envelopes, seen);
      return true;
    })().finally(() => { loadingInitial = undefined; });
    return loadingInitial;
  };
  const retryWhenOnline = () => {
    if (controller.signal.aborted || (historyLoaded && !hasOlder)) return;
    if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
    historyRetryTimer = undefined;
    void (historyLoaded ? loadRemainingHistory() : loadInitial());
  };
  const loadOlderPage = (): Promise<boolean> => {
    if (!historyEnabled || !historyLoaded || !hasOlder || controller.signal.aborted) {
      return Promise.resolve(false);
    }
    if (loadingOlder) return loadingOlder;
    const before = olderBeforeCursor;
    if (before === undefined) {
      stopOlderPagination(undefined, "no decreasing before cursor was available");
      compactManagedEnvelopeRetention(envelopes, seen);
      return Promise.resolve(false);
    }
    loadingOlder = requestHistoryPage({ before, limit: MANAGED_HISTORY_PAGE_SIZE }).then((page) => {
      outageReported = false;
      historyRetryDelay = MANAGED_HISTORY_RETRY_INITIAL_MS;
      const nextBefore = oldestManagedCursor(page.data);
      const hasNewEnvelope = page.data.some((envelope) => !seen.has(envelope.cursor));
      if (page.hasMore && page.data.length === 0) {
        stopOlderPagination(before, "an empty page reported hasMore");
        compactManagedEnvelopeRetention(envelopes, seen);
        return false;
      }
      if (page.hasMore && !hasNewEnvelope) {
        stopOlderPagination(before, "a duplicate-only page reported hasMore");
        compactManagedEnvelopeRetention(envelopes, seen);
        return false;
      }
      if (page.hasMore && (
        nextBefore === undefined || compareManagedCursor(nextBefore, before) >= 0
      )) {
        stopOlderPagination(before, "the next before cursor did not strictly decrease");
        compactManagedEnvelopeRetention(envelopes, seen);
        return false;
      }

      let added = false;
      for (const envelope of page.data) added = retain(envelope) || added;
      if (added) envelopes.sort((left, right) => compareManagedCursor(left.cursor, right.cursor));
      hasOlder = page.hasMore;
      olderBeforeCursor = page.hasMore ? nextBefore : undefined;
      if (added) emitHistory();
      if (!hasOlder) compactManagedEnvelopeRetention(envelopes, seen);
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
      scheduleHistoryRetry();
    } finally {
      if (!hasOlder) compactManagedEnvelopeRetention(envelopes, seen);
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
    onEvent(listener: (event: AgentEvent) => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    onHistory(listener: (events: readonly AgentEvent[]) => void) {
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
      controller.abort();
      if (historyRetryTimer !== undefined) clearTimeout(historyRetryTimer);
      globalThis.removeEventListener?.("online", retryWhenOnline);
      listeners.clear();
      historyListeners.clear();
    },
  });
}

export async function managedHistoryPageAttempt<T>(
  load: (signal: AbortSignal) => Promise<T>,
  lifetimeSignal: AbortSignal,
  timeoutMs = MANAGED_HISTORY_ATTEMPT_TIMEOUT_MS,
): Promise<T> {
  if (lifetimeSignal.aborted) {
    throw lifetimeSignal.reason ?? new Error("managed history detached");
  }
  const attempt = new AbortController();
  let rejectBoundary!: (reason: unknown) => void;
  const boundary = new Promise<never>((_, reject) => { rejectBoundary = reject; });
  const abort = (reason: unknown) => {
    if (attempt.signal.aborted) return;
    attempt.abort(reason);
    rejectBoundary(reason);
  };
  const lifetimeAborted = () => abort(lifetimeSignal.reason ?? new Error("managed history detached"));
  const timeout = setTimeout(
    () => abort(new Error(`managed history request exceeded ${timeoutMs}ms`)),
    Math.max(0, timeoutMs),
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

function compactManagedEnvelopeRetention(envelopes: ManagedEvent[], seen: Set<string>): void {
  while (envelopes.length > MAX_MANAGED_RETAINED_ENVELOPES) {
    const groups = managedEnvelopeGroups(envelopes);
    const oversizedComplete = [...groups.values()].find((group) =>
      group.complete && group.envelopes.length > MAX_MANAGED_RETAINED_ENVELOPES
    );
    if (oversizedComplete) {
      removeManagedEnvelopes(
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
      .sort((left, right) => compareManagedCursor(left.oldestCursor, right.oldestCursor))[0];
    if (complete) {
      removeManagedEnvelopes(envelopes, seen, new Set(complete.envelopes));
      continue;
    }

    const removable = [...groups.values()]
      .flatMap((group) => group.envelopes.filter((envelope) => !group.mandatory.has(envelope)))
      .sort((left, right) => compareManagedCursor(left.cursor, right.cursor))[0];
    if (!removable) return;
    removeManagedEnvelopes(envelopes, seen, new Set([removable]));
  }
}

type ManagedEnvelopeGroup = Readonly<{
  envelopes: readonly ManagedEvent[];
  mandatory: ReadonlySet<ManagedEvent>;
  complete: boolean;
  oldestCursor: string;
}>;

function managedEnvelopeGroups(envelopes: readonly ManagedEvent[]): Map<string, ManagedEnvelopeGroup> {
  const grouped = new Map<string, ManagedEvent[]>();
  for (const envelope of envelopes) {
    const group = managedEnvelopeGroup(envelope);
    const retained = grouped.get(group) ?? [];
    retained.push(envelope);
    grouped.set(group, retained);
  }
  return new Map([...grouped].map(([group, retained]) => {
    retained.sort((left, right) => compareManagedCursor(left.cursor, right.cursor));
    const prompt = retained.find((envelope) => envelope.data.type === "turn_accepted");
    const terminal = [...retained].reverse().find(managedOuterTerminal);
    const mandatory = new Set<ManagedEvent>();
    if (prompt) mandatory.add(prompt);
    if (terminal) mandatory.add(terminal);
    return [group, Object.freeze({
      envelopes: retained,
      mandatory,
      complete: terminal !== undefined,
      oldestCursor: retained[0]!.cursor,
    })];
  }));
}

function managedOuterTerminal(envelope: ManagedEvent): boolean {
  return envelope.data.type === "turn_completed"
    || envelope.data.type === "turn_cancelled"
    || envelope.data.type === "turn_failed"
    || envelope.data.type === "stream_failed";
}

function removeManagedEnvelopes(
  envelopes: ManagedEvent[],
  seen: Set<string>,
  removed: ReadonlySet<ManagedEvent>,
): void {
  for (let index = envelopes.length - 1; index >= 0; index -= 1) {
    const envelope = envelopes[index]!;
    if (!removed.has(envelope)) continue;
    envelopes.splice(index, 1);
    seen.delete(envelope.cursor);
  }
}

function managedEnvelopeGroup(envelope: ManagedEvent): string {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return envelope.turnId ?? (typeof id === "string" ? id : `cursor:${envelope.cursor}`);
}

function managedEnvelopeTurnId(envelope: ManagedEvent): string | undefined {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return envelope.turnId ?? (typeof id === "string" ? id : undefined);
}

function compareManagedCursor(left: string, right: string): number {
  if (left.length !== right.length) return left.length - right.length;
  return left < right ? -1 : left > right ? 1 : 0;
}

function oldestManagedCursor(envelopes: readonly ManagedEvent[]): string | undefined {
  let oldest: string | undefined;
  for (const envelope of envelopes) {
    if (oldest === undefined || compareManagedCursor(envelope.cursor, oldest) < 0) {
      oldest = envelope.cursor;
    }
  }
  return oldest;
}

export function terminalEvent(
  envelope: ManagedEvent,
  sessionId: string,
  submitted: Set<string> | undefined,
  sequence: number,
): AgentEvent | undefined {
  if (envelope.data.type === "event") {
    const value = envelope.data.event;
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    const event = value as AgentEvent;
    return typeof event.type === "string" && event.payload && typeof event.payload === "object"
      ? {
          ...event,
          request_id: sessionId,
          seq: sequence,
          payload: {
            ...event.payload,
            ...(typeof envelope.cursor === "string" ? { managed_event_cursor: envelope.cursor } : {}),
            ...(envelope.turnId ? { turn_id: envelope.turnId } : {}),
          },
        }
      : undefined;
  }
  if (envelope.data.type !== "turn_accepted") {
    return undefined;
  }
  return {
    protocol_version: 1,
    request_id: sessionId,
    seq: sequence,
    type: "managed.prompt",
    payload: {
      text: promptText(envelope.data.input),
      turn_id: envelope.data.id,
    },
  };
}

export function managedHistoryEvents(
  envelopes: readonly ManagedEvent[],
  sessionId: string,
  submitted: Set<string> | undefined,
): readonly AgentEvent[] {
  const events: AgentEvent[] = [];
  const assistantTurns = rawAssistantMessageTurns(envelopes);
  for (const envelope of envelopes) {
    events.push(...managedEnvelopeEvents(
      envelope,
      assistantTurns,
      sessionId,
      submitted,
      events.length + 1,
    ));
  }
  return Object.freeze(events);
}

function managedEnvelopeEvents(
  envelope: ManagedEvent,
  rawAssistantTurns: ReadonlySet<string>,
  sessionId: string,
  submitted: Set<string> | undefined,
  firstSequence: number,
): AgentEvent[] {
  if (envelope.data.type === "event") {
    const projected = terminalEvent(envelope, sessionId, submitted, firstSequence);
    if (!projected || RAW_RUN_TERMINALS.has(projected.type)) return [];
    return [projected];
  }
  if (envelope.data.type === "turn_accepted") {
    const projected = terminalEvent(envelope, sessionId, submitted, firstSequence);
    return projected ? [projected] : [];
  }

  const turnId = terminalTurnId(envelope);
  if (envelope.data.type === "turn_completed") {
    const projected: AgentEvent[] = [];
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
      historyEvent(sessionId, firstSequence, "run.error", {
        message: envelope.data.error,
      }),
      historyEvent(sessionId, firstSequence + 1, "run.failed", {
        status: "failed",
        disposition: "stream_failed",
      }),
    ];
  }
  return [];
}

const RAW_RUN_TERMINALS = new Set(["run.error", "run.completed", "run.failed"]);

function terminalTurnId(envelope: ManagedEvent): string {
  const id = "id" in envelope.data ? envelope.data.id : undefined;
  return typeof id === "string" ? id : envelope.turnId ?? "unknown";
}

function rawAssistantMessageTurns(
  history: readonly ManagedEvent[],
): ReadonlySet<string> {
  const turns = new Set<string>();
  for (const candidate of history) {
    if (!candidate.turnId || candidate.data.type !== "event") continue;
    const event = candidate.data.event;
    if (
      event
      && typeof event === "object"
      && !Array.isArray(event)
      && (event as { type?: unknown }).type === "assistant.message"
    ) {
      turns.add(candidate.turnId);
    }
  }
  return turns;
}

function historyEvent(
  sessionId: string,
  seq: number,
  type: string,
  payload: Record<string, unknown>,
): AgentEvent {
  return { protocol_version: 1, request_id: sessionId, seq, type, payload };
}

function promptText(input: unknown): string {
  if (typeof input === "string") return input;
  if (!Array.isArray(input)) return "[prompt]";
  return input.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    const value = item as Record<string, unknown>;
    return value.type === "text" && typeof value.text === "string"
      ? [value.text]
      : value.type === "image"
        ? ["[image]"]
        : value.type === "audio"
          ? ["[audio]"]
          : [];
  }).join("\n");
}

function titleFromPrompt(input: string): string {
  const text = input.replace(/\s+/g, " ").trim();
  if (!text) return "";
  return text.length > 56 ? `${text.slice(0, 55).trimEnd()}…` : text;
}
