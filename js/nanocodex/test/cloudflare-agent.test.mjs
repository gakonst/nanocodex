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
    this.subagentHostContextColumn = true;
    this.subagentSchemaAlterations = 0;
    this.meta = { total_bytes: 0, stream_error: null };
    this.sessionId = undefined;
    this.stateId = undefined;
    this.sql = { exec: (sql, ...args) => this.#exec(sql, args) };
  }

  transactionSync(callback) { return callback(); }

  #exec(sql, args) {
    const statement = sql.replace(/\s+/g, " ").trim();
    let rows = [];
    let rowsWritten = 0;
    if (statement.startsWith("CREATE TABLE")) {
      // Schema setup is idempotent.
    } else if (statement.startsWith("ALTER TABLE nanocodex_cloudflare_subagents")) {
      this.subagentHostContextColumn = true;
      this.subagentSchemaAlterations += 1;
    } else if (statement.startsWith("PRAGMA table_info")) {
      rows = durabilityPragmaRows(statement, this.subagentHostContextColumn);
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
    } else if (statement.startsWith("SELECT chunk_index, payload FROM nanocodex_cloudflare_subagent_checkpoints")) {
      rows = [...this.subagentCheckpoints].map(([chunk_index, payload]) => ({ chunk_index, payload }))
        .sort((left, right) => left.chunk_index - right.chunk_index);
    } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_subagent_checkpoints")) {
      this.subagentCheckpoints.set(args[0], args[1]);
      rowsWritten = 1;
    } else if (statement.startsWith("DELETE FROM nanocodex_cloudflare_subagent_checkpoints")) {
      if (this.failCheckpointDeletion) throw new Error("checkpoint deletion failed");
      rowsWritten = this.subagentCheckpoints.size;
      this.subagentCheckpoints.clear();
    } else if (statement.startsWith(
      "SELECT descriptor_json, host_context_ref FROM nanocodex_cloudflare_subagents",
    )) {
      this.onSubagentLoad?.();
      rows = [...this.subagents.values()]
        .map(({ descriptorJson, hostContextRef }) => ({
          descriptor_json: descriptorJson,
          host_context_ref: hostContextRef ?? null,
        }));
    } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_subagents")) {
      this.subagents.set(args[0], {
        agentId: args[1],
        descriptorJson: args[2],
        hostContextRef: args[3],
      });
      rowsWritten = 1;
    } else if (statement.startsWith(
      "SELECT 1 AS retained FROM nanocodex_cloudflare_subagents",
    )) {
      const retained = this.subagents.get(args[0]);
      rows = retained?.hostContextRef === args[1] ? [{ retained: 1 }] : [];
    } else if (statement.startsWith("DELETE FROM nanocodex_cloudflare_subagents")) {
      if (args.length > 1) {
        const retained = this.subagents.get(args[0]);
        if (retained?.hostContextRef === args[1]) {
          rowsWritten = Number(this.subagents.delete(args[0]));
        }
      } else if (args.length > 0) {
        rowsWritten = Number(this.subagents.delete(args[0]));
      } else {
        rowsWritten = this.subagents.size;
        this.subagents.clear();
      }
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

function durabilityPragmaRows(sql, subagentHostContextColumn = true) {
  let shapes;
  if (sql.includes("nanocodex_cloudflare_subagent_checkpoints")) {
    shapes = [["chunk_index", "INTEGER", 0, 1], ["payload", "TEXT", 1, 0]];
  } else if (sql.includes("nanocodex_cloudflare_subagents")) {
    shapes = [
      ["session_id", "TEXT", 0, 1],
      ["agent_id", "TEXT", 1, 0],
      ["descriptor_json", "TEXT", 1, 0],
      ...(subagentHostContextColumn ? [["host_context_ref", "TEXT", 0, 0]] : []),
    ];
  } else if (sql.includes("nanocodex_durable_owners")) {
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

test("Cloudflare Agent owns credentials, transport, and durability options", async () => {
  const module = new Uint8Array();
  await assert.rejects(create(module), /requires a Durable Object instance/);
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { apiKey: "managed-secret" }),
    /does not accept apiKey; only durabilityId, eventPersistence, instructions, additionalInstructions, terminalReceiptRetention, and tools are configurable/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { CODEX_OAUTH_BOOTSTRAP: "managed-secret" }),
    /does not accept CODEX_OAUTH_BOOTSTRAP/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { transport: {} }),
    /does not accept transport/,
  );
  for (const name of [
    "model", "thinking", "reasoningMode", "fastMode",
    "filesystem", "mcp", "codeEvaluator", "toolMode",
    "waitForPreconnect",
  ]) {
    await assert.rejects(
      create(module, durableOwner(new MemoryStorage()), { [name]: "forbidden" }),
      new RegExp(`does not accept ${name}`),
    );
  }
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { subject: "caller-selected" }),
    /does not accept subject/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { eventPersistence: "somewhere" }),
    /eventPersistence must be durable or caller/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), { terminalReceiptRetention: -1 }),
    /terminalReceiptRetention must be an integer from 0 through 4096/,
  );
  await assert.rejects(
    create(module, { ctx: durableContext(new MemoryStorage()), env: {} }),
    /owner\.env\.NANOCODEX Service Binding/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), {
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: [],
    }),
    /internal runtime options must be an object/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), {
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
        subagentLifecycle: true,
      },
    }),
    /subagent lifecycle hook must be a function/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), {
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { subagentMaxConcurrency: 0 },
    }),
    /subagentMaxConcurrency must be a positive safe integer/,
  );
  await assert.rejects(
    create(module, durableOwner(new MemoryStorage()), {
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { waitForPreconnect: "false" },
    }),
    /waitForPreconnect must be a boolean/,
  );
  await assert.rejects(
    create(module, { env: { NANOCODEX: egressBinding() } }),
    /requires owner\.ctx/,
  );
  await assert.rejects(
    create(module, { ctx: durableContext(new MemoryStorage(), ""), env: { NANOCODEX: egressBinding() } }),
    /requires owner\.ctx\.id/,
  );
  await assert.rejects(
    create(module, {
      ctx: { id: { toString: () => FIRST_OBJECT_ID } },
      env: { NANOCODEX: egressBinding() },
    }),
    /requires Durable Object SQLite storage/,
  );
});

