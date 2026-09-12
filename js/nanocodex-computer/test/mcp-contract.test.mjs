import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { readFile } from "node:fs/promises";
import { createComputerTools } from "../index.mjs";
import { CUA_JS_NAME, CUA_RESET_NAME, CUA_PARAMETERS, CUA_RESET_PARAMETERS } from "../contract.mjs";

test("Codex cua_repl MCP names, schemas, state and image blocks cross stdio unchanged", { timeout: 15_000 }, async t => {
  const executable = fileURLToPath(new URL("../../../crates/experimental/nanocodex-computer/runtime/target/debug/nanocodex-computer", import.meta.url));
  const child = spawn(executable, ["--fixture", "serve"], { env: { PATH: process.env.PATH }, stdio: ["pipe", "pipe", "inherit"] });
  const lines = createInterface({ input: child.stdout });
  t.after(() => { lines.close(); child.kill("SIGKILL"); });
  const iterator = lines[Symbol.asyncIterator]();
  let sequence = 0;
  const rpc = async (method, params) => {
    const id = ++sequence;
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    for (;;) {
      const line = await iterator.next();
      assert.equal(line.done, false, "MCP process exited before replying");
      const response = JSON.parse(line.value);
      if (response.method) continue;
      assert.equal(response.id, id);
      assert.equal(response.error, undefined);
      return response.result;
    }
  };
  const initialized = await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "codex-contract-test", version: "1" } });
  // MCP negotiation may select the server's supported older version.
  assert.equal(initialized.protocolVersion, "2025-03-26");
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  const catalog = await rpc("tools/list", {});
  assert.deepEqual(catalog.tools.map(tool => tool.name), ["js", "js_reset"]);
  assert.deepEqual(catalog.tools[0].inputSchema, CUA_PARAMETERS);
  assert.deepEqual(catalog.tools[1].inputSchema, CUA_RESET_PARAMETERS);
  const rustSchema = JSON.parse(await readFile(new URL("../../../crates/experimental/nanocodex-computer/src/js-schema.json", import.meta.url), "utf8"));
  assert.deepEqual(rustSchema, CUA_PARAMETERS);
  const adapter = createComputerTools({ executable, args: ["--fixture"] });
  t.after(adapter.close);
  assert.deepEqual(adapter.tools.map(tool => tool.name), [CUA_JS_NAME, CUA_RESET_NAME]);
  assert.deepEqual(adapter.tools.map(tool => tool.name), catalog.tools.map(tool => `mcp__cua_repl__${tool.name}`));

  const call = (name, args) => rpc("tools/call", { name, arguments: args });
  assert.equal((await call("js", { code: "let app = await cua.getApp('fixture://native');", title: "Select fixture" })).isError, false);
  const screenshot = await call("js", { code: "await nodeRepl.emitImage(await app.getScreenshot({emit:false}));", timeout_ms: 30_000 });
  const image = screenshot.content.find(item => item.type === "image");
  assert(image?.data.length > 0);
  assert.equal(image._meta["codex/imageDetail"], "original");
  assert.equal((await call("js_reset", {})).isError, false);
  const reset = await call("js", { code: "nodeRepl.write(typeof app);" });
  assert.match(JSON.stringify(reset.content), /undefined/);
});
