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
    this.chunks = [];
    this.chunkHeads = new Map();
    this.events = [];
    this.stateRevisions = new Map();
    this.owners = new Map();
    this.subagents = new Map();
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
    } else if (statement.startsWith(
      "SELECT revision, chunk_count FROM nanocodex_durable_chunk_heads",
    )) {
      const head = this.chunkHeads.get(args[0]);
      rows = head ? [head] : [];
    } else if (statement.startsWith(
      "SELECT revision, chunk_index, payload FROM nanocodex_durable_state_chunks",
    )) {
      rows = this.chunks
        .filter((chunk) => chunk.stateId === args[0])
        .map((chunk) => ({
          revision: chunk.revision,
          chunk_index: chunk.chunkIndex,
          payload: chunk.payload,
        }));
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
    return { rowsWritten, toArray: () => rows };
  }
}

function durabilityPragmaRows(sql, subagentHostContextColumn = true) {
  let shapes;
  if (sql.includes("nanocodex_cloudflare_subagents")) {
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
      format: 2,
      operations: {},
      latest_checkpoint: null,
    },
  });
  assert.deepEqual(store.replace(stateId, {
    ownerId: ownership.ownerId,
    fence: ownership.fence,
    expectedRevision: ownership.revision,
    payload,
  }), { status: "replaced", revision: "1" });

  const archive = await exportDurabilityState(sourceOwner);
  assert.deepEqual(archive, {
    format: "nanocodex-durability-state-v1",
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
      format: "nanocodex-durability-state-v1",
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
    format: "nanocodex-durability-state-v1",
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
      accepted_order: index * 2 + 1,
    },
  ]));
  storage.states.push({
    stateId,
    revision: "20",
    payload: JSON.stringify({
      nanocodex_durable_state: {
        format: 2,
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
  assert.equal(checkpoint.format, 2);
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
        format: 2,
        operations: {
          "turn-compaction-race": {
            input: JSON.stringify("prompt"),
            status: "pending",
            steps: {},
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
  storage.chunks.push({ stateId, revision: "1", chunkIndex: 0, payload: "retained" });
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
  assert.equal(storage.chunks.length, 0);
  assert.equal(storage.stateRevisions.size, 0);
  assert.equal(storage.events.length, 0);
  assert.equal(storage.subagents.size, 0);
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
