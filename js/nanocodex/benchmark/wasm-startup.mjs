// Run directly with Node from the repository root. Requires generated release pkg-web.
// Native Node timing only: precompilation is excluded, imports and JSON.stringify
// are instrumented, and subagent toggling measures the whole feature, not its clone.
// 32 tools approximates a modest catalog; 100/500 are synthetic stress scenarios.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {performance} from 'node:perf_hooks';
import {Agent, Transport} from '../host/index.mjs';
import {Nanocodex} from '../pkg-web/nanocodex.js';
import {initializeBrowserEngine} from '../browser/engine.mjs';
const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
const nativeInstance=WebAssembly.Instance;
const nativeInstantiate=WebAssembly.instantiate;
let active;
function instrumentImports(imports) {
 const wrapped={};
 for(const [namespace, functions] of Object.entries(imports??{})) {
  wrapped[namespace]={};
  for(const [name,value] of Object.entries(functions)) wrapped[namespace][name]=typeof value!=='function'?value:function(...args) {
   if(!active)return value(...args);
   const slot=active.imports[name]??={calls:0,ms:0};slot.calls++;
   const start=performance.now();try{return value(...args);}finally{slot.ms+=performance.now()-start;}
  };
 }
 return wrapped;
}
WebAssembly.Instance=class extends nativeInstance {
 static [Symbol.hasInstance](value) { return value instanceof nativeInstance; }
 constructor(module,imports) { super(module,instrumentImports(imports)); }
};
WebAssembly.instantiate=(source,imports)=>nativeInstantiate(source,instrumentImports(imports));
let engine;
try { engine=await initializeBrowserEngine({module}); }
finally { WebAssembly.Instance=nativeInstance;WebAssembly.instantiate=nativeInstantiate; }
const nativeCreate=Nanocodex.create;
Nanocodex.create=async function(config) {
 if(!active)return nativeCreate.call(this,config);
 active.configBytes=Buffer.byteLength(config);active.configCrossings++;
 const start=performance.now();try{return await nativeCreate.call(this,config);}finally{active.rustCreateMs+=performance.now()-start;}
};
const stringify=JSON.stringify;
JSON.stringify=function(value,...args){
 if(!active)return stringify(value,...args);
 const start=performance.now();const result=stringify(value,...args);
 active.stringifyMs+=performance.now()-start;active.stringifyCalls++;active.stringifyBytes+=typeof result==='string'?Buffer.byteLength(result):0;
 return result;
};
const transport=Transport.openAi({apiKey:'synthetic-startup',stateless:true});
const instructions='Synthetic normal Managed prompt configuration. '.repeat(20000);
const makeTools=n=>Object.fromEntries(Array.from({length:n},(_,i)=>['fixture_'+i,{description:'Synthetic fixture tool.',parameters:{type:'object',properties:{query:{type:'string'}},required:['query'],additionalProperties:false},handler:async()=>({text:'ok'})}]));
const scenarios=[{bytes:0,tools:0},{bytes:49152,tools:0},{bytes:262144,tools:0},{bytes:49152,tools:32},{bytes:49152,tools:100},{bytes:49152,tools:500}];
const output=[];
for(const scenario of scenarios){
 const options={module,transport,model:'gpt-6.1-sol',thinking:'low',toolMode:'direct',instructions:instructions.slice(0,scenario.bytes),tools:makeTools(scenario.tools)};
 const samples={true:[],false:[]};
 for(let i=0;i<90;i++)for(const enabled of (i%2?[true,false]:[false,true])){
  const state={imports:{},configBytes:0,configCrossings:0,rustCreateMs:0,stringifyMs:0,stringifyCalls:0,stringifyBytes:0};
  active=state;const start=performance.now();
  const agent=await Agent.create({...options,[Symbol.for('nanocodex.browser.internalRuntime')]:{subagentsEnabled:enabled}});
  state.totalMs=performance.now()-start;active=undefined;
  assert.ok(agent.sessionId);
  await agent.session.shutdown();
  if(i>=10)samples[enabled].push(state);
 }
 const percentile=(xs,p)=>[...xs].sort((a,b)=>a-b)[Math.floor(xs.length*p)];
 for(const enabled of [false,true]){
  const rows=samples[enabled];const summary={...scenario,subagents:enabled,n:rows.length,memoryBytes:engine.memory.buffer.byteLength};
  for(const key of ['totalMs','rustCreateMs','stringifyMs','stringifyCalls','stringifyBytes','configBytes','configCrossings'])summary[key]={p50:percentile(rows.map(x=>x[key]),.5),p95:percentile(rows.map(x=>x[key]),.95)};
  summary.imports=rows[40].imports;output.push(summary);console.log(stringify(summary));
 }
}
