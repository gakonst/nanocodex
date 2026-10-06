import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

const root = fileURLToPath(new URL("..", import.meta.url));
// Actual managed shell, lazy interpreter, and Cloudflare execution. Only the
// caller-owned workspace is synthetic. The HTTP adapter invokes its public tool
// handler; this intentionally does not stand in for a whole model/session test.
const source = `
import { createManagedComputerRuntime } from './src/computer-runtime.ts';
import { traceToolInvocation } from './src/tool-tracing.ts';
const info = console.info.bind(console);
console.info = value => info(JSON.stringify(value));
export default { async fetch(request) {
  const data = new TextEncoder().encode('synthetic-private-sentinel\\n');
  const filesystem = {
    root: '/brain',
    async list() { return [{path:'/brain/private-file', kind:'file', size:data.length}]; },
    async readFile() { return data; },
    async writeFile() { throw new Error('read-only fixture'); },
    async mkdir() {}, async remove() {},
  };
  const runtime = await createManagedComputerRuntime({ filesystem,
    computer: { [Symbol.dispose]() {} },
    egress: { fetch() { throw new Error('unexpected network'); } },
  });
  try {
    const input = await request.json();
    const context = {
      callId: request.headers.get('x-call-id'), parentCallId: 'synthetic-parent',
      turnId: 'synthetic-host-turn', sessionId: 'synthetic-runtime', model: 'synthetic', signal: request.signal,
    };
    if (input.parallel) return Response.json(await Promise.all(['a', 'b'].map(label => {
      const scoped = { ...context, callId: 'synthetic-queued-' + label, turnId: 'synthetic-host-' + label };
      return traceToolInvocation('nanocodex.tool', 'synthetic-thread-' + label, 'exec_command', scoped,
        () => runtime.tool.handler({cmd: label === 'a' ? 'sleep 0.01; echo a' : 'echo b'}, scoped), 'synthetic-turn-' + label);
    })));
    return Response.json(await traceToolInvocation('nanocodex.tool', 'synthetic-thread', 'exec_command', context,
      () => runtime.tool.handler(input, context), 'synthetic-turn'));
  } catch { return Response.json({error:'tool_failed'}, {status:400}); }
  finally { runtime.dispose(); }
}};
`;

test("managed workerd shell emits private-safe correlated results across search retries", { timeout: 30_000 }, async (t) => {
  const output = join(root, "../../output/just-bash-telemetry", `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const records = [], raw = [], transcript = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], logLevel: "silent",
    alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
  });
  const capture = line => {
    raw.push(line);
    const start = line.indexOf('{"type":"managed.just_bash"');
    if (start >= 0) { try { records.push(JSON.parse(line.slice(start))); } catch {} }
  };
  const mf = new Miniflare({ port: 0, unsafeLocalExplorer: true, unsafeObservability: true,
    handleRuntimeStdio(stdout, stderr) {
      createInterface({ input: stdout }).on("line", capture);
      createInterface({ input: stderr }).on("line", capture);
    }, modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }],
    compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
  });
  try {
    for (const [id, input, exit, category] of [
      ["rg", {cmd:"rg -F absent private-file"}, 1, "command_exit"],
      ["sed", {cmd:"sed -n '1p' private-file"}, 0, "none"],
      ["grep", {cmd:"grep -F synthetic-private-sentinel private-file"}, 0, "none"],
      ["admission", {cmd:"cat private-file | rg '" + "x".repeat(9000) + "' private-file"}, 126, "search_admission"],
      ["invalid", {cmd:"echo synthetic-private-sentinel", tty:true}, null, "input_validation"],
      ["recovered", {cmd:"echo recovered"}, 0, "none"],
    ]) {
      const response = await mf.dispatchFetch("https://fixture.internal/exec", {
        method: "POST", headers: {"content-type":"application/json", "x-call-id":`synthetic-${id}`},
        body: JSON.stringify(input),
      });
      const result = await response.json();
      assert.equal(response.status, exit === null ? 400 : 200);
      if (exit !== null) assert.equal(result.exit_code, exit, JSON.stringify(result));
      transcript.push({call_id:`synthetic-${id}`, http_status:response.status, exit_code:exit, category});
    }
    const parallel = await mf.dispatchFetch("https://fixture.internal/exec", {
      method: "POST", headers: {"content-type":"application/json"}, body: JSON.stringify({parallel:true}),
    });
    assert.equal(parallel.status, 200);
    const queuedResults = await parallel.json();
    assert.deepEqual(queuedResults.map(r => [r.exit_code, r.output]), [[0, "a\n"], [0, "b\n"]]);
    for (const label of ["a", "b"]) transcript.push({call_id:`synthetic-queued-${label}`,
      exit_code:0, category:"none", thread_id:`synthetic-thread-${label}`,
      managed_turn_id:`synthetic-turn-${label}`, host_turn_id:`synthetic-host-${label}`});
    // workerd's log stream is asynchronous relative to HTTP completion.
    for (let n = 0; records.length < 24 && n < 100; n++) await new Promise(r => setTimeout(r, 10));
    assert.equal(records.length, 24, raw.join("\n"));
    for (const expected of transcript) {
      const events = records.filter(e => e.tool_call_id === expected.call_id);
      assert.deepEqual(events.map(e => e.phase), ["queued", "started", "finished"]);
      const event = events.at(-1);
      assert.equal(event.exit_code, expected.exit_code);
      assert.equal(event.category, expected.category);
      if (expected.category === "search_admission") {
        assert.equal(event.command, "cat");
        assert.equal(event.admission_command, "rg");
      }
      assert.equal(event.status, expected.exit_code === 0 ? "success" : "error");
      assert.equal(event.thread_id, expected.thread_id ?? "synthetic-thread");
      assert.equal(event.managed_turn_id, expected.managed_turn_id ?? "synthetic-turn");
      assert.equal(event.runtime_session_id, "synthetic-runtime");
      assert.equal(event.host_turn_id, expected.host_turn_id ?? "synthetic-host-turn");
      assert.equal(event.parent_call_id, "synthetic-parent");
    }
    assert.deepEqual(records.filter(e => e.phase === "finished").slice(0, 3).map(e => e.command), ["rg", "sed", "grep"]);
    for (const forbidden of ["synthetic-private-sentinel", "private-file", "/brain", "https://", "xxxxxxxxxx"])
      assert.equal(JSON.stringify(records).includes(forbidden), false, forbidden);
    for (const forbidden of ["synthetic-private-sentinel", "private-file", "xxxxxxxxxx"])
      assert.equal(raw.join("\n").includes(forbidden), false, forbidden);
  } finally {
    await mf.dispose();
    await writeFile(join(output, "events.json"), JSON.stringify(records, null, 2) + "\n");
    await writeFile(join(output, "transcript.json"), JSON.stringify(transcript, null, 2) + "\n");
    await writeFile(join(output, "workerd.log"), raw.join("\n") + "\n");
    t.diagnostic(`Evidence: ${output}`);
  }
});
