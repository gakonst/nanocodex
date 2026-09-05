const DATABASE_NAME = "nanocodex-local-transcripts-v1";
const DATABASE_VERSION = 4;
const TURNS_STORE = "turns";
const SESSIONS_STORE = "sessions";
const THREAD_ORDER_INDEX = "thread-order";
const THREAD_STATUS_INDEX = "thread-status";

export const MAX_LOCAL_TRANSCRIPT_TURNS = 100;
export const MAX_LOCAL_UNFINISHED_TURNS = 32;
export const MAX_LOCAL_TRANSCRIPT_STEERS = 32;

export type LocalTranscriptTurnStatus =
  | "pending"
  | "retryable"
  | "blocked"
  | "reopen_required"
  | "completed"
  | "cancelled"
  | "failed";

export type LocalTranscriptSteer = Readonly<{
  id: string;
  text: string;
  status: "pending" | "accepted" | "rejected";
  error?: string;
}>;

export type LocalTranscriptTurn = Readonly<{
  threadId: string;
  turnId: string;
  createdAt: number;
  prompt?: string;
  steers?: readonly LocalTranscriptSteer[];
  assistant?: string;
  status?: LocalTranscriptTurnStatus;
  error?: string;
  cancelRequested?: boolean;
}>;

export type LocalTranscriptLoad = Readonly<{
  initialized: boolean;
  turns: readonly LocalTranscriptTurn[];
}>;

export type LocalTranscriptTransition = Readonly<{
  applied: boolean;
  turn: LocalTranscriptTurn;
}>;

export type LocalTranscriptJournal = Readonly<{
  watch(threadId: string, listener: () => void): () => void;
  load(threadId: string): Promise<LocalTranscriptLoad>;
  bootstrap(threadId: string, turns: readonly LocalTranscriptTurn[]): Promise<void>;
  recordPrompt(turn: LocalTranscriptTurn): Promise<void>;
  appendSteer(
    turn: LocalTranscriptTurn,
    steer: LocalTranscriptSteer,
  ): Promise<LocalTranscriptTransition>;
  updateSteer(
    turn: LocalTranscriptTurn,
    steerId: string,
    update: Readonly<{ status: "accepted" | "rejected"; error?: string }>,
  ): Promise<LocalTranscriptTransition>;
  requestCancel(turn: LocalTranscriptTurn): Promise<LocalTranscriptTransition>;
  completeTurn(turn: LocalTranscriptTurn): Promise<LocalTranscriptTransition>;
  updateTurn(
    turn: LocalTranscriptTurn,
    update: Readonly<{ status: LocalTranscriptTurnStatus; error?: string }>,
  ): Promise<LocalTranscriptTransition>;
}>;

type StoredTurn = LocalTranscriptTurn & Readonly<{ order: string; sequence: number }>;

type TranscriptBroadcastChannel = Pick<BroadcastChannel, "close" | "postMessage"> & {
  onmessage: ((event: MessageEvent<unknown>) => void) | null;
};

type TranscriptBroadcastChannelConstructor = new (name: string) => TranscriptBroadcastChannel;

