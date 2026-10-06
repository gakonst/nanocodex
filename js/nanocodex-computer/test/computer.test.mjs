import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setImmediate } from "node:timers/promises";
import { connectComputerTools, createComputerTools, outputContent } from "../index.mjs";
import { catalog, provider, png } from "./provider-fixture.mjs";

const context = (sessionId, signal = new AbortController().signal) => ({ sessionId, signal, callId: "test", parentCallId: "", model: "fixture" });
const open = async t => { const computer = await connectComputerTools(provider()); t.after(computer.close); return computer; };

test("each conversation owns a provider process and reset is forwarded to the provider", async t => {
  const computer = await open(t), js = computer.tool("js");
  const first = await js.handler({set:"one"}, context("one"));
  assert.deepEqual(first.metadata, {provider:"fixture"});
  assert.deepEqual(first.metadata, first.value._meta);
  assert.equal((await js.handler({get:true}, context("one"))).output[0].text, "one");
  assert.equal((await js.handler({get:true}, context("two"))).output[0].text, "undefined");
  await computer.tool("js_reset").handler({providerOption:"unaltered"}, context("one"));
  assert.equal((await js.handler({get:true}, context("one"))).output[0].text, "undefined");
});

test("provider owns arguments, errors, and post-error behavior", async t => {
  const computer = await open(t), js = computer.tool("js");
  const input = {unknown:{value:1},title:null,timeout_ms:5000,wait:30,isError:true};
  const result = await js.handler(input, context("contracts"));
  assert.deepEqual(JSON.parse(result.output[0].text).arguments, input);
  assert.equal(result.success, false);
  assert.equal(result.value.isError, true);
  assert.equal((await js.handler({}, context("contracts"))).success, true);
});

test("JavaScript attachments forward the native confirmation defaults and trusted overrides", async t => {
  const original = process.env.NANOCODEX_COMPUTER_CONFIRMATION_POLICIES;
  t.after(() => {
    if (original === undefined) delete process.env.NANOCODEX_COMPUTER_CONFIRMATION_POLICIES;
    else process.env.NANOCODEX_COMPUTER_CONFIRMATION_POLICIES = original;
  });
  for (const [configured, option, expected] of [
    [undefined, undefined, "No confirmation policy applies."],
    ["upstream host policy", undefined, "upstream host policy"],
    ["off", undefined, undefined],
    ["upstream host policy", null, undefined],
    ["off", "task host policy", "task host policy"],
  ]) {
    if (configured === undefined) delete process.env.NANOCODEX_COMPUTER_CONFIRMATION_POLICIES;
    else process.env.NANOCODEX_COMPUTER_CONFIRMATION_POLICIES = configured;
    const computer = await connectComputerTools({ ...provider(), confirmationPolicies: option });
    t.after(computer.close);
    const result = await computer.tool("js").handler({ source: "1 + 1" }, { ...context("policy"), turnId: "turn" });
    const call = JSON.parse(result.output[0].text);
    assert.deepEqual(call._meta["openai/confirmation_policies"], expected === undefined ? undefined : { browser_use: expected, computer_use: expected });
    assert.deepEqual(call._meta["x-codex-turn-metadata"], {
      session_id: "policy", thread_id: "policy", turn_id: "turn", call_id: "test", model: "fixture",
    });
    assert.deepEqual(call.arguments, { source: "1 + 1" });
  }
});

test("queued cancellation rejects promptly without running or resetting the active process", async t => {
  const computer = await open(t), js = computer.tool("js");
  await js.handler({set:"kept"}, context("queue"));
  const blocking = js.handler({wait:100}, context("queue"));
  const abort = new AbortController();
  const cancelled = assert.rejects(js.handler({set:"wrong"}, context("queue", abort.signal)), /cancelled/);
  abort.abort(new Error("queued call cancelled"));
  await cancelled;
  await blocking;
  assert.equal((await js.handler({get:true}, context("queue"))).output[0].text, "kept");
});

test("provider calls serialize per conversation and independent conversations run concurrently", async t => {
  const computer = await open(t), js = computer.tool("js");
  const abort = new AbortController();
  const blocked = assert.rejects(js.handler({block:true}, context("blocked", abort.signal)));
  const [first, second] = await Promise.all([
    js.handler({set:"first",wait:50}, context("ordered")),
    js.handler({get:true}, context("ordered")),
  ]);
  assert(first.success);
  assert.equal(second.output[0].text, "first");
  abort.abort();
  await blocked;
});

test("large source and fragmented output cross stdio without artificial size limits", async t => {
  const computer = await open(t), js = computer.tool("js");
  const input = {source:"🧪".repeat(300_000)};
  assert.deepEqual(JSON.parse((await js.handler(input, context("large"))).output[0].text).arguments, input);
  assert.equal((await js.handler({large:9*1024*1024}, context("large"))).output[0].text.length, 9*1024*1024);
});

