import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { build } from "esbuild";
import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { newQuickJSAsyncWASMModuleFromVariant } from "quickjs-emscripten-core";

const root = fileURLToPath(new URL("../", import.meta.url));
const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);
const wav = Buffer.alloc(44 + 160);
wav.write("RIFF"); wav.writeUInt32LE(wav.length - 8, 4); wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28);
wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
wav.write("data", 36); wav.writeUInt32LE(160, 40);
const audioUrl = `data:audio/wav;base64,${wav.toString("base64")}`;

for (const keepNames of [false, true]) {
  test(`minified QuickJS bundle preserves every guest helper (keepNames=${keepNames})`, async () => {
    const bundle = await build({
      absWorkingDir: root,
      stdin: { contents: 'export { createCodeRuntime } from "./runtime/code-runtime.mjs"; export { createQuickJsEvaluator } from "./runtime/quickjs-evaluator.mjs";', resolveDir: root },
      bundle: true, minify: true, keepNames, platform: "node", format: "esm", write: false,
    });
    const { createCodeRuntime, createQuickJsEvaluator } = await import(
      `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString("base64")}`
    );
    const runtime = createCodeRuntime({ "fixture.echo": {
      description: "Echo a fixture value.",
      parameters: { type: "object", properties: { value: { type: "integer" } }, required: ["value"] },
      handler: async (input) => input,
    } }, { evaluate: createQuickJsEvaluator(quickJs) });
    const result = JSON.parse(await runtime.executeCode(`
      store("fixture", { value: 42 });
      const found = searchTools("FIXTURE echo", { limit: 1 });
      const described = describeTool(found[0].callableName);
      try { described.inputSchema.properties.value.type = "string"; } catch {}
      let invalidQuery = false;
      try { searchTools(""); } catch (error) { invalidQuery = error instanceof TypeError; }
      let invalidLimit = false;
      try { searchTools("echo", { limit: 101 }); } catch (error) { invalidLimit = error instanceof RangeError; }
      text({ found, described, invalidQuery, invalidLimit,
        namespace: describeNamespace("fixture").map((tool) => tool.name),
        missing: describeTool("unavailable") === undefined,
        value: await tools[described.callableName](load("fixture")) });
      image("data:image/png;base64,YQ==");
      generatedImage({ image_url: "data:image/png;base64,YQ==", output_hint: "fixture-image" });
      audio(${JSON.stringify(audioUrl)});
    `, `bundle-${keepNames}`, "fixture-call"));
    assert.equal(result.success, true, JSON.stringify(result.output).slice(0, 600));
    const output = JSON.stringify(result.output);
    assert.match(output, /42/);
    assert.match(output, /fixture-image/);
    assert.match(output, /shorter than 25 ms/);
    const report = JSON.parse(result.output.find((item) => item.type === "input_text" && item.text.startsWith('{"found":')).text);
    assert.deepEqual(report, {
      found: [{ name: "fixture.echo", callableName: "fixture_echo", description: "Echo a fixture value." }],
      described: {
        name: "fixture.echo", callableName: "fixture_echo", description: "Echo a fixture value.",
        inputSchema: { type: "object", properties: { value: { type: "integer" } }, required: ["value"] },
        outputSchema: null, kind: "function",
      },
      invalidQuery: true, invalidLimit: true, namespace: ["fixture.echo"], missing: true, value: { value: 42 },
    });
    assert.deepEqual(result.nested_calls.map((call) => call.name), ["fixture.echo"]);
  });
}
