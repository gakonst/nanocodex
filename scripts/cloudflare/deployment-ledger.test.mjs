import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createDeploymentLedger, deploymentEnvironment } from './deployment-ledger.mjs';
import { releaseTag, workerScripts } from './live-worker-state.mjs';

const fingerprint = 'a'.repeat(64), ref = 'b'.repeat(40), account = '1'.repeat(32);
const deploymentId = '11111111-1111-4111-8111-111111111111';
const versionId = '22222222-2222-4222-8222-222222222222';
function fixture() {
  const records = [], statuses = new Map(), calls = [], liveCalls = [];
  const liveState = { account, deploymentId, versionId, tag: releaseTag(fingerprint) };
  const live = async (worker, options) => { liveCalls.push({ worker, options }); return { script: workerScripts[worker], ...liveState }; };
  const request = async call => {
    calls.push(call);
    if(call.method==='POST' && call.path.endsWith('/deployments')) {
      const record = {...call.body,sha:call.body.ref,id:records.length+1};
      records.unshift(record); return record;
    }
    if(call.method==='POST') {
      const id=Number(call.path.split('/').at(-2));
      const status={...call.body,id:calls.length+100};statuses.set(id,[status]);return status;
    }
    if(call.path.includes('/statuses?')) return statuses.get(Number(call.path.split('/').at(-2)))??[];
    const env=new URL('https://example.invalid/'+call.path).searchParams.get('environment');
    return records.filter(record=>record.environment===env).slice(0,1);
  };
  return {ledger:createDeploymentLedger({repository:'fixture/repo',ref,account,request,live}),records,statuses,calls,liveCalls,liveState,request,live};
}

test('only latest successful deployment with matching live identity is reusable', async()=>{
  const f=fixture();
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  const first=await f.ledger.start('managed',fingerprint);
  assert.equal(f.statuses.get(first.id)[0].state,'in_progress');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  await f.ledger.finish(first,'success');
  assert.equal(f.statuses.get(first.id)[0].description,`cf:v1:${deploymentId}:${versionId}`);
  assert.ok(f.statuses.get(first.id)[0].description.length <= 140);
  assert.deepEqual(f.records[0].payload,{schema:2,fingerprint,account,script:workerScripts.managed});
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),fingerprint);
  const next=await f.ledger.start('managed','c'.repeat(64));
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  await f.ledger.finish(next,'failure');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  const rollback=await f.ledger.start('managed',fingerprint);
  await f.ledger.finish(rollback,'success');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),fingerprint);
  assert(f.calls.filter(c=>c.method==='POST'&&c.path.endsWith('/deployments')).every(c=>c.body.auto_merge===false&&c.body.required_contexts.length===0));
});

test('manual deployment, old-version rollback, missing receipt and changed tag force redeployment',async()=>{
  for(const mutate of [
    f=>{f.liveState.deploymentId='33333333-3333-4333-8333-333333333333';}, // even the same tagged version after rollback
    f=>{f.liveState.versionId='44444444-4444-4444-8444-444444444444';},
    f=>{f.liveState.tag='manual';},
    f=>{delete f.liveState.tag;},
    f=>{delete f.statuses.get(1)[0].description;},
    f=>{f.records[0].payload.schema=1;},
    f=>{f.records[0].payload.script=workerScripts.account;},
  ]) {
    const f=fixture();const record=await f.ledger.start('managed',fingerprint);await f.ledger.finish(record,'success');
    mutate(f);assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  }
});

test('account separation is required for stored and live receipts',async()=>{
  const f=fixture();const record=await f.ledger.start('managed',fingerprint);await f.ledger.finish(record,'success');
  const other=createDeploymentLedger({repository:'fixture/repo',ref,account:'2'.repeat(32),request:f.request,live:f.live});
  const count=f.liveCalls.length;
  assert.equal(await other.lastSuccessfulFingerprint('managed'),null);assert.equal(f.liveCalls.length,count);
  f.liveState.account='2'.repeat(32);assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  const next=await f.ledger.start('managed',fingerprint);
  await assert.rejects(f.ledger.finish(next,'success'),/uncertain/);
  assert.equal(f.statuses.get(next.id)[0].state,'in_progress');
  await f.ledger.finish(next,'failure');
});

test('success requires expected tag; unavailable provider state cannot skip or certify deployment',async()=>{
  const f=fixture();const record=await f.ledger.start('managed',fingerprint);
  f.liveState.tag=releaseTag('d'.repeat(64));
  await assert.rejects(f.ledger.finish(record,'success'),/uncertain/);
  assert.equal(f.statuses.get(record.id)[0].state,'in_progress');
  await f.ledger.finish(record,'failure');
  const g=fixture();const ready=await g.ledger.start('managed',fingerprint);await g.ledger.finish(ready,'success');
  const unavailable=createDeploymentLedger({repository:'fixture/repo',ref,account,request:g.request,live:async()=>{throw Error('synthetic-private-token');}});
  assert.equal(await unavailable.lastSuccessfulFingerprint('managed'),null);
  const pending=await unavailable.start('managed',fingerprint);
  await assert.rejects(unavailable.finish(pending,'success'),error=>!error.message.includes('private-token')&&!error.cause);
});

test('components track partial success independently; skipped commands cannot become successes',async()=>{
  const f=fixture();
  const managed=await f.ledger.start('managed',fingerprint);
  const accountRecord=await f.ledger.start('account','d'.repeat(64));
  await f.ledger.finish(managed,'success');
  await f.ledger.finish(accountRecord,'inactive');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),fingerprint);
  assert.equal(await f.ledger.lastSuccessfulFingerprint('account'),null);
  await assert.rejects(f.ledger.finish(accountRecord,'success'),/Invalid/);
});

test('unknown state never skips; uncertain ledger writes are not retried',async()=>{
  const ledger=createDeploymentLedger({repository:'fixture/repo',ref,account,request:async()=>{throw new Error('private token should not escape');}});
  assert.equal(await ledger.lastSuccessfulFingerprint('managed'),null);
  await assert.rejects(ledger.start('managed',fingerprint),error=>!error.message.includes('private token'));
  assert.throws(()=>deploymentEnvironment('managed\n'),/Invalid/);
  const f=fixture();let attempted=0;
  const uncertain=createDeploymentLedger({repository:'fixture/repo',ref,account,live:f.live,request:async call=>{
    if(call.body?.state==='success'){attempted++;throw Error('uncertain write');}return f.request(call);
  }});
  const record=await uncertain.start('managed',fingerprint);
  await assert.rejects(uncertain.finish(record,'success'));
  await assert.rejects(uncertain.finish(record,'failure'),/Invalid/);
  assert.equal(attempted,1);
});
