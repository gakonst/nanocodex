import test from 'node:test';
import assert from 'node:assert/strict';
import { setup } from './gmail-push-setup.mjs';

// Failure modes: invalid/mismatched project, unsafe endpoint, authorization failure,
// incompatible existing push routing/auth, failed mutation, accidental dry-run writes.
const base = ['--project', 'example-project', '--gmail-oauth-project', 'example-project', '--push-endpoint', 'https://push.example.com/gmail'];
const email = 'gmail-push@example-project.iam.gserviceaccount.com';
const subscription = { name: 'projects/example-project/subscriptions/gmail-push', topic: 'projects/example-project/topics/gmail-push', pushConfig: { pushEndpoint: base[5], oidcToken: { serviceAccountEmail: email, audience: base[5] } } };
function fixture({ existing = false, sub = subscription, fail } = {}) {
  const calls = [], logs = [];
  const run = (args) => {
    calls.push(args);
    if (fail?.(args)) throw new Error('SECRET_TOKEN authorization failed');
    if (args[0] === 'projects') return { projectNumber: '123456789', projectId: 'example-project', lifecycleState: 'ACTIVE' };
    if (args.includes('list')) {
      if (!existing) return [];
      if (args[1] === 'topics') return [{ name: subscription.topic }];
      if (args[1] === 'subscriptions') return [sub];
      return [{ email }];
    }
    return {};
  };
  return { calls, logs, run, log: (s) => logs.push(s) };
}
test('dry run validates and describes plan without invoking gcloud', () => {
  const f = fixture(); setup(base, f); assert.equal(f.calls.length, 0); assert.match(f.logs.join('\n'), /dry.run/i);
});
test('reject unsafe or ambiguous input before any gcloud command', () => {
  for (const args of [[], [...base, '--project', 'other-project'], [...base.slice(0, 3), 'other-project', ...base.slice(4)], ...['http://push.example.com', 'https://localhost/push', 'https://127.0.0.1', 'https://10.0.0.1', 'https://[::1]', 'https://user:pass@push.example.com', 'https://push.example.com/?token=secret'].map(url => [...base.slice(0, 5), url]), [...base, '--topic', '--bad'], [...base, '--unknown']]) {
    const f = fixture(); assert.throws(() => setup(args, f)); assert.equal(f.calls.length, 0);
  }
});
test('authorization failures stop and redact provider diagnostics', () => {
  const f = fixture({ fail: a => a.includes('list') });
  assert.throws(() => setup([...base, '--apply'], f), e => !e.message.includes('SECRET_TOKEN') && /gcloud failed/.test(e.message));
  assert.equal(f.calls.filter(a => a.includes('create')).length, 0);
});
test('incompatible existing subscriptions never get replaced or updated', () => {
  for (const sub of [{ ...subscription, topic: 'projects/example-project/topics/other' }, { ...subscription, pushConfig: { ...subscription.pushConfig, pushEndpoint: 'https://other.example.com' } }, { ...subscription, pushConfig: { ...subscription.pushConfig, oidcToken: { serviceAccountEmail: email, audience: 'wrong' } } }, { ...subscription, pushConfig: { pushEndpoint: base[5] } }]) {
    const f = fixture({ existing: true, sub }); assert.throws(() => setup([...base, '--apply'], f), /incompatible/);
    assert.equal(f.calls.some(a => a.includes('create') || a.includes('add-iam-policy-binding') || a.includes('update')), false);
  }
});
test('fresh apply grants scoped IAM and creates authenticated subscription with exact audience', () => {
  const f = fixture(); setup([...base, '--apply', '--audience', 'https://push.example.com/exact'], f);
  const create = f.calls.find(a => a[1] === 'subscriptions' && a[2] === 'create');
  assert.ok(create.includes('--push-auth-service-account=' + email));
  assert.ok(create.includes('--push-auth-token-audience=https://push.example.com/exact'));
  assert.ok(f.calls.some(a => a.includes('--member=serviceAccount:gmail-api-push@system.gserviceaccount.com') && a.includes('--role=roles/pubsub.publisher')));
  assert.ok(f.calls.some(a => a.includes('--member=serviceAccount:service-123456789@gcp-sa-pubsub.iam.gserviceaccount.com') && a.includes('--role=roles/iam.serviceAccountTokenCreator') && a.includes(email)));
  assert.ok(f.calls.every(a => a.includes('--project=example-project')));
});
test('compatible resources are reused; failed mutation stops subsequent writes', () => {
  const f = fixture({ existing: true }); setup([...base, '--apply'], f);
  assert.equal(f.calls.some(a => a.includes('create') && a[0] !== 'beta'), false);
  const broken = fixture({ fail: a => a.includes('add-iam-policy-binding') });
  assert.throws(() => setup([...base, '--apply'], broken), /gcloud failed/);
  assert.equal(broken.calls.some(a => a[1] === 'subscriptions' && a[2] === 'create'), false);
});
