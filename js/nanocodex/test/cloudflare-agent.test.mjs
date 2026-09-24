import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import {
  bindAgent,
  pruneDurableReceipts,
  create,
  createEphemeral,
  destroy,
  exportDurabilityState,
  importDurabilityState,
} from "../cloudflare/Agent.mjs";
import * as HostAgent from "../host/Agent.mjs";
import { createCloudflareDurabilityStore } from "../runtime/cloudflare-durability-store.mjs";
import * as Subagents from "../runtime/subagents.mjs";

const FIRST_OBJECT_ID = "a".repeat(64);
const SECOND_OBJECT_ID = "b".repeat(64);

class MemoryStorage {
  constructor() {
    this.states = [];
    this.records = new Map();
    this.chunks = [];
    this.chunkHeads = new Map();
    this.events = [];
    this.stateRevisions = new Map();
    this.owners = new Map();
    this.subagents = new Map();
    this.subagentCheckpoints = new Map();
    this.statements = [];
    this.meta = { total_bytes: 0, stream_error: null };
    this.sessionId = undefined;
    this.stateId = undefined;
    this.sql = { exec: (sql, ...args) => this.#exec(sql, args) };
  }

  transactionSync(callback) { return callback(); }

  #exec(sql, args) {
    const statement = sql.replace(/\s+/g, " ").trim();
    this.statements.push(statement);
    if (statement.includes("nanocodex_cloudflare_subagent") && !statement.startsWith("DROP TABLE IF EXISTS ")) {
      throw new Error(`child persistence is forbidden: ${statement}`);
    }
    let rows = [];
    let rowsWritten = 0;
    if (statement.startsWith("CREATE TABLE")) {
      // Schema setup is idempotent.
    } else if (statement === "DROP TABLE IF EXISTS nanocodex_cloudflare_subagents") {
      this.subagents.clear();
    } else if (statement === "DROP TABLE IF EXISTS nanocodex_cloudflare_subagent_checkpoints") {
      this.subagentCheckpoints.clear();
    } else if (statement.startsWith("PRAGMA table_info")) {
      rows = durabilityPragmaRows(statement);
    } else if (statement.startsWith("INSERT OR IGNORE INTO nanocodex_cloudflare_event_meta")) {
      // The in-memory meta row exists from construction.
    } else if (statement.startsWith("SELECT total_bytes, stream_error")) {
      rows = [{ ...this.meta }];
    } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_events")) {
      const [event_json, created_at] = args;
      const cursor = String(this.events.length + 1);
      this.events.push({ cursor, event_json, created_at });
      rows = [{ cursor }];
    } else if (statement.startsWith(
      "UPDATE nanocodex_cloudflare_event_meta SET total_bytes = total_bytes",
    )) {
      this.meta.total_bytes += args[0];
    } else if (statement.startsWith("UPDATE nanocodex_cloudflare_event_meta SET stream_error")) {
      this.meta.stream_error = args[0];
    } else if (statement.startsWith("SELECT CAST(COALESCE(MAX(cursor)")) {
      rows = [{ cursor: this.events.at(-1)?.cursor ?? "0" }];
    } else if (statement.startsWith("SELECT CAST(cursor AS TEXT)")) {
      const after = BigInt(args[0]);
      rows = this.events.filter((event) => BigInt(event.cursor) > after).slice(0, 1);
    } else if (statement.startsWith("SELECT session_id FROM nanocodex_cloudflare_agent")) {
      rows = this.sessionId === undefined ? [] : [{ session_id: this.sessionId }];
    } else if (statement.startsWith("INSERT OR IGNORE INTO nanocodex_cloudflare_agent")) {
      this.sessionId ??= args[0];
    } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_agent")) {
      if (this.sessionId !== undefined) throw new Error("duplicate Cloudflare Agent identity");
      this.sessionId = args[0];
    } else if (statement.startsWith("SELECT state_id FROM nanocodex_cloudflare_durability")) {
      rows = this.stateId === undefined ? [] : [{ state_id: this.stateId }];
    } else if (statement.startsWith("INSERT OR IGNORE INTO nanocodex_cloudflare_durability")) {
      this.stateId ??= args[0];
    } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_durability")) {
      if (this.stateId !== undefined) throw new Error("duplicate Cloudflare durability identity");
      this.stateId = args[0];
    } else if (statement.startsWith("SELECT owner_id, fence FROM nanocodex_durable_owners")) {
      const owner = this.owners.get(args[0]);
      rows = owner === undefined ? [] : [{ owner_id: owner.ownerId, fence: owner.fence }];
    } else if (statement.startsWith("SELECT fence FROM nanocodex_durable_owners")) {
      const owner = this.owners.get(args[0]);
      rows = owner === undefined ? [] : [{ fence: owner.fence }];
    } else if (statement.startsWith("INSERT INTO nanocodex_durable_owners")) {
      this.owners.set(args[0], { ownerId: args[1], fence: args[2] });
    } else if (statement.startsWith("SELECT revision FROM nanocodex_durable_states")) {
      rows = this.states.filter((batch) => batch.stateId === args[0])
        .map(({ revision }) => ({ revision }));
    } else if (statement.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) {
      rows = this.states
        .filter((batch) => batch.stateId === args[0])
        .map(({ revision, payload }) => ({ revision, payload }));
    } else if (statement.startsWith("SELECT key, value FROM nanocodex_durable_records")) {
      rows = [...this.records].map(([address, value]) => ({ address: JSON.parse(address), value }))
        .filter(({ address: [stateId, key] }) => stateId === args[0] && (statement.includes("key IN") ? args.slice(1).includes(key) : key > args[1]))
        .map(({ address: [, key], value }) => ({ key, value })).sort((a, b) => a.key < b.key ? -1 : 1);
      if (statement.includes("LIMIT")) rows = rows.slice(0, args[2]);
    } else if (statement.startsWith("SELECT value FROM nanocodex_durable_records")) {
      const value = this.records.get(JSON.stringify(args));
      rows = value === undefined ? [] : [{ value }];
    } else if (statement.startsWith("INSERT INTO nanocodex_durable_records")) {
      this.records.set(JSON.stringify(args.slice(0, 2)), args[2]);
    } else if (statement.startsWith("DELETE FROM nanocodex_durable_records")) {
      for (const key of this.records.keys()) if (!args.length || JSON.parse(key)[0] === args[0]) this.records.delete(key);
    } else if (statement.startsWith("INSERT INTO nanocodex_durable_states")) {
      this.stateRevisions.set(args[0], args[1]);
      this.states = this.states.filter((batch) => batch.stateId !== args[0]);
      this.states.push({ stateId: args[0], revision: args[1], payload: args[2] });
    } else if (statement.startsWith("INSERT INTO nanocodex_durable_chunk_heads")) {
      this.chunkHeads.set(args[0], { revision: args[1], chunk_count: args[2] });
    } else if (statement.startsWith("INSERT INTO nanocodex_durable_state_chunks")) {
      this.chunks.push({
        stateId: args[0],
        revision: args[1],
        chunkIndex: args[2],
        payload: args[3],
      });
    } else if (statement.startsWith("DELETE FROM nanocodex_durable_chunk_heads")) {
      this.chunkHeads.delete(args[0]);
    } else if (statement.startsWith("DELETE FROM nanocodex_durable_state_chunks")) {
      this.chunks = this.chunks.filter((chunk) => chunk.stateId !== args[0]);
    } else if (statement.startsWith("DELETE FROM nanocodex_durable_states")) {
      this.states = this.states.filter((batch) => batch.stateId !== args[0]);
      this.stateRevisions.delete(args[0]);
    } else if (statement.startsWith("DELETE FROM nanocodex_durable_owners")) {
      this.owners.delete(args[0]);
    } else if (statement === "DELETE FROM nanocodex_cloudflare_events") {
      this.events = [];
    } else if (statement.startsWith("UPDATE nanocodex_cloudflare_event_meta SET total_bytes = 0")) {
      this.meta = { total_bytes: 0, stream_error: null };
    } else {
      throw new Error(`unexpected SQL: ${statement}`);
    }
    return { rowsWritten, toArray: () => rows, [Symbol.iterator]: () => rows[Symbol.iterator]() };
  }
}

