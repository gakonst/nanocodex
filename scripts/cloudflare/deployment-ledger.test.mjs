import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createDeploymentLedger, deploymentEnvironment } from './deployment-ledger.mjs';

const fingerprint = 'a'.repeat(64), ref = 'b'.repeat(40);
function fixture() {
  const records = [], statuses = new Map(), calls = [];
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
  return {ledger:createDeploymentLedger({repository:'fixture/repo',ref,request}),records,statuses,calls};
}

test('only latest successful deployment is reusable, including rollback and interrupted attempts', async()=>{
  const f=fixture();
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  const first=await f.ledger.start('managed',fingerprint);
  assert.equal(f.statuses.get(first.id)[0].state,'in_progress');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),null);
  await f.ledger.finish(first,'success');
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

test('components track partial success independently; skipped commands cannot become successes',async()=>{
  const f=fixture();
  const managed=await f.ledger.start('managed',fingerprint);
  const account=await f.ledger.start('account','d'.repeat(64));
  await f.ledger.finish(managed,'success');
  await f.ledger.finish(account,'inactive');
  assert.equal(await f.ledger.lastSuccessfulFingerprint('managed'),fingerprint);
  assert.equal(await f.ledger.lastSuccessfulFingerprint('account'),null);
  await assert.rejects(f.ledger.finish(account,'success'),/Invalid/);
});

test('unknown state never skips; uncertain ledger writes stop before mutation',async()=>{
  const ledger=createDeploymentLedger({repository:'fixture/repo',ref,request:async()=>{throw new Error('private token should not escape');}});
  assert.equal(await ledger.lastSuccessfulFingerprint('managed'),null);
  await assert.rejects(ledger.start('managed',fingerprint),error=>!error.message.includes('private token'));
  assert.throws(()=>deploymentEnvironment('managed\n'),/Invalid/);
});