/** Browser-owned transcript durability, separate from the model's compactable context. */
export function createLocalTranscriptJournal(options: {
  indexedDB?: IDBFactory;
  keyRange?: typeof IDBKeyRange;
  databaseName?: string;
  broadcastChannel?: TranscriptBroadcastChannelConstructor | null;
} = {}): LocalTranscriptJournal {
  const indexedDb = options.indexedDB ?? globalThis.indexedDB;
  const keyRange = options.keyRange ?? globalThis.IDBKeyRange;
  const databaseName = options.databaseName ?? DATABASE_NAME;
  const Broadcast = options.broadcastChannel === null
    ? undefined
    : options.broadcastChannel ?? globalThis.BroadcastChannel;
  const broadcasts = new Map<string, Readonly<{
    channel: TranscriptBroadcastChannel;
    listeners: Set<() => void>;
  }>>();
  let database: Promise<IDBDatabase> | undefined;

  const notifyThread = (threadId: string) => {
    const active = broadcasts.get(threadId);
    if (active) {
      notifyJournalListeners(active.listeners);
      active.channel.postMessage({ type: "nanocodex.local-transcript.changed", threadId });
      return;
    }
    if (!Broadcast) return;
    const channel = new Broadcast(broadcastName(databaseName, threadId));
    channel.postMessage({ type: "nanocodex.local-transcript.changed", threadId });
    channel.close();
  };

  const open = () => {
    if (!indexedDb || !keyRange) {
      return Promise.reject(new Error("local transcript storage requires IndexedDB"));
    }
    if (database) return database;
    const opening = openDatabase(indexedDb, databaseName, () => {
      if (database === opening) database = undefined;
    }).catch((error) => {
      if (database === opening) database = undefined;
      throw error;
    });
    database = opening;
    return opening;
  };

  return Object.freeze({
    watch(threadId, listener) {
      if (!Broadcast) return () => {};
      let active = broadcasts.get(threadId);
      if (!active) {
        const listeners = new Set<() => void>();
        const channel = new Broadcast(broadcastName(databaseName, threadId));
        channel.onmessage = (event) => {
          const message = event.data;
          if (!message || typeof message !== "object" || Array.isArray(message)) return;
          const value = message as Record<string, unknown>;
          if (value.type !== "nanocodex.local-transcript.changed" || value.threadId !== threadId) return;
          notifyJournalListeners(listeners);
        };
        active = Object.freeze({ channel, listeners });
        broadcasts.set(threadId, active);
      }
      active.listeners.add(listener);
      let watching = true;
      return () => {
        if (!watching) return;
        watching = false;
        active?.listeners.delete(listener);
        if (active?.listeners.size === 0 && broadcasts.get(threadId) === active) {
          broadcasts.delete(threadId);
          active.channel.close();
        }
      };
    },

    async load(threadId) {
      const db = await open();
      const transaction = db.transaction([SESSIONS_STORE, TURNS_STORE], "readonly");
      const completed = transactionCompletion(transaction);
      const initialized = requestResult<{ initialized?: unknown } | undefined>(
        transaction.objectStore(SESSIONS_STORE).get(threadId),
      );
      const range = keyRange.bound([threadId, ""], [threadId, "\uffff"]);
      const turns = recentTurns(
        transaction.objectStore(TURNS_STORE).index(THREAD_ORDER_INDEX),
        range,
        threadId,
      );
      const [session, recent] = await Promise.all([initialized, turns, completed]);
      return Object.freeze({
        initialized: session?.initialized === true,
        turns: Object.freeze(recent),
      });
    },

    async bootstrap(threadId, turns) {
      const db = await open();
      const transaction = db.transaction([SESSIONS_STORE, TURNS_STORE], "readwrite");
      const completed = transactionCompletion(transaction);
      const sessions = transaction.objectStore(SESSIONS_STORE);
      const storedTurns = transaction.objectStore(TURNS_STORE);
      const initialized = new Promise<void>((resolve, reject) => {
        const request = sessions.get(threadId);
        request.onerror = () => reject(request.error ?? new Error("reading transcript bootstrap state failed"));
        request.onsuccess = () => {
          if (request.result?.initialized === true) {
            resolve();
            return;
          }
          const bootstrapTurns = turns.slice(-MAX_LOCAL_TRANSCRIPT_TURNS);
          if (bootstrapTurns.length === 0) {
            markInitialized();
            return;
          }
          let remaining = bootstrapTurns.length;
          for (const [index, turn] of bootstrapTurns.entries()) {
            const existing = storedTurns.get([turn.threadId, turn.turnId]);
            existing.onerror = () => reject(existing.error ?? new Error("reading transcript bootstrap turn failed"));
            existing.onsuccess = () => {
              if (existing.result !== undefined) {
                finishTurn();
                return;
              }
              const added = storedTurns.add(storedTurn(turn, true, index + 1));
              added.onerror = () => reject(added.error ?? new Error("writing transcript bootstrap turn failed"));
              added.onsuccess = finishTurn;
            };
          }

          function finishTurn() {
            remaining -= 1;
            if (remaining === 0) markInitialized();
          }

          function markInitialized() {
            let prior;
            try {
              prior = storedSession(request.result, threadId);
            } catch (error) {
              reject(error);
              transaction.abort();
              return;
            }
            const marked = sessions.put({
              ...prior.value,
              threadId,
              initialized: true,
              nextSequence: prior.nextSequence,
            });
            marked.onerror = () => reject(marked.error ?? new Error("writing transcript bootstrap state failed"));
            marked.onsuccess = () => resolve();
          }
        };
      });
      await Promise.all([initialized, completed]);
      notifyThread(threadId);
    },

    async recordPrompt(turn) {
      const db = await open();
      const transaction = db.transaction([SESSIONS_STORE, TURNS_STORE], "readwrite");
      const completed = transactionCompletion(transaction);
      const sessions = transaction.objectStore(SESSIONS_STORE);
      const turns = transaction.objectStore(TURNS_STORE);
      const recorded = new Promise<void>((resolve, reject) => {
        const request = sessions.get(turn.threadId);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript sequence failed"));
        request.onsuccess = () => {
          let session;
          try {
            session = storedSession(request.result, turn.threadId);
          } catch (error) {
            reject(error);
            transaction.abort();
            return;
          }
          if (session.nextSequence >= Number.MAX_SAFE_INTEGER) {
            reject(new Error("local transcript sequence is exhausted"));
            transaction.abort();
            return;
          }
          void countUnfinishedTurns(turns.index(THREAD_STATUS_INDEX), keyRange, turn.threadId)
            .then((unfinished) => {
              if (unfinished >= MAX_LOCAL_UNFINISHED_TURNS) {
                reject(new Error(
                  `local thread ${turn.threadId} already has ${MAX_LOCAL_UNFINISHED_TURNS} unfinished turns; recover or replace it before submitting more work`,
                ));
                transaction.abort();
                return;
              }
              const sequence = session.nextSequence + 1;
              const added = turns.add(storedTurn({ ...turn, status: "pending" }, false, sequence));
              added.onerror = () => reject(added.error ?? new Error("writing local transcript prompt failed"));
              added.onsuccess = () => {
                const advanced = sessions.put({ ...session.value, threadId: turn.threadId, nextSequence: sequence });
                advanced.onerror = () => reject(advanced.error ?? new Error("writing local transcript sequence failed"));
                advanced.onsuccess = () => resolve();
              };
            }, (error) => {
              reject(error);
              transaction.abort();
            });
        };
      });
      await Promise.all([recorded, completed]);
      notifyThread(turn.threadId);
    },

    async completeTurn(turn) {
      const db = await open();
      const transaction = db.transaction(TURNS_STORE, "readwrite");
      const completed = transactionCompletion(transaction);
      const turns = transaction.objectStore(TURNS_STORE);
      const updated = new Promise<LocalTranscriptTransition>((resolve, reject) => {
        const request = turns.get([turn.threadId, turn.turnId]);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript turn failed"));
        request.onsuccess = () => {
          if (!request.result) {
            reject(new Error("cannot complete a local turn whose prompt was not persisted"));
            transaction.abort();
            return;
          }
          const current = decodeRequiredTurn(request.result, turn.threadId);
          if (terminalStatus(current.status)) {
            resolve(Object.freeze({ applied: false, turn: current }));
            return;
          }
          const next = {
            ...request.result,
            assistant: turn.assistant,
            status: "completed",
            error: undefined,
          };
          const put = turns.put(next);
          put.onerror = () => reject(put.error ?? new Error("writing local transcript completion failed"));
          put.onsuccess = () => resolve(Object.freeze({
            applied: true,
            turn: decodeRequiredTurn(next, turn.threadId),
          }));
        };
      });
      const pruning = updated.then((transition) => transition.applied
        && terminalStatus(transition.turn.status)
        ? pruneTerminalTurns(
          turns.index(THREAD_ORDER_INDEX),
          keyRange.bound([turn.threadId, ""], [turn.threadId, "\uffff"]),
          turn.threadId,
        )
        : undefined);
      const [transition] = await Promise.all([updated, pruning, completed]);
      if (transition.applied) notifyThread(turn.threadId);
      return transition;
    },

    async appendSteer(turn, steer) {
      if (steer.status !== "pending") {
        throw new Error("a local steering intent must be reserved as pending before dispatch");
      }
      const db = await open();
      const transaction = db.transaction(TURNS_STORE, "readwrite");
      const completed = transactionCompletion(transaction);
      const turns = transaction.objectStore(TURNS_STORE);
      const updated = new Promise<LocalTranscriptTransition>((resolve, reject) => {
        const request = turns.get([turn.threadId, turn.turnId]);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript turn failed"));
        request.onsuccess = () => {
          if (!request.result) {
            reject(new Error("cannot append steering to a local turn whose prompt was not persisted"));
            transaction.abort();
            return;
          }
          const current = decodeRequiredTurn(request.result, turn.threadId);
          const steers = current.steers ?? [];
          const existing = steers.find((candidate) => candidate.id === steer.id);
          if (existing) {
            if (existing.text !== steer.text) {
              reject(new Error(`persisted local steer ${steer.id} conflicts with the accepted input`));
              transaction.abort();
              return;
            }
            resolve(Object.freeze({ applied: false, turn: current }));
            return;
          }
          if (steers.length >= MAX_LOCAL_TRANSCRIPT_STEERS) {
            reject(new Error(
              `durable turn ${turn.turnId} already has ${MAX_LOCAL_TRANSCRIPT_STEERS} retained steers`,
            ));
            transaction.abort();
            return;
          }
          const next = { ...request.result, steers: [...steers, steer] };
          const put = turns.put(next);
          put.onerror = () => reject(put.error ?? new Error("writing local transcript steering failed"));
          put.onsuccess = () => resolve(Object.freeze({
            applied: true,
            turn: decodeRequiredTurn(next, turn.threadId),
          }));
        };
      });
      const [transition] = await Promise.all([updated, completed]);
      if (transition.applied) notifyThread(turn.threadId);
      return transition;
    },

    async updateSteer(turn, steerId, update) {
      const db = await open();
      const transaction = db.transaction(TURNS_STORE, "readwrite");
      const completed = transactionCompletion(transaction);
      const turns = transaction.objectStore(TURNS_STORE);
      const updated = new Promise<LocalTranscriptTransition>((resolve, reject) => {
        const request = turns.get([turn.threadId, turn.turnId]);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript turn failed"));
        request.onsuccess = () => {
          if (!request.result) {
            reject(new Error("cannot update steering for a local turn whose prompt was not persisted"));
            transaction.abort();
            return;
          }
          const current = decodeRequiredTurn(request.result, turn.threadId);
          const index = current.steers?.findIndex((candidate) => candidate.id === steerId) ?? -1;
          if (index < 0) {
            reject(new Error(`cannot update local steer ${steerId} before its intent is persisted`));
            transaction.abort();
            return;
          }
          const steer = current.steers![index]!;
          if (steer.status !== "pending") {
            if (steer.status !== update.status) {
              reject(new Error(`persisted local steer ${steerId} is already ${steer.status}`));
              transaction.abort();
              return;
            }
            resolve(Object.freeze({ applied: false, turn: current }));
            return;
          }
          const steers = current.steers!.slice();
          steers[index] = Object.freeze({
            ...steer,
            status: update.status,
            ...(update.error === undefined ? {} : { error: update.error }),
          });
          const next = { ...request.result, steers };
          const put = turns.put(next);
          put.onerror = () => reject(put.error ?? new Error("writing local transcript steering status failed"));
          put.onsuccess = () => resolve(Object.freeze({
            applied: true,
            turn: decodeRequiredTurn(next, turn.threadId),
          }));
        };
      });
      const [transition] = await Promise.all([updated, completed]);
      if (transition.applied) notifyThread(turn.threadId);
      return transition;
    },

    async requestCancel(turn) {
      const db = await open();
      const transaction = db.transaction(TURNS_STORE, "readwrite");
      const completed = transactionCompletion(transaction);
      const turns = transaction.objectStore(TURNS_STORE);
      const updated = new Promise<LocalTranscriptTransition>((resolve, reject) => {
        const request = turns.get([turn.threadId, turn.turnId]);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript turn failed"));
        request.onsuccess = () => {
          if (!request.result) {
            reject(new Error("cannot cancel a local turn whose prompt was not persisted"));
            transaction.abort();
            return;
          }
          const current = decodeRequiredTurn(request.result, turn.threadId);
          if (terminalStatus(current.status) || current.cancelRequested) {
            resolve(Object.freeze({ applied: false, turn: current }));
            return;
          }
          const next = { ...request.result, cancelRequested: true };
          const put = turns.put(next);
          put.onerror = () => reject(put.error ?? new Error("writing local transcript cancellation intent failed"));
          put.onsuccess = () => resolve(Object.freeze({
            applied: true,
            turn: decodeRequiredTurn(next, turn.threadId),
          }));
        };
      });
      const [transition] = await Promise.all([updated, completed]);
      if (transition.applied) notifyThread(turn.threadId);
      return transition;
    },

    async updateTurn(turn, update) {
      const db = await open();
      const transaction = db.transaction(TURNS_STORE, "readwrite");
      const completed = transactionCompletion(transaction);
      const turns = transaction.objectStore(TURNS_STORE);
      const updated = new Promise<LocalTranscriptTransition>((resolve, reject) => {
        const request = turns.get([turn.threadId, turn.turnId]);
        request.onerror = () => reject(request.error ?? new Error("reading local transcript turn failed"));
        request.onsuccess = () => {
          if (!request.result) {
            reject(new Error("cannot update a local turn whose prompt was not persisted"));
            transaction.abort();
            return;
          }
          const current = decodeRequiredTurn(request.result, turn.threadId);
          if (terminalStatus(current.status)) {
            resolve(Object.freeze({ applied: false, turn: current }));
            return;
          }
          const next = {
            ...request.result,
            status: update.status,
            error: update.error,
          };
          const put = turns.put(next);
          put.onerror = () => reject(put.error ?? new Error("writing local transcript status failed"));
          put.onsuccess = () => resolve(Object.freeze({
            applied: true,
            turn: decodeRequiredTurn(next, turn.threadId),
          }));
        };
      });
      const pruning = updated.then((transition) => transition.applied
        && terminalStatus(transition.turn.status)
        ? pruneTerminalTurns(
          turns.index(THREAD_ORDER_INDEX),
          keyRange.bound([turn.threadId, ""], [turn.threadId, "\uffff"]),
          turn.threadId,
        )
        : undefined);
      const [transition] = await Promise.all([updated, pruning, completed]);
      if (transition.applied) notifyThread(turn.threadId);
      return transition;
    },
  });
}