function durabilityPragmaRows(sql) {
  let shapes;
  if (sql.includes("nanocodex_durable_owners")) {
    shapes = [["state_id", "TEXT", 0, 1], ["owner_id", "TEXT", 1, 0], ["fence", "TEXT", 1, 0]];
  } else if (sql.includes("nanocodex_durable_states")) {
    shapes = [["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["payload", "TEXT", 1, 0]];
  } else if (sql.includes("nanocodex_durable_chunk_heads")) {
    shapes = [["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["chunk_count", "INTEGER", 1, 0]];
  } else {
    shapes = [
      ["state_id", "TEXT", 1, 1], ["revision", "TEXT", 1, 2],
      ["chunk_index", "INTEGER", 1, 3], ["payload", "TEXT", 1, 0],
    ];
  }
  return shapes.map(([name, type, notnull, pk], cid) => ({ cid, name, type, notnull, pk }));
}

class UpstreamSocket {
  addEventListener() {}
  accept() {}
  close() { this.closed = true; }
}

function durableContext(storage, id = FIRST_OBJECT_ID) {
  return {
    id: { toString: () => id },
    storage,
    acceptWebSocket() {},
    getWebSockets() { return []; },
  };
}

function egressBinding(subjects) {
  return {
    async fetch(_input, init) {
      subjects?.push(init.headers.get("x-nanocodex-subject"));
      return {
        status: 101,
        headers: new Headers(),
        webSocket: new UpstreamSocket(),
      };
    },
  };
}

function durableOwner(storage, binding = egressBinding(), id = FIRST_OBJECT_ID) {
  return {
    ctx: durableContext(storage, id),
    env: { NANOCODEX: binding },
  };
}

// Keep this before successful creation: the engine is shared for the whole realm.
test("prepared construction shares cold engine initialization without retaining failed metadata", { timeout: 10_000 }, async t => {
  const module = await WebAssembly.compile(await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)));
  const instantiate = WebAssembly.instantiate;
  let engine = { started: deferred(), release: deferred(), finished: deferred() };
  const instantiations = t.mock.method(WebAssembly, "instantiate", async (...args) => {
    const current = engine;
    current.started.resolve();
    await current.release.promise;
    const result = await instantiate(...args);
    current.finished.resolve();
    return result;
  });
  const sockets = [];
  const storage = new MemoryStorage();
  const owner = durableOwner(storage, { async fetch() {
    const socket = new UpstreamSocket();
    sockets.push(socket);
    return { status: 101, headers: new Headers(), webSocket: socket };
  } });
  const invalid = nativePreparationOptions(() => { throw new Error("must not prepare"); });
  invalid.apiKey = "forbidden";
  await assert.rejects(create(module, owner, invalid), /does not accept apiKey/);
  assert.equal(instantiations.mock.callCount(), 0);
  assert.equal(sockets.length, 0);

  const discoveryFailure = new Error("discovery failed");
  const discovery = deferred();
  let staleFinish;
  const failed = create(module, owner, nativePreparationOptions(async finish => {
    staleFinish = finish;
    await discovery.promise;
    throw discoveryFailure;
  }));
  const rejected = assert.rejects(failed, error => error === discoveryFailure);
  await engine.started.promise;
  assert.equal(sockets.length, 1, "socket and engine start while discovery is blocked");
  assert.equal(storage.owners.size, 0, "warming the engine does not acquire a durable runtime owner");
  assert.equal(storage.states.length, 0);
  discovery.resolve();
  await rejected;
  assert.equal(sockets[0].closed, true);
  assert.throws(() => staleFinish(nativePreparationOptions()), /already completed/);

  const initializationFailure = new Error("engine initialization failed");
  let retryPrepared = false;
  const failedEngine = create(module, owner, nativePreparationOptions(finish => {
    retryPrepared = true;
    return finish(nativePreparationOptions());
  }));
  assert.equal(retryPrepared, true, "metadata failure releases the lifecycle before the engine settles");
  assert.equal(instantiations.mock.callCount(), 1, "retry shares the still-pending initialization");
  const rejectedEngine = assert.rejects(failedEngine, error => error === initializationFailure);
  engine.release.reject(initializationFailure);
  await rejectedEngine;
  assert.equal(storage.owners.size, 0);
  assert.equal(sockets[1].closed, true);

  engine = { started: deferred(), release: deferred(), finished: deferred() };
  const metadata = deferred();
  const prepare = async finish => { await metadata.promise; return finish(nativePreparationOptions()); };
  const first = create(module, owner, nativePreparationOptions(prepare));
  await engine.started.promise;
  const otherStorage = new MemoryStorage();
  const second = create(module, durableOwner(otherStorage, egressBinding(), SECOND_OBJECT_ID), nativePreparationOptions(prepare));
  const cancelledStorage = new MemoryStorage();
  const cancellation = new AbortController();
  const cancelled = create(module, durableOwner(cancelledStorage, egressBinding(), "c".repeat(64)),
    nativePreparationOptions(finish => finish(nativePreparationOptions()), cancellation.signal));
  cancellation.abort();
  const rejectedCancellation = assert.rejects(cancelled, /abort/i);
  assert.equal(instantiations.mock.callCount(), 2, "concurrent owners share the single retrying initialization");
  engine.release.resolve();
  await engine.finished.promise;
  await rejectedCancellation;
  assert.equal(storage.owners.size, 0, "engine readiness alone does not create a session");
  assert.equal(otherStorage.owners.size, 0);
  assert.equal(cancelledStorage.owners.size, 0, "cancellation while the engine loads prevents runtime ownership");
  metadata.resolve();
  const agents = await Promise.all([first, second]);
  try {
    assert.equal(instantiations.mock.callCount(), 2, "normal Agent construction reuses the initialized engine");
    assert.equal(storage.owners.size, 1);
    assert.equal(otherStorage.owners.size, 1);
  } finally {
    await Promise.all(agents.map(agent => agent.session.shutdown()));
  }
});

test("Cloudflare Agent rejects caller credentials and transport authority", async () => {
  const module = new Uint8Array();
  for (const name of ["apiKey", "CODEX_OAUTH_BOOTSTRAP", "transport", "subject"]) {
    await assert.rejects(
      create(module, durableOwner(new MemoryStorage()), { [name]: "caller-selected" }),
      new RegExp(`does not accept ${name}`),
    );
  }
});

test("host delegation prohibition reaches Rust and overrides caller subagent extensions", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  for (const tools of [[], [...Subagents.create({ maxConcurrency: 2 })]]) {
    const storage = new MemoryStorage();
    const agent = await create(module, durableOwner(storage), {
      tools,
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { subagentsEnabled: false },
    });
    try {
      await assert.rejects(Subagents.spawn(agent, {
        role: "disabled-check", task: "This task must never start.", outputSchema: { type: "object" },
      }), /not created with the subagent extension/);
      assert.equal(storage.subagents.size, 0);
    } finally { await agent.session.shutdown(); }
  }
});

test("Cloudflare ephemeral Agent owns transport without durable state", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const subjects = [];
  const owner = durableOwner(storage, egressBinding(subjects));
  const agent = await createEphemeral(module, owner, {
    instructions: "Use the caller's search tool.",
    model: "gpt-6-sol",
    tools: [{
      name: "search",
      description: "Search account history",
      handler: () => [],
    }],
  });

  assert.deepEqual(subjects, [FIRST_OBJECT_ID]);
  assert.equal(storage.sessionId, undefined);
  assert.equal(storage.stateRevisions.size, 0);
  assert.equal(storage.events.length, 0);
  await agent.session.shutdown();
});

test("Cloudflare ephemeral Agent validates adapter-owned startup", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const owner = durableOwner(new MemoryStorage(), {
    async fetch() {
      return { status: 403, headers: new Headers() };
    },
  });

  await assert.rejects(
    createEphemeral(module, owner),
    /EGRESS broker rejected.*HTTP 403/,
  );
  await assert.rejects(
    createEphemeral(module, owner, { transport: {} }),
    /createEphemeral does not accept transport/,
  );
});

test("managed voice admission does not wait for a cold Responses preconnection", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const release = deferred();
  let requests = 0;
  const socket = new UpstreamSocket();
  const owner = durableOwner(new MemoryStorage(), {
    async fetch(_input, init) {
      requests += 1;
      assert.equal(init.headers.get("x-nanocodex-subject"), FIRST_OBJECT_ID);
      assert.equal(init.headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      await release.promise;
      return { status: 101, headers: new Headers(), webSocket: socket };
    },
  });
  const options = { eventPersistence: "caller" };
  Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), {
    value: { waitForPreconnect: false },
  });
  let agent;
  try {
    // Keep the text relay unavailable through both voice lifecycle operations.
    // Its container allows 20 seconds to become ready; the old creation gate
    // rejected this otherwise healthy voice session after just 10 seconds.
    agent = await create(module, owner, options);
    const context = await agent.session.realtime.start();
    assert.ok(Array.isArray(context.history));
    await agent.session.realtime.end();
    assert.equal(requests, 1, "Warm the owned Responses transport speculatively");
    await agent.session.shutdown();
    agent = undefined;
  } finally {
    release.resolve();
    await agent?.session.shutdown();
  }
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(socket.closed, true, "Shutdown closes a preconnection that finishes late");
});

test("public durable creation still validates credentials before returning", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const owner = durableOwner(new MemoryStorage(), {
    async fetch() { return { status: 403, headers: new Headers() }; },
  });
  await assert.rejects(create(module, owner), /EGRESS broker rejected.*HTTP 403/);
});

