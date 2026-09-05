import assert from "node:assert/strict";
import { test } from "node:test";

import { createIndexedDbDurabilityStore } from "../browser/indexeddb-durability-store.mjs";

test("IndexedDB durability validates one complete retained state", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "ordered" });

  assert.deepEqual(await store.load("thread"), { revision: "0", payload: null });
  indexedDB.seed("ordered", "thread", "10", "ten");
  assert.deepEqual(await store.load("thread"), {
    revision: "10",
    payload: "ten",
  });
  const owner = await store.acquire("thread", { ownerId: "owner-1" });
  await assert.rejects(
    store.replace("thread", {
      ownerId: owner.ownerId,
      fence: owner.fence,
      expectedRevision: "01",
      payload: "invalid",
    }),
    /unsigned 64-bit decimal string/,
  );
  await assert.rejects(
    store.replace("thread", {
      ownerId: owner.ownerId,
      fence: owner.fence,
      expectedRevision: "10",
      payload: new Uint8Array(),
    }),
    /payload must be a string/,
  );
});

test("IndexedDB durability serializes atomic compare-and-replace transactions", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "atomic" });
  const owner = await store.acquire("thread", { ownerId: "owner-1" });
  const results = await Promise.all([
    store.replace("thread", { ...owner, expectedRevision: "0", payload: "left" }),
    store.replace("thread", { ...owner, expectedRevision: "0", payload: "right" }),
  ]);

  assert.equal(results.filter(({ status }) => status === "replaced").length, 1);
  assert.deepEqual(results.find(({ status }) => status === "conflict"), {
    status: "conflict",
    actualRevision: "1",
  });
  const state = await store.load("thread");
  assert.equal(state.revision, "1");
  assert.ok(state.payload === "left" || state.payload === "right");

  indexedDB.failNextStatePut("atomic");
  await assert.rejects(
    store.replace("thread", { ...owner, expectedRevision: "1", payload: "rolled back" }),
    /injected state failure/,
  );
  assert.deepEqual(await store.load("thread"), state);

  indexedDB.resetContent("atomic", "thread");
  const replacement = await store.acquire("thread", { ownerId: "owner-2" });
  assert.deepEqual(replacement, {
    ownerId: "owner-2",
    fence: "2",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(
    await store.replace("thread", { ...owner, expectedRevision: "0", payload: "stale" }),
    { status: "fenced" },
  );
  assert.deepEqual(
    await store.replace("thread", {
      ...owner,
      expectedRevision: "not-a-revision",
      payload: new Uint8Array(),
    }),
    { status: "fenced" },
  );
  assert.deepEqual(
    await store.replace("thread", {
      ...replacement,
      expectedRevision: "1",
      payload: new Uint8Array(),
    }),
    { status: "conflict", actualRevision: "0" },
  );
});

test("IndexedDB durability atomically increments concurrent owner acquisitions", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "owners" });
  const [first, second] = await Promise.all([
    store.acquire("thread", { ownerId: "owner-1" }),
    store.acquire("thread", { ownerId: "owner-2" }),
  ]);

  assert.equal(first.fence, "1");
  assert.equal(second.fence, "2");
  assert.deepEqual(
    await store.replace("thread", { ...first, expectedRevision: "99", payload: "stale" }),
    { status: "fenced" },
  );
  assert.deepEqual(
    await store.replace("thread", { ...second, expectedRevision: "0", payload: "current" }),
    { status: "replaced", revision: "1" },
  );
});

test("IndexedDB durability reports u64 overflow without committing", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "overflow" });
  const maximum = "18446744073709551615";
  await store.load("thread");
  indexedDB.seed("overflow", "thread", maximum, "last");
  const owner = await store.acquire("thread", { ownerId: "owner" });

  assert.deepEqual(
    await store.replace("thread", { ...owner, expectedRevision: maximum, payload: "never" }),
    {
      status: "not_committed",
      message: "IndexedDB durability revision overflow",
    },
  );
  assert.deepEqual(await store.load("thread"), {
    revision: maximum,
    payload: "last",
  });
});

