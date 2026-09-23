#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { workerSpecs, fingerprintWorkers } from './worker-inputs.mjs';
import { resolveReleasedImages } from './released-images.mjs';
import { releasedAccountIdentity } from './released-account-image.mjs';
import { createDeploymentLedger } from './deployment-ledger.mjs';

export const planPath = '.ci-release-plan.json';
export async function releaseFingerprints({cwd=process.cwd(),account=process.env.CLOUDFLARE_ACCOUNT_ID,epoch=process.env.MANAGED_IMAGE_CACHE_EPOCH || '1'}={}) {
  const result = await fingerprintWorkers(cwd);
  // Only already-published images affect an API release; source changes cannot
  // schedule a native build here. Freeze the selection for this job.
  const [images, relay] = await Promise.all([resolveReleasedImages({account,cwd,epoch}), releasedAccountIdentity({account,cwd})]);
  result.managed=createHash('sha256').update(JSON.stringify([result.managed,images.phone.ref,images.sandbox.ref])).digest('hex');
  result.account=createHash('sha256').update(JSON.stringify([result.account,relay])).digest('hex');
  for (const name of Object.keys(result)) result[name] = createHash('sha256')
    .update(JSON.stringify([account, result[name]])).digest('hex');
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
    workspace:plan.selected.length>0,astra:plan.selected.includes('astra'),managed:plan.selected.includes('managed'),account:plan.selected.includes('account')};
}
export function installSelected(plan, run=execFileSync) {
  const packages=[...new Set([...plan.selected.filter(name=>name!=='astra').map(name=>workerSpecs[name].package),...(plan.selected.includes('astra')?['nanocodex']:[]),...(releaseNeeds(plan).wasm?['nanocodex','nanocodex-vite']:[])])];
  if(packages.length)run('pnpm',['install','--frozen-lockfile','--filter','nanocodex-monorepo',...packages.flatMap(name=>['--filter',`${name}...`])],{stdio:'inherit'});
  if(plan.selected.includes('astra'))run('npm',['ci','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
export function buildSelected(plan, run=execFileSync) {
  const targets=[...new Set(plan.selected.flatMap(name=>workerSpecs[name].buildTargets ?? []))];
  // Explicit tiers keep JS-only SDK users away from nanocodex's WASM build,
  // while retaining compiled dependency ordering from a clean checkout.
  const tiers = [
    ['nanocodex-tools', 'nanocodex-connect-protocol', 'nanocodex'],
    ['nanocodex-connect-ui', 'nanocodex-terminal'],
    ['@nanocodex/connect-api', '@nanocodex/connect-dialog', '@nanocodex/connect-playground', 'nanocodex-web'],
  ];
  for (const tier of tiers) {
    const selected = tier.filter(name => targets.includes(name));
    if (selected.length) run('pnpm', ['exec','turbo','run','build','--only',...selected.flatMap(name=>['--filter',name])], {stdio:'inherit'});
  }
  if(plan.selected.includes('managed'))run(process.execPath,['js/managed/scripts/prepare-code-evaluator.mjs'],{stdio:'inherit'});
  if(plan.selected.includes('astra'))run('npm',['run','build:client','--prefix','examples/astra-mpp-trial'],{stdio:'inherit'});
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const command=process.argv[2];
  if(command==='plan'){
    const only=process.env.RELEASE_ONLY;
    const components=only?only.split(','):[];
    assert.ok(components.every(name=>['managed','account'].includes(name)));
    const plan=await selectRelease(await releaseFingerprints(),{force:Boolean(only)||process.env.GITHUB_EVENT_NAME==='workflow_dispatch'});
    if(only)plan.selected=plan.selected.filter(name=>components.includes(name));
    writeFileSync(planPath,JSON.stringify(plan,null,2)+'\n');
    const needs=releaseNeeds(plan);
    if(process.env.GITHUB_OUTPUT)appendFileSync(process.env.GITHUB_OUTPUT,Object.entries(needs).map(([key,value])=>`${key}=${value}\n`).join(''));
    console.log(plan.selected.length?`Selected Workers: ${plan.selected.join(', ')}`:'No Worker changes since their last successful deployments');
  }else if(command==='install')installSelected(readPlan());
  else if(command==='build')buildSelected(readPlan());
  else throw new Error('Usage: release-plan.mjs plan|install|build');
}