test("a failed speculative connection does not authorize a later managed text turn", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  let requests = 0;
  const owner = durableOwner(new MemoryStorage(), {
    async fetch(_input, init) {
      requests += 1;
      assert.equal(init.headers.get("x-nanocodex-subject"), FIRST_OBJECT_ID);
      assert.equal(init.headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      return new Response("credential_broker_rejected", { status: 403 });
    },
  });
  const agent = await create(module, owner, {
    eventPersistence: "caller",
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { waitForPreconnect: false },
  });
  try {
    // Let the speculative denial settle; the actual turn must request its own
    // brokered transport and still surface the credential rejection.
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(requests, 1);
    await assert.rejects(
      agent.turn.prompt({ input: "Check transport authorization" }).result(),
      /HTTP 403: credential_broker_rejected/,
    );
    assert.ok(requests > 1, "A model turn must still cross the credential broker");
  } finally {
    await agent.session.shutdown();
  }
});

test("Cloudflare Agent isolates states per Durable Object and can recreate after shutdown", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const firstStorage = new MemoryStorage();
  const secondStorage = new MemoryStorage();
  const subjects = [];
  const binding = egressBinding(subjects);
  const owner = (storage, id) => durableOwner(storage, binding, id);

  const [first, second] = await Promise.all([
    create(module, owner(firstStorage, FIRST_OBJECT_ID), {
      terminalReceiptRetention: 512,
      tools: [...Subagents.create({ maxConcurrency: 2 })],
    }),
    create(module, owner(secondStorage, SECOND_OBJECT_ID)),
  ]);
  assert.notEqual(first.sessionId, second.sessionId);
  assert.equal(firstStorage.stateId, first.sessionId);
  assert.equal(secondStorage.stateId, second.sessionId);
  assert.deepEqual(new Set(subjects), new Set([FIRST_OBJECT_ID, SECOND_OBJECT_ID]));
  await Promise.all([first.session.shutdown(), second.session.shutdown()]);

  const recreated = await create(module, owner(firstStorage, FIRST_OBJECT_ID));
  assert.equal(recreated.sessionId, first.sessionId);
  await recreated.session.shutdown();

  const explicitStorage = new MemoryStorage();
  const explicitOwner = owner(explicitStorage, "c".repeat(64));
  const explicit = await create(module, explicitOwner, { durabilityId: "managed-agent-id" });
  assert.equal(explicitStorage.stateId, "managed-agent-id");
  await explicit.session.shutdown();
  await assert.rejects(
    create(module, explicitOwner, { durabilityId: "rewritten-agent-id" }),
    /does not match the retained state identity/,
  );
});

test("Cloudflare Agent reconstruction takes over the same durable owner after fencing", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const first = await create(module, durableOwner(storage, binding, FIRST_OBJECT_ID));
  const firstDurableOwner = { ...storage.owners.get(storage.stateId) };

  const reconstructed = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
  );
  assert.equal(reconstructed.sessionId, first.sessionId);
  assert.notEqual(storage.owners.get(storage.stateId).ownerId, firstDurableOwner.ownerId);
  assert.ok(
    BigInt(storage.owners.get(storage.stateId).fence) > BigInt(firstDurableOwner.fence),
  );

  first.dispose();
  await reconstructed.session.shutdown();
  const reopened = await create(module, durableOwner(storage, binding, FIRST_OBJECT_ID));
  await reopened.session.shutdown();
});

test("Cloudflare root takeover starts without children and stale cleanup preserves new children", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const lifecycles = [];
  const options = {
    tools: { identity: { parameters: { type: "object" }, handler: (_input, context) => context.subagent } },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { subagentLifecycle: event => lifecycles.push(event) },
  };
  const first = await create(module, durableOwner(storage), options);
  let replacement;
  try {
    await Subagents.spawn(first, { role: "old-child", task: "Wait until restart.", outputSchema: { type: "object" } });
    const oldBind = lifecycles.find(({ type }) => type === "bind");
    assert.ok(oldBind);
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);
    replacement = await create(module, durableOwner(storage), options);
    assert.equal(replacement.sessionId, first.sessionId, "root identity remains durable");
    assert.deepEqual((await Subagents.list(replacement, { includeCompleted: true })).agents, []);
    const child = await Subagents.spawn(replacement, { role: "new-child", task: "Use only live authority.", outputSchema: { type: "object" } });
    const newBind = lifecycles.find(({ type, descriptor }) => type === "bind" && descriptor.role === "new-child");
    assert.ok(newBind);
    assert.notEqual(newBind.sessionId, oldBind.sessionId);
    await first.session.shutdown();
    const routed = JSON.parse(await globalThis.nanocodexHost.executeTool("identity", "{}", newBind.sessionId, "after-stale-cleanup"));
    assert.equal(routed.structured_result.role, "new-child");
    assert.throws(() => globalThis.nanocodexHost.executeTool("identity", "{}", oldBind.sessionId, "old-child"), /no Nanocodex host is active/);
    assert.equal(lifecycles.some(({ type }) => type === "reconstruct"), false);
    await Subagents.close(replacement, child.agent_id);
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);
  } finally {
    await first.session.shutdown();
    await replacement?.session.shutdown();
  }
});

test("Cloudflare startup drops legacy child descriptors and malformed checkpoints without reading them", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  storage.subagents.set("legacy-child", { descriptorJson: "not-json", hostContextRef: "obsolete-context" });
  storage.subagentCheckpoints.set(9, "malformed checkpoint");
  const agent = await create(module, durableOwner(storage));
  try {
    assert.deepEqual((await Subagents.list(agent, { includeCompleted: true })).agents, []);
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);
    assert.deepEqual(storage.statements.filter(sql => sql.includes("nanocodex_cloudflare_subagent")).sort(), [
      "DROP TABLE IF EXISTS nanocodex_cloudflare_subagent_checkpoints",
      "DROP TABLE IF EXISTS nanocodex_cloudflare_subagents",
    ]);
  } finally { await agent.session.shutdown(); }
});

test("Cloudflare child bindings expose only live operations and release a subtree without SQL", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const lifecycle = [];
  let sessions;
  const bound = bindAgent(module, {
    create(options) {
      sessions = options[Symbol.for("nanocodex.browser.internalRuntime")].subagentSessions;
      return HostAgent.create(options);
    },
  });
  const agent = await bound.create(durableOwner(storage), {
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { subagentLifecycle: event => lifecycle.push(event) },
  });
  try {
    for (const method of ["restore", "checkpoint", "restoreCheckpoint"]) assert.equal(sessions[method], undefined);
    const descriptors = [
      { agentId: "10", parentAgentId: null, sessionId: "live-parent", role: "parent", task: "Original task" },
      { agentId: "11", parentAgentId: "10", sessionId: "live-descendant", role: "descendant", task: "Child task" },
      { agentId: "12", parentAgentId: null, sessionId: "live-sibling", role: "sibling", task: "Sibling task" },
    ];
    const sqlCount = storage.statements.length;
    for (const descriptor of descriptors) sessions.bind(descriptor.sessionId, descriptor, "private-context");
    const [parent, descendant, sibling] = descriptors;
    assert.deepEqual(sessions.bindingDescriptor(parent.sessionId, { ...parent, task: "New task" }, "private-context"), parent);
    assert.throws(() => sessions.bindingDescriptor(parent.sessionId, { ...parent, agentId: "99" }, "private-context"), /identity or host context changed/);
    sessions.release(parent.sessionId, "wrong-context");
    assert.equal(lifecycle.filter(({ type }) => type === "release").length, 0);
    sessions.release(parent.sessionId, "private-context");
    assert.deepEqual(lifecycle.filter(({ type }) => type === "release").map(({ sessionId }) => sessionId), [parent.sessionId, descendant.sessionId]);
    sessions.release(descendant.sessionId, "private-context");
    assert.equal(lifecycle.filter(({ type }) => type === "release").length, 2, "released descendants are idempotent");
    assert.deepEqual(sessions.bindingDescriptor(sibling.sessionId, { ...sibling, task: "Continued task" }, "private-context"), sibling);
    sessions.release(sibling.sessionId, "private-context");
    assert.equal(storage.statements.length, sqlCount, "live child bindings never access SQL");
  } finally { await agent.session.shutdown(); }
});

test("Cloudflare Agent keeps failed private releases exactly retryable", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  let releaseAttempts = 0;
  const lifecycle = (event) => {
    if (event.type !== "release") return;
    releaseAttempts += 1;
    if (releaseAttempts === 1) throw new Error("private release failed");
  };
  const options = {};
  Object.defineProperty(
    options,
    Symbol.for("nanocodex.cloudflare.internalRuntime"),
    { value: { subagentLifecycle: lifecycle } },
  );
  const agent = await create(
    module,
    durableOwner(storage, egressBinding(), FIRST_OBJECT_ID),
    options,
  );
  const bridge = globalThis.nanocodexHost;
  const binds = [];
  globalThis.nanocodexHost = Object.freeze({
    ...bridge,
    bindSubagentSession(...args) {
      binds.push(args);
      return bridge.bindSubagentSession(...args);
    },
  });
  let started;
  try {
    started = await Subagents.spawn(agent, {
      role: "release-retry",
      task: "Keep the private release retryable.",
      outputSchema: { type: "object" },
    });
  } finally {
    globalThis.nanocodexHost = bridge;
  }
  assert.equal(binds.length, 1);
  await Subagents.interrupt(agent, started.agent_id);
  const bind = binds[0];
  assert.ok(bind);

  assert.throws(
    () => bridge.releaseSubagentSession(bind[0], bind[1], bind[2]),
    /private release failed/,
  );
  assert.equal(releaseAttempts, 1);
  bridge.releaseSubagentSession(bind[0], bind[1], bind[2]);
  assert.equal(storage.subagents.size, 0);
  assert.equal(releaseAttempts, 2);

  await agent.session.shutdown();
  assert.equal(releaseAttempts, 2);
});

