import { durabilityRevision } from "../runtime/durability-store.mjs";

const DATABASE_NAME = "nanocodex-browser-durability-v2";
const DATABASE_VERSION = 1;
const OWNERS = "owners";
const STATES = "states";
const MAX_REVISION = "18446744073709551615";

/** Creates the Worker-local IndexedDB durability capability. */
export function createIndexedDbDurabilityStore(options = {}) {
  const indexedDb = options.indexedDB ?? globalThis.indexedDB;
  if (!indexedDb || typeof indexedDb.open !== "function") {
    throw new TypeError("browser durability requires IndexedDB");
  }
  const databaseName = options.databaseName ?? DATABASE_NAME;
  requireId(databaseName, "browser durability database name");
  let database;
  const open = () => {
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
    async load(stateId) {
      requireId(stateId, "durability state ID");
      const db = await open();
      const transaction = db.transaction([STATES], "readonly");
      const state = await requestResult(transaction.objectStore(STATES).get(stateId));
      await transactionCompletion(transaction);
      return storedState(state, stateId);
    },

    async acquire(stateId, request) {
      requireId(stateId, "durability state ID");
      const ownerId = requireId(request?.ownerId, "durability owner ID");
      const db = await open();
      const transaction = db.transaction([OWNERS, STATES], "readwrite");
      const completed = transactionCompletion(transaction);
      try {
        const owners = transaction.objectStore(OWNERS);
        const previous = await requestResult(owners.get(stateId));
        const previousFence = previous === undefined ? "0" : storedOwner(previous, stateId).fence;
        if (previousFence === MAX_REVISION) throw new RangeError("IndexedDB durability fence overflow");
        const fence = durabilityRevision(BigInt(previousFence) + 1n);
        await requestResult(owners.put({ stateId, ownerId, fence }));
        const state = storedState(await requestResult(transaction.objectStore(STATES).get(stateId)), stateId);
        await completed;
        return Object.freeze({ ownerId, fence, ...state });
      } catch (error) {
        try { transaction.abort(); } catch {}
        await completed.catch(() => {});
        throw error;
      }
    },

    async replace(stateId, request) {
      requireId(stateId, "durability state ID");
      const ownerId = requireId(request?.ownerId, "durability owner ID");
      const fence = durabilityRevision(request?.fence);
      const db = await open();
      const transaction = db.transaction([OWNERS, STATES], "readwrite");
      const completed = transactionCompletion(transaction);
      try {
        const ownerValue = await requestResult(transaction.objectStore(OWNERS).get(stateId));
        const owner = ownerValue === undefined ? undefined : storedOwner(ownerValue, stateId);
        if (owner?.ownerId !== ownerId || durabilityRevision(owner?.fence ?? "0") !== fence) {
          await completed;
          return { status: "fenced" };
        }
        const expectedRevision = durabilityRevision(request?.expectedRevision);
        const states = transaction.objectStore(STATES);
        const state = storedState(await requestResult(states.get(stateId)), stateId);
        if (state.revision !== expectedRevision) {
          await completed;
          return { status: "conflict", actualRevision: state.revision };
        }
        if (expectedRevision === MAX_REVISION) {
          await completed;
          return { status: "not_committed", message: "IndexedDB durability revision overflow" };
        }
        const payload = requirePayload(request?.payload);
        const revision = durabilityRevision(BigInt(expectedRevision) + 1n);
        await requestResult(states.put({ stateId, revision, payload }));
        await completed;
        return { status: "replaced", revision };
      } catch (error) {
        try { transaction.abort(); } catch {}
        await completed.catch(() => {});
        throw error;
      }
    },
  });
}

function openDatabase(indexedDb, databaseName, invalidate) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const request = indexedDb.open(databaseName, DATABASE_VERSION);
    request.onupgradeneeded = () => {
      const database = request.result;
      database.createObjectStore(OWNERS, { keyPath: "stateId" });
      database.createObjectStore(STATES, { keyPath: "stateId" });
    };
    request.onerror = () => settle(reject, request.error ?? new Error("opening browser durability failed"));
    request.onblocked = () => settle(reject, new Error("opening browser durability was blocked"));
    request.onsuccess = () => {
      const opened = request.result;
      if (settled) return opened.close();
      const stores = Array.from(opened.objectStoreNames).sort();
      if (stores.length !== 2 || stores[0] !== OWNERS || stores[1] !== STATES
        || !validStoreSchema(opened, OWNERS) || !validStoreSchema(opened, STATES)) {
        opened.close();
        return settle(reject, new Error("incompatible IndexedDB durability schema; delete the database"));
      }
      settled = true;
      opened.onversionchange = () => { invalidate(); opened.close(); };
      opened.onclose = () => invalidate();
      resolve(opened);
    };
    function settle(callback, value) {
      if (settled) return;
      settled = true;
      callback(value);
    }
  });
}

function validStoreSchema(database, name) {
  try {
    const store = database.transaction([name], "readonly").objectStore(name);
    return store.keyPath === "stateId" && store.autoIncrement === false;
  } catch {
    return false;
  }
}

function storedState(value, stateId) {
  if (value === undefined) return Object.freeze({ revision: "0", payload: null });
  exactRecord(value, ["stateId", "revision", "payload"], "IndexedDB durability state");
  if (value.stateId !== stateId) throw new TypeError("IndexedDB durability state ID mismatch");
  return Object.freeze({
    revision: durabilityRevision(value.revision),
    payload: requirePayload(value.payload),
  });
}

function storedOwner(value, stateId) {
  exactRecord(value, ["stateId", "ownerId", "fence"], "IndexedDB durability owner");
  if (value.stateId !== stateId) throw new TypeError("IndexedDB durability owner ID mismatch");
  return Object.freeze({
    ownerId: requireId(value.ownerId, "durability owner ID"),
    fence: durabilityRevision(value.fence),
  });
}

function exactRecord(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const required = [...expected].sort();
  if (actual.length !== required.length || actual.some((key, index) => key !== required[index])) {
    throw new TypeError(`${label} has an invalid shape`);
  }
}

function requestResult(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB durability request failed"));
  });
}

function transactionCompletion(transaction) {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("IndexedDB durability transaction failed"));
    transaction.onabort = () => reject(transaction.error ?? new Error("IndexedDB durability transaction was aborted"));
  });
}

function requireId(value, label) {
  if (typeof value !== "string" || !value.trim()) throw new TypeError(`${label} must be a non-empty string`);
  return value;
}

function requirePayload(value) {
  if (typeof value !== "string") throw new TypeError("durability payload must be a string");
  return value;
}