function openDatabase(
  indexedDb: IDBFactory,
  databaseName: string,
  invalidate: () => void,
): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let upgradeError: Error | undefined;
    const request = indexedDb.open(databaseName, DATABASE_VERSION);
    request.onupgradeneeded = (event) => {
      const database = request.result;
      if (!database.objectStoreNames.contains(SESSIONS_STORE)) {
        database.createObjectStore(SESSIONS_STORE, { keyPath: "threadId" });
      }
      if (!database.objectStoreNames.contains(TURNS_STORE)) {
        const turns = database.createObjectStore(TURNS_STORE, {
          keyPath: ["threadId", "turnId"],
        });
        turns.createIndex(THREAD_ORDER_INDEX, ["threadId", "order"], { unique: false });
        turns.createIndex(THREAD_STATUS_INDEX, ["threadId", "status"], { unique: false });
      } else {
        const turns = request.transaction?.objectStore(TURNS_STORE);
        if (turns && !turns.indexNames.contains(THREAD_ORDER_INDEX)) {
          turns.createIndex(THREAD_ORDER_INDEX, ["threadId", "order"], { unique: false });
        }
        if (turns && !turns.indexNames.contains(THREAD_STATUS_INDEX)) {
          turns.createIndex(THREAD_STATUS_INDEX, ["threadId", "status"], { unique: false });
        }
      }
      if (event.oldVersion < 4) {
        const turns = request.transaction?.objectStore(TURNS_STORE);
        const cursorRequest = turns?.openCursor();
        if (turns && cursorRequest) {
          const terminalTurnsByThread = new Map<string, Record<string, unknown>[]>();
          const retainedTurns: Record<string, unknown>[] = [];
          cursorRequest.onsuccess = () => {
            const cursor = cursorRequest.result;
            if (!cursor) {
              turns.clear();
              for (const retained of retainedTurns) turns.put(retained);
              for (const terminalTurns of terminalTurnsByThread.values()) {
                for (const retained of terminalTurns) turns.put(retained);
              }
              return;
            }
            const value = cursor.value;
            if (value && typeof value === "object" && !Array.isArray(value)
              && typeof value.threadId === "string"
              && typeof value.order === "string") {
              const status = value.status
                ?? (value.assistant === undefined ? "pending" : "completed");
              const normalized = value.status === undefined ? { ...value, status } : value;
              if (terminalStatus(status)) {
                const terminalTurns = terminalTurnsByThread.get(value.threadId) ?? [];
                terminalTurns.push(normalized);
                terminalTurns.sort((left, right) =>
                  indexedDb.cmp(String(right.order), String(left.order))
                );
                if (terminalTurns.length > MAX_LOCAL_TRANSCRIPT_TURNS) terminalTurns.pop();
                terminalTurnsByThread.set(value.threadId, terminalTurns);
              } else {
                retainedTurns.push(normalized);
              }
            } else {
              // A row omitted by the order index cannot participate in ordered
              // recovery. Abort the owning upgrade and preserve the old database
              // byte-for-byte instead of clearing evidence that loading cannot see.
              upgradeError = new Error(
                "persisted local transcript contains a row that cannot be migrated safely",
              );
              request.transaction?.abort();
              return;
            }
            cursor.continue();
          };
        }
      }
      if (event.oldVersion === 1) {
        const sessions = request.transaction?.objectStore(SESSIONS_STORE);
        const cursorRequest = sessions?.openCursor();
        if (cursorRequest) {
          cursorRequest.onsuccess = () => {
            const cursor = cursorRequest.result;
            if (!cursor) return;
            const value = cursor.value;
            if (value && typeof value === "object" && !Array.isArray(value)
              && value.nextSequence === undefined) {
              cursor.update({ ...value, nextSequence: 0 });
            }
            cursor.continue();
          };
        }
      }
    };
    request.onerror = () => rejectOnce(
      upgradeError ?? request.error ?? new Error("opening local transcript storage failed"),
    );
    request.onblocked = () => rejectOnce(new Error("opening local transcript storage was blocked"));
    request.onsuccess = () => {
      const database = request.result;
      if (settled) {
        database.close();
        return;
      }
      settled = true;
      database.onversionchange = () => {
        invalidate();
        database.close();
      };
      resolve(database);
    };
    function rejectOnce(error: unknown) {
      if (settled) return;
      settled = true;
      reject(error);
    }
  });
}

