// Real workerd fixture for screen playback journeys. Only external account
// enrollment is synthetic: API keys are issued by the production account-auth
// code. Public HTTP enters the production account proxy, then the production
// managed Worker, AccountHostedTools broker and ScreenPlayback Durable Objects.
//
//   node test/screen-playback-fixture.mjs -- <command> [args...]
// starts the fixture (production routing only; missing wiring fails), publishes one idle synthetic playback Hand, runs the
// command with SCREEN_PLAYBACK_TEST_ORIGIN, SCREEN_PLAYBACK_TEST_TOKEN,
// NANOCODEX_MANAGED_URL and NANOCODEX_API_KEY (synthetic key, process env only,
// never written to disk), saves a redacted transcript under output/ and exits
// with the command status.
import { fork, spawn, execFile } from "node:child_process";
import { mkdir, readFile, readdir, writeFile, appendFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import WebSocket from "ws";
import { fetch } from "./support/miniflare-fetch.mjs";

export const root = fileURLToPath(new URL("..", import.meta.url));
export const ownerA = "22222222-2222-4222-8222-222222222201";
export const ownerB = "22222222-2222-4222-8222-222222222202";
export const fullCaps = ["agents:read", "agents:write", "tools:use"];
const secrets = new Set();
/** Redact every synthetic credential and stream token before evidence is written. */
export function redact(value) {
  let text = typeof value === "string" ? value : JSON.stringify(value);
  for (const secret of secrets) if (secret) text = text.split(secret).join("[redacted]");
  return text.replace(/nsv_[A-Za-z0-9_-]{43}/g, "nsv_[redacted]").replace(/nsu_[A-Za-z0-9_-]{43}/g, "nsu_[redacted]");
}
export function remember(secret) { if (secret) secrets.add(secret); return secret; }

const managedSource = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from './src/account-auth.ts';
import { ScreenPlayback } from './src/index.ts';
export { DurableAgentSession, AccountHostedTools, UserAccount, Organization, ApiKeyRecord, NonceStorage };
/** Production class; the fixture adds only an eviction trigger (ctx.abort drops RAM, keeps storage). */
globalThis.__fixtureDelays ??= {};
export class FixtureScreenPlayback extends ScreenPlayback {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/__fixture/abort') this.ctx.abort('fixture eviction');
    // Race injection only: hold a create step so a revoke can land inside it.
    const hold = globalThis.__fixtureDelays[path];
    if (hold) { delete globalThis.__fixtureDelays[path]; await new Promise(resolve => setTimeout(resolve, hold)); }
    return super.fetch(request);
  }
}
export class FixtureSandbox extends DurableObject { async clearRemoteDesktop() {} async destroy() {} }
export class FixtureModel extends DurableObject { async fetch() { return Response.json({ error: 'unavailable' }, { status: 503 }); } }
export default { async fetch(request, env, ctx) {
  const url = new URL(request.url);
  if (url.hostname === 'fixture.test' && url.pathname === '/__fixture/enroll') {
    const b = await request.json(); await ensureAccount(env, b.user, true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env, { kind: 'api_key', userId: b.user, ...auth.grant, subjectId: 'api_key:' + b.user,
      credentialId: 'fixture', capabilities: b.capabilities }, 'Synthetic playback journey');
    return Response.json({ token: key.token });
  }
  if (url.hostname === 'fixture.test' && url.pathname === '/__fixture/delay') {
    const b = await request.json(); globalThis.__fixtureDelays[b.path] = b.ms; return Response.json({ ok: true });
  }
  if (url.hostname === 'fixture.test' && url.pathname === '/__fixture/evict') {
    const b = await request.json();
    try { await env.NANOCODEX_SCREEN_PLAYBACK.getByName('stream:' + b.id).fetch('https://screen-playback.internal/__fixture/abort', { method: 'POST', body: '{}' }); }
    catch (error) { return Response.json({ evicted: true, error: String(error).slice(0, 120) }); }
    return Response.json({ evicted: false });
  }
  return worker.fetch(request, env, ctx);
} };
`;

const proxyEntry = `import { routeManaged } from '../account/worker/managedProxy.ts';
export default { async fetch(request, env) {
  const routed = await routeManaged(request, env, new URL(request.url));
  if (routed) return routed;
  return new Response(null, { status: 404 });
} };`;

const childSource = `
import { Miniflare } from ${JSON.stringify(import.meta.resolve("miniflare"))};
import { readFile } from 'node:fs/promises';
const dir = process.argv[2], port = Number(process.argv[3]);
const common = { compatibilityDate: '2026-07-30', compatibilityFlags: ['nodejs_compat', 'enable_request_signal'] };
try {
  const manifest = JSON.parse(await readFile(dir + '/manifest.json', 'utf8'));
  const assets = await Promise.all(manifest.assets.map(async name => ({ type: 'CompiledWasm', path: name, contents: await readFile(dir + '/' + name) })));
  const mf = new Miniflare({ port, host: '127.0.0.1', durableObjectsPersist: dir + '/sqlite',
    handleRuntimeStdio(stdout, stderr) { stdout.pipe(process.stdout); stderr.pipe(process.stderr); }, workers: [
    { ...common, name: 'account', modules: true, script: await readFile(dir + '/proxy.mjs', 'utf8'), serviceBindings: { NANOCODEX_BACKEND: 'managed' } },
    { ...common, name: 'managed', modules: [{ type: 'ESModule', path: 'worker.mjs', contents: await readFile(dir + '/worker.mjs', 'utf8') }, ...assets],
      durableObjects: {
        NANOCODEX_SESSIONS: { className: 'DurableAgentSession', useSQLite: true },
        NANOCODEX_USERS: { className: 'UserAccount', useSQLite: true },
        NANOCODEX_ORGANIZATIONS: { className: 'Organization', useSQLite: true },
        NANOCODEX_API_KEYS: { className: 'ApiKeyRecord', useSQLite: true },
        NANOCODEX_AUTH: { className: 'NonceStorage', useSQLite: true },
        NANOCODEX_SANDBOXES: { className: 'FixtureSandbox', useSQLite: true },
        NANOCODEX_ACCOUNT_TOOLS: { className: 'AccountHostedTools', useSQLite: true },
        NANOCODEX_SCREEN_PLAYBACK: { className: 'FixtureScreenPlayback', useSQLite: true },
        NANOCODEX_MEMORY: { className: 'FixtureModel', useSQLite: true },
        MODEL: { className: 'FixtureModel', useSQLite: true },
      }, serviceBindings: { NANOCODEX: 'provider' },
      r2Buckets: ['NANOCODEX_HISTORY', 'NANOCODEX_WORKSPACES', 'NANOCODEX_USER_DATA_OBJECTS'] },
    { ...common, name: 'provider', modules: true, script: 'export default {fetch(){return new Response(null,{status:503})}};' },
  ] });
  const base = await mf.ready;
  const managed = await mf.getWorker('managed');
  process.on('message', async message => {
    try {
      const response = await managed.fetch('https://fixture.test' + message.path, { method: 'POST', body: JSON.stringify(message.body ?? {}) });
      process.send({ id: message.id, status: response.status, body: await response.text() });
    } catch (error) { process.send({ id: message.id, status: 0, body: String(error) }); }
  });
  process.send({ ready: true, base: base.href });
} catch (error) { process.send({ error: String(error && error.stack || error) }); process.exitCode = 1; }
`;

async function bounded(promise, description, ms = 30_000) {
  const timer = new AbortController();
  try {
    return await Promise.race([promise, delay(ms, undefined, { signal: timer.signal }).then(() => { throw Error(`${description} exceeded ${ms}ms`); })]);
  } finally { timer.abort(); }
}

/** Bundle production sources and run workerd in a killable child with persistent SQLite. */
export async function startPlaybackFixture(output) {
  await mkdir(output, { recursive: true });
  const assets = [];
  const bundle = await build({ stdin: { contents: managedSource, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = join(args.resolveDir, args.path), contents = await readFile(path);
      const name = `fixture-${assets.length}.wasm`;
      assets.push(name); await writeFile(join(output, name), contents);
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent" });
  const proxy = await build({ stdin: { contents: proxyEntry, resolveDir: root }, bundle: true, write: false, format: "esm",
    platform: "node", conditions: ["workerd"], target: "es2022", external: ["cloudflare:*", "node:*"], logLevel: "silent" });
  await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
  await writeFile(join(output, "proxy.mjs"), proxy.outputFiles[0].text);
  await writeFile(join(output, "manifest.json"), JSON.stringify({ assets }, null, 2));
  await writeFile(join(output, "runtime-process.mjs"), childSource);
  const runtimeLog = join(output, "runtime.log");
  let child, port = 0, base, sequence = 0;
  const waiting = new Map();
  const fixture = {
    output,
    get base() { return base; },
    async start() {
      const current = fork(join(output, "runtime-process.mjs"), [output, String(port)], { detached: true, stdio: ["ignore", "pipe", "pipe", "ipc"] });
      child = current;
      for (const stream of [current.stdout, current.stderr]) stream.on("data", chunk => { void appendFile(runtimeLog, redact(String(chunk))); });
      const ready = await bounded(new Promise((resolve, reject) => {
        current.once("exit", (code, signal) => reject(Error(`runtime exited before ready: ${code}/${signal}`)));
        current.on("message", message => {
          if (message.ready) resolve(message); else if (message.error) reject(Error(message.error));
          else if (waiting.has(message.id)) { waiting.get(message.id)(message); waiting.delete(message.id); }
        });
      }), "runtime readiness", 60_000);
      base = new URL(ready.base);
      if (port) { if (Number(base.port) !== port) throw Error("restart changed port"); } else port = Number(base.port);
      return base;
    },
    /** SIGKILL the whole workerd process group: every DO loses RAM; SQLite persists. */
    async kill() {
      const current = child; if (!current) return;
      const exited = new Promise(resolve => current.once("exit", (code, signal) => resolve(signal)));
      process.kill(-current.pid, "SIGKILL");
      const signal = await bounded(exited, "SIGKILL exit"); child = undefined;
      return signal;
    },
    async internal(path, body) {
      const id = ++sequence;
      const reply = new Promise(resolve => waiting.set(id, resolve));
      child.send({ id, path, body });
      const message = await bounded(reply, `fixture ${path}`);
      return { status: message.status, body: message.body ? JSON.parse(message.body) : undefined };
    },
    async enroll(user, capabilities = fullCaps) {
      const response = await fixture.internal("/__fixture/enroll", { user, capabilities });
      if (response.status !== 200) throw Error(`enroll failed ${response.status}`);
      return remember(response.body.token);
    },
    /** Evict one stream DO instance (RAM lost, storage kept) via ctx.abort inside the production class. */
    async evict(id) { return (await fixture.internal("/__fixture/evict", { id })).body; },
    /** Delay the next internal ScreenPlayback request to `path` (e.g. /stream/init, /stream/activate). */
    async delayNext(path, ms) { return (await fixture.internal("/__fixture/delay", { path, ms })).body; },
    async stop() { await fixture.kill(); },
  };
  await fixture.start();
  return fixture;
}

/** Encode real H.264/AAC MPEG-TS segments (1 s, keyframe-aligned) with ffmpeg. */
export async function makeSegments(directory, seconds = 30) {
  await mkdir(directory, { recursive: true });
  await promisify(execFile)("ffmpeg", ["-hide_banner", "-loglevel", "error", "-y",
    "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=15", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
    "-t", String(seconds), "-c:v", "libx264", "-preset", "ultrafast", "-g", "15", "-keyint_min", "15", "-sc_threshold", "0",
    "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "64k", "-f", "hls", "-hls_time", "1", "-hls_list_size", "0",
    "-hls_segment_filename", join(directory, "g%d.ts"), join(directory, "g.m3u8")]);
  const playlist = await readFile(join(directory, "g.m3u8"), "utf8");
  const durations = [...playlist.matchAll(/#EXTINF:([0-9.]+),/g)].map(match => Number(match[1]).toFixed(3));
  return Promise.all(durations.map(async (duration, index) => ({ duration, bytes: await readFile(join(directory, `g${index}.ts`)) })));
}

/**
 * A synthetic Hand speaking the production host protocol: authenticated host
 * WebSocket, catalog with `playback`, lease renewal, broker HLS commands,
 * and the upload contract including 409 missing_segments recovery.
 */
export class SyntheticHost {
  constructor({ fixture, token, machineId, surfaceId = "display-1", playback = true, segments, intervalMs = 400, mode = "normal" }) {
    Object.assign(this, { fixture, token, machineId, surfaceId, playback, segments, intervalMs, mode });
    this.events = []; this.starts = []; this.stops = 0; this.window = []; this.next = 0; this.refills = 0;
    this.active = false; this.reconnect = true; this.generations = [];
  }
  log(event) { this.events.push({ at: Date.now(), machine: this.machineId, ...event }); }
  headers() { return { authorization: `Bearer ${this.token}` }; }
  async connect() {
    const base = this.fixture.base;
    const socket = new WebSocket(new URL("/v1/account/hands/host", base.href.replace(/^http/, "ws")), { headers: this.headers() });
    this.socket = socket;
    const ready = await bounded(new Promise((resolve, reject) => {
      socket.once("error", reject);
      socket.once("unexpected-response", (_, response) => reject(Error(`host upgrade ${response.statusCode}`)));
      socket.on("message", data => {
        const message = JSON.parse(String(data));
        if (message.type === "ready") resolve(message); else void this.onMessage(message);
      });
    }), "host ready");
    Object.assign(this, { connectionId: ready.connection_id, generation: ready.generation });
    this.generations.push(ready.generation);
    socket.send(JSON.stringify({ type: "catalog", machine_id: this.machineId, machine_name: `Synthetic ${this.machineId}`,
      surfaces: [{ id: this.surfaceId, name: "Synthetic display", kind: "desktop", width: 1280, height: 720, controllable: false,
        ...(this.playback ? { playback: true } : {}) }] }));
    socket.on("close", () => { this.log({ type: "host_socket_closed" }); if (this.reconnect) void this.reconnectLoop(); });
    clearInterval(this.renewer);
    this.renewer = setInterval(() => void fetch(new URL("/v1/account/hands/renew", base), { method: "POST",
      headers: { ...this.headers(), "content-type": "application/json" }, body: JSON.stringify({ connection_id: this.connectionId }) })
      .then(response => response.body?.cancel()).catch(() => undefined), 8_000);
    await this.waitPublished();
    this.log({ type: "published", generation: this.generation });
  }
  async reconnectLoop() {
    if (this.reconnecting) return; this.reconnecting = true;
    try {
      for (let attempt = 0; attempt < 200 && this.reconnect; attempt += 1) {
        await delay(250);
        try { await this.connect(); return; } catch { /* runtime still restarting */ }
      }
    } finally { this.reconnecting = false; }
  }
  async waitPublished() {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const response = await fetch(new URL("/v1/account/hands/screens", this.fixture.base), { headers: this.headers() }).catch(() => undefined);
      const body = response?.ok ? await response.json() : undefined;
      if (body?.surfaces?.some(surface => surface.machine_id === this.machineId && surface.generation === this.generation)) return;
      await delay(50);
    }
    throw Error(`catalog for ${this.machineId} never published`);
  }
  send(value) { try { this.socket.send(JSON.stringify(value)); } catch { this.log({ type: "send_failed" }); } }
  result(stream_id, request_id, status, error) {
    this.send({ type: "broadcast_result", target: "hls", request_id, stream_id, status, ...(error ? { error } : {}) });
    this.log({ type: "result", stream_id, status, ...(error ? { error } : {}) });
  }
  async onMessage(message) {
    if (message.type !== "broadcast" || message.target !== "hls") return;
    if (message.action === "start") {
      const upload = new URL(message.upload.url);
      remember(message.upload.token);
      this.starts.push({ stream_id: message.stream_id, preset: message.preset, upload_origin: upload.origin, upload_path: upload.pathname });
      this.log({ type: "start", stream_id: message.stream_id, preset: message.preset });
      if (this.mode === "busy") { this.result(message.stream_id, message.request_id, "failed", "busy"); return; }
      if (this.active) { this.result(message.stream_id, message.request_id, "failed", "busy"); return; }
      Object.assign(this, { active: true, streamId: message.stream_id, upload: { url: message.upload.url, token: message.upload.token },
        requestId: message.request_id, live: false, window: [] });
      this.result(message.stream_id, message.request_id, "starting");
      void this.pump();
    } else if (message.action === "stop") {
      this.stops += 1; this.log({ type: "stop", stream_id: message.stream_id });
      if (this.active && message.stream_id === this.streamId) { this.active = false; this.result(message.stream_id, message.request_id, "stopped"); }
    }
  }
  put(file, body, contentType) {
    return fetch(new URL(file, this.upload.url), { method: "PUT", redirect: "manual",
      headers: { authorization: `Bearer ${this.upload.token}`, "content-type": contentType }, body, signal: AbortSignal.timeout(10_000) });
  }
  playlistText() {
    const lines = ["#EXTM3U", "#EXT-X-VERSION:3", "#EXT-X-TARGETDURATION:2", `#EXT-X-MEDIA-SEQUENCE:${this.window[0].n}`];
    for (const entry of this.window) { if (entry.discontinuity) lines.push("#EXT-X-DISCONTINUITY"); lines.push(`#EXTINF:${entry.duration},`, `s${entry.n}.ts`); }
    return lines.join("\n") + "\n";
  }
  terminal(status) {
    if (![401, 403, 404, 410].includes(status)) return false;
    this.active = false; this.log({ type: "uploader_stopped", status });
    this.result(this.streamId, this.requestId, "failed", status === 410 ? "expired" : "upload_rejected");
    return true;
  }
  async step() {
    const n = this.next++, source = this.segments[n % this.segments.length];
    const entry = { n, duration: source.duration, bytes: source.bytes, discontinuity: n > 0 && n % this.segments.length === 0 };
    const segment = await this.put(`s${n}.ts`, entry.bytes, "video/mp2t");
    await segment.body?.cancel();
    if (this.terminal(segment.status)) return;
    this.window.push(entry); if (this.window.length > 6) this.window.shift();
    let playlist = await this.put("index.m3u8", this.playlistText(), "application/vnd.apple.mpegurl");
    if (playlist.status === 409) {
      const body = await playlist.json();
      if (body.error === "missing_segments") {
        // The server lost its RAM window (eviction/restart): re-upload the listed segments, then the playlist.
        this.log({ type: "missing_segments", missing: body.missing });
        for (const missing of body.missing) {
          const held = this.window.find(candidate => candidate.n === missing);
          if (!held) continue;
          const refill = await this.put(`s${missing}.ts`, held.bytes, "video/mp2t"); await refill.body?.cancel();
          if (this.terminal(refill.status)) return;
          this.refills += 1;
        }
        playlist = await this.put("index.m3u8", this.playlistText(), "application/vnd.apple.mpegurl");
      }
    }
    await playlist.body?.cancel();
    if (this.terminal(playlist.status)) return;
    this.log({ type: "uploaded", n, segment: segment.status, playlist: playlist.status });
    if (playlist.status === 204 && !this.live) { this.live = true; this.result(this.streamId, this.requestId, "live"); }
  }
  async pump() {
    while (this.active) {
      try { await this.step(); } catch (error) { this.log({ type: "upload_error", error: String(error).slice(0, 80) }); }
      await delay(this.intervalMs);
    }
  }
  close() { this.reconnect = false; this.active = false; clearInterval(this.renewer); try { this.socket?.close(); } catch {} }
}

