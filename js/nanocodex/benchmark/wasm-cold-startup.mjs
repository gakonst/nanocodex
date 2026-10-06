// Run in a fresh Node process: node js/nanocodex/benchmark/wasm-cold-startup.mjs
// Use the same generated release module for comparisons. Module compilation is
// reported separately; engineMs is nested inside createMs. This is native Node,
// not Workers CPU or network latency. The 32-tool fixture has subagents enabled.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {performance} from 'node:perf_hooks';
import {Agent,Transport} from '../host/index.mjs';
import {initializeBrowserEngine} from '../browser/engine.mjs';
import {createTools} from '../tools/Tools.mjs';
const compileStart=performance.now();
const module=await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm',import.meta.url)));
const compileMs=performance.now()-compileStart;
const start=performance.now();
const tools=await createTools({mcp:false,tools:Array.from({length:32},(_,i)=>({name:'fixture_'+i,description:'Synthetic fixture tool.',parameters:{type:'object',properties:{query:{type:'string'}},required:['query'],additionalProperties:false},handler:async()=>({text:'ok'})}))});
const prepMs=performance.now()-start;
const createStart=performance.now();
await initializeBrowserEngine({module});
const engineMs=performance.now()-createStart;
const agent=await Agent.create({module,tools,transport:Transport.openAi({apiKey:'synthetic-startup',stateless:true}),model:'gpt-6.1-sol',thinking:'low',toolMode:'direct',instructions:'Synthetic normal Managed prompt. '.repeat(1536)});
const createMs=performance.now()-createStart;
assert.ok(agent.sessionId);await agent.session.shutdown();await tools.close();
console.log(JSON.stringify({compileMs,prepMs,engineMs,createMs,totalMs:prepMs+createMs,tools:32,instructionBytes:50688,subagents:true}));
