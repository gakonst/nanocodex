#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync, spawn } from 'node:child_process';
import { createDeploymentLedger, DeploymentLedgerError } from './deployment-ledger.mjs';
import { releaseTag } from './live-worker-state.mjs';
import { currentRelease } from './current-production-release.mjs';
import { phases } from './deploy-workers.mjs';
import { readPlan, buildSelected, startBuilds } from './release-plan.mjs';
import { resolveReleasedImages } from './released-images.mjs';
import { configureReleasedAccount } from './released-account-image.mjs';

const commands=Object.fromEntries([...Object.values(phases).flat(),
  ['managed','js/managed',[process.execPath,'../../scripts/cloudflare/managed-crm.mjs','deploy','--config','wrangler.ci.jsonc','--containers-rollout','immediate']],
  ['account','js/account',['npx','wrangler','deploy','--config','dist/nanocodex/wrangler.ci.json']],
].map(([name,directory,command])=>[name,{directory,command}]));
// Publish named managed entry points before the broker binds to them.
export const releasePhases=[['x','media','sites'],['managed'],['egress'],['email','dialog','connect-api','astra','chief-of-staff','playground'],['account']];

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
// Account health failures are classified into this closed set. Annotations print
// only the category, fixed text and a validated numeric HTTP status: never the
// response body, parsed values, thrown error text or provider/network details.
export const accountHealthMessages=Object.freeze({
  http_status:'Account health returned an unexpected HTTP status',
  timeout:'Account health request timed out',
  network:'Account health request failed before a valid response',
  invalid_json:'Account health response was not valid JSON',
  invalid_shape:'Account health response was not a JSON object',
  service_mismatch:'Account health service identity mismatch',
  runtime_mismatch:'Account health runtime identity mismatch',
  status_mismatch:'Account health status was not ok',
  revision_missing:'Account health did not report a deployment revision',
  revision_mismatch:'Account health must identify the released revision',
});
const healthDetails=new WeakMap();
export class AccountHealthError extends Error {
  constructor(category,status,observedRevision){
    if(!Object.hasOwn(accountHealthMessages,category))throw new TypeError('Unknown account health category');
    const httpStatus=category==='http_status'&&Number.isInteger(status)&&status>=100&&status<=599?status:undefined;
    super(accountHealthMessages[category]+(httpStatus===undefined?'':` (HTTP ${httpStatus})`));
    healthDetails.set(this,this.message);
    this.name='AccountHealthError';this.category=category;
    if(httpStatus!==undefined)this.httpStatus=httpStatus;
    // Only a validated public Git revision is retained for diagnostics.
    if(category==='revision_mismatch'&&isRevision(observedRevision))this.observedRevision=observedRevision;
  }
}
const isRevision=value=>typeof value==='string'&&/^[a-f0-9]{40}$/.test(value);
// Edge propagation of a new Worker version is eventually consistent, so a just
// deployed revision or a transient 5xx/transport failure can be observed for a
// few seconds after Wrangler reports success. Only those categories are retried;
// identity, shape and non-transient HTTP failures fail on the first observation.
const transientHealth=error=>error instanceof AccountHealthError&&(
  ['revision_missing','revision_mismatch','timeout','network'].includes(error.category)||
  (error.category==='http_status'&&(error.httpStatus===429||error.httpStatus>=500)));
const healthAttempt=(attempts,seconds,last)=>
  `after ${attempts} attempt${attempts===1?'':'s'} over ${seconds.toFixed(1)}s`+
  (last.observedRevision?`; last observed revision ${last.observedRevision}`:'');