function recentTurns(
  index: IDBIndex,
  range: IDBKeyRange,
  threadId: string,
): Promise<LocalTranscriptTurn[]> {
  return new Promise((resolve, reject) => {
    const turns: LocalTranscriptTurn[] = [];
    let terminalTurns = 0;
    let unfinishedTurns = 0;
    const request = index.openCursor(range, "prev");
    request.onerror = () => reject(request.error ?? new Error("reading local transcript failed"));
    request.onsuccess = () => {
      const cursor = request.result;
      if (!cursor) {
        turns.reverse();
        resolve(turns);
        return;
      }
      const turn = decodeTurn(cursor.value, threadId);
      if (!turn) {
        reject(new Error(`persisted local transcript row for thread ${threadId} is invalid`));
        return;
      }
      if (terminalStatus(turn.status)) {
        terminalTurns += 1;
        if (terminalTurns <= MAX_LOCAL_TRANSCRIPT_TURNS) turns.push(turn);
      } else {
        unfinishedTurns += 1;
        if (unfinishedTurns > MAX_LOCAL_UNFINISHED_TURNS) {
          reject(new Error(
            `local thread ${threadId} exceeds the ${MAX_LOCAL_UNFINISHED_TURNS}-turn unfinished recovery limit; recover or replace it`,
          ));
          return;
        }
        turns.push(turn);
      }
      cursor.continue();
    };
  });
}

