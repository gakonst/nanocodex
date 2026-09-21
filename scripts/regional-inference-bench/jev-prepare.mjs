import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash,randomBytes} from 'node:crypto';
import {makeCalibration,assertFresh,REGIONS,PROMPTS,POLICY,SOURCE} from './jev-core.mjs';
const [runnersFile,streamingFile,privateDir,publicDir]=process.argv.slice(2);
if(!publicDir)throw Error('usage: jev-prepare.mjs PRIVATE_RUNNERS_JSON STREAMING_RESULTS_JSONL NEW_PRIVATE_DIR NEW_PUBLIC_DIR');
const runners=JSON.parse(await fs.readFile(runnersFile,'utf8'));
if(runners.length!==3||new Set(runners.map(r=>r.region)).size!==3||runners.some(r=>!REGIONS[r.region]))throw Error('requires_three_regions');
const rows=(await fs.readFile(streamingFile,'utf8')).trim().split('\n').map(JSON.parse);
const calibration=makeCalibration(rows,runners);assertFresh(calibration);
const encoded=JSON.stringify(calibration),hash=createHash('sha256').update(encoded).digest('hex');
await fs.mkdir(privateDir,{mode:0o700});await fs.mkdir(publicDir);
await fs.writeFile(path.join(publicDir,'calibration.json'),encoded+'\n');
const expires=new Date(Math.min(Date.now()+2*3600e3,...calibration.metrics.map(m=>m.lastObservedAt+m.windowMs))).toISOString();
const prepared=[];
for(const r of runners){
 const name=r.id.replace('-bench-','-jev-'),config=path.resolve(privateDir,`${name}.json`),tokenfile=path.resolve(privateDir,`${name}.token`);
 const old=JSON.parse(await fs.readFile(r.config,'utf8'));
 if(!name.includes('-jev-')||name===r.id)throw Error('invalid_isolated_name');
 const url=new URL(r.url);url.hostname=url.hostname.replace(r.id,name);
 await fs.writeFile(tokenfile,randomBytes(32).toString('hex'),{mode:0o600});
 await fs.writeFile(config,JSON.stringify({name,main:path.resolve('scripts/regional-inference-bench/jev-worker.mjs'),compatibility_date:'2026-09-21',compatibility_flags:['global_fetch_strictly_public'],workers_dev:true,placement:{region:`aws:${r.region}`},observability:{enabled:false},ai:{binding:'AI'},durable_objects:{bindings:[{name:'JEV_BUDGET',class_name:'JevBudget'}]},migrations:[{tag:'v1',new_sqlite_classes:['JevBudget']}],vars:{RUNNER_ID:name,PLACEMENT_REGION:`aws:${r.region}`,EXPECTED_API_INGRESS:REGIONS[r.region],TARGET_URL:old.vars.TARGET_URL,EXPIRES_AT:expires,CALIBRATION_SHA256:hash,...Object.fromEntries(Array.from({length:Math.ceil(encoded.length/4000)},(_,i)=>[`CALIBRATION_JSON_${i}`,encoded.slice(i*4000,(i+1)*4000)]))}},null,2),{mode:0o600});
 prepared.push({id:name,region:r.region,key_index:r.key_index,config,tokenfile,url:url.href.replace(/\/$/,''),calibration_sha256:hash});
}
await fs.writeFile(path.join(privateDir,'runners.json'),JSON.stringify(prepared,null,2),{mode:0o600});
await fs.writeFile(path.join(publicDir,'plan.json'),JSON.stringify({source:SOURCE,calibration_sha256:hash,expires_at:expires,prompts:PROMPTS,policy:POLICY,regions:REGIONS,classifications_max:18,downstream_max:18,total_attempts_max:36,prior_attempts_upper_bound:144,final_attempts_upper_bound:180,remaining_from_200:20,downstream_max_output_tokens:256,additional_budget_reserve_usd:0.54,reserve_is_not_provider_price_guarantee:true,no_retries:true,status:'prepared_not_deployed'},null,2));
console.log(JSON.stringify({prepared:3,calibration_rows:27,calibration_sha256:hash,expires_at:expires,paid_calls:0}));