test("closing an attachment cancels active and queued calls and prevents reuse", async t => {
  const computer = await open(t), js = computer.tool("js");
  await js.handler({}, context("closing"));
  const active = assert.rejects(js.handler({block:true}, context("closing")));
  const queued = assert.rejects(js.handler({}, context("closing")));
  await computer.close();
  await Promise.all([active, queued]);
  assert.throws(() => js.handler({}, context("closing")), /closed/);
});

test("output conversion preserves provider MIME declarations and unfamiliar MCP content", () => {
  const resource = {type:"resource_link",name:"Provider document",uri:"fixture://document",_meta:{provider:true}};
  const output = outputContent({content:[
    {type:"image",mimeType:"image/jpeg",data:png},
    {type:"audio",mimeType:"audio/provider-format",data:"fixture"},
    resource,
  ]});
  assert.equal(output[0].image_url, `data:image/jpeg;base64,${png}`);
  assert.equal(output[1].audio_url, "data:audio/provider-format;base64,fixture");
  assert.deepEqual(JSON.parse(output[2].text), resource);
});

async function loggedProvider(t, options = {}) {
  const directory = await mkdtemp(join(tmpdir(), "cua-ownership-"));
  t.after(() => rm(directory, {recursive:true,force:true}));
  const callLog = join(directory, "calls"), requestLog = join(directory, "requests");
  const read = async path => readFile(path, "utf8").catch(error => {
    if (error.code !== "ENOENT") throw error;
    return "";
  });
  return {
    options: provider({...options, callLog, requestLog}),
    calls: async () => (await read(callLog)).trim().split("\n").filter(Boolean).map(JSON.parse),
    requests: async () => (await read(requestLog)).trim().split("\n"),
  };
}

async function until(predicate) {
  while (!await predicate()) await setImmediate();
}

for (const name of ["js", "js_reset"]) {
  test(`${name} preserves provider arguments through slow startup, queueing, and execution`, { timeout: 5000 }, async t => {
    const fixture = await loggedProvider(t, {startupWait:50});
    const definitions = structuredClone(catalog);
    const computer = createComputerTools({...fixture.options, definitions});
    t.after(computer.close);
    const tool = computer.tool(name);
    const firstInput = Object.freeze({timeout_ms:1, wait:100, set:"first"});
    const queuedInput = Object.freeze({timeout_ms:1, wait:40, nested:Object.freeze({untouched:true})});
    const [first, queued] = await Promise.all([
      tool.handler(firstInput, context("provider-budget")),
      tool.handler(queuedInput, context("provider-budget")),
    ]);
    assert.deepEqual(JSON.parse(first.output[0].text).arguments, firstInput);
    assert.deepEqual(JSON.parse(queued.output[0].text).arguments, queuedInput);
    assert.deepEqual((await fixture.calls()).map(call => call.arguments), [firstInput, queuedInput]);
    assert.deepEqual(definitions, catalog);
    assert.deepEqual(computer.definitions, catalog);
    assert.deepEqual(computer.tools.map(tool => tool.providerDefinition), catalog);
  });

  test(`${name} has no default, parsed, or overflow tool deadline`, { timeout: 5000 }, async t => {
    const fixture = await loggedProvider(t);
    const computer = createComputerTools({...fixture.options, definitions:catalog});
    t.after(computer.close);
    await computer.tool("js").handler({}, context("unbounded"));
    t.mock.timers.enable({apis:["setTimeout"]});
    const active = new AbortController();
    let activeSettled = false;
    const blocked = assert.rejects(computer.tool(name).handler({block:true}, context("unbounded", active.signal)).finally(() => { activeSettled = true; }), /native effects may continue/);
    await until(async () => (await fixture.calls()).length === 2);
    const inputs = [{}, ...[0, -1, 1.5, "1", null, Number.MAX_SAFE_INTEGER + 1, Number.MAX_SAFE_INTEGER].map(timeout_ms => ({timeout_ms}))];
    let settled = 0;
    const queuedAbort = new AbortController();
    const queued = inputs.map(input => assert.rejects(computer.tool(name).handler(input, context("unbounded", queuedAbort.signal)).finally(() => settled++), /queued cancellation/));
    t.mock.timers.tick(2_147_483_648);
    await setImmediate();
    assert.equal(settled, 0);
    assert.equal(activeSettled, false, "active calls have no host deadline either");
    assert.equal((await fixture.calls()).length, 2);
    queuedAbort.abort(new Error("queued cancellation"));
    await Promise.all(queued);
    active.abort();
    await blocked;
    assert.equal((await fixture.calls()).length, 2, "cancelled calls are never submitted or replayed");
  });
}

