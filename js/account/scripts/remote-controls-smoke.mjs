// Synthetic Chromium regression for the production Screen + RemoteBrowserSession.
// Input checks mock remote transport; the final audio check uses real WebRTC loopback.
// Run: node js/account/scripts/remote-controls-smoke.mjs
// Optional focused run: REMOTE_SMOKE_FILTER='real WebRTC' node js/account/scripts/remote-controls-smoke.mjs
import assert from 'node:assert/strict';
import { installRemoteControlsLoopback } from './fixtures/remote-controls-loopback.mjs';
import { createServer } from 'node:http';
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
assert.ok(entry, 'Install workspace dependencies, including playwright-core');
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const bundle = await build({
  stdin: { contents: `import React from 'react'; import {createRoot} from 'react-dom/client';
    import {Screen} from './src/RemoteScreens';
    const hand = {id:'synthetic-screen',name:'Synthetic desktop',kind:'desktop',width:960,height:540,
      controllable:true,machine_id:'synthetic-machine',machine_name:'Synthetic computer',generation:'synthetic-generation'};
    createRoot(document.getElementById('root')).render(<div className="remote-screens" style={{display:'flex',height:800,width:1100}}>
      <Screen hand={hand} onBack={()=>{window.backCount++}} /></div>);`,
    resolveDir: new URL('..', import.meta.url).pathname, loader: 'tsx' },
  bundle: true, write: false, outfile: 'app.js', jsx: 'automatic',
  alias: { 'nanocodex-connect-ui/browserAccountSession': new URL('../../nanocodex-connect-ui/src/browserAccountSession.ts', import.meta.url).pathname },
});
const javascript = bundle.outputFiles.find(file => file.path.endsWith('.js')).text;
const css = bundle.outputFiles.find(file => file.path.endsWith('.css'))?.text ?? '';
const server = createServer((req, res) => {
  if (req.url === '/app.js') { res.setHeader('Content-Type', 'text/javascript'); res.end(javascript); }
  else if (req.url === '/app.css') { res.setHeader('Content-Type', 'text/css'); res.end(css); }
  else { res.setHeader('Content-Type', 'text/html'); res.end('<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="/app.css"><style>body{margin:24px;font:14px system-ui}*{box-sizing:border-box}</style><div id="root"></div><script src="/app.js"></script>'); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
// Chromium reads this generated PCM tone as its fake microphone. No physical
// capture device is opened, and the file is removed when the browser closes.
const audioDirectory = mkdtempSync(join(tmpdir(), 'remote-controls-smoke-'));
const microphoneFile = join(audioDirectory, 'microphone.wav');
const sampleRate = 48_000, seconds = 2, samples = sampleRate * seconds;
const wav = Buffer.alloc(44 + samples * 2);
wav.write('RIFF', 0); wav.writeUInt32LE(wav.length - 8, 4); wav.write('WAVEfmt ', 8);
wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(sampleRate, 24); wav.writeUInt32LE(sampleRate * 2, 28);
wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
wav.write('data', 36); wav.writeUInt32LE(samples * 2, 40);
for (let index = 0; index < samples; index++) wav.writeInt16LE(Math.round(8_192 * Math.sin(2 * Math.PI * 440 * index / sampleRate)), 44 + index * 2);
writeFileSync(microphoneFile, wav);
const screenshots = fileURLToPath(new URL('../../../build/remote-controls-smoke/', import.meta.url));
mkdirSync(screenshots, { recursive: true });
// Keep generated evidence out of git without changing repository ignore policy.
writeFileSync(join(screenshots, '.gitignore'), '*\n');
let browser;
const results = [], failures = [];
async function check(name, task) { if (process.env.REMOTE_SMOKE_FILTER && !name.includes(process.env.REMOTE_SMOKE_FILTER)) return; try { await task(); results.push(name); console.log(`PASS ${name}`); } catch (error) { failures.push(name); console.error(`FAIL ${name}: ${error.stack}`); } }
try {
  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', `--use-file-for-fake-audio-capture=${microphoneFile}`] });
  const page = await browser.newPage({ viewport: { width: 1200, height: 1000 } });
  const errors = [], externalRequests = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) { externalRequests.push(url.origin); return route.abort(); }
    if (url.pathname === '/v1/account/hands/ice') return route.fulfill({ json: { iceServers: [] } });
    if (url.pathname.startsWith('/v1/account/hands/')) return route.fulfill({ json: {} });
    return route.continue();
  });
  await page.addInitScript(() => {
    window.packets = []; window.backCount = 0; window.grants = 0; window.inputEvents = [];
    for (const name of ['pointerdown','pointermove','pointerup','mousedown','mouseup','lostpointercapture']) document.addEventListener(name, event => window.inputEvents.push({type:event.type,buttons:event.buttons,button:event.button,target:event.target.className}), true);
    class Channel {
      constructor(label, ordered) { this.label = label; this.ordered = ordered; this.maxRetransmits = ordered ? null : 0; this.maxPacketLifeTime = null; this.readyState = 'open'; this.bufferedAmount = 0; }
      send(raw) {
        const message = JSON.parse(raw); window.packets.push({ ...message, channel: this.label });
        if (message.type === 'acquire') queueMicrotask(() => this.onmessage?.({ data: JSON.stringify({ type: 'granted', generation: `lease-${++window.grants}`, relativePointer: true, microphone: true }) }));
        if (message.type === 'microphone') queueMicrotask(() => this.onmessage?.({ data: JSON.stringify({ ...message }) }));
        if (message.type === 'release') queueMicrotask(() => this.onmessage?.({ data: JSON.stringify({ type: 'revoked', generation: message.generation }) }));
      }
      close() { this.readyState = 'closed'; }
    }
    window.RTCPeerConnection = class {
      constructor() {
        this.connectionState = 'connected';
        const audio = new AudioContext(); const destination = audio.createMediaStreamDestination();
        const oscillator = audio.createOscillator(); oscillator.connect(destination); oscillator.start();
        this.audio = audio;
        this.transceiver = { stopped:false, currentDirection:'sendrecv', direction:'sendrecv',
          receiver:{track:destination.stream.getAudioTracks()[0]},
          sender:{replaceTrack:async track => { window.microphoneTrack = track; if(track) window.lastMicrophoneTrack = track; }} };
        const canvas = document.createElement('canvas'); canvas.width = 960; canvas.height = 540;
        canvas.getContext('2d').fillRect(0,0,960,540);
        const track = canvas.captureStream(1).getVideoTracks()[0];
        setTimeout(() => {
        this.ontrack?.({track}); this.ontrack?.({track:this.transceiver.receiver.track});
        this.ondatachannel?.({ channel: new Channel('remote-control-v1', true) });
        this.ondatachannel?.({ channel: new Channel('remote-motion-v1', false) });
        this.onconnectionstatechange?.();
      }, 0); }
      close() { this.connectionState = 'closed'; void this.audio.close(); }
      getTransceivers() { return [this.transceiver]; }
      async setRemoteDescription(value) { this.remoteDescription = value; }
      async createAnswer() { return {type:'answer',sdp:'synthetic-answer'}; }
      async setLocalDescription(value) { this.localDescription = value; }
    };
    window.WebSocket = class {
      static OPEN = 1;
      constructor(url) {
        if (new URL(url).host !== location.host) throw new Error('External socket denied');
        this.readyState = 1;
        setTimeout(() => {
          this.onmessage?.({ data: JSON.stringify({ type: 'ready', connection_id: 'synthetic-connection' }) });
          this.onmessage?.({ data: JSON.stringify({ type: 'signal', signal:{type:'offer',sdp:'synthetic-offer'} }) });
        }, 0);
      }
      send() {}
      close() { this.readyState = 3; }
    };
  });
  await page.goto(origin);
  const screen = page.getByTestId('remote-screen');
  const take = async () => { if (await page.getByRole('button', { name: 'Release control', exact: true }).count()) return; await page.getByRole('button', { name: 'Take control', exact: true }).click(); await page.getByRole('button', { name: 'Release control', exact: true }).waitFor(); };
  const clear = () => page.evaluate(() => { window.packets.length = 0; window.inputEvents.length = 0; });
  const packets = () => page.evaluate(() => window.packets);
  const center = async () => { const b = await screen.boundingBox(); assert.ok(b); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; };
  const moveCenter = async () => { const p = await center(); await page.mouse.move(p.x, p.y); };
  await take();
  await page.screenshot({ path: join(screenshots, 'windowed.png') });
  for (const order of [['left', 'right'], ['right', 'left']]) {
    await check(`mouse chord ${order.join('+')}: independent release beyond picture with capture fallback`, async () => {
      await moveCenter(); await clear();
      await page.mouse.down({ button: order[0] });
      await page.mouse.down({ button: order[1] });
      assert.equal(await screen.evaluate(el => el.hasPointerCapture(1)), true);
      // The top-left margin is outside the picture; held capture must retain both ups.
      await page.mouse.move(4, 4);
      await page.mouse.up({ button: order[0] });
      await page.mouse.move(8, 8);
      await page.mouse.up({ button: order[1] });
      const actual = (await packets()).filter(p => p.kind === 'button').map(({ button, down }) => ({ button, down }));
      const code = button => button === 'left' ? 0 : 1;
      assert.deepEqual(actual, [{ button: code(order[0]), down: true }, { button: code(order[1]), down: true },
        { button: code(order[0]), down: false }, { button: code(order[1]), down: false }],
        JSON.stringify(await page.evaluate(() => ({events:window.inputEvents, packets:window.packets}))));
      assert.equal((await packets()).some(p => p.kind === 'releaseAll'), false, 'A partial mouse-up must not clear the remaining button');
    });
  }
  await check('outside chord cancellation clears held input; unrelated pointer IDs cannot release it', async () => {
    await moveCenter(); await page.mouse.down({button:'left'}); await page.mouse.down({button:'right'});
    await page.mouse.move(4,4); await page.mouse.up({button:'left'});
    await clear();
    await page.evaluate(() => document.dispatchEvent(new PointerEvent('pointerup', {pointerId:999,pointerType:'mouse',buttons:0,bubbles:true})));
    assert.deepEqual((await packets()).filter(p => ['button','releaseAll'].includes(p.kind)), [], 'An unrelated pointer cannot end this gesture');
    await clear();
    // A real browser cancellation is difficult to request with desktop automation;
    // inject only the lifecycle event after native capture has already been lost.
    await page.evaluate(() => document.dispatchEvent(new PointerEvent('pointercancel', {pointerId:1,pointerType:'mouse',buttons:2,bubbles:true})));
    assert.equal((await packets()).filter(p => p.kind === 'releaseAll').length, 1);
    await clear(); await page.mouse.move(8,8); await page.mouse.up({button:'right'});
    assert.deepEqual(await packets(), [], 'Canceled outside input must not restart the gesture');
  });
  for (const button of ['left', 'right']) await check(`single ${button} release outside picture retains native capture`, async () => {
    await moveCenter(); await clear(); await page.mouse.down({button}); await page.mouse.move(4,4); await page.mouse.up({button});
    assert.deepEqual((await packets()).filter(p=>p.kind==='button').map(p=>[p.button,p.down]), [[button==='left'?0:1,true],[button==='left'?0:1,false]]);
  });
  await check('native wheel forwards remote scroll and prevents page scroll only while controlling', async () => {
    await page.evaluate(() => {
      document.body.style.minHeight = '2400px'; window.scrollTo(0,0); window.wheelEvents = [];
      document.addEventListener('wheel', event => window.wheelEvents.push({defaultPrevented:event.defaultPrevented}), {passive:true});
    });
    try {
      await moveCenter(); await clear(); await page.mouse.wheel(0,160);
      await page.waitForFunction(() => window.wheelEvents.length > 0);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await page.evaluate(() => window.scrollY), 0);
      assert.equal(await page.evaluate(() => window.wheelEvents.at(-1).defaultPrevented), true);
      assert.deepEqual((await packets()).filter(p => p.kind === 'scroll').map(p => [p.deltaX,p.deltaY]), [[0,-160]]);
      await page.getByRole('button', {name:'Release control',exact:true}).click();
      await moveCenter(); await clear(); await page.mouse.wheel(0,160);
      await page.waitForFunction(() => window.scrollY > 0);
      assert.equal(await page.evaluate(() => window.wheelEvents.at(-1).defaultPrevented), false);
      assert.deepEqual((await packets()).filter(p => p.kind === 'scroll'), []);
    } finally {
      await page.evaluate(() => { document.body.style.minHeight = ''; window.scrollTo(0,0); });
      await take();
    }
  });
  await check('physical keyboard forwards from picture; toolbar keyboard stays local', async () => {
    await screen.click(); await clear(); await page.keyboard.press('w');
    assert.deepEqual((await packets()).filter(p => p.kind === 'key').map(p => [p.key, p.down]), [[26, true], [26, false]]);
    await page.getByRole('button', { name: 'All screens', exact: true }).focus(); await clear(); await page.keyboard.press('w');
    assert.deepEqual((await packets()).filter(p => ['key', 'text'].includes(p.kind)), []);
    await page.keyboard.press('Enter'); assert.equal(await page.evaluate(() => window.backCount), 1);
  });
  await check('physical Shift+W chord forwards HID keys, repeat, and exactly one up per key', async () => {
    await screen.click(); await clear();
    await page.evaluate(() => {
      window.keyboardEvents = [];
      document.addEventListener('keydown', event => window.keyboardEvents.push({code:event.code,repeat:event.repeat}), {capture:true});
    });
    try {
      await page.keyboard.down('Shift');
      await page.keyboard.down('KeyW');
      await page.keyboard.down('KeyW'); // Playwright sends a native repeated keydown.
      // Keep the chord down across the production three-second control renewal.
      await page.waitForTimeout(4_000);
      assert.equal((await packets()).some(p => p.type === 'renew'), true);
      await page.keyboard.up('KeyW');
      await page.keyboard.up('Shift');
      assert.deepEqual((await packets()).filter(p => p.kind === 'key').map(p => [p.key,p.down]),
        [[225,true],[26,true],[26,true],[26,false],[225,false]]);
      assert.deepEqual(await page.evaluate(() => window.keyboardEvents),
        [{code:'ShiftLeft',repeat:false},{code:'KeyW',repeat:false},{code:'KeyW',repeat:true}]);
      assert.deepEqual((await packets()).filter(p => p.kind === 'text' || p.kind === 'releaseAll'), []);
    } finally { await page.keyboard.up('KeyW'); await page.keyboard.up('Shift'); }
  });
  await check('native pointer lock emits relative deltas and Escape releases control', async () => {
    await page.getByRole('button', { name: 'Lock mouse', exact: true }).click();
    await page.waitForFunction(() => document.pointerLockElement === document.querySelector('[data-testid="remote-screen"]'));
    await clear(); await page.mouse.move(500, 400); await page.mouse.move(528, 418);
    await page.waitForFunction(() => window.packets.some(p => p.kind === 'relativeMove' && (p.deltaX || p.deltaY)));
    assert.equal((await packets()).some(p => p.kind === 'move'), false);
    for (const order of [['left','right'],['right','left']]) {
      await clear();
      for (const button of order) await page.mouse.down({button});
      for (const button of order) await page.mouse.up({button});
      const codes=order.map(button=>button==='left'?0:1);
      assert.deepEqual((await packets()).filter(p=>p.kind==='button').map(p=>[p.button,p.down]), [[codes[0],true],[codes[1],true],[codes[0],false],[codes[1],false]]);
    }
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => document.pointerLockElement === null);
    await page.getByRole('button', { name: 'Take control', exact: true }).waitFor();
    assert.equal((await packets()).some(p => p.type === 'release'), true);
  });
  await check('native fullscreen keeps visible exit and releases held input on exit', async () => {
    await take(); await page.getByRole('button', { name: 'Fullscreen', exact: true }).click();
    await page.waitForFunction(() => document.fullscreenElement !== null);
    const exit = page.getByRole('button', { name: 'Exit fullscreen', exact: true }); await exit.waitFor({state:'visible'});
    await page.screenshot({ path: join(screenshots, 'fullscreen.png') });
    await screen.click(); await page.keyboard.down('w'); await clear();
    await exit.click(); await page.keyboard.up('w');
    await page.waitForFunction(() => document.fullscreenElement === null);
    await page.getByRole('button', { name: 'Take control', exact: true }).waitFor();
    assert.equal((await packets()).some(p => p.kind === 'releaseAll'), true);
    assert.equal((await packets()).some(p => p.type === 'release'), true);
  });
  await check('focus loss releases held Shift+W and prevents subsequent input', async () => {
    await take(); await screen.click(); await clear();
    try {
      await page.keyboard.down('Shift'); await page.keyboard.down('KeyW');
      assert.deepEqual((await packets()).filter(p => p.kind === 'key').map(p => [p.key,p.down]), [[225,true],[26,true]]);
      const heldGeneration = (await packets()).find(p => p.kind === 'key').generation;
      await clear();
      // Headless browsers do not expose an OS window to focus; dispatch its lifecycle event.
      await page.evaluate(() => window.dispatchEvent(new Event('blur')));
      await page.getByRole('button', { name: 'Take control', exact: true }).waitFor();
      const released = await packets();
      // Releasing the lease tells the host to clear every held key, including Shift.
      assert.deepEqual(released.filter(p => p.type === 'release').map(p => p.generation), [heldGeneration]);
      await clear();
      await page.keyboard.up('KeyW'); await page.keyboard.up('Shift'); await page.keyboard.press('a');
      assert.deepEqual((await packets()).filter(p => ['key', 'text', 'button', 'move', 'relativeMove'].includes(p.kind)), []);
    } finally { await page.keyboard.up('KeyW'); await page.keyboard.up('Shift'); }
  });
  await check('sound toggle updates actual media; microphone opt-in, mute, and release stop capture', async () => {
    const video = page.getByTestId('remote-video');
    assert.equal(await video.evaluate(el => el.muted), true);
    await page.getByRole('button', { name: 'Enable sound', exact: true }).click();
    await page.getByRole('button', { name: 'Mute sound', exact: true }).waitFor();
    assert.equal(await video.evaluate(el => el.muted), false);
    await page.getByRole('button', { name: 'Mute sound', exact: true }).click();
    assert.equal(await video.evaluate(el => el.muted), true);
    await take();
    assert.equal(await page.evaluate(() => Boolean(window.lastMicrophoneTrack)), false, 'Taking control must not capture microphone');
    await page.getByRole('button', { name: 'Enable microphone', exact: true }).click();
    await page.getByRole('button', { name: 'Mute microphone', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.microphoneTrack.readyState), 'live');
    await page.getByRole('button', { name: 'Mute microphone', exact: true }).click();
    await page.waitForFunction(() => window.lastMicrophoneTrack.readyState === 'ended' && window.microphoneTrack === null);
    await page.getByRole('button', { name: 'Enable microphone', exact: true }).click();
    await page.getByRole('button', { name: 'Mute microphone', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Release control', exact: true }).click();
    await page.waitForFunction(() => window.lastMicrophoneTrack.readyState === 'ended' && window.microphoneTrack === null);
  });
  await check('real WebRTC loopback: host ACK gates microphone, microphone tone arrives, sound stays independent, mute/release stop tracks', async () => {
    const loopback = await browser.newPage({ viewport: { width: 1200, height: 1000 } });
    loopback.on('pageerror', error => errors.push(error.message));
    await loopback.route('**/*', async route => {
      const url = new URL(route.request().url());
      if (url.origin !== origin) { externalRequests.push(url.origin); return route.abort(); }
      if (url.pathname === '/v1/account/hands/ice') return route.fulfill({json:{iceServers:[]}});
      if (url.pathname.startsWith('/v1/account/hands/')) return route.fulfill({json:{}});
      return route.continue();
    });
    await loopback.addInitScript(installRemoteControlsLoopback);
    try {
      await loopback.goto(origin);
      await loopback.getByRole('button', {name:'Take control',exact:true}).click();
      await loopback.evaluate(() => window.loopback.audio.resume());
      const enable = loopback.getByRole('button', {name:'Enable microphone',exact:true});
      await enable.waitFor();
      assert.equal(await loopback.evaluate(() => window.loopback.captureRequests), 0);
      await enable.click();
      await loopback.waitForFunction(() => Boolean(window.loopback.ackMicrophone));
      assert.equal(await loopback.evaluate(() => window.loopback.captureRequests), 0, 'No getUserMedia call before matching host ACK');
      await loopback.getByRole('button', {name:'Cancel microphone',exact:true}).click();
      await loopback.evaluate(() => { window.loopback.ackMicrophone(); window.loopback.ackMicrophone = null; });
      await enable.click();
      await loopback.waitForFunction(() => Boolean(window.loopback.ackMicrophone));
      assert.equal(await loopback.evaluate(() => window.loopback.captureRequests), 0, 'A late ACK for canceled capture must be ignored');
      await loopback.evaluate(() => window.loopback.ackMicrophone());
      await loopback.getByRole('button', {name:'Mute microphone',exact:true}).waitFor();
      assert.equal(await loopback.evaluate(() => window.loopback.captureRequests), 1);
      // Prove decoded nonzero audio from the generated fake-device tone, not just RTP.
      const received = await loopback.evaluate(async () => {
        const deadline = performance.now() + 5_000;
        while (performance.now() < deadline) {
          const stats = await window.loopback.returnAudioStats();
          if (stats.some(value => value.bytesReceived > 0 && value.packetsReceived >= 50 && value.totalAudioEnergy > 0)) return stats;
          await new Promise(resolve => setTimeout(resolve, 50));
        }
        throw new Error('Fewer than 50 microphone packets or no decoded audio energy within 5s: '+JSON.stringify({audio:await window.loopback.returnAudioStats(),errors:window.loopback.errors,audioState:window.loopback.audio.state,track:window.loopback.captures.at(-1)?.getAudioTracks()[0]?.getSettings()}));
      });
      assert.equal(received.some(value => value.bytesReceived > 0 && value.packetsReceived >= 50 && value.totalAudioEnergy > 0), true);
      console.log('WebRTC return-audio evidence', JSON.stringify(received));
      assert.equal(await loopback.evaluate(() => window.loopback.returnTrack.readyState), 'live');
      const video = loopback.getByTestId('remote-video');
      assert.equal(await video.evaluate(el => el.muted), true, 'Microphone enable must not enable remote sound');
      await loopback.getByRole('button', {name:'Enable sound',exact:true}).click();
      await loopback.getByRole('button', {name:'Mute sound',exact:true}).waitFor();
      await loopback.getByRole('button', {name:'Mute sound',exact:true}).click();
      assert.equal(await loopback.evaluate(() => window.loopback.captures.at(-1).getAudioTracks()[0].readyState), 'live', 'Speaker mute must not stop microphone');
      // Track shutdown is the privacy contract; WebRTC comfort-noise RTP may continue.
      await loopback.getByRole('button', {name:'Mute microphone',exact:true}).click();
      await loopback.waitForFunction(() => window.loopback.captures.every(stream => stream.getTracks().every(track => track.readyState === 'ended')));
      await loopback.evaluate(() => { window.loopback.ackMicrophone = null; });
      await enable.click();
      await loopback.waitForFunction(() => Boolean(window.loopback.ackMicrophone));
      await loopback.evaluate(() => window.loopback.ackMicrophone());
      await loopback.getByRole('button', {name:'Mute microphone',exact:true}).waitFor();
      await loopback.getByRole('button', {name:'Release control',exact:true}).click();
      await loopback.waitForFunction(() => window.loopback.captures.every(stream => stream.getTracks().every(track => track.readyState === 'ended')));
      assert.deepEqual(await loopback.evaluate(() => window.loopback.errors), []);
    } finally { await loopback.close(); }
  });
  assert.deepEqual(externalRequests, [], 'Fixture must not contact external accounts/sites');
  assert.deepEqual(errors, [], 'No browser runtime errors');
  assert.deepEqual(failures, [], 'All regression cases must pass');
  console.log(`PASS ${results.length} remote controls regressions; no external traffic or page errors`);
} finally {
  try { await browser?.close(); } finally {
    rmSync(audioDirectory, { recursive: true, force: true });
    await new Promise(resolve => server.close(resolve));
  }
}