test("Cloudflare Agent reconstruction rejects a different durable owner before fencing", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const first = await create(module, durableOwner(storage, binding, FIRST_OBJECT_ID));
  const retainedOwner = { ...storage.owners.get(storage.stateId) };

  await assert.rejects(
    create(module, durableOwner(storage, binding, SECOND_OBJECT_ID)),
    /session ID is already active/,
  );
  assert.deepEqual(storage.owners.get(storage.stateId), retainedOwner);

  await first.session.shutdown();
});

test("failed reconstruction keeps the prior same-owner reservation fail closed", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const first = await create(module, durableOwner(storage, binding, FIRST_OBJECT_ID));
  await Subagents.spawn(first, {
    role: "retry-proof",
    task: "Remain live until the root is replaced.",
    outputSchema: { type: "object" },
  });
  assert.equal((await Subagents.list(first)).agents.length, 1);
  const failing = bindAgent(module, {
    async create(options) {
      const agent = await HostAgent.create(options);
      return new Proxy(agent, {
        get(target, property, receiver) {
          if (property === "events") {
            return { watch: () => { throw new Error("reconstruction setup failed"); } };
          }
          return Reflect.get(target, property, receiver);
        },
      });
    },
  });

  await assert.rejects(
    failing.create(durableOwner(storage, binding, FIRST_OBJECT_ID)),
    /reconstruction setup failed/,
  );
  assert.equal(storage.subagents.size, 0);
  await assert.rejects(
    create(module, durableOwner(storage, binding, SECOND_OBJECT_ID)),
    /session ID is already active/,
  );

  const reconstructed = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
  );
  assert.deepEqual((await Subagents.list(reconstructed, { includeCompleted: true })).agents, []);
  first.dispose();
  await reconstructed.session.shutdown();
  assert.equal(storage.subagents.size, 0);
});

test("Cloudflare Agent rejects a takeover while its predecessor is not committed", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const first = await create(module, durableOwner(storage, binding, FIRST_OBJECT_ID));
  const entered = deferred();
  const release = deferred();
  const held = bindAgent(module, {
    async create(options) {
      const agent = await HostAgent.create(options);
      entered.resolve();
      await release.promise;
      return agent;
    },
  });
  const pending = held.create(durableOwner(storage, binding, FIRST_OBJECT_ID));
  await entered.promise;

  await assert.rejects(
    create(module, durableOwner(storage, binding, FIRST_OBJECT_ID)),
    /session ID is already active/,
  );

  release.resolve();
  const reconstructed = await pending;
  first.dispose();
  await reconstructed.session.shutdown();
});

test("Cloudflare Agent exports and imports one stable state across a fresh runtime identity", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const sourceStorage = new MemoryStorage();
  const sourceOwner = durableOwner(sourceStorage);
  const source = await create(module, sourceOwner);
  await source.session.shutdown();
  const sourceSessionId = source.sessionId;
  const stateId = sourceStorage.stateId;
  const store = createCloudflareDurabilityStore(sourceStorage);
  const ownership = store.acquire(stateId, { ownerId: "seed" });
  const payload = JSON.stringify({
    nanocodex_durable_state: {
      format: 4,
      operations: {},
      latest_checkpoint: null,
    },
  });
  assert.deepEqual(store.replace(stateId, { records: [],
    ownerId: ownership.ownerId,
    fence: ownership.fence,
    expectedRevision: ownership.revision,
    payload,
  }), { status: "replaced", revision: "1" });

  const archive = await exportDurabilityState(sourceOwner);
  assert.deepEqual(await bindAgent(module).exportDurabilityHead(sourceOwner), { ...archive, records: [] });
  assert.deepEqual(archive, {
    format: "nanocodex-durability-state-v2", records: [],
    stateId,
    revision: "1",
    payload,
  });
  const pages = [];
  let cursor;
  do {
    const page = await exportDurabilityState(sourceOwner, {
      from: "0",
      to: "1",
      cursor,
      limit: 19,
    });
    pages.push(page);
    cursor = page.nextCursor ?? undefined;
  } while (cursor !== undefined);
  assert.equal(pages.map((page) => page.payload).join(""), payload);
  assert(pages.length > 1, "the Cloudflare lifecycle API must expose resumable pages");

  const destinationStorage = new MemoryStorage();
  const destinationOwner = durableOwner(destinationStorage, egressBinding(), SECOND_OBJECT_ID);
  const bound = bindAgent(module);
  await bound.importDurabilityState(destinationOwner, JSON.parse(JSON.stringify(archive)));
  await assert.doesNotReject(
    bound.importDurabilityState(destinationOwner, JSON.parse(JSON.stringify(archive))),
  );
  await assert.rejects(
    importDurabilityState(destinationOwner, { ...archive, revision: 1.5 }),
    /revision numbers must be nonnegative safe integers/,
  );
  await assert.rejects(
    importDurabilityState(destinationOwner, { ...archive, unexpected: true }),
    /invalid shape/,
  );
  const destination = await create(module, destinationOwner);
  assert.notEqual(destination.sessionId, sourceSessionId);
  assert.equal(destinationStorage.stateId, stateId);
  assert.deepEqual(createCloudflareDurabilityStore(destinationStorage).load(stateId), {
    revision: "1",
    payload,
  });
  await destination.session.shutdown();
});

test("Cloudflare Agent rejects corrupt canonical state before importing it", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  await assert.rejects(
    bindAgent(module).importDurabilityState(owner, {
      format: "nanocodex-durability-state-v2", records: [],
      stateId: "corrupt-canonical-state",
      revision: "1",
      payload: "{}",
    }),
    /durability state at revision 1 is invalid/,
  );
  assert.equal(storage.sessionId, undefined);
  assert.equal(storage.stateId, undefined);
  assert.deepEqual(storage.states, []);
  assert.equal(storage.owners.size, 0);
});

test("Cloudflare Agent portability refuses active and non-pristine owners", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  const agent = await create(module, owner);
  await assert.rejects(exportDurabilityState(owner), /shutdown must complete/);
  await assert.rejects(importDurabilityState(owner, {}), /shutdown must complete/);
  await agent.session.shutdown();
  await assert.rejects(importDurabilityState(owner, {
    format: "nanocodex-durability-state-v2", records: [],
    stateId: "another-state",
    revision: "0",
    payload: null,
  }), /pristine Durable Object/);
});

test("Cloudflare Agent disposal releases lifecycle authority without bypassing joined shutdown", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  const disposed = await create(module, owner);

  disposed.dispose();
  assert.doesNotThrow(() => destroy(owner));

  const replacement = await create(module, owner);
  const shutdown = replacement.session.shutdown();
  await assert.rejects(create(module, owner), /shutdown must complete before create/);
  await shutdown;

  const reopened = await create(module, owner);
  await reopened.session.shutdown();
});

test("Cloudflare Agent prunes retained receipts before runtime construction", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  storage.sessionId = "018f1f9a-7b3c-7a17-8000-000000000097";
  const stateId = `cloudflare:${storage.sessionId}`;
  const operations = Object.fromEntries(Array.from({ length: 10 }, (_, index) => [
    `turn-compacted-${index}`,
    {
      input: JSON.stringify("prompt"),
      status: { cancelled: { checkpoint: null } },
      steps: {},
      retired_steers: 0,
      accepted_order: index * 2 + 1,
    },
  ]));
  storage.states.push({
    stateId,
    revision: "20",
    payload: JSON.stringify({
      nanocodex_durable_state: {
        format: 4,
        operations,
        latest_checkpoint: null,
      },
    }),
  });
  storage.stateRevisions.set(stateId, "20");

  const owner = durableOwner(storage);
  await pruneDurableReceipts(module, owner, {
    terminalReceiptRetention: 512,
  });

  assert.equal(storage.stateRevisions.get(stateId), "20");
  assert.equal(storage.states.length, 1);
  assert.equal(storage.states[0].revision, "20");
  let checkpoint = JSON.parse(storage.states[0].payload).nanocodex_durable_state;
  assert.equal(checkpoint.format, 4);
  assert.equal(Object.keys(checkpoint.operations).length, 10);

  await pruneDurableReceipts(module, owner, {
    terminalReceiptRetention: 0,
  });

  assert.equal(storage.states.length, 1);
  assert.equal(storage.stateRevisions.get(stateId), "21");
  assert.equal(storage.states[0].revision, "21");
  checkpoint = JSON.parse(storage.states[0].payload).nanocodex_durable_state;
  assert.deepEqual(checkpoint.operations, {});
});

