// Manual CUA/browser smoke fixture for the production Screen and real WebRTC.
// Run from the repository: node js/account/scripts/remote-performance-fixture.mjs
// Build-only check without opening a server: add --check
// Account signaling is synthetic and localhost-only. No credentials or physical
// capture devices are used. The animated source targets 60 FPS by default;
// add ?fps=30 (1–120) to compare rates. Loopback excludes real capture/network costs.
// Presentation regression: ?prepare=1 decodes inside a hidden Screen before the
// Select prepared screen button reveals the same video. The default is a cold
// modal viewer. Both must show animation without Fullscreen or Take control.
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { installRemoteControlsLoopback } from './fixtures/remote-controls-loopback.mjs';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const hand = { id: 'synthetic-screen', name: 'Synthetic desktop', kind: 'desktop', width: 960, height: 540,
  controllable: true, machine_id: 'synthetic-machine', machine_name: 'Synthetic computer', generation: 'synthetic-generation' };
const bundle = await build({
  stdin: { contents: `import React, {useEffect, useRef, useState} from 'react'; import {createRoot} from 'react-dom/client';
    import {Screen} from './src/RemoteScreens';
    const frameRate = Number(new URLSearchParams(location.search).get('fps') ?? 60);
    (${installRemoteControlsLoopback.toString()})({ frameRate, animated: true });
    document.getElementById('fixture-title').textContent = 'Real WebRTC loopback · animated ' + frameRate + ' FPS target';
    const Socket = window.WebSocket;
    window.WebSocket = class extends Socket {
      send(raw) {
        if (JSON.parse(raw).type === 'ping') {
          queueMicrotask(() => this.onmessage?.({ data: JSON.stringify({type:'renewed'}) }));
        } else super.send(raw);
      }
    };
    const hand = ${JSON.stringify(hand)};
    function Fixture() {
      const dialog = useRef(null);
      const [preparing, setPreparing] = useState(new URLSearchParams(location.search).get('prepare') === '1');
      useEffect(() => { dialog.current.showModal(); }, []);
      return <dialog ref={dialog} className="remote-screens" aria-label="Synthetic remote screen">
        <header><h2>Modal video presentation regression</h2></header>
        {preparing && <div className="remote-screen-list"><p>Allow the source to run before selecting.</p>
          <button onClick={() => setPreparing(false)}>Select prepared screen</button></div>}
        <Screen hand={hand} preparing={preparing} onBack={()=>location.reload()} />
        <p id="presentation-status" style={{margin:8,height:40,flexShrink:0,overflow:"hidden"}}>Preparing source…</p>
      </dialog>;
    }
    createRoot(document.getElementById('root')).render(<Fixture />);
    setInterval(() => {
      const events = window.loopback?.channels ?? [];
      const video = document.querySelector('[data-testid="remote-video"]');
      if (video && !window.presentationVideo) window.presentationVideo = video;
      const status = document.getElementById('presentation-status');
      if (status) status.textContent = 'Same video: ' + (video === window.presentationVideo) +
        ' · readyState: ' + (video?.readyState ?? 0) + ' · time: ' + (video?.currentTime ?? 0).toFixed(1) +
        ' · source frames: ' + (window.loopback?.sourceFrames ?? 0) +
        ' · acquire/input packets: ' + events.filter(e => e.type === 'acquire' || e.kind).length;
      document.getElementById('input-status').textContent = 'Host received ' + events.filter(e=>e.kind).length +
        ' input events. Last event: ' + (events.at(-1)?.kind ?? events.at(-1)?.type ?? 'none') +
        '. Source frames: ' + (window.loopback?.sourceFrames ?? 0) +
        '. Errors: ' + (window.loopback?.errors.join('; ') || 'none');
    }, 250);`, resolveDir: new URL('..', import.meta.url).pathname, loader: 'tsx' },
  bundle: true, write: false, outfile: 'app.js', jsx: 'automatic',
  alias: { 'nanocodex-connect-ui/browserAccountSession': new URL('../../nanocodex-connect-ui/src/browserAccountSession.ts', import.meta.url).pathname },
});
const javascript = bundle.outputFiles.find(file => file.path.endsWith('.js')).text;
const css = bundle.outputFiles.find(file => file.path.endsWith('.css'))?.text ?? '';
const server = createServer((req, res) => {
  res.setHeader('Cache-Control', 'no-store');
  if (req.url === '/app.js') { res.setHeader('Content-Type', 'text/javascript'); res.end(javascript); }
  else if (req.url === '/app.css') { res.setHeader('Content-Type', 'text/css'); res.end(css); }
  else if (req.url?.startsWith('/v1/account/hands/')) {
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify(req.url.endsWith('/ice') ? { iceServers: [] } : req.url.endsWith('/screens') ? { surfaces: [hand] } : {}));
  } else {
    res.setHeader('Content-Type', 'text/html');
    res.end('<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Remote performance fixture</title><link rel="stylesheet" href="/app.css"><style>body{margin:16px;font:14px system-ui;background:#e8eaee}*{box-sizing:border-box}.remote-screens{height:calc(100dvh - 110px);max-height:none}h1{font-size:16px;margin:0 0 8px}p{margin:8px 0}</style><h1 id="fixture-title">Real WebRTC loopback · animated 60 FPS target</h1><div id="root"></div><p id="input-status" aria-live="polite">Waiting for the synthetic host</p><script src="/app.js"></script></html>');
  }
});
if (process.argv.includes('--check')) console.log('Remote presentation fixture bundle passed');
else server.listen(0, '127.0.0.1', () => console.log(`Remote performance fixture: http://127.0.0.1:${server.address().port}`));