test("Cloudflare Agent accepts complete hosted policy only through its internal configuration", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const owner = durableOwner(new MemoryStorage());
  let captured;
  const configured = bindAgent(module, {
    async create(options) {
      captured = options;
      return HostAgent.create(options);
    },
  });
  const agent = await configured.create(owner, {
    additionalInstructions: "Keep the host's account boundaries.",
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-6-astra",
      thinking: "xhigh",
      reasoning_mode: "standard",
      fast_mode: true,
    },
  });

  assert.equal(captured.model, "gpt-6-astra");
  assert.equal(captured.instructions, undefined);
  assert.equal(captured.additionalInstructions, "Keep the host's account boundaries.");
  assert.equal(captured.thinking, "xhigh");
  assert.equal(captured.reasoningMode, "standard");
  assert.equal(captured.fastMode, true);
  await agent.session.shutdown();

  await assert.rejects(configured.create(owner, {
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: {
      model: "gpt-5.6-terra",
      thinking: "xhigh",
      reasoning_mode: "pro",
      fast_mode: "true",
    },
  }), /internal configuration is invalid/);
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
  await assert.rejects(create(module, durableOwner(new MemoryStorage()), {
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: { subagentsEnabled: "false" },
  }), /subagentsEnabled must be a boolean/);
});

test("Cloudflare ephemeral Agent owns transport without durable state", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const subjects = [];
  const owner = durableOwner(storage, egressBinding(subjects));
  const agent = await createEphemeral(module, owner, {
    instructions: "Use the caller's search tool.",
    model: "gpt-5.6-sol",
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

test("Cloudflare Agent reconstructs interrupted subagents without stale-owner cleanup races", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const identity = (source) => ({
    identity: {
      parameters: { type: "object", additionalProperties: false },
      handler: (_input, context) => ({ source, subagent: context.subagent ?? null }),
    },
  });
  const predecessorLifecycle = [];
  const predecessorOptions = { tools: identity("predecessor") };
  Object.defineProperty(
    predecessorOptions,
    Symbol.for("nanocodex.cloudflare.internalRuntime"),
    { value: { subagentLifecycle: (event) => predecessorLifecycle.push(event) } },
  );
  const first = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
    predecessorOptions,
  );
  const bridge = globalThis.nanocodexHost;
  const predecessorBinds = [];
  globalThis.nanocodexHost = Object.freeze({
    ...bridge,
    bindSubagentSession(...args) {
      predecessorBinds.push(args);
      return bridge.bindSubagentSession(...args);
    },
  });
  let started;
  let continued;
  try {
    started = await Subagents.spawn(first, {
      role: "durability-check",
      task: "Remain available until the owner is reconstructed.",
      outputSchema: { type: "object" },
    });
    continued = await Subagents.spawn(first, {
      role: "forwarding-check",
      task: "Prove descriptor forwarding continues after one callback failure.",
      outputSchema: { type: "object" },
    });
  } finally {
    globalThis.nanocodexHost = bridge;
  }
  await eventually(() => assert.equal(storage.subagents.size, 2));
  assert.equal(predecessorBinds.length, 2);
  const descriptor = [...storage.subagents.values()]
    .map(({ descriptorJson }) => JSON.parse(descriptorJson))
    .find((candidate) => candidate.agentId === String(started.agent_id));
  const continuedDescriptor = [...storage.subagents.values()]
    .map(({ descriptorJson }) => JSON.parse(descriptorJson))
    .find((candidate) => candidate.agentId === String(continued.agent_id));
  const predecessorBind = predecessorBinds.find((args) => args[2] === descriptor.sessionId);
  const hostContextRef = "opaque-root-turn";
  bridge.bindSubagentSession(
    predecessorBind[0],
    predecessorBind[1],
    predecessorBind[2],
    predecessorBind[3],
    hostContextRef,
  );
  assert.equal(storage.subagents.get(descriptor.sessionId).hostContextRef, hostContextRef);
  assert.equal(JSON.stringify(descriptor).includes(hostContextRef), false);
  assert.deepEqual(
    predecessorLifecycle
      .filter(({ hostContextRef: retained }) => retained === hostContextRef)
      .map(({ type, sessionId, hostContextRef: retained }) => ({
        type,
        sessionId,
        hostContextRef: retained,
      })),
    [{ type: "bind", sessionId: descriptor.sessionId, hostContextRef }],
  );
  const predecessorFence = storage.owners.get(storage.stateId).fence;
  storage.onSubagentLoad = () => assert.ok(
    BigInt(storage.owners.get(storage.stateId).fence) > BigInt(predecessorFence),
    "restored descriptors must load only after the replacement acquires its durability fence",
  );

  const replacementLifecycle = [];
  const replacementOptions = { tools: identity("replacement") };
  Object.defineProperty(
    replacementOptions,
    Symbol.for("nanocodex.cloudflare.internalRuntime"),
    { value: { subagentLifecycle: (event) => replacementLifecycle.push(event) } },
  );
  const reconstructed = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
    replacementOptions,
  );
  storage.onSubagentLoad = undefined;
  const listed = await Subagents.list(reconstructed, {
    includeCompleted: true,
    includeSelf: true,
  });
  const restored = listed.agents.find((entry) => entry.agent_id === started.agent_id);
  assert.deepEqual(restored?.status, { state: "interrupted" });
  assert.deepEqual(
    listed.agents.find((entry) => entry.agent_id === continued.agent_id)?.status,
    { state: "interrupted" },
  );
  assert.equal(descriptor.agentId, String(started.agent_id));
  assert.deepEqual(
    replacementLifecycle
      .filter(({ hostContextRef: retained }) => retained === hostContextRef)
      .map(({ type, sessionId, hostContextRef: retained }) => ({
        type,
        sessionId,
        hostContextRef: retained,
      })),
    [{ type: "reconstruct", sessionId: descriptor.sessionId, hostContextRef }],
  );

  let routed = JSON.parse(await globalThis.nanocodexHost.executeTool(
    "identity", "{}", descriptor.sessionId, "replacement-before-stale-release",
  ));
  assert.equal(routed.structured_result.source, "replacement");
  assert.deepEqual(routed.structured_result.subagent, descriptor);
  routed = JSON.parse(await globalThis.nanocodexHost.executeTool(
    "identity", "{}", continuedDescriptor.sessionId, "continued-after-bind-failure",
  ));
  assert.equal(routed.structured_result.source, "replacement");
  assert.deepEqual(routed.structured_result.subagent, continuedDescriptor);

  const staleDescriptor = { ...descriptor, role: "stale-predecessor-rebind" };
  const predecessorLifecycleCount = predecessorLifecycle.length;
  const replacementLifecycleCount = replacementLifecycle.length;
  bridge.bindSubagentSession(
    predecessorBind[0],
    predecessorBind[1],
    predecessorBind[2],
    JSON.stringify(staleDescriptor),
  );
  assert.equal(predecessorLifecycle.length, predecessorLifecycleCount);
  assert.equal(replacementLifecycle.length, replacementLifecycleCount);
  assert.equal(
    storage.subagents.get(descriptor.sessionId).descriptorJson,
    JSON.stringify(descriptor),
  );
  assert.equal(storage.subagents.size, 2);
  routed = JSON.parse(await globalThis.nanocodexHost.executeTool(
    "identity", "{}", descriptor.sessionId, "replacement-after-stale-bind",
  ));
  assert.equal(routed.structured_result.source, "replacement");
  assert.deepEqual(routed.structured_result.subagent, descriptor);

  await first.session.shutdown();
  assert.equal(predecessorLifecycle.length, predecessorLifecycleCount);
  assert.equal(replacementLifecycle.length, replacementLifecycleCount);
  routed = JSON.parse(await globalThis.nanocodexHost.executeTool(
    "identity", "{}", descriptor.sessionId, "replacement-after-stale-release",
  ));
  assert.equal(routed.structured_result.source, "replacement");
  assert.equal(storage.subagents.size, 2);

  await Subagents.close(reconstructed, started.agent_id);
  await Subagents.close(reconstructed, continued.agent_id);
  await reconstructed.session.shutdown();
  assert.equal(storage.subagents.size, 0);
  assert.deepEqual(
    replacementLifecycle
      .filter(({ hostContextRef: retained }) => retained === hostContextRef)
      .map(({ type, sessionId, hostContextRef: retained }) => ({
        type,
        sessionId,
        hostContextRef: retained,
      })),
    [
      { type: "reconstruct", sessionId: descriptor.sessionId, hostContextRef },
      { type: "release", sessionId: descriptor.sessionId, hostContextRef },
    ],
  );
  assert.throws(
    () => globalThis.nanocodexHost.executeTool(
      "identity", "{}", descriptor.sessionId, "after-planned-release",
    ),
    /no Nanocodex host is active/,
  );
});

