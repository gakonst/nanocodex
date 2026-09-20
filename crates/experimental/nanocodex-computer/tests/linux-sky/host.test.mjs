import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { startSkyHost, createSession, frame, validateRequest } from '../../src/linux_sky_host.mjs';
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function client(socketPath) {
  const socket = net.createConnection(socketPath);
  await once(socket, 'connect');
  let buffer = Buffer.alloc(0), next = 1;
  const pending = new Map();
  socket.on('error', () => {});
  socket.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= buffer.readUInt32LE() + 4) {
      const length = buffer.readUInt32LE();
      const result = JSON.parse(buffer.subarray(4, 4 + length)); buffer = buffer.subarray(4 + length);
      pending.get(result.id)?.(result); pending.delete(result.id);
    }
  });
  return { socket, call(request, split = false) {
    const id = next++;
    const result = new Promise(resolve => pending.set(id, resolve));
    const bytes = frame({ id, request });
    if (split) { socket.write(bytes.subarray(0, 2)); socket.write(bytes.subarray(2, 7)); socket.write(bytes.subarray(7)); }
    else socket.write(bytes);
    return result;
  } };
}
test('private socket preserves fragmented RPC responses and serializes desktop calls across clients', async t => {
  let active = 0, high = 0, stopped = 0;
  const host = await startSkyHost({ makeSession: () => ({ async request(r) {
    active++; high = Math.max(high, active); await delay(15); active--; return r;
  }, async stop() { stopped++; } }) });
  t.after(() => host.dispose());
  assert.equal(fs.statSync(path.dirname(host.socketPath)).mode & 0o777, 0o700);
  assert.equal(fs.statSync(host.socketPath).mode & 0o777, 0o600);
  const a = await client(host.socketPath), b = await client(host.socketPath);
  const request = { type: 'execute', method: 'type_text', args: [{ text: 'Unicode: Ω 🙂' }] };
  const responses = await Promise.all([a.call(request, true), b.call({ type: 'setup' })]);
  assert.deepEqual(responses[0].value, request); assert.equal(high, 1);
  await host.endTurn(); assert.equal(stopped, 2);
});
test('turn completion cancels queued calls and prevents stale replies', async t => {
  let release, calls = 0;
  const entered = new Promise(resolve => { release = resolve; });
  let finish;
  const blocked = new Promise(resolve => { finish = resolve; });
  const host = await startSkyHost({ makeSession: () => ({ async request() { calls++; release(); await blocked; return 'done'; }, async stop() { finish(); } }) });
  t.after(() => host.dispose());
  const a = await client(host.socketPath);
  void a.call({ type: 'setup' }); void a.call({ type: 'setup' });
  await entered;
  const closed = once(a.socket, 'close');
  await host.endTurn(); await closed; await delay(10);
  assert.equal(calls, 1);
});
test('oversized frames and invalid requests close without invoking Sky', async t => {
  let calls = 0;
  const host = await startSkyHost({ makeSession: () => ({ async request() { calls++; }, async stop() {} }) });
  t.after(() => host.dispose());
  for (const bytes of [Buffer.from([1, 0, 0, 1]), frame({ id: 1, request: { type: 'shell', command: 'ignored' } })]) {
    const a = await client(host.socketPath); const closed = once(a.socket, 'close'); a.socket.write(bytes); await closed;
  }
  assert.equal(calls, 0);
  assert.throws(() => validateRequest({ type: 'execute', method: 'list_windows', args: {} }));
});
test('worker releases tracked drags through the unchanged service API', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sky-worker-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const log = path.join(dir, 'calls.jsonl'), service = path.join(dir, 'service.mjs');
  fs.writeFileSync(service, `import fs from 'node:fs'; export async function handleRpc(r) { fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify(r)+'\\n'); return r.type; }`);
  const session = createSession(service); t.after(() => session.stop());
  await session.request({ type: 'drag_start', handle_id: 'fixture', point: { x: 4, y: 5 } });
  await session.stop();
  const calls = fs.readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.at(-1), { type: 'drag_end', handle_id: 'fixture' });
  await assert.rejects(session.request({ type: 'setup' }), /closed/);
});
test('a stuck native call has bounded shutdown and its helper process group is reaped', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sky-worker-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const log = path.join(dir, 'pid'), service = path.join(dir, 'service.mjs');
  fs.writeFileSync(service, `import fs from 'node:fs'; import {spawn} from 'node:child_process'; export async function handleRpc() { const p=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'}); fs.writeFileSync(${JSON.stringify(log)},String(p.pid)); return new Promise(()=>{}); }`);
  const session = createSession(service, { cleanupMs: 50 }); t.after(() => session.stop());
  const pending = session.request({ type: 'setup' }).catch(e => e);
  for (let i = 0; i < 100 && !fs.existsSync(log); i++) await delay(10);
  assert.ok(fs.existsSync(log));
  const pid = Number(fs.readFileSync(log, 'utf8'));
  const start = Date.now(); await session.stop(); assert.ok(Date.now() - start < 2000);
  assert.match(String(await pending), /ended|exited/);
  for (let i = 0; i < 100; i++) { try { process.kill(pid, 0); } catch (e) { if (e.code === 'ESRCH') return; throw e; } await delay(10); }
  assert.fail('Native helper remained alive after shutdown');
});
