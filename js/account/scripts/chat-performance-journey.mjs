// Real-browser chat rendering journey: a long rich history plus a streamed live turn
// through the real controller and terminal view. Records render/CPU cost; asserts behavior only.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const RUNS = Number(process.env.RUNS ?? 3), THROTTLE = Number(process.env.CPU_THROTTLE ?? 4);
const LABEL = process.env.LABEL ?? 'current';

// Voice needs the WASM SDK; this journey only exercises typed transcripts, so a disabled voice stub stands in.
const voiceStub = { name: 'voice-stub', setup(b) {
  b.onResolve({ filter: /^nanocodex-react$/ }, () => ({ path: 'nanocodex-react', namespace: 'voice-stub' }));
  b.onLoad({ filter: /.*/, namespace: 'voice-stub' }, () => ({ loader: 'js', contents: `const idle = Object.freeze({ status: "idle", transcripts: Object.freeze([]), noteTypedInput: async () => {} });
    export const useVoice = () => idle; export const createElevenLabsManager = () => ({}); export const Voice = { voices: [], defaultVoice: "alloy" };` }));
  // One React copy for every package; the profiling build keeps Profiler timings in production.
  b.onResolve({ filter: /^(react|react-dom)(\/.*)?$/ }, args => ({
    path: require.resolve(args.path === 'react-dom/client' ? 'react-dom/profiling' : args.path) }));
} };
// The profiling React build keeps Profiler timings in an otherwise production bundle.
const bundle = await build({ entryPoints: [new URL('fixtures/chat-performance.tsx', import.meta.url).pathname], bundle: true,
  write: false, outdir: 'out', jsx: 'automatic', loader: { '.tsx': 'tsx' }, minify: !process.env.CPU_PROFILE, plugins: [voiceStub],
define: { 'process.env.NODE_ENV': '"production"' } });
const file = ext => bundle.outputFiles.find(f => f.path.endsWith(ext)).text;
const server = createServer((req, res) => {
  if (req.url === '/app.js') { res.setHeader('Content-Type', 'text/javascript'); res.end(file('.js')); return; }
  res.setHeader('Content-Type', 'text/html');
  res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><style>html,body{margin:0;background:#212121;color:#ececec}*{box-sizing:border-box}body{font:14px/1.5 system-ui}#root{--terminal-background:#212121;--terminal-foreground:#ececec;--terminal-muted:#a3a3a3;--terminal-border:#383838;--terminal-hover:#2b2b2b;--terminal-font-sans:system-ui;max-width:900px;margin:auto;height:100vh;display:flex;flex-direction:column}${file('.css')}#root .agent-terminal-workspace,#root .agent-terminal-shell{height:100%;flex:1;min-height:0;border:0}</style><div id="root"></div><script src="/app.js"></script>`);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const output = new URL('../../../output/chat-performance/', import.meta.url); mkdirSync(output, { recursive: true });
const browser = await chromium.launch({ headless: true, ...(process.env.BROWSER_CHANNEL ? { channel: process.env.BROWSER_CHANNEL } : {}) });
const metric = (list, name) => list.metrics.find(m => m.name === name)?.value ?? 0;
const stats = values => { const s = [...values].sort((a, b) => a - b); const at = q => s[Math.min(s.length - 1, Math.floor(q * s.length))] ?? 0;
  return { count: s.length, total: +s.reduce((a, b) => a + b, 0).toFixed(1), p50: +at(0.5).toFixed(2), p95: +at(0.95).toFixed(2), max: +(s.at(-1) ?? 0).toFixed(2) }; };
const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];

// Emits events [from, to) one per animation frame and records live-state observations.
async function stream(page, from, to) {
  return page.evaluate(async ([from, to]) => {
    const events = window.liveSequence, seen = {};
    for (let i = from; i < to; i++) {
      window.perf.emit(events[i]);
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      if (events[i].payload.call_id === 'live-cmd' && events[i].type === 'tool.call') {
        seen.liveStatus = document.querySelector('.agent-live-status [role="status"]')?.textContent;
        seen.liveGroupOpen = [...document.querySelectorAll('.agent-work')].at(-1)?.querySelector('details')?.open;
      }
    }
    return seen;
  }, [from, to]);
}

const runs = [];
try {
  for (let run = 0; run < RUNS; run++) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce' });
    const page = await context.newPage(); const errors = [];
    page.on('pageerror', error => errors.push(error.message)); page.on('console', m => m.type() === 'error' && errors.push(m.text()));
    const cdp = await context.newCDPSession(page);
    await cdp.send('Performance.enable');
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.locator('.agent-dom-transcript').waitFor({ timeout: 10000 }).catch(error => { throw new Error(`${error.message}\n${errors.join('\n')}`); });
    await cdp.send('Emulation.setCPUThrottlingRate', { rate: THROTTLE });
    await page.evaluate(() => { window.longTasks = []; new PerformanceObserver(list => list.getEntries()
      .forEach(e => window.longTasks.push({ phase: window.perf.phase, duration: e.duration }))).observe({ type: 'longtask' }); });

    // History: a capped 200-entry transcript of rich answers, grouped tools, subagents and previews.
    const historyMs = await page.evaluate(async () => { window.perf.phase = 'history'; const start = performance.now();
      window.perf.history(30);
      await new Promise(resolve => { const check = () => document.body.innerText.includes('Turn 29 summary') ? resolve() : requestAnimationFrame(check); check(); });
      await new Promise(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
      return performance.now() - start; });
    assert.ok(await page.locator('.agent-work-group').count() > 10, 'History renders grouped work');
    assert.ok(await page.locator('.agent-subagent').count() >= 3, 'History renders subagents');
    assert.ok(await page.getByRole('link', { name: /Open preview/ }).count() >= 1, 'Preview cards stay visible');
    const sticksToTail = await page.locator('.agent-dom-transcript').evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight < 48);
    assert.ok(sticksToTail, 'Loaded history follows the tail');
    await page.waitForTimeout(500);

    // Live turn streamed one event per frame; pause mid-answer to read history.
    const total = await page.evaluate(() => { window.liveSequence = window.perf.liveEvents(); window.perf.phase = 'stream'; return window.liveSequence.length; });
    const pauseAt = await page.evaluate(() => window.liveSequence.findIndex(e => e.type === 'assistant.delta' && e.payload.item_id === 'a-live') + 40);
    if (process.env.CPU_PROFILE && run === 0) { await cdp.send('Profiler.enable'); await cdp.send('Profiler.start'); }
    const before = await cdp.send('Performance.getMetrics');
    const seen = await stream(page, 0, pauseAt);
    assert.match(seen.liveStatus ?? '', /Running command/, 'Live status names the running tool');
    assert.equal(seen.liveGroupOpen, true, 'Live work group is open');
    const transcript = page.locator('.agent-dom-transcript');
    await transcript.evaluate(el => { el.scrollTop = el.scrollHeight / 2; el.dispatchEvent(new Event('scroll')); });
    const anchor = page.locator('.agent-dom-transcript-inner > *').nth(60);
    await anchor.scrollIntoViewIfNeeded();
    const anchorTop = await anchor.evaluate(el => el.getBoundingClientRect().top);
    await stream(page, pauseAt, total - 2);
    const after = await cdp.send('Performance.getMetrics');
    if (process.env.CPU_PROFILE && run === 0) writeFileSync(new URL(`${LABEL}.cpuprofile`, output), JSON.stringify((await cdp.send('Profiler.stop')).profile));
    assert.ok(Math.abs(await anchor.evaluate(el => el.getBoundingClientRect().top) - anchorTop) <= 2, 'Reading position preserved while streaming');
    await page.getByRole('button', { name: 'Jump to latest response' }).click();
    await page.waitForFunction(() => { const el = document.querySelector('.agent-dom-transcript'); return el.scrollHeight - el.scrollTop - el.clientHeight < 48; });
    await page.evaluate(() => { window.perf.phase = 'complete'; });
    await stream(page, total - 2, total);
    await page.waitForFunction(() => !document.querySelector('.agent-live-status'));
    const last = page.locator('.agent-terminal-markdown.is-assistant').last();
    assert.match(await last.innerText(), /Rerun summary[\s\S]*svc-99-4/);
    assert.equal(await page.locator('.agent-work-group').last().getAttribute('open'), null, 'Finished work collapses');
    assert.match(await page.locator('.agent-subagent').last().locator(':scope > summary').innerText(), /reviewer · Agent 900[\s\S]*finished/);
    assert.deepEqual(errors, []);

    const data = await page.evaluate(() => ({ commits: window.perf.commits, longTasks: window.longTasks }));
    const phase = name => data.commits.filter(c => c.phase === name).map(c => c.actual);
    const longStream = data.longTasks.filter(t => t.phase === 'stream').map(t => t.duration);
    const delta = name => +((metric(after, name) - metric(before, name)) * 1000).toFixed(1);
    runs.push({ run, historyMs: +historyMs.toFixed(1), historyRender: stats(phase('history')), streamEvents: total - 2,
      streamRender: stats(phase('stream')), streamLongTasks: stats(longStream),
      streamCpuMs: { script: delta('ScriptDuration'), layout: delta('LayoutDuration'), style: delta('RecalcStyleDuration'), task: delta('TaskDuration') },
      layoutCount: metric(after, 'LayoutCount') - metric(before, 'LayoutCount'), domNodes: metric(after, 'Nodes') });
    if (run === 0) await page.screenshot({ path: new URL(`${LABEL}-final.png`, output).pathname });
    await context.close();
  }
  const summary = { label: LABEL, cpuThrottle: THROTTLE, runs: RUNS,
    median: { historyMs: median(runs.map(r => r.historyMs)), historyRenderTotal: median(runs.map(r => r.historyRender.total)),
      streamRenderTotal: median(runs.map(r => r.streamRender.total)), streamRenderP95: median(runs.map(r => r.streamRender.p95)),
      streamRenderMax: median(runs.map(r => r.streamRender.max)), streamScriptMs: median(runs.map(r => r.streamCpuMs.script)),
      streamLayoutMs: median(runs.map(r => r.streamCpuMs.layout)), streamStyleMs: median(runs.map(r => r.streamCpuMs.style)),
      streamTaskMs: median(runs.map(r => r.streamCpuMs.task)), streamLongTasks: median(runs.map(r => r.streamLongTasks.count)),
      streamLongTaskMs: median(runs.map(r => r.streamLongTasks.total)) }, details: runs };
  writeFileSync(new URL(`${LABEL}.json`, output), JSON.stringify(summary, null, 2)); console.log(JSON.stringify(summary.median, null, 2));
} finally { await browser.close(); server.close(); }
