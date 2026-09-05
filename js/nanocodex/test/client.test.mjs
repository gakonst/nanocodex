import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions } from "../index.mjs";
import {
  createMemoryDurabilityStore,
  createSqliteDurabilityStore,
  durabilityRevision,
  sqliteDurabilitySchema,
} from "nanocodex/durability";
import {
  activateHost,
  bindHostSession,
  createAgentClient,
  defineRuntime,
  parseSubagentAgentId,
  releaseHostSession,
  toWasmConfig,
} from "../internal.mjs";
import {
  own as ownDurabilityHost,
  release as releaseDurabilityHost,
  retain as retainDurabilityHost,
} from "../runtime/durability.mjs";

test("subagent event agent IDs cross the WASM ABI as safe JavaScript numbers", () => {
  assert.equal(parseSubagentAgentId(undefined), undefined);
  assert.equal(parseSubagentAgentId("1"), 1);
  for (const invalid of [0, "0", "01", "-1", "1.0", "x", "9007199254740992"]) {
    assert.throws(() => parseSubagentAgentId(invalid), /subagent agent ID/);
  }
});

test("the memory durability store replaces one complete opaque state", () => {
  const store = createMemoryDurabilityStore("state-1");
  assert.deepEqual(store.load("state-1"), { revision: "0", payload: null });
  assert.deepEqual(store.acquire("state-1", { ownerId: "owner-1" }), {
    ownerId: "owner-1",
    fence: "1",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(store.replace("state-1", {
    ownerId: "owner-1",
    fence: "1",
    expectedRevision: "0",
    payload: "{\"entry\":1}",
  }), { status: "replaced", revision: "1" });
  assert.deepEqual(store.acquire("state-1", { ownerId: "owner-2" }), {
    ownerId: "owner-2",
    fence: "2",
    revision: "1",
    payload: "{\"entry\":1}",
  });
  assert.deepEqual(store.replace("state-1", {
    ownerId: "owner-1",
    fence: "1",
    expectedRevision: "0",
    payload: "stale",
  }), { status: "fenced" });
  assert.deepEqual(store.replace("state-1", {
    ownerId: "owner-2",
    fence: "2",
    expectedRevision: "0",
    payload: "conflicting",
  }), { status: "conflict", actualRevision: "1" });
  assert.deepEqual(store.replace("state-1", {
    ownerId: "owner-2",
    fence: "2",
    expectedRevision: "1",
    payload: "{\"nanocodex_durable_state\":{}}",
  }), { status: "replaced", revision: "2" });
  assert.deepEqual(store.snapshot(), {
    revision: "2",
    payload: "{\"nanocodex_durable_state\":{}}",
  });
  assert.throws(() => store.load("other"), /unknown durability state/);
  assert.deepEqual(store.acquire("child", { ownerId: "child-owner" }), {
    ownerId: "child-owner",
    fence: "1",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(store.replace("child", {
    ownerId: "child-owner",
    fence: "1",
    expectedRevision: "0",
    payload: "child-state",
  }), { status: "replaced", revision: "1" });
  assert.deepEqual(store.load("child"), { revision: "1", payload: "child-state" });
  assert.equal(store.snapshot().revision, "2");
  assert.throws(
    () => store.acquire("invalid-child", { ownerId: "" }),
    /owner ID must be a non-empty string/,
  );
  assert.throws(() => store.load("invalid-child"), /unknown durability state/);
  assert.throws(
    () => store.importState("invalid-import", { revision: "1", payload: null }),
    /payload must be null exactly at revision zero/,
  );
  assert.throws(() => store.load("invalid-import"), /unknown durability state/);
  assert.throws(
    () => store.importState(
      "conflicting-import",
      { revision: "1", payload: "imported" },
      { expectedRevision: "1" },
    ),
    /expected destination revision 1.*but found 0/,
  );
  assert.throws(() => store.load("conflicting-import"), /unknown durability state/);
  assert.equal(durabilityRevision("18446744073709551615"), "18446744073709551615");
  assert.throws(
    () => durabilityRevision("18446744073709551616"),
    /unsigned 64-bit decimal string/,
  );
  assert.equal(durabilityRevision(Number.MAX_SAFE_INTEGER), "9007199254740991");
  assert.throws(
    () => durabilityRevision(-1),
    /revision numbers must be nonnegative safe integers/,
  );
  assert.throws(
    () => durabilityRevision(1.5),
    /revision numbers must be nonnegative safe integers/,
  );
  const exhausted = createMemoryDurabilityStore("exhausted", {
    revision: "18446744073709551615",
    payload: "retained",
  });
  const exhaustedOwner = exhausted.acquire("exhausted", { ownerId: "owner" });
  assert.deepEqual(exhausted.replace("exhausted", {
    ownerId: exhaustedOwner.ownerId,
    fence: exhaustedOwner.fence,
    expectedRevision: "18446744073709551615",
    payload: "never-written",
  }), {
    status: "not_committed",
    message: "in-memory durability revision overflow",
  });
  assert.equal(exhausted.snapshot().revision, "18446744073709551615");
});

test("the SQLite durability store owns revision validation and compare-and-replace", () => {
  const owners = new Map();
  const states = new Map();
  const query = (sql, args) => {
    const [stateId, revision, payload] = args;
    if (sql.startsWith("SELECT owner_id, fence FROM nanocodex_durable_owners")) {
      const stored = owners.get(stateId);
      return stored === undefined ? [] : [stored];
    }
    if (sql.startsWith("INSERT INTO nanocodex_durable_owners")) {
      const [, ownerId, fence] = args;
      owners.set(stateId, { owner_id: ownerId, fence });
      return [];
    }
    if (sql.startsWith("SELECT revision FROM nanocodex_durable_states")) {
      const stored = states.get(stateId);
      return stored === undefined ? [] : [{ revision: stored.revision }];
    }
    if (sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) {
      const stored = states.get(stateId);
      return stored === undefined ? [] : [stored];
    }
    if (sql.startsWith("INSERT INTO nanocodex_durable_states")) {
      states.set(stateId, { revision, payload });
      return [];
    }
    throw new Error(`unexpected SQL: ${sql}`);
  };
  const store = createSqliteDurabilityStore({
    transaction: (callback) => callback(query),
  });
  assert.equal(sqliteDurabilitySchema.length, 2);

  assert.deepEqual(store.load("state-1"), { revision: "0", payload: null });
  const firstOwner = store.acquire("state-1", { ownerId: "owner-1" });
  assert.deepEqual(firstOwner, {
    ownerId: "owner-1",
    fence: "1",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(store.replace("state-1", {
    ownerId: firstOwner.ownerId,
    fence: firstOwner.fence,
    expectedRevision: "0",
    payload: "opaque",
  }), { status: "replaced", revision: "1" });
  assert.deepEqual(store.replace("state-1", {
    ownerId: firstOwner.ownerId,
    fence: firstOwner.fence,
    expectedRevision: "0",
    payload: "stale",
  }), { status: "conflict", actualRevision: "1" });
  assert.deepEqual(store.replace("state-1", {
    ownerId: firstOwner.ownerId,
    fence: firstOwner.fence,
    expectedRevision: "1",
    payload: "checkpoint",
  }), { status: "replaced", revision: "2" });
  assert.deepEqual(store.load("state-1"), {
    revision: "2",
    payload: "checkpoint",
  });
  states.set("exhausted", { revision: "18446744073709551615", payload: "retained" });
  const exhaustedOwner = store.acquire("exhausted", { ownerId: "owner-exhausted" });
  assert.deepEqual(store.replace("exhausted", {
    ownerId: exhaustedOwner.ownerId,
    fence: exhaustedOwner.fence,
    expectedRevision: "18446744073709551615",
    payload: "never-written",
  }), {
    status: "not_committed",
    message: "SQLite durability revision overflow",
  });
  assert.equal(states.get("exhausted").payload, "retained");
  assert.deepEqual(store.load("state-1"), {
    revision: "2",
    payload: "checkpoint",
  });
  states.delete("state-1");
  const secondOwner = store.acquire("state-1", { ownerId: "owner-2" });
  assert.deepEqual(secondOwner, {
    ownerId: "owner-2",
    fence: "2",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(store.replace("state-1", {
    ownerId: firstOwner.ownerId,
    fence: firstOwner.fence,
    expectedRevision: "0",
    payload: "stale-after-reset",
  }), { status: "fenced" });

  const roundedRevision = Number("9007199254740993");
  assert.equal(roundedRevision, 9007199254740992);
  states.set("unsafe-revision", { revision: roundedRevision, payload: "unsafe" });
  assert.throws(
    () => store.load("unsafe-revision"),
    /revision numbers must be nonnegative safe integers; use exact unsigned decimal text/,
  );

  states.set("exact-revision", { revision: "9007199254740993", payload: "exact" });
  assert.equal(store.load("exact-revision").revision, "9007199254740993");

  owners.set("unsafe-fence", { owner_id: "old-owner", fence: roundedRevision });
  assert.throws(
    () => store.acquire("unsafe-fence", { ownerId: "new-owner" }),
    /fence numbers must be nonnegative safe integers; use exact unsigned decimal text/,
  );
});

test("the headless client exposes matching direct and standalone actions", async () => {
  const events = new Set();
  const runtime = defineRuntime({
    create: () => rawAgent("session-1"),
    subscribe(listener) {
      events.add(listener);
      return () => events.delete(listener);
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  const firstTurn = agent.turn.prompt({ input: "first" });
  const first = await firstTurn.result();
  assert.equal(first.finalMessage, "session-1:first");
  assert.deepEqual(Object.getOwnPropertySymbols(agent), []);
  assert.deepEqual(Object.getOwnPropertySymbols(firstTurn), []);
  assert.deepEqual(Object.getOwnPropertySymbols(first), []);
  assert.equal(Object.isFrozen(first), true);
  const [usage, sameUsage] = await Promise.all([first.usage(), Actions.turn.getUsage(first)]);
  const [snapshot, sameSnapshot] = await Promise.all([
    first.snapshot(),
    Actions.turn.getSnapshot(first),
  ]);
  assert.equal(Object.isFrozen(usage), true);
  assert.equal(Object.isFrozen(snapshot), true);
  assert.strictEqual(sameUsage, usage);
  assert.strictEqual(sameSnapshot, snapshot);
  const secondTurn = Actions.turn.prompt(agent, { input: "second" });
  const second = await Actions.turn.getResult(secondTurn);
  assert.equal(second.finalMessage, "session-1:second");
  const durable = await agent.turn.prompt({ id: "request-7", input: "durable" }).result();
  assert.equal(durable.finalMessage, "session-1:request-7:durable");

  const seen = [];
  const watch = agent.events.watch();
  const unwatch = watch.onEvent((event) => seen.push(event.type));
  for (const listener of events) {
    listener({ type: "ignored", request_id: "another-session" });
    listener({ type: "accepted", request_id: "session-1" });
  }
  unwatch();
  watch.off();
  assert.deepEqual(seen, ["accepted"]);

  const iterable = Actions.events.watch(agent);
  const iterator = iterable[Symbol.asyncIterator]();
  const next = iterator.next();
  for (const listener of events) listener({ type: "streamed", request_id: "session-1" });
  assert.deepEqual(await next, {
    done: false,
    value: { type: "streamed", request_id: "session-1" },
  });
  await iterator.return();
  iterable.off();

  const branch = await agent.session.fork({ at: first });
  assert.equal(branch.sessionId, "session-1-fork");
  assert.equal(
    (await branch.turn.prompt({ input: "branch" }).result()).finalMessage,
    "session-1-fork:branch",
  );
  first.dispose();

  const fresh = await agent.session.spawn();
  assert.equal(fresh.sessionId, "session-1-spawn");

  await agent.session.compact();
  await Actions.session.compact(agent);
  assert.deepEqual(
    await agent.session.context(),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  assert.deepEqual(
    await Actions.session.context(agent),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  assert.deepEqual(
    await agent.session.appendDeveloperMessage("voice started"),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  assert.deepEqual(
    await Actions.session.appendDeveloperMessage(agent, "voice stopped"),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  await assert.rejects(agent.session.appendDeveloperMessage("  "), /non-empty string/);
  assert.deepEqual(
    await agent.session.realtime.start(),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  assert.deepEqual(
    await agent.session.realtime.end(),
    { workspace: "/workspace", history: [{ type: "message", role: "developer" }] },
  );
  assert.equal(
    await agent.session.realtime.delegation("ship", [{ role: "user", text: "now" }]),
    "delegated:ship:user: now",
  );
  assert.equal(
    await agent.session.realtime.tailDelegation([{ role: "assistant", text: "done" }]),
    "tail:assistant: done",
  );

  const extended = agent.extend((client) => ({ inspect: { session: () => client.sessionId } }));
  assert.equal(extended.inspect.session(), "session-1");
  branch.dispose();
  fresh.dispose();
  agent.dispose();
});

test("a duplicate stable session rejects before touching durability authority", async () => {
  const sessionId = "session-duplicate-reservation";
  let authorityAcquisitions = 0;
  let releases = 0;
  const runtime = defineRuntime({
    create(options) {
      authorityAcquisitions += 1;
      return rawAgent(options.sessionId);
    },
    release() {
      releases += 1;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const first = await createAgentClient(runtime, { sessionId, durabilityId: "state-1" });

  await assert.rejects(
    createAgentClient(runtime, { sessionId, durabilityId: "state-1" }),
    /session ID is already active/,
  );
  assert.equal(authorityAcquisitions, 1);
  assert.equal(
    (await first.turn.prompt({ input: "first owner remains live" }).result()).finalMessage,
    `${sessionId}:first owner remains live`,
  );

  first.dispose();
  assert.equal(releases, 1);
  const replacement = await createAgentClient(runtime, { sessionId, durabilityId: "state-1" });
  assert.equal(authorityAcquisitions, 2);
  replacement.dispose();
  assert.equal(releases, 2);
});

test("concurrent creation admits exactly one stable session owner", async () => {
  const sessionId = "session-concurrent-reservation";
  let creations = 0;
  let releaseFirst;
  const firstGate = new Promise((resolve) => { releaseFirst = resolve; });
  const runtime = defineRuntime({
    async create(options) {
      creations += 1;
      await firstGate;
      return rawAgent(options.sessionId);
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });

  const firstCreation = createAgentClient(runtime, { sessionId });
  const secondCreation = createAgentClient(runtime, { sessionId });
  await assert.rejects(secondCreation, /session ID is already active/);
  assert.equal(creations, 1);
  releaseFirst();
  const first = await firstCreation;
  first.dispose();
});

test("failed construction releases its stable session reservation", async () => {
  const sessionId = "session-construction-failure";
  let creations = 0;
  const runtime = defineRuntime({
    create(options) {
      creations += 1;
      if (creations === 1) throw new Error("WASM construction failed");
      return rawAgent(options.sessionId);
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });

  await assert.rejects(
    createAgentClient(runtime, { sessionId }),
    /WASM construction failed/,
  );
  const recovered = await createAgentClient(runtime, { sessionId });
  assert.equal(creations, 2);
  recovered.dispose();
});

test("turn acceptance forwards durable IDs and remains optional for custom runtimes", async () => {
  const legacyRuntime = defineRuntime({
    create: () => rawAgent("legacy-session"),
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const legacy = await createAgentClient(legacyRuntime);
  const legacyTurn = legacy.turn.prompt({ input: "legacy" });
  assert.equal(await legacyTurn.accepted(), undefined);
  legacyTurn.dispose();
  legacy.dispose();

  let acceptanceCalls = 0;
  const runtime = defineRuntime({
    create() {
      const raw = rawAgent("durable-session");
      const prompt = raw.prompt;
      raw.prompt = (input, id) => {
        const turn = prompt(input, id);
        turn.accepted = () => {
          acceptanceCalls += 1;
          return id;
        };
        return turn;
      };
      return raw;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);
  const turn = agent.turn.prompt({ input: "durable", id: "request-7" });
  const accepted = turn.accepted();
  assert.strictEqual(turn.accepted(), accepted);
  assert.strictEqual(Actions.turn.accepted(turn), accepted);
  assert.equal(await accepted, "request-7");
  assert.equal(acceptanceCalls, 1);
  turn.dispose();
  agent.dispose();
});

test("turn prompt forwards atomic cancellation through the WASM boundary", async () => {
  const calls = [];
  const raw = rawAgent("cancel-on-admission");
  const prompt = raw.prompt;
  raw.prompt = (input, id, cancelOnAdmission) => {
    calls.push({ input, id, cancelOnAdmission });
    return prompt(input, id);
  };
  const runtime = defineRuntime({
    create: () => raw,
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  await agent.turn.prompt({
    input: "cancel before work",
    id: "cancel-1",
    cancelOnAdmission: true,
  }).result();
  assert.deepEqual(calls, [{
    input: "cancel before work",
    id: "cancel-1",
    cancelOnAdmission: true,
  }]);
  agent.dispose();
});

test("the WASM config pairs a durability route with its state", () => {
  assert.deepEqual(toWasmConfig({
    apiKey: "test-key",
    hostDefinitionId: 1,
    durabilityId: "state-1",
    durabilityHostId: "durability-route-1",
    terminalReceiptRetention: 512,
  }), {
    api_key: "test-key",
    durability_id: "state-1",
    durability_host_id: "durability-route-1",
    terminal_receipt_retention: 512,
    host_definition_id: 1,
  });
});

test("the WASM config distinguishes prompt replacement from host additions", () => {
  assert.deepEqual(toWasmConfig({
    apiKey: "test-key",
    model: "gpt-6-astra",
    instructions: "caller replacement",
    additionalInstructions: "host additions",
  }), {
    api_key: "test-key",
    model: "gpt-6-astra",
    instructions: "caller replacement",
    additional_instructions: "host additions",
  });
});

test("the WASM host bridge routes owner-fenced durability per Agent binding", async () => {
  let state = { revision: "0", payload: null };
  let owner;
  const durability = {
    acquire(_stateId, { ownerId }) {
      const fence = String(BigInt(owner?.fence ?? "0") + 1n);
      owner = { ownerId, fence };
      return { ...owner, ...state };
    },
    replace(_stateId, { ownerId, fence, expectedRevision, payload }) {
      if (ownerId !== owner?.ownerId || fence !== owner?.fence) {
        return { status: "fenced" };
      }
      if (payload === "definite-failure") {
        return { status: "not_committed", message: "transaction rolled back" };
      }
      if (expectedRevision !== state.revision) {
        return { status: "conflict", actualRevision: state.revision };
      }
      const revision = String(BigInt(state.revision) + 1n);
      state = { revision, payload };
      return { status: "replaced", revision };
    },
  };
  const firstHost = { connect() {} };
  const secondHost = { connect() {} };
  activateHost(firstHost);
  const firstRoute = ownDurabilityHost(firstHost, durability, "state-1");
  const secondRoute = ownDurabilityHost(secondHost, durability, "state-1");
  assert.notEqual(firstRoute.id, secondRoute.id);
  retainDurabilityHost(firstHost, firstRoute.id);
  retainDurabilityHost(firstHost, firstRoute.id);
  retainDurabilityHost(secondHost, secondRoute.id);
  try {
    assert.deepEqual(
      (await globalThis.nanocodexHost.durabilityAcquire(
        firstRoute.id,
        "state-1",
        "owner-1",
      )),
      { owner_id: "owner-1", fence: "1", revision: "0", payload: null },
    );
    assert.deepEqual(
      JSON.parse(await globalThis.nanocodexHost.durabilityReplace(
        firstRoute.id,
        "state-1",
        "owner-1",
        "1",
        "0",
        "opaque-rust-state",
      )),
      { status: "replaced", revision: "1" },
    );
    assert.deepEqual(
      JSON.parse(await globalThis.nanocodexHost.durabilityReplace(
        firstRoute.id,
        "state-1",
        "owner-1",
        "1",
        "0",
        "stale",
      )),
      { status: "conflict", actual_revision: "1" },
    );
    assert.deepEqual(
      JSON.parse(await globalThis.nanocodexHost.durabilityReplace(
        firstRoute.id,
        "state-1",
        "owner-1",
        "1",
        "1",
        "definite-failure",
      )),
      { status: "not_committed", message: "transaction rolled back" },
    );
    assert.deepEqual(
      (await globalThis.nanocodexHost.durabilityAcquire(
        secondRoute.id,
        "state-1",
        "owner-2",
      )),
      {
        owner_id: "owner-2",
        fence: "2",
        revision: "1",
        payload: new TextEncoder().encode("opaque-rust-state"),
      },
    );
    assert.deepEqual(
      JSON.parse(await globalThis.nanocodexHost.durabilityReplace(
        firstRoute.id,
        "state-1",
        "owner-1",
        "1",
        "1",
        "stale-owner",
      )),
      { status: "fenced" },
    );
    assert.equal(
      (await globalThis.nanocodexHost.durabilityAcquire(
        firstRoute.id,
        "another-state",
        "owner-3",
      )).owner_id,
      "owner-3",
      "one private route must serve independently fenced spawned states",
    );
    releaseDurabilityHost(firstHost, firstRoute.id);
    assert.equal(
      (await globalThis.nanocodexHost.durabilityAcquire(
        firstRoute.id,
        "state-1",
        "owner-3",
      )).fence,
      "4",
      "releasing a child host reference must preserve its parent's state binding",
    );
    releaseDurabilityHost(firstHost, firstRoute.id);
    await assert.rejects(
      globalThis.nanocodexHost.durabilityAcquire(
        firstRoute.id,
        "state-1",
        "owner-4",
      ),
      /no Nanocodex host owns durability route/,
    );
    assert.equal(
      (await globalThis.nanocodexHost.durabilityAcquire(
        secondRoute.id,
        "state-1",
        "owner-5",
      )).fence,
      "5",
      "releasing one route must not release another route for the same state",
    );
  } finally {
    releaseDurabilityHost(firstHost, firstRoute.id);
    releaseDurabilityHost(secondHost, secondRoute.id);
  }
  await assert.rejects(
    globalThis.nanocodexHost.durabilityAcquire(firstRoute.id, "state-1", "owner-4"),
    /no Nanocodex host owns durability route/,
  );
});

test("concurrent graceful shutdown defers exactly-once release until the join completes", async () => {
  let shutdowns = 0;
  let releases = 0;
  let disposals = 0;
  let resolveShutdown;
  const shutdownGate = new Promise((resolve) => { resolveShutdown = resolve; });
  const subscriptions = new Set();
  const raw = rawAgent("session-shutdown");
  raw.shutdown = async () => {
    shutdowns += 1;
    await shutdownGate;
  };
  const runtime = defineRuntime({
    create: () => raw,
    subscribe(listener) {
      subscriptions.add(listener);
      return () => subscriptions.delete(listener);
    },
    release() {
      releases += 1;
    },
    dispose() {
      disposals += 1;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);
  const extended = agent.extend(() => ({ inspect: true }));
  const watcher = agent.events.watch();
  const pendingEvent = watcher[Symbol.asyncIterator]().next();

  const first = agent.session.shutdown();
  const second = Actions.session.shutdown(extended);
  const joined = Promise.all([first, second]);
  void joined.catch(() => {});
  agent.dispose();
  await Promise.resolve();

  assert.equal(shutdowns, 1);
  assert.equal(releases, 0);
  assert.equal(disposals, 0);
  assert.equal(subscriptions.size, 1);
  assert.throws(
    () => extended.turn.prompt({ input: "too late" }),
    /agent has been disposed/,
  );

  resolveShutdown();
  await joined;
  assert.deepEqual(await pendingEvent, { done: true, value: undefined });
  assert.equal(subscriptions.size, 0);
  assert.equal(releases, 1);
  assert.equal(disposals, 1);

  await agent.session.shutdown();
  agent.dispose();
  assert.equal(shutdowns, 1);
  assert.equal(releases, 1);
  assert.equal(disposals, 1);
});

test("a failing release hook still frees the raw agent exactly once", async () => {
  const releaseError = new Error("release failed");
  let shutdowns = 0;
  let releases = 0;
  let disposals = 0;
  const raw = rawAgent("session-release-failure");
  raw.shutdown = async () => {
    shutdowns += 1;
  };
  const runtime = defineRuntime({
    create: () => raw,
    release() {
      releases += 1;
      throw releaseError;
    },
    dispose() {
      disposals += 1;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  await assert.rejects(agent.session.shutdown(), releaseError);
  agent.dispose();

  assert.equal(shutdowns, 1);
  assert.equal(releases, 1);
  assert.equal(disposals, 1);
});

test("shutdown preserves driver and cleanup failures in causal order", async () => {
  const shutdownError = new Error("driver shutdown failed");
  const releaseError = new Error("release failed");
  const disposeError = new Error("dispose failed");
  const raw = rawAgent("session-multiple-shutdown-errors");
  raw.shutdown = async () => {
    throw shutdownError;
  };
  const runtime = defineRuntime({
    create: () => raw,
    release() {
      throw releaseError;
    },
    dispose() {
      throw disposeError;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  await assert.rejects(
    agent.session.shutdown(),
    (error) => {
      assert.equal(error instanceof AggregateError, true);
      assert.deepEqual(error.errors, [shutdownError, releaseError, disposeError]);
      return true;
    },
  );
});

test("a lone driver shutdown failure retains its exact identity", async () => {
  const shutdownError = new Error("driver shutdown failed");
  let releases = 0;
  let disposals = 0;
  const raw = rawAgent("session-driver-shutdown-error");
  raw.shutdown = async () => {
    throw shutdownError;
  };
  const runtime = defineRuntime({
    create: () => raw,
    release() {
      releases += 1;
    },
    dispose() {
      disposals += 1;
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  await assert.rejects(agent.session.shutdown(), (error) => error === shutdownError);
  assert.equal(releases, 1);
  assert.equal(disposals, 1);
});

test("the host bridge keeps retry timing and handshake detail session-scoped", async () => {
  const sleeps = [];
  const left = {
    connect(_endpoint, _apiKey, sessionId, metadata) {
      const error = new Error(`rejected ${sessionId}`);
      error.status = 429;
      error.body = "slow down";
      error.retryAfter = 3;
      assert.deepEqual(metadata, {
        accountId: "acct-left",
        fedramp: true,
        threadId: "session-left",
        turnState: "turn-left",
      });
      throw error;
    },
    sleep(milliseconds) {
      sleeps.push(["left", milliseconds]);
      return Promise.resolve();
    },
  };
  const right = {
    connect() {
      throw new Error("unused");
    },
    sleep(milliseconds) {
      sleeps.push(["right", milliseconds]);
      return Promise.resolve();
    },
  };

  activateHost(left);
  bindHostSession(left, "session-left");
  bindHostSession(right, "session-right");
  await globalThis.nanocodexHost.sleep("session-left", 7);
  await globalThis.nanocodexHost.sleep("session-right", 11);
  assert.deepEqual(sleeps, [["left", 7], ["right", 11]]);

  await assert.rejects(
    globalThis.nanocodexHost.connect(
      "wss://api.test",
      "secret",
      "acct-left",
      true,
      "session-left",
      "session-left",
      "turn-left",
    ),
    (error) => {
      assert.deepEqual(JSON.parse(error), {
        kind: "handshake_rejected",
        status: 429,
        body: "slow down",
        retry_after: 3,
      });
      return true;
    },
  );

  releaseHostSession(left, "session-left");
  releaseHostSession(right, "session-right");
});

test("event iterators release subscriptions and fail closed before buffering without bound", async () => {
  const subscriptions = new Set();
  const runtime = defineRuntime({
    create: () => rawAgent("session-events"),
    subscribe(listener) {
      subscriptions.add(listener);
      return () => subscriptions.delete(listener);
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);
  const watch = agent.events.watch();
  const iterator = watch[Symbol.asyncIterator]();

  assert.equal(subscriptions.size, 1);
  for (let seq = 1; seq <= 4_097; seq += 1) {
    for (const listener of subscriptions) {
      listener({ type: "api.event", request_id: agent.sessionId, seq });
    }
  }
  for (let seq = 1; seq <= 4_096; seq += 1) {
    assert.equal((await iterator.next()).value.seq, seq);
  }
  await assert.rejects(iterator.next(), /event iterator exceeded its private buffer/);
  assert.equal(subscriptions.size, 0);

  const restarted = watch[Symbol.asyncIterator]();
  assert.equal(subscriptions.size, 1);
  const firstPending = restarted.next();
  const secondPending = restarted.next();
  for (const listener of subscriptions) {
    listener({ type: "api.event", request_id: agent.sessionId, seq: 4_098 });
    listener({ type: "api.event", request_id: agent.sessionId, seq: 4_099 });
  }
  assert.deepEqual(
    (await Promise.all([firstPending, secondPending])).map(({ value }) => value.seq),
    [4_098, 4_099],
  );
  await restarted.return();
  assert.equal(subscriptions.size, 0);

  watch.off();
  agent.dispose();
});

test("a failing event listener is reported without interrupting other observers", async () => {
  const subscriptions = new Set();
  const reported = [];
  const previousReportError = globalThis.reportError;
  globalThis.reportError = (error) => reported.push(error);
  try {
    const runtime = defineRuntime({
      create: () => rawAgent("session-observers"),
      subscribe(listener) {
        subscriptions.add(listener);
        return () => subscriptions.delete(listener);
      },
      decorate: (agent) => agent.extend(Actions.agentActions()),
    });
    const agent = await createAgentClient(runtime);
    const watch = agent.events.watch();
    watch.onEvent(() => { throw new Error("observer failed"); });
    const seen = [];
    watch.onEvent((event) => seen.push(event.seq));
    const iterator = watch[Symbol.asyncIterator]();
    const next = iterator.next();

    for (const listener of subscriptions) {
      listener({ type: "api.event", request_id: agent.sessionId, seq: 1 });
    }
    assert.deepEqual(seen, [1]);
    assert.equal((await next).value.seq, 1);
    assert.match(reported[0]?.message, /observer failed/);

    watch.off();
    agent.dispose();
  } finally {
    if (previousReportError === undefined) delete globalThis.reportError;
    else globalThis.reportError = previousReportError;
  }
});

function rawAgent(sessionId) {
  return {
    sessionId,
    prompt(input, id) {
      return rawTurn(id === undefined
        ? `${sessionId}:${input}`
        : `${sessionId}:${id}:${input}`);
    },
    promptContent(input, id) {
      const text = JSON.parse(input)[0].text;
      return rawTurn(id === undefined ? `${sessionId}:${text}` : `${sessionId}:${id}:${text}`);
    },
    async fork() {
      return rawAgent(`${sessionId}-fork`);
    },
    async forkFrom() {
      return rawAgent(`${sessionId}-fork`);
    },
    async spawn() {
      return rawAgent(`${sessionId}-spawn`);
    },
    async compact() {},
    async context() {
      return JSON.stringify({
        workspace: "/workspace",
        history: [{ type: "message", role: "developer" }],
      });
    },
    async appendDeveloperMessage() {
      return JSON.stringify({
        workspace: "/workspace",
        history: [{ type: "message", role: "developer" }],
      });
    },
    async startRealtimeConversation() {
      return this.appendDeveloperMessage();
    },
    async endRealtimeConversation() {
      return this.appendDeveloperMessage();
    },
    realtimeDelegation(input, transcript) {
      return `delegated:${input}:${JSON.parse(transcript).map(({ role, text }) => `${role}: ${text}`).join("\n")}`;
    },
    realtimeTailDelegation(transcript) {
      const entries = JSON.parse(transcript);
      return entries.length
        ? `tail:${entries.map(({ role, text }) => `${role}: ${text}`).join("\n")}`
        : undefined;
    },
    free() {},
  };
}

function rawTurn(value) {
  return {
    async result() {
      return {
        finalMessage: value,
        snapshot() {
          return JSON.stringify({
            version: 1,
            model: "gpt-5.6-sol",
            lineage_id: "test-lineage",
            prompt_cache_key: "test-cache-key",
            workspace: ".",
            canonical_context: {},
            history: [],
          });
        },
        usage() {
          return JSON.stringify({
            input_tokens: 0,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            estimated_cost: null,
            cost_status: "usage_not_reported",
          });
        },
        free() {},
      };
    },
    async steer() {},
    async steerContent() {},
    async cancel() {},
    free() {},
  };
}
