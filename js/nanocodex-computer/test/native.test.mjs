import test from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { writeFile } from "node:fs/promises";
import { createComputerTools } from "../index.mjs";

const app = process.env.NANOCODEX_TEST_NATIVE_APP;
test("macOS public CUA controls an owned AppKit fixture and emits its screenshot", { skip: !app, timeout: 30_000 }, async t => {
  assert.equal(process.platform, "darwin");
  const executable = fileURLToPath(new URL("../../../crates/experimental/nanocodex-computer/runtime/target/debug/nanocodex-computer", import.meta.url));
  const computer = createComputerTools({ executable });
  t.after(computer.close);
  const context = { sessionId: "native-fixture", callId: "native-fixture", model: "test", signal: new AbortController().signal };
  const invoke = async code => {
    const result = await computer.tools[0].handler({ code }, context);
    assert.equal(result.success, true, JSON.stringify(result.value));
    return result;
  };
  await invoke(`let app = await cua.getApp(${JSON.stringify(app)});`);
  await invoke(`
    var state = await app.getAXState({disableDiffing:true,emit:false});
    var line = state.split('\\n').find(line => line.includes('ID: skyre.test.input'));
    if (!line) throw new Error('Owned input missing from AX tree: '+state);
    var field = Number(line.trim().match(/^\\d+/)[0]);
    await app.setValue(field, 'Nanocodex native Ω');
    // setValue changes the AX value: observe again before reusing a target.
    state = await app.getAXState({disableDiffing:true,emit:false});
    line = state.split('\\n').find(line => line.includes('ID: skyre.test.input'));
    field = Number(line.trim().match(/^\\d+/)[0]);
    await app.click(field);
    await app.typeText(' 🧪');
    var changed = await app.getAXState({disableDiffing:true,emit:false});
    if (!changed.includes(' 🧪')) throw new Error('Native typing did not reach the fixture: '+changed);
    nodeRepl.write(changed);
  `);
  const result = await invoke("await app.getScreenshot();");
  assert.equal(result.value.content.find(item => item.type === "image")?.mimeType, "image/jpeg");
  const image = result.output.find(item => item.type === "input_image");
  assert(image, "The screenshot must cross the adapter as an API image input");
  assert.match(image.image_url, /^data:image\/jpeg;base64,/);
  assert.equal(image.detail, "original");
  if (process.env.NANOCODEX_TEST_SCREENSHOT) await writeFile(process.env.NANOCODEX_TEST_SCREENSHOT, Buffer.from(image.image_url.split(",")[1], "base64"));
});
