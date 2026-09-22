import fs from 'node:fs/promises';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {randomBytes} from 'node:crypto';
const root=process.cwd();
const [priv,keyFile,out]=process.argv.slice(2);
if(!priv||!keyFile||!out)throw Error('usage: deploy-runners.mjs private-config-directory private-key-batch.json private-log-directory');
await fs.mkdir(priv,{recursive:true,mode:0o700});await fs.mkdir(out,{recursive:true});
const wrangler=path.join(root,'js/managed/node_modules/.bin/wrangler');
const keys=JSON.parse(await fs.readFile(keyFile,'utf8')).keys;
const models=['cloudflare','openrouter','vercel'].map(p=>`${p}:openai/gpt-5.6-luna:low`);
async function command(args,input,log){return new Promise((resolve,reject)=>{let data='';const p=spawn(wrangler,args,{cwd:root,stdio:['pipe','pipe','pipe'],env:{...process.env,WRANGLER_SEND_METRICS:'false'}});p.stdout.on('data',x=>data+=x);p.stderr.on('data',x=>data+=x);p.stdin.end(input);p.on('close',async code=>{await fs.writeFile(path.join(out,log),data);code===0?resolve(data):reject(new Error(`wrangler exit ${code}; see ${log}`));});});}
const runners=[];
for(const [i,region] of ['us-east-1','eu-west-1','ap-northeast-1'].entries()){
 const name=`nanocodex-bench-${['us','eu','ap'][i]}-20260921`, config=path.join(priv,`${name}.json`), tokenfile=path.join(priv,`${name}.token`);
 let token;try{token=await fs.readFile(tokenfile,'utf8');}catch{token=randomBytes(32).toString('hex');await fs.writeFile(tokenfile,token,{mode:0o600});}
 await fs.writeFile(config,JSON.stringify({name,main:path.join(root,'scripts/regional-inference-bench/worker.mjs'),compatibility_date:'2026-09-21',compatibility_flags:['global_fetch_strictly_public'],workers_dev:true,placement:{region:`aws:${region}`},observability:{enabled:false},vars:{PLACEMENT_REGION:`aws:${region}`,TARGET_URL:'https://nanocodex.gakonst.workers.dev/v1/responses',ALLOWED_MODELS:models.join(','),EXPIRES_AT:new Date(Date.now()+4*3600e3).toISOString()}},null,2));
 await command(['deploy','--config',config],undefined,`${name}-deploy.log`);
 await command(['secret','bulk','--config',config],JSON.stringify({BENCH_TOKEN:token,INFERENCE_KEY:keys[i].api_key}),`${name}-secrets.log`);
 const runner={id:name,region,key_index:i,config,tokenfile,url:`https://${name}.gakonst.workers.dev`};runners.push(runner);console.log(JSON.stringify({deployed:name,region}));
}
await fs.writeFile(path.join(priv,'runners.json'),JSON.stringify(runners,null,2),{mode:0o600});
