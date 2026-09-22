import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { test } from "node:test";
import { connectDeviceHand } from "../src/device-hand.mjs";

// The publisher can inherit a client's output handles on Windows. Reproduce
// that ownership on every platform so closing a lease cannot wait on its host.
test("closing a client does not wait for a publisher holding its output pipes", { timeout: 5000 }, async t => {
  let publisherPid;
  const script = `
    const { spawn } = require('node:child_process');
    const publisher = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 30000)'], {
      detached: true, stdio: ['ignore', process.stdout, process.stderr]
    });
    publisher.unref();
    console.log(JSON.stringify({ status: 'connected', publisherPid: publisher.pid }));
    process.stdin.resume();
    process.stdin.on('end', () => process.exit(0));
  `;
  const states = [];
  const connection = connectDeviceHand({
    binary: process.execPath, env: process.env, signal: new AbortController().signal,
    spawnProcess: (binary, _args, options) => spawn(binary, ['-e', script], options),
    onState: state => { states.push(state); publisherPid = state.publisherPid; },
  });
  t.after(async () => {
    if (publisherPid) { try { process.kill(publisherPid); } catch {} }
    await connection.close();
  });
  await connection.ready;
  await connection.close();
  assert.doesNotThrow(() => process.kill(publisherPid, 0), 'Closing a lease must preserve its publisher');
  assert.equal(states.length, 1);
});
