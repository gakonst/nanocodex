import assert from 'node:assert/strict';
import { test } from 'node:test';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { fileURLToPath } from 'node:url';

// Real workerd sockets, SQLite and the actual SDK header/accept path. The
// controllable admission join supplements real storage.sync(); it does not
// simulate replication latency or establish a production latency saving.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { PreparedModelUpgrade } from './src/prepared-model-upgrade.ts';
import { cloudflareEgress } from '../nanocodex/cloudflare/egress.mjs';
export class Session extends DurableObject {
  frames=0; upgrades=0; closed=0; committed=false; valid=true; reused=false;
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/state') return Response.json({frames:this.frames,upgrades:this.upgrades,closed:this.closed,committed:this.committed,reused:this.reused});
    if(path==='/release') {this.committed=true;this.release?.();this.releasePending?.();return new Response('ok');}
    if(path==='/dispose') {this.valid=false;this.prepared?.dispose('retired');return new Response('ok');}
    const mode=new URL(request.url).searchParams.get('mode');
    const headers=new Headers({authorization:'Bearer NANOCODEX_PROVIDER_CREDENTIAL',upgrade:'websocket','openai-beta':'responses_websockets=2026-02-06','session-id':'runtime','thread-id':'runtime','x-client-request-id':'runtime','x-openai-internal-codex-responses-lite':'true','x-responsesapi-include-timing-metrics':'true','user-agent':'nanocodex-js/cloudflare'});
    const expected=new Request('https://nanocodex.internal/v1/responses',{headers});
    let preparedResponse;
    const provider={fetch:()=>{
      this.upgrades++;
      const [client,server]=Object.values(new WebSocketPair());server.accept();
      server.addEventListener('message',()=>{if(!this.committed)throw Error('frame before admission');this.frames++;});
      server.addEventListener('close',()=>{this.closed++;try{server.close();}catch{}});
      return Promise.resolve(new Response(null,{status:101,webSocket:client}));
    }};
    const speculative={fetch:(req)=>{
      if(mode==='stalled'){this.upgrades++;return new Promise(()=>{});}
      if(mode==='throw')throw Error('sync failure');
      if(mode==='reject')return Promise.reject(Error('async failure'));
      if(mode==='http')return Promise.resolve(new Response('unavailable',{status:503}));
      return provider.fetch(req).then(async response=>{preparedResponse=response;if(mode==='late')await new Promise(resolve=>{this.releasePending=resolve;});return response;});
    }};
    this.prepared=new PreparedModelUpgrade(expected,speculative);
    this.ctx.storage.sql.exec('CREATE TABLE admission (id INTEGER)');
    this.ctx.storage.sql.exec('INSERT INTO admission VALUES (1)');
    const gate=new Promise(resolve=>{this.release=resolve;});
    const transport=cloudflareEgress({binding:{fetch:async(input,init)=>{
      const actual=new Request(input,init);
      if(mode==='mismatch')actual.headers.set('x-codex-turn-state','changed');
      // Admission is mandatory even when preparation failed or mismatched.
      await this.ctx.storage.sync();
      const response=await this.prepared.take(actual,()=>gate,()=>this.valid);
      await gate;
      this.reused=!!response && response===preparedResponse;
      if(response && await this.prepared.take(actual,()=>gate,()=>true))throw Error("duplicate consumption");
      return response ?? provider.fetch(actual);
    }}});
    try {
      const result=await transport.createWebSocket('https://nanocodex.internal/v1/responses','runtime',{authorization:'host_managed'});
      result.socket.send(JSON.stringify({type:'response.create'}));
      return new Response('done');
    } catch { return new Response('retired', {status:409}); }
  }
}
export default {fetch(request,env){return env.SESSIONS.getByName(new URL(request.url).searchParams.get('id')??'default').fetch(request);}};
`;

test('prepared upgrade uses real SDK once after admission and cleans failures/mismatch/retirement', {timeout:30000}, async()=>{
  const bundle=await build({stdin:{contents:source,resolveDir:fileURLToPath(new URL('..',import.meta.url))},bundle:true,write:false,format:'esm',platform:'neutral',external:['cloudflare:*']});
  const mf=new Miniflare({modules:true,script:bundle.outputFiles[0].text,compatibilityDate:'2026-07-30',compatibilityFlags:['enable_request_signal'],durableObjects:{SESSIONS:{className:'Session',useSQLite:true}}});
  const call=(path,id)=>mf.dispatchFetch('https://fixture.internal'+path+(path.includes('?')?'&':'?')+'id='+id);
  async function state(id,predicate){for(let i=0;i<1200;i++){const value=await(await call('/state',id)).json();if(predicate(value))return value;await new Promise(r=>setTimeout(r,10));}throw Error('state did not converge');}
  try {
    for(const mode of ['reuse','throw','reject','http','mismatch','retire','late','stalled','expiry']){
      const pending=call('/start?mode='+mode,mode);
      if(!['throw','reject','http'].includes(mode))await state(mode,v=>v.upgrades===1);
      const held=await(await call('/state',mode)).json();
      assert.equal(held.frames,0);assert.equal(held.committed,false);
      if(mode==='stalled') {
        await call('/release',mode);
        await call('/dispose',mode);
        let deadline;
        try {
          assert.equal((await Promise.race([pending,new Promise((_,reject)=>{deadline=setTimeout(()=>reject(Error('cancel did not settle stalled consume')),1000);})])).status,409);
        } finally { clearTimeout(deadline); }
        const cancelled=await(await call('/state',mode)).json();
        assert.equal(cancelled.frames,0);assert.equal(cancelled.upgrades,1);
        continue;
      }
      if(['retire','late'].includes(mode))await call('/dispose',mode);
      if(mode==='expiry')await state(mode,v=>v.closed===1);
      await call('/release',mode);
      if(['retire','late'].includes(mode)) {
        assert.equal((await pending).status,409);
        const retired=await state(mode,v=>v.closed===1);
        assert.equal(retired.frames,0,'retired preparation sent a frame');
        assert.equal(retired.upgrades,1,'retired preparation opened a fallback connection');
        continue;
      }
      assert.equal((await pending).status,200);
      const done=await state(mode,v=>v.frames===1);
      assert.equal(done.reused,mode==='reuse');
      assert.equal(done.upgrades,['mismatch','retire','late','expiry'].includes(mode)?2:1);
      if(['mismatch','retire','late','expiry'].includes(mode))await state(mode,v=>v.closed===1);
    }
  }finally{await mf.dispose();}
});