async function countUnfinishedTurns(
  index: IDBIndex,
  keyRange: typeof IDBKeyRange,
  threadId: string,
): Promise<number> {
  const statuses: readonly LocalTranscriptTurnStatus[] = [
    "pending",
    "retryable",
    "blocked",
    "reopen_required",
  ];
  const counts = await Promise.all(statuses.map((status) => requestResult<number>(
    index.count(keyRange.only([threadId, status])),
  )));
  return counts.reduce((total, count) => total + count, 0);
}

function pruneTerminalTurns(
  index: IDBIndex,
  range: IDBKeyRange,
  threadId: string,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let terminalTurns = 0;
    const request = index.openCursor(range, "prev");
    request.onerror = () => reject(request.error ?? new Error("pruning local transcript failed"));
    request.onsuccess = () => {
      const cursor = request.result;
      if (!cursor) {
        resolve();
        return;
      }
      const turn = decodeTurn(cursor.value, threadId);
      if (!turn) {
        reject(new Error(`persisted local transcript row for thread ${threadId} is invalid`));
        return;
      }
      if (!terminalStatus(turn.status)) {
        cursor.continue();
        return;
      }
      terminalTurns += 1;
      if (terminalTurns <= MAX_LOCAL_TRANSCRIPT_TURNS) {
        cursor.continue();
        return;
      }
      const deleted = cursor.delete();
      deleted.onerror = () => reject(deleted.error ?? new Error("deleting expired local transcript turn failed"));
      deleted.onsuccess = () => cursor.continue();
    };
  });
}

