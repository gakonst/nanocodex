#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { workerSpecs, fingerprintWorkers } from './worker-inputs.mjs';
import { resolveReleasedImages } from './released-images.mjs';
import { releasedAccountIdentity } from './released-account-image.mjs';
import { createDeploymentLedger } from './deployment-ledger.mjs';

export const planPath = '.ci-release-plan.json';
export async function releaseFingerprints({cwd=process.cwd(),account=process.env.CLOUDFLARE_ACCOUNT_ID,epoch=process.env.MANAGED_IMAGE_CACHE_EPOCH || '1',released={}}={}) {
  // Only already-published images affect an API release; source changes cannot
  // schedule a native build here. Freeze the selection for this job.
  const [result, images, relay] = await Promise.all([fingerprintWorkers(cwd), resolveReleasedImages({account,cwd,epoch}), releasedAccountIdentity({account,cwd})]);
  Object.assign(released,{phone:images.phone.ref,sandbox:images.sandbox.ref,relay});
  result.managed=createHash('sha256').update(JSON.stringify([result.managed,images.phone.ref,images.sandbox.ref])).digest('hex');
  result.account=createHash('sha256').update(JSON.stringify([result.account,relay])).digest('hex');
  for (const name of Object.keys(result)) result[name] = createHash('sha256')
    .update(JSON.stringify([account, result[name]])).digest('hex');
  return result;
}
// Every deployed Wrangler config: bindings, service entrypoints, Durable Object
// classes and migrations. Cross-Worker deployment order only matters when one
// of these changes; otherwise every selected Worker uploads in parallel.
export const topologyFiles = ['js/egress/wrangler.broker.jsonc','js/x-api/wrangler.jsonc','js/media/wrangler.jsonc','js/sites/wrangler.jsonc',
  'js/managed/wrangler.jsonc','js/email/wrangler.jsonc','js/connect-dialog/wrangler.jsonc','js/connect-api/wrangler.jsonc',
  'examples/astra-mpp-trial/wrangler.jsonc','js/chief-of-staff/wrangler.jsonc','js/connect-playground/wrangler.jsonc',
  'js/account/wrangler.jsonc','scripts/cloudflare/release-workers.mjs'];
