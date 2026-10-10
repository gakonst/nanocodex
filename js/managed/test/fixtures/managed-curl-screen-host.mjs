// One synthetic native screen host for managed-curl-hand-recovery.test.mjs, in
// its own OS process. It speaks the shipped remote screen protocol over the
// public /v1/account/hands/host WebSocket with an API key: catalog on "ready",
// then one agent_result per agent_call. Only the screen hardware is synthetic:
// every typed action appends one line to <workspace>/screen.log (the side
// effect) and answers with a fixed JPEG. Typed text selects the hardware
// timing: __SLOW__ answers after 2.5s, __HOLD__ answers only on "release".
// The parent controls only the process lifetime (SIGKILL = host crash).
import { appendFile } from 'node:fs/promises';
import { join } from 'node:path';
import WebSocket from 'ws';

const { endpoint, workspace, machine, label } = JSON.parse(process.argv[2]);
const report = event => process.send?.({ label, at: Date.now(), ...event });
const token = await new Promise(resolve => process.once('message', message => resolve(message.token)));
const JPEG = '/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAEBAQ==';
const held = new Map();
const summary = frame => ({ type: frame.type, request_id: frame.request_id, agent_id: frame.agent_id,
  surface_id: frame.surface_id, input: frame.input, status: frame.status, generation: frame.generation });

const socket = new WebSocket(endpoint, { headers: { authorization: `Bearer ${token}` } });
const send = frame => { report({ kind: 'frame', direction: 'host', frame: summary(frame) }); socket.send(JSON.stringify(frame)); };
const answer = request_id => { if (socket.readyState === WebSocket.OPEN) send({ type: 'agent_result', request_id, status: 'ok', jpeg: JPEG, width: 1, height: 1 }); };
socket.on('open', () => report({ kind: 'socket', event: 'open' }));
socket.on('close', (code, reason) => report({ kind: 'socket', event: 'close', code, reason: String(reason) }));
socket.on('unexpected-response', (_, response) => report({ kind: 'socket', event: 'rejected', status: response.statusCode }));
socket.on('message', async data => {
  let frame; try { frame = JSON.parse(String(data)); } catch { return; }
  report({ kind: 'frame', direction: 'broker', frame: summary(frame) });
  if (frame.type === 'ready') send({ type: 'catalog', machine_id: machine, machine_name: 'Synthetic curl screen',
    surfaces: [{ id: 'desktop', name: 'Synthetic desktop', kind: 'desktop', width: 1, height: 1, controllable: true, agent_tools: true }] });
  else if (frame.type === 'published') report({ kind: 'ready', generation: frame.generation });
  else if (frame.type === 'agent_cancel') report({ kind: 'cancelled', request_id: frame.request_id });
  else if (frame.type === 'agent_call') {
    const typed = String(frame.input?.text ?? frame.input?.action ?? '');
    await appendFile(join(workspace, 'screen.log'), `${label}:${typed}\n`);
    report({ kind: 'effect', request_id: frame.request_id, typed });
    if (typed.includes('__HOLD__')) held.set(frame.request_id, typed);
    else setTimeout(() => answer(frame.request_id), typed.includes('__SLOW__') ? 2_500 : 0);
  }
});
process.on('message', message => {
  if (message.op === 'release') { for (const id of held.keys()) answer(id); held.clear(); }
  else if (message.op === 'close') { socket.close(1000); setTimeout(() => process.exit(0), 200); }
  report({ kind: 'control', op: message.op });
});
