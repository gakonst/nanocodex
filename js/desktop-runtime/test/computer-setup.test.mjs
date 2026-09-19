import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { DesktopRuntime } from "../src/runtime.mjs";

for (const failSetup of [false, true]) {
  test(`desktop first start ${failSetup ? "surfaces provisioning failure" : "provisions before connecting and reuses installation"}`, { timeout: 10_000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), "desktop-cua-setup-"));
    const originalEnv = process.env;
    const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
    process.env = { HOME: root, PATH: "", NANOCODEX_DIR: join(root, "custom-root") };
    Object.defineProperty(process, "platform", { value: "darwin" });
    const binary = join(root, "native helper"), calls = join(root, "calls.jsonl");
    const machine = { id: "33333333-3333-4333-8333-333333333333", name: "Fixture Mac", workspace: root };
    await writeFile(binary, `#!${process.execPath}
      const fs = require('node:fs'), path = require('node:path');
      const args = process.argv.slice(2), machine = ${JSON.stringify(machine)};
      fs.appendFileSync(${JSON.stringify(calls)}, JSON.stringify(args) + '\\n');
      if (args[0] === 'computer') {
        if (${failSetup}) { console.error('fixture network failure'); process.exit(3); }
        const executable = path.join(process.env.NANOCODEX_DIR, 'runtimes/openai-cua/current/cua-provider');
        fs.mkdirSync(path.dirname(executable), { recursive: true }); fs.writeFileSync(executable, '', { mode: 0o755 });
        console.log(JSON.stringify({ status: 'installed', executable, transport: 'mcp' }));
      } else if (args.includes('--describe')) console.log(JSON.stringify(machine));
      else { console.log(JSON.stringify({ machine, status: 'connected' })); process.stdin.resume(); process.stdin.on('end', () => process.exit(0)); }
    `, { mode: 0o755 });
    const server = createServer((_request, response) => { response.setHeader("content-type", "application/json"); response.end('{"data":[]}'); });
    server.listen(0, "127.0.0.1"); await once(server, "listening");
    const runtime = new DesktopRuntime({ baseUrl: `http://127.0.0.1:${server.address().port}`, apiKey: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`, defaults: { deviceBinary: binary }, dataDirectory: root });
    t.after(async () => {
      await runtime.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
      process.env = originalEnv; Object.defineProperty(process, "platform", originalPlatform);
      await rm(root, { recursive: true, force: true });
    });
    await runtime.refresh();
    if (failSetup) {
      await assert.rejects(runtime.prepareDefaultHand(), /OpenAI CUA setup failed:.*[\s\S]*computer setup/);
      assert.equal(runtime.state().hands[0].status, "error");
    } else {
      assert.equal((await runtime.prepareDefaultHand()).status, "connected");
      await runtime.stopHand(machine.id);
      await runtime.startHand(machine.id);
      assert.equal(runtime.state().hands[0].status, "connected");
    }
    assert.deepEqual((await readFile(calls, "utf8")).trim().split("\n").map(JSON.parse), [
      ["__device-hand", "--describe"], ["computer", "setup"],
      ...(!failSetup ? [["__device-hand", "--parent-pipe"], ["__device-hand", "--parent-pipe"]] : []),
    ]);
  });
}
