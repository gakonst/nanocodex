import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { startSkyHost, frame } from '../../src/windows_sky_host.mjs';

const meta = { session_id: 'fixture-session', turn_id: 'fixture-turn', call_id: 'fixture-call' };
function request(id, method = 'get_app_state', codexTurnMetadata = meta) {
  return { jsonrpc: '2.0', id, method: 'request', params: { method, params: {}, codexTurnMetadata } };
}
async function fixture(t, perform, approvalTimeoutMs = 500) {
  const calls = []; let closes = 0;
  const pipePath = process.platform === 'win32' ? `\\\\.\\pipe\\sky-test-${randomUUID()}` : path.join(os.tmpdir(), `sky-${randomUUID()}.sock`);
  const host = await startSkyHost({ pipePath, approvalTimeoutMs, makeTransport: () => ({
    request: async (...args) => { calls.push(args); return perform ? perform(...args) : { apps: [] }; },
    close: async () => { closes++; },
  }) });
  const socket = net.connect(pipePath); await new Promise(resolve => socket.once('connect', resolve));
  const queue = [], waiters = []; let buffer = Buffer.alloc(0);
  socket.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= buffer.readUInt32LE() + 4) {
      const length = buffer.readUInt32LE(); const item = JSON.parse(buffer.subarray(4, 4 + length)); buffer = buffer.subarray(4 + length);
      const waiter = waiters.shift(); waiter ? waiter(item) : queue.push(item);
    }
  });
  t.after(async () => { socket.destroy(); await host.dispose(); });
  return { host, socket, calls, closes: () => closes, send: message => socket.write(frame(message)), read: () => queue.length ? Promise.resolve(queue.shift()) : new Promise(resolve => waiters.push(resolve)) };
}
test('fragmented native frame reaches transport with unchanged turn metadata', async t => {
  const f = await fixture(t); const bytes = frame(request(1, 'list_apps'));
  f.socket.write(bytes.subarray(0, 2)); f.socket.write(bytes.subarray(2));
  assert.deepEqual(await f.read(), { jsonrpc: '2.0', id: 1, result: { apps: [] } });
  assert.deepEqual(f.calls[0][2].codexTurnMetadata, meta);
  await f.host.endTurn(); assert.equal(f.closes(), 1);
});
test('approval is relayed, denial is not converted to acceptance, concurrent calls remain blocked', async t => {
  const f = await fixture(t, async (method, params, options) => {
    const answer = await options.createElicitation({ message: 'Approve fixture?', meta: { connector_id: 'computer-use' } });
    if (answer.action !== 'accept') throw new Error('Fixture denied');
    return answer;
  });
  f.send(request(1)); const approval = await f.read(); assert.equal(approval.method, 'requestComputerUseApproval');
  for (const id of [2, 3]) { f.send(request(id)); assert.match((await f.read()).error.message, /active request/); }
  f.send({ jsonrpc: '2.0', id: approval.id, result: { action: 'decline' } });
  assert.equal((await f.read()).error.message, 'Fixture denied'); assert.equal(f.calls.length, 1);
});
test('approval timeout fails closed', async t => {
  const f = await fixture(t, (m, p, options) => options.createElicitation({ message: 'Fixture' }), 10);
  f.send(request(1)); await f.read(); assert.match((await f.read()).error.message, /timed out/);
});
test('end turn cancels pending approval and closes helper', async t => {
  const f = await fixture(t, (m, p, options) => options.createElicitation({ message: 'Fixture' }));
  f.send(request(1)); await f.read(); await f.host.endTurn();
  assert.match((await f.read()).error.message, /turn ended/); assert.equal(f.closes(), 1);
});
test('disconnect rejects pending approval and closes helper', async t => {
  let rejected;
  const done = new Promise(resolve => rejected = resolve);
  const f = await fixture(t, async (m, p, options) => {
    try { await options.createElicitation({ message: 'Fixture' }); } catch (e) { rejected(e.message); throw e; }
  });
  f.send(request(1)); await f.read(); f.socket.destroy();
  assert.match(await done, /disconnected/); assert.equal(f.closes(), 1);
});
test('missing turn or supplied approval token never reaches helper', async t => {
  const f = await fixture(t);
  for (const metadata of [{}, { ...meta, 'x-oai-cua-approved-app': 'fixture' }]) {
    f.send(request(1, 'list_apps', metadata)); assert.ok((await f.read()).error);
  }
  assert.equal(f.calls.length, 0);
});
test('oversized frame closes pipe without calling helper', async t => {
  const f = await fixture(t); const bytes = Buffer.alloc(4); bytes.writeUInt32LE(8388609);
  const closed = new Promise(resolve => f.socket.once('close', resolve)); f.socket.write(bytes); await closed;
  assert.equal(f.calls.length, 0);
});
test('approval reply from another pipe client cannot resolve the request', async t => {
  const f = await fixture(t, async (m, p, options) => {
    const result = await options.createElicitation({ message: 'Fixture' });
    throw new Error(`Fixture ${result.action}`);
  });
  f.send(request(1)); const approval = await f.read();
  const other = net.connect(f.host.pipePath); await new Promise(resolve => other.once('connect', resolve));
  t.after(() => other.destroy());
  other.write(frame({ jsonrpc: '2.0', id: approval.id, result: { action: 'cancel' } }));
  f.send(request(2)); assert.match((await f.read()).error.message, /active request/);
  f.send({ jsonrpc: '2.0', id: approval.id, result: { action: 'decline' } });
  assert.equal((await f.read()).error.message, 'Fixture decline');
});
test('turn ending with different metadata cannot cancel an active request', async t => {
  const f = await fixture(t, (m, p, options) => options.createElicitation({ message: 'Fixture' }));
  f.send(request(1)); const approval = await f.read();
  f.send(request(2, 'end_turn', { ...meta, turn_id: 'other' }));
  assert.match((await f.read()).error.message, /turn mismatch/); assert.equal(f.closes(), 0);
  f.send({ jsonrpc: '2.0', id: approval.id, result: { action: 'decline' } });
  assert.equal((await f.read()).result.action, 'decline');
});