function storedTurn(turn: LocalTranscriptTurn, bootstrap = false, sequence = 0): StoredTurn {
  // A first-run context import must sort before prompts that race with it. The
  // live prefix sorts after the v1 wall-clock records (`~:`). Live ordering is
  // allocated atomically from the per-thread session row, never from a clock or
  // UUID. Existing v1 rows retain their relative position during the upgrade.
  const prefix = bootstrap ? "!" : "~~";
  return Object.freeze({
    ...turn,
    sequence,
    order: `${prefix}:${String(sequence).padStart(16, "0")}`,
  });
}

function decodeTurn(value: unknown, threadId: string): LocalTranscriptTurn | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const turn = value as Partial<LocalTranscriptTurn>;
  if (turn.threadId !== threadId || typeof turn.turnId !== "string" || !turn.turnId
    || typeof turn.createdAt !== "number" || !Number.isFinite(turn.createdAt)
    || (turn.prompt !== undefined && typeof turn.prompt !== "string")
    || (turn.steers !== undefined && !validSteers(turn.steers))
    || (turn.assistant !== undefined && typeof turn.assistant !== "string")
    || (turn.error !== undefined && typeof turn.error !== "string")
    || (turn.cancelRequested !== undefined && typeof turn.cancelRequested !== "boolean")
    || (turn.status !== undefined && !turnStatus(turn.status))) return undefined;
  const status = turn.status ?? (turn.assistant === undefined ? "pending" : "completed");
  return Object.freeze({
    threadId,
    turnId: turn.turnId,
    createdAt: turn.createdAt,
    ...(turn.prompt === undefined ? {} : { prompt: turn.prompt }),
    ...(turn.steers === undefined ? {} : { steers: Object.freeze(turn.steers.map((steer) => Object.freeze({
      ...steer,
      status: steer.status ?? "accepted",
    }))) }),
    ...(turn.assistant === undefined ? {} : { assistant: turn.assistant }),
    status,
    ...(turn.error === undefined ? {} : { error: turn.error }),
    ...(turn.cancelRequested === undefined ? {} : { cancelRequested: turn.cancelRequested }),
  });
}