test("Cloudflare Agent migrates and restores legacy subagent rows without private refs", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const binding = egressBinding();
  const first = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
  );
  const started = await Subagents.spawn(first, {
    role: "legacy-ref",
    task: "Remain reconstructable without private provenance.",
    outputSchema: { type: "object" },
  });
  await eventually(() => assert.equal(storage.subagents.size, 1));
  const retained = [...storage.subagents.values()][0];
  delete retained.hostContextRef;
  storage.subagentHostContextColumn = false;

  const reconstructed = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
  );
  assert.equal(storage.subagentHostContextColumn, true);
  assert.equal(storage.subagentSchemaAlterations, 1);
  const listed = await Subagents.list(reconstructed, { includeCompleted: true });
  assert.deepEqual(
    listed.agents.find(({ agent_id }) => agent_id === started.agent_id)?.status,
    { state: "interrupted" },
  );
  assert.equal([...storage.subagents.values()][0].hostContextRef, null);

  first.dispose();
  await Subagents.close(reconstructed, started.agent_id);
  await reconstructed.session.shutdown();
  assert.equal(storage.subagents.size, 0);
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
  await eventually(() => assert.equal(storage.subagents.size, 1));
  await Subagents.interrupt(agent, started.agent_id);
  const bind = binds[0];
  assert.ok(bind);

  assert.throws(
    () => bridge.releaseSubagentSession(bind[0], bind[1], bind[2]),
    /private release failed/,
  );
  assert.equal(storage.subagents.size, 1);
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
  const started = await Subagents.spawn(first, {
    role: "retry-proof",
    task: "Remain reconstructable after setup failure.",
    outputSchema: { type: "object" },
  });
  await eventually(() => assert.equal(storage.subagents.size, 1));
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
  assert.equal(storage.subagents.size, 1);
  await assert.rejects(
    create(module, durableOwner(storage, binding, SECOND_OBJECT_ID)),
    /session ID is already active/,
  );

  const reconstructed = await create(
    module,
    durableOwner(storage, binding, FIRST_OBJECT_ID),
  );
  const restored = await Subagents.list(reconstructed, { includeCompleted: true });
  assert.deepEqual(
    restored.agents.find((entry) => entry.agent_id === started.agent_id)?.status,
    { state: "interrupted" },
  );
  first.dispose();
  await Subagents.close(reconstructed, started.agent_id);
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

