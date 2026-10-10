import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { monitorEventLoopDelay } from "node:perf_hooks";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fetch } from "./support/miniflare-fetch.mjs";

// Large-result journey: real Node Hand exec_command -> reverse WebSocket ->
// AccountHostedTools broker + SQLite ledger in workerd -> namespace runtime ->
// public HTTP client. Run from js/managed with node --test. SOURCE_ROOT selects
// the implementation under measurement; LABEL names baseline/candidate runs.
const checkout = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const repo = resolve(process.env.NANOCODEX_BENCHMARK_SOURCE_ROOT ?? checkout);
const label = process.env.NANOCODEX_BENCHMARK_LABEL ?? "result-size";
assert.match(label, /^[A-Za-z0-9_.-]+$/);
const output = join(checkout, "output/hand-result-size-journey", label, `${Date.now()}-${process.pid}`);
const require = createRequire(join(repo, "js/managed/package.json"));
const { build } = require("esbuild");
const { Miniflare } = require("miniflare");
const { WebSocket } = require("ws");
const { createTools } = await import(pathToFileURL(join(repo, "js/nanocodex/tools/Tools.mjs")));
const { createNodeProcessTools } = await import(pathToFileURL(join(repo, "js/nanocodex-tools/tools/nodeProcess.mjs")));
const owner = "00000000-0000-4000-8000-000000000003";
const thread = "00000000-0000-7000-8000-000000000001";
const KiB = 1024, MiB = 1024 * KiB;
const scale = Number(process.env.NANOCODEX_RESULT_SIZE_SAMPLES_SCALE ?? 1);
// ASCII sweeps measure cost; variants prove exact bytes for multibyte, JSON
// escapes, control characters and embedded JSON text through every hop.
const plans = [
  // Exec results carry the text twice (output + structuredResult.output) and the
  // broker ledger row is bounded by the Durable Object SQLite 2 MB string/row
  // limit, so the largest passing exec output is just under 1 MiB.
  { name: "ascii-1KiB", bytes: KiB, samples: 200, kind: "ascii" },
  { name: "ascii-64KiB", bytes: 64 * KiB, samples: 200, kind: "ascii" },
  { name: "ascii-512KiB", bytes: 512 * KiB, samples: 60, kind: "ascii" },
  { name: "ascii-960KiB", bytes: 960 * KiB, samples: 40, kind: "ascii" },
  { name: "mixed-64KiB", bytes: 64 * KiB, samples: 60, kind: "mixed" },
  { name: "mixed-384KiB", bytes: 384 * KiB, samples: 30, kind: "mixed" },
]
function payload(kind, bytes) {
  const unit = kind === "ascii" ? "abcdefghijklmnopqrstuvwxyz012345\n"
    : 'é中😀 "quote" \\back\\slash\t\u0001{"nested":{"json":[1,"two",null]}}\r\n';
  const encoded = Buffer.from(unit, "utf8");
  const out = Buffer.alloc(bytes);
  for (let offset = 0; offset < bytes; offset += encoded.length) encoded.copy(out, offset, 0, Math.min(encoded.length, bytes - offset));
  // Never end inside a multibyte sequence; pad the tail with ASCII.
  let end = bytes;
  while (end > 0 && (out[end - 1] & 0xc0) === 0x80) end--;
  if (end > 0 && out[end - 1] >= 0xc0) end--;
  out.fill(0x2e, end);
  return out;
}
function distribution(values) {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!sorted.length) return { count: 0 };
  const quantile = q => sorted[Math.max(0, Math.ceil(sorted.length * q) - 1)];
  return { count: sorted.length, min: sorted[0], p50: quantile(.5), p95: quantile(.95), max: sorted.at(-1),
    mean: sorted.reduce((a, b) => a + b, 0) / sorted.length };
}
async function descendants(pid) {
  const all = [];
  for (const entry of await readdir("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    const stat = await readFile(`/proc/${entry}/stat`, "utf8").catch(() => "");
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
    if (stat) all.push({ pid: Number(entry), ppid: Number(fields[1]), comm: stat.slice(stat.indexOf("(") + 1, stat.lastIndexOf(")")) });
  }
  const found = [], frontier = [pid];
  while (frontier.length) {
    const parent = frontier.pop();
    for (const row of all) if (row.ppid === parent) { found.push(row); frontier.push(row.pid); }
  }
  return found;
}
async function processSample(pid) {
  const stat = await readFile(`/proc/${pid}/stat`, "utf8");
  const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
  const status = await readFile(`/proc/${pid}/status`, "utf8");
  const kib = name => Number(status.match(new RegExp(`^${name}:\\s+(\\d+) kB`, "m"))?.[1]);
  return { cpu_ticks: Number(fields[11]) + Number(fields[12]), rss_kib: kib("VmRSS"), hwm_kib: kib("VmHWM") };
}
async function resetPeak(pid) { await writeFile(`/proc/${pid}/clear_refs`, "5").catch(() => {}); }

test("large Hand results keep exact bytes and measured cost over the real Brain–Hand path", { timeout: 1_800_000 }, async () => {
  await mkdir(join(output, "hand"), { recursive: true });
  const observations = [], wire = [];
  let attachment, mf, native, tools, base;
  const originalInfo = console.info;
  console.info = (record, ...rest) => { if (record?.type !== "hand.attachment") originalInfo(record, ...rest); };
  const capture = line => {
    const start = line.indexOf('{"type":"hand.call.broker"');
    if (start >= 0) { try { const row = JSON.parse(line.slice(start)); if (row.stage === "receipt") observations.push(row); } catch {} }
  };
  async function request(body) {
    const started = performance.now();
    const response = await fetch(new URL("/namespace", base), { method: "POST",
      headers: { authorization: "Bearer fixture", "content-type": "application/json" }, body: JSON.stringify(body) });
    const text = await response.text();
    const client_total_ms = performance.now() - started;
    return { status: response.status, value: JSON.parse(text), client_total_ms };
  }
  try {
    const sourceCommit = execFileSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    const source = `
import {DurableObject} from 'cloudflare:workers';
import {AccountHostedTools,AccountHostedToolsProvider} from './src/account-hosted-tools.ts';
import {createNamespaceExecutionRuntime} from './src/namespace-tools.ts';
export {AccountHostedTools};
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class Namespace extends DurableObject {
 constructor(ctx,env){super(ctx,env);
  this.provider=new AccountHostedToolsProvider(env.HANDS,'${owner}',()=>true,'${thread}');
  this.runtime=createNamespaceExecutionRuntime(c=>this.provider.machines(c),(id,name,c)=>this.provider.machineTool(id,name,c),undefined,undefined,()=> 'synthetic-authority',undefined,'${thread}');
 }
 async fetch(request){
  if(request.headers.get('authorization')!=='Bearer fixture')return Response.json({error:'unauthorized'},{status:401});
  const body=await request.json();
  await this.provider.refreshOptional(1000);
  const context={sessionId:'fixture-session',parentCallId:'fixture-cell',callId:body.call_id,model:'fixture-model',turnId:'fixture-turn',signal:request.signal};
  try{return Response.json({result:await this.runtime.tools[body.name].handler(body.input,context)});}
  catch(error){return Response.json({error:error.message},{status:500});}
 }
}
export default {fetch(request,env){const path=new URL(request.url).pathname;
 if(path==='/namespace')return env.NAMESPACE.getByName('fixture').fetch(request);
 return env.HANDS.getByName('${owner}').fetch(new Request('https://hand.internal'+path+new URL(request.url).search,request));}};
`;
    const bundle = await build({ stdin: { contents: source, resolveDir: join(repo, "js/managed") },
      bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
      external: ["cloudflare:*", "node:*"], alias: {
        "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
        "nanocodex-tools/internal/hosted-machine": join(repo, "js/nanocodex-tools/tools/hostedMachine.mjs"),
        "nanocodex-tools": join(repo, "js/nanocodex-tools/src/index.ts"),
        "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
      }, logLevel: "silent" });
    const sourceFiles = ["js/nanocodex-tools/src/hosted/broker-core.ts", "js/nanocodex-tools/src/hosted/protocol.ts",
      "js/nanocodex-tools/tools/attachment.mjs", "js/managed/src/account-hosted-tools.ts", "js/managed/src/hosted-tools-broker.ts"];
    const hashes = Object.fromEntries(await Promise.all(sourceFiles.map(async path =>
      [path, createHash("sha256").update(await readFile(join(repo, path))).digest("hex")])));
    mf = new Miniflare({ name: "benchmark", port: 0, modules: true, script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      durableObjects: { HANDS: { className: "AccountHostedTools", useSQLite: true }, NAMESPACE: { className: "Namespace", useSQLite: true } },
      handleRuntimeStdio(stdout, stderr) {
        for (const stream of [stdout, stderr]) {
          let pending = "";
          stream.on("data", chunk => { pending += chunk; const lines = pending.split("\n"); pending = lines.pop(); lines.forEach(capture); });
        }
      } });
    native = await createNodeProcessTools({ workspace: join(output, "hand") });
    tools = await createTools({ attachmentId: "fixture-hand", machines: [{ id: "fixture-hand", name: "Synthetic Hand",
      workspace: join(output, "hand"), capabilities: ["shell"] }],
      tools: Object.fromEntries(native.tools.map(tool => [tool.name, tool])) });
    base = String(await mf.ready);
    const workerd = (await descendants(process.pid)).filter(row => row.comm.startsWith("workerd"));
    assert.ok(workerd.length >= 1, "workerd runtime process must be observable");
    attachment = tools.attach({ endpoint: new URL("/tool-host", base).href.replace(/^http/, "ws"), transport: { async connect() {
      const socket = new WebSocket(new URL("/tool-host", base).href.replace(/^http/, "ws"), { headers: { "x-nanocodex-owner-id": owner },
        maxPayload: 256 * MiB });
      const send = socket.send.bind(socket);
      socket.send = (...args) => {
        const text = String(args[0]);
        if (text.startsWith('{"type":"result"')) {
          const at = text.lastIndexOf(',"timing":');
          try { wire.push({ bytes: Buffer.byteLength(text), timing: JSON.parse(text.slice(at + 10, -1)) }); } catch {}
        }
        return send(...args);
      };
      return socket;
    } } });
    await attachment.connect();
    const results = {};
    let sequence = 0;
    for (const plan of plans) {
      const content = payload(plan.kind, plan.bytes);
      const file = `payload-${plan.name}.txt`;
      await writeFile(join(output, "hand", file), content);
      const expected = content.toString("utf8");
      const samples = [];
      const run = async (warm) => {
        const callId = `${plan.name}-${sequence++}`;
        const wireBefore = wire.length;
        const result = await request({ name: "exec_command", call_id: callId, input: { workdir: "/fixture-hand", shell: "/bin/sh",
          login: false, yield_time_ms: 30000, cmd: `cat ${file}` } });
        assert.equal(result.status, 200, JSON.stringify(result.value).slice(0, 500));
        assert.equal(result.value.result.success, true, JSON.stringify(result.value).slice(0, 800));
        // Exact UTF-8 bytes survive Hand encoding, broker ledger, and both HTTP hops.
        assert.equal(result.value.result.structuredResult.exit_code, 0);
        assert.ok(result.value.result.structuredResult.output === expected, `${callId} structured output differs`);
        if (warm) return;
        const frame = wire.slice(wireBefore).at(-1);
        samples.push({ call_id: callId, client_total_ms: result.client_total_ms, result_frame_bytes: frame?.bytes,
          hand_result_encode_ms: frame?.timing.result_encode_ms, hand_host_elapsed_ms: frame?.timing.host_elapsed_ms,
          hand_execution_ms: frame?.timing.execution_ms });
      };
      for (let index = 0; index < 3; index++) await run(true);
      const count = Math.max(3, Math.round(plan.samples * scale));
      for (const proc of workerd) await resetPeak(proc.pid);
      const before = await Promise.all(workerd.map(proc => processSample(proc.pid)));
      const nodeCpuBefore = process.cpuUsage();
      const loop = monitorEventLoopDelay({ resolution: 1 }); loop.enable();
      let nodePeak = process.memoryUsage().rss;
      const started = performance.now();
      for (let index = 0; index < count; index++) { await run(false); nodePeak = Math.max(nodePeak, process.memoryUsage().rss); }
      const elapsed = performance.now() - started;
      loop.disable();
      const after = await Promise.all(workerd.map(proc => processSample(proc.pid)));
      const nodeCpu = process.cpuUsage(nodeCpuBefore);
      const ticks = after.reduce((sum, row, index) => sum + row.cpu_ticks - before[index].cpu_ticks, 0);
      results[plan.name] = { bytes: plan.bytes, kind: plan.kind, samples: count, elapsed_ms: elapsed,
        workerd_cpu_ms_per_call: ticks * 10 / count, workerd_peak_rss_mib: Math.max(...after.map(row => row.hwm_kib)) / 1024,
        node_cpu_ms_per_call: (nodeCpu.user + nodeCpu.system) / 1000 / count, node_peak_rss_mib: nodePeak / MiB,
        node_event_loop_delay_ms: { p50: loop.percentile(50) / 1e6, p99: loop.percentile(99) / 1e6, max: loop.max / 1e6 },
        client_total_ms: distribution(samples.map(row => row.client_total_ms)),
        hand_result_encode_ms: distribution(samples.map(row => row.hand_result_encode_ms)),
        hand_host_elapsed_ms: distribution(samples.map(row => row.hand_host_elapsed_ms)),
        result_frame_bytes: samples[0]?.result_frame_bytes };
      await writeFile(join(output, `samples-${plan.name}.json`), JSON.stringify(samples, null, 2) + "\n");
      originalInfo(JSON.stringify({ plan: plan.name, client_p50: results[plan.name].client_total_ms.p50,
        encode_p50: results[plan.name].hand_result_encode_ms.p50, workerd_cpu_ms_per_call: results[plan.name].workerd_cpu_ms_per_call }));
    }
    const summary = { outcome: "passed", label, source_root: repo, source_commit: sourceCommit, source_sha256: hashes,
      source_dirty: execFileSync("git", ["-C", repo, "status", "--short", "--", ...sourceFiles], { encoding: "utf8" }).trim(),
      node: process.version, workerd_processes: workerd.map(row => row.comm), broker_receipts: observations.length,
      clocks: "client and Hand use Node monotonic clocks; workerd CPU comes from /proc utime+stime (10 ms ticks) over each batch; DO-internal timers are coarse and excluded",
      command: `NANOCODEX_BENCHMARK_SOURCE_ROOT=${repo} NANOCODEX_BENCHMARK_LABEL=${label} node --test test/hand-result-size-journey.test.mjs`,
      results, evidence: output };
    await writeFile(join(output, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
    originalInfo(JSON.stringify({ outcome: "passed", label, evidence: output }));
  } finally {
    console.info = originalInfo;
    if (attachment) await attachment.close().catch(() => {});
    await tools?.close(); await native?.close(); await mf?.dispose();
  }
});
