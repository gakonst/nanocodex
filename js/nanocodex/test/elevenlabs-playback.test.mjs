import test from 'node:test';
import assert from 'node:assert/strict';
import { ElevenLabsPlayback } from '../browser/ElevenLabsPlayback.mjs';

test('streams sentence PCM once, preserves order, and stops scheduled audio on interruption', async () => {
  const sources = [];
  globalThis.AudioContext = class {
    currentTime = 0;
    destination = {};
    async resume() {}
    async close() {}
    createBuffer(_channels, length, rate) {
      return { duration: length / rate, getChannelData: () => new Float32Array(length) };
    }
    createBufferSource() {
      const source = { connect() {}, disconnect() {}, start(time) { this.time = time; }, stop() { this.stopped = true; } };
      sources.push(source);
      return source;
    }
  };
  const calls = [];
  const player = new ElevenLabsPlayback(async (text, signal) => {
    calls.push({ text, signal });
    return new Response(new Uint8Array([0, 0, 255, 127]));
  }, (error) => { throw error; });
  player.transcript({ speaker: 'assistant', id: 0, text: 'Hello', is_partial: true });
  player.transcript({ speaker: 'assistant', id: 0, text: 'Hello. Next', is_partial: true });
  player.transcript({ speaker: 'assistant', id: 0, text: 'Hello. Next.', is_partial: false });
  await new Promise(setImmediate);
  assert.deepEqual(calls.map(c => c.text), ['Hello.', 'Next.']);
  assert.equal(sources.length, 2);
  assert.ok(sources[1].time > sources[0].time);
  player.interrupt();
  assert.ok(sources.every(s => s.stopped));
  assert.ok(calls.every(c => c.signal.aborted));
  player.close();
  delete globalThis.AudioContext;
});

test('interruption discards an in-flight response and queued sentences', async () => {
  let resolve;
  let calls = 0;
  globalThis.AudioContext = class {
    async resume() {}
    async close() {}
    createBuffer() { throw new Error('stale audio played'); }
  };
  const player = new ElevenLabsPlayback(() => { calls++; return new Promise(r => { resolve = r; }); }, assert.fail);
  player.transcript({ speaker: 'assistant', id: 0, text: 'First.', is_partial: true });
  player.transcript({ speaker: 'assistant', id: 0, text: 'First. Second.', is_partial: false });
  await new Promise(setImmediate);
  player.interrupt();
  resolve(new Response(new Uint8Array([0, 0])));
  await new Promise(setImmediate);
  assert.equal(calls, 1);
  player.close();
  delete globalThis.AudioContext;
});

test('late deltas and finals remain suppressed after interruption, next caption plays', async () => {
  const calls = [];
  globalThis.AudioContext = class {
    async resume() {}
    async close() {}
  };
  const player = new ElevenLabsPlayback(async text => { calls.push(text); return new Response(new Uint8Array()); }, assert.fail);
  player.transcript({ speaker: 'assistant', id: 2, text: 'Unfinished', is_partial: true });
  player.interrupt();
  player.transcript({ speaker: 'assistant', id: 2, text: 'Unfinished late.', is_partial: true });
  player.transcript({ speaker: 'assistant', id: 2, text: 'Unfinished late. Final.', is_partial: false });
  player.transcript({ speaker: 'assistant', id: 3, text: 'New turn.', is_partial: false });
  player.transcript({ speaker: 'assistant', id: 2, text: 'Even later.', is_partial: false });
  await new Promise(setImmediate);
  assert.deepEqual(calls, ['New turn.']);
  player.close();
  delete globalThis.AudioContext;
});

test('text queue overflow interrupts and reports a bounded failure', async () => {
  const errors = [];
  const player = new ElevenLabsPlayback(assert.fail, error => errors.push(error.message));
  for (let id = 0; id < 33; id++) player.transcript({ speaker: 'assistant', id, text: 'Sentence.', is_partial: false });
  await new Promise(setImmediate);
  assert.deepEqual(errors, ['ElevenLabs playback queue exceeded its limit']);
  player.close();
});

test('audio scheduling applies backpressure and interruption releases it', async () => {
  const sources = [];
  globalThis.AudioContext = class {
    currentTime = 0;
    destination = {};
    async resume() {}
    async close() {}
    createBuffer(_channels, length, rate) { return { duration: length / rate, getChannelData: () => new Float32Array(length) }; }
    createBufferSource() {
      const source = { connect() {}, disconnect() {}, start(time) { this.time = time; }, stop() { this.stopped = true; } };
      sources.push(source);
      return source;
    }
  };
  const player = new ElevenLabsPlayback(async () => new Response(new Uint8Array(24000 * 2 * 5)), assert.fail);
  player.transcript({ speaker: 'assistant', id: 0, text: 'Five seconds.', is_partial: false });
  await new Promise(setImmediate);
  assert.ok(sources.length > 0 && sources.length <= 11);
  assert.ok(sources.every(source => source.time <= 2.02));
  player.interrupt();
  await new Promise(setImmediate);
  assert.ok(sources.every(source => source.stopped));
  player.close();
  delete globalThis.AudioContext;
});