async function eventually(assertion) {
  let error;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try {
      await assertion();
      return;
    } catch (candidate) {
      error = candidate;
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }
  throw error;
}

for (const provider of ["openrouter", "vercel"]) {
  test(`Cloudflare Agent pins ${provider} transport and effort over two tool turns`, {timeout:30_000}, async () => {
    const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    let calls=0, tools=0;
    const gateway = {provider,model:"gpt-5.6-sol",reasoningEffort:"low",apiKey:"synthetic-fixture-key",
      async fetch(url,init) {
        assert.equal(url,provider === "openrouter" ? "https://openrouter.ai/api/v1/chat/completions" : "https://ai-gateway.vercel.sh/v1/chat/completions");
        const body=JSON.parse(init.body); calls++;
        assert.equal(body.model,"openai/gpt-5.6-sol");
        assert.equal(provider === "openrouter" ? body.reasoning.effort : body.reasoning_effort,"low");
        if(calls===1 || calls===3){
          const tool=body.tools.find(t=>t.function.description.startsWith("runtimeInfo\n")); assert.ok(tool);
          if(calls===3)assert.ok(body.messages.some(m=>m.content?.includes("GATEWAY_TURN_1")));
          return Response.json({choices:[{finish_reason:"tool_calls",message:{content:null,tool_calls:[{id:`call-${calls}`,type:"function",function:{name:tool.function.name,arguments:"{}"}}]}}]});
        }
        assert.ok(body.messages.some(m=>m.role==="tool"&&m.content.includes("gateway-fixture")));
        return Response.json({choices:[{finish_reason:"stop",message:{content:`GATEWAY_TURN_${calls/2}`}}]});
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

test("routed children use their own provider and reuse the pin on continuation", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  let rootCalls = 0, childCalls = 0, choices = 0;
  const routes = new Map();
  const childAi = { async run(model, input) {
    childCalls++;
    assert.ok(childCalls <= 4, "bounded child model requests");
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.reasoning_effort, "high");
    assert.equal(routes.size, 1, "child route is saved before inference");
    if (input.messages.at(-1)?.role === "tool") {
      assert.deepEqual(JSON.parse(input.messages.at(-1).content), { accepted: true, decoded_json_text: true });
      return { choices: [{ finish_reason: "stop", message: { content: "CHILD_DONE" } }] };
    }
    const submit = input.tools.find(t => t.function.description.startsWith("submit_result\n"));
    assert.ok(submit);
    const tokens = [...JSON.stringify(input.messages).matchAll(/turn_token: (\d+)/g)];
    assert.ok(tokens.length);
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id: `submit-${childCalls}`, type: "function", function: { name: submit.function.name,
        arguments: JSON.stringify({ turn_token: Number(tokens.at(-1)[1]), output: JSON.stringify({ ok: Number(tokens.at(-1)[1]) }) }) },
    }] } }] };
  } };
  const gateway = { provider: "openrouter", model: "gpt-5.6-sol", reasoningEffort: "low", apiKey: "synthetic-test-key",
    async fetch(_url, init) {
      rootCalls++;
      const body = JSON.parse(init.body);
      assert.equal(body.model, "openai/gpt-5.6-sol");
      assert.equal(body.reasoning.effort, "low");
      return Response.json({ choices: [{ finish_reason: "stop", message: { content: "ROOT_PIN_OK" } }] });
    },
  };
  const agent = await create(module, durableOwner(storage), {
    [Symbol.for("nanocodex.cloudflare.internalConfiguration")]: { model: gateway.model, thinking: "low", reasoning_mode: "standard", fast_mode: false },
    [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
      gateway, toolMode: "direct", subagentsEnabled: true,
      subagentRouting: {
        async resolve(request) {
          choices++;
          assert.equal(request.parentSessionId, storage.sessionId);
          return { model: "@cf/zai-org/glm-5.3", thinking: "high", routeId: "child-choice" };
        },
        bind(request) {
          assert.equal(request.routeId, "child-choice");
          routes.set(request.sessionId, { model: "@cf/zai-org/glm-5.3", thinking: "high",
            workersAi: { ai: childAi, model: "@cf/zai-org/glm-5.3", thinking: "high" } });
        },
      },
      inferenceForSession(id) {
        return id === storage.sessionId ? { model: gateway.model, thinking: "low", gateway } : routes.get(id);
      },
    },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "Respond briefly." }).result()).finalMessage, "ROOT_PIN_OK");
    const child = await Subagents.spawn(agent, { role: "test-child", task: "Return an object.", outputSchema: { type: "object", properties: { ok: { type: "integer" } }, required: ["ok"], additionalProperties: false } });
    const first = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(first.agents[0].status, { state: "completed", output: { ok: 1 } });
    await Subagents.send(agent, { agentId: child.agent_id, message: "Return another object." });
    const second = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(second.agents[0].status, { state: "completed", output: { ok: 2 } });
    assert.equal(choices, 1, "continuing a child does not reroute");
    assert.equal(childCalls, 4);
    assert.equal((await agent.turn.prompt({ input: "Still the root." }).result()).finalMessage, "ROOT_PIN_OK");
    assert.equal(rootCalls, 2);
  } finally { await agent.session.shutdown(); }
});