// Released container images are part of the topology: an unchanged topology
// lets deploys skip Wrangler's container application comparison.
export function releaseTopology(cwd=process.cwd(), account=process.env.CLOUDFLARE_ACCOUNT_ID, released={}) {
  const hash=createHash('sha256').update(JSON.stringify({schema:2,account,released}));
  for(const path of topologyFiles)hash.update(path).update('\0').update(readFileSync(resolve(cwd,path))).update('\0');
  return hash.digest('hex');
}
export async function selectRelease(fingerprints, {ledger=createDeploymentLedger(), force=false, revision=process.env.GITHUB_SHA, topology}={}) {
  for (const name of Object.keys(workerSpecs)) assert.match(fingerprints[name], /^[a-f0-9]{64}$/);
  // Forced releases (dispatch, image rollouts) skip history and keep ordered phases.
  if (force) return {schema:1,revision,fingerprints,selected:Object.keys(workerSpecs),...(topology?{topology,parallel:false}:{})};
  const last = Object.fromEntries(await Promise.all(Object.keys(workerSpecs).map(async name => {
    return [name, ledger.lastSuccessful ? await ledger.lastSuccessful(name)
      : { fingerprint: await ledger.lastSuccessfulFingerprint(name), topology: null }];
  })));
  const selected = Object.keys(workerSpecs).filter(name => last[name]?.fingerprint !== fingerprints[name]);
  // Parallel only when every selected Worker's live release already used this
  // exact topology, so no Worker newly depends on another's new entrypoint.
  const parallel = Boolean(topology) && selected.every(name => last[name]?.topology === topology);
  return {schema:1,revision,fingerprints,selected,...(topology?{topology,parallel}:{})};
}
export function readPlan(cwd=process.cwd(), revision=process.env.GITHUB_SHA) {
  const plan=JSON.parse(readFileSync(resolve(cwd,planPath),'utf8'));
  assert.equal(plan.schema,1);assert.equal(plan.revision,revision);
  assert(Array.isArray(plan.selected));assert.equal(new Set(plan.selected).size,plan.selected.length);
  for(const name of plan.selected){assert(Object.hasOwn(workerSpecs,name));assert.match(plan.fingerprints[name],/^[a-f0-9]{64}$/);}
  if(plan.topology!==undefined){assert.match(plan.topology,/^[a-f0-9]{64}$/);assert.equal(typeof plan.parallel,'boolean');}
  return plan;
}
export function scopedRelease(selected, only) {
  if (!only) return selected;
  const components=only.split(',');
  assert.ok(components.every(name=>['managed','account'].includes(name)));
  const scope=new Set(components);
  // A forced managed release also refreshes its private service dependency.
  if(scope.has('managed'))scope.add('media');
  const included=new Set(selected.filter(name=>scope.has(name)));
  if(included.has('managed'))included.add('media');
  return Object.keys(workerSpecs).filter(name=>included.has(name));
}
export function releaseNeeds(plan) {
  return {any:plan.selected.length>0,wasm:plan.selected.some(name=>workerSpecs[name].needsWasm),
    workspace:plan.selected.length>0,astra:plan.selected.includes('astra'),managed:plan.selected.includes('managed'),account:plan.selected.includes('account')};
}
export function installSelected(plan, run=execFileSync, {deferAstra=false}={}) {
  const packages=[...new Set([...plan.selected.filter(name=>name!=='astra').map(name=>workerSpecs[name].package),...(plan.selected.includes('astra')?['nanocodex']:[]),...(releaseNeeds(plan).wasm?['nanocodex','nanocodex-vite']:[])])];
  if(packages.length)run('pnpm',['install','--frozen-lockfile','--filter','nanocodex-monorepo',...packages.flatMap(name=>['--filter',`${name}...`])],{stdio:'inherit'});
  if(plan.selected.includes('astra')&&!deferAstra)run('npm',['ci','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
// Explicit tiers keep JS-only SDK users away from nanocodex's WASM build,
// while retaining compiled dependency ordering from a clean checkout.
export const tiers = [
  ['nanocodex-tools', 'nanocodex-connect-protocol', 'nanocodex'],
  ['nanocodex-connect-ui', 'nanocodex-terminal'],
  ['@nanocodex/connect-api', '@nanocodex/connect-dialog', '@nanocodex/connect-playground', 'nanocodex-web'],
];
// Deploys only bundle the leaf apps: their `build` scripts also typecheck
// (tsc --noEmit) and lint docs, which PR CI owns. connect-api's build is only a
// typecheck; Wrangler bundles its source directly.
export const bundleOnly = {
  '@nanocodex/connect-api': null,
  '@nanocodex/connect-dialog': 'js/connect-dialog',
  '@nanocodex/connect-playground': 'js/connect-playground',
  'nanocodex-web': 'js/account',
};
// Invoke tool binaries directly. `pnpm exec`/`pnpm --filter exec` first verifies
// dependencies, re-installs against the whole workspace and checks every
// lockfile entry against the registry (~17s per deploy).
export const turboBuild = targets => ['node_modules/.bin/turbo', ['run','build','--only',...targets.flatMap(name=>['--filter',name])]];
export const viteBuild = directory => [`${directory}/node_modules/.bin/vite`, ['build', directory]];
export function buildSelected(plan, run=execFileSync, completedTargets=new Set()) {
  const targets=[...new Set(plan.selected.flatMap(name=>workerSpecs[name].buildTargets ?? []))];
  for (const [index, tier] of tiers.entries()) {
    const selected = tier.filter(name => targets.includes(name) && !completedTargets.has(name));
    if (!selected.length) continue;
    if (index === tiers.length - 1) {
      for (const name of selected) {
        if (bundleOnly[name]) run(...viteBuild(bundleOnly[name]), {stdio:'inherit'});
        completedTargets.add(name);
      }
      continue;
    }
    run(...turboBuild(selected), {stdio:'inherit'});
    for (const name of selected) completedTargets.add(name);
  }
  if(plan.selected.includes('managed')){
    run(process.execPath,['js/managed/scripts/prepare-code-evaluator.mjs'],{stdio:'inherit'});
    // The production Wrangler deploy bypasses npm predeploy/prebuild; generate
    // the standalone shell module before Wrangler resolves its dynamic import.
    run(process.execPath,['js/managed/scripts/prepare-just-bash-lazy.mjs'],{stdio:'inherit'});
  }
  if(plan.selected.includes('astra'))run('npm',['run','build:client','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
// Start every selected build at once, off the deployment critical path. Tiers
// keep their order, leaf bundles run in parallel, and each Worker phase awaits
// only its own targets, so early phases upload while later apps still build.
export function startBuilds(plan, {cwd=process.cwd(), env=process.env, launch=spawn, completedTargets=new Set()}={}) {
  const children=new Set();
  const run=(command,args)=>new Promise((done,fail)=>{
    const child=launch(command,args,{cwd,env,stdio:'inherit'});
    children.add(child);
    child.once('error',fail);
    child.once('close',code=>{children.delete(child);code===0?done():fail(new Error(`Build step failed: ${command} ${args.slice(0,4).join(' ')}`));});
  });
  const targets=new Set(plan.selected.flatMap(name=>workerSpecs[name].buildTargets ?? []));
  const ready=new Map();
  let previous=Promise.resolve();
  for(const [index,tier] of tiers.entries()){
    const selected=tier.filter(name=>targets.has(name)&&!completedTargets.has(name));
    if(!selected.length)continue;
    const after=previous;
    if(index===tiers.length-1){
      for(const name of selected)ready.set(name,after.then(()=>bundleOnly[name]&&run(...viteBuild(bundleOnly[name]))).then(()=>completedTargets.add(name)));
      continue;
    }
    previous=after.then(()=>run(...turboBuild(selected)))
      .then(()=>{for(const name of selected)completedTargets.add(name);});
    for(const name of selected)ready.set(name,previous);
  }
  const tools=ready.get('nanocodex-tools')??Promise.resolve();
  const extras={};
  if(plan.selected.includes('managed'))extras.managed=tools
    .then(()=>run(process.execPath,['js/managed/scripts/prepare-code-evaluator.mjs']))
    .then(()=>run(process.execPath,['js/managed/scripts/prepare-just-bash-lazy.mjs']));
  // The deploy action installs Astra's npm dependencies with the rest.
  if(plan.selected.includes('astra'))extras.astra=tools
    .then(()=>run('npm',['run','build:client','--prefix','examples/astra-mpp-trial']));
  for(const promise of [...ready.values(),...Object.values(extras)])promise.catch(()=>{});
  return {
    async ready(names){
      await Promise.all(names.flatMap(name=>[...(workerSpecs[name].buildTargets ?? []).map(target=>ready.get(target)).filter(Boolean),extras[name]].filter(Boolean)));
    },
    stop(){for(const child of children)child.kill();},
  };
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const command=process.argv[2];
  if(command==='plan'){
    const only=process.env.RELEASE_ONLY;
    const released={};
    const fingerprints=await releaseFingerprints({released});
    const plan=await selectRelease(fingerprints,{force:Boolean(only)||process.env.GITHUB_EVENT_NAME==='workflow_dispatch',topology:releaseTopology(process.cwd(),process.env.CLOUDFLARE_ACCOUNT_ID,released)});
    plan.selected=scopedRelease(plan.selected,only);
    writeFileSync(planPath,JSON.stringify(plan,null,2)+'\n');
    const needs=releaseNeeds(plan);
    if(process.env.GITHUB_OUTPUT)appendFileSync(process.env.GITHUB_OUTPUT,Object.entries(needs).map(([key,value])=>`${key}=${value}\n`).join(''));
    console.log(plan.selected.length?`Selected Workers: ${plan.selected.join(', ')} (${plan.parallel?'parallel: topology unchanged':'ordered phases'})`:'No Worker changes since their last successful deployments');
  }else if(command==='install'){
    const args=process.argv.slice(3);
    assert.ok(args.length===0||(args.length===1&&['--defer-astra','--all'].includes(args[0])));
    // --all installs every deployable Worker so one cached node_modules serves any selection.
    if(args[0]==='--all')installSelected({selected:Object.keys(workerSpecs)},execFileSync,{deferAstra:true});
    else installSelected(readPlan(),execFileSync,{deferAstra:args.includes('--defer-astra')});
  }
  else if(command==='build')buildSelected(readPlan());
  else throw new Error('Usage: release-plan.mjs plan|install|build');
}
