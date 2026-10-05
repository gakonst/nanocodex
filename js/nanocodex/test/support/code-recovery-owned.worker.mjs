// Real owned public SDK, Rust WASM and asyncify QuickJS; terminated without shutdown.
import { parentPort, workerData } from 'node:worker_threads';
import { DatabaseSync } from 'node:sqlite';
import WebSocket from 'ws';
import { once } from 'node:events';
import { bindAgent } from '../../cloudflare/Agent.mjs';
import { readFile } from 'node:fs/promises';
import { Agent as NodeAgent, Transport as NodeTransport, createQuickJsEvaluator } from '../../node/index.mjs';
import { Agent as HostAgent, Transport as HostTransport } from '../../host/index.mjs';
import { toolResult } from '../../runtime/code-runtime.mjs';
import variant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';
let next = 1;
let activeTurn;
let queuedTurn;
const pending = new Map();
function rpc(method, ...args) {
  const id = next++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    parentPort.postMessage({ type: 'rpc', id, method, args });
  });
}
parentPort.on('message', ({ id, result, error, action }) => {
  if (action === 'cancel') { void activeTurn.cancel(); return; }
  const callback = pending.get(id);
  if (!callback) return;
  pending.delete(id);
  if (error) callback.reject(new Error(error)); else callback.resolve(result);
});
const durability = Object.fromEntries(['acquire', 'replace', 'load', 'readRecord', 'readRecords', 'scanRecords']
  .map(method => [method, (...args) => rpc('durability.' + method, ...args)]));