test("IndexedDB durability creates only owner and state stores", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "upgrade" });
  await store.load("thread");
  assert.deepEqual(indexedDB.storeNames("upgrade"), ["owners", "states"]);
});

test("IndexedDB durability rejects stores with incompatible key semantics", async () => {
  const indexedDB = createFakeIndexedDb();
  await createIndexedDbDurabilityStore({ indexedDB, databaseName: "schema" }).load("thread");
  indexedDB.alterStore("schema", "owners", { keyPath: "ownerId" });

  await assert.rejects(
    createIndexedDbDurabilityStore({ indexedDB, databaseName: "schema" }).load("thread"),
    /incompatible IndexedDB durability schema/,
  );
});

test("IndexedDB durability has no browser-global import-time dependency", () => {
  assert.throws(() => createIndexedDbDurabilityStore(), /requires IndexedDB/);
});

test("IndexedDB durability retries failed opens and reopens retained state after close", async () => {
  const indexedDB = createFakeIndexedDb();
  const store = createIndexedDbDurabilityStore({ indexedDB, databaseName: "reopen" });
  indexedDB.failNextOpen("reopen");

  await assert.rejects(store.load("thread"), /injected open failure/);
  const owner = await store.acquire("thread", { ownerId: "owner" });
  assert.deepEqual(
    await store.replace("thread", { ...owner, expectedRevision: "0", payload: "retained" }),
    { status: "replaced", revision: "1" },
  );
  assert.equal(indexedDB.openCount("reopen"), 2, "a rejected open is not cached");

  indexedDB.triggerVersionChange("reopen");
  assert.deepEqual(await store.load("thread"), {
    revision: "1",
    payload: "retained",
  });
  assert.equal(indexedDB.openCount("reopen"), 3, "a closed connection is not cached");

  indexedDB.triggerAbnormalClose("reopen");
  assert.deepEqual(await store.load("thread"), {
    revision: "1",
    payload: "retained",
  });
  assert.equal(indexedDB.openCount("reopen"), 4, "an abnormal close is not cached");
});

function createFakeIndexedDb() {
  const databases = new Map();
  const failedOpens = new Set();
  const openCounts = new Map();
  return {
    open(name, version) {
      const request = fakeRequest();
      openCounts.set(name, (openCounts.get(name) ?? 0) + 1);
      queueMicrotask(() => {
        if (failedOpens.delete(name)) {
          request.error = new Error("injected open failure");
          request.onerror?.();
          return;
        }
        let database = databases.get(name);
        let upgrade = false;
        if (!database) {
          database = new FakeDatabase(version);
          databases.set(name, database);
          upgrade = true;
        } else if (version > database.version) {
          database.version = version;
          upgrade = true;
        }
        database.closed = false;
        request.result = database;
        if (upgrade) {
          request.onupgradeneeded?.();
        }
        queueMicrotask(() => request.onsuccess?.());
      });
      return request;
    },
    failNextOpen(name) {
      failedOpens.add(name);
    },
    openCount(name) {
      return openCounts.get(name) ?? 0;
    },
    triggerVersionChange(name) {
      databases.get(name).onversionchange?.();
    },
    triggerAbnormalClose(name) {
      const database = databases.get(name);
      database.closed = true;
      database.onclose?.();
    },
    storeNames(name) {
      return [...databases.get(name).stores.keys()].sort();
    },
    alterStore(name, storeName, shape) {
      Object.assign(databases.get(name).stores.get(storeName), shape);
    },
    seed(name, stateId, revision, payload) {
      const database = databases.get(name);
      if (!database) throw new Error(`database ${name} has not been opened`);
      database.stores.get("states").records.set(stateId, { stateId, revision, payload });
    },
    failNextStatePut(name) {
      databases.get(name).failNextStatePut = true;
    },
    resetContent(name, stateId) {
      const database = databases.get(name);
      database.stores.get("states").records.delete(stateId);
    },
  };
}

class FakeDatabase {
  constructor(version) {
    this.version = version;
    this.stores = new Map();
    this.closed = false;
    this.failNextStatePut = false;
    this.objectStoreNames = {
      contains: (name) => this.stores.has(name),
      [Symbol.iterator]: () => this.stores.keys(),
    };
    this.writeTail = Promise.resolve();
  }