test("Cloudflare receipt pruning reserves lifecycle authority against create", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  storage.sessionId = "018f1f9a-7b3c-7a17-8000-000000000098";
  const stateId = `cloudflare:${storage.sessionId}`;
  storage.states.push({
    stateId,
    revision: "1",
    payload: JSON.stringify({
      nanocodex_durable_state: {
        format: 4,
        operations: {
          "turn-compaction-race": {
            input: JSON.stringify("prompt"),
            status: "pending",
            steps: {},
            retired_steers: 0,
      accepted_order: 1,
          },
        },
        latest_checkpoint: null,
      },
    }),
  });
  storage.stateRevisions.set(stateId, "1");
  const owner = durableOwner(storage);

  const compaction = pruneDurableReceipts(module, owner);
  await assert.rejects(
    create(module, owner),
    /creation is already in progress/,
  );
  await compaction;
});

test("Cloudflare Agent releases its state when event projection setup fails", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  const failing = bindAgent(module, {
    async create(options) {
      const agent = await HostAgent.create(options);
      return new Proxy(agent, {
        get(target, property, receiver) {
          if (property === "events") {
            return { watch: () => { throw new Error("event projection setup failed"); } };
          }
          return Reflect.get(target, property, receiver);
        },
      });
    },
  });

  await assert.rejects(failing.create(owner), /event projection setup failed/);

  const recreated = await create(module, owner);
  await recreated.session.shutdown();
});

test("Cloudflare Agent lets an embedding Durable Object own the only retained event log", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  storage.events.push({ cursor: "1", event_json: "{}", created_at: Date.now() });
  storage.meta.total_bytes = 2;
  const owner = durableOwner(storage);
  const agent = await create(module, owner, { eventPersistence: "caller" });
  assert.equal(storage.events.length, 0);
  assert.equal(storage.meta.total_bytes, 0);
  assert.equal(typeof agent.events.connect, "function");
  const unavailable = agent.events.connect(new Request("https://agent.invalid/events"));
  assert.equal(unavailable.status, 409);
  assert.deepEqual(await unavailable.json(), { error: "event_persistence_caller_owned" });
  await agent.session.shutdown();
});

test("Cloudflare Agent destroy and duplicate create refuse an in-flight creation", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  const entered = deferred();
  const release = deferred();
  const held = bindAgent(module, {
    async create(options) {
      entered.resolve();
      await release.promise;
      return HostAgent.create(options);
    },
  });

  const pending = held.create(owner);
  await entered.promise;
  assert.throws(() => destroy(owner), /creation must settle before destroy/);
  await assert.rejects(held.create(owner), /creation is already in progress/);

  release.resolve();
  const agent = await pending;
  assert.throws(() => destroy(owner), /shutdown must complete before destroy/);
  await agent.session.shutdown();
  destroy(owner);
});

test("Cloudflare Agent classifies failed creation rollback as reopen required", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);
  const failing = bindAgent(module, {
    async create(options) {
      const agent = await HostAgent.create(options);
      return new Proxy(agent, {
        get(target, property, receiver) {
          if (property === "events") {
            return { watch: () => { throw new Error("event projection setup failed"); } };
          }
          if (property === "session") {
            return new Proxy(target.session, {
              get(session, sessionProperty, sessionReceiver) {
                if (sessionProperty === "shutdown") {
                  return async () => {
                    await session.shutdown();
                    throw new Error("injected rollback acknowledgement failure");
                  };
                }
                return Reflect.get(session, sessionProperty, sessionReceiver);
              },
            });
          }
          return Reflect.get(target, property, receiver);
        },
      });
    },
  });

  await assert.rejects(failing.create(owner), (error) => {
    assert.equal(error.code, "reopen_required");
    assert.match(error.message, /rollback requires reopen/);
    assert.ok(error.cause instanceof AggregateError);
    assert.equal(error.cause.errors.length, 2);
    return true;
  });

  const recreated = await create(module, owner);
  await recreated.session.shutdown();
});

test("Cloudflare Agent destroy owns idempotent adapter cleanup", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const owner = durableOwner(storage);

  destroy(owner);
  const agent = await create(module, owner);
  await agent.session.shutdown();
  const stateId = storage.stateId;
  const staleOwner = { ...storage.owners.get(stateId) };
  assert.equal(staleOwner.fence, "2");
  storage.records.set(JSON.stringify([stateId, "fixture"]), "retained");
  storage.subagents.set("child-session", {
    agentId: "1",
    descriptorJson: JSON.stringify({
      agentId: "1",
      parentAgentId: null,
      sessionId: "child-session",
      role: "stale",
      task: "Do not survive destroy.",
    }),
  });
  destroy(owner);
  const destroyedOwner = storage.owners.get(stateId);
  assert.equal(destroyedOwner.fence, "3");
  assert.match(destroyedOwner.ownerId, /^destroy:/);
  assert.deepEqual(
    createCloudflareDurabilityStore(storage).replace(stateId, {
      ...staleOwner,
      expectedRevision: "0",
      payload: "stale resurrection",
    }),
    { status: "fenced" },
  );
  destroy(owner);

  assert.equal(storage.states.length, 0);
  assert.equal(storage.records.size, 0);
  assert.equal(storage.stateRevisions.size, 0);
  assert.equal(storage.events.length, 0);
  assert.equal(storage.subagents.size, 0);
  assert.equal(storage.subagentCheckpoints.size, 0);
});

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, reject, resolve };
}

function gatewayFixtureResponse(request, value) {
  if (!request.stream) return Response.json(value);
  const choices = value.choices.map(({ message, finish_reason }, index) => ({
    index,
    delta: { ...message, ...(message.tool_calls && {
      tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })),
    }) },
    finish_reason,
  }));
  return new Response(`data: ${JSON.stringify({ choices })}\n\ndata: [DONE]\n\n`, {
    headers: { "content-type": "text/event-stream" },
  });
}

for (const provider of ["openrouter", "vercel"]) {
  test(`Cloudflare Agent pins ${provider} transport and effort over two tool turns`, {timeout:30_000}, async () => {
    const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    let calls=0, tools=0;
    const gateway = {provider,model:"gpt-6-astra",reasoningEffort:"low",apiKey:"synthetic-fixture-key",
      async fetch(url,init) {
        assert.equal(url,provider === "openrouter" ? "https://openrouter.ai/api/v1/chat/completions" : "https://ai-gateway.vercel.sh/v1/chat/completions");
        const body=JSON.parse(init.body); calls++;
        assert.equal(body.model,"openai/gpt-6-astra");
        assert.equal(provider === "openrouter" ? body.reasoning.effort : body.reasoning_effort,"low");
        if(calls===1 || calls===3){
          const tool=body.tools.find(t=>t.function.description.startsWith("runtimeInfo\n")); assert.ok(tool);
          if(calls===3)assert.ok(body.messages.some(m=>m.content?.includes("GATEWAY_TURN_1")));
          return gatewayFixtureResponse(body, {choices:[{finish_reason:"tool_calls",message:{content:null,tool_calls:[{id:`call-${calls}`,type:"function",function:{name:tool.function.name,arguments:"{}"}}]}}]});
        }
        assert.ok(body.messages.some(m=>m.role==="tool"&&m.content.includes("gateway-fixture")));
        return gatewayFixtureResponse(body, {choices:[{finish_reason:"stop",message:{content:`GATEWAY_TURN_${calls/2}`}}]});
      }};
    const agent=await create(module,durableOwner(new MemoryStorage()),{
      [Symbol.for("nanocodex.cloudflare.internalConfiguration")]:{model:gateway.model,thinking:"low",reasoning_mode:"standard",fast_mode:false},
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]:{gateway,toolMode:"direct",subagentsEnabled:false},
      tools:{runtimeInfo:{description:"Return fixture runtime",parameters:{type:"object",additionalProperties:false},handler(){tools++;return {runtime:"gateway-fixture"};}}},
    });
    try {
      assert.equal((await agent.turn.prompt({input:"Call runtimeInfo."}).result()).finalMessage,"GATEWAY_TURN_1");
      assert.equal((await agent.turn.prompt({input:"Call runtimeInfo again."}).result()).finalMessage,"GATEWAY_TURN_2");
      assert.equal(calls,4);assert.equal(tools,2);
    } finally {await agent.session.shutdown();}
  });
}

