#!/usr/bin/env node
import fs from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {measure,promptFor,summarize,VERSION} from './core.mjs';
// node run.mjs plan.json [--execute]; dry-run is the default.
export async function run(plan,execute=false) {
  if(!Array.isArray(plan.arms)||!plan.arms.length||!Array.isArray(plan.models)||!plan.models.length)throw new Error('invalid_plan');
  if(!Number.isInteger(plan.repetitions)||plan.repetitions<1||plan.repetitions>20)throw new Error('invalid_repetitions');
  if(!Number.isInteger(plan.max_output_tokens)||plan.max_output_tokens<1||plan.max_output_tokens>256)throw new Error('invalid_output_cap');
  if(!Array.isArray(plan.families)||!plan.families.length||!plan.families.every(f=>['short','long-prefix'].includes(f))||!Array.isArray(plan.stream_modes)||!plan.stream_modes.length||!plan.stream_modes.every(s=>typeof s==='boolean'))throw new Error('invalid_matrix');
  if(!Number.isInteger(plan.max_requests)||plan.max_requests<1||!Number.isFinite(plan.max_reserved_usd)||plan.max_reserved_usd<=0)throw new Error('invalid_budget');
  promptFor(plan.families[0],plan.run_id,0);
  const cases=[];
  for(let pair=0;pair<plan.repetitions;pair++)for(const family of plan.families)for(const model of plan.models) {
    const block=plan.arms.flatMap(arm=>plan.stream_modes.map(stream=>({arm,stream,family,model,pair})));
    // Reverse alternating paired blocks to counterbalance time/order without RNG ambiguity.
    if(pair%2)block.reverse(); cases.push(...block);
  }
  const reserved=Math.round(cases.length*plan.reserve_usd_per_request*1e6)/1e6;
  if(cases.length>200||cases.length>plan.max_requests||!Number.isFinite(plan.reserve_usd_per_request)||plan.reserve_usd_per_request<=0||reserved>Math.min(5,plan.max_reserved_usd))throw new Error('budget_limit');
  const manifest={version:VERSION,cases:cases.length,reserved_usd:reserved,run_id:plan.run_id,stage:plan.stage,execute,max_output_tokens:plan.max_output_tokens,interval_ms:plan.interval_ms??null,repetitions:plan.repetitions,families:plan.families,models:plan.models,stream_modes:plan.stream_modes,arms:plan.arms.map(a=>({id:a.id,kind:a.kind,runner:a.runner,deployment_version:a.deployment_version??null}))};
  if(!execute)return manifest;
  if(!plan.output||!Number.isFinite(plan.interval_ms)||plan.interval_ms<7000)throw new Error('output_and_pacing_required');
  const credentials=new Map();
  for(const arm of plan.arms) {
    const url=new URL(arm.url);if(url.protocol!=='https:'||url.username||url.password||url.search||url.hash)throw new Error('invalid_url');
    let key=process.env[arm.key_env];
    if(arm.key_file) {const batch=JSON.parse(await fs.readFile(arm.key_file,'utf8'));key=batch.keys[arm.key_index]?.api_key;}
    if(typeof key!=='string'||!key)throw new Error('missing_credential');
    credentials.set(arm.id,key);
  }
  // Refuse overwrite, keep operational evidence private. No plan/credential serialization.
  await fs.mkdir(plan.output,{mode:0o700});
  await fs.writeFile(`${plan.output}/manifest.json`,JSON.stringify(manifest,null,2),{mode:0o600});
  const rows=[];
  for(const c of cases) {
    const arm=c.arm;let result;
    if(arm.kind==='worker') {
      try {
        const response=await fetch(arm.url,{method:'POST',headers:{authorization:`Bearer ${credentials.get(arm.id)}`,'content-type':'application/json'},redirect:'error',signal:AbortSignal.timeout(135000),body:JSON.stringify({model:c.model,stream:c.stream,family:c.family,run_id:plan.run_id,pair:c.pair,max_output_tokens:plan.max_output_tokens})});
        if(!response.ok)result={http_status:response.status,error:`runner_http_${response.status}`};else result=await response.json();
      }catch{result={error:'runner_transport_outcome_unknown'};}
    } else result=await measure({url:arm.url,key:credentials.get(arm.id),body:{model:c.model,input:promptFor(c.family,plan.run_id,c.pair),stream:c.stream,store:false,max_output_tokens:plan.max_output_tokens}});
    const row={...result,run_id:plan.run_id,stage:plan.stage,runner:arm.runner,arm:arm.id,model_requested:c.model,family:c.family,stream:c.stream,pair:c.pair,execution_evidence:arm.execution_evidence??null};
    // Defense in depth against a server accidentally echoing the supplied credential.
    let serialized=JSON.stringify(row);for(const key of credentials.values())serialized=serialized.replaceAll(key,'[REDACTED]');
    rows.push(JSON.parse(serialized));await fs.appendFile(`${plan.output}/results.jsonl`,serialized+'\n',{mode:0o600});
    console.log(JSON.stringify({case:rows.length,total:cases.length,arm:arm.id,model:c.model,family:c.family,stream:c.stream,status:result.http_status,error:result.error,first_meaningful_ms:result.first_meaningful_ms}));
    if(result.error||result.status!=='completed')break; // No uncertain-outcome retries or multiplying failures.
    if(rows.length<cases.length)await new Promise(resolve=>setTimeout(resolve,plan.interval_ms));
  }
  const summary=summarize(rows);await fs.writeFile(`${plan.output}/summary.json`,JSON.stringify(summary,null,2),{mode:0o600});
  return {...manifest,attempts:rows.length,summary};
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  try {const plan=JSON.parse(await fs.readFile(process.argv[2],'utf8'));console.log(JSON.stringify(await run(plan,process.argv.includes('--execute')),null,2));}
  catch {console.error('Benchmark stopped: invalid plan, budget, credential, or output. No automatic retry.');process.exitCode=1;}
}
