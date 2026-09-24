#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync, spawn } from 'node:child_process';
import { createDeploymentLedger } from './deployment-ledger.mjs';
import { releaseTag } from './live-worker-state.mjs';
import { currentRelease } from './current-production-release.mjs';
import { phases } from './deploy-workers.mjs';
import { readPlan, releaseFingerprints, buildSelected } from './release-plan.mjs';
import { resolveReleasedImages } from './released-images.mjs';
import { configureReleasedAccount } from './released-account-image.mjs';

const commands=Object.fromEntries([...phases.infrastructure,...phases.consumers,
  ['managed','js/managed',['npx','wrangler','deploy','--config','wrangler.ci.jsonc','--containers-rollout','immediate']],
  ['account','js/account',['npx','wrangler','deploy','--config','dist/nanocodex/wrangler.ci.json']],
].map(([name,directory,command])=>[name,{directory,command}]));
export const releasePhases=[['egress','x'],['media'],['managed'],['email','dialog','connect-api','astra','chief-of-staff','playground'],['account']];

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
export async function accountHealth(expectedRevision) {
  const response=await fetch('https://nanocodex.gakonst.workers.dev/api/health',{signal:AbortSignal.timeout(20_000)});
  assert.equal(response.status,200);const health=await response.json();
  assert.equal(health.service,'nanocodex');assert.equal(health.runtime,'cloudflare-workers');assert.equal(health.status,'ok');
  if(expectedRevision)assert.equal(health.deployment_sha,expectedRevision,'Account health must identify the released revision');
}
// Prepare only the next selected deployment phase. The same checkout and set of
// completed targets let later consumers reuse dependencies already built here.
export async function prepareReleasePhase(plan, {cwd=process.cwd(),env=process.env,
  completedTargets=new Set(),run=execFileSync,managed=resolveReleasedImages,
  account=configureReleasedAccount}={}) {
  const buildEnv={...env};
  for(const key of ['ASTRA_MANAGED_API_KEY','ASTRA_MPP_SECRET','TEMPO_API_KEY'])delete buildEnv[key];
  const buildRun=(command,args,options)=>run(command,args,{...options,cwd,env:buildEnv});
  if(plan.selected.includes('astra'))buildRun('npm',['ci','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
  buildSelected(plan,buildRun,completedTargets);
  if(plan.selected.includes('managed'))await managed({cwd,account:env.CLOUDFLARE_ACCOUNT_ID,
    repository:env.GITHUB_REPOSITORY,epoch:env.MANAGED_IMAGE_CACHE_EPOCH||'1',
    requireCurrent:(env.RELEASE_ONLY||'').split(',').includes('managed')});
  // Account's generated Wrangler config exists only after its application build.
  if(plan.selected.includes('account'))await account({cwd,account:env.CLOUDFLARE_ACCOUNT_ID,
    repository:env.GITHUB_REPOSITORY,token:env.CLOUDFLARE_API_TOKEN});
}

export async function releaseWorkers(plan,{ledger=createDeploymentLedger(),isCurrent=currentRelease,run=guardedCommand,health=accountHealth,env=process.env,cwd=process.cwd(),prepare=prepareReleasePhase,verify=async()=>{}}={}){
  if(plan.selected.includes('account'))assert.match(plan.revision,/^[a-f0-9]{40}$/);
  const results=[];
  const completedTargets=new Set();
  const result=(pending,state)=>{
    results.push({name:pending.name,state,seconds:(Date.now()-pending.started)/1000});
    if(state==='success')console.log(`::notice title=Worker released::${pending.name} verified at ${new Date().toISOString()}`);
  };
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
      if(name==='account')command.push('--var',`DEPLOYMENT_SHA:${plan.revision}`);
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
      const selected=phase.filter(name=>plan.selected.includes(name));
      if(!selected.length)continue;
      // Avoid starting unrelated compilation after a newer push supersedes us.
      if(!await isCurrent()){
        for(const name of selected)results.push({name,state:'superseded',seconds:0});
        continue;
      }
      const started=Date.now();
      console.log(`::notice title=Worker preparation::${selected.join(", ")} started at ${new Date(started).toISOString()}`);
      try{
        await prepare({...plan,selected},{cwd,env,completedTargets});
        await verify(plan);
      }
      catch{
        for(const name of selected)results.push({name,state:'preparation-failure',seconds:(Date.now()-started)/1000});
        throw new Error('Worker phase preparation failed; dependent Workers were not deployed');
      }
      const completed=await Promise.allSettled(selected.map(deploy));
      const pending=completed.filter(row=>row.status==='fulfilled'&&row.value).map(row=>row.value);
      let failed=completed.some(row=>row.status==='rejected');
      let healthy=true;
      // One required health check per phase, before ANY successful receipt in it.
      // An empty or wholly superseded phase does no health work.
      if(pending.length)try{await health(pending.some(row=>row.name==='account')?plan.revision:undefined);}catch{healthy=false;failed=true;}
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
    // Retain successful compilation even if a later upload/health check fails.
    if(env.GITHUB_OUTPUT)appendFileSync(env.GITHUB_OUTPUT,
      `wasm-built=${completedTargets.has('nanocodex')}\n`);
    if(env.GITHUB_STEP_SUMMARY)appendFileSync(env.GITHUB_STEP_SUMMARY,
      '\n| Worker | Result | Seconds |\n|---|---|---:|\n'+results.map(result=>`| ${result.name} | ${result.state} | ${result.seconds.toFixed(1)} |\n`).join(''));
  }
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  const plan=readPlan();
  assert.deepEqual(plan.fingerprints,await releaseFingerprints(),'Release inputs changed after planning');
  await releaseWorkers(plan,{verify:async()=>
    assert.deepEqual(plan.fingerprints,await releaseFingerprints(),'Release inputs changed during preparation')});
}
