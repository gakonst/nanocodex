// Real-browser journey for the /agents chat: sidebar and transcript layout, collapsed
// tool activity while streaming, the composer (auto-grow, Enter/Shift+Enter, IME, stop),
// attachments by drag-and-drop, paste and picker delivered as protocol-valid prompt
// input, and readable error notices. Desktop and mobile, dark and light.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
// The managed service's own prompt validator judges what the browser submits.
const protocol = await build({ entryPoints: [new URL('../../managed/src/protocol.ts', import.meta.url).pathname], bundle: true, write: false, format: 'esm', platform: 'node' });
const { validatePromptInput } = await import(`data:text/javascript;base64,${Buffer.from(protocol.outputFiles[0].text).toString('base64')}`);
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));

// Voice needs the WASM SDK; this journey exercises typed turns, so a disabled stub stands in.
const voiceStub = { name: 'voice-stub', setup(b) {
  b.onResolve({ filter: /^nanocodex-react$/ }, args => /nanocodex-terminal/.test(args.importer) ? { path: 'nanocodex-react', namespace: 'voice-stub' } : undefined);
  b.onLoad({ filter: /.*/, namespace: 'voice-stub' }, () => ({ loader: 'js', contents: `const idle = Object.freeze({ status: "idle", transcripts: Object.freeze([]), noteTypedInput: async () => {} });
    export const useVoice = () => idle; export const createElevenLabsManager = () => ({}); export const Voice = { voices: ["cove", "maple"], defaultVoice: "cove" };` }));
  // The brand mark's module also preloads every route; a static mark stands in.
  b.onResolve({ filter: /\/MainNavigation$/ }, () => ({ path: 'main-navigation', namespace: 'mark-stub' }));
  b.onLoad({ filter: /.*/, namespace: 'mark-stub' }, () => ({ loader: 'js', resolveDir: new URL('..', import.meta.url).pathname,
    contents: `import { createElement } from "react"; export const NanocodexMark = () => createElement("svg", { className: "nanocodex-mark", viewBox: "0 0 24 24", "aria-hidden": true }); export const MainNavigationLinks = () => null;` }));
  // Node-only SSH dependencies reachable from the sidebar's imports are never executed here.
  b.onResolve({ filter: /^(node:.*|fs|crypto|stream|net|tls|os|path|util|buffer|events|zlib|child_process|node-rsa)$/ }, () => ({ path: 'node-builtin', namespace: 'empty' }));
  b.onLoad({ filter: /.*/, namespace: 'empty' }, () => ({ loader: 'js', contents: 'module.exports = {};' }));
  // The real app stylesheet references public assets such as /paradigm-mark.svg by absolute URL.
  b.onResolve({ filter: /^\/[^/]/ }, args => args.kind === 'url-token' ? { path: args.path, external: true } : undefined);
  b.onResolve({ filter: /^(react|react-dom)(\/.*)?$/ }, args => ({ path: require.resolve(args.path) }));
} };
const bundle = await build({ entryPoints: [new URL('fixtures/agents-redesign.tsx', import.meta.url).pathname], bundle: true,
  write: false, outdir: 'out', format: 'esm', jsx: 'automatic', loader: { '.tsx': 'tsx', '.woff2': 'empty', '.svg': 'dataurl' }, plugins: [voiceStub],
  define: { 'process.env.NODE_ENV': '"production"' } });