  createObjectStore(name, { keyPath, autoIncrement = false }) {
    const definition = { keyPath, autoIncrement, indexes: new Map(), records: new Map() };
    this.stores.set(name, definition);
    return {
      createIndex: (indexName, indexKeyPath) => {
        definition.indexes.set(indexName, indexKeyPath);
      },
    };
  }

  transaction(names, mode) {
    if (this.closed) throw new Error("database connection is closed");
    return new FakeTransaction(this, names, mode);
  }

  close() { this.closed = true; }
}

class FakeTransaction {
  constructor(database, names, mode) {
    this.database = database;
    this.mode = mode;
    this.error = null;
    this.pending = 0;
    this.finished = false;
    let release;
    this.released = new Promise((resolve) => { release = resolve; });
    this.release = release;
    const predecessor = database.writeTail;
    if (mode === "readwrite") database.writeTail = predecessor.then(() => this.released);
    this.ready = (mode === "readwrite" ? predecessor : database.writeTail).then(() => {
      this.views = new Map(names.map((name) => [name, cloneStore(database.stores.get(name))]));
    });
  }

  objectStore(name) {
    return new FakeObjectStore(this, name);
  }

  enqueue(operation) {
    const request = fakeRequest();
    this.pending += 1;
    this.ready.then(() => {
      if (this.finished) return;
      try {
        request.result = operation();
        request.onsuccess?.();
      } catch (error) {
        request.error = error;
        this.error = error;
        request.onerror?.();
        this.abort();
        return;
      }
      this.pending -= 1;
      queueMicrotask(() => this.completeIfIdle());
    });
    return request;
  }

  completeIfIdle() {
    if (this.finished || this.pending !== 0) return;
    this.finished = true;
    if (this.mode === "readwrite") {
      for (const [name, view] of this.views) this.database.stores.set(name, view);
    }
    this.release();
    this.oncomplete?.();
  }

  abort() {
    if (this.finished) return;
    this.finished = true;
    this.release();
    this.onerror?.();
    this.onabort?.();
  }
}

class FakeObjectStore {
  constructor(transaction, name) {
    this.transaction = transaction;
    this.name = name;
  }

  get keyPath() {
    return this.transaction.database.stores.get(this.name).keyPath;
  }

  get autoIncrement() {
    return this.transaction.database.stores.get(this.name).autoIncrement;
  }

  get(key) {
    return this.transaction.enqueue(() => clone(this.definition.records.get(serializeKey(key))));
  }

  put(value) {
    return this.transaction.enqueue(() => {
      if (this.name === "states" && this.transaction.database.failNextStatePut) {
        this.transaction.database.failNextStatePut = false;
        throw new Error("injected state failure");
      }
      this.definition.records.set(recordKey(this.definition.keyPath, value), clone(value));
      return clone(value);
    });
  }

  delete(key) {
    return this.transaction.enqueue(() => this.definition.records.delete(serializeKey(key)));
  }

  add(value) {
    return this.transaction.enqueue(() => {
      const key = recordKey(this.definition.keyPath, value);
      if (this.definition.records.has(key)) throw new Error("duplicate key");
      this.definition.records.set(key, clone(value));
      return clone(value);
    });
  }

  index(name) {
    return {
      getAll: (key) => this.transaction.enqueue(() => {
        const keyPath = this.definition.indexes.get(name);
        return [...this.definition.records.values()]
          .filter((value) => value[keyPath] === key)
          .map(clone);
      }),
    };
  }

  get definition() {
    return this.transaction.views.get(this.name);
  }
}

function cloneStore(store) {
  return {
    keyPath: store.keyPath,
    autoIncrement: store.autoIncrement,
    indexes: new Map(store.indexes),
    records: new Map([...store.records].map(([key, value]) => [key, clone(value)])),
  };
}

function fakeRequest() {
  return { error: null, result: undefined };
}

function recordKey(keyPath, value) {
  return serializeKey(Array.isArray(keyPath) ? keyPath.map((key) => value[key]) : value[keyPath]);
}

function serializeKey(key) {
  return Array.isArray(key) ? JSON.stringify(key) : key;
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}
