// Actual Chromium page and module Worker using Rust/WASM over loopback HTTP.
// Set NANOXAI_PLAYWRIGHT_MODULE and optionally NANOXAI_CHROMIUM_EXECUTABLE.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
const { chromium } = await import(process.env.NANOXAI_PLAYWRIGHT_MODULE ?? 'playwright');
const root = fileURLToPath(new URL('../', import.meta.url));
const source = `import { create as createBrowser } from ${JSON.stringify(`${root}browser/Xai.mjs`)};
import { create as createWorker } from ${JSON.stringify(`${root}worker/Xai.mjs`)};
const create = typeof document === 'undefined' ? createWorker : createBrowser;
import { createMemoryDurabilityStore } from ${JSON.stringify(`${root}runtime/durability-store.mjs`)};
async function acceptance() {
  const module = await (await fetch('/nanocodex_bg.wasm')).arrayBuffer();
  const store = createMemoryDurabilityStore('chromium-xai');
  let authCalls=0,effects=0;
  const options={module,model:'grok-4.6',endpoint:location.origin+'/v1/responses',requestTimeoutMs:5000,maxRetries:0,repetitionLimit:1,compactionKeepTail:0,durability:store,durabilityId:'chromium-xai',
    auth:{headers(){authCalls++;return {authorization:'Bearer synthetic-browser-only'}}},
    tools:[{name:'read_file',description:'Synthetic explicit browser file',handler(input,ctx){if(!ctx.callId||!ctx.turnId||!ctx.signal)throw Error('missing invocation context');effects++;return 'BROWSER_RECEIPT'}}]};
  let agent=await create(options);
  const events=[]; const watcher=agent.events.watch();watcher.onEvent(e=>events.push(e));
  const first=await agent.turn.prompt({input:'one browser effect',id:'stable'}).result();
  const usage=await first.usage(); const context=await agent.session.context();
  watcher.off();await agent.session.shutdown();agent.dispose();
  const beforeAuth=authCalls;
  agent=await create({...options,auth:{headers(){throw Error('must never authenticate replay')}}});
  const replay=await agent.turn.prompt({input:'one browser effect',id:'stable'}).result();
  await agent.session.shutdown();agent.dispose();
  return {first:first.finalMessage,replay:replay.finalMessage,usage,effects,authCalls,beforeAuth,hasContext:JSON.stringify(context).includes('BROWSER_RECEIPT'),streamed:events.some(e=>e.type==='assistant.delta')};
}
if(typeof document==='undefined')onmessage=async()=>{try{postMessage({result:await acceptance()})}catch(error){postMessage({error:String(error),stack:error.stack})}};
else globalThis.runXaiAcceptance=acceptance;`;
const bundle = await build({ stdin: { contents: source, resolveDir: root, sourcefile: 'xai-browser-acceptance.mjs' }, bundle: true, platform: 'browser', format: 'esm', write: false });
const wasm = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
const requests = [];
const server = createServer(async (request, response) => {
  if (request.url === '/') { response.writeHead(200, { 'content-type': 'text/html' }); response.end('<!doctype html><script type="module" src="/acceptance.mjs"></script>'); return; }
  if (request.url === '/acceptance.mjs') { response.writeHead(200, { 'content-type': 'text/javascript' }); response.end(bundle.outputFiles[0].text); return; }
  if (request.url === '/nanocodex_bg.wasm') { response.writeHead(200, { 'content-type': 'application/wasm' }); response.end(wasm); return; }
  if (request.url !== '/v1/responses') { response.writeHead(404); response.end(); return; }
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks)); requests.push(body);
  const output = requests.length % 2 === 1 ? [{type:'function_call',call_id:'browser-read',name:'read_file',arguments:'{}'}]
    : [{type:'message',role:'assistant',content:[{type:'output_text',text:'CHROMIUM_XAI_OK'}]}];
  const frames=[{type:'response.output_text.delta',item_id:'browser-message',delta:'native browser delta'},
    {type:'response.completed',response:{id:'browser-response',status:'completed',output,usage:{input_tokens:12,output_tokens:4,total_tokens:16}}}];
  response.writeHead(200, {'content-type':'text/event-stream'});
  response.end(frames.map(frame=>`data: ${JSON.stringify(frame)}\n\n`).join(''));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true, ...(process.env.NANOXAI_CHROMIUM_EXECUTABLE ? { executablePath: process.env.NANOXAI_CHROMIUM_EXECUTABLE } : {}) });
  const page = await browser.newPage(); const errors=[];page.on('pageerror',e=>errors.push(String(e)));
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.waitForFunction(()=>typeof globalThis.runXaiAcceptance==='function');
  const pageResult=await page.evaluate(()=>globalThis.runXaiAcceptance());
  const workerResult=await page.evaluate(()=>new Promise((resolve,reject)=>{
    const worker=new Worker('/acceptance.mjs',{type:'module'});
    const timeout=setTimeout(()=>{worker.terminate();reject(Error('xAI Worker timed out'))},15000);
    worker.onmessage=e=>{clearTimeout(timeout);worker.terminate();e.data.error?reject(Error(e.data.error)):resolve(e.data.result)};
    worker.onerror=e=>{clearTimeout(timeout);worker.terminate();reject(Error(e.message))};worker.postMessage({run:true});
  }));
  for(const result of [pageResult,workerResult]) {
    assert.equal(result.first,'CHROMIUM_XAI_OK');assert.equal(result.replay,result.first);
    assert.equal(result.effects,1);assert.equal(result.authCalls,result.beforeAuth);assert.equal(result.hasContext,true);assert.equal(result.streamed,true);
  }
  assert.equal(requests.length,4);assert.deepEqual(errors,[]);
  for(const index of [1,3]) assert(requests[index].input.some(i=>i.type==='function_call_output' && i.output==='BROWSER_RECEIPT'));
  console.log(JSON.stringify({browser:await browser.version(),actualPage:pageResult,actualModuleWorker:workerResult,responsesRequests:requests.length,terminalReplayRequests:0},null,2));
} finally { await browser?.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve)); }
