// Isolated transport fixtures exercise the real Lua/UI. Never contacts an account.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const http=require('node:http'), fs=require('node:fs'), path=require('node:path');
(async()=>{
 const root=path.join(__dirname,'public');
 const server=http.createServer((req,res)=>{const p=path.join(root,req.url==='/'?'index.html':req.url.split('?')[0]);res.setHeader('Content-Type',p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/html');fs.readFile(p,(e,b)=>{res.statusCode=e?404:200;res.end(e?'missing':b)})});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true});
 try {
 const page=await browser.newPage({viewport:{width:1440,height:960}}), errors=[], sends=[];
 page.on('pageerror',e=>errors.push(e.message));
 let releaseAlpha, alphaWaiting=false;const alphaGate=new Promise(r=>releaseAlpha=r);
 const pending=new Map();let seq=0;
 const out=(rid,kind,value,state='completed')=>({request_id:rid,kind,value,state,event_id:`fixture-${++seq}`});
 await page.route('**/api/addon/**',async route=>{
  const url=new URL(route.request().url());let outputs=[];
  if(url.pathname.endsWith('/status'))return route.fulfill({json:{connected:true}});
  if(url.pathname.endsWith('/dispatch')){
   const {payload,id}=route.request().postDataJSON(), q=JSON.parse(payload);
   if(q.action==='refresh_projects')outputs=[out(id,'projects','ncw1\nP\tp\tFixture project\nT\tp\talpha\tAlpha\tidle\nT\tp\tbeta\tBeta\tidle')];
   else if(q.action==='connection_status')outputs=[out(id,'ack','ncm1\tconnection\tconnected')];
   else if(q.action==='load_history'){
    if(q.thread_id==='alpha'&&!alphaWaiting){alphaWaiting=true;await alphaGate}
    outputs=[out(id,'reply',`nch1\t${id}\t${q.thread_id}\t0\t1\t\t0\t${q.view_id}\n${q.thread_id} history`)];
   }else if(q.type==='nanocodex.ask'){
    sends.push(q.thread_id);outputs=[out(id,'ack',`ncm1\tsend\t${id}\t${q.thread_id}\t${id}\tremoteaccepted`,'remoteaccepted')];
    pending.set(id,[out(id,'stream',`ncs1\t${id}\t${id}\ts\t1\t0\tappend\n${q.thread_id} partial`,'streaming')]);
   }else throw Error('Unexpected request');
  }else if(url.pathname.endsWith('/poll'))for(const id of url.searchParams.getAll('id'))outputs.push(...pending.get(id)||[]);
  await route.fulfill({json:{outputs}});
 });
 await page.goto(`http://127.0.0.1:${server.address().port}`);
 await page.locator('[data-thread="alpha"]').click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('Loading'));
 const switchStart=Date.now();
 await page.locator('[data-thread="beta"]').click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('beta history'));
 const switchMs=Date.now()-switchStart;
 assert(alphaWaiting,'Alpha request must still be stalled');
 await page.getByRole('textbox',{name:'Message',exact:true}).fill('Beta prompt');
 await page.getByRole('button',{name:'Ask',exact:true}).click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('beta partial'));
 releaseAlpha();
 await page.locator('[data-thread="alpha"]').click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('alpha history'));
 await page.getByRole('textbox',{name:'Message',exact:true}).fill('Alpha prompt');
 await page.getByRole('button',{name:'Ask',exact:true}).click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('alpha partial'));
 assert.deepEqual(sends,['beta','alpha']);
 assert(await page.getByText('CURRENT · Working · Fixture project',{exact:true}).isVisible());
 await page.locator('[data-thread="beta"]').click();
 await page.waitForFunction(()=>document.querySelector('[aria-label="Conversation"]')?.value.includes('beta partial'));
 assert(!(await page.getByRole('textbox',{name:'Conversation',exact:true}).inputValue()).includes('alpha partial'));
 assert.deepEqual(errors,[]);
 // The idle pump should not rewrite the DOM continuously.
 await page.evaluate(()=>{window.mutationCount=0;window.observer=new MutationObserver(rows=>window.mutationCount+=rows.length);observer.observe(document.querySelector('#lua-root'),{subtree:true,attributes:true,childList:true,characterData:true})});
 await page.waitForTimeout(400);
 const mutations=await page.evaluate(()=>{observer.disconnect();return mutationCount});
 assert(mutations<20,`Idle DOM churn: ${mutations}`);
 console.log(JSON.stringify({passed:true,blockedHistoryDidNotBlockOtherThread:true,sends,switchMs,idleMutations:mutations,errors}));
 }finally{await browser.close();await new Promise(r=>server.close(r))}
})().catch(e=>{console.error(e);process.exit(1)});