test("completed object children resume history and pinned routing after owner shutdown", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const routes = new Map();
  const lifecycleEvents = [];
  const requestedTokens = [];
  const childRequests = [];
  const marker = "CHILD_OBJECT_HISTORY_BEFORE_RESTART";
  let classifierCalls = 0;
  let acceptedReceipts = 0;
  let rejectedReceipts = 0;
  let invalidSecondResultSent = false;
  let childSessionId;
  const childAi = { async run(model, input) {
    childRequests.push(input);
    assert.ok(childRequests.length <= 5, "bounded child requests across restart");
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.reasoning_effort, "high");
    assert.equal(routes.size, 1, "the child route remains retained until explicit close");
    const tokens = [...JSON.stringify(input.messages).matchAll(/turn_token: (\d+)/g)];
    assert.ok(tokens.length, "child requests contain the current turn token");
    const token = Number(tokens.at(-1)[1]);
    const last = input.messages.at(-1);
    if (last?.role === "tool") {
      if (!last.content.includes("submitted output does not match the required schema")) {
        assert.deepEqual(JSON.parse(last.content), { accepted: true, decoded_json_text: true });
        acceptedReceipts++;
        return { choices: [{ finish_reason: "stop", message: { content: `CHILD_DONE_${token}` } }] };
      }
      assert.equal(token, 2);
      assert.match(last.content, /submitted output does not match the required schema/,
        "the restored exact schema rejects extra properties");
      rejectedReceipts++;
    }
    if (token === 2) {
      const history = JSON.stringify(input.messages);
      assert.ok(history.includes(marker), "the child retains its first object result in provider history");
      assert.ok(history.includes("CHILD_DONE_1"), "the first assistant turn survives reconstruction");
    }
    requestedTokens.push(token);
    const submit = input.tools.find((tool) => tool.function.description.startsWith("submit_result\n"));
    assert.ok(submit);
    const output = { ok: token, marker: token === 1 ? marker : "AFTER_RESTART" };
    if (token === 2 && !invalidSecondResultSent) {
      output.unexpected = "reject this extra property";
      invalidSecondResultSent = true;
    }
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id: `restart-submit-${childRequests.length}`, type: "function", function: {
        name: submit.function.name,
        arguments: JSON.stringify({ turn_token: token, output: JSON.stringify(output) }),
      },
    }] } }] };
  } };
  const gateway = {
    provider: "openrouter", model: "gpt-5.6-sol", reasoningEffort: "low", apiKey: "synthetic-test-key",
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
        assert.equal(id, childSessionId, "reconstruction retains the child session identity");
        return routes.get(id);
      },
    },
  };
  let agent = await create(module, durableOwner(storage), options);
  try {
    const child = await Subagents.spawn(agent, {
      role: "restart-object-child", task: "Return an object with ok equal to the turn token and a history marker.",
      outputSchema: {
        type: "object", properties: { ok: { type: "integer" }, marker: { type: "string" } },
        required: ["ok", "marker"], additionalProperties: false,
      },
    });
    const first = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(first.agents[0].status, { state: "completed", output: { ok: 1, marker } });
    const retainedDescriptor = storage.subagents.get(childSessionId).descriptorJson;
    assert.equal(JSON.parse(retainedDescriptor).agentId, String(child.agent_id));
    assert.equal(classifierCalls, 1);
    assert.equal(acceptedReceipts, 1);

    await agent.session.shutdown();
    assert.equal(storage.subagents.get(childSessionId)?.descriptorJson, retainedDescriptor);
    assert.equal(routes.size, 1, "owner shutdown retains the child's provider pin");
    assert.equal(lifecycleEvents.filter(({ type }) => type === "release").length, 0);
    assert.ok(storage.subagentCheckpoints.size > 0, "shutdown persists child runtime checkpoints");
    const validChunks = new Map(storage.subagentCheckpoints);
    const savedCheckpoint = [...validChunks.values()].join("");
    await assert.rejects(create(module, durableOwner(storage, {
      async fetch() { return { status: 403, headers: new Headers() }; },
    }), {
      tools: options.tools,
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
        subagentsEnabled: true,
        subagentLifecycle: options[Symbol.for("nanocodex.cloudflare.internalRuntime")].subagentLifecycle,
      },
    }), /EGRESS broker rejected.*HTTP 403/);
    assert.deepEqual(storage.subagentCheckpoints, validChunks,
      "startup failure after successful child restoration preserves the reusable boundary");
    assert.equal(childRequests.length, 2, "failed owner creation never replays child inference");
    lifecycleEvents.length = 0;
    for (const corruptIdentity of [
      (checkpoint) => { checkpoint.root_session_id = "different-root-session"; },
      (checkpoint) => { checkpoint.children[0].descriptor.id = child.agent_id + 100; },
      (checkpoint) => { checkpoint.children[0].descriptor.session_id = "different-child-session"; },
    ]) {
      const invalidCheckpoint = JSON.parse(savedCheckpoint);
      corruptIdentity(invalidCheckpoint);
      const encoded = JSON.stringify(invalidCheckpoint);
      storage.subagentCheckpoints.clear();
      for (let offset = 0, index = 0; offset < encoded.length; offset += 65_536, index++) {
        storage.subagentCheckpoints.set(index, encoded.slice(offset, offset + 65_536));
      }
      const corruptedChunks = new Map(storage.subagentCheckpoints);
      await assert.rejects(create(module, durableOwner(storage), options),
        /Durable child checkpoint (does not match its session bindings|identity differs from its session binding)/);
      assert.equal(storage.subagents.get(childSessionId)?.descriptorJson, retainedDescriptor,
        "failed identity validation preserves the child's durable binding");
      assert.deepEqual(storage.subagentCheckpoints, corruptedChunks,
        "failed reconstruction does not replace or erase checkpoint evidence");
      assert.equal(routes.size, 1, "failed identity validation preserves the child's route");
      assert.equal(lifecycleEvents.filter(({ type }) => type === "release").length, 0);
      assert.equal(classifierCalls, 1);
      assert.equal(childRequests.length, 2);
    }
    storage.subagentCheckpoints = validChunks;

    agent = await create(module, durableOwner(storage), options);
    const restored = await Subagents.list(agent, { includeCompleted: true });
    assert.deepEqual(restored.agents.find(({ agent_id }) => agent_id === child.agent_id)?.status,
      { state: "completed", output: { ok: 1, marker } });
    assert.equal(classifierCalls, 1, "reconstruction does not rerun classification");
    assert.deepEqual(lifecycleEvents.filter(({ type }) => type === "reconstruct").map(({ sessionId }) => sessionId), [childSessionId]);
    assert.equal(childRequests.length, 2, "reconstruction does not replay completed inference");
    assert.equal(storage.subagents.get(childSessionId)?.descriptorJson, retainedDescriptor);

    await Subagents.send(agent, { agentId: child.agent_id, purpose: "delegate", message: "Return another object using the new turn token." });
    const second = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(second.agents[0].status,
      { state: "completed", output: { ok: 2, marker: "AFTER_RESTART" } });
    assert.deepEqual(requestedTokens, [1, 2, 2]);
    assert.equal(acceptedReceipts, 2);
    assert.equal(rejectedReceipts, 1);
    assert.equal(classifierCalls, 1, "the resumed child uses its original classifier choice");
    assert.equal(childRequests.length, 5);

    assert.equal(storage.subagents.get(childSessionId)?.descriptorJson, retainedDescriptor,
      "delegation keeps its immutable spawning authorization descriptor");
    const delegatedContext = JSON.parse(await globalThis.nanocodexHost.executeTool("inspectAuthorization", "{}", childSessionId, "delegated-authority"));
    assert.equal(delegatedContext.structured_result.task, JSON.parse(retainedDescriptor).task,
      "delegated tools retain the original authorization descriptor");
    await agent.session.shutdown();
    agent = await create(module, durableOwner(storage), options);
    const delegated = await Subagents.list(agent, { includeCompleted: true });
    assert.equal(delegated.agents.find(({ agent_id }) => agent_id === child.agent_id)?.task,
      "Return another object using the new turn token.", "mutable delegated task survives a second restart");
    const restoredContext = JSON.parse(await globalThis.nanocodexHost.executeTool("inspectAuthorization", "{}", childSessionId, "restored-delegated-authority"));
    assert.equal(restoredContext.structured_result.task, JSON.parse(retainedDescriptor).task);
    assert.equal(storage.subagents.get(childSessionId)?.descriptorJson, retainedDescriptor);
    assert.equal(classifierCalls, 1);
    await Subagents.close(agent, child.agent_id);
    assert.equal(storage.subagents.size, 0, "explicit close releases the child descriptor");
    assert.equal(routes.size, 0, "explicit close releases the child route");
    assert.equal(storage.subagentCheckpoints.size, 0, "explicit close invalidates the child's retained checkpoint");
    assert.deepEqual(lifecycleEvents.filter(({ type }) => type === "release").map(({ sessionId }) => sessionId), [childSessionId]);
    await agent.session.shutdown();
    if (storage.subagentCheckpoints.size > 0) {
      const checkpoint = JSON.parse([...storage.subagentCheckpoints.values()].join(""));
      assert.deepEqual(checkpoint.children, [], "closed child checkpoints cannot be resurrected");
    }
    agent = await create(module, durableOwner(storage), options);
    const afterClose = await Subagents.list(agent, { includeCompleted: true });
    assert.equal(afterClose.agents.some(({ agent_id }) => agent_id === child.agent_id), false);
    assert.equal(routes.size, 0);
    assert.equal(classifierCalls, 1);
    assert.equal(childRequests.length, 5);
  } finally {
    await agent.session.shutdown();
  }
});