test("live child continuation preserves schema, history, routing, and spawning authorization until shutdown", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const routes = new Map();
  const lifecycleEvents = [];
  const requestedTurns = [];
  const childRequests = [];
  const marker = "CHILD_OBJECT_LIVE_HISTORY";
  let classifierCalls = 0;
  let acceptedReceipts = 0;
  let rejectedReceipts = 0;
  let invalidSecondResultSent = false;
  let childSessionId;
  let childTurn = 1;
  const childAi = { async run(model, input) {
    childRequests.push(input);
    assert.ok(childRequests.length <= 5, "bounded child requests during live continuation");
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.reasoning_effort, "high");
    assert.equal(routes.size, 1, "the child route remains retained until explicit close");
    const last = input.messages.at(-1);
    if (last?.role === "tool") {
      if (!last.content.includes("submitted output does not match the required schema")) {
        assert.deepEqual(JSON.parse(last.content), { accepted: true, status: "accepted", decoded_json_text: true });
        acceptedReceipts++;
        return { choices: [{ finish_reason: "stop", message: { content: `CHILD_DONE_${childTurn}` } }] };
      }
      assert.equal(childTurn, 2);
      assert.match(last.content, /submitted output does not match the required schema/,
        "the exact schema rejects extra properties");
      rejectedReceipts++;
    }
    if (childTurn === 2) {
      const history = JSON.stringify(input.messages);
      assert.ok(history.includes(marker), "the child retains its first object result in provider history");
      assert.ok(history.includes("CHILD_DONE_1"), "the first assistant turn remains in live memory");
    }
    requestedTurns.push(childTurn);
    const submit = input.tools.find((tool) => tool.function.description.startsWith("submit_result\n"));
    assert.ok(submit);
    const output = { ok: childTurn, marker: childTurn === 1 ? marker : "AFTER_CONTINUATION" };
    if (childTurn === 2 && !invalidSecondResultSent) {
      output.unexpected = "reject this extra property";
      invalidSecondResultSent = true;
    }
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id: `restart-submit-${childRequests.length}`, type: "function", function: {
        name: submit.function.name,
        arguments: JSON.stringify({ output: JSON.stringify(output) }),
      },
    }] } }] };
  } };
  const gateway = {
    provider: "openrouter", model: "gpt-6-sol", reasoningEffort: "low", apiKey: "synthetic-test-key",
    async fetch() { throw new Error("a child must never use the parent provider"); },
  };
  const options = {
    tools: { inspectAuthorization: { parameters: { type: "object" }, handler: (_input, context) => context.subagent } },
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: gateway.model, thinking: "low", reasoning_mode: "standard", fast_mode: false,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      gateway, toolMode: "direct", subagentsEnabled: true,
      subagentRouting: {
        async resolve(request) {
          classifierCalls++;
          assert.equal(request.parentSessionId, storage.sessionId);
          return { model: "@cf/zai-org/glm-5.3", thinking: "high", routeId: "restart-child-route" };
        },
        bind(request) {
          assert.equal(request.routeId, "restart-child-route");
          childSessionId = request.sessionId;
          routes.set(request.sessionId, {
            model: "@cf/zai-org/glm-5.3", thinking: "high",
            workersAi: { ai: childAi, model: "@cf/zai-org/glm-5.3", thinking: "high" },
          });
        },
      },
      subagentLifecycle(event) {
        lifecycleEvents.push(event);
        if (event.type === "release") routes.delete(event.sessionId);
      },
      inferenceForSession(id) {
        if (id === storage.sessionId) return { model: gateway.model, thinking: "low", gateway };
        assert.equal(id, childSessionId, "live continuation retains the child session identity");
        return routes.get(id);
      },
    },
  };
  let agent = await create(module, durableOwner(storage), options);
  try {
    const child = await Subagents.spawn(agent, {
      role: "restart-object-child", task: "Return an object with ok equal to 1 and a history marker.",
      outputSchema: {
        type: "object", properties: { ok: { type: "integer" }, marker: { type: "string" } },
        required: ["ok", "marker"], additionalProperties: false,
      },
    });
    const first = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(first.agents[0].status, { state: "completed", output: { ok: 1, marker } });
    const retainedDescriptor = lifecycleEvents.find(({ type }) => type === "bind").descriptor;
    assert.equal(retainedDescriptor.agentId, String(child.agent_id));
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);

    childTurn = 2;
    await Subagents.send(agent, { agentId: child.agent_id, purpose: "delegate", message: "Return another object with ok equal to 2." });
    const second = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(second.agents[0].status,
      { state: "completed", output: { ok: 2, marker: "AFTER_CONTINUATION" } });
    assert.deepEqual(requestedTurns, [1, 2, 2]);
    assert.equal(acceptedReceipts, 2);
    assert.equal(rejectedReceipts, 1);
    assert.equal(classifierCalls, 1, "the resumed child uses its original classifier choice");
    assert.equal(childRequests.length, 5);

    const delegatedContext = JSON.parse(await globalThis.nanocodexHost.executeTool("inspectAuthorization", "{}", childSessionId, "delegated-authority"));
    assert.deepEqual(delegatedContext.structured_result, retainedDescriptor,
      "delegating a new task retains the immutable spawning authorization descriptor");
    assert.equal((await Subagents.list(agent, { includeCompleted: true })).agents[0].task,
      "Return another object with ok equal to 2.");
    await agent.session.shutdown();
    assert.equal(routes.size, 0, "shutdown releases child routes");
    assert.deepEqual(lifecycleEvents.filter(({ type }) => type === "release").map(({ sessionId }) => sessionId), [childSessionId]);
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);
    const persisted = [...storage.records.values(), ...storage.states.map(({ payload }) => payload)].join("\n");
    assert.equal(persisted.includes(marker), false, "child history never reaches root durability");
    agent = await create(module, durableOwner(storage), options);
    assert.deepEqual((await Subagents.list(agent, { includeCompleted: true })).agents, []);
    await assert.rejects(Subagents.send(agent, { agentId: child.agent_id, message: "Cannot resume after restart." }));
    assert.equal(classifierCalls, 1);
    assert.equal(childRequests.length, 5, "restart neither restores nor replays child inference");
  } finally {
    await agent.session.shutdown();
  }
});

test("closing one live child preserves sibling history and its pinned route", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const routes = new Map();
  const sessionsByAgent = new Map();
  let classifierCalls = 0;
  let modelCalls = 0;
  let childTurn = 1;
  const ai = { async run(model, input) {
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.reasoning_effort, "high");
    modelCalls++;
    assert.ok(modelCalls <= 6, "bounded sibling model requests");
    if (input.messages.at(-1)?.role === "tool") {
      assert.deepEqual(JSON.parse(input.messages.at(-1).content), { accepted: true, status: "accepted" });
      return { choices: [{ finish_reason: "stop", message: { content: "SIBLING_DONE" } }] };
    }
    if (childTurn === 2) assert.ok(JSON.stringify(input.messages).includes("SIBLING_DONE"), "closing a sibling preserves live conversation history");
    const submit = input.tools.find((tool) => tool.function.description.startsWith("submit_result\n"));
    assert.ok(submit);
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id: `sibling-submit-${modelCalls}`, type: "function", function: {
        name: submit.function.name, arguments: JSON.stringify({ output: { turn: childTurn } }),
      },
    }] } }] };
  } };
  const profile = {
    model: "@cf/zai-org/glm-5.3", thinking: "high",
    workersAi: { ai, model: "@cf/zai-org/glm-5.3", thinking: "high" },
  };
  const identityTool = (source) => ({
    identity: {
      parameters: { type: "object", additionalProperties: false },
      handler: (_input, context) => ({ source, subagent: context.subagent }),
    },
  });
  const options = {
    tools: identityTool("predecessor"),
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: profile.model, thinking: profile.thinking, reasoning_mode: "standard", fast_mode: false,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      workersAi: profile.workersAi, toolMode: "direct", subagentsEnabled: true,
      subagentRouting: {
        async resolve() {
          classifierCalls++;
          return { model: profile.model, thinking: profile.thinking, routeId: `sibling-route-${classifierCalls}` };
        },
        bind({ sessionId }) { routes.set(sessionId, profile); },
      },
      subagentLifecycle(event) {
        if (event.type === "release") routes.delete(event.sessionId);
        else sessionsByAgent.set(event.descriptor.agentId, event.sessionId);
      },
      inferenceForSession(id) { return id === storage.sessionId ? profile : routes.get(id); },
    },
  };
  let agent = await create(module, durableOwner(storage), options);
  try {
    const children = [];
    for (const role of ["child-to-close", "retained-sibling"]) {
      const child = await Subagents.spawn(agent, {
        role, task: "Return an object with turn equal to 1.",
        outputSchema: {
          type: "object", properties: { turn: { type: "integer" } },
          required: ["turn"], additionalProperties: false,
        },
      });
      const result = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
      assert.deepEqual(result.agents[0].status, { state: "completed", output: { turn: 1 } });
      children.push(child);
    }
    const [closed, retained] = children;
    const retainedSession = sessionsByAgent.get(String(retained.agent_id));
    const closedSession = sessionsByAgent.get(String(closed.agent_id));
    await Subagents.close(agent, closed.agent_id);
    assert.equal(routes.has(closedSession), false);
    assert.equal(routes.has(retainedSession), true);
    assert.throws(() => globalThis.nanocodexHost.executeTool("identity", "{}", closedSession, "closed-child"), /no Nanocodex host is active/);
    const routed = JSON.parse(await globalThis.nanocodexHost.executeTool("identity", "{}", retainedSession, "live-sibling"));
    assert.equal(routed.structured_result.subagent.agentId, String(retained.agent_id));
    childTurn = 2;
    await Subagents.send(agent, { agentId: retained.agent_id, message: "Return an object with turn equal to 2." });
    const resumed = await Subagents.wait(agent, { agentIds: [retained.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(resumed.agents[0].status, { state: "completed", output: { turn: 2 } });
    assert.equal(classifierCalls, 2, "continuing the sibling reuses its live route");
    assert.equal(modelCalls, 6);
    await agent.session.shutdown();
    assert.equal(routes.size, 0);
    assert.equal(storage.subagents.size, 0);
    assert.equal(storage.subagentCheckpoints.size, 0);
    agent = await create(module, durableOwner(storage), options);
    assert.deepEqual((await Subagents.list(agent, { includeCompleted: true })).agents, []);
    assert.equal(modelCalls, 6);
  } finally {
    await agent.session.shutdown();
  }
});


test("Cloudflare SDK sibling shutdown preserves live siblings without child checkpoints", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const agent = await create(module, durableOwner(storage), { tools: {
    identity: { parameters: { type: "object" }, handler: () => ({ source: "independent-sibling" }) },
  } });
  const first = await agent.session.spawn();
  const retained = await agent.session.spawn();
  try {
    await first.session.shutdown();
    assert.equal(storage.subagentCheckpoints.size, 0, "SDK sibling shutdown does not create child checkpoints");
    const result = JSON.parse(await globalThis.nanocodexHost.executeTool("identity", "{}", retained.sessionId, "sibling-with-live-root"));
    assert.equal(result.structured_result.source, "independent-sibling");
    await agent.session.shutdown();
    assert.equal(storage.subagentCheckpoints.size, 0, "root shutdown does not persist children");
    assert.throws(() => globalThis.nanocodexHost.executeTool("identity", "{}", retained.sessionId, "released-root"), /no Nanocodex host is active/);
    await retained.session.shutdown();
    assert.equal(storage.subagentCheckpoints.size, 0, "SDK sibling shutdown does not persist children");
  } finally {
    await first.session.shutdown();
    await retained.session.shutdown();
    await agent.session.shutdown();
  }
});