export async function waitForAccountHealth(expectedRevision,{deadlineMs=120_000,timeoutMs=20_000,
  retryDelayMs=500,maxRetryDelayMs=2_000,minimumProbeMs=5_000,probe=accountHealth,sleep=ms=>new Promise(done=>setTimeout(done,ms)),
  now=Date.now,log=console.log,...options}={}) {
  const started=now();
  for(let attempts=1;;attempts++){
    const remaining=deadlineMs-(now()-started);
    try{
      await probe(expectedRevision,{...options,timeoutMs:Math.max(1,Math.min(timeoutMs,remaining))});
      if(attempts>1)log(`::notice title=Account health::healthy ${healthAttempt(attempts,(now()-started)/1000,{})}`);
      return;
    }catch(error){
      if(!(error instanceof AccountHealthError))throw error;
      const elapsed=now()-started;
      const delay=Math.min(maxRetryDelayMs,retryDelayMs*attempts);
      const diagnostic=`${healthDetails.get(error)} ${healthAttempt(attempts,elapsed/1000,error)}`+
        (expectedRevision&&error.category.startsWith('revision_')?`; expected ${expectedRevision}`:'');
      // Another attempt needs its delay plus a useful probe window inside the deadline.
      if(!transientHealth(error)||elapsed+delay+Math.min(timeoutMs,minimumProbeMs)>deadlineMs){
        // The final observation stays a real failure with bounded, validated detail.
        healthDetails.set(error,diagnostic);
        throw error;
      }
      log(`Account health not ready: ${diagnostic}; retrying in ${(delay/1000).toFixed(1)}s`);
      await sleep(delay);
    }
  }
}
const transportCategory=error=>error?.name==='TimeoutError'||error?.name==='AbortError'?'timeout':'network';
export async function accountHealth(expectedRevision,{url='https://nanocodex.gakonst.workers.dev/api/health',timeoutMs=20_000,request=globalThis.fetch}={}) {
  const signal=AbortSignal.timeout(timeoutMs);
  let response;
  try{response=await request(url,{signal});}catch(error){throw new AccountHealthError(transportCategory(error));}
  if(response.status!==200)throw new AccountHealthError('http_status',response.status);
  let health;
  try{health=await response.json();}
  catch(error){throw new AccountHealthError(error instanceof SyntaxError?'invalid_json':transportCategory(error));}
  if(health===null||typeof health!=='object'||Array.isArray(health))throw new AccountHealthError('invalid_shape');
  if(health.service!=='nanocodex')throw new AccountHealthError('service_mismatch');
  if(health.runtime!=='cloudflare-workers')throw new AccountHealthError('runtime_mismatch');
  if(health.status!=='ok')throw new AccountHealthError('status_mismatch');
  if(expectedRevision){
    if(health.deployment_sha===undefined||health.deployment_sha===null)throw new AccountHealthError('revision_missing');
    if(health.deployment_sha!==expectedRevision)throw new AccountHealthError('revision_mismatch',undefined,health.deployment_sha);
  }
}
// Run the package's installed Wrangler instead of resolving it through npx.
export function localWrangler(command,directory,exists=existsSync){
  if(command[0]!=='npx'||command[1]!=='wrangler')return command;
  return exists(join(directory,'node_modules/.bin/wrangler'))?['node_modules/.bin/wrangler',...command.slice(2)]:command;
}
export function buildEnvironment(env){
  const buildEnv={...env};
  for(const key of ['ASTRA_MANAGED_API_KEY','ASTRA_MPP_SECRET','TEMPO_API_KEY'])delete buildEnv[key];
  return buildEnv;
}
// Prepare only the next selected deployment phase. The same checkout and set of
// completed targets let later consumers reuse dependencies already built here.
export async function prepareReleasePhase(plan, {cwd=process.cwd(),env=process.env,
  completedTargets=new Set(),run=execFileSync,managed=resolveReleasedImages,
  account=configureReleasedAccount,builds}={}) {
  if(builds)await builds.ready(plan.selected);
  else{
    const buildEnv=buildEnvironment(env);
    const buildRun=(command,args,options)=>run(command,args,{...options,cwd,env:buildEnv});
    if(plan.selected.includes('astra'))buildRun('npm',['ci','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
    buildSelected(plan,buildRun,completedTargets);
  }
  if(plan.selected.includes('managed'))await managed({cwd,account:env.CLOUDFLARE_ACCOUNT_ID,
    repository:env.GITHUB_REPOSITORY,epoch:env.MANAGED_IMAGE_CACHE_EPOCH||'1',
    requireCurrent:(env.RELEASE_ONLY||'').split(',').includes('managed')});
  // Account's generated Wrangler config exists only after its application build.
  if(plan.selected.includes('account'))await account({cwd,account:env.CLOUDFLARE_ACCOUNT_ID,
    repository:env.GITHUB_REPOSITORY,token:env.CLOUDFLARE_API_TOKEN});
}

export async function releaseWorkers(plan,{ledger=createDeploymentLedger(),isCurrent=currentRelease,run=guardedCommand,health=waitForAccountHealth,env=process.env,cwd=process.cwd(),prepare=prepareReleasePhase,verify=async()=>{},completedTargets=new Set()}={}){
  if(plan.selected.includes('account'))assert.match(plan.revision,/^[a-f0-9]{40}$/);
  const results=[];
  const failures=[];
  // Stage/component are controlled release metadata. Only ledger and health
  // classifier fixed messages may be included: child/provider error text can contain secrets.
  const describeFailure=(name,stage,error)=> {
    const safeHealthDetail=healthDetails.get(error);
    const detail=safeHealthDetail ? `: ${safeHealthDetail}` : error instanceof DeploymentLedgerError ? `: ${error.message}` : '';
    const description=`${name} ${stage}${detail}`;
    failures.push(description);
    console.error(`::error title=Worker release failed::${description}`);
  };
  const result=(pending,state)=>{
    results.push({name:pending.name,state,seconds:(Date.now()-pending.started)/1000});
    if(state==='success')console.log(`::notice title=Worker released::${pending.name} verified at ${new Date().toISOString()}`);
  };
  async function deploy(name){
    if(!plan.selected.includes(name))return;
    const pending={name,started:Date.now()};
    let temporary, stage='freshness check';
    try{
      if(!await isCurrent()){results.push({name,state:'superseded',seconds:0});return;}
      stage='ledger admission';
      pending.record=await ledger.start(name,plan.fingerprints[name],plan.topology?{topology:plan.topology}:undefined);
      console.log(`Deploying ${name}`);
      const spec=commands[name];
      const childEnv={...env};
      for(const key of ['ASTRA_MANAGED_API_KEY','ASTRA_MPP_SECRET','TEMPO_API_KEY'])delete childEnv[key];
      const command=[...localWrangler(spec.command,resolve(cwd,spec.directory)),'--message',env.DEPLOY_MESSAGE??'','--tag',releaseTag(plan.fingerprints[name])];
      // Parallel plans prove configs and released image refs are unchanged, so
      // skip Wrangler's per-container-application comparison (~1s per app).
      if(plan.parallel){
        const at=command.indexOf('--containers-rollout');
        if(at>=0)command[at+1]='none';
        else if(name==='account'||name==='managed')command.push('--containers-rollout','none');
      }
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
      stage='upload';
      const active=await run(command,{cwd,directory:spec.directory,env:childEnv});
      if(active)return pending;
      stage='inactive receipt';
      await ledger.finish(pending.record,'inactive');
      result(pending,'superseded');
    }catch(error){
      describeFailure(name,stage,error);
      if(pending.record)try{await ledger.finish(pending.record,'failure');}catch{}
      result(pending,'failure');
      throw error;
    }finally{
      if(temporary)rmSync(temporary,{recursive:true,force:true});
    }
  }
  try{
    // Unchanged topology: one parallel phase (one health check). Otherwise the
    // dependency-ordered phases publish entrypoints before their consumers.
    const phases=plan.parallel?[releasePhases.flat()]:releasePhases;
    for(const phase of phases){
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
      catch(error){
        describeFailure(selected.join(', '),'preparation or input verification',error);
        for(const name of selected)results.push({name,state:'preparation-failure',seconds:(Date.now()-started)/1000});
        throw new Error('Worker phase preparation failed; dependent Workers were not deployed');
      }
      const completed=await Promise.allSettled(selected.map(deploy));
      const pending=completed.filter(row=>row.status==='fulfilled'&&row.value).map(row=>row.value);
      let failed=completed.some(row=>row.status==='rejected');
      let healthy=true;
      // One required health check per phase, before ANY successful receipt in it.
      // An empty or wholly superseded phase does no health work.
      if(pending.length)try{await health(pending.some(row=>row.name==='account')?plan.revision:undefined);}catch(error){healthy=false;failed=true;describeFailure(selected.join(', '),'account health check',error);}
      const finished=await Promise.allSettled(pending.map(async row=>{
        try{
          await ledger.finish(row.record,healthy?'success':'failure');
          result(row,healthy?'success':'failure');
        }catch(error){
          describeFailure(row.name,'completion receipt',error);
          try{await ledger.finish(row.record,'failure');}catch{}
          result(row,'failure');
          throw error;
        }
      }));
      if(failed||finished.some(row=>row.status==='rejected'))throw new Error(`Release phase failed; dependent Workers were not deployed (${failures.join('; ')})`);
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
  // The plan step computed fingerprints earlier in this same job and checkout;
  // recomputing them (git walk plus image lookups) before and after every phase
  // only added latency.
  const plan=readPlan();
  const completedTargets=new Set();
  // All builds start now; each phase waits only for its own targets.
  const builds=startBuilds(plan,{env:buildEnvironment(process.env),completedTargets});
  try{
    await releaseWorkers(plan,{completedTargets,
      prepare:(phase,options)=>prepareReleasePhase(phase,{...options,builds})});
  }finally{builds.stop();}
}
