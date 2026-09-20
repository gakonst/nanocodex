// Trusted-service adapter. It exports exactly the upstream Sky service entrypoint.
// No socket or nativePipe authority is provided to model JavaScript.
let connection;
let opening;
let nextId = 1;
let generation = 0;
let buffer = Buffer.alloc(0);
const pending = new Map();
const LIMIT = 64 * 1024 * 1024;
function close(reason = new Error('Sky desktop connection closed')) {
  generation++;
  opening = undefined;
  const previous = connection;
  connection = undefined;
  buffer = Buffer.alloc(0);
  for (const call of pending.values()) { clearTimeout(call.timer); call.reject(reason); }
  pending.clear();
  previous?.end();
}
async function connect() {
  if (connection) return connection;
  if (opening) return opening;
  const epoch = generation;
  let promise;
  promise = (async () => {
    if (typeof nodeRepl.nativePipe?.createConnection !== 'function') throw new Error('Trusted nativePipe is unavailable');
    const socketPath = nodeRepl.env?.NANOCODEX_LINUX_SKY_SOCKET;
    if (!socketPath) throw new Error('Linux Sky host socket is unavailable');
    const peer = await nodeRepl.nativePipe.createConnection(socketPath);
    if (epoch !== generation) { peer.end(); throw new Error('Sky connection was cancelled'); }
    connection = peer;
    peer.on('error', error => { if (connection === peer) close(error); });
    peer.on('close', () => { if (connection === peer) close(); });
    peer.on('data', chunk => {
      if (connection !== peer) return;
      try {
        buffer = Buffer.concat([buffer, Buffer.from(chunk)]);
        while (buffer.length >= 4) {
          const length = buffer.readUInt32LE();
          if (length > LIMIT) throw new Error('Sky response exceeds 64 MiB');
          if (buffer.length < length + 4) break;
          const response = JSON.parse(buffer.subarray(4, length + 4));
          buffer = buffer.subarray(length + 4);
          const call = pending.get(response.id);
          if (!call) continue;
          pending.delete(response.id); clearTimeout(call.timer);
          response.error ? call.reject(new Error(response.error)) : call.resolve(response.value);
        }
      } catch (error) { close(error); }
    });
    return peer;
  })().finally(() => { if (opening === promise) opening = undefined; });
  opening = promise;
  return promise;
}
nodeRepl.addTurnEndedHandler({ run: async () => close(), timeoutMs: 4000 });
export async function handleRpc(request) {
  const peer = await connect();
  if (peer !== connection) throw new Error('Sky request was cancelled');
  const id = nextId++;
  const body = Buffer.from(JSON.stringify({ id, request }));
  if (body.length > 8 * 1024 * 1024) throw new Error('Sky request exceeds 8 MiB');
  const frame = Buffer.allocUnsafe(body.length + 4);
  frame.writeUInt32LE(body.length); body.copy(frame, 4);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => close(new Error('Sky desktop request timed out')), 30000);
    pending.set(id, { resolve, reject, timer });
    try { peer.write(frame); } catch (error) { close(error); }
  });
}
