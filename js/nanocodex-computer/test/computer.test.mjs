import test from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { createComputerTools, outputContent } from "../index.mjs";
import { validateInput } from "../contract.mjs";

const executable = process.env.NANOCODEX_TEST_COMPUTER ?? fileURLToPath(new URL("../../../crates/experimental/nanocodex-computer/runtime/target/debug/nanocodex-computer", import.meta.url));
const context = (sessionId, signal = new AbortController().signal) => ({ sessionId, signal, callId: "test", parentCallId: "", model: "gpt-6-astra" });

test("CUA input validation matches the Rust transport without artificial size caps", () => {
  for (const timeout_ms of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) assert.throws(() => validateInput({ code: "1", timeout_ms }));
  assert.equal(validateInput({ code: "1", timeout_ms: 300000 }).timeout_ms, 300000);
  assert.deepEqual(validateInput({ code: "1", title: null, timeout_ms: null }), { code: "1", title: null });
  assert.throws(() => validateInput({ code: "1", executable: "/bin/sh" }));
  assert.throws(() => validateInput({ code: "1" }, true));
  assert.equal(validateInput({ code: "🧪".repeat(262145) }).code.length, 524290);
  assert.deepEqual(validateInput({ code: "1" }), { code: "1" });
});

test("MCP images remain image inputs in Codex-compatible function outputs", () => {
  const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a7WQAAAAASUVORK5CYII=";
  const output = outputContent({ content: [{ type: "text", text: "observed" }, { type: "image", mimeType: "image/png", data: png }] });
  assert.equal(output[0].type, "input_text");
  assert.equal(output[1].type, "input_image");
  assert.equal(output[1].detail, "original");
  assert.equal(outputContent({ content: [{ type: "image", mimeType: "image/jpeg", data: png }] })[0].image_url, `data:image/png;base64,${png}`);
  assert.throws(() => outputContent({ content: [{ type: "image", mimeType: "image/png", data: "AAAA" }] }));
  assert.throws(() => outputContent({ content: [{ type: "image", mimeType: "image/png", data: "%%%" }] }));
  assert.throws(() => outputContent({ content: [{ type: "resource_link", uri: "file:///private/data" }] }));
});

test("real companion retains each conversation, returns screenshots, and resets", async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js, reset] = computer.tools;
  const first = await js.handler({ code: "let app = await cua.getApp('fixture://native');" }, context("one"));
  assert.equal(first.success, true);
  assert.equal(typeof first.metadata["codex/nodeReplExecutionDurationMs"], "number");
  assert.deepEqual(first.metadata, first.value._meta);
  const image = await js.handler({ code: "await app.click(2); await nodeRepl.emitImage(await app.getScreenshot({emit:false}));" }, context("one"));
  assert.equal(image.success, true);
  assert(image.output.some(item => item.type === "input_image"));
  const other = await js.handler({ code: "nodeRepl.write(typeof app);" }, context("two"));
  assert.match(JSON.stringify(other.value), /undefined/);
  await reset.handler({}, context("one"));
  const cleared = await js.handler({ code: "nodeRepl.write(typeof app);" }, context("one"));
  assert.match(JSON.stringify(cleared.value), /undefined/);
});

test("aborting native execution closes the process and requires reset", async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js, reset] = computer.tools;
  await js.handler({ code: "nodeRepl.write(1);" }, context("cancel"));
  const abort = new AbortController();
  const pending = js.handler({ code: "while (true) {}" }, context("cancel", abort.signal));
  setTimeout(() => abort.abort(), 150);
  await assert.rejects(pending);
  await assert.rejects(js.handler({ code: "nodeRepl.write(2);" }, context("cancel")), /js_reset/);
  await reset.handler({}, context("cancel"));
  assert.equal((await js.handler({ code: "nodeRepl.write(3);" }, context("cancel"))).success, true);
});

test("Codex optional nulls, long deadlines and current metadata survive the Node adapter", async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  for (const timeout_ms of [null, 300_000, 2_147_483_648]) {
    const result = await js.handler({ code: "await new Promise(resolve=>setTimeout(resolve,20)); nodeRepl.write(JSON.stringify(nodeRepl.requestMeta));", title: null, timeout_ms }, context("metadata"));
    assert.equal(result.success, true);
    assert.deepEqual(JSON.parse(result.output.at(-1).text)["x-codex-turn-metadata"], { thread_id: "metadata", call_id: "test", model: "gpt-6-astra" });
  }
});

