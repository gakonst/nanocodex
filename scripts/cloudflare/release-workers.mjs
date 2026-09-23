#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createDeploymentLedger } from './deployment-ledger.mjs';
import { releaseTag } from './live-worker-state.mjs';
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
  const result=(pending,state)=>results.push({name:pending.name,state,seconds:(Date.now()-pending.started)/1000});
  async function deploy(name){
    if(!plan.selected.includes(name))return;
    if(!await isCurrent()){results.push({name,state:'superseded',seconds:0});return;}
    const pending={name,started:Date.now()};
    let temporary;
    try{
      pending.record=await ledger.start(name,plan.fingerprints[name]);
      console.log(`Deploying ${name}`);
      const spec=commands[name];
      const childEnv={...env};
      for(const key of ['ASTRA_MANAGED_API_KEY','ASTRA_MPP_SECRET','TEMPO_API_KEY'])delete childEnv[key];
      const command=[...spec.command,'--message',env.DEPLOY_MESSAGE??'','--tag',releaseTag(plan.fingerprints[name])];
      if(name==='astra'){
        const secrets=Object.fromEntries([
          ['NANOCODEX_ASTRA_MANAGED_API_KEY',env.ASTRA_MANAGED_API_KEY],
          ['NANOCODEX_ASTRA_MPP_SECRET',env.ASTRA_MPP_SECRET],['TEMPO_MPP_API_KEY',env.TEMPO_API_KEY],
        ].filter(([,value])=>value));
        if(Object.keys(secrets).length){
          temporary=mkdtempSync(join(tmpdir(),'nanocodex-release-secrets-'));
          const path=join(temporary,'secrets.json');
          writeFileSync(path,JSON.stringify(secrets),{mode:0o600,flag:'wx'});
          // Wrangler 4.127.1 applies this file additively in the tagged deployment.
          command.push('--secrets-file',path);
        }else console.log('::notice::Astra secrets unchanged; no configured repository values');
      }
      const active=await run(command,{cwd,directory:spec.directory,env:childEnv});
      if(active)return pending;
      await ledger.finish(pending.record,'inactive');
      result(pending,'superseded');
    }catch(error){
      if(pending.record)try{await ledger.finish(pending.record,'failure');}catch{}
      result(pending,'failure');
      throw error;
    }finally{
      if(temporary)rmSync(temporary,{recursive:true,force:true});
    }
  }
  try{
    for(const phase of releasePhases){
      const completed=await Promise.allSettled(phase.map(deploy));
      const pending=completed.filter(row=>row.status==='fulfilled'&&row.value).map(row=>row.value);
      let failed=completed.some(row=>row.status==='rejected');
      let healthy=true;
      // One required health check per phase, before ANY successful receipt in it.
      // An empty or wholly superseded phase does no health work.
      if(pending.length)try{await health();}catch{healthy=false;failed=true;}
      const finished=await Promise.allSettled(pending.map(async row=>{
        try{
          await ledger.finish(row.record,healthy?'success':'failure');
          result(row,healthy?'success':'failure');
        }catch(error){
          try{await ledger.finish(row.record,'failure');}catch{}
          result(row,'failure');
          throw error;
        }
      }));
      if(failed||finished.some(row=>row.status==='rejected'))throw new Error('Release phase failed; dependent Workers were not deployed');
    }
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
