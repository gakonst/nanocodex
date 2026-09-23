import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const action = readFileSync(new URL('../../.github/actions/rust-compiler-cache/action.yml', import.meta.url), 'utf8');
const expression = action.match(/CACHE_MODE: \$\{\{ (.+) \}\}/)?.[1];
assert.ok(expression, 'cache mode must be selected explicitly');
const evaluate = new Function('github', 'contains', 'fromJSON', `return (${expression});`);
const mode = (ref, event_name) => evaluate({ ref, event_name }, (values, value) => values.includes(value), JSON.parse);

test('only trusted master builds write compiler cache entries', () => {
  for (const event of ['push', 'schedule', 'workflow_dispatch']) {
    assert.equal(mode('refs/heads/master', event), 'READ_WRITE', event);
    assert.equal(mode('refs/heads/feature', event), 'READ_ONLY', event);
    assert.equal(mode('refs/tags/v1.0.0', event), 'READ_ONLY', event);
  }
});

test('PRs and privileged PR events cannot write even when ref names master', () => {
  for (const event of ['pull_request', 'pull_request_target', 'workflow_run', 'unknown', '']) {
    for (const ref of ['refs/heads/master', 'refs/pull/1/merge', 'refs/heads/feature', '']) {
      assert.equal(mode(ref, event), 'READ_ONLY', `${event} ${ref}`);
    }
  }
});

test('compiler cache keeps reads enabled and exports the selected write policy', () => {
  assert.ok(action.includes('echo "SCCACHE_GHA_ENABLED=true" >> "$GITHUB_ENV"'));
  assert.ok(action.includes('echo "SCCACHE_GHA_RW_MODE=$CACHE_MODE" >> "$GITHUB_ENV"'));
  assert.ok(action.includes('echo "RUSTC_WRAPPER=sccache" >> "$GITHUB_ENV"'));
  assert.ok(action.indexOf('mozilla-actions/sccache-action@') < action.indexOf('name: Enable compiler caching'));
});
