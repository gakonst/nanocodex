import fs from 'node:fs/promises';
import path from 'node:path';
import {summarize} from './core.mjs';
const [stage,runnersFile,outputRoot]=process.argv.slice(2);
if(!['baseline','streaming'].includes(stage)||!runnersFile||!outputRoot)throw Error('usage: campaign.mjs baseline|streaming private-runners.json output-root');
const runners=JSON.parse(await fs.readFile(runnersFile,'utf8'));
const dir=path.join(outputRoot,stage);await fs.mkdir(dir);
const models=['cloudflare','openrouter','vercel'].map(p=>`${p}:openai/gpt-5.6-luna:low`);
const manifest={stage,run_id:'regional-20260921-matched',intended_calls:45,prior_attempts_upper_bound:99,max_total_calls:200,incremental_budget_usd:5,max_output_tokens:256,source:stage==='baseline'?'89439eb':'ce18887f:5d343432-7e2f-440d-a720-f60bff2add7f',protocol:'5 samples per provider/caller; pairs 0,1,2 calibration,3,4 heldout. Families alternate short/long/short/long/short. Explicit same Luna low. No retries. SSE requested in both stages.',reserve_usd_per_call:.015};
await fs.writeFile(path.join(dir,'manifest.json'),JSON.stringify(manifest,null,2));
const all=[];
await Promise.all(runners.map(async runner=>{
 const token=await fs.readFile(runner.tokenfile,'utf8');const rows=[];
 for(let pair=0;pair<5;pair++)for(let m=0;m<3;m++){
  const model=models[(m+pair)%3],family=pair%2?'long-prefix':'short';
  const body={model,family,pair,run_id:manifest.run_id,stream:true,max_output_tokens:256};
  const start=Date.now();let result;
  try{const r=await fetch(runner.url+'/run',{method:'POST',headers:{authorization:`Bearer ${token}`,'content-type':'application/json'},body:JSON.stringify(body),signal:AbortSignal.timeout(135000)});result=r.ok?await r.json():{error:`runner_http_${r.status}`};result.runner_response_placement=r.headers.get('cf-placement');}
  catch{result={error:'runner_transport_outcome_unknown'};}
  const row={...result,stage,runner:runner.id,placement_hint:runner.region,model_requested:model,arm:'fixed-luna-low',family,pair,split:pair<3?'calibration':'heldout',stream:true,run_id:manifest.run_id};
  const serial=JSON.stringify(row).replaceAll(token,'[REDACTED]');rows.push(JSON.parse(serial));all.push(JSON.parse(serial));
  await fs.appendFile(path.join(dir,runner.id+'.jsonl'),serial+'\n');
  console.log(JSON.stringify({stage,runner:runner.region,pair,model,http_status:row.http_status,error:row.error,ttft_ms:row.first_meaningful_ms,total_ms:row.total_ms,placement:row.runner_response_placement}));
  if(stage==='streaming'&&row.buffering!=='streaming')throw Error(`expected streaming delivery; observed ${row.buffering}; saved receipt for ${runner.id}`);
  if(row.error||row.status!=='completed'||row.first_meaningful_ms===null)throw Error(`case failed; inspect saved receipt for ${runner.id}`);
  await new Promise(r=>setTimeout(r,Math.max(0,7500-(Date.now()-start))));
 }
}));
await fs.writeFile(path.join(dir,'results.jsonl'),all.map(x=>JSON.stringify(x)).join('\n')+'\n');
await fs.writeFile(path.join(dir,'summary.json'),JSON.stringify(summarize(all),null,2));
console.log(JSON.stringify({completed_stage:stage,attempts:all.length,errors:all.filter(x=>x.error).length}));
