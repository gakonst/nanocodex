import test from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { createComputerTools, outputContent } from "../index.mjs";
import { validateInput } from "../contract.mjs";

const executable = fileURLToPath(new URL("../../../crates/experimental/nanocodex-computer/runtime/target/debug/nanocodex-computer", import.meta.url));
const context = (sessionId, signal = new AbortController().signal) => ({ sessionId, signal, callId: "test", parentCallId: "", model: "gpt-6-astra" });

test("CUA input bounds match the Rust transport", () => {
  for (const timeout_ms of [0, -1, 1.5, 120001]) assert.throws(() => validateInput({ code: "1", timeout_ms }));
  assert.throws(() => validateInput({ code: "1", executable: "/bin/sh" }));
  assert.throws(() => validateInput({ code: "1" }, true));
  assert.throws(() => validateInput({ code: "🧪".repeat(262145) }));
  assert.deepEqual(validateInput({ code: "1" }), { code: "1", timeout_ms: 30000 });
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
