#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createDeploymentLedger } from './deployment-ledger.mjs';
import { currentRelease } from './current-production-release.mjs';
import { phases } from './deploy-workers.mjs';
import { readPlan, releaseFingerprints } from './release-plan.mjs';

const commands=Object.fromEntries([...phases.infrastructure,...phases.consumers,
  ['managed','js/managed',['npx','wrangler','deploy','--config','wrangler.ci.jsonc','--containers-rollout','immediate']],
  ['account','js/account',['npx','wrangler','deploy','--config','dist/nanocodex/wrangler.json']],
].map(([name,directory,command])=>[name,{directory,command}]));
export const releasePhases=[['egress','x'],['managed'],['email','dialog','connect-api','astra','chief-of-staff','playground'],['account']];

export async function guardedCommand(command, {cwd=process.cwd(),directory='.',env=process.env,input,launch=spawn}={}) {
  const temporary=mkdtempSync(join(tmpdir(),'nanocodex-release-'));
  const output=join(temporary,'guard-output');
  try {
    const code=await new Promise((done,reject)=>{
      const child=launch(process.execPath,[resolve(cwd,'scripts/cloudflare/current-production-release.mjs'),'--',...command],{
        cwd:resolve(cwd,directory),env:{...env,GITHUB_OUTPUT:output},stdio:[input===undefined?'inherit':'pipe','inherit','inherit'],
      });
      child.once('error',reject);child.once('close',done);
      if(input!==undefined){child.stdin.on('error',()=>{});child.stdin.end(input);}
    });
    if(code!==0)throw new Error('Guarded Worker command failed');
    return readFileSync(output,'utf8').trim().split('\n').at(-1)==='active=true';
  }finally{rmSync(temporary,{recursive:true,force:true});}
}
export async function accountHealth() {
  const response=await fetch('https://nanocodex.gakonst.workers.dev/api/health',{signal:AbortSignal.timeout(20_000)});
  assert.equal(response.status,200);const health=await response.json();
  assert.equal(health.service,'nanocodex');assert.equal(health.runtime,'cloudflare-workers');assert.equal(health.status,'ok');
}
export async function releaseWorkers(plan,{ledger=createDeploymentLedger(),isCurrent=currentRelease,run=guardedCommand,health=accountHealth,env=process.env,cwd=process.cwd()}={}){
  const results=[];
  async function deploy(name){
    if(!plan.selected.includes(name))return;
    if(!await isCurrent()){results.push({name,state:'superseded',seconds:0});return;}
    const started=Date.now();
    const record=await ledger.start(name,plan.fingerprints[name]);
    try{
      console.log(`Deploying ${name}`);
      const spec=commands[name];
      const childEnv={...env};
      for(const key of ['ASTRA_MANAGED_API_KEY','ASTRA_MPP_SECRET','TEMPO_API_KEY'])delete childEnv[key];
      let active=await run([...spec.command,'--message',env.DEPLOY_MESSAGE??''],{cwd,directory:spec.directory,env:childEnv});
      if(active&&name==='astra'){
        const secrets=Object.fromEntries([
          ['NANOCODEX_ASTRA_MANAGED_API_KEY',env.ASTRA_MANAGED_API_KEY],
          ['NANOCODEX_ASTRA_MPP_SECRET',env.ASTRA_MPP_SECRET],['TEMPO_MPP_API_KEY',env.TEMPO_API_KEY],
        ].filter(([,value])=>value));
        if(Object.keys(secrets).length)active=await run(['npx','wrangler','secret','bulk','--env='],{cwd,directory:spec.directory,env:childEnv,input:JSON.stringify(secrets)});
        else console.log('::notice::Astra secrets unchanged; no configured repository values');
      }
      if(active&&name==='account')await health();
      await ledger.finish(record,active?'success':'inactive');
      results.push({name,state:active?'success':'superseded',seconds:(Date.now()-started)/1000});
    }catch(error){
      try{await ledger.finish(record,'failure');}catch{}
      results.push({name,state:'failure',seconds:(Date.now()-started)/1000});
      throw error;
    }
  }
  try{
    for(const phase of releasePhases){
      const completed=await Promise.allSettled(phase.map(deploy));
      if(completed.some(result=>result.status==='rejected'))throw new Error('Release phase failed; dependent Workers were not deployed');
    }
    if(plan.selected.length&&!plan.selected.includes('account'))await health();
    return results;
  }finally{
    if(env.GITHUB_STEP_SUMMARY)appendFileSync(env.GITHUB_STEP_SUMMARY,
      '\n| Worker | Result | Seconds |\n|---|---|---:|\n'+results.map(result=>`| ${result.name} | ${result.state} | ${result.seconds.toFixed(1)} |\n`).join(''));
  }
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  const plan=readPlan();
  assert.deepEqual(plan.fingerprints,await releaseFingerprints(),'Release inputs changed after planning');
  await releaseWorkers(plan);
}