for (const blockMethod of ["initialize", "tools/list"]) {
  for (const mode of ["connection", "session"]) {
    test(`${mode} startup bounds blocked ${blockMethod} independently of provider tool arguments`, { timeout: 5000 }, async t => {
      const fixture = await loggedProvider(t, {blockMethod});
      t.mock.timers.enable({ apis: ["setTimeout"] });
      let rejected = false;
      let pending;
      if (mode === "connection") pending = connectComputerTools(fixture.options);
      else {
        const computer = createComputerTools({...fixture.options, definitions:catalog});
        t.after(computer.close);
        pending = computer.tool("js").handler({timeout_ms:1}, context("startup"));
      }
      const expired = assert.rejects(pending, error => {
        rejected = true;
        return /provider startup\/discovery timed out after 120000 ms/.test(error.message);
      });
      await until(async () => (await fixture.requests()).includes(blockMethod));
      t.mock.timers.tick(119_999);
      await setImmediate();
      assert.equal(rejected, false);
      t.mock.timers.tick(1);
      await expired;
      assert.equal((await fixture.calls()).length, 0);
    });
  }
}

for (const trigger of ["abort", "release", "close"]) {
  test(`${trigger} reports uncertain dispatched effects and never replays cancelled work`, { timeout:5000 }, async t => {
    const fixture = await loggedProvider(t);
    const computer = createComputerTools({...fixture.options, definitions:catalog});
    t.after(computer.close);
    const js = computer.tool("js"), abort = new AbortController();
    await js.handler({set:"independent"}, context("other"));
    const active = assert.rejects(js.handler({block:true, set:"effect"}, context("cancel", abort.signal)), error => {
      assert.match(error.message, /native effects may continue and completion is unknown/);
      if (trigger === "abort") assert.equal(error.cause, abort.signal.reason);
      return true;
    });
    await until(async () => (await fixture.calls()).some(call => call.arguments.set === "effect"));
    const queued = assert.rejects(js.handler({set:"never"}, context("cancel", abort.signal)));
    if (trigger === "abort") abort.abort(new Error("caller cancelled"));
    else if (trigger === "release") js.releaseSession("cancel");
    else await computer.close();
    await Promise.all([active, queued]);
    if (trigger !== "close") {
      if (trigger === "abort") {
        await assert.rejects(js.handler({get:true}, context("cancel")), /session interrupted.*js_reset/);
        assert.equal((await computer.tool("js_reset").handler({isError:true}, context("cancel"))).success, false);
        await assert.rejects(js.handler({get:true}, context("cancel")), /session interrupted.*js_reset/);
        await computer.tool("js_reset").handler({}, context("cancel"));
      }
      assert.equal((await js.handler({get:true}, context("cancel"))).output[0].text, "undefined");
      assert.equal((await js.handler({get:true}, context("other"))).output[0].text, "independent");
    }
    const calls = await fixture.calls();
    assert.equal(calls.filter(call => call.arguments.set === "effect").length, 1);
    assert.equal(calls.filter(call => call.arguments.set === "never").length, 0);
  });
}

test("dispatched transport failure blocks already queued work until explicit reset", {timeout:5000}, async t => {
  const fixture = await loggedProvider(t);
  const computer = createComputerTools({...fixture.options, definitions:catalog});
  t.after(computer.close);
  const js = computer.tool("js");
  const failed = assert.rejects(js.handler({crash:true}, context("transport")), /interrupted after dispatch.*effects are uncertain.*js_reset/);
  const queued = assert.rejects(js.handler({set:"never"}, context("transport")), /session interrupted.*js_reset/);
  await Promise.all([failed, queued]);
  await computer.tool("js_reset").handler({}, context("transport"));
  assert.equal((await js.handler({get:true}, context("transport"))).output[0].text, "undefined");
  assert.deepEqual((await fixture.calls()).map(call => call.arguments), [{crash:true}, {}, {get:true}]);
});


test("turn completion calls the upstream hook once for retained turns without starting a new session", async t => {
  const directory = await mkdtemp(join(tmpdir(), "cua-lifecycle-"));
  t.after(() => rm(directory, {recursive:true,force:true}));
  const callLog = join(directory, "calls");
  const computer = await connectComputerTools(provider({callLog, lifecycle:true}));
  t.after(computer.close);
  assert.equal(computer.tools.some(tool => tool.name.endsWith("turn_ended")), false);
  await computer.endTurn("unused", "turn");
  const current = {...context("owner"), turnId:"one"};
  await computer.tool("js").handler({set:"retained"}, current);
  await computer.endTurn("owner", "one");
  await computer.endTurn("owner", "one");
  assert.equal((await computer.tool("js").handler({get:true}, {...current,turnId:"two"})).output[0].text, "retained");
  await computer.endTurn("owner", "two", "Interrupt");
  const calls = (await readFile(callLog, "utf8")).trim().split("\n").map(JSON.parse);
  const ends = calls.filter(call => call.name === "turn_ended");
  assert.deepEqual(ends.map(call => call.arguments), [
    {hook_event_name:"Stop",session_id:"owner",turn_id:"one"},
    {hook_event_name:"Interrupt",session_id:"owner",turn_id:"two"},
  ]);
  assert.equal(ends[0]._meta["x-codex-turn-metadata"].turn_id, "one");
});
