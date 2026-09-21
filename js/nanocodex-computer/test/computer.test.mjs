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

test("MCP images remain image inputs in model outputs", () => {
  const output = outputContent({ content: [{ type: "text", text: "observed" }, { type: "image", mimeType: "image/png", data: png }] });
  assert.equal(output[0].type, "input_text");
  assert.deepEqual(output[1], {type:"input_image",image_url:`data:image/png;base64,${png}`,detail:"original"});
});

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

for (const trigger of ["abort", "release", "exit"]) {
  test(`${trigger} stops the owned process; subsequent calls use a fresh provider without custom reset policy`, async t => {
    const computer = await open(t), js = computer.tool("js");
    await js.handler({set:"old"}, context(trigger));
    const abort = new AbortController();
    const pending = assert.rejects(js.handler(trigger === "exit" ? {crash:true} : {block:true}, context(trigger, abort.signal)));
    if (trigger !== "exit") setTimeout(() => trigger === "abort" ? abort.abort() : js.releaseSession(trigger), 30);
    await pending;
    assert.equal((await js.handler({get:true}, context(trigger))).output[0].text, "undefined");
  });
}

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

for (const name of ["js", "js_reset"]) {
  test(`${name} host deadline stops a blocked process and permits fresh calls without retry`, { timeout: 5000 }, async t => {
    const computer = await open(t), js = computer.tool("js");
    await js.handler({set:"old"}, context("deadline"));
    const expired = assert.rejects(
      computer.tool(name).handler({block:true,timeout_ms:100}, context("deadline")),
      new RegExp(`CUA ${name} timed out after 100 ms`),
    );
    assert.equal((await js.handler({set:"independent"}, context("other"))).success, true);
    await expired;
    assert.equal((await js.handler({get:true}, context("deadline"))).output[0].text, "undefined");
    assert.equal((await js.handler({get:true}, context("other"))).output[0].text, "independent");
  });

  test(`${name} queued deadline rejects before active work completes and preserves its process`, { timeout: 5000 }, async t => {
    const computer = await open(t), js = computer.tool("js");
    await js.handler({set:"kept"}, context("queue-deadline"));
    let completed = false;
    const active = js.handler({wait:200}, context("queue-deadline")).then(result => { completed = true; return result; });
    await assert.rejects(
      computer.tool(name).handler({set:"wrong",timeout_ms:30}, context("queue-deadline")),
      new RegExp(`CUA ${name} timed out after 30 ms`),
    );
    assert.equal(completed, false);
    await active;
    assert.equal((await js.handler({get:true}, context("queue-deadline"))).output[0].text, "kept");
  });
}

for (const blockMethod of ["initialize", "tools/list"]) {
  test(`host deadline includes blocked ${blockMethod} during session startup`, { timeout: 5000 }, async t => {
    const directory = await mkdtemp(join(tmpdir(), "cua-startup-deadline-"));
    t.after(() => rm(directory, {recursive:true,force:true}));
    const requestLog = join(directory, "requests");
    const computer = createComputerTools({ ...provider({blockMethod,requestLog}), definitions: catalog });
    t.after(computer.close);
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const expired = assert.rejects(
      computer.tool("js").handler({timeout_ms:150}, context("startup-deadline")),
      /CUA js timed out after 150 ms/,
    );
    // Wait for evidence that startup reached the stalled RPC before expiring it.
    let requests;
    while (!requests?.split("\n").includes(blockMethod)) {
      requests = await readFile(requestLog, "utf8").catch(error => {
        if (error.code !== "ENOENT") throw error;
      });
      await setImmediate();
    }
    t.mock.timers.tick(150);
    await expired;
    assert(!requests.split("\n").includes("tools/call"));
  });
}

test("host deadline defaults to 30 seconds for invalid values and clamps timer overflow", { timeout: 5000 }, async t => {
  const computer = await open(t), js = computer.tool("js");
  await js.handler({}, context("timer-values"));
  t.mock.timers.enable({ apis: ["setTimeout"] });
  for (const timeout_ms of [undefined, 0, -1, 1.5, "1", null, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, Number.MAX_SAFE_INTEGER]) {
    const expected = timeout_ms === Number.MAX_SAFE_INTEGER ? 2_147_483_647 : 30_000;
    const active = new AbortController();
    const blocked = assert.rejects(js.handler({block:true}, context("timer-values", active.signal)));
    let rejected = false;
    const queued = assert.rejects(js.handler({timeout_ms}, context("timer-values")), error => {
      rejected = true;
      return error.message === `CUA js timed out after ${expected} ms`;
    });
    t.mock.timers.tick(expected - 1);
    await setImmediate();
    assert.equal(rejected, false);
    t.mock.timers.tick(1);
    await queued;
    active.abort();
    await blocked;
  }
});