const codeEffectJournal = workerData.journal ? {
  begin: context => rpc('journal.begin', context),
  complete: (context, receipt) => rpc('journal.complete', context, receipt),
  ...(workerData.cellJournal ? {
    beginCell: context => rpc('journal.beginCell', context),
    completeCell: (context, writes, receipt) => rpc('journal.completeCell', context, writes, receipt),
  } : {}),
} : undefined;
const shared = {
  module: await readFile(new URL('../../pkg-web/nanocodex_bg.wasm', import.meta.url)),
  durability, durabilityId: 'code-recovery-owned',
  sessionId: '018f1f9a-7b3c-7a07-8000-000000000021',
  codeEvaluator: workerData.evaluator === "native" ? undefined
    : createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(variant)),
  codeEffectJournal,
  toolMode: workerData.direct ? "direct" : "code",
  tools: { effect: {
    description: 'Synthetic externally observable effect', supportsParallelToolCalls: true,
    parameters: { type: 'object', properties: { kind: { type: 'string' } }, required: ['kind'] },
    async handler({ kind }, context) {
      const outcome = rpc('effect', kind, context.callId);
      if (workerData.queuedProviderIds && !queuedTurn && kind === 'read-one') {
        queuedTurn = agent.turn.prompt({ ...(workerData.nonDurable ? {} : { id: 'follow-on' }), input: 'Queued while original effect is outstanding.' });
        void queuedTurn.result().catch(() => {});
        parentPort.postMessage({ type: 'queued-operation-submitted' });
      }
      if (kind === 'abortable') {
        await Promise.race([outcome, new Promise((_, reject) => {
          context.signal.addEventListener('abort', () => reject(context.signal.reason), { once: true });
        })]);
      } else await outcome;
      if (kind === 'undefined') return undefined;
      if (kind === 'raw-undefined') throw undefined;
      if (kind === 'raw-error') throw { code: 'RAW', value: 7 };
      if (kind === 'typed-error') throw Object.assign(new TypeError('fixture failed'), { code: 'TYPED', details: { retry: false } });
      if (kind === 'failed-result') return toolResult('tool failure', { failed: true }, { success: false, value: { code: 'FAILED_RESULT' } });
      if (kind === 'huge') return 'x'.repeat(16 * 1024 * 1024);
      if (kind === 'max-output') {
        const prefix = 'MAX_OUTPUT_BEGIN', suffix = 'MAX_OUTPUT_END';
        return prefix + 'x'.repeat(399000 - prefix.length - suffix.length) + suffix;
      }
      if (kind === 'media') {
        const image = { content: [{ type: 'image', data: largePng().toString('base64'), mimeType: 'image/png' }] };
        return toolResult(image, image, { value: image });
      }
      return { kind, done: true };
    },
  } },
};
if (workerData.nonDurable) { delete shared.durability; delete shared.durabilityId; }
const api = workerData.sdk === 'host' ? { Agent: HostAgent, Transport: HostTransport } : { Agent: NodeAgent, Transport: NodeTransport };
let agent;
if (workerData.sdk === 'cloudflare') {
  // Exercise the actual Cloudflare -> host -> InlineAgent internal bridge. Only
  // its external SQL/socket dependencies are adapted to this native test host.
  const database = new DatabaseSync(workerData.databasePath);
  database.exec('PRAGMA busy_timeout = 5000');
  const storage = {
    sql: { exec(sql, ...args) {
      const statement = database.prepare(sql);
      const rows = statement.all(...args);
      return { toArray: () => rows, one: () => rows[0], [Symbol.iterator]: () => rows[Symbol.iterator]() };
    } },
    transactionSync(callback) {
      database.exec('BEGIN IMMEDIATE');
      try { const value = callback(); database.exec('COMMIT'); return value; }
      catch (error) { database.exec('ROLLBACK'); throw error; }
    },
  };
  const owner = { ctx: { id: { toString: () => 'a'.repeat(64) }, storage, acceptWebSocket() {}, getWebSockets() { return []; } },
    env: { NANOCODEX: { async fetch() {
      const socket = new WebSocket(workerData.url);
      socket.accept = () => {};
      await once(socket, 'open');
      return { status: 101, headers: new Headers(), webSocket: socket };
    } } },
  };
  agent = await bindAgent(shared.module).create(owner, { durabilityId: shared.durabilityId,
    eventPersistence: 'caller', tools: shared.tools,
    [Symbol.for('nanocodex.cloudflare.internalRuntime')]: { toolMode: shared.toolMode,
      codeEvaluator: shared.codeEvaluator, codeEffectJournal: shared.codeEffectJournal },
  });
} else agent = await api.Agent.create({ ...shared,
  transport: api.Transport.openAi({ apiKey: 'synthetic', websocketUrl: workerData.url, websocketWarmup: false }),
});
agent.events.watch().onEvent(event => parentPort.postMessage({ type: 'event', event }));
parentPort.postMessage({ type: 'ready' });
try {
  activeTurn = agent.turn.prompt({ ...(workerData.nonDurable ? {} : { id: 'original' }), input: 'Execute the synthetic recovery journey.' });
  const result = await activeTurn.result();
  parentPort.postMessage({ type: 'result', finalMessage: result.finalMessage });
  const follow = await (queuedTurn ?? agent.turn.prompt({ ...(workerData.nonDurable ? {} : { id: 'follow-on' }), input: 'Reply after recovery.' })).result();
  parentPort.postMessage({ type: 'follow-on', finalMessage: follow.finalMessage });
  if (workerData.reusedProviderIds) {
    const third = await agent.turn.prompt({ id: 'third-turn', input: 'Execute a new tool call with the reused provider ID.' }).result();
    parentPort.postMessage({ type: 'third-on', finalMessage: third.finalMessage });
  }
  await agent.session.shutdown();
} catch (error) {
  parentPort.postMessage({ type: 'failure', error: { message: error.message, code: error.code } });
  if (workerData.cancellation) {
    const follow = await agent.turn.prompt({ ...(workerData.nonDurable ? {} : { id: 'follow-on' }), input: 'Reply after cancellation.' }).result();
    parentPort.postMessage({ type: 'follow-on', finalMessage: follow.finalMessage });
    await agent.session.shutdown();
  }
}

// Valid PNG plus trailing padding follows the real image-memory WASM fixture:
// it exercises ordinary photo-sized transfer without expensive decoded pixels.
function largePng() {
  const tiny = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC', 'base64');
  return Buffer.concat([tiny, Buffer.alloc(2 * 1024 * 1024)]);
}
