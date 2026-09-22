// Credential-free fresh socket controls. Default route returns409 without application I/O.
import https from 'node:https';
import { randomBytes } from 'node:crypto';
import { createRequire } from 'node:module';
import { performance } from 'node:perf_hooks';
import os from 'node:os';
const require=createRequire(new URL('../../../js/account/package.json', import.meta.url));
const WebSocket=require('ws');
const base=process.argv[2] ?? 'https://nanocodex.gakonst.workers.dev/api/auth/chatgpt';
console.log(JSON.stringify({started_at:new Date().toISOString(),load_average:os.loadavg(),node:process.version}));
async function probe(kind,index){
 return await new Promise(resolve=>{
  const began=performance.now(), times={}; let settled=false;
  function mark(name){times[name]=performance.now()-began;}
  function finish(res,error){if(settled)return;settled=true;mark('elapsed_ms'); console.log(JSON.stringify({kind,index,...times,status:res?.statusCode,error:error?.message,cf_colo:String(res?.headers?.['cf-ray']??'').split('-').at(-1),http_version:res?.httpVersion,reused:req?.reusedSocket??false}));res?.resume();resolve();}
  function track(req){ req.on('socket',s=>{mark('socket_ms');s.on('lookup',()=>mark('dns_end_ms'));s.on('connect',()=>mark('connect_end_ms'));s.on('secureConnect',()=>mark('tls_end_ms'));});req.on('finish',()=>mark('request_end_ms'));req.on('error',e=>finish(null,e)); }
  let req;
  if(kind==='ws'){
   const ws=new WebSocket(base.replace('https:','wss:'),{agent:false,handshakeTimeout:10000}); req=ws._req; track(req);
   ws.on('unexpected-response',(_,res)=>{mark('response_ms');finish(res);ws.terminate();});ws.on('error',e=>finish(null,e));
  } else {
   const headers=kind==='upgrade'?{Connection:'Upgrade',Upgrade:'websocket','Sec-WebSocket-Version':'13','Sec-WebSocket-Key':randomBytes(16).toString('base64')} : {};
   req=https.request(base,{agent:false,headers,timeout:10000},res=>{mark('response_ms');finish(res);});track(req);req.on('timeout',()=>req.destroy(new Error('timeout')));req.end();
  }
 });
}
for(let i=0;i<4;i++)for(const kind of ['https','ws','upgrade'])await probe(kind,i);