const file = ext => bundle.outputFiles.find(f => f.path.endsWith(ext)).text;
const server = createServer((req, res) => {
  if (req.url === '/app.js') { res.setHeader('Content-Type', 'text/javascript'); res.end(file('.js')); return; }
  res.setHeader('Content-Type', 'text/html');
  res.end(`<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><style>html,body{margin:0;height:100%}*{box-sizing:border-box}button{border:0}${file('.css')}</style><div id="root"></div><script type="module" src="/app.js"></script>`);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const output = new URL('../../../output/agents-redesign/', import.meta.url); mkdirSync(output, { recursive: true });
const browser = await chromium.launch({ headless: true, ...(process.env.BROWSER_CHANNEL ? { channel: process.env.BROWSER_CHANNEL } : {}) });
const evidence = [];
const log = (name, detail) => { evidence.push({ step: name, ...detail }); };

// In-page helpers: synthesize real File objects and dispatch native drag/paste events.
const helpers = () => {
  window.makePng = async (name, size = 64) => {
    const canvas = Object.assign(document.createElement('canvas'), { width: size, height: size });
    const context = canvas.getContext('2d'); context.fillStyle = '#e66'; context.fillRect(0, 0, size, size);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    return new File([blob], name, { type: 'image/png' });
  };
  window.dropFiles = async (selector, files) => {
    const target = document.querySelector(selector); const data = new DataTransfer();
    for (const item of await Promise.all(files)) data.items.add(item);
    for (const type of ['dragenter', 'dragover']) target.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: data }));
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    const overlay = Boolean(document.querySelector('.agent-composer-drop'));
    const drop = new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: data });
    target.dispatchEvent(drop);
    return { overlay, prevented: drop.defaultPrevented };
  };
  window.pasteFiles = async (files) => {
    const data = new DataTransfer(); for (const item of await Promise.all(files)) data.items.add(item);
    const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: data });
    document.querySelector('.agent-composer textarea').dispatchEvent(event);
    return event.defaultPrevented;
  };
};

const css = (locator, property) => locator.evaluate((el, p) => getComputedStyle(el)[p], property);
const isReddish = color => { const [r, g, b] = color.match(/[\d.]+/g).map(Number); return r > 150 && r > g * 1.6 && r > b * 1.6; };
const frames = page => page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));

const scenarios = [['desktop-dark', { width: 1280, height: 860 }, false, 'dark'], ['desktop-light', { width: 1280, height: 860 }, false, 'light'],
  ['mobile-dark', { width: 390, height: 844 }, true, 'dark'], ['mobile-light', { width: 390, height: 844 }, true, 'light'],
  ['android-dark', { width: 360, height: 780 }, true, 'dark']];