/** Run an external client (shipped CLI, Swift HTTP journey) against a running fixture; credentials only in its env. */
export async function runClient(fixture, token, command, args, output, name, extraEnv = {}) {
  const started = Date.now();
  const child = spawn(command, args, { cwd: join(root, "../.."), env: { ...process.env, ...extraEnv,
    SCREEN_PLAYBACK_TEST_ORIGIN: fixture.base.origin, SCREEN_PLAYBACK_TEST_TOKEN: token,
    NANOCODEX_MANAGED_URL: fixture.base.origin, NANOCODEX_API_KEY: token }, stdio: ["ignore", "pipe", "pipe"] });
  let transcript = "";
  child.stdout.on("data", chunk => { transcript += chunk; });
  child.stderr.on("data", chunk => { transcript += chunk; });
  const code = await new Promise(resolve => child.once("exit", resolve));
  await writeFile(join(output, `${name}.log`), redact(`$ ${command} ${args.join(" ")}\n${transcript}\nexit=${code}\n`));
  return { name, command: `${command} ${args.join(" ")}`, code, ms: Date.now() - started };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const separator = process.argv.indexOf("--");
  const [command, ...args] = separator >= 0 ? process.argv.slice(separator + 1) : [];
  const output = join(root, "../../output/screen-playback-fixture", `${Date.now()}-${process.pid}`);
  const fixture = await startPlaybackFixture(output);
  const token = await fixture.enroll(ownerA);
  const host = new SyntheticHost({ fixture, token, machineId: "synthetic-playback-hand", segments: await makeSegments(join(output, "media"), 20) });
  await host.connect();
  const summary = { output, origin: fixture.base.origin, machine_id: host.machineId, surface_id: host.surfaceId };
  let code = 0;
  try {
    if (command) {
      const result = await runClient(fixture, token, command, args, output, "client");
      code = result.code ?? 1; Object.assign(summary, { client: result });
    } else {
      // Interactive mode: origin is printed; the synthetic key stays in this process only.
      console.log(JSON.stringify({ ...summary, note: "fixture running; Ctrl-C to stop" }));
      await new Promise(resolve => process.once("SIGINT", resolve));
    }
  } finally {
    Object.assign(summary, { host_events: host.events, starts: host.starts });
    await writeFile(join(output, "summary.json"), redact(JSON.stringify(summary, null, 2)));
    host.close(); await fixture.stop();
  }
  console.log(redact(JSON.stringify({ output, client: summary.client })));
  process.exit(code);
}
