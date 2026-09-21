import fs from 'node:fs/promises';
import path from 'node:path';
import {PROMPTS,CANDIDATES,SOURCE} from './jev-core.mjs';
const [runnersFile,outputDir,approval]=process.argv.slice(2);
if(!outputDir||approval!=='--approved')throw Error('usage: jev-campaign.mjs PRIVATE_RUNNERS_JSON NEW_OUTPUT_DIR --approved (parent review required)');
const runners=JSON.parse(await fs.readFile(runnersFile,'utf8'));
if(runners.length!==3||new Set(runners.map(r=>r.id)).size!==3||new Set(runners.map(r=>r.key_index)).size!==3)throw Error('invalid_runners');
await fs.mkdir(outputDir);
const all=[],attempts=[];let stopped=false;
// Read-only authentication/placement receipts, before any inference reservation.
const evidence=[];
for(const r of runners){
 const token=await fs.readFile(r.tokenfile,'utf8');
 const response=await fetch(r.url+'/evidence',{headers:{authorization:`Bearer ${token}`},signal:AbortSignal.timeout(15000)});
 if(!response.ok)throw Error('runner_evidence_failed');
 const data=await response.json();
 if(data.source!==SOURCE||data.calibration_sha256!==r.calibration_sha256)throw Error('wrong_deployment');
 evidence.push({...data,response_cf_placement:response.headers.get('cf-placement'),response_cf_ray:response.headers.get('cf-ray')});
}
await fs.writeFile(path.join(outputDir,'evidence.json'),JSON.stringify(evidence,null,2));
await Promise.all(runners.map(async(r,regionIndex)=>{
 const token=await fs.readFile(r.tokenfile,'utf8');
 for(let prompt_index=0;prompt_index<PROMPTS.length;prompt_index++){
  const arms=(regionIndex+prompt_index)%2?['regional','global']:['global','regional'];
  for(const arm of arms){
   if(stopped)return;
   const started=Date.now();const attempt={runner:r.id,prompt_index,arm,started_at:new Date(started).toISOString(),classification_upper_bound:1,downstream_upper_bound:1};
   attempts.push(attempt);await fs.appendFile(path.join(outputDir,`${r.id}-attempts.jsonl`),JSON.stringify(attempt)+'\n');
   let result;
   try{
    const response=await fetch(r.url+'/run',{method:'POST',headers:{authorization:`Bearer ${token}`,'content-type':'application/json'},body:JSON.stringify({prompt_index,arm}),signal:AbortSignal.timeout(155000)});
    result={...await response.json(),runner_http_status:response.status,runner_response_placement:response.headers.get('cf-placement'),runner_response_ray:response.headers.get('cf-ray')};
   }catch{result={error:'runner_transport_outcome_unknown'};}
   const row={...result,runner:r.id,prompt_index,arm};
   const serial=JSON.stringify(row).replaceAll(token,'[REDACTED]');all.push(JSON.parse(serial));
   await fs.appendFile(path.join(outputDir,r.id+'.jsonl'),serial+'\n');
   const d=row.downstream,valid=!row.error&&row.calibration_sha256===r.calibration_sha256&&row.origin_validated&&row.classification?.attempts===1&&CANDIDATES.includes(row.diagnostics?.chosen_candidate)&&d?.status==='completed'&&!d?.error&&d?.buffering==='streaming'&&Number.isFinite(d?.first_meaningful_ms)&&d?.route?.model==='gpt-5.6-luna'&&d?.route?.thinking==='low'&&d?.route?.backend===row.route?.backend;
   if(!valid)stopped=true;
   console.log(JSON.stringify({runner:r.id,prompt_index,arm,selected:row.diagnostics?.chosen_candidate,selection:row.route?.selection,confidence_status:row.diagnostics?.confidence_status,error:row.error,valid,ttft_ms:d?.first_meaningful_ms}));
   await new Promise(resolve=>setTimeout(resolve,Math.max(0,7500-(Date.now()-started))));
  }
 }
}));
await fs.writeFile(path.join(outputDir,'results.jsonl'),all.map(r=>JSON.stringify(r)).join('\n')+'\n');
const paired=[];
for(const r of runners)for(let p=0;p<3;p++){
 const a=all.find(x=>x.runner===r.id&&x.prompt_index===p&&x.arm==='global'),b=all.find(x=>x.runner===r.id&&x.prompt_index===p&&x.arm==='regional');
 if(a&&b)paired.push({runner:r.id,prompt_index:p,global_selected:a.diagnostics?.chosen_candidate,regional_selected:b.diagnostics?.chosen_candidate,global_selection:a.route?.selection,regional_selection:b.route?.selection,valid:!a.error&&!b.error&&a.origin_validated&&b.origin_validated,api_delivery_regional_minus_global_ms:Number.isFinite(a.downstream?.first_meaningful_ms)&&Number.isFinite(b.downstream?.first_meaningful_ms)?b.downstream.first_meaningful_ms-a.downstream.first_meaningful_ms:null});
}
await fs.writeFile(path.join(outputDir,'receipt.json'),JSON.stringify({source:SOURCE,stopped,cases_reserved_locally:attempts.length,total_attempts_upper_bound:attempts.length*2,classifications_observed:all.reduce((n,r)=>n+(r.classification?.attempts??0),0),downstream_receipts:all.filter(r=>r.downstream).length,paired,limitations:'n=3 fresh tasks per arm/region; stochastic classifier, sequential order/cache/load confounding; API delivery proxy calibration mixes 2 short + 1 long-prefix per regional candidate; no internal generation or task-success claim; null probabilities remain null; fallback is not a successful learned selection.'},null,2));
if(stopped)process.exitCode=1;