test("Cloudflare child checkpoints reject malformed chunks and stale owner writes", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const captured = [];
  const bound = bindAgent(module, {
    async create(options) {
      captured.push(options[Symbol.for("nanocodex.browser.internalRuntime")].subagentSessions);
      return HostAgent.create(options);
    },
  });
  const first = await bound.create(durableOwner(storage));
  const sessions = captured[0];
  try {
    const checkpoint = JSON.stringify({ marker: "x".repeat(65_524) + "😀" + "y".repeat(80_000) });
    sessions.checkpoint(checkpoint);
    assert.ok(storage.subagentCheckpoints.size > 1, "large checkpoints are stored in bounded chunks");
    assert.equal(sessions.restoreCheckpoint(), checkpoint, "chunk round trips preserve Unicode exactly");
    const retained = new Map(storage.subagentCheckpoints);
    for (const invalid of ["", "not-json", "x".repeat(16 * 1024 * 1024 + 1)]) {
      assert.throws(() => sessions.checkpoint(invalid));
      assert.deepEqual(storage.subagentCheckpoints, retained, "invalid writes preserve the last complete checkpoint");
    }
    storage.subagentCheckpoints.delete(0);
    assert.throws(() => sessions.restoreCheckpoint(), /Invalid durable subagent checkpoint chunks/);
    storage.subagentCheckpoints = new Map([[0, "x".repeat(65_537)]]);
    assert.throws(() => sessions.restoreCheckpoint(), /Invalid durable subagent checkpoint chunks/);
    storage.subagentCheckpoints = new Map([[0, null]]);
    assert.throws(() => sessions.restoreCheckpoint(), /Invalid durable subagent checkpoint chunks/);
    storage.subagentCheckpoints.clear();
  } finally {
    await first.session.shutdown();
  }
  const replacement = await bound.create(durableOwner(storage));
  try {
    const retained = new Map(storage.subagentCheckpoints);
    assert.throws(() => sessions.checkpoint("{}"), /no longer owns the session/);
    assert.throws(() => sessions.consumeCheckpoint(), /no longer owns the session/);
    assert.deepEqual(storage.subagentCheckpoints, retained, "a stale owner cannot overwrite the replacement checkpoint");
  } finally {
    await replacement.session.shutdown();
  }
});

