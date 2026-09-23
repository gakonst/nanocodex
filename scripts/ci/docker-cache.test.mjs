import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const vm = workflow.split('  vm-guest:\n')[1].split('  policy:\n')[0];
const expression = vm.match(/CACHE_WRITE: \$\{\{ (.+) \}\}/)?.[1];
assert.ok(expression, 'registry cache writing must be gated');
const evaluate = new Function('github', 'contains', 'fromJSON', `return (${expression});`);
const mayWrite = (ref, event_name) => evaluate({ ref, event_name }, (values, value) => values.includes(value), JSON.parse);

test('only master push, manual, and scheduled jobs export Docker cache', () => {
  for (const event of ['push', 'workflow_dispatch', 'schedule', 'pull_request', 'pull_request_target', 'unknown']) {
    for (const ref of ['refs/heads/master', 'refs/heads/feature', 'refs/pull/1/merge']) {
      assert.equal(mayWrite(ref, event), ref === 'refs/heads/master' && ['push', 'workflow_dispatch', 'schedule'].includes(event));
    }
  }
});

test('VM cache uses scoped public registry tags without replacing the local image', () => {
  assert.match(vm, /CACHE_ARCH: \$\{\{ matrix.arch \}\}/);
  assert.ok(vm.includes('ghcr.io/${GITHUB_REPOSITORY,,}-hand:buildcache-ci-hand-$CACHE_ARCH'));
  assert.ok(vm.includes('echo "from=type=registry,ref=$cache_ref"'));
  assert.ok(vm.includes('if [ "$CACHE_WRITE" = true ]; then'));
  assert.ok(vm.includes('echo "to=type=registry,ref=$cache_ref,mode=max,ignore-error=true"'));
  assert.ok(vm.includes("if: steps.hand-cache.outputs.to != ''"));
  assert.ok(vm.includes('cache-from: ${{ steps.hand-cache.outputs.from }}'));
  assert.ok(vm.includes("cache-to: ${{ steps.cache-login.outcome == 'success' && steps.hand-cache.outputs.to || '' }}"));
  assert.ok(vm.includes('id: cache-login\n        continue-on-error: true'));
  assert.ok(vm.includes('docker buildx imagetools inspect "$CACHE_REF"'));
  assert.ok(vm.includes('::notice title=Docker registry cache::verified $CACHE_REF'));
  assert.ok(vm.includes('::warning title=Docker registry cache::Cache export unavailable; future runs will build normally.'));
  assert.ok(!vm.includes('type=gha'));
  assert.match(vm, /load: true\n          tags: nanocodex-hand:ci/);
  assert.ok(!vm.includes('push: true'));
  assert.ok(vm.includes('--entrypoint sh nanocodex-hand:ci'));
  assert.ok(vm.includes('NANOCODEX_DOCKER_TEST_IMAGE: nanocodex-hand:ci'));
});