test("queued cancellation rejects immediately and only release discards the scope", { timeout: 10_000 }, async t => {
  for (const release of [true, false]) {
    const computer = createComputerTools({ executable, args: ["--fixture"] });
    t.after(computer.close);
    const [js] = computer.tools;
    await js.handler({ code: "let marker = 'old';" }, context("queued"));
    const blocker = new AbortController(), cancelled = new AbortController();
    const blocking = assert.rejects(js.handler({ code: "while (true) {}" }, context("other", blocker.signal)));
    const queued = js.handler({ code: "throw new Error('cancelled call ran');" }, context("queued", cancelled.signal));
    const rejected = assert.rejects(queued, release ? /released/ : /cancelled/);
    if (release) js.releaseSession("queued");
    else cancelled.abort(new Error("queued call cancelled"));
    // Do not release the input queue until cancellation is visible to its
    // caller. Waiting for the prior call would deadlock this test.
    await rejected;
    blocker.abort();
    await blocking;
    const fresh = await js.handler({ code: "nodeRepl.write(typeof marker);" }, context("queued"));
    assert.equal(fresh.output.at(-1).text, release ? "undefined" : "string");
  }
});

test("independent conversations execute in parallel", { timeout: 10_000 }, async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  await Promise.all([
    js.handler({ code: "nodeRepl.write('warm');" }, context("parallel-left")),
    js.handler({ code: "nodeRepl.write('warm');" }, context("parallel-right")),
  ]);
  const started = performance.now();
  const [left, right] = await Promise.all([
    js.handler({ code: "await new Promise(resolve=>setTimeout(resolve,1000)); nodeRepl.write('left');" }, context("parallel-left")),
    js.handler({ code: "await new Promise(resolve=>setTimeout(resolve,1000)); nodeRepl.write('right');" }, context("parallel-right")),
  ]);
  assert.equal(left.output.at(-1).text, "left");
  assert.equal(right.output.at(-1).text, "right");
  assert(performance.now() - started < 1750, "independent sessions were serialized");
});

test("a persistent Sky-style CUA realm orders concurrent calls without a global lock", async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  await js.handler({ code: "globalThis.order = [];" }, context("ordered"));
  const [first, second, observed] = await Promise.all([
    js.handler({ code: "await new Promise(resolve=>setTimeout(resolve,150)); order.push('first'); nodeRepl.write('first');" }, context("ordered")),
    js.handler({ code: "order.push('second'); nodeRepl.write('second');" }, context("ordered")),
    js.handler({ code: "nodeRepl.write(JSON.stringify(order));" }, context("ordered")),
  ]);
  assert.equal(first.output.at(-1).text, "first");
  assert.equal(second.output.at(-1).text, "second");
  assert.equal(observed.output.at(-1).text, '["first","second"]');
});

test("many persistent CUA realms run concurrently", { timeout: 15_000 }, async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  const sessions = Array.from({ length: 12 }, (_, index) => `saturation-${index}`);
  await Promise.all(sessions.map(session => js.handler({ code: "nodeRepl.write('warm');" }, context(session))));
  const started = performance.now();
  const results = await Promise.all(sessions.map(session => js.handler({
    code: `await new Promise(resolve=>setTimeout(resolve,250)); nodeRepl.write(${JSON.stringify(session)});`,
  }, context(session))));
  assert.deepEqual(results.map(result => result.output.at(-1).text), sessions);
  assert(performance.now() - started < 1800, "independent CUA realms saturated a shared serial queue");
});

test("large source and fragmented output cross the real stdio transport", { timeout: 30_000 }, async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  const source = `nodeRepl.write('input-ok');/*${"🧪".repeat(300_000)}*/`;
  assert.equal((await js.handler({ code: source }, context("large"))).output.at(-1).text, "input-ok");
  const bytes = 9 * 1024 * 1024;
  const output = await js.handler({ code: `nodeRepl.write('x'.repeat(${bytes}));` }, context("large"));
  assert.equal(output.output.at(-1).text.length, bytes);
});

test("releasing an executing conversation interrupts its owned process", async t => {
  const computer = createComputerTools({ executable, args: ["--fixture"] });
  t.after(computer.close);
  const [js] = computer.tools;
  await js.handler({ code: "let marker = 1;" }, context("released"));
  const pending = js.handler({ code: "while (true) {}" }, context("released"));
  const rejected = assert.rejects(pending, /released/);
  setTimeout(() => js.releaseSession("released"), 100);
  await rejected;
  assert.equal((await js.handler({ code: "nodeRepl.write(typeof marker);" }, context("released"))).output.at(-1).text, "undefined");
});