test("manual GPT root keeps WebSockets while a Kimi child uses gateway HTTP across continuation", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const routes = new Map();
  const rootRequests = [];
  let sockets = 0, childCalls = 0, choices = 0, childTurn = 1, authorized = true;
  class RootSocket extends EventTarget {
    readyState = 1;
    accept() {}
    close() { this.readyState = 3; }
    send(encoded) {
      const request = JSON.parse(encoded);
      rootRequests.push(request);
      assert.ok(rootRequests.length <= 4);
      assert.equal(request.model, "gpt-6-astra");
      assert.equal(request.reasoning.effort, "xhigh");
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed", response: {
          id: `manual-root-${rootRequests.length}`, status: "completed", end_turn: true,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "MANUAL_ROOT_OK" }] }],
          usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
        },
      }) })));
    }
  }
  const egress = { async fetch(_url, init) {
    assert.equal(init.method, "GET", "manual root retains WebSocket transport");
    assert.equal(init.headers.get("thread-id"), storage.sessionId, "children never reach parent egress");
    sockets++;
    return { status: 101, headers: new Headers(), webSocket: new RootSocket() };
  } };
  const gateway = {
    provider: "openrouter", model: "kimi-k3", reasoningEffort: "low", apiKey: "synthetic-test-key",
    async fetch(_url, init) {
      childCalls++;
      assert.ok(childCalls <= 4, "bounded child requests");
      assert.equal(routes.size, 1, "child route is bound before inference");
      const body = JSON.parse(init.body);
      assert.equal(body.model, "moonshotai/kimi-k3");
      assert.equal(body.reasoning.effort, "low");
      if (childTurn === 2) assert.ok(JSON.stringify(body.messages).includes("KIMI_HISTORY_1"), "continuation replays child history");
      if (body.messages.at(-1)?.role === "tool") {
        return gatewayFixtureResponse(body, { choices: [{ finish_reason: "stop", message: { content: `KIMI_HISTORY_${childTurn}` } }] });
      }
      const submit = body.tools.find(tool => tool.function.description.startsWith("submit_result\n"));
      assert.ok(submit);
      return gatewayFixtureResponse(body, { choices: [{ finish_reason: "tool_calls", message: {
        content: null, tool_calls: [{ id: `manual-child-${childCalls}`, type: "function", function: {
          name: submit.function.name, arguments: JSON.stringify({ output: JSON.stringify({ turn: childTurn }) }),
        } }],
      } }] });
    },
  };
  const agent = await create(module, durableOwner(storage, egress), {
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-6-astra", thinking: "xhigh", reasoning_mode: "standard", fast_mode: false,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      preserveRootTransport: true, toolMode: "direct", subagentsEnabled: true,
      subagentRouting: {
        async resolve(request) {
          if (!authorized) throw new Error("gateway authorization lost");
          choices++;
          assert.equal(request.parentSessionId, storage.sessionId);
          assert.equal(request.model, "kimi-k3");
          return { model: "kimi-k3", thinking: "low", routeId: "manual-kimi-route", statelessHttp: true };
        },
        bind(request) {
          routes.set(request.sessionId, { model: "kimi-k3", thinking: "low", gateway });
        },
      },
      inferenceForSession(id) {
        if (id === storage.sessionId) return { native: true, model: "gpt-6-astra", thinking: "xhigh" };
        if (!authorized) throw new Error("gateway authorization lost");
        return routes.get(id);
      },
    },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "Respond briefly." }).result()).finalMessage, "MANUAL_ROOT_OK");
    const child = await Subagents.spawn(agent, {
      role: "gateway-child", task: "Return the first result.", model: "kimi", thinking: "low",
      outputSchema: { type: "object", properties: { turn: { type: "integer" } }, required: ["turn"], additionalProperties: false },
    });
    assert.deepEqual((await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 })).agents[0].status,
      { state: "completed", output: { turn: 1 } });
    childTurn = 2;
    await Subagents.send(agent, { agentId: child.agent_id, message: "Return the second result." });
    assert.deepEqual((await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 })).agents[0].status,
      { state: "completed", output: { turn: 2 } });
    assert.equal((await agent.turn.prompt({ input: "Respond again." }).result()).finalMessage, "MANUAL_ROOT_OK");
    assert.equal(choices, 1);
    assert.equal(childCalls, 4);
    assert.ok(sockets >= 1);
    assert.ok(rootRequests.length >= 2);
    authorized = false;
    await assert.rejects(Subagents.spawn(agent, { role: "denied-gateway", task: "Must not start.", model: "kimi",
      thinking: "low", outputSchema: { type: "object" } }), /not authorized/);
    await Subagents.send(agent, { agentId: child.agent_id, message: "Authorization has been revoked." });
    assert.equal((await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 })).agents[0].status.state, "failed");
    assert.equal(choices, 1);
    assert.equal(childCalls, 4, "revoked continuation fails before provider inference");
  } finally { await agent.session.shutdown(); }
});

test("manual GPT children preserve native defaults, max/xhigh/none, fast mode, and binding before WebSockets", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const native = new Set();
  const requests = new Map();
  let choices = 0, admitted = true, responseId = 0;
  class NativeSocket extends EventTarget {
    readyState = 1;
    constructor(id) { super(); this.id = id; }
    accept() {}
    close() { this.readyState = 3; }
    send(encoded) {
      const request = JSON.parse(encoded);
      const isChild = this.id !== storage.sessionId;
      if (isChild) {
        assert.ok(native.has(this.id), "native admission is bound before inference");
        const seen = requests.get(this.id) ?? [];
        seen.push(request);
        requests.set(this.id, seen);
      }
      const submitted = request.input.some(item => item.type === "function_call_output");
      const output = request.generate === false ? [] : isChild && !submitted ? [{
        type: "function_call", call_id: `native-submit-${++responseId}`, name: "submit_result",
        arguments: JSON.stringify({ output: { ok: true } }),
      }] : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "NATIVE_OK" }] }];
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed", response: { id: `native-response-${++responseId}`, status: "completed", output,
          usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } },
      }) })));
    }
  }
  const egress = { async fetch(_url, init) {
    assert.equal(init.method, "GET", "native children retain WebSockets");
    return { status: 101, headers: new Headers(), webSocket: new NativeSocket(init.headers.get("thread-id")) };
  } };
  const agent = await create(module, durableOwner(storage, egress), {
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-6-astra", thinking: "xhigh", reasoning_mode: "standard", fast_mode: true,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      preserveRootTransport: true, toolMode: "direct", subagentsEnabled: true,
      subagentRouting: {
        async resolve() { choices++; return { native: true, routeId: `native-${choices}` }; },
        bind(request) { if (!admitted) throw new Error("spawning authorization lost"); native.add(request.sessionId); },
      },
      inferenceForSession(id) {
        if (id === storage.sessionId) return { native: true };
        if (!admitted || !native.has(id)) throw new Error("native child authorization missing");
        return { native: true };
      },
    },
  });
  try {
    await agent.session.setThinking("max");
    for (const [overrides, model, thinking] of [
      [{}, "gpt-6-astra", "max"],
      [{ model: "astra", thinking: "max" }, "gpt-6-astra", "max"],
      [{ model: "luna", thinking: "xhigh" }, "gpt-6-luna", "xhigh"],
      [{ model: "sol", thinking: "none" }, "gpt-6-sol", "none"],
    ]) {
      const child = await Subagents.spawn(agent, { role: "native-child", task: "Submit ok true.", ...overrides,
        outputSchema: { type: "object", properties: { ok: { type: "boolean" } }, required: ["ok"], additionalProperties: false } });
      assert.deepEqual((await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 })).agents[0].status,
        { state: "completed", output: { ok: true } });
      const seen = [...requests.values()].at(-1);
      assert.ok(seen.length >= 2);
      for (const request of seen) {
        assert.equal(request.model, model);
        assert.equal(request.reasoning.effort, thinking);
        assert.equal(request.service_tier, "priority");
      }
    }
    const created = native.size;
    await assert.rejects(Subagents.spawn(agent, { role: "invalid-native", task: "Must fail before inference.", model: "kimi",
      thinking: "low", outputSchema: { type: "object" } }), /invalid native subagent choice/);
    assert.equal(native.size, created, "a native choice cannot admit an explicit non-GPT request");
    admitted = false;
    await assert.rejects(Subagents.spawn(agent, { role: "revoked-native", task: "Must fail before inference.",
      outputSchema: { type: "object" } }), /binding failed/);
    assert.equal(native.size, created);
  } finally { await agent.session.shutdown(); }
});

