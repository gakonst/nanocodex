// One synthetic local Hand for managed-curl-hand-recovery.test.mjs: the shipped
// Node process tools and attachment publisher in their own OS process, connected
// to the public /v1/account/tool-host WebSocket with an API key. The parent test
// controls only the network path (drop, hold, lose one delivered call frame) and
// the process lifetime (SIGKILL = Hand crash, new process = publisher replacement).
import WebSocket from 'ws';
import { createTools } from '../../../nanocodex/tools/Tools.mjs';
import { createAttachment } from '../../../nanocodex-tools/tools/attachment.mjs';
import { createNodeProcessTools } from '../../../nanocodex-tools/tools/nodeProcess.mjs';

const { endpoint, workspace, machine, label, timeoutMs } = JSON.parse(process.argv[2]);
const report = event => process.send?.({ label, at: Date.now(), ...event });
// Receive the credential over IPC so neither argv nor the environment inherited
// by Hand shell commands ever contains it.
const token = await new Promise(resolve => process.once('message', message => resolve(message.token)));
let gate, lose, current;

// Shipped attachment observations carry no inputs or credentials; keep them.
const info = console.info.bind(console);
console.info = (record, ...rest) => {
  if (record?.type === 'hand.attachment') report({ kind: 'attachment', event: record.event,
    runtime_id: record.runtime_id, attempt: record.attempt, active_calls: record.active_calls, retained_calls: record.retained_calls });
  else info(record, ...rest);
};

const summary = frame => ({ type: frame.type, call_id: frame.call_id, name: frame.name, state: frame.state,
  call_ids: frame.call_ids, cmd: frame.input?.cmd, status: frame.status ?? frame.result?.status,
  // write_stdin: only the size of its chars (0 = empty poll), never their content.
  chars_length: typeof frame.input?.chars === 'string' ? frame.input.chars.length : undefined });

// A WebSocket-compatible view of one real ws connection that records frame
// summaries and can drop one delivered call frame before the attachment sees it.
function observed(socket) {
  return {
    get readyState() { return socket.readyState; },
    send(data) {
      try { report({ kind: 'frame', direction: 'publisher', frame: summary(JSON.parse(String(data))) }); } catch {}
      socket.send(data);
    },
    close(code, reason) { socket.close(code, reason); },
    ping(data) { socket.ping(data); },
    on(type, listener) { socket.on(type, listener); return this; },
    addEventListener(type, listener) {
      if (type !== 'message') return socket.addEventListener(type, listener);
      socket.addEventListener('message', event => {
        let frame;
        try { frame = JSON.parse(String(event.data)); } catch { return listener(event); }
        report({ kind: 'frame', direction: 'broker', frame: summary(frame) });
        if (frame.type === 'call' && lose && String(frame.input?.cmd ?? '').includes(lose)) {
          // The broker durably dispatched this call; the network loses it.
          report({ kind: 'lost_in_transit', call_id: frame.call_id, marker: lose });
          lose = undefined; socket.terminate(); return;
        }
        listener(event);
      });
    },
  };
}

const native = await createNodeProcessTools({ workspace });
// A Hand publishes its own per-tool deadline, as native Hands do.
const tools = await createTools({ tools: native.tools.map(tool => tool.name === 'exec_command' && timeoutMs ? { ...tool, timeoutMs } : tool) });
const connector = createAttachment(tools, { endpoint, transport: { async connect() {
  if (gate) { report({ kind: 'reconnect_held' }); await gate.promise; }
  const socket = new WebSocket(endpoint, { headers: { authorization: `Bearer ${token}` } });
  current = socket;
  socket.on('open', () => report({ kind: 'socket', event: 'open' }));
  socket.on('close', (code, reason) => report({ kind: 'socket', event: 'close', code, reason: String(reason) }));
  socket.on('unexpected-response', (_, response) => report({ kind: 'socket', event: 'rejected', status: response.statusCode }));
  return observed(socket);
} } }, { machines: [{ id: machine, name: 'Synthetic curl recovery Hand', workspace, capabilities: ['shell'] }],
  attachmentId: machine, heartbeatMs: 500, reconnectDelayMs: 50, drainTimeoutMs: 1_000 });

process.on('message', async message => {
  if (message.op === 'hold') { let release; const promise = new Promise(done => { release = done; }); gate = { promise, release }; }
  else if (message.op === 'release') { const held = gate; gate = undefined; held?.release(); }
  else if (message.op === 'drop') current?.terminate();
  else if (message.op === 'lose') lose = message.marker;
  else if (message.op === 'close') {
    await Promise.race([connector.close(), new Promise(done => setTimeout(done, 2_000))]);
    await native.close(); process.exit(0);
  }
  report({ kind: 'control', op: message.op });
});
const client = await connector.connect();
report({ kind: 'ready', connected: client.connected });