const MOBILE_UA = { 'mobile-dark': 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
  'android-dark': 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36' };
// Phone ergonomics: inputs never trigger iOS focus zoom, every visible control is a 44px target,
// and nothing overflows the viewport except inside its own horizontal scroller.
const mobileAudit = () => {
  const vw = innerWidth; const out = { smallText: [], smallTargets: [], overflow: [] };
  const shown = el => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none' && !el.closest('[inert]'); };
  for (const el of document.querySelectorAll('input:not([type=file]), textarea, select')) if (shown(el) && parseFloat(getComputedStyle(el).fontSize) < 16) out.smallText.push(`${el.tagName}.${el.className} ${getComputedStyle(el).fontSize}`);
  for (const el of document.querySelectorAll('button, a[href], summary, [role=button]')) {
    if (!shown(el) || (el.tagName === 'A' && el.closest('p, li, td'))) continue;
    const r = el.getBoundingClientRect(); if (r.width < 43.5 || r.height < 43.5) out.smallTargets.push(`${el.getAttribute('aria-label') || el.innerText.trim().slice(0, 30)} ${Math.round(r.width)}x${Math.round(r.height)}`);
  }
  for (const el of document.querySelectorAll('body *')) {
    if (!shown(el) || el.getBoundingClientRect().right <= vw + 1) continue;
    let p = el.parentElement, contained = false;
    while (p) { if (/auto|scroll|hidden|clip/.test(getComputedStyle(p).overflowX)) { contained = p.getBoundingClientRect().right <= vw + 1; break; } p = p.parentElement; }
    if (!contained) out.overflow.push(`${el.tagName}.${el.className}`);
  }
  return out;
};
try {
  for (const [name, viewport, mobile, theme] of scenarios) {
    const context = await browser.newContext({ viewport, isMobile: mobile, hasTouch: mobile, reducedMotion: 'reduce', ...(MOBILE_UA[name] ? { userAgent: MOBILE_UA[name], deviceScaleFactor: 3 } : {}) });
    await context.tracing.start({ screenshots: true, snapshots: true });
    const page = await context.newPage(); const errors = [];
    page.on('pageerror', error => errors.push(error.message)); page.on('console', m => m.type() === 'error' && errors.push(m.text()));
    await page.addInitScript(helpers);
    await page.addInitScript(value => { const set = () => { document.documentElement.dataset.theme = value; }; if (document.documentElement) set(); else document.addEventListener("readystatechange", set, { once: true }); }, theme);
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    const shot = label => page.screenshot({ path: new URL(`${name}-${label}.png`, output).pathname });
    const textarea = page.locator('.agent-composer textarea');
    await textarea.waitFor();
    const fixture = (method, ...args) => page.evaluate(([m, a]) => window.fixture[m](...a), [method, args]);
    const prompts = () => page.evaluate(() => window.fixture.prompts);

    // Layout: persistent sidebar on desktop, drawer on mobile; centered column; composer pinned at the bottom.
    const sidebar = await page.locator('.agent-navigation').boundingBox();
    if (mobile) assert.ok(sidebar === null || sidebar.x + sidebar.width <= 0 || await page.locator('.agent-navigation').evaluate(el => getComputedStyle(el).visibility === 'hidden'), 'Mobile sidebar is a closed drawer');
    else assert.ok(sidebar.width >= 240 && sidebar.width <= 264, `Sidebar width ${sidebar.width}`);
    const main = await page.locator('.conversation-main').boundingBox();
    const form = await page.locator('form.agent-composer').boundingBox();
    assert.ok(form.y + form.height >= viewport.height - (mobile ? 16 : 48), `Composer pinned to the bottom (${form.y + form.height})`);
    assert.ok(Math.abs((form.x - main.x) - (main.x + main.width - form.x - form.width)) <= 2, 'Composer is centered');
    // Homepage palette: the chat paints with the same --surface/--text/--brand-* tokens as /.
    const palette = await page.evaluate(() => {
      const paint = value => { const probe = document.body.appendChild(Object.assign(document.createElement('i'), { style: `color:${value}` }));
        const color = getComputedStyle(probe).color; probe.remove(); return color; };
      const workspace = getComputedStyle(document.querySelector('.chat-workspace'));
      return { surface: paint('var(--surface)'), text: paint('var(--text)'), accent: paint('var(--brand-accent)'),
        background: workspace.backgroundColor, color: workspace.color,
        marker: getComputedStyle(document.querySelector('.agent-navigation-thread[aria-current="location"]'), '::before').backgroundColor,
        radius: getComputedStyle(document.querySelector('.agent-touch-field')).borderRadius };
    });
    assert.equal(palette.background, palette.surface, 'Chat background is the homepage surface');
    assert.equal(palette.color, palette.text, 'Chat text is the homepage text color');
    if (!mobile) assert.equal(palette.marker, palette.accent, 'Selection marker uses the homepage accent');
    assert.equal(palette.radius, '18px', 'Composer is a rounded homepage-style panel');
    log(`${name}:palette`, palette);

    // Header: one row, readable title; secondary actions collapse into a menu on mobile.
    const header = await page.locator('.agent-chat-header').boundingBox();
    assert.ok(header.height <= 60, `Header is a single row (${header.height})`);
    const heading = await page.locator('.agent-chat-heading strong').boundingBox();
    if (mobile) {
      assert.ok(heading.width >= 120, `Mobile title has room (${heading.width})`);
      assert.equal(await page.locator('.agent-chat-secondary').isVisible(), false, 'Secondary actions are hidden behind More');
      await page.getByRole('button', { name: 'More actions' }).click();
      const items = page.locator('.agent-chat-secondary.is-open > button');
      assert.equal(await items.count(), 4, 'More menu lists secondary actions');
      for (const box of await items.evaluateAll(els => els.map(e => e.getBoundingClientRect().toJSON()))) {
        assert.ok(box.height >= 44 && box.right <= viewport.width && box.left >= 0, `Menu item fits (${JSON.stringify(box)})`);
      }
      await shot('header-menu');
      await page.locator('.agent-chat-menu-backdrop').click({ position: { x: 10, y: 400 } });
      assert.equal(await page.locator('.agent-chat-secondary.is-open').count(), 0, 'Backdrop closes the menu');
    } else {
      assert.equal(await page.getByRole('button', { name: 'More actions' }).isVisible(), false, 'Desktop shows actions inline');
      assert.equal(await page.locator('.agent-chat-secondary > button').first().isVisible(), true, 'Desktop secondary actions visible');
    }

    // Sidebar: no Home entry (the brand mark already links home); composer shows no voice name.
    assert.equal(await page.locator('.agent-navigation-primary').getByText('Home', { exact: true }).count(), 0, 'Sidebar has no Home button');
    assert.equal(await page.locator('.agent-voice-select').count(), 0, 'No inline voice picker');
    assert.doesNotMatch(await page.locator('form.agent-composer').innerText(), /cove|maple|voice active/i, 'Composer never shows the voice name');
    assert.equal(await page.getByRole('button', { name: 'Voice settings' }).count(), 1, 'Voice preferences stay reachable');
    const toolbar = await page.locator('.agent-composer-toolbar').evaluate(el => [...el.querySelectorAll(':scope > button, :scope > .agent-runtime-controls > button, .agent-voice-control button')]
      .map(b => { const r = b.getBoundingClientRect(); return { label: b.getAttribute('aria-label'), top: Math.round(r.top), bottom: Math.round(r.bottom) }; }));
    const centers = toolbar.map(b => (b.top + b.bottom) / 2);
    assert.ok(Math.max(...centers) - Math.min(...centers) <= 1, `Composer controls share one baseline ${JSON.stringify(toolbar)}`);
    const sendIdle = page.getByRole('button', { name: 'Send message' });
    await textarea.fill('x');
    for (let i = 0; i < 40 && await css(sendIdle, 'backgroundColor') !== palette.text; i++) await page.waitForTimeout(25);
    assert.equal(await css(sendIdle, 'backgroundColor'), palette.text, 'Send is the homepage primary button');
    await textarea.fill('');

    if (mobile) {
      await page.getByRole('button', { name: 'Open sidebar' }).click();
      await page.locator('.agent-navigation.is-open').waitFor();
      assert.match(await page.locator('.agent-navigation-thread[aria-current="location"]').innerText(), /Fix the release check[\s\S]*Running/);
      await shot('drawer');
      await page.getByRole('button', { name: 'Close sidebar' }).click();
    }

    // Composer: grows with content up to a bound; Shift+Enter (desktop) or Enter (phone) adds a line.
    await textarea.click();
    const base = (await textarea.boundingBox()).height;
    await textarea.pressSequentially('first line');
    for (let i = 0; i < 3; i++) { await page.keyboard.press(mobile ? 'Enter' : 'Shift+Enter'); await textarea.pressSequentially(`line ${i + 2}`); }
    const grown = (await textarea.boundingBox()).height;
    assert.ok(grown > base + 30, `Composer grows (${base} → ${grown})`);
    assert.equal(await textarea.inputValue(), 'first line\nline 2\nline 3\nline 4');
    assert.equal((await prompts()).length, 0, 'Newline keys never send');
    await textarea.fill(Array.from({ length: 40 }, (_, i) => `row ${i}`).join('\n'));
    const capped = (await textarea.boundingBox()).height;
    assert.ok(capped <= (mobile ? 170 : 290) && await css(textarea, 'overflowY') === 'auto', `Composer height is bounded (${capped})`);
    await textarea.fill('');
    assert.ok((await textarea.boundingBox()).height <= base + 1, 'Composer shrinks back');

    // IME: Enter that confirms a composition never sends.
    await textarea.fill('かな');
    await textarea.evaluate(el => { el.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
      el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true, cancelable: true })); });
    assert.equal((await prompts()).length, 0, 'IME Enter does not send');
    await textarea.evaluate(el => el.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: 'かな' })));
    await page.waitForTimeout(60); // a real Enter cannot follow compositionend this quickly
    await textarea.fill('');
    log(`${name}:composer`, { base, grown, capped });

    // Send: Enter on desktop, the Send button on a phone keyboard.
    const send = async text => {
      await textarea.fill(text);
      if (mobile) await page.getByRole('button', { name: 'Send message' }).click(); else await textarea.press('Enter');
      await frames(page);
    };
    const composerBox = async () => { const box = await page.locator('form.agent-composer').boundingBox(); return { y: Math.round(box.y), h: Math.round(box.height) }; };
    const restingComposer = await composerBox();
    await send('Run the release check');
    assert.deepEqual(await prompts(), ['Run the release check']);
    assert.equal(await textarea.inputValue(), '', 'Draft clears after sending');

    // Streaming: tool activity is one collapsed summary row with a spinner; nothing auto-expands.
    await fixture('play', 10); await frames(page);
    const group = page.locator('.agent-work').last();
    const details = group.locator('details.agent-work-group');
    await details.waitFor();
    assert.equal(await details.getAttribute('open'), null, 'Live work group stays collapsed');
    assert.equal(await page.locator('.agent-work-body').count(), 0, 'Collapsed rows are not rendered');
    assert.ok(await details.locator('summary .agent-tool-status-icon.is-running').count() === 1, 'Running spinner');
    assert.match(await details.locator(':scope > summary').innerText(), /Working[\s\S]*pnpm test --watch=false/);
    assert.equal(await page.getByRole('button', { name: 'Send message' }).count(), 0, 'Empty draft while running: Stop only');
    assert.match(await page.getByRole('button', { name: 'Stop response' }).getAttribute('class'), /is-primary/);
    // Stability: the composer does not move when a turn starts, and summaries keep one height as timers tick.
    assert.deepEqual(await composerBox(), restingComposer, 'Composer stays put while a tool runs');
    const summaryHeight = async () => Math.round((await details.locator(':scope > summary').boundingBox()).height);
    const runningSummary = await summaryHeight();
    const liveHeight = Math.round((await page.locator('.agent-live-status').boundingBox())?.height ?? 0);
    await page.waitForTimeout(1100); // the live clock ticks
    assert.equal(await summaryHeight(), runningSummary, 'Running summary height is stable across ticks');
    await textarea.fill('next');
    assert.equal(await page.getByRole('button', { name: 'Send message' }).count(), 1, 'A draft shows Send beside Stop');
    await textarea.fill('');
    await shot('streaming');
    await fixture('play', 5); await frames(page);
    assert.equal(await details.getAttribute('open'), null, 'Still collapsed after more tool events');
    await fixture('drain'); await page.locator('.agent-live-status').waitFor({ state: 'detached' });
    assert.equal(await summaryHeight(), runningSummary, 'Finishing does not resize the work summary');
    assert.deepEqual(await composerBox(), restingComposer, 'Composer stays put after the turn');
    log(`${name}:stability`, { composer: restingComposer, summary: runningSummary, liveStatus: liveHeight });
    const summary = await details.locator(':scope > summary').innerText();
    assert.match(summary, /Worked[\s\S]*2 commands[\s\S]*1 failed/);
    assert.equal(await details.getAttribute('open'), null, 'Finished work stays collapsed');
    const answer = page.locator('.agent-terminal-markdown.is-assistant').last();
    assert.ok(await answer.locator('table').count() === 1 && await answer.locator('pre').count() >= 1, 'Tables and code render');

    // Expanding: rows are single collapsed lines; a failure carries a red marker; details on demand.
    await details.locator(':scope > summary').click();
    const rows = details.locator('.agent-work-body > .agent-tool-row:not(.is-thinking)');
    await rows.first().waitFor({ timeout: 5000 }).catch(() => {});
    assert.equal(await rows.count(), 3);
    assert.equal(await details.locator('.agent-tool-row > details[open]').count(), 0, 'Rows start collapsed');
    assert.equal(await page.locator('.agent-tool-error-line').count(), 0, 'No error text outside the row');
    const failedRow = details.locator('.agent-tool-row.is-failed');
    assert.ok(isReddish(await css(failedRow.locator('.agent-tool-status-icon.is-failed'), 'color')), 'Failed marker is red');
    assert.ok(!isReddish(await css(rows.first().locator('.agent-tool-status-icon'), 'color')), 'Success marker is monochrome');
    const sizes = await details.locator('.agent-tool-row > details > summary').evaluateAll(list => list.map(el => ({ h: el.getBoundingClientRect().height, o: el.scrollWidth - el.clientWidth, t: el.innerText.replace(/\s+/g, ' ') })));
    for (const row of sizes) assert.ok((mobile ? row.h >= 44 && row.h <= 46 : row.h <= 30) && row.o <= 1, `Compact single-line row (44px touch target on phones) ${JSON.stringify(row)}`);
    assert.equal(new Set(sizes.map(row => row.h)).size, 1, 'Every tool row has the same height, whatever its status or duration');
    await failedRow.locator(':scope > details > summary').click();
    assert.match(await failedRow.locator('.agent-tool-terminal').innerText(), /\$ pnpm test release\.test\.ts[\s\S]*FAIL release\.test\.ts[\s\S]*Exit code 1/);
    await shot('expanded');
    log(`${name}:activity`, { summary: summary.replace(/\s+/g, ' '), rows: sizes });
    if (mobile) {
      const audit = await page.evaluate(mobileAudit);
      assert.deepEqual(audit, { smallText: [], smallTargets: [], overflow: [] }, `Phone ergonomics ${JSON.stringify(audit)}`);
      log(`${name}:ergonomics`, audit);
    }

    // Attachments: drop on the composer, drop on the transcript, paste, and the picker.
    const chips = page.locator('.agent-composer-chip:not(.is-preparing)');
    const dropped = await page.evaluate(() => window.dropFiles('form.agent-composer', [window.makePng('screen.png')]));
    assert.ok(dropped.overlay && dropped.prevented, 'Drop target highlights and consumes the drop');
    await chips.nth(0).waitFor();
    await page.evaluate(() => window.dropFiles('.agent-dom-transcript', [new File(['# Notes\nship it\n'], 'notes.md', { type: 'text/markdown' })]));
    await chips.nth(1).waitFor();
    assert.equal(await page.evaluate(() => window.pasteFiles([window.makePng('clip.png', 32)])), true, 'Image paste is consumed');
    await chips.nth(2).waitFor();
    await page.locator('.agent-composer input[type="file"]').setInputFiles({ name: 'config.json', mimeType: 'application/json', buffer: Buffer.from('{"region":"eu-west"}') });
    await chips.nth(3).waitFor();
    assert.ok(await chips.nth(0).locator('img[src^="data:image/png;base64,"]').count() === 1, 'Image chip shows a thumbnail');
    await shot('attachments');
    await page.getByRole('button', { name: 'Remove clip.png' }).click();
    assert.equal(await chips.count(), 3, 'A chip can be removed');
    await page.evaluate(() => window.dropFiles('form.agent-composer', [new File([new Uint8Array([0, 1, 2])], 'tool.exe', { type: 'application/octet-stream' })]));
    assert.match(await page.locator('.agent-composer-notice').innerText(), /tool\.exe: attach images, text or code files/);
    await send('attach these');
    const sent = (await prompts()).at(-1);
    assert.ok(Array.isArray(sent), 'Attachments send structured input');
    assert.deepEqual(sent.map(item => item.type), ['text', 'image', 'text', 'text']);
    assert.equal(sent[0].text, 'attach these');
    assert.match(sent[1].image_url, /^data:image\/png;base64,/);
    assert.match(sent[2].text, /^<attached_file name="notes\.md" media_type="text\/markdown">\n# Notes\nship it\n/);
    assert.match(sent[3].text, /name="config\.json"[\s\S]*"region":"eu-west"/);
    validatePromptInput(sent); // The managed service accepts exactly this input.
    assert.equal(await chips.count(), 0, 'Chips clear after sending');
    await fixture('drain'); await frames(page);
    const user = page.locator('.agent-terminal-user').last();
    assert.equal(await user.locator('img[src^="data:image/png"]').count(), 1, 'Sent message shows the image');
    const userText = await user.innerText();
    assert.match(userText, /attach these[\s\S]*notes\.md[\s\S]*config\.json/);
    assert.doesNotMatch(userText, /base64|attached_file|eu-west/, 'Payloads never appear as message text');

    // Leaks: a raw provider error becomes one sentence; the source waits behind Details.
    await send('fail'); await fixture('drain'); await frames(page);
    const notice = page.locator('.agent-error-notice').last();
    await notice.waitFor();
    assert.equal(await notice.locator('.agent-error-notice-body > p').innerText(), 'Request failed (500): Upstream model is overloaded');
    const visible = await notice.evaluate(el => [...el.querySelectorAll('p')].map(p => p.innerText).join('\n'));
    assert.doesNotMatch(visible, /[{}]|worker\.js|\bat /, 'No JSON or stack trace in the notice');
    const raw = notice.locator('details');
    assert.equal(await raw.getAttribute('open'), null);
    await raw.locator('summary').click();
    assert.match(await raw.locator('pre').innerText(), /"type":"server_error"[\s\S]*worker\.js:120:15/);
    await send('envelope'); await fixture('drain'); await frames(page);
    const envelope = await page.locator('.agent-terminal-markdown.is-assistant').last().innerText();
    assert.match(envelope, /The envelope answer, shown as prose\./);
    assert.doesNotMatch(envelope, /output_text|annotations|[{}]/, 'Protocol envelopes render as prose');

    // Stop: cancels the running turn.
    await send('Run it again'); await fixture('play', 10); await frames(page);
    // Stick to bottom only while the reader is there: streaming follows the tail, scrolling up stops it.
    const log_ = page.locator('.agent-dom-transcript');
    const gap = () => log_.evaluate(el => Math.round(el.scrollHeight - el.scrollTop - el.clientHeight));
    assert.ok(await gap() < 48, `Streaming follows the tail (${await gap()})`);
    if (await log_.evaluate(el => el.scrollHeight > el.clientHeight + 200)) {
      await log_.hover(); await page.mouse.wheel(0, -400); await page.waitForTimeout(150);
      const before = await log_.evaluate(el => el.scrollTop);
      await fixture('play', 3); await frames(page); await page.waitForTimeout(100);
      assert.equal(await log_.evaluate(el => el.scrollTop), before, 'Scrolled-up reader is not pulled down by new output');
      await page.getByRole('button', { name: 'Jump to latest response' }).click(); await page.waitForTimeout(150);
      assert.ok(await gap() < 48, 'Jump to latest returns to the tail');
      log(`${name}:follow-tail`, { heldAt: before });
    }
    await page.getByRole('button', { name: 'Stop response' }).click();
    await page.waitForFunction(() => window.fixture.cancels.length === 1);
    await page.locator('.agent-live-status').waitFor({ state: 'detached' });
    const width = await page.evaluate(() => document.documentElement.scrollWidth);
    assert.ok(width <= viewport.width, `No horizontal overflow (${width})`);
    assert.deepEqual(errors, []);
    await shot('final');
    log(`${name}:complete`, { prompts: (await prompts()).length, documentWidth: width, sentTypes: sent.map(item => item.type) });
    await context.tracing.stop({ path: new URL(`trace-${name}.zip`, output).pathname }); await context.close();
  }
  writeFileSync(new URL('results.json', output), JSON.stringify(evidence, null, 2));
  console.log(JSON.stringify(evidence, null, 2));
  console.log('Agents redesign journey passed');
} finally { await browser.close(); server.close(); }