test("manual root HTTP fallback follows live thinking changes", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const efforts = [];
  let sockets = 0;
  const egress = { async fetch(_url, init) {
    if (init.method === "GET") {
      sockets++;
      return { status: 426, headers: new Headers() };
    }
    const body = JSON.parse(init.body);
    efforts.push(body.reasoning.effort);
    assert.equal(body.model, "gpt-6-astra");
    const response = { type: "response.completed", response: { id: `http-${efforts.length}`, status: "completed",
      output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "HTTP_OK" }] }],
      usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } };
    return new Response(`data: ${JSON.stringify(response)}\n\ndata: [DONE]\n\n`, { headers: { "content-type": "text/event-stream" } });
  } };
  const agent = await create(module, durableOwner(storage, egress), {
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-6-astra", thinking: "xhigh", reasoning_mode: "standard", fast_mode: false,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      preserveRootTransport: true, waitForPreconnect: false,
      inferenceForSession(id) {
        assert.equal(id, storage.sessionId);
        return { native: true };
      },
    },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "First HTTP turn." }).result()).finalMessage, "HTTP_OK");
    await agent.session.setThinking("max");
    assert.equal((await agent.turn.prompt({ input: "Second HTTP turn." }).result()).finalMessage, "HTTP_OK");
    assert.deepEqual(efforts, ["xhigh", "max"]);
    assert.ok(sockets > 0, "real WASM starts on WebSocket and falls back to HTTP");
  } finally { await agent.session.shutdown(); }
});

test("Cloudflare internal socket timing reaches the real InlineAgent host and closes once", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const observations = [];
  class TimingSocket extends EventTarget {
    readyState = 1;
    bufferedAmount = 0;
    accept() {}
    close() { this.readyState = 3; }
    send() {
      queueMicrotask(() => {
        for (const event of [
          { type: "responsesapi.websocket_timing", response_id: "resp_integration",
            timing_metrics: { pre_inference_ms: 21, engine_queue_max_ms: 3, engine_service_ttft_total_ms: 10 } },
          { type: "response.completed", response: { id: "resp_integration", status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "OK" }] }],
            usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } },
        ]) this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(event) }));
      });
    }
  }
  const owner = durableOwner(new MemoryStorage(), { async fetch() {
    return { status: 101, headers: new Headers(), webSocket: new TimingSocket() };
  } });
  await assert.rejects(create(module, owner, {
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { onSocketTiming: true },
  }), /socket timing hook must be a function/);
  const agent = await create(module, owner, {
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { onSocketTiming: value => observations.push(value) },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "Return OK." }).result()).finalMessage, "OK");
  } finally { await agent.session.shutdown(); }
  assert.equal(observations.length, 1);
  assert.equal(observations[0].message_count, 2);
  assert.equal(observations[0].delivered_message_count, 2);
  assert.equal(observations[0].discarded_message_count, 0);
  assert.deepEqual(observations[0].provider_timings, [{ response_id: "resp_integration",
    pre_inference_ms: 21, engine_queue_max_ms: 3, engine_service_ttft_total_ms: 10 }]);
});

function nativePreparationOptions(prepare, signal) {
  return {
    durabilityId: "fixture-prepared-state",
    eventPersistence: "caller",
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false,
    },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: prepare === undefined
      ? { waitForPreconnect: false }
      : { prepare, preparationSignal: signal },
  };
}

test("owned native preparation overlaps discovery and transfers exactly one scoped socket", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const discovery = deferred();
  const dial = deferred();
  const socket = new UpstreamSocket();
  let requests = 0;
  const owner = durableOwner(storage, { async fetch(_url, init) {
    requests++;
    assert.equal(init.method, "GET");
    assert.equal(init.body, undefined);
    assert.equal(init.headers.get("x-nanocodex-subject"), FIRST_OBJECT_ID);
    assert.equal(init.headers.get("session-id"), storage.sessionId);
    assert.notEqual(storage.sessionId, FIRST_OBJECT_ID);
    dial.resolve();
    return { status: 101, headers: new Headers(), webSocket: socket };
  } });
  const creating = create(module, owner, nativePreparationOptions(async finish => {
    await discovery.promise;
    return finish(nativePreparationOptions());
  }));
  await dial.promise;
  assert.equal(requests, 1, "dial begins while discovery remains blocked");
  assert.throws(() => destroy(owner), /creation must settle/);
  await assert.rejects(exportDurabilityState(owner), /lifecycle operation/);
  await assert.rejects(create(module, owner), /already in progress/);
  discovery.resolve();
  const agent = await creating;
  assert.equal(agent.sessionId, storage.sessionId);
  assert.equal(requests, 1, "host adopts the already owned connection");
  await agent.session.shutdown();
  assert.equal(socket.closed, true);
  assert.doesNotThrow(() => destroy(owner));
});

test("aborted preparation closes a late socket and releases lifecycle after discovery joins", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const discovery = deferred();
  const dial = deferred();
  const socketReady = deferred();
  const socket = new UpstreamSocket();
  const controller = new AbortController();
  const owner = durableOwner(new MemoryStorage(), { async fetch() {
    dial.resolve();
    await socketReady.promise;
    return { status: 101, headers: new Headers(), webSocket: socket };
  } });
  const creating = create(module, owner, nativePreparationOptions(async finish => {
    await discovery.promise;
    return finish(nativePreparationOptions());
  }, controller.signal));
  await dial.promise;
  controller.abort();
  assert.throws(() => destroy(owner), /creation must settle/);
  discovery.resolve();
  await assert.rejects(creating, /abort/i);
  socketReady.resolve();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(socket.closed, true);
  const agent = await create(module, owner);
  await agent.session.shutdown();
});

test("preparation failure and changed model close the owned socket without creating a runtime", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  for (const failure of ["discovery", "model", "route"]) {
    const socket = new UpstreamSocket();
    const dial = deferred();
    const owner = durableOwner(new MemoryStorage(), { async fetch() {
      dial.resolve();
      return { status: 101, headers: new Headers(), webSocket: socket };
    } });
    const options = nativePreparationOptions(async finish => {
      await dial.promise;
      if (failure === "discovery") throw new Error("fixture discovery failed");
      const prepared = nativePreparationOptions();
      if (failure === "model") prepared[Symbol.for("nanocodex.cloudflare.internalConfiguration")].model = "gpt-6-luna";
      else prepared[Symbol.for("nanocodex.cloudflare.internalRuntime")].gateway = {};
      return finish(prepared);
    });
    await assert.rejects(create(module, owner, options), /discovery failed|changed its pinned transport/);
    await new Promise(resolve => setTimeout(resolve, 0));
    assert.equal(socket.closed, true);
    assert.doesNotThrow(() => destroy(owner));
  }
});

test("a preparation continuation cannot create after its owning lifecycle has ended", async () => {
  const owner = durableOwner(new MemoryStorage());
  let finish;
  await assert.rejects(create(undefined, owner, nativePreparationOptions(complete => {
    finish = complete;
  })), /must return its completed Agent/);
  assert.throws(() => finish(nativePreparationOptions()), /already completed/);
  assert.doesNotThrow(() => destroy(owner));
});

test("host shutdown closes a transferred preparation socket that resolves late", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const ready = deferred();
  const socket = new UpstreamSocket();
  let requests = 0;
  const owner = durableOwner(new MemoryStorage(), { async fetch() {
    requests++;
    await ready.promise;
    return { status: 101, headers: new Headers(), webSocket: socket };
  } });
  const agent = await create(module, owner, nativePreparationOptions(finish => finish(nativePreparationOptions())));
  assert.equal(requests, 1);
  await agent.session.shutdown();
  ready.resolve();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(socket.closed, true);
});