function decodeRequiredTurn(value: unknown, threadId: string): LocalTranscriptTurn {
  const turn = decodeTurn(value, threadId);
  if (!turn) throw new Error("persisted local transcript turn is invalid");
  return turn;
}

function storedSession(value: unknown, threadId: string): {
  nextSequence: number;
  value: Record<string, unknown>;
} {
  if (value === undefined) return { nextSequence: 0, value: { threadId } };
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`persisted local transcript session for thread ${threadId} is invalid`);
  }
  const session = value as Record<string, unknown>;
  const candidate = session.nextSequence;
  if (session.threadId !== threadId
    || (session.initialized !== undefined && typeof session.initialized !== "boolean")
    || typeof candidate !== "number"
    || !Number.isSafeInteger(candidate)
    || candidate < 0) {
    throw new Error(`persisted local transcript session for thread ${threadId} is invalid`);
  }
  return {
    nextSequence: candidate,
    value: session,
  };
}

function turnStatus(value: unknown): value is LocalTranscriptTurnStatus {
  return value === "pending" || value === "retryable" || value === "blocked"
    || value === "reopen_required" || value === "completed" || value === "cancelled"
    || value === "failed";
}

function terminalStatus(
  status: LocalTranscriptTurnStatus | undefined,
): status is "completed" | "cancelled" | "failed" {
  return status === "completed" || status === "cancelled" || status === "failed";
}

function validSteers(value: unknown): value is readonly LocalTranscriptSteer[] {
  return Array.isArray(value)
    && value.length <= MAX_LOCAL_TRANSCRIPT_STEERS
    && value.every((steer) => steer
      && typeof steer === "object"
      && !Array.isArray(steer)
      && typeof steer.id === "string"
      && steer.id.length > 0
      && typeof steer.text === "string"
      && (steer.status === undefined || steer.status === "pending"
        || steer.status === "accepted" || steer.status === "rejected")
      && (steer.error === undefined || typeof steer.error === "string"));
}

function broadcastName(databaseName: string, threadId: string): string {
  return `nanocodex-local-transcript:${databaseName}:${threadId}`;
}

function notifyJournalListeners(listeners: ReadonlySet<() => void>): void {
  for (const listener of listeners) {
    try {
      listener();
    } catch (error) {
      console.error("local transcript observer failed", error);
    }
  }
}

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("local transcript request failed"));
  });
}

function transactionCompletion(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("local transcript transaction failed"));
    transaction.onabort = () => reject(transaction.error ?? new Error("local transcript transaction aborted"));
  });
}