test("closing one child before owner shutdown preserves a resumable sibling", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const routes = new Map();
  const sessionsByAgent = new Map();
  let classifierCalls = 0;
  let modelCalls = 0;
  const ai = { async run(model, input) {
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.reasoning_effort, "high");
    modelCalls++;
    assert.ok(modelCalls <= 6, "bounded sibling model requests");
    if (input.messages.at(-1)?.role === "tool") {
      assert.deepEqual(JSON.parse(input.messages.at(-1).content), { accepted: true });
      return { choices: [{ finish_reason: "stop", message: { content: "SIBLING_DONE" } }] };
    }
    const tokens = [...JSON.stringify(input.messages).matchAll(/turn_token: (\d+)/g)];
    const token = Number(tokens.at(-1)[1]);
    const submit = input.tools.find((tool) => tool.function.description.startsWith("submit_result\n"));
    assert.ok(submit);
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id: `sibling-submit-${modelCalls}`, type: "function", function: {
        name: submit.function.name, arguments: JSON.stringify({ turn_token: token, output: { turn: token } }),
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
        role, task: "Return an object with the current turn token.",
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
    await agent.session.shutdown();
    const unloadedCheckpoint = new Map(storage.subagentCheckpoints);
    agent = await create(module, durableOwner(storage), options);
    // Model a clean persisted boundary for the takeover/rollback assertions.
    // Successful reconstruction consumes it before any new child work can run.
    storage.subagentCheckpoints = unloadedCheckpoint;
    const retainedBindings = new Map(storage.subagents);
    const retainedChunks = new Map(storage.subagentCheckpoints);
    const bridge = globalThis.nanocodexHost;
    let reconstructedBinds = 0;
    const secondSession = sessionsByAgent.get(String(retained.agent_id));
    await assert.rejects(create(module, durableOwner(storage), {
      ...options, tools: identityTool("failed-replacement"),
      [Symbol.for("nanocodex.cloudflare.internalRuntime")]: {
        ...options[Symbol.for("nanocodex.cloudflare.internalRuntime")],
        subagentLifecycle(event) {
          if (event.type === "reconstruct") {
            reconstructedBinds++;
            if (event.sessionId === secondSession) throw new Error("second child reconstruction bind failed");
          }
          options[Symbol.for("nanocodex.cloudflare.internalRuntime")].subagentLifecycle(event);
        },
      },
    }), /second child reconstruction bind failed/);
    assert.ok(reconstructedBinds >= 2, "failure occurs after one child host registration succeeds");
    assert.deepEqual(storage.subagents, retainedBindings, "partial restore failure retains durable child bindings");
    assert.deepEqual(storage.subagentCheckpoints, retainedChunks, "partial restore failure retains the checkpoint");
    assert.equal(routes.size, 2);
    for (const child of children) {
      const sessionId = sessionsByAgent.get(String(child.agent_id));
      const routed = JSON.parse(await bridge.executeTool("identity", "{}", sessionId, "after-partial-restore-failure"));
      assert.equal(routed.structured_result.source, "predecessor", "rollback restores the predecessor child host");
      assert.equal(routed.structured_result.subagent.agentId, String(child.agent_id));
    }
    const predecessor = agent;
    agent = await create(module, durableOwner(storage), { ...options, tools: identityTool("retry") });
    await predecessor.session.shutdown();
    for (const child of children) {
      const sessionId = sessionsByAgent.get(String(child.agent_id));
      const routed = JSON.parse(await bridge.executeTool("identity", "{}", sessionId, "after-reconstruction-retry"));
      assert.equal(routed.structured_result.source, "retry", "retry replaces every child host registration");
      assert.equal(routed.structured_result.subagent.agentId, String(child.agent_id));
    }
    assert.equal(classifierCalls, 2);
    assert.equal(modelCalls, 4, "failed reconstruction and retry never replay child inference");
    await Subagents.close(agent, closed.agent_id);
    assert.equal(storage.subagents.size, 1);
    assert.deepEqual([...routes.keys()], [retainedSession]);
    await agent.session.shutdown();
    const checkpoint = JSON.parse([...storage.subagentCheckpoints.values()].join(""));
    assert.deepEqual(checkpoint.children.map(({ descriptor }) => String(descriptor.id)), [String(retained.agent_id)]);
    assert.equal(checkpoint.children[0].descriptor.session_id, retainedSession);
    const originalChild = JSON.parse([...retainedChunks.values()].join("")).children.find((child) => child.descriptor.session_id === retainedSession);
    assert.deepEqual(checkpoint.children[0].runtime.conversation, originalChild.runtime.conversation,
      "reconstruction without a child turn must preserve its exact conversation checkpoint");

    agent = await create(module, durableOwner(storage), options);
    const listed = await Subagents.list(agent, { includeCompleted: true });
    assert.equal(listed.agents.some(({ agent_id }) => agent_id === closed.agent_id), false);
    assert.deepEqual(listed.agents.find(({ agent_id }) => agent_id === retained.agent_id)?.status,
      { state: "completed", output: { turn: 1 } });
    await Subagents.send(agent, { agentId: retained.agent_id, message: "Return the new turn token." });
    const resumed = await Subagents.wait(agent, { agentIds: [retained.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(resumed.agents[0].status, { state: "completed", output: { turn: 2 } });
    assert.equal(classifierCalls, 2, "only the two initial spawns classify routes");
    assert.equal(modelCalls, 6);
    assert.deepEqual([...routes.keys()], [retainedSession]);
    await Subagents.close(agent, retained.agent_id);
    assert.equal(storage.subagents.size, 0);
    assert.equal(routes.size, 0);
  } finally {
    await agent.session.shutdown();
  }
});


test("Cloudflare SDK sibling shutdown cannot overwrite the root checkpoint", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const agent = await create(module, durableOwner(storage), { tools: {
    identity: { parameters: { type: "object" }, handler: () => ({ source: "independent-sibling" }) },
  } });
  const first = await agent.session.spawn();
  const retained = await agent.session.spawn();
  try {
    await first.session.shutdown();
    assert.equal(storage.subagentCheckpoints.size, 0, "SDK sibling cannot write the root checkpoint");
    const result = JSON.parse(await globalThis.nanocodexHost.executeTool("identity", "{}", retained.sessionId, "sibling-with-live-root"));
    assert.equal(result.structured_result.source, "independent-sibling");
    await agent.session.shutdown();
    const checkpoint = new Map(storage.subagentCheckpoints);
    assert.equal(JSON.parse([...checkpoint.values()].join("")).root_session_id, agent.sessionId);
    assert.throws(() => globalThis.nanocodexHost.executeTool("identity", "{}", retained.sessionId, "released-root"), /no Nanocodex host is active/);
    await retained.session.shutdown();
    assert.deepEqual(storage.subagentCheckpoints, checkpoint, "SDK sibling cannot overwrite the root checkpoint");
  } finally {
    await first.session.shutdown();
    await retained.session.shutdown();
    await agent.session.shutdown();
  }
});


test("Cloudflare consumes a restored checkpoint before exposing a mutable owner", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const first = await create(module, durableOwner(storage));
  await first.session.shutdown();
  assert.ok(storage.subagentCheckpoints.size > 0);
  const second = await create(module, durableOwner(storage));
  try {
    assert.equal(storage.subagentCheckpoints.size, 0,
      "a restored snapshot must not survive subsequent child mutations or abrupt eviction");
  } finally {
    await second.session.shutdown();
  }
  assert.ok(storage.subagentCheckpoints.size > 0, "a clean unload writes a fresh snapshot");
});


test("Cloudflare recovers a legacy checkpoint older than the retained child bindings", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const first = await create(module, durableOwner(storage));
  await first.session.shutdown();
  const previous = { agentId: "1", parentAgentId: null, sessionId: "legacy-previous-child", role: "research", task: "Previous work" };
  const checkpoint = JSON.parse([...storage.subagentCheckpoints.values()].join(""));
  checkpoint.next_agent_id = 2;
  checkpoint.children.push({
    descriptor: { id: 1, parent: null, session_id: previous.sessionId, role: previous.role, task: previous.task },
    runtime: null, output_schema: true, next_turn_token: 0, status: { state: "interrupted" },
    last_output: null, host_context: "synthetic-host-context",
  });
  const savedCheckpoint = new Map([[0, JSON.stringify(checkpoint)]]);
  storage.subagents.set(previous.sessionId, {
    agentId: previous.agentId, descriptorJson: JSON.stringify(previous), hostContextRef: "synthetic-host-context",
  });
  const descriptor = { agentId: "2", parentAgentId: null, sessionId: "legacy-extra-child", role: "research", task: "Retained work" };
  storage.subagents.set(descriptor.sessionId, {
    agentId: descriptor.agentId, descriptorJson: JSON.stringify(descriptor), hostContextRef: null,
  });
  const corrupt = JSON.parse([...savedCheckpoint.values()].join(""));
  corrupt.next_agent_id = 0;
  storage.subagentCheckpoints = new Map([[0, JSON.stringify(corrupt)]]);
  await assert.rejects(create(module, durableOwner(storage)), /agent.*id|allocator|checkpoint/i,
    "legacy recovery must still validate the complete Rust checkpoint schema");
  assert.equal(storage.subagents.size, 2);
  corrupt.next_agent_id = 2;
  corrupt.children[0].host_context = "different-host-context";
  storage.subagentCheckpoints = new Map([[0, JSON.stringify(corrupt)]]);
  await assert.rejects(create(module, durableOwner(storage)), /host context differs/);
  storage.subagentCheckpoints = savedCheckpoint;
  let replacement = await create(module, durableOwner(storage));
  try {
    assert.equal(replacement.sessionId, first.sessionId);
    assert.equal(storage.subagentCheckpoints.size, 0);
    let listed = await Subagents.list(replacement, { includeCompleted: true });
    assert.deepEqual(listed.agents.map(({ agent_id }) => agent_id).sort(), [1, 2]);
    assert.equal(listed.agents[0].status.state, "interrupted");
    assert.equal(listed.agents[0].can_message, false, "stale child work is never replayed");
    await replacement.session.shutdown();
    replacement = await create(module, durableOwner(storage));
    listed = await Subagents.list(replacement, { includeCompleted: true });
    assert.equal(listed.agents[0].status.state, "interrupted");
    assert.equal(listed.agents[0].can_message, false, "archived recovery survives another unload");
    assert.equal(storage.subagents.size, 2);
  } finally { await replacement.session.shutdown(); }
});


test("Cloudflare checkpoint consumption failure preserves the saved boundary", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const storage = new MemoryStorage();
  const first = await create(module, durableOwner(storage));
  await first.session.shutdown();
  const retained = new Map(storage.subagentCheckpoints);
  storage.failCheckpointDeletion = true;
  await assert.rejects(create(module, durableOwner(storage)), /checkpoint deletion failed/);
  assert.deepEqual(storage.subagentCheckpoints, retained);
  storage.failCheckpointDeletion = false;
  const replacement = await create(module, durableOwner(storage));
  assert.equal(storage.subagentCheckpoints.size, 0);
  await replacement.session.shutdown();
});
