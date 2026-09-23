#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { workerSpecs, fingerprintWorkers } from './worker-inputs.mjs';
import { createDeploymentLedger } from './deployment-ledger.mjs';
import { fingerprint, validateReceipt } from './managed-images.mjs';

export const planPath = '.ci-release-plan.json';
export async function releaseFingerprints({cwd=process.cwd(),account=process.env.CLOUDFLARE_ACCOUNT_ID,epoch=process.env.MANAGED_IMAGE_CACHE_EPOCH || '1'}={}) {
  const result = await fingerprintWorkers(cwd);
  const refs = ['phone','sandbox'].map(image=>validateReceipt(JSON.parse(readFileSync(resolve(cwd,`.ci-images/${image}.json`),'utf8')),
    image,account,fingerprint(image,account,epoch,cwd)));
  result.managed = createHash('sha256').update(JSON.stringify([result.managed,...refs])).digest('hex');
  return result;
}
export async function selectRelease(fingerprints, {ledger=createDeploymentLedger(), force=false, revision=process.env.GITHUB_SHA}={}) {
  const selected = (await Promise.all(Object.keys(workerSpecs).map(async name => {
    assert.match(fingerprints[name], /^[a-f0-9]{64}$/);
    return force || await ledger.lastSuccessfulFingerprint(name) !== fingerprints[name] ? name : null;
  }))).filter(Boolean);
  return {schema:1,revision,fingerprints,selected};
}
export function readPlan(cwd=process.cwd(), revision=process.env.GITHUB_SHA) {
  const plan=JSON.parse(readFileSync(resolve(cwd,planPath),'utf8'));
  assert.equal(plan.schema,1);assert.equal(plan.revision,revision);
  assert(Array.isArray(plan.selected));assert.equal(new Set(plan.selected).size,plan.selected.length);
  for(const name of plan.selected){assert(Object.hasOwn(workerSpecs,name));assert.match(plan.fingerprints[name],/^[a-f0-9]{64}$/);}
  return plan;
}
export function releaseNeeds(plan) {
  return {any:plan.selected.length>0,wasm:plan.selected.some(name=>workerSpecs[name].needsWasm),
    workspace:plan.selected.some(name=>name!=='astra'||workerSpecs[name].needsWasm),astra:plan.selected.includes('astra'),managed:plan.selected.includes('managed'),account:plan.selected.includes('account')};
}
export function installSelected(plan, run=execFileSync) {
  const packages=[...new Set([...plan.selected.filter(name=>name!=='astra').map(name=>workerSpecs[name].package),...(releaseNeeds(plan).wasm?['nanocodex','nanocodex-vite']:[])])];
  if(packages.length)run('pnpm',['install','--frozen-lockfile','--filter','nanocodex-monorepo',...packages.flatMap(name=>['--filter',`${name}...`])],{stdio:'inherit'});
  if(plan.selected.includes('astra'))run('npm',['ci','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
export function buildSelected(plan, run=execFileSync) {
  const targets=[...new Set(plan.selected.flatMap(name=>workerSpecs[name].buildTargets ?? []))];
  if(targets.length)run('pnpm',['exec','turbo','run','build',...targets.flatMap(name=>['--filter',name])],{stdio:'inherit'});
  if(plan.selected.includes('managed'))run(process.execPath,['js/managed/scripts/prepare-code-evaluator.mjs'],{stdio:'inherit'});
  if(plan.selected.includes('astra'))run('npm',['run','build:client','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const command=process.argv[2];
  if(command==='plan'){
    const plan=await selectRelease(await releaseFingerprints(),{force:process.env.GITHUB_EVENT_NAME==='workflow_dispatch'});
    writeFileSync(planPath,JSON.stringify(plan,null,2)+'\n');
    const needs=releaseNeeds(plan);
    if(process.env.GITHUB_OUTPUT)appendFileSync(process.env.GITHUB_OUTPUT,Object.entries(needs).map(([key,value])=>`${key}=${value}\n`).join(''));
    console.log(plan.selected.length?`Selected Workers: ${plan.selected.join(', ')}`:'No Worker changes since their last successful deployments');
  }else if(command==='install')installSelected(readPlan());
  else if(command==='build')buildSelected(readPlan());
  else throw new Error('Usage: release-plan.mjs plan|install|build');
}
